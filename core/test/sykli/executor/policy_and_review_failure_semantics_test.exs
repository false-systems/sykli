defmodule Sykli.Executor.PolicyAndReviewFailureSemanticsTest do
  @moduledoc """
  Covers two executor classification paths not exercised by
  Sykli.Executor.FailureSemanticsTest: gate denial -> policy_block and
  review primitive failure -> contract_failure.

  Both touch global state (System.put_env for gates, Application.put_env
  for review runners) and therefore run with `async: false`.
  """

  use ExUnit.Case, async: false

  alias Sykli.Executor
  alias Sykli.Executor.TaskResult
  alias Sykli.FailureSemantics
  alias Sykli.Graph.{Review, Task}
  alias Sykli.Graph.Task.Gate
  alias Sykli.ReviewPrimitive.Result
  alias Sykli.Target.Local

  defmodule FailingApiBreakageRunner do
    def evaluate(_task, _state, _opts) do
      {:error,
       %Result{
         review_type: "api_breakage",
         status: :failed,
         severity: "breaking",
         message: "public API breakage detected",
         tool: "fixture",
         findings: [%{"symbol" => "Client.connect", "change" => "removed"}],
         evidence: %{}
       }}
    end
  end

  setup do
    original_runner = Application.get_env(:sykli, :api_breakage_review_runner)
    original_gate = System.get_env("SYKLI_TEST_FAILSEM_GATE")

    on_exit(fn ->
      if original_runner do
        Application.put_env(:sykli, :api_breakage_review_runner, original_runner)
      else
        Application.delete_env(:sykli, :api_breakage_review_runner)
      end

      case original_gate do
        nil -> System.delete_env("SYKLI_TEST_FAILSEM_GATE")
        value -> System.put_env("SYKLI_TEST_FAILSEM_GATE", value)
      end
    end)

    :ok
  end

  test "gate denial classifies the task as policy_block (source: gate)" do
    System.put_env("SYKLI_TEST_FAILSEM_GATE", "denied")

    task = %Task{
      name: "approve-deploy",
      kind: :task,
      command: nil,
      depends_on: [],
      inputs: [],
      outputs: %{},
      success_criteria: [],
      gate: %Gate{strategy: :env, env_var: "SYKLI_TEST_FAILSEM_GATE", timeout: 5}
    }

    assert {:error,
            [
              %TaskResult{
                status: :failed,
                failure_semantics: %FailureSemantics{
                  class: :policy_block,
                  source: :gate,
                  retryable: false,
                  reason: "gate_denied"
                }
              }
            ]} = Executor.run([task], graph([task]), target: Local)
  end

  test "review primitive failure classifies the task as contract_failure (source: executor)" do
    Application.put_env(:sykli, :api_breakage_review_runner, FailingApiBreakageRunner)

    task = %Task{
      name: "review-api",
      kind: :review,
      command: nil,
      depends_on: [],
      inputs: [],
      outputs: %{},
      services: [],
      task_inputs: [],
      review: %Review{
        primitive: "api_breakage",
        agent: "local",
        context: ["lib/**/*.ex"],
        deterministic: true
      }
    }

    assert {:error,
            [
              %TaskResult{
                status: :failed,
                failure_semantics: %FailureSemantics{
                  class: :contract_failure,
                  source: :executor,
                  retryable: false,
                  reason: "review_primitive_failed"
                }
              }
            ]} = Executor.run([task], graph([task]), target: Local)
  end

  defp graph(tasks), do: Map.new(tasks, &{&1.name, &1})
end
