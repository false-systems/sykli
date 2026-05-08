defmodule Sykli.Executor.ReviewPrimitiveTest do
  use ExUnit.Case, async: false

  alias Sykli.Error
  alias Sykli.Executor
  alias Sykli.Executor.TaskResult
  alias Sykli.Graph.{Review, Task}
  alias Sykli.ReviewPrimitive.Result
  alias Sykli.Target.Local

  defmodule PassingApiBreakageRunner do
    def evaluate(task, _state, _opts) do
      {:ok,
       %Result{
         review_type: "api_breakage",
         status: :passed,
         severity: "info",
         message: "no public API breakage detected",
         tool: "fixture",
         findings: [],
         evidence: %{"task" => task.name}
       }}
    end
  end

  defmodule FailingApiBreakageRunner do
    def evaluate(task, _state, _opts) do
      {:error,
       %Result{
         review_type: "api_breakage",
         status: :failed,
         severity: "breaking",
         message: "public API breakage detected",
         tool: "fixture",
         findings: [%{"symbol" => "Client.connect", "change" => "removed"}],
         evidence: %{"task" => task.name}
       }}
    end
  end

  setup do
    original_runner = Application.get_env(:sykli, :api_breakage_review_runner)

    on_exit(fn ->
      if original_runner do
        Application.put_env(:sykli, :api_breakage_review_runner, original_runner)
      else
        Application.delete_env(:sykli, :api_breakage_review_runner)
      end
    end)
  end

  test "api_breakage review node invokes configured primitive runner and passes" do
    Application.put_env(:sykli, :api_breakage_review_runner, PassingApiBreakageRunner)
    task = review_task("review-api")

    assert {:ok, [%TaskResult{status: :passed, review_result: result, command: nil}]} =
             Executor.run([task], graph(task), target: Local)

    assert %Result{
             review_type: "api_breakage",
             status: :passed,
             message: "no public API breakage detected"
           } = result
  end

  test "failing api_breakage result fails the review node with structured evidence" do
    Application.put_env(:sykli, :api_breakage_review_runner, FailingApiBreakageRunner)
    task = review_task("review-api")

    assert {:error,
            [
              %TaskResult{
                status: :failed,
                error: %Error{code: "review_primitive_failed"},
                review_result: %Result{
                  review_type: "api_breakage",
                  status: :failed,
                  severity: "breaking",
                  findings: [%{"symbol" => "Client.connect"}]
                }
              }
            ]} = Executor.run([task], graph(task), target: Local)
  end

  test "default api_breakage behavior is explicit unsupported failure" do
    Application.delete_env(:sykli, :api_breakage_review_runner)
    task = review_task("review-api")

    assert {:error,
            [
              %TaskResult{
                status: :failed,
                error: %Error{code: "review_primitive_failed"},
                review_result: %Result{
                  review_type: "api_breakage",
                  status: :unsupported,
                  message: "api_breakage review primitive has no configured adapter"
                }
              }
            ]} = Executor.run([task], graph(task), target: Local)
  end

  test "unknown review primitive fails explicitly" do
    task = review_task("review-unknown", primitive: "unknown_check")

    assert {:error,
            [
              %TaskResult{
                status: :failed,
                error: %Error{code: "review_primitive_failed"},
                review_result: %Result{
                  review_type: "unknown_check",
                  status: :unsupported,
                  message: "unsupported review primitive: unknown_check"
                }
              }
            ]} = Executor.run([task], graph(task), target: Local)
  end

  defp review_task(name, opts \\ []) do
    primitive = Keyword.get(opts, :primitive, "api_breakage")

    %Task{
      name: name,
      kind: :review,
      command: nil,
      depends_on: [],
      inputs: [],
      outputs: %{},
      services: [],
      task_inputs: [],
      review: %Review{
        primitive: primitive,
        agent: "local",
        context: ["lib/**/*.ex"],
        deterministic: true
      }
    }
  end

  defp graph(%Task{} = task), do: %{task.name => task}
end
