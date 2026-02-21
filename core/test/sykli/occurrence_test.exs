defmodule Sykli.OccurrenceTest do
  use ExUnit.Case, async: true

  alias Sykli.Occurrence
  alias Sykli.Occurrence.Enrichment
  alias Sykli.Executor.TaskResult
  alias Sykli.Graph

  @tmp_dir System.tmp_dir!()

  setup do
    test_dir = Path.join(@tmp_dir, "occurrence_test_#{:erlang.unique_integer([:positive])}")
    File.mkdir_p!(test_dir)
    on_exit(fn -> File.rm_rf!(test_dir) end)
    {:ok, workdir: test_dir}
  end

  # ─────────────────────────────────────────────────────────────────────────────
  # Occurrence struct factories
  # ─────────────────────────────────────────────────────────────────────────────

  describe "Occurrence.run_completed/3" do
    test "creates ci.run.passed for :ok result" do
      occ = Occurrence.run_completed("run-1", :ok)

      assert %Occurrence{} = occ
      assert occ.type == "ci.run.passed"
      assert occ.outcome == "passed"
      assert occ.severity == :info
      assert occ.source == "sykli"
      assert occ.version == "1.0"
    end

    test "creates ci.run.failed for {:error, _} result" do
      occ = Occurrence.run_completed("run-2", {:error, :task_failed})

      assert %Occurrence{} = occ
      assert occ.type == "ci.run.failed"
      assert occ.outcome == "failed"
      assert occ.severity == :error
    end
  end

  # ─────────────────────────────────────────────────────────────────────────────
  # enrich_and_persist/4 (replaces generate/4)
  # ─────────────────────────────────────────────────────────────────────────────

  describe "enrich_and_persist/4" do
    test "writes occurrence.json to .sykli/", %{workdir: workdir} do
      {graph, result, occ} = simple_passing_run()

      assert :ok = Enrichment.enrich_and_persist(occ, graph, result, workdir)
      assert File.exists?(Path.join(workdir, ".sykli/occurrence.json"))
    end

    test "produces valid JSON", %{workdir: workdir} do
      {graph, result, occ} = simple_passing_run()

      :ok = Enrichment.enrich_and_persist(occ, graph, result, workdir)
      json = File.read!(Path.join(workdir, ".sykli/occurrence.json"))
      data = Jason.decode!(json)

      assert data["version"] == "1.0"
      assert data["id"] == occ.run_id
      assert is_binary(data["timestamp"])
    end
  end

  # ─────────────────────────────────────────────────────────────────────────────
  # enrich/4 — FALSE Protocol envelope
  # ─────────────────────────────────────────────────────────────────────────────

  describe "enrich/4 — FALSE Protocol envelope" do
    test "sets type and severity for passing run", %{workdir: workdir} do
      {graph, result, occ} = simple_passing_run()

      enriched = Enrichment.enrich(occ, graph, result, workdir)

      assert enriched.outcome == "passed"
      assert enriched.type == "ci.run.passed"
      assert enriched.severity == :info
      assert enriched.source == "sykli"
    end

    test "sets type and severity for failing run", %{workdir: workdir} do
      {graph, result, occ} = simple_failing_run()

      enriched = Enrichment.enrich(occ, graph, result, workdir)

      assert enriched.outcome == "failed"
      assert enriched.type == "ci.run.failed"
      assert enriched.severity == :error
    end

    test "no error block for passing run", %{workdir: workdir} do
      {graph, result, occ} = simple_passing_run()

      enriched = Enrichment.enrich(occ, graph, result, workdir)

      assert enriched.error == nil
    end

    test "no reasoning block for passing run", %{workdir: workdir} do
      {graph, result, occ} = simple_passing_run()

      enriched = Enrichment.enrich(occ, graph, result, workdir)

      assert enriched.reasoning == nil
    end
  end

  # ─────────────────────────────────────────────────────────────────────────────
  # enrich/4 — ci_data summary
  # ─────────────────────────────────────────────────────────────────────────────

  describe "enrich/4 — ci_data summary" do
    test "includes summary for passing run", %{workdir: workdir} do
      {graph, result, occ} = simple_passing_run()

      enriched = Enrichment.enrich(occ, graph, result, workdir)

      assert enriched.ci_data["summary"]["passed"] == 1
      assert enriched.ci_data["summary"]["failed"] == 0
    end

    test "includes summary for failing run", %{workdir: workdir} do
      {graph, result, occ} = simple_failing_run()

      enriched = Enrichment.enrich(occ, graph, result, workdir)

      assert enriched.ci_data["summary"]["failed"] == 1
    end

    test "includes cached task count", %{workdir: workdir} do
      graph =
        parse_graph!([
          %{"name" => "lint", "command" => "mix lint"},
          %{"name" => "test", "command" => "mix test"}
        ])

      results = [
        %TaskResult{name: "lint", status: :cached, duration_ms: 0},
        %TaskResult{name: "test", status: :passed, duration_ms: 100}
      ]

      occ = Occurrence.run_completed("test-cached", :ok)
      enriched = Enrichment.enrich(occ, graph, {:ok, results}, workdir)

      assert enriched.ci_data["summary"]["cached"] == 1
      assert enriched.ci_data["summary"]["passed"] == 2

      lint = Enum.find(enriched.ci_data["tasks"], &(&1["name"] == "lint"))
      assert lint["cached"] == true
    end
  end

  # ─────────────────────────────────────────────────────────────────────────────
  # enrich/4 — ci_data tasks
  # ─────────────────────────────────────────────────────────────────────────────

  describe "enrich/4 — ci_data tasks" do
    test "includes task details", %{workdir: workdir} do
      {graph, result, occ} = simple_passing_run()

      enriched = Enrichment.enrich(occ, graph, result, workdir)
      tasks = enriched.ci_data["tasks"]

      assert length(tasks) == 1
      task = hd(tasks)
      assert task["name"] == "test"
      assert task["status"] == "passed"
      assert task["command"] == "mix test"
      assert is_integer(task["duration_ms"])
    end

    test "preserves structured errors from Sykli.Error", %{workdir: workdir} do
      {graph, result, occ} = run_with_structured_error()

      enriched = Enrichment.enrich(occ, graph, result, workdir)
      task = Enum.find(enriched.ci_data["tasks"], &(&1["name"] == "build"))

      assert task["status"] == "failed"
      assert task["error"]["code"] == "task_failed"
      assert task["error"]["exit_code"] == 127
      assert is_binary(task["error"]["output"])
      assert is_list(task["error"]["hints"])
      assert length(task["error"]["hints"]) > 0
    end

    test "includes blocked task error", %{workdir: workdir} do
      graph =
        parse_graph!([
          %{"name" => "build", "command" => "make"},
          %{"name" => "test", "command" => "make test", "depends_on" => ["build"]}
        ])

      results = [
        %TaskResult{
          name: "build",
          status: :failed,
          duration_ms: 100,
          error: Sykli.Error.task_failed("build", "make", 1, "error")
        },
        %TaskResult{name: "test", status: :blocked, duration_ms: 0, error: :dependency_failed}
      ]

      occ = Occurrence.run_completed("test-blocked", {:error, :task_failed})
      enriched = Enrichment.enrich(occ, graph, {:error, results}, workdir)

      blocked = Enum.find(enriched.ci_data["tasks"], &(&1["name"] == "test"))
      assert blocked["status"] == "blocked"
      assert blocked["error"]["code"] == "dependency_failed"
    end

    test "includes depends_on and blocks", %{workdir: workdir} do
      graph =
        parse_graph!([
          %{"name" => "lint", "command" => "mix lint"},
          %{"name" => "test", "command" => "mix test", "depends_on" => ["lint"]},
          %{"name" => "build", "command" => "mix build", "depends_on" => ["test"]}
        ])

      results = [
        %TaskResult{name: "lint", status: :passed, duration_ms: 50},
        %TaskResult{name: "test", status: :passed, duration_ms: 100},
        %TaskResult{name: "build", status: :passed, duration_ms: 200}
      ]

      occ = Occurrence.run_completed("test-deps", :ok)
      enriched = Enrichment.enrich(occ, graph, {:ok, results}, workdir)

      test_task = Enum.find(enriched.ci_data["tasks"], &(&1["name"] == "test"))
      assert test_task["depends_on"] == ["lint"]
      assert test_task["blocks"] == ["build"]
    end

    test "includes semantic covers", %{workdir: workdir} do
      graph =
        parse_graph!([
          %{
            "name" => "auth-test",
            "command" => "mix test",
            "semantic" => %{"covers" => ["lib/auth/*"]}
          }
        ])

      results = [
        %TaskResult{name: "auth-test", status: :passed, duration_ms: 100}
      ]

      occ = Occurrence.run_completed("test-covers", :ok)
      enriched = Enrichment.enrich(occ, graph, {:ok, results}, workdir)

      task = hd(enriched.ci_data["tasks"])
      assert task["covers"] == ["lib/auth/*"]
    end
  end

  # ─────────────────────────────────────────────────────────────────────────────
  # enrich/4 — git context
  # ─────────────────────────────────────────────────────────────────────────────

  describe "enrich/4 — git context" do
    test "includes git context in ci_data", %{workdir: workdir} do
      {graph, result, occ} = simple_passing_run()

      enriched = Enrichment.enrich(occ, graph, result, workdir)

      assert is_map(enriched.ci_data["git"])
      assert Map.has_key?(enriched.ci_data["git"], "sha")
      assert Map.has_key?(enriched.ci_data["git"], "branch")
    end
  end

  # ─────────────────────────────────────────────────────────────────────────────
  # enrich/4 — error block
  # ─────────────────────────────────────────────────────────────────────────────

  describe "enrich/4 — error block" do
    test "includes error block for failed run", %{workdir: workdir} do
      {graph, result, occ} = simple_failing_run()

      enriched = Enrichment.enrich(occ, graph, result, workdir)

      assert is_map(enriched.error)
      assert is_binary(enriched.error["what_failed"])
      assert enriched.error["code"] != nil
    end

    test "error block has exit_code and output for task failure", %{workdir: workdir} do
      {graph, result, occ} = run_with_structured_error()

      enriched = Enrichment.enrich(occ, graph, result, workdir)

      assert enriched.error["exit_code"] == 127
      assert is_binary(enriched.error["output"])
    end
  end

  # ─────────────────────────────────────────────────────────────────────────────
  # enrich/4 — reasoning block
  # ─────────────────────────────────────────────────────────────────────────────

  describe "enrich/4 — reasoning block" do
    test "includes reasoning for failed run", %{workdir: workdir} do
      {graph, result, occ} = simple_failing_run()

      enriched = Enrichment.enrich(occ, graph, result, workdir)

      assert is_map(enriched.reasoning)
      assert is_binary(enriched.reasoning["summary"])
      assert is_number(enriched.reasoning["confidence"])
      assert is_map(enriched.reasoning["tasks"])
      assert Map.has_key?(enriched.reasoning["tasks"], "build")
    end
  end

  # ─────────────────────────────────────────────────────────────────────────────
  # enrich/4 — history block
  # ─────────────────────────────────────────────────────────────────────────────

  describe "enrich/4 — history block" do
    test "includes history steps for all tasks", %{workdir: workdir} do
      {graph, result, occ} = simple_passing_run()

      enriched = Enrichment.enrich(occ, graph, result, workdir)

      assert is_map(enriched.history)
      assert is_list(enriched.history["steps"])
      assert length(enriched.history["steps"]) == 1

      step = hd(enriched.history["steps"])
      assert step["description"] == "test"
      assert step["status"] == "passed"
      assert is_integer(step["duration_ms"])
    end

    test "history step includes error for failed task", %{workdir: workdir} do
      {graph, result, occ} = simple_failing_run()

      enriched = Enrichment.enrich(occ, graph, result, workdir)

      step = hd(enriched.history["steps"])
      assert step["status"] == "failed"
      assert is_binary(step["error"])
    end

    test "history includes total duration", %{workdir: workdir} do
      graph =
        parse_graph!([
          %{"name" => "lint", "command" => "mix lint"},
          %{"name" => "test", "command" => "mix test"}
        ])

      results = [
        %TaskResult{name: "lint", status: :passed, duration_ms: 50},
        %TaskResult{name: "test", status: :passed, duration_ms: 100}
      ]

      occ = Occurrence.run_completed("test-duration", :ok)
      enriched = Enrichment.enrich(occ, graph, {:ok, results}, workdir)

      assert enriched.history["duration_ms"] == 150
    end
  end

  # ─────────────────────────────────────────────────────────────────────────────
  # error_detail_map/1
  # ─────────────────────────────────────────────────────────────────────────────

  describe "error_detail_map/1" do
    test "converts Sykli.Error struct to map" do
      error = Sykli.Error.task_failed("test", "mix test", 127, "command not found: mix")
      map = Enrichment.error_detail_map(error)

      assert map["code"] == "task_failed"
      assert map["exit_code"] == 127
      assert map["output"] == "command not found: mix"
      assert is_list(map["hints"])
      assert length(map["hints"]) > 0
    end

    test "returns nil for nil" do
      assert Enrichment.error_detail_map(nil) == nil
    end

    test "wraps unknown errors with inspect" do
      map = Enrichment.error_detail_map({:unknown, :reason})
      assert is_binary(map["message"])
    end

    test "handles :dependency_failed atom" do
      map = Enrichment.error_detail_map(:dependency_failed)
      assert map["code"] == "dependency_failed"
    end

    test "truncates long output" do
      long_output = Enum.map_join(1..300, "\n", fn i -> "line #{i}" end)
      error = Sykli.Error.task_failed("test", "mix test", 1, long_output)
      map = Enrichment.error_detail_map(error)

      lines = String.split(map["output"], "\n")
      # 200 lines + 1 truncation message
      assert length(lines) == 201
      assert String.contains?(List.last(lines), "more lines")
    end
  end

  # ─────────────────────────────────────────────────────────────────────────────
  # to_persistence_map/1
  # ─────────────────────────────────────────────────────────────────────────────

  describe "to_persistence_map/1" do
    test "converts enriched occurrence to string-key map", %{workdir: workdir} do
      {graph, result, occ} = simple_passing_run()

      enriched = Enrichment.enrich(occ, graph, result, workdir)
      map = Enrichment.to_persistence_map(enriched)

      assert map["version"] == "1.0"
      assert map["type"] == "ci.run.passed"
      assert map["source"] == "sykli"
      assert map["outcome"] == "passed"
      assert map["severity"] == "info"
      assert is_binary(map["timestamp"])
      assert is_map(map["ci_data"])
      assert is_map(map["history"])
    end

    test "omits nil error and reasoning for passing run", %{workdir: workdir} do
      {graph, result, occ} = simple_passing_run()

      enriched = Enrichment.enrich(occ, graph, result, workdir)
      map = Enrichment.to_persistence_map(enriched)

      refute Map.has_key?(map, "error")
      refute Map.has_key?(map, "reasoning")
    end

    test "includes error and reasoning for failing run", %{workdir: workdir} do
      {graph, result, occ} = simple_failing_run()

      enriched = Enrichment.enrich(occ, graph, result, workdir)
      map = Enrichment.to_persistence_map(enriched)

      assert is_map(map["error"])
      assert is_map(map["reasoning"])
    end
  end

  # ─────────────────────────────────────────────────────────────────────────────
  # Log paths in ci_data tasks
  # ─────────────────────────────────────────────────────────────────────────────

  describe "enrich/4 — log paths in ci_data tasks" do
    test "task with output includes log path", %{workdir: workdir} do
      graph = parse_graph!([%{"name" => "test", "command" => "mix test"}])

      results = [
        %TaskResult{name: "test", status: :passed, duration_ms: 100, output: "ok\n"}
      ]

      run_id = "01ABC"
      occ = Occurrence.run_completed(run_id, :ok)
      enriched = Enrichment.enrich(occ, graph, {:ok, results}, workdir)

      task = hd(enriched.ci_data["tasks"])
      assert task["log"] == ".sykli/logs/01ABC/test.log"
    end

    test "task without output has no log path", %{workdir: workdir} do
      graph = parse_graph!([%{"name" => "test", "command" => "mix test"}])

      results = [
        %TaskResult{name: "test", status: :passed, duration_ms: 100, output: nil}
      ]

      occ = Occurrence.run_completed("test-nolog", :ok)
      enriched = Enrichment.enrich(occ, graph, {:ok, results}, workdir)

      task = hd(enriched.ci_data["tasks"])
      refute Map.has_key?(task, "log")
    end

    test "task with empty output has no log path", %{workdir: workdir} do
      graph = parse_graph!([%{"name" => "test", "command" => "mix test"}])

      results = [
        %TaskResult{name: "test", status: :passed, duration_ms: 100, output: ""}
      ]

      occ = Occurrence.run_completed("test-emptylog", :ok)
      enriched = Enrichment.enrich(occ, graph, {:ok, results}, workdir)

      task = hd(enriched.ci_data["tasks"])
      refute Map.has_key?(task, "log")
    end

    test "task name with slash is sanitized in log path", %{workdir: workdir} do
      graph = parse_graph!([%{"name" => "sdk/go", "command" => "go test"}])

      results = [
        %TaskResult{name: "sdk/go", status: :passed, duration_ms: 50, output: "PASS\n"}
      ]

      run_id = "01XYZ"
      occ = Occurrence.run_completed(run_id, :ok)
      enriched = Enrichment.enrich(occ, graph, {:ok, results}, workdir)

      task = hd(enriched.ci_data["tasks"])
      assert task["log"] == ".sykli/logs/01XYZ/sdk:go.log"
    end
  end

  # ─────────────────────────────────────────────────────────────────────────────
  # TaskResult output field
  # ─────────────────────────────────────────────────────────────────────────────

  describe "TaskResult output field" do
    test "TaskResult supports output field" do
      result = %TaskResult{
        name: "test",
        status: :passed,
        duration_ms: 100,
        output: "some output\n"
      }

      assert result.output == "some output\n"
    end

    test "TaskResult output defaults to nil" do
      result = %TaskResult{name: "test", status: :passed, duration_ms: 100}
      assert result.output == nil
    end
  end

  # ─────────────────────────────────────────────────────────────────────────────
  # FIXTURES
  # ─────────────────────────────────────────────────────────────────────────────

  defp simple_passing_run do
    graph = parse_graph!([%{"name" => "test", "command" => "mix test"}])

    results = [
      %TaskResult{name: "test", status: :passed, duration_ms: 3200}
    ]

    result = {:ok, results}
    occ = Occurrence.run_completed("test-pass-#{:erlang.unique_integer([:positive])}", :ok)
    {graph, result, occ}
  end

  defp simple_failing_run do
    graph = parse_graph!([%{"name" => "build", "command" => "make"}])

    error = Sykli.Error.task_failed("build", "make", 1, "compilation error")

    results = [
      %TaskResult{name: "build", status: :failed, duration_ms: 1500, error: error}
    ]

    result = {:error, results}

    occ =
      Occurrence.run_completed(
        "test-fail-#{:erlang.unique_integer([:positive])}",
        {:error, :task_failed}
      )

    {graph, result, occ}
  end

  defp run_with_structured_error do
    graph = parse_graph!([%{"name" => "build", "command" => "make"}])

    error =
      Sykli.Error.task_failed("build", "make", 127, "command not found: make", duration_ms: 1500)

    results = [
      %TaskResult{name: "build", status: :failed, duration_ms: 1500, error: error}
    ]

    result = {:error, results}

    occ =
      Occurrence.run_completed(
        "test-err-#{:erlang.unique_integer([:positive])}",
        {:error, :task_failed}
      )

    {graph, result, occ}
  end

  defp parse_graph!(tasks) do
    json = Jason.encode!(%{"tasks" => tasks})
    {:ok, graph} = Graph.parse(json)
    graph
  end
end
