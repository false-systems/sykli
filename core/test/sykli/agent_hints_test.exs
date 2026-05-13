defmodule Sykli.AgentHintsTest do
  use ExUnit.Case, async: true

  alias Sykli.{AgentHints, FailureSemantics}

  test "runtime failures point agents at the target and preserve retryability" do
    hints =
      AgentHints.from_failure_semantics(
        FailureSemantics.timeout("task_timeout", "task timed out")
      )

    assert hints["retry_may_help"] == true
    assert hints["inspect_target"] == true
    refute hints["inspect_contract"]
    refute hints["requires_human_decision"]
  end

  test "criteria failures point agents at the contract" do
    hints =
      AgentHints.from_failure_semantics(
        FailureSemantics.criteria_failure("success_criteria_failed", "criteria failed")
      )

    assert hints["inspect_contract"] == true
    refute hints["inspect_target"]
    refute hints["retry_may_help"]
  end

  test "unsupported targets expose target and contract inspection paths" do
    hints =
      AgentHints.from_failure_semantics(
        FailureSemantics.unsupported_target("unsupported_success_criteria", "unsupported")
      )

    assert hints["inspect_target"] == true
    assert hints["inspect_contract"] == true
  end

  test "policy blocks require a human decision without claiming retry value" do
    hints = AgentHints.from_failure_semantics(FailureSemantics.policy_block("gate", "blocked"))

    assert hints["requires_human_decision"] == true
    refute hints["retry_may_help"]
  end

  test "unknown classifications remain explicit but conservative" do
    hints = AgentHints.from_failure_semantics(FailureSemantics.unknown("raw", "unknown"))

    assert hints == %{
             "retry_may_help" => false,
             "inspect_target" => false,
             "inspect_contract" => false,
             "inspect_dependencies" => false,
             "requires_human_decision" => false,
             "unknown_failure_class" => true
           }
  end

  test "unrecognized future classifications preserve retryability and mark the gap" do
    hints =
      AgentHints.from_failure_semantics(%{
        "class" => "future_failure",
        "retryable" => true,
        "source" => "target",
        "reason" => "future",
        "message" => "future"
      })

    assert hints["retry_may_help"] == true
    assert hints["unknown_failure_class"] == true
    refute hints["inspect_target"]
  end

  test "malformed hint input returns nil instead of looking like success" do
    assert AgentHints.from_failure_semantics(%{"reason" => "missing-class"}) == nil
  end
end
