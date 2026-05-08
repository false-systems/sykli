# Schema-Invalid Conformance Fixtures

Each JSON file in this directory is expected to fail validation against
`schemas/sykli-pipeline.schema.json`. The validation script treats a passing
fixture as a test failure.

These fixtures assert structural schema rules, not runtime support decisions.
For example, a target/runtime combination may be unsupported at execution time
while still being schema-valid.

## Fixture Intent

| Fixture | Rule asserted |
|---------|---------------|
| `depends-on-wrong-type.json` | `depends_on` must be an array of strings. |
| `gate-invalid-strategy.json` | `gate.strategy` must be one of the schema enum values (`prompt`, `env`, `file`, `webhook`). |
| `gate-missing-strategy.json` | `gate.strategy` is required when a gate object is present. |
| `k8s-extra-field.json` | `k8s` rejects fields outside the canonical `{memory,cpu,gpu,raw}` shape. |
| `review-with-command.json` | Review nodes must not carry executable task fields such as `command`. |
| `review-with-task-type.json` | Review nodes must not carry executable-task `task_type`. |
| `service-extra-field.json` | Service objects reject unknown fields. |
| `service-missing-image.json` | Service objects require `image`. |
| `success-criteria-duplicate-exit-code.json` | A task may declare at most one `exit_code` criterion. |
| `success-criteria-exit-code-missing-equals.json` | `exit_code` criteria require `equals`. |
| `success-criteria-file-missing-path.json` | File criteria require `path`. |
| `success-criteria-unknown-type.json` | Success criterion `type` is a closed enum. |
| `success-criteria-version-1.json` | `success_criteria` is rejected under version `"1"`. |
| `success-criteria-version-2.json` | `success_criteria` is rejected under version `"2"`. |
| `task-type-unknown.json` | `task_type` is a closed enum. |
| `task-type-version-1.json` | `task_type` is rejected under version `"1"`. |
| `task-type-version-2.json` | `task_type` is rejected under version `"2"`. |
| `task-type-wrong-type.json` | `task_type` must be a string enum value. |
| `task-with-review-primitive.json` | Normal executable tasks must not carry review-node `primitive`. |
| `version-*.json` | Top-level `version` is required, string-typed, non-empty, and explicitly supported. |
| `when-and-condition.json` | `when` and `condition` are mutually exclusive aliases. |

