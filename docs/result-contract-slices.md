# Result Contract Slices

Sykli stores a compact `contract_slice` beside task result evidence. The slice
is not a pipeline SDK field and does not change the contract schema. It is a
post-parse projection of contract fields that already exist, captured so agents
can inspect historical results and see what law applied.

Current slice fields are optional and reference-sized:

- `kind`
- `task_type`
- `semantic`
- `ai_hooks`
- `provides`
- `needs`
- `success_criteria`
- `evidence_required`
- `target`
- `review`
- `gate`

`target` contains `container`, `workdir`, `timeout_seconds`, and `requires`,
omitting keys whose values are nil or empty. `review` contains `primitive`,
`agent`, `context`, and `deterministic`, and is only present for review tasks.

Run history also stores `success_criteria_results` beside the slice. Occurrence
task details include both the declared `success_criteria` in `contract_slice`
and the evaluated `success_criteria_results`. Run history and occurrence task
details also carry `evidence_results` alongside the slice.

The slice intentionally excludes command output, logs, source code, prompts,
artifacts, and raw generated content. Evidence remains local/reference-oriented.

V1 does not add:

- new SDK or pipeline schema fields
- risk/effects
- agent metadata or variance
- gate decision type changes
- failure-mode learning
