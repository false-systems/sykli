defmodule Sykli.Work.StoreTest do
  use ExUnit.Case, async: true

  alias Sykli.Work.Store
  alias Sykli.WorkItem

  @moduletag :tmp_dir

  @now "2026-05-08T10:00:00Z"

  describe "create/2 and get/2" do
    test "persists work item under .sykli/work/items", %{tmp_dir: tmp_dir} do
      assert {:ok, item} =
               Store.create("Investigate failing checkout deploy",
                 id: "work_001",
                 intent: "Find the failing deploy step",
                 created_by: "member:yair",
                 now: @now,
                 path: tmp_dir
               )

      path = Path.join([tmp_dir, ".sykli", "work", "items", "work_001.json"])
      assert File.exists?(path)

      assert {:ok, loaded} = Store.get("work_001", path: tmp_dir)
      assert loaded == item

      persisted = path |> File.read!() |> Jason.decode!()
      assert persisted["version"] == "1"
      assert persisted["title"] == "Investigate failing checkout deploy"
      refute Map.has_key?(persisted, "logs")
      refute Map.has_key?(persisted, "artifacts")
      refute Map.has_key?(persisted, "secrets")
    end

    test "missing item returns structured not-found error", %{tmp_dir: tmp_dir} do
      assert {:error, {:work_item_not_found, "missing"}} = Store.get("missing", path: tmp_dir)
    end
  end

  describe "list/1" do
    test "returns deterministic ID order", %{tmp_dir: tmp_dir} do
      assert {:ok, _} = Store.create("Second", id: "work_b", now: @now, path: tmp_dir)
      assert {:ok, _} = Store.create("First", id: "work_a", now: @now, path: tmp_dir)

      assert {:ok, items} = Store.list(path: tmp_dir)
      assert Enum.map(items, & &1.id) == ["work_a", "work_b"]
    end

    test "returns an empty list when no local work directory exists", %{tmp_dir: tmp_dir} do
      assert {:ok, []} = Store.list(path: tmp_dir)
    end
  end

  describe "updates" do
    test "updates status and survives reload", %{tmp_dir: tmp_dir} do
      assert {:ok, _} = Store.create("Review PR", id: "work_001", now: @now, path: tmp_dir)

      assert {:ok, updated} =
               Store.update_status("work_001", "running",
                 now: "2026-05-08T10:01:00Z",
                 path: tmp_dir
               )

      assert updated.status == "running"
      assert updated.updated_at == "2026-05-08T10:01:00Z"

      assert {:ok, reloaded} = Store.get("work_001", path: tmp_dir)
      assert reloaded.status == "running"
    end

    test "claims a work item", %{tmp_dir: tmp_dir} do
      assert {:ok, _} = Store.create("Review PR", id: "work_001", now: @now, path: tmp_dir)

      assert {:ok, claimed} =
               Store.claim("work_001", "daemon", "yair-mbp", now: @now, path: tmp_dir)

      assert claimed.status == "claimed"
      assert claimed.assigned_to_type == "daemon"
      assert claimed.assigned_to_id == "yair-mbp"
    end

    test "appends a note", %{tmp_dir: tmp_dir} do
      assert {:ok, _} = Store.create("Review PR", id: "work_001", now: @now, path: tmp_dir)

      assert {:ok, note, updated} =
               Store.append_note("work_001", "Found likely API breakage",
                 note_id: "note_001",
                 author_type: "member",
                 author_id: "yair",
                 now: @now,
                 path: tmp_dir
               )

      assert note["id"] == "note_001"
      assert length(updated.notes) == 1

      assert {:ok, reloaded} = Store.get("work_001", path: tmp_dir)
      assert hd(reloaded.notes)["body"] == "Found likely API breakage"
    end
  end

  describe "validation and malformed state" do
    test "invalid status and assignment type are rejected", %{tmp_dir: tmp_dir} do
      assert {:ok, _} = Store.create("Review PR", id: "work_001", now: @now, path: tmp_dir)

      assert {:error, {:invalid_work_item_status, "waiting"}} =
               Store.update_status("work_001", "waiting", path: tmp_dir)

      assert {:error, {:invalid_assignment_type, "robot"}} =
               Store.claim("work_001", "robot", "r2d2", path: tmp_dir)
    end

    test "invalid IDs cannot escape the store", %{tmp_dir: tmp_dir} do
      assert {:error, {:invalid_work_item_id, "../escape"}} =
               Store.get("../escape", path: tmp_dir)

      assert {:error, {:invalid_work_item_id, "/tmp/escape"}} =
               Store.get("/tmp/escape", path: tmp_dir)
    end

    test "malformed persisted JSON is explicit", %{tmp_dir: tmp_dir} do
      dir = Store.items_dir(path: tmp_dir)
      File.mkdir_p!(dir)
      File.write!(Path.join(dir, "work_001.json"), "{not json")

      assert {:error, {:malformed_work_item_json, path, %Jason.DecodeError{}}} =
               Store.get("work_001", path: tmp_dir)

      assert String.ends_with?(path, ".sykli/work/items/work_001.json")
    end

    test "structurally invalid persisted JSON is explicit", %{tmp_dir: tmp_dir} do
      dir = Store.items_dir(path: tmp_dir)
      File.mkdir_p!(dir)
      File.write!(Path.join(dir, "work_001.json"), Jason.encode!(%{"id" => "work_001"}))

      assert {:error, {:invalid_title, nil}} = Store.get("work_001", path: tmp_dir)
    end

    test "save validates work item IDs", %{tmp_dir: tmp_dir} do
      item = %WorkItem{
        id: "../escape",
        title: "Bad",
        status: "open",
        created_at: @now,
        updated_at: @now
      }

      assert {:error, {:invalid_work_item_id, "../escape"}} = Store.save(item, path: tmp_dir)
    end
  end
end
