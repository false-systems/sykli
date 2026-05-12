defmodule Sykli.Executor.FailureSemanticsTest do
  use ExUnit.Case, async: true

  alias Sykli.Error
  alias Sykli.Executor
  alias Sykli.Executor.TaskResult
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

    assert {:error, [%TaskResult{status: :failed, failure_semantics: semantics}]} =
             Executor.run([task], graph([task]), target: TimeoutTarget, workdir: workdir)

    assert semantics.class == :timeout
    assert semantics.retryable == true
    assert semantics.source == :target
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
