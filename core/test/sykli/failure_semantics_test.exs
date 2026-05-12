defmodule Sykli.FailureSemanticsTest do
  use ExUnit.Case, async: true

  alias Sykli.Error
  alias Sykli.FailureSemantics

  test "classifies command failure separately from criteria failure" do
    command = Error.task_failed("test", "exit 1", 1, "")
    criteria = Error.success_criteria_failed("test", [])

    assert %FailureSemantics{class: :runtime_failure, source: :target, retryable: false} =
             FailureSemantics.for_error(command)

    assert %FailureSemantics{class: :criteria_failure, source: :criteria, retryable: false} =
             FailureSemantics.for_error(criteria)
  end

  test "classifies unsupported criteria and timeouts distinctly" do
    unsupported = Error.unsupported_success_criteria_for_target("test", "k8s", [])
    timeout = Error.task_timeout("test", "sleep 10", 1_000)

    assert %FailureSemantics{class: :unsupported_target, reason: "unsupported_success_criteria"} =
             FailureSemantics.for_error(unsupported)

    assert %FailureSemantics{class: :timeout, reason: "task_timeout", retryable: true} =
             FailureSemantics.for_error(timeout)
  end

  test "classifies skipped and dependency-blocked results" do
    assert %FailureSemantics{class: :skipped, reason: "condition_not_met"} =
             FailureSemantics.for_result(:skipped, nil)

    assert %FailureSemantics{class: :dependency_failure, source: :dependency} =
             FailureSemantics.for_result(:blocked, :dependency_failed)
  end

  test "serializes to stable string-keyed map" do
    semantics = FailureSemantics.timeout("task_timeout", "task timed out", %{duration_ms: 1000})

    assert FailureSemantics.to_map(semantics) == %{
             "class" => "timeout",
             "retryable" => true,
             "source" => "target",
             "reason" => "task_timeout",
             "message" => "task timed out",
             "details" => %{"duration_ms" => 1000}
           }
  end
end
