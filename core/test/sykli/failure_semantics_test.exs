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

  describe "for_result/2 dispatch covers status branches" do
    test "success-like statuses have no failure semantics" do
      assert FailureSemantics.for_result(:passed, nil) == nil
      assert FailureSemantics.for_result(:cached, nil) == nil
    end

    test "skipped statuses remain skipped" do
      assert %FailureSemantics{class: :skipped, reason: "condition_not_met"} =
               FailureSemantics.for_result(:skipped, nil)

      assert %FailureSemantics{class: :skipped, reason: "manual_skip"} =
               FailureSemantics.for_result(:skipped, :manual_skip)
    end

    test "blocked statuses remain dependency failures" do
      assert %FailureSemantics{class: :dependency_failure, reason: "dependency_failed"} =
               FailureSemantics.for_result(:blocked, :dependency_failed)

      assert %FailureSemantics{class: :dependency_failure, reason: "missing_artifact"} =
               FailureSemantics.for_result(:blocked, :missing_artifact)
    end

    test "Sykli.Error values are classified by error code before status fallback" do
      error = Error.task_timeout("slow", "sleep 10", 1_000)

      assert %FailureSemantics{class: :timeout, reason: "task_timeout"} =
               FailureSemantics.for_result(:errored, error)
    end

    test "non-error errored and failed values stay calibrated" do
      assert %FailureSemantics{class: :internal_error, reason: "process_exit"} =
               FailureSemantics.for_result(:errored, :process_exit)

      assert %FailureSemantics{class: :unknown, reason: "raw_failure"} =
               FailureSemantics.for_result(:failed, :raw_failure)
    end
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

  test "to_map/1 passes through an already-serialized string-keyed map" do
    already_serialized = %{
      "class" => "criteria_failure",
      "retryable" => false,
      "source" => "criteria",
      "reason" => "success_criteria_failed",
      "message" => "criteria failed"
    }

    assert FailureSemantics.to_map(already_serialized) == already_serialized
  end

  describe "for_error/1 dispatch covers every named error code" do
    # If any Sykli.Error.* factory renames its code, the corresponding test
    # here fails — preventing a silent fall-through to the :unknown bucket.
    test "task_failed -> runtime_failure" do
      assert %FailureSemantics{class: :runtime_failure} =
               FailureSemantics.for_error(Error.task_failed("t", "exit 1", 1, ""))
    end

    test "success_criteria_failed -> criteria_failure" do
      assert %FailureSemantics{class: :criteria_failure} =
               FailureSemantics.for_error(Error.success_criteria_failed("t", []))
    end

    test "unsupported_success_criteria_for_target -> unsupported_target" do
      assert %FailureSemantics{class: :unsupported_target} =
               FailureSemantics.for_error(
                 Error.unsupported_success_criteria_for_target("t", "k8s", [])
               )
    end

    test "task_timeout -> timeout (retryable)" do
      assert %FailureSemantics{class: :timeout, retryable: true} =
               FailureSemantics.for_error(Error.task_timeout("t", "sleep 10", 1000))
    end

    test "review_primitive_failed -> contract_failure" do
      review_result = %{status: :failed, review_type: "api_breakage", message: "broke"}

      assert %FailureSemantics{class: :contract_failure, source: :executor} =
               FailureSemantics.for_error(Error.review_primitive_failed("review", review_result))
    end

    test "missing_secrets -> dependency_failure" do
      assert %FailureSemantics{class: :dependency_failure, source: :dependency} =
               FailureSemantics.for_error(Error.missing_secrets("t", ["TOKEN"]))
    end

    test "Error{type: :internal} -> internal_error" do
      assert %FailureSemantics{class: :internal_error} =
               FailureSemantics.for_error(Error.internal("BEAM lost its mind"))
    end

    test "Error with unrecognized code -> unknown (calibrated, not guessed)" do
      surprise = %Error{code: "novel_future_code", type: :execution, message: "weird"}

      assert %FailureSemantics{class: :unknown, source: :unknown} =
               FailureSemantics.for_error(surprise)
    end
  end

  describe "reserved future classes load safely" do
    # missing_evidence and agent_variance_failure are declared but not yet
    # produced by any code path. The deserializer uses String.to_existing_atom,
    # so the atoms must be loaded at compile time. This test fails immediately
    # if a future refactor drops them from @classes.
    test "missing_evidence round-trips through from_map" do
      input = %{
        "class" => "missing_evidence",
        "source" => "criteria",
        "reason" => "r",
        "message" => "m"
      }

      assert %FailureSemantics{class: :missing_evidence, source: :criteria} =
               FailureSemantics.from_map(input)
    end

    test "agent_variance_failure round-trips through from_map" do
      input = %{
        "class" => "agent_variance_failure",
        "source" => "executor",
        "reason" => "r",
        "message" => "m"
      }

      assert %FailureSemantics{class: :agent_variance_failure, source: :executor} =
               FailureSemantics.from_map(input)
    end

    test "from_map falls back to :unknown for a class string not in @classes" do
      input = %{
        "class" => "made_up_class",
        "source" => "executor",
        "reason" => "r",
        "message" => "m"
      }

      assert %FailureSemantics{class: :unknown} = FailureSemantics.from_map(input)
    end
  end

  describe "to_map / from_map round-trip preserves all fields" do
    test "timeout with details" do
      original =
        FailureSemantics.timeout("task_timeout", "timed out at 10s", %{
          duration_ms: 10_000,
          attempt: 2
        })

      assert ^original = original |> FailureSemantics.to_map() |> FailureSemantics.from_map()
    end

    test "policy_block without details (empty details map omitted from JSON, restored as empty)" do
      original = FailureSemantics.policy_block("gate_denied", "gate 'review' denied")
      round_tripped = original |> FailureSemantics.to_map() |> FailureSemantics.from_map()

      # to_map drops empty details for compactness; from_map restores it as %{}.
      # Both are semantically empty — assert structural equality on every field.
      assert round_tripped.class == original.class
      assert round_tripped.retryable == original.retryable
      assert round_tripped.source == original.source
      assert round_tripped.reason == original.reason
      assert round_tripped.message == original.message
      assert round_tripped.details == %{}
    end
  end
end
