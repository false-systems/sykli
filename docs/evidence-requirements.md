# Evidence Requirements

Evidence requirements are the v4 contract field for proof refs.

`success_criteria` says what condition must be true. `evidence_required` says
what proof reference must exist after the task runs. Sykli stores refs and small
status records, not evidence bytes.

## Contract Shape

Executable tasks may declare:

```json
{
  "evidence_required": [
    {
      "type": "file",
      "name": "coverage",
      "required": true,
      "visibility": "local",
      "predicate": "non_empty",
      "ref_pattern": "coverage.out"
    }
  ]
}
```

Rules:

- Requires pipeline `version: "4"`.
- Applies only to executable tasks.
- Review nodes must not declare it.
- `required` defaults to `true`.
- `visibility` defaults to `local`.
- File evidence requires `ref_pattern`.
- File `predicate` is `exists` or `non_empty`.

Allowed types are `file`, `log`, `attestation`, `occurrence`, `metric`,
`test_report`, `artifact_ref`, and `custom`.

## Runtime V1

V1 evaluates only local file evidence on the local shell target. The local
target resolves `ref_pattern` relative to the task workdir and records a
reference when satisfied.

Unsupported evidence types, containers, and targets fail explicitly with
`unsupported_evidence_requirement_for_target`. Missing required proof fails with
`missing_evidence` and `failure_semantics.class = missing_evidence`.

This is deliberately strict. Sykli should not pretend a target checked evidence
it cannot see.

## Output

Task results may include `evidence_results`:

```json
[
  {
    "type": "file",
    "name": "coverage",
    "status": "satisfied",
    "message": "required file evidence coverage exists",
    "required": true,
    "evidence_ref": {
      "type": "local_ref",
      "uri": "file:///repo/coverage.out",
      "summary": "file evidence coverage",
      "visibility": "local"
    },
    "target": "local"
  }
]
```

The same results are propagated through run history, occurrence enrichment, CLI
JSON, context/query/report data, and MCP task maps where task result data is
already present.

## Non-Goals

V1 does not implement remote evidence upload, artifact storage, prompt/log
capture, advanced predicates, risk policy, or agent variance. Those depend on
the same reference-only discipline.
