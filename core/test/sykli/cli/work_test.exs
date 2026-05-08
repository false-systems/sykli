defmodule Sykli.CLI.WorkTest do
  use ExUnit.Case, async: true

  import ExUnit.CaptureIO

  alias Sykli.CLI.Work

  @moduletag :tmp_dir

  @now "2026-05-08T10:00:00Z"

  describe "local work commands" do
    test "create prints human output and persists item", %{tmp_dir: tmp_dir} do
      output =
        capture_io(fn ->
          assert Work.run(["create", "Investigate deploy"],
                   path: tmp_dir,
                   id: "work_001",
                   now: @now
                 ) == 0
        end)

      assert output =~ "Created work item work_001"
      assert File.exists?(Path.join([tmp_dir, ".sykli", "work", "items", "work_001.json"]))
    end

    test "create --json returns shared envelope", %{tmp_dir: tmp_dir} do
      result =
        run_json(["create", "Investigate deploy", "--intent", "Find failure", "--json"],
          path: tmp_dir,
          id: "work_001",
          now: @now
        )

      assert result["ok"] == true
      assert result["data"]["source"] == "local"
      assert result["data"]["item"]["id"] == "work_001"
      assert result["data"]["item"]["title"] == "Investigate deploy"
      assert result["data"]["item"]["intent"] == "Find failure"
      assert result["data"]["item"]["created_by_type"] == "member"
      assert result["data"]["item"]["created_by_id"] == "test-user"
    end

    test "create supports equals-form flags and joins unquoted title words", %{tmp_dir: tmp_dir} do
      result =
        run_json(["create", "Investigate", "deploy", "--intent=Find failure", "--json"],
          path: tmp_dir,
          id: "work_001",
          now: @now
        )

      assert result["data"]["item"]["title"] == "Investigate deploy"
      assert result["data"]["item"]["intent"] == "Find failure"
    end

    test "list --json handles empty and deterministic lists", %{tmp_dir: tmp_dir} do
      assert run_json(["list", "--json"], path: tmp_dir)["data"]["items"] == []

      assert run_silent(["create", "Second"], path: tmp_dir, id: "work_b", now: @now) == 0
      assert run_silent(["create", "First"], path: tmp_dir, id: "work_a", now: @now) == 0

      result = run_json(["list", "--json"], path: tmp_dir)
      assert Enum.map(result["data"]["items"], & &1["id"]) == ["work_a", "work_b"]
    end

    test "show --json returns one work item", %{tmp_dir: tmp_dir} do
      assert run_silent(["create", "Review PR"], path: tmp_dir, id: "work_001", now: @now) == 0

      result = run_json(["show", "work_001", "--json"], path: tmp_dir)
      assert result["data"]["item"]["id"] == "work_001"
      assert result["data"]["item"]["status"] == "open"
    end

    test "claim --json updates assignment with default actor", %{tmp_dir: tmp_dir} do
      assert run_silent(["create", "Review PR"], path: tmp_dir, id: "work_001", now: @now) == 0

      result = run_json(["claim", "work_001", "--json"], path: tmp_dir, now: @now)
      item = result["data"]["item"]
      assert item["status"] == "claimed"
      assert item["assigned_to_type"] == "member"
      assert item["assigned_to_id"] == "test-user"
    end

    test "claim supports explicit actor with equals-form flag", %{tmp_dir: tmp_dir} do
      assert run_silent(["create", "Review PR"], path: tmp_dir, id: "work_001", now: @now) == 0

      result =
        run_json(["claim", "work_001", "--actor=agent:claude", "--json"],
          path: tmp_dir,
          now: @now
        )

      item = result["data"]["item"]
      assert item["assigned_to_type"] == "agent"
      assert item["assigned_to_id"] == "claude"
    end

    test "note --json appends note and returns note plus item", %{tmp_dir: tmp_dir} do
      assert run_silent(["create", "Review PR"], path: tmp_dir, id: "work_001", now: @now) == 0

      result =
        run_json(["note", "work_001", "Found likely API breakage", "--json"],
          path: tmp_dir,
          note_id: "note_001",
          now: @now
        )

      assert result["data"]["note"]["id"] == "note_001"
      assert result["data"]["note"]["body"] == "Found likely API breakage"
      assert [note] = result["data"]["item"]["notes"]
      assert note["id"] == "note_001"
    end

    test "note joins unquoted body words", %{tmp_dir: tmp_dir} do
      assert run_silent(["create", "Review PR"], path: tmp_dir, id: "work_001", now: @now) == 0

      result =
        run_json(["note", "work_001", "Found", "likely", "API", "breakage", "--json"],
          path: tmp_dir,
          note_id: "note_001",
          now: @now
        )

      assert result["data"]["note"]["body"] == "Found likely API breakage"
    end

    test "help exits successfully", %{tmp_dir: tmp_dir} do
      output =
        capture_io(fn ->
          assert Work.run(["--help"], path: tmp_dir) == 0
        end)

      assert output =~ "Usage: sykli work <command>"
      assert output =~ "Unknown flags are rejected"
    end
  end

  describe "errors" do
    test "create without title returns structured JSON error", %{tmp_dir: tmp_dir} do
      result = run_json(["create", "--json"], path: tmp_dir, expect: 1)
      assert result["ok"] == false
      assert result["error"]["code"] == "work_item_missing_title"
    end

    test "unknown command returns clear JSON error", %{tmp_dir: tmp_dir} do
      result = run_json(["wat", "--json"], path: tmp_dir, expect: 1)
      assert result["ok"] == false
      assert result["error"]["code"] == "invalid_work_item"
      assert result["error"]["message"] == "invalid local work item: invalid work command \"wat\""
    end

    test "unknown flag returns clear JSON error", %{tmp_dir: tmp_dir} do
      result = run_json(["list", "--bogus", "--json"], path: tmp_dir, expect: 1)
      assert result["ok"] == false
      assert result["error"]["code"] == "invalid_work_item"
      assert result["error"]["message"] == "invalid local work item: unknown flag --bogus"
    end

    test "invalid actor type returns clear JSON error without raw tuple text", %{tmp_dir: tmp_dir} do
      result =
        run_json(["claim", "work_001", "--actor", "robot:r2d2", "--json"],
          path: tmp_dir,
          expect: 1
        )

      assert result["ok"] == false
      assert result["error"]["code"] == "invalid_work_item"
      assert result["error"]["message"] == "invalid local work item: invalid actor type \"robot\""
      refute result["error"]["message"] =~ "{:"
    end

    test "invalid id returns structured JSON error", %{tmp_dir: tmp_dir} do
      result = run_json(["show", "../escape", "--json"], path: tmp_dir, expect: 1)
      assert result["ok"] == false
      assert result["error"]["code"] == "invalid_work_item_id"
    end

    test "not found returns structured JSON error", %{tmp_dir: tmp_dir} do
      result = run_json(["show", "missing", "--json"], path: tmp_dir, expect: 1)
      assert result["ok"] == false
      assert result["error"]["code"] == "work_item_not_found"
    end

    test "already claimed returns structured JSON error", %{tmp_dir: tmp_dir} do
      assert run_silent(["create", "Review PR"], path: tmp_dir, id: "work_001", now: @now) == 0
      assert run_silent(["claim", "work_001"], path: tmp_dir, now: @now) == 0

      result = run_json(["claim", "work_001", "--json"], path: tmp_dir, expect: 1)
      assert result["error"]["code"] == "work_item_already_claimed"
    end

    test "malformed persisted JSON returns structured JSON error", %{tmp_dir: tmp_dir} do
      dir = Path.join([tmp_dir, ".sykli", "work", "items"])
      File.mkdir_p!(dir)
      File.write!(Path.join(dir, "work_001.json"), "{bad json")

      result = run_json(["show", "work_001", "--json"], path: tmp_dir, expect: 1)
      assert result["error"]["code"] == "malformed_work_item_json"
    end
  end

  defp run_json(args, opts) do
    opts = Keyword.put_new(opts, :default_actor_id, "test-user")
    expected_code = Keyword.get(opts, :expect, 0)

    output =
      capture_io(fn ->
        assert Work.run(args, opts) == expected_code
      end)

    Jason.decode!(output)
  end

  defp run_silent(args, opts) do
    opts = Keyword.put_new(opts, :default_actor_id, "test-user")

    capture_io(fn ->
      assert Work.run(args, opts) == 0
    end)

    0
  end
end
