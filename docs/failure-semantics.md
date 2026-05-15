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
- `timeout` — task execution timed out.
- `dependency_failure` — the task could not run because required upstream work,
  inputs, or configuration were missing.
- `policy_block` — gate or policy logic blocked progress.
- `skipped` — the task intentionally did not run.
- `internal_error` — Sykli itself could not complete the step.
- `unknown` — Sykli cannot classify this result yet.
- `missing_evidence` — required declared evidence was missing.

Reserved for later contract slices:

- `agent_variance_failure`

`source` is a coarse provenance tag, distinct from `class`. Valid values are
`executor`, `target`, `criteria`, `gate`, `dependency`, `system`, and
`unknown`.

`retryable` is conservative. It defaults to `false` for every constructor
except `timeout/3`, which defaults to `true`. This field is the only signal
`agent_hints` uses to set `retry_may_help`.

`Sykli.Error` codes map to failure classes as follows:

- `task_failed` -> `runtime_failure`
- `success_criteria_failed` -> `criteria_failure`
- `unsupported_success_criteria_for_target` -> `unsupported_target`
- `missing_evidence` -> `missing_evidence`
- `unsupported_evidence_requirement_for_target` -> `unsupported_target`
- `task_timeout` -> `timeout`
- `review_primitive_failed` -> `contract_failure`
- `missing_secrets` -> `dependency_failure`
- any `Sykli.Error` with `type: :internal` -> `internal_error`
- any other `Sykli.Error` -> `unknown`

When failure semantics are produced from a `Sykli.Error`, `details` may contain
the string keys `code`, `task`, `step`, `exit_code`, and `duration_ms`. Other
keys may appear for class-specific constructors. Consumers should treat unknown
keys as opaque.

V1 writes failure semantics to executor task results, run history, enriched
FALSE Protocol occurrences, CLI JSON run output, generated context, and MCP run
tool output where those paths already expose task result data.

Non-goals:

- no SDK or pipeline schema fields
- no agent metadata or expected variance
- no gate decision type split
- no failure-mode learning store
