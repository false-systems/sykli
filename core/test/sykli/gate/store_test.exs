defmodule Sykli.Gate.StoreTest do
  use ExUnit.Case, async: true

  alias Sykli.Gate.Store
  alias Sykli.GateDecision

  @moduletag :tmp_dir

  @now "2026-05-09T10:00:00Z"
  @later "2026-05-09T10:05:00Z"

  describe "create/1 and get/2" do
    test "persists gate decision under .sykli/gates", %{tmp_dir: tmp_dir} do
      assert {:ok, gate} =
               Store.create(
                 id: "gate_001",
                 work_item_id: "work_001",
                 run_id: "run-001",
                 node_id: "approve",
                 requested_by_type: "system",
                 requested_by_id: "executor",
                 evidence_refs: [%{"type" => "occurrence", "uri" => "occ://1"}],
                 now: @now,
                 path: tmp_dir
               )

      path = Path.join([tmp_dir, ".sykli", "gates", "gate_001.json"])
      assert File.exists?(path)

      assert {:ok, loaded} = Store.get("gate_001", path: tmp_dir)
      assert loaded == gate

      persisted = path |> File.read!() |> Jason.decode!()
      assert persisted["version"] == "1"
      assert persisted["status"] == "waiting"
      refute Map.has_key?(persisted, "logs")
      refute Map.has_key?(persisted, "artifacts")
      refute Map.has_key?(persisted, "secrets")
    end

    test "missing gate returns structured not-found error", %{tmp_dir: tmp_dir} do
      assert {:error, {:gate_not_found, "missing"}} = Store.get("missing", path: tmp_dir)
    end
  end

  describe "list/1" do
    test "returns deterministic id order and supports status filtering", %{tmp_dir: tmp_dir} do
      assert {:ok, _} = Store.create(id: "gate_b", status: "approved", now: @now, path: tmp_dir)
      assert {:ok, _} = Store.create(id: "gate_a", status: "waiting", now: @now, path: tmp_dir)

      assert {:ok, gates} = Store.list(path: tmp_dir)
      assert Enum.map(gates, & &1.id) == ["gate_a", "gate_b"]

      assert {:ok, waiting} = Store.list_by_status("waiting", path: tmp_dir)
      assert Enum.map(waiting, & &1.id) == ["gate_a"]
    end

    test "returns an empty list when no local gate directory exists", %{tmp_dir: tmp_dir} do
      assert {:ok, []} = Store.list(path: tmp_dir)
    end
  end

  describe "decisions" do
    test "approves and rejects persisted gates", %{tmp_dir: tmp_dir} do
      assert {:ok, _} = Store.create(id: "gate_001", now: @now, path: tmp_dir)
      assert {:ok, _} = Store.create(id: "gate_002", now: @now, path: tmp_dir)

      assert {:ok, approved} =
               Store.approve("gate_001", "Looks safe",
                 decided_by: "member:yair",
                 now: @later,
                 path: tmp_dir
               )

      assert approved.status == "approved"
      assert approved.reason == "Looks safe"
      assert approved.decided_by == "member:yair"
      assert approved.decided_at == @later

      assert {:ok, rejected} = Store.reject("gate_002", "Not safe", now: @later, path: tmp_dir)
      assert rejected.status == "rejected"

      assert {:ok, reloaded} = Store.get("gate_001", path: tmp_dir)
      assert reloaded.status == "approved"
    end

    test "rejects invalid transitions", %{tmp_dir: tmp_dir} do
      assert {:ok, _} = Store.create(id: "gate_001", now: @now, path: tmp_dir)
      assert {:ok, _} = Store.approve("gate_001", "Reviewed", now: @later, path: tmp_dir)

      assert {:error, {:invalid_gate_transition, "approved", "rejected"}} =
               Store.reject("gate_001", "No", path: tmp_dir)
    end
  end

  describe "validation and malformed state" do
    test "invalid status and invalid ids are rejected", %{tmp_dir: tmp_dir} do
      assert {:error, {:invalid_gate_status, "done"}} =
               Store.create(id: "gate_001", status: "done", path: tmp_dir)

      assert {:error, {:invalid_gate_id, "../escape"}} = Store.get("../escape", path: tmp_dir)
      assert {:error, {:invalid_gate_id, "/tmp/escape"}} = Store.get("/tmp/escape", path: tmp_dir)
    end

    test "malformed persisted JSON is explicit", %{tmp_dir: tmp_dir} do
      dir = Store.gates_dir(path: tmp_dir)
      File.mkdir_p!(dir)
      File.write!(Path.join(dir, "gate_001.json"), "{not json")

      assert {:error, {:malformed_gate_json, path, %Jason.DecodeError{}}} =
               Store.get("gate_001", path: tmp_dir)

      assert String.ends_with?(path, ".sykli/gates/gate_001.json")
    end

    test "structurally invalid persisted JSON is explicit", %{tmp_dir: tmp_dir} do
      dir = Store.gates_dir(path: tmp_dir)
      File.mkdir_p!(dir)
      File.write!(Path.join(dir, "gate_001.json"), Jason.encode!(%{"id" => "gate_001"}))

      assert {:error, {:missing_gate_version, nil}} = Store.get("gate_001", path: tmp_dir)
    end

    test "save validates gate IDs", %{tmp_dir: tmp_dir} do
      gate = %GateDecision{
        id: "../escape",
        status: "waiting",
        created_at: @now,
        updated_at: @now
      }

      assert {:error, {:invalid_gate_id, "../escape"}} = Store.save(gate, path: tmp_dir)
    end
  end
end
