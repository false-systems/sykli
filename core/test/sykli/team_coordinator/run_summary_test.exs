defmodule Sykli.TeamCoordinator.RunSummaryTest do
  use ExUnit.Case, async: true
  @moduletag :tmp_dir

  alias Sykli.RunHistory.{Run, TaskResult}
  alias Sykli.TeamCoordinator.RunSummary

  @session %{"org" => "false-systems", "team" => "platform", "session_id" => "sess_001"}
  @ts ~U[2026-05-11 10:00:00Z]

  test "projects run metadata nodes criteria reviews and evidence refs", %{tmp_dir: tmp_dir} do
    File.mkdir_p!(Path.join(tmp_dir, ".sykli"))
    File.write!(Path.join([tmp_dir, ".sykli", "occurrence.json"]), ~s({"ok":true}))

    run = %Run{
      id: "run_001",
      timestamp: @ts,
      git_ref: "abc1234",
      git_branch: "main",
      work_item_id: "work_001",
      contract_hash: "sha256:abc",
      overall: :failed,
      tasks: [
        %TaskResult{
          name: "test",
          kind: "task",
          status: :failed,
          duration_ms: 12,
          error: "task_failed: task failed",
          success_criteria_results: [
            %{"type" => "exit_code", "status" => "failed", "message" => "exit code 1"}
          ]
        },
        %TaskResult{
          name: "review-api",
          kind: "review",
          status: :passed,
          duration_ms: 4,
          review_result: %{
            "review_type" => "api_breakage",
            "status" => "passed",
            "severity" => nil,
            "message" => "no breaking changes detected",
            "tool" => "api-diff"
          }
        }
      ]
    }

    encoded = run |> RunSummary.from_run(session: @session, path: tmp_dir) |> RunSummary.encode()

    assert encoded["version"] == "1"
    assert encoded["run"]["status"] == "failed"
    assert encoded["run"]["error_code"] == "task_failed"
    assert [%{"name" => "test", "status" => "failed"}, %{"kind" => "review"}] = encoded["nodes"]
    assert [%{"task" => "test", "type" => "exit_code"}] = encoded["criteria_results"]

    assert [%{"task" => "review-api", "review_type" => "api_breakage"}] =
             encoded["review_results"]

    assert [%{"visibility" => "local_only", "uri" => "file://" <> _}] = encoded["evidence_refs"]
  end

  test "projects total_duration_ms and derives started_at from finished_at minus duration",
       %{tmp_dir: tmp_dir} do
    run = %Run{
      id: "run_dur",
      timestamp: @ts,
      git_ref: "abc1234",
      git_branch: "main",
      overall: :passed,
      tasks: [
        %TaskResult{name: "a", kind: "task", status: :passed, duration_ms: 1_200},
        %TaskResult{name: "b", kind: "task", status: :passed, duration_ms: 800},
        %TaskResult{name: "c", kind: "task", status: :cached, duration_ms: 0}
      ]
    }

    encoded = run |> RunSummary.from_run(session: @session, path: tmp_dir) |> RunSummary.encode()

    assert encoded["run"]["total_duration_ms"] == 2_000
    assert encoded["run"]["finished_at"] == DateTime.to_iso8601(@ts)

    {:ok, started, _} = DateTime.from_iso8601(encoded["run"]["started_at"])
    assert DateTime.diff(@ts, started, :millisecond) == 2_000
  end

  test "zero-duration run still produces equal started_at and finished_at without crashing",
       %{tmp_dir: tmp_dir} do
    run = %Run{
      id: "run_zero",
      timestamp: @ts,
      git_ref: "abc1234",
      git_branch: "main",
      overall: :passed,
      tasks: [%TaskResult{name: "a", kind: "task", status: :cached, duration_ms: nil}]
    }

    encoded = run |> RunSummary.from_run(session: @session, path: tmp_dir) |> RunSummary.encode()

    assert encoded["run"]["total_duration_ms"] == 0
    assert encoded["run"]["started_at"] == encoded["run"]["finished_at"]
  end
end
