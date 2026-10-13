# Sykli for coding agents

For a declared graph (`sykli.json`), the loop is `sykli plan --changed PATH
--explain --json` to learn which tasks a change requires, `sykli run --json` to run them
and receive a `sykli-receipt.v1`, and `sykli verify RECEIPT` to check that a
receipt still describes the tree in front of you. Exit codes are the answer:
`run` 0 all passed or cached, 1 a task failed, 2 could not evaluate; `verify`
0 verified, 1 the work failed, 2 cannot verify, 3 the tree or inputs changed,
4 the contract changed. A verified receipt is consistent, not proven: verify
establishes a valid schema, consistent records, the contract's identity, and
the tree and every declared input's bytes and execute bit, read from the
checkout (or, with `--commit`, from the commit at each input's exact path,
against what the receipt records per task under `tasks[].inputs`), never from
the receipt; it does not establish where the receipt came from, who produced
it, or that its tasks ran. Receipts are unsigned, so a receipt edited
consistently, such as an outcome rewritten to passed, verifies. With
`--commit`, a receipt whose run read an input the commit cannot show at its
path (an ignored or untracked file, a name spelled differently from git's, a
file inside a submodule or reached through a symbolic link to a directory, a
symbolic link to an undeclared file) exits 3;
declare inputs as git spells them. Write receipts under `.sykli/` or outside
the tree; a receipt inside the tree changes the tree it describes. A receipt
binds a tree, not a commit: `sykli verify RECEIPT --commit REV` judges the
receipt as evidence for that commit, reading the contract, the lock and
every input from `REV`, none from the checkout. It requires a passing run of
exactly `REV^{tree}` (a pre-commit run counts; it need not be clean), exits 3
when it is not one, and prints the first failing check on stderr.

Evidence of accepted pushes outlives the worktree that produced it. When
`sykli hook pre-push` accepts a push to the default branch, it copies the
receipt that verified it to
`$(git rev-parse --git-common-dir)/sykli/receipts/rcpt_<sha256 of the receipt>.json`,
shared by every worktree of the repository. To check later that a pushed
commit was verified, look there for a receipt whose `subject.tree_oid` is
`git rev-parse COMMIT^{tree}` and run
`sykli verify RECEIPT --contract sykli.json --commit COMMIT` from any checkout
of the repository. Always run that verify: the directory confers no trust. A
kept receipt is evidence for the commit, not proof that the commit reached the
remote, since the hook runs before the remote answers and also for
`git push --dry-run`; `git merge-base --is-ancestor COMMIT REMOTE/BRANCH` says
whether it did. The directory keeps the newest `SYKLI_RECEIPT_BUDGET` receipts
(default 200); keeping a receipt again makes it the newest.

The contract need not be in the repository. Every command that takes one
(`init --contract`, `run`, `lock`, `plan`, `validate`, `verify`,
`hook pre-commit --contract`, `hook pre-push --contract`) accepts a path
outside it, such as a person's own configuration directory. The repository is
then the git toplevel of the current directory, never the contract's
directory; declared paths are from that root, the lock is the `sykli.lock`
beside the contract as it is named (beside the link, for a symbolic link;
sykli warns when only the link's target has one), and receipts and the cache
stay in the repository's `.sykli/`. `init --contract` touches nothing in the
repository: it neither creates `.sykli/` (run `mkdir -p .sykli` before
redirecting a receipt there) nor adds it to `.gitignore`. A task may depend
only on repository files, which it declares, and tools on `PATH`: files kept
beside an outside contract cannot be declared and are not hashed, so changing
one leaves cached passes standing. `verify` reads such a contract and its
lock from their files, with or without `--commit`; with `--commit` the
commit's tree is still checked. Every receipt records `lock_digest`, the
SHA-256 of the lock file it ran with (null without one), and for a contract
outside the repository verify requires it to match the lock file beside the
contract now: a receipt from another version of the contract or its lock
exits 4. With no lock on either side they match, and the contract hash alone
binds the receipt. The digest covers the whole lock file, one entry per
contract in its directory, so keep one contract per directory.
`hook pre-push --contract PATH --branch NAME REMOTE URL` gates pushes to
exactly the named branches (repeat `--branch` for more; it also works without
`--contract`) with that contract, whatever the pushed commits carry, and keeps
the accepted receipts in the common git directory as above. Without
`--branch` the hook guesses the default branch from
`refs/remotes/REMOTE/HEAD`, else `main` and `master`, and treats a HEAD naming
a ref that no longer exists as unknown. Inside or outside is decided on
resolved paths, so a symbolic link in the repository to a contract elsewhere
counts as outside; a contract inside the worktree that the pushed commit does
not carry can never be verified, and the hook says so.

Every command that looks for the repository accepts `--repo-root` and
`--git-common-dir` (both, as `git rev-parse --path-format=absolute
--show-toplevel --git-common-dir` prints them); with them sykli runs no git
to find the repository; the root's `.git` must lead to the common directory
(checked from files, or exit 2). `hook pre-push` then starts one git process, one
`git cat-file --batch` for all pushed commits, `verify --commit` one, and
`run` five; without them each starts one more.

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

Every graph, Cargo or not, has a second prerequisite, a tripwire for one
pattern: no undeclared sibling of a declared input. Every tracked file (in the
index, so a new file counts once it is `git add`ed) directly inside a folder
that holds one of a task's declared inputs must be one of that task's inputs
too. A test that reads a whole fixture folder otherwise keeps its cached pass
after a fixture is added. Only the repository root is exempt; a task's working
directory is checked like any other folder. `run` exits 2 naming each file and
its task, and explain lists them as `folder_missing_inputs: [{path, task}]`
under `input_coverage` (absent when there are none), marks task cache states as
input errors and exits 2. Declare the file in that task's inputs, or move it
out of the folder, and re-lock.

It is not a dependency scan, and it does not catch: files in new subfolders of
an input folder, or in its parent folders; anything in the repository root;
untracked files (so editor and OS scratch files never refuse a run; the
pre-commit hook refuses commits while untracked files exist), ignored or
generated files; or anything outside the repository. A task that walks a
folder tree recursively, or globs a folder in which it declares nothing, still
needs its inputs declared by hand.
It can require explicit inputs for unused fixtures. It does not inspect ignored
generated files, arbitrary build-script reads, external tools or files outside
the Git inventory, nor prove each input belongs to every task that reads it.
Git discovery errors, invalid base refs and unsupported non-UTF-8 paths return
exit 2, never an empty successful coverage result. Commands without `--explain`
keep their existing output. This diagnostic does not modify the graph.

The same Cargo-graph test decides one more thing (sykli#64): the compiler is
part of every task's runtime. `cargo` is usually rustup's proxy, and moving
rustup's default changes no file or `PATH` entry, so `run` and explain both
fold what the task shell resolves `rustc -vV` and `cargo -V` to (in each
workdir, with the tasks' environment) into the runtime fingerprint. A pass
recorded under one compiler is `missing` under another. A Cargo graph whose
task shell cannot run them cannot be evaluated (exit 2).

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
