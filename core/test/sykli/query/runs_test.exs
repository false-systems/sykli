defmodule Sykli.Query.RunsTest do
  use ExUnit.Case, async: true

  alias Sykli.Query.Runs
  alias Sykli.RunHistory
  alias Sykli.RunHistory.{Run, TaskResult}

  @tmp_dir System.tmp_dir!()

  setup do
    test_dir = Path.join(@tmp_dir, "query_runs_#{:erlang.unique_integer([:positive])}")
    File.mkdir_p!(test_dir)
    on_exit(fn -> File.rm_rf!(test_dir) end)
    {:ok, workdir: test_dir}
  end

  describe "latest" do
    test "returns the most recent run", %{workdir: workdir} do
      save_run(workdir, :failed, ~U[2026-02-20 10:00:00Z], [
        {"test", :failed, 3000, "exit code 1"}
      ])

      {:ok, result} = Runs.execute(:latest, workdir)

      assert result.type == :runs
      assert result.data.run.overall == :failed
      assert length(result.data.run.tasks) == 1
      assert hd(result.data.run.tasks).failure_semantics["class"] == "runtime_failure"
    end

    test "returns error when no runs exist", %{workdir: workdir} do
      assert {:error, :no_runs} = Runs.execute(:latest, workdir)
    end
  end

  describe "last_good" do
    test "returns the most recent passing run", %{workdir: workdir} do
      save_run(workdir, :passed, ~U[2026-02-20 10:00:00Z], [
        {"test", :passed, 3000, nil}
      ])

      save_run(workdir, :failed, ~U[2026-02-21 10:00:00Z], [
        {"test", :failed, 3000, nil}
      ])

      {:ok, result} = Runs.execute(:last_good, workdir)
      assert result.data.run.overall == :passed
    end
  end

  describe "recent" do
    test "returns runs from last 7 days", %{workdir: workdir} do
      save_run(workdir, :passed, DateTime.utc_now(), [
        {"test", :passed, 3000, nil}
      ])

      {:ok, result} = Runs.execute(:recent, workdir)
      assert result.data.count >= 1
    end
  end

  # Helpers

  defp save_run(workdir, overall, timestamp, task_results) do
    tasks =
      Enum.map(task_results, fn {name, status, duration_ms, error} ->
        %TaskResult{
          name: name,
          status: status,
          duration_ms: duration_ms,
          error: error,
          failure_semantics: failure_semantics(status, error)
        }
      end)

    run = %Run{
      id: "run-#{:erlang.unique_integer([:positive])}",
      timestamp: timestamp,
      git_ref: "abc123",
      git_branch: "main",
      tasks: tasks,
      overall: overall
    }

    RunHistory.save(run, path: workdir)
  end

  defp failure_semantics(:failed, _error),
    do: Sykli.FailureSemantics.runtime_failure("command_failed", "task command failed")

  defp failure_semantics(status, error), do: Sykli.FailureSemantics.for_result(status, error)
end
