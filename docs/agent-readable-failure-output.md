# Agent-Readable Failure Output

Sykli exposes typed failure facts to agents without asking the agent to parse
human prose.

The stable facts are:

- `failure_semantics`: normalized result classification produced after
  execution.
- `contract_slice`: compact task contract context when available.
- `success_criteria_results`: evaluated criteria results when criteria ran.
- `agent_hints`: conservative booleans derived only from `failure_semantics`.

`agent_hints` is not a diagnosis engine. It does not infer root cause and does
not propose a fix. It only exposes which follow-up paths are semantically valid:

```json
{
  "retry_may_help": false,
  "inspect_target": true,
  "inspect_contract": false,
  "inspect_dependencies": false,
  "requires_human_decision": false
}
```

Current derivation:

- `runtime_failure`: inspect the target; retry only when the failure semantics
  says it is retryable.
- `criteria_failure` and `contract_failure`: inspect the declared contract.
- `unsupported_target`: inspect both the target and the declared contract.
- `timeout`: inspect the target; retry only when marked retryable.
- `dependency_failure`: inspect dependencies.
- `policy_block`: a human decision path is required.
- `unknown`, `internal_error`, and `skipped`: no strong hint is emitted beyond
  explicit false values.

This is V1. It does not implement natural-language recommendations,
`evidence_required`, risk/effects, agent variance, failure-mode learning, or new
SDK contract fields.
