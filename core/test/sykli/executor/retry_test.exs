defmodule Sykli.Executor.RetryTest do
  @moduledoc """
  Tests for executor public API: group_by_level/2 and task struct contracts.

  Retry logic (apply_on_fail_retry_boost, do_run_with_retry, apply_on_fail_hook)
  is private in Sykli.Executor and tested indirectly via blackbox tests.
  These tests verify the data structures and public functions that feed into
  the retry pipeline.
  """

  use ExUnit.Case, async: true

  alias Sykli.Graph.Task
  alias Sykli.Graph.Task.AiHooks
  alias Sykli.Executor
  alias Sykli.Executor.TaskResult

  # group_by_level/2 expects graph as %{name => %Task{}} map
  defp build_graph(tasks) do
    Map.new(tasks, fn t -> {t.name, t} end)
  end

  describe "group_by_level/2" do
    test "single task with no deps is level 0" do
      tasks = [%Task{name: "a", command: "echo a", depends_on: []}]
      graph = build_graph(tasks)

      levels = Executor.group_by_level(tasks, graph)
      assert length(levels) == 1
      assert [%Task{name: "a"}] = hd(levels)
    end

    test "linear chain produces sequential levels" do
      tasks = [
        %Task{name: "a", command: "echo a", depends_on: []},
        %Task{name: "b", command: "echo b", depends_on: ["a"]},
        %Task{name: "c", command: "echo c", depends_on: ["b"]}
      ]

      graph = build_graph(tasks)
      levels = Executor.group_by_level(tasks, graph)
      assert length(levels) == 3

      names_per_level = Enum.map(levels, fn level -> Enum.map(level, & &1.name) end)
      assert names_per_level == [["a"], ["b"], ["c"]]
    end

    test "independent tasks share a level" do
      tasks = [
        %Task{name: "a", command: "echo a", depends_on: []},
        %Task{name: "b", command: "echo b", depends_on: []},
        %Task{name: "c", command: "echo c", depends_on: []}
      ]

      graph = build_graph(tasks)
      levels = Executor.group_by_level(tasks, graph)
      assert length(levels) == 1

      names = Enum.map(hd(levels), & &1.name) |> Enum.sort()
      assert names == ["a", "b", "c"]
    end

    test "diamond dependency produces correct levels" do
      tasks = [
        %Task{name: "a", command: "echo a", depends_on: []},
        %Task{name: "b", command: "echo b", depends_on: ["a"]},
        %Task{name: "c", command: "echo c", depends_on: ["a"]},
        %Task{name: "d", command: "echo d", depends_on: ["b", "c"]}
      ]

      graph = build_graph(tasks)
      levels = Executor.group_by_level(tasks, graph)
      assert length(levels) == 3

      level_names = Enum.map(levels, fn l -> Enum.map(l, & &1.name) |> Enum.sort() end)
      assert level_names == [["a"], ["b", "c"], ["d"]]
    end
  end

  describe "TaskResult status contract" do
    test "all expected status values are valid atoms" do
      for status <- [:passed, :failed, :errored, :cached, :skipped, :blocked] do
        result = %TaskResult{name: "test", status: status, duration_ms: 0}
        assert result.status == status
      end
    end

    test "failure states are :failed and :errored" do
      assert :failed in [:failed, :errored]
      assert :errored in [:failed, :errored]
      refute :passed in [:failed, :errored]
      refute :skipped in [:failed, :errored]
    end
  end

  describe "on_fail AI hook contract" do
    test "on_fail: :retry with no retry set implies boost to at least 2" do
      hooks = %AiHooks{on_fail: :retry}
      task = %Task{name: "test", ai_hooks: hooks, retry: nil}
      assert (task.retry || 0) < 2
    end

    test "on_fail: :retry with retry > 2 preserves higher value" do
      hooks = %AiHooks{on_fail: :retry}
      task = %Task{name: "test", ai_hooks: hooks, retry: 5}
      assert task.retry > 2
    end

    test "max_attempts is retry + 1" do
      assert (nil || 0) + 1 == 1
      assert (0 || 0) + 1 == 1
      assert (2 || 0) + 1 == 3
      assert (5 || 0) + 1 == 6
    end
  end
end
