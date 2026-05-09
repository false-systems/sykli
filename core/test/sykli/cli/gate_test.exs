defmodule Sykli.CLI.GateTest do
  use ExUnit.Case, async: true

  import ExUnit.CaptureIO

  alias Sykli.CLI.Gate
  alias Sykli.Gate.Store

  @moduletag :tmp_dir

  @now "2026-05-09T10:00:00Z"
  @later "2026-05-09T10:05:00Z"

  describe "local gate commands" do
    test "list --json handles empty and deterministic lists", %{tmp_dir: tmp_dir} do
      assert run_json(["list", "--json"], path: tmp_dir)["data"]["gates"] == []

      create_gate(tmp_dir, id: "gate_b", status: "waiting")
      create_gate(tmp_dir, id: "gate_a", status: "approved")

      result = run_json(["list", "--json"], path: tmp_dir)
      assert Enum.map(result["data"]["gates"], & &1["id"]) == ["gate_a", "gate_b"]
    end

    test "list can filter by status", %{tmp_dir: tmp_dir} do
      create_gate(tmp_dir, id: "gate_a", status: "approved")
      create_gate(tmp_dir, id: "gate_b", status: "waiting")

      result = run_json(["list", "--status", "waiting", "--json"], path: tmp_dir)
      assert Enum.map(result["data"]["gates"], & &1["id"]) == ["gate_b"]
    end

    test "show --json returns one gate", %{tmp_dir: tmp_dir} do
      create_gate(tmp_dir, id: "gate_001", status: "waiting", node_id: "approve")

      result = run_json(["show", "gate_001", "--json"], path: tmp_dir)
      assert result["data"]["source"] == "local"
      assert result["data"]["gate"]["id"] == "gate_001"
      assert result["data"]["gate"]["node_id"] == "approve"
    end

    test "approve --json records decision", %{tmp_dir: tmp_dir} do
      create_gate(tmp_dir, id: "gate_001", status: "waiting")

      result =
        run_json(["approve", "gate_001", "--reason", "Evidence reviewed", "--json"],
          path: tmp_dir,
          now: @later
        )

      gate = result["data"]["gate"]
      assert gate["status"] == "approved"
      assert gate["reason"] == "Evidence reviewed"
      assert gate["decided_by"] == "member:test-user"
      assert gate["decided_at"] == @later
    end

    test "approve preserves explicit actor type in decided_by", %{tmp_dir: tmp_dir} do
      create_gate(tmp_dir, id: "gate_001", status: "waiting")

      result =
        run_json(
          ["approve", "gate_001", "--actor=agent:claude", "--reason", "Agent review", "--json"],
          path: tmp_dir,
          now: @later
        )

      assert result["data"]["gate"]["decided_by"] == "agent:claude"
    end

    test "reject --json records decision", %{tmp_dir: tmp_dir} do
      create_gate(tmp_dir, id: "gate_001", status: "waiting")

      result =
        run_json(["reject", "gate_001", "--reason=Not safe", "--json"],
          path: tmp_dir,
          now: @later
        )

      gate = result["data"]["gate"]
      assert gate["status"] == "rejected"
      assert gate["reason"] == "Not safe"
    end

    test "help exits successfully", %{tmp_dir: tmp_dir} do
      output =
        capture_io(fn ->
          assert Gate.run(["--help"], path: tmp_dir) == 0
        end)

      assert output =~ "Usage: sykli gate <command>"
      assert output =~ "Approval and rejection require --reason"
    end
  end

  describe "errors" do
    test "not-found gate returns structured JSON error", %{tmp_dir: tmp_dir} do
      result = run_json(["show", "missing", "--json"], path: tmp_dir, expect: 1)
      assert result["error"]["code"] == "gate_not_found"
    end

    test "invalid gate ID returns structured JSON error", %{tmp_dir: tmp_dir} do
      result = run_json(["show", "../escape", "--json"], path: tmp_dir, expect: 1)
      assert result["error"]["code"] == "invalid_gate_id"
    end

    test "missing reason returns structured JSON error", %{tmp_dir: tmp_dir} do
      create_gate(tmp_dir, id: "gate_001", status: "waiting")
      result = run_json(["approve", "gate_001", "--json"], path: tmp_dir, expect: 1)
      assert result["error"]["code"] == "gate_decision_missing_reason"
    end

    test "blank reason returns structured JSON error", %{tmp_dir: tmp_dir} do
      create_gate(tmp_dir, id: "gate_001", status: "waiting")

      result =
        run_json(["approve", "gate_001", "--reason", "   ", "--json"], path: tmp_dir, expect: 1)

      assert result["error"]["code"] == "gate_decision_missing_reason"
    end

    test "invalid transition returns structured JSON error", %{tmp_dir: tmp_dir} do
      create_gate(tmp_dir, id: "gate_001", status: "approved")

      result =
        run_json(["reject", "gate_001", "--reason", "No", "--json"],
          path: tmp_dir,
          expect: 1
        )

      assert result["error"]["code"] == "invalid_gate_transition"
      assert result["error"]["message"] == "invalid gate transition: approved -> rejected"
    end

    test "unknown flag returns clear JSON error", %{tmp_dir: tmp_dir} do
      result = run_json(["list", "--bogus", "--json"], path: tmp_dir, expect: 1)
      assert result["error"]["code"] == "invalid_gate_decision"
      assert result["error"]["message"] == "invalid local gate decision: unknown flag --bogus"
    end

    test "malformed persisted JSON returns structured JSON error", %{tmp_dir: tmp_dir} do
      dir = Store.gates_dir(path: tmp_dir)
      File.mkdir_p!(dir)
      File.write!(Path.join(dir, "gate_001.json"), "{bad json")

      result = run_json(["show", "gate_001", "--json"], path: tmp_dir, expect: 1)
      assert result["error"]["code"] == "malformed_gate_json"
    end
  end

  defp run_json(args, opts) do
    opts = Keyword.put_new(opts, :default_actor_id, "test-user")
    expected_code = Keyword.get(opts, :expect, 0)

    output =
      capture_io(fn ->
        assert Gate.run(args, opts) == expected_code
      end)

    Jason.decode!(output)
  end

  defp create_gate(tmp_dir, opts) do
    opts =
      opts
      |> Keyword.put_new(:now, @now)
      |> Keyword.put(:path, tmp_dir)

    assert {:ok, _gate} = Store.create(opts)
  end
end
