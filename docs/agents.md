# Sykli for coding agents

For a declared graph (`sykli.json`), the loop is `sykli plan --changed PATH
--json` to learn which tasks a change requires, `sykli run --json` to run them
and receive a `sykli-receipt.v1`, and `sykli verify RECEIPT` to check that a
receipt still describes the tree in front of you. Exit codes are the answer:
`run` 0 all passed or cached, 1 a task failed, 2 could not evaluate; `verify`
0 verified, 1 the work failed, 2 cannot verify, 3 the tree or inputs changed,
4 the contract changed. Write receipts under `.sykli/` or outside the tree;
a receipt inside the tree changes the tree it describes.

Use `sykli plan --explain` to see why tasks are selected and inspect current
cache evidence. It reads `sykli.json` automatically, even if `sykli.rs` also
exists; an explicit JSON path selects a different contract. Missing JSON
contracts require `sykli init`. Explain rejects Rust emitters and
`--target`, runs no task or emitter commands, and creates no cache entries,
receipts or restored outputs. Git and shell discovery still invoke those
tools, so explain requires Git and the task shell.

Add `--json` for the existing `sykli-plan.v1` document with an optional
`explanations` array. Without `--explain`, output is unchanged. Each
explanation names a `task`, its `selection` reasons, and its `cache` state.
Selection codes are `all_tasks`, `changed_input` (with a declared `path`),
and `affected_dependency` (with a `task`). Multiple reasons may apply.
`--changed` is a caller-supplied hint, not a Git diff or cache invalidation;
it filters the displayed tasks and does not restrict what `run` executes.

Cache states:

- `available`: source `receipt` and cached artifacts validate. Restoration
  has not been attempted and may still fail during execution.
- `missing`: `code: entry_missing` for the current content key. This does
  not establish which input changed or identify a previous run.
- `invalid`: `code` is `entry_unreadable`, `entry_invalid`,
  `provenance_invalid`, `artifact_missing`, `artifact_unreadable`, or
  `artifact_invalid`. Execution falls back to running the task.
- `deferred`: named `dependencies` need execution, output restoration,
  or resolution of an input error before this task can be evaluated.
  Ancestors are inspected even when omitted by the display filter. When
  an ancestor has a definite input error, `input_errors` maps its task name
  to the original error message, including through transitive dependencies.
- `input_error`: a `message` describes an input that cannot be evaluated
  now. A missing generated input is deferred when its dependency is pending.

Exit 0 means an explanation was produced, including misses, invalid cache
entries and deferred decisions. Exit 2 means evaluation failed; definite
input errors in displayed tasks or their ancestors retain the JSON plan
and its other explanations. Errors in unrelated, omitted tasks do not
change the exit code. Contract or runtime errors use stderr and produce no plan.
Explanations describe current local evidence, not a promised run outcome:
files, environment or cache contents can change, and task side effects are
not simulated. Given the same task key and cache contents, planning and
execution share the same evidence validation.

The repository's [CI shadow experiment](ci-shadow.md) compares changed-path
selection and baseline cache evidence with an independent full run. Its
observations never enable skipping or decide the required CI result.

For typed artifact production, start with `sykli targets --json` and
`sykli plan sykli.production.json --target TARGET --json`. Use the discovered
target name (`sykli` in this repository), not an assumed `app` alias.
`sykli produce TARGET --prepare --summary --json` captures the request without
executing it. Keep its production ID. A fresh worker can inspect
`sykli status ID --summary --json`, then select ready work with
`sykli resume ID --operation NAME --summary --json` or finish all remaining work
with `sykli resume ID --jobs 2 --summary --json`.
The same local store is sufficient; no chat handover is required. Compact state
omits command captures; retrieve them with `sykli diagnostics ID ATTEMPT --json`.
Read `assessment` and `delivery` separately: an available artifact can still
have unfinished checks, and a failed operation needs an explicit `--retry`.

To learn what a pull request has established before acting on it, run
`sykli inspect --repo OWNER/NAME --pr N --requirements FILE --json` and read the
embedded `sykli-assessment.v1`: each unresolved obligation carries a reason code,
the evidence it would need (`missing`), and source references into the saved
bundle. Replay or explain it offline with `sykli assess BUNDLE --requirements FILE
--why ID`. The result is advisory and never a merge authorization.
