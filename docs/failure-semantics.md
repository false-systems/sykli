# Typed Failure Semantics

Sykli records a normalized `failure_semantics` object beside existing task
status and error fields. This is not a pipeline SDK field and does not change
the contract schema. It is result metadata produced by the executor so humans
and agents can tell what kind of law was violated.

Shape:

```json
{
  "class": "criteria_failure",
  "retryable": false,
  "source": "criteria",
  "reason": "success_criteria_failed",
  "message": "task 'test' failed success_criteria",
  "details": {
    "code": "success_criteria_failed",
    "task": "test",
    "step": "run"
  }
}
```

V1 classes:

- `runtime_failure` — command or target execution failed.
- `contract_failure` — non-criteria contract behavior failed, such as a review
  primitive result.
- `criteria_failure` — the command completed successfully, but declared
  `success_criteria` failed.
- `unsupported_target` — the selected target cannot evaluate the declared
  contract.
- `timeout` — execution or a gate timed out.
- `dependency_failure` — the task could not run because required upstream work,
  inputs, or configuration were missing.
- `policy_block` — gate or policy logic blocked progress.
- `skipped` — the task intentionally did not run.
- `internal_error` — Sykli itself could not complete the step.
- `unknown` — Sykli cannot classify this result yet.

Reserved for later contract slices:

- `missing_evidence`
- `agent_variance_failure`

V1 writes failure semantics to executor task results, run history, enriched
FALSE Protocol occurrences, CLI JSON run output, generated context, and MCP run
tool output where those paths already expose task result data.

Non-goals:

- no SDK or pipeline schema fields
- no `evidence_required`
- no agent metadata or expected variance
- no gate decision type split
- no failure-mode learning store
