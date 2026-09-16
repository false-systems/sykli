# Sykli for coding agents

For a declared graph (`sykli.json`), the loop is `sykli plan --changed PATH
--explain --json` to learn which tasks a change requires, `sykli run --json` to run them
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

Explain also checks **input coverage**, independently of that display filter.
Requested by Yair after the remote-workers trial: an undeclared source file
must remain visible even when another changed input selects every task.
It compares the working tree with HEAD (including staged changes), adds
untracked non-ignored files, and includes every supplied `--changed` hint.
Use `sykli plan --explain --base origin/main --json` to include committed
branch changes relative to that exact ref. `--base` requires `--explain` and
does not filter task selection; it is not an implicit merge-base calculation.
With unborn HEAD, tracked/index paths and untracked files are considered.
Discovered `.sykli/` state is excluded; explicit hints can name ignored paths.

The optional `input_coverage` object on `sykli-plan.v1` contains the resolved
`base_commit` (null for unborn HEAD) and a sorted `paths` array. Each entry has
a repository-relative `path` (absolute for external hints), `kind`, and `tasks`:

- `declared_input`: names every task with that exact input, respecting workdir.
- `evaluation_metadata`: the selected JSON contract or its sibling sykli.lock,
  unless it is itself a declared input.
- `unmapped`: no task declares this path. Text output prints a warning even
  when another changed file selected every task or valid cache entries exist.

For an unmapped source/configuration file, inspect which tasks read it, update
their inputs, and run `sykli lock sykli.json`. Documentation is not automatically
exempt in this CLI diagnostic. An unmapped path requires investigation, not
proof that the file affects a task. Likewise, mapping a path to one task does
not prove all consumers are covered. External tools and unchanged missing
dependencies are outside this changed-path check.

General unmapped-path warnings keep exit 0. Cargo-root graphs have an additional
mandatory prerequisite, requested by Yair after the remote-workers false-pass
test: every tracked or untracked non-ignored Rust source, Cargo manifest/lock,
known Rust/Cargo configuration file, and file beneath a `tests` directory must
appear in at least one task's inputs. This inventory is discovered afresh on
every invocation, including committed files absent from the changed-path view;
it is never itself cached against the existing input list. `.sykli/` is excluded.
`run` exits 2 before executing tasks or reusing/writing cached passes when inputs
are missing. Explain retains its JSON with `cargo_missing_inputs` under
`input_coverage`, marks task cache states as input errors and exits 2, even if
the task filter selected nothing. Re-locking alone does not fix missing inputs.

This conservative check applies when the working directory has Cargo.toml and
the graph declares that manifest or names `cargo` in a task command. It does
not turn an unrelated shell-only graph into a Cargo graph. Shell wrappers that
hide Cargo invocation must declare their Cargo manifest to opt into the check.
It can require explicit inputs for unused fixtures. It does not inspect ignored
generated files, arbitrary build-script reads, external tools or files outside
the Git inventory, nor prove each input belongs to every task that reads it.
Git discovery errors, invalid base refs and unsupported non-UTF-8 paths return
exit 2, never an empty successful coverage result. Commands without `--explain`
keep their existing output. This diagnostic does not modify the graph.

To inspect a contract edit before accepting it, use
`sykli plan --explain --preview --json`. Preview reads the edited JSON despite
lock drift and labels the result with `contract_preview.pinned_hash` and
`matches_lock`. It runs no task or emitter and writes no lock. Ordinary planning
and execution still enforce the lock; malformed locks still fail. Preview does
not suppress Cargo completeness errors. After review, use `sykli lock sykli.json`.

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
