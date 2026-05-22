defmodule Sykli.Executor.FailureSemanticsTest do
  use ExUnit.Case, async: true

  alias Sykli.Error
  alias Sykli.Executor
  alias Sykli.Executor.TaskResult
  alias Sykli.Graph.Task.AiHooks
  alias Sykli.Graph.Task
  alias Sykli.Target.Local

  defmodule TimeoutTarget do
    @behaviour Sykli.Target.Behaviour

    @impl true
    def name, do: "timeout-target"

    @impl true
    def available?, do: {:ok, %{}}

    @impl true
    def setup(opts), do: {:ok, %{workdir: Keyword.get(opts, :workdir, ".")}}

    @impl true
    def teardown(_state), do: :ok

    @impl true
    def run_task(task, _state, _opts),
      do: {:error, Error.task_timeout(task.name, task.command, 1)}

    @impl true
    def resolve_secret(_name, _state), do: {:error, :not_found}

    @impl true
    def create_volume(_name, _opts, _state), do: {:ok, %{id: "v", host_path: nil, reference: "v"}}

    @impl true
    def artifact_path(_task, _artifact, _workdir, _state), do: "/tmp/missing"

    @impl true
    def copy_artifact(_src, _dest, _workdir, _state), do: :ok

    @impl true
    def start_services(_name, _services, _state), do: {:ok, nil}

    @impl true
    def stop_services(_info, _state), do: :ok
  end

  setup do
    workdir =
      Path.join(
        System.tmp_dir!(),
        "sykli-failure-semantics-#{System.unique_integer([:positive])}"
      )

    File.mkdir_p!(workdir)
    on_exit(fn -> File.rm_rf!(workdir) end)

    {:ok, workdir: workdir}
  end

  test "timeout is classified distinctly from runtime failure", %{workdir: workdir} do
    task = task("slow", command: "sleep 10")

    assert {:error, [%TaskResult{status: :errored, failure_semantics: semantics}]} =
             Executor.run([task], graph([task]), target: TimeoutTarget, workdir: workdir)

    assert semantics.class == :timeout
    assert semantics.retryable == true
    assert semantics.source == :target
  end

  test "global timeout is enforced by the local fake runtime without waiting for command completion",
       %{workdir: workdir} do
    task = task("slow", command: "sleep 30")
    start_time = System.monotonic_time(:millisecond)

    assert {:error,
            [
              %TaskResult{
                status: :errored,
                duration_ms: duration_ms,
                error: %Error{code: "task_timeout"},
                failure_semantics: semantics
              }
            ]} =
             Executor.run([task], graph([task]),
               target: Local,
               runtime: Sykli.Runtime.Fake,
               workdir: workdir,
               timeout: 100
             )

    elapsed_ms = System.monotonic_time(:millisecond) - start_time

    assert duration_ms < 1_000
    assert elapsed_ms < 1_000
    assert semantics.class == :timeout
    assert semantics.reason == "task_timeout"
    assert semantics.details["code"] == "task_timeout"
    # Timeout failure semantics record the configured timeout, not measured wall-clock runtime.
    assert semantics.details["duration_ms"] == 100
  end

  test "condition skip is classified as skipped and not as failure", %{workdir: workdir} do
    task =
      task("conditional", command: "echo no", condition: "branch == 'definitely-not-current'")

    assert {:ok, [%TaskResult{status: :skipped, failure_semantics: semantics}]} =
             Executor.run([task], graph([task]), target: Local, workdir: workdir)

    assert semantics.class == :skipped
    assert semantics.reason == "condition_not_met"
  end

  test "dependency blocked task is classified as dependency failure", %{workdir: workdir} do
    build = task("build", command: "exit 2")
    deploy = task("deploy", command: "echo deploy", depends_on: ["build"])

    assert {:error, results} =
             Executor.run([build, deploy], graph([build, deploy]),
               target: Local,
               workdir: workdir
             )

    assert %TaskResult{status: :blocked, failure_semantics: semantics} =
             Enum.find(results, &(&1.name == "deploy"))

    assert semantics.class == :dependency_failure
    assert semantics.reason == "dependency_failed"
  end

  test "on_fail skip preserves original failure semantics in details", %{workdir: workdir} do
    task =
      task("optional-failure",
        command: "exit 3",
        ai_hooks: %AiHooks{on_fail: :skip}
      )

    assert {:ok, [%TaskResult{status: :skipped, failure_semantics: semantics}]} =
             Executor.run([task], graph([task]), target: Local, workdir: workdir)

    assert semantics.class == :skipped
    assert semantics.reason == "ai_hook_skip"
    assert semantics.details["original_failure_semantics"]["class"] == "runtime_failure"
    assert semantics.details["original_failure_semantics"]["reason"] == "command_failed"
  end

  defp task(name, attrs) do
    struct(
      Task,
      Keyword.merge(
        [
          name: name,
          kind: :task,
          command: "echo ok",
          depends_on: [],
          inputs: [],
          outputs: %{},
          success_criteria: []
        ],
        attrs
      )
    )
  end

  defp graph(tasks), do: Map.new(tasks, &{&1.name, &1})
end
