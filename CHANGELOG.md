# Changelog

All notable changes to sykli are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[SemVer](https://semver.org/). Every entry names what a receipt, plan, or
contract consumer can observe; internal refactors are not listed.

## [Unreleased]

### Added
- In a Cargo graph, a warning for a document (`*.md`, `LICENSE*`,
  `LICENCE*`, `COPYING*`, `NOTICE*`, anything under `docs/` or `.github/`,
  at any depth) that a string literal in a Rust source names but no task
  declares: `validate` and `run` print `PATH is named in FILE:LINE but not
  declared; declare it if a task reads it` on stderr, `plan --explain` in
  its text and as `named_inputs: [{path, named_at}]` under
  `input_coverage`, and all of them go on. A test that reads `README.md`
  names it so, and so does a help text that says "see AGENTS.md", which
  reads nothing; hence a warning, never a refusal. A top-level document is
  named by its file name alone, a deeper one by its path; a comment or a doc
  comment names nothing.
- `${worktree_cache}` in a task's declared `env` values: a directory that
  belongs to the worktree the run is in,
  `<git-common-dir>/sykli/worktrees/<id>/cache`. `<id>` is 16 hex digits of
  the identity (`sykli-worktree.v1`) of the worktree's incarnation: its top
  level and its own git directory, both resolved, and a token sykli keeps in
  that git directory (`sykli-incarnation`). The directory is made when a run
  needs it, is the same on every run in that worktree, and lies outside the
  work tree, so it changes neither `git status` nor a receipt's tree. A
  worktree removed and made again at the same path gets a new directory, so
  it never inherits the old one's build (which a checkout that restores file
  times would find fresh). The directory belongs to the tasks: sykli's own
  `worktree.json` and `lease` sit beside it in `<id>/`, so a task may empty
  or remove it (`cargo clean`), and the next run makes it again.
  - **For Rust contracts,** declare both
    `"CARGO_TARGET_DIR": "${worktree_cache}/cargo-target"` and
    `"CARGO_BUILD_BUILD_DIR": "${worktree_cache}/cargo-build"`. With one
    target directory for every worktree (`[build] target-dir` in Cargo's
    configuration), a gate in one worktree could run a binary another
    worktree built, and its receipt claimed a pass for code that never ran.
    Cargo found the other worktree's build fresh and built nothing, or another
    build replaced `debug/<name>` while the gate's tests ran. A shared
    `[build] build-dir` (Cargo 1.91 and later) does the same with the
    intermediate artifacts and test binaries, which `CARGO_TARGET_DIR` does
    not move; a Cargo without `build-dir` ignores the second variable.
  - **The cost** is a cold first build in each worktree, and build
    directories per worktree on disk. On Windows their paths are about 50
    characters longer than an in-tree `target\`, which counts against
    `MAX_PATH`. An older sykli hands the placeholder to the task as written,
    and Cargo then builds in a `${worktree_cache}/` directory inside the work
    tree, so adopt it once every runner has this version.
  - **Cache keys cover the value as declared,** never the directory.
    Identical work in two worktrees has one key, a moved worktree keeps its
    passes, and moving a contract from a shared directory to
    `${worktree_cache}` changes its key. Sykli records the directory in no
    receipt and no journal; a task's captured output may name it.
  - **Pruning (Linux and macOS):** a run holds the directory's lease, and so
    do its tasks that name it. When any run is recorded, sykli sets aside the
    directory of every worktree incarnation that is gone (removed, moved,
    made again, or its git directory pruned, and not locked with
    `git worktree lock`), never one whose lease is held. The run deletes what
    it set aside once its work is done. On Windows the directories are made
    but not pruned.
  - Finding the worktree starts no git process. On Windows the expanded path
    is an ordinary absolute path, not a verbatim `\\?\` one.
- A run keeps the receipt it concludes with in its own directory,
  `<git-common-dir>/sykli/runs/<run-id>/rcpt_<sha256>.json`. The copy is
  written durably before the run is recorded concluded, so the evidence of a
  run, detached or not, outlives the worktree that ran it.
  - `status --run` reports it as `receipt.kept` (`path`, `present`) and
    prints it as `kept:`. A receipt gone from its worktree prints as
    `(gone)`.
  - `status --commit` judges it like any receipt of the run. A concluded
    failed run whose receipt is gone from its worktree still counts as a
    failure, unless its kept copy is itself current evidence for the commit.
  - `verify` reads it from there like any receipt; with `--commit`, from any
    worktree of the clone.
  - It is removed when the run is pruned.
- Durable graph runs (Linux and macOS). Every `run` is journaled in
  `<git-common-dir>/sykli/runs/<run-id>/` before any task is dispatched, and
  each task's attempts as they happen (dispatched, started, finished with exit
  or signal and its captured output, interrupted, lost). The run id is
  printed first (`run: ID`) and is the receipt's new `run` field. A task whose
  end was not recorded is reported `lost`, never passed, and not while the
  process group the journal recorded for it still exists (a process that
  closed the attempt's lease, as `sudo` does, is still seen). At most 100
  runs are kept per clone (`SYKLI_RUN_BUDGET`, `0` keeps all); each one
  removed leaves a tombstone in `<git-common-dir>/sykli/pruned-runs/` (at
  most 1000). Once a newer generation has taken a run over, the old
  coordinator's first refused write fences it off and it writes nothing more.
  Journal errors name the file they are about.
- `sykli status --run ID [--json]` (`sykli-run-status.v1`): the run's state
  (`requested`, `running`, `completed`, `interrupted`, `lost`), subject,
  contract and lock digests, each task's state and attempts (an interrupted
  attempt's own exit code or signal, and `stopped_by`, the signal that
  stopped the run), and its receipt (path, in the file system's spelling, and
  digest). Pure: it reads the journal and its locks and writes nothing. A
  pruned run's id answers that it was pruned, and what it was.
- `sykli resume --run ID`: continue a run whose coordinator ended. Recorded
  outcomes are kept; lost and interrupted tasks are retried as new attempts
  that supersede the old ones: at least once, and possibly more than once, and
  `resume` warns for each task it runs again whose last attempt had started.
  Refused while the coordinator or an attempt's process is alive (its lease
  is held, or its process group still exists: `--assume-ended` overrides the
  group check), and the lost record says so), and when the source, contract, lock or runtime changed.
- `sykli run --detach [--json]`: record the run, hand it to a coordinator in
  its own session, and print `{"schema":"sykli-run.v1","run":ID}` once the
  record is durable. The coordinator, and so its tasks, keeps none of the
  caller's descriptors, so a pipe the caller reads ends when `run` returns.
- `sykli status --commit REV [--contract PATH] [--json]`
  (`sykli-commit-status.v1`): every kept receipt of `REV`'s tree judged for
  `REV` as `verify --commit` judges it, split into `current` and `historical`,
  with the runs of that tree and each task's attempts. The `verdict` never
  lets a pass hide a failure: the newest recorded result decides, and a pass
  no run recorded never outranks a recorded failure (`conflict`, with why).
  Each current receipt says whether its run `recorded` it and which tasks it
  `retried`; a run whose journal cannot be read is named in `unreadable_runs`.
- `sykli verify --json` (`sykli-verify.v1`): every check, the first failing
  stage, the exit code and the trust tiers established (`schema`, `contract`,
  `source`, `consistency`) and not established (`outcome`, `provenance`,
  `producer`, `execution`): an outcome is the receipt's claim. Exit codes are
  unchanged. `claimed_outcome` gives the receipt's own outcome as it
  records it (`passed`, `cached`, `failed`, `errored`; null when the receipt
  cannot be read).

- Receipts record each task's declared inputs (`tasks[].inputs`): for each
  input, the SHA-256 of the bytes the run read, its execute bits (`mode`) and
  `object`, the tree entry (`MODE ID`) the run's own snapshot had at the
  input's path, absent for a path not in that tree (an ignored or generated
  file). A missing input is `null`. `mode` is the execute bits as git has
  them: those read from the file system when they agree with the entry (git
  takes an entry's bit from the owner's), else the entry's (`0o111` or 0), as
  on Windows, which has no execute bits, and with `core.filemode=false`, where
  git does not trust them; it is absent only on Windows, for an input whose
  file the run's tree does not show. The records hash to the subject's
  `inputs_digest` by the same formula as before. Additive: the receipt schema
  and cache keys are unchanged (cache keys still use the bits on disk), and
  older receipts are still read. `inputs_digest` changes only where the
  recorded bit now differs from the one on disk: an executable input on
  Windows, or with `core.filemode=false` a bit git does not have. An earlier
  sykli's `verify` of such a receipt against the checkout exits 3; here,
  with `core.filemode=false`, a change git ignores (a `chmod` alone) no
  longer makes a receipt stale for `verify` against the checkout either.
- `verify` prints what a verified receipt establishes, and what it does not,
  after `verified:`, on two lines (`established:`, `not established:`);
  `verify --help` says the same.
- `--repo-root ABS_PATH` and `--git-common-dir ABS_PATH`, accepted by every
  command (given together): a caller that already knows the repository says
  so, and sykli runs no git to find it. With them, `hook pre-push` starts one
  git process, `verify --commit` one and `run` five (`hook pre-push` and
  `verify --commit` one more for each declared input a checkout converts).
  The receipt records the root as given. They are checked without git: the
  root's `.git` must lead to the common directory (a directory, or a
  `gitdir:` file followed through `commondir`), so a subdirectory, or
  another repository's common directory, exits 2.
- Contracts kept outside the repository. `run`, `lock`, `plan`, `validate`
  and `verify` accept a contract path outside the git repository around the
  current directory (decided on resolved paths, so a symbolic link to a
  contract elsewhere counts as outside). The repository is then that
  directory's git toplevel, never the contract's directory: declared paths
  are from its root, and the undeclared-sibling check and the Cargo
  completeness gate apply as usual. Command-line paths (`--changed`, the
  receipt) keep meaning what they meant where they were typed. The lock is
  the `sykli.lock` beside the contract as it is named (beside the link, for
  a symbolic link; a warning says when only the link's target has a lock, so
  the contract runs unpinned), and receipts and the cache stay in the
  repository's `.sykli/`. A task may depend only on repository files and
  tools on `PATH`: files beside an outside contract cannot be declared and
  are not hashed. A contract inside the repository works as before.
- `sykli init --contract PATH` writes the contract it infers from the
  repository, and its lock, to PATH (its directory is created) and touches
  nothing in the repository: no `.sykli/`, no `.gitignore` entry. It
  conflicts with the positional path; outside any git repository it exits 1.
- `sykli hook pre-commit --contract PATH` runs that contract on the staged
  tree, and `sykli hook pre-push --contract PATH REMOTE URL` gates pushes
  with it, whatever the pushed commits carry. A named contract that does not
  exist exits 2; one inside the worktree that the pushed commit does not carry
  is refused with a reason that says to keep it outside the worktree or commit
  it.
- `sykli hook pre-push --branch NAME`, repeatable, with or without
  `--contract`: the gated branches are exactly `refs/heads/NAME` for each, and
  nothing is guessed from `refs/remotes/REMOTE/HEAD`. A value that is not a
  branch name exits 2. Without `--branch`, nothing changes.
- Receipts record `lock_digest`: the SHA-256 of the bytes of the lock file
  beside the contract as the run read it, or null when there was none.
  Receipts without the field are still read.
- `verify` with a contract outside the repository reads it and its lock from
  their files, with or without `--commit` (the `--commit` checks on the
  commit's tree are unchanged), and adds a `lock` check: the receipt's
  `lock_digest` must be the digest of that lock file now. A receipt from
  another version of the contract (`contract`, whose reason names the
  contract's path) or of its lock (`lock`) exits 4, as does a receipt without
  `lock_digest` when the contract has a lock. With no lock on either side,
  the check passes and the contract hash alone binds the receipt. The digest
  covers the whole lock file, so a second contract locked in the same
  directory refuses the first one's receipts; keep one contract per
  directory. A contract inside the repository is verified as before, and its
  lock digest is not compared.
- `sykli hook pre-push` keeps the evidence of the pushes it accepts: once
  every ref of a push to the default branch is accepted, the receipt that
  verified each is copied to
  `$(git rev-parse --git-common-dir)/sykli/receipts/rcpt_<sha256>.json`, so it
  survives the removal of the worktree it was made in. The copy is atomic,
  never written through a symbolic link, and bounded by
  `SYKLI_RECEIPT_BUDGET` (default 200, oldest removed first; keeping a
  receipt again makes it the newest; stale temporary copies are removed). Two
  first pushes at once both keep theirs. A refused push keeps nothing; a copy
  that fails only warns, and the push goes ahead. A kept receipt is evidence
  for the commit, not proof it reached the remote (the hook also runs for
  `--dry-run` and before the remote answers), and it confers no trust:
  readers must run `verify --commit` on it.
- `sykli verify RECEIPT --contract FILE --commit REV` judges a
  receipt as evidence for a commit rather than the checkout. The contract and
  its lock pin are read from `REV`, and nothing is read from the checkout, so
  any commit verifies from any checkout. The receipt's tree must be
  `REV^{tree}` (with `.sykli/` excluded, as in every receipt tree). The tree
  check alone binds the content, so the run need not be clean: the pre-commit
  hook's run counts for the push, and `clean` is reported but does not refuse.
  A miss exits 3, a contract or lock in `REV` that differs exits 4, and a
  revision that names no commit, or a commit without the JSON contract, exits
  2. Declared inputs the commit does not track are not judged. Without
  `--commit`, verify is unchanged and still accepts a dirty receipt for the
  tree on disk.
- A run in a repository whose submodule has uncommitted changes (tracked or
  untracked) is `dirty`, and its receipt's subject records
  `submodules_modified: true`; the tree shows a submodule only as its gitlink.
  `verify --commit` refuses such a receipt (exit 3) on its own check. The
  field is written only when true, so other receipts are unchanged.
- `verify` prints the first failing check to stderr as one line
  (`refused: CHECK expected … but got …`); stdout is unchanged.
- `sykli hook pre-commit`, a git pre-commit entrypoint that checks the tree
  being committed rather than the files on disk. It refuses (exit 1) when a
  tracked file has unstaged changes or an untracked file is not ignored, since
  the run would then check another tree; otherwise it runs and verifies the
  graph and refuses when either fails, or when the tree that ran is not the
  tree the index will commit (`git write-tree`; `git diff` is silent for
  assume-unchanged and skip-worktree files). It stashes nothing. No `sykli.json` or
  `sykli.rs` at the root, or no commit yet, passes with a note.
- `sykli hook pre-push [REMOTE [URL]]`, a git pre-push entrypoint reading
  git's `<local ref> <local sha> <remote ref> <remote sha>` lines. A push that
  updates the remote's default branch (`refs/remotes/REMOTE/HEAD`, else `main`
  and `master`) is refused (exit 1) unless a receipt in this worktree,
  `.sykli/receipt.json` or `.sykli/receipts/*.json`, passes
  `verify --commit` for the pushed commit; the refusal names the command that
  produces one. Deletions and other branches pass. Whether a push is gated is
  decided from the commits: the pushed commit's own contract gates it, and a
  commit without one is refused when the branch it replaces (the line's remote
  sha, else `refs/remotes/REMOTE/BRANCH`) had one; `--no-verify` is the
  deliberate way to take the gate off. A push where neither has a contract
  passes with a one-line note, and an unknown default branch is noted with the
  `git remote set-head` command that names it.
- `sykli hook install [--pre-commit] [--pre-push] [--force]` writes thin
  `#!/bin/sh` hooks (`exec sykli hook NAME "$@"`) into
  `$(git rev-parse --git-common-dir)/hooks`, so every worktree shares them;
  both hooks when neither flag is given. Idempotent. A hook sykli did not
  write, or an edited sykli hook, is replaced only with `--force` (exit 1
  otherwise, nothing written), which keeps the old file as `NAME.pre-sykli`,
  and a `core.hooksPath` that sends git elsewhere is refused.
- `contract_inputs` on a task of a contract kept outside the repository
  (sykli#75): files beside the contract that the task reads, as paths from
  the contract's directory, validated as `inputs` are (relative, no `..`).
  A helper script there could not be declared, so a changed one reused the
  old pass (`cached`, exit 0).
  - Each is read once per run. Its bytes and execute bits join the task's
    cache key, so any change misses the cache; a task that declares none
    keeps the key it had. A missing one errors the task (`input_error`), as
    a missing input does.
  - The task reaches them as `$SYKLI_CONTRACT_DIR/PATH`: a directory of its
    own in the system's temporary directory, holding a copy of exactly the
    declared files, each hashed as it is copied and checked to be the
    recorded bytes, and removed when the task ends. The task runs what was
    hashed, and a helper that reaches a neighbour through
    `$SYKLI_CONTRACT_DIR` finds only declared ones there. A task that
    declares none has no `SYKLI_CONTRACT_DIR`.
  - The copy is its owner's alone. The directory is made exclusively under
    an unguessable name and is 0700. Its files are 0600, or 0700 where the
    owner may execute the original. A name another process holds, such as
    the copy a killed sykli left, is passed over, so a leftover never stops
    a run. The first copy a run makes also removes the user's own copies
    that have stood unchanged for over a day (Linux and macOS).
  - What sykli cannot see is not hashed. A helper that names a neighbour by
    its path, a relative path, another link or case of the directory, a
    `PATH` lookup and an inherited value still reuse a stale pass when the
    file changes. The README lists them: declare every helper and reach it
    only through `$SYKLI_CONTRACT_DIR`.
  - Receipts record them per task as `contract_inputs` (`sha256`, `mode`,
    no tree entry; absent on tasks without any). The subject and its digests
    are unchanged. A run's journal keeps them, so a detached or resumed
    coordinator gives the task the same files, and `resume` refuses a run
    whose contract inputs changed.
  - `verify`, with or without `--commit`, `status --commit` and the
    pre-push hook read each from beside the contract as it is now (no commit
    carries it, as none carries the contract) and refuse a receipt whose
    records differ from it or are missing (exit 3, check `contract inputs`).
  - A contract inside the repository may not declare them: its neighbours
    are repository files, declared as `inputs`. `plan --changed` still
    selects by repository inputs only.

### Breaking
- In a Cargo graph, `run` (exit 2) and `plan --explain` (`cargo_missing_inputs`,
  exit 2) also refuse a contract that leaves out a file git lists (tracked,
  or untracked and not ignored) that a Rust source includes
  (`include_str!`, `include_bytes!`, `include!`) by a string literal,
  resolved from that source's directory as rustc resolves it, or by
  `concat!(env!("CARGO_MANIFEST_DIR"), "/path")`, from the nearest
  `Cargo.toml`. A file built into the binary, or a README a crate's
  documentation includes, kept a cached pass when it changed. Sources are
  read as Rust: an invocation in a string or a plain comment is none, a doc
  comment's literal ones count.
- In a Cargo graph, an include whose path is not a string literal in one of
  those forms (a macro's variable, an expression) refuses `init` (exit 1),
  `validate`, `run` and `plan --explain` (exit 2), naming its file and line,
  since the file it reads cannot be known; it used to be passed over. A path
  built from `OUT_DIR` names a file made at build time and is not refused.
- Inside a git repository, `validate` refuses what `run` refuses before any
  task for undeclared files: the Cargo completeness rule, then the
  undeclared-sibling check. It exits 2, as `run` does, with `run`'s message
  (in `errors` with `--json`; `sykli-validate.v1` is unchanged), where it
  used to exit 0 on a contract `run` would not start; exit 1 stays an
  invalid, unreadable or unpinned contract. Its verdict now depends on the
  work tree too. Outside a git repository nothing changes. A caller that
  reads exit 1 as "re-lock" is still right; exit 2 means "declare the files
  named", which re-locking alone does not fix.
- A declared symbolic link to a directory in the repository, or to nothing,
  is recorded by its own text, as git stores it: its input record carries
  `"link": true` and the text's SHA-256, and its digest is
  `sykli-input-link.v1`. Such a declared input used to error the task
  (`missing or not a file`). Pointing such a link elsewhere now misses the
  cache and the receipt. For a link to a directory, `validate`, `run` and
  `plan --explain` (`link_missing_inputs: [{path, task, link}]`) refuse,
  exit 2, until the same task declares every tracked file under the
  directory. `verify --commit` and the pre-push hook judge a text record by
  the commit's link text, and refuse it where the link leads to a file or
  out of the repository. A link to a file in the repository is read through
  and judged as before.
- A declared symbolic link that leads out of the repository, to a file, a
  directory or nothing there, is refused by `validate`, `run` and
  `plan --explain` before any task (exit 2): no commit holds what it leads
  to, so no receipt of it could verify. A link to a file outside used to
  run, with every receipt then refused by `verify --commit`. `init` leaves
  such a link out with a note, and the Cargo rule and the sibling check do
  not ask for it.
- Where a contract is written or run (`validate`, `lock`, `plan`, `run`,
  `resume`, the pre-commit hook), a declared `env` value that contains `${`
  must name `${worktree_cache}` and close it, and must not contain `$${`
  (reserved for an escape of `${`); and `${worktree_cache}` or
  `$worktree_cache` anywhere but an `env` value (in `run`, `workdir`,
  `inputs`, `outputs` or an env name, where a shell would make it empty) is
  refused. `validate` exits 1, and the others exit 2. Such a value used to
  reach the task literally (sykli never expanded it). A `$` that opens no
  `${` is kept as written (`$ORIGIN`, `$LINENO`). Where a contract is judged
  (`verify`, `status --commit`, the pre-push hook), nothing changes: a
  contract an earlier sykli ran keeps its verdict.
- On Linux and macOS `run` needs a writable common git directory: every run
  is recorded in `<git-common-dir>/sykli/runs/` before any task starts, and
  `run` exits 2 when it cannot write there (with `chmod a-w .git`, an earlier
  sykli exited 0).
- On Linux and macOS a signal no longer kills `run`: it stops the run's tasks
  (TERM, then KILL after 3 s or on a second signal), records the run stopped,
  and exits 128+N. A caller reading the wait status sees an exit code, not a
  death by signal, and the exit takes as long as the tasks take to stop.
- Every receipt written on Linux and macOS carries `run`, so its bytes, and
  therefore its `rcpt_<sha256>.json` name, differ from a receipt of the same
  run before. The schema stays `sykli-receipt.v1`; readers that ignore unknown
  fields (including older `verify`) are unaffected.
- `verify --commit` and `sykli hook pre-push` refuse (exit 3, and the push)
  a receipt whose run read a declared input that the commit cannot show at
  its declared path (sykli#78): an ignored or untracked file, a name spelled
  differently from git's (case, Unicode normalization), a file inside a
  submodule or reached through a symbolic link to a directory, a symbolic
  link to an undeclared file or with a `..` after a name in its target, or
  `.git` declared as an input and read in a linked worktree, where `.git` is
  a file. Such inputs used to pass unjudged. To migrate, drop such paths
  from a contract you verify against commits, declare inputs as git spells
  them, declare a symbolic link's target as well, and declare files a task
  generates as that task's `outputs` (reported as not judged, never
  refused). Plain `verify`, against the checkout, is unaffected.
- `verify --commit` and `sykli hook pre-push` need git 2.40 or later to
  check an input that a checkout converts (line endings, a smudge filter):
  they ask `git --attr-source=COMMIT cat-file --filters`. With an older git
  such a receipt cannot be checked and exits 2; inputs a checkout leaves as
  they are need no newer git.
- `run` refuses (exit 2) an undeclared sibling of a declared input: a tracked
  file (in the index) directly inside a folder holding one of a task's declared
  inputs, which that task does not declare. Such a file could change the
  task's result without changing its key; a test reading a whole fixture
  folder kept a cached pass after a fixture was added. This applies to every
  graph, not only Cargo graphs. Only the repository root is exempt; a task's
  working directory is checked like any other folder. The message names each
  file and its task; `plan --explain` reports them as
  `input_coverage.folder_missing_inputs` (absent when empty), marks the cache
  states as input errors and exits 2. It is a tripwire, not a dependency scan:
  new subfolders, parent folders, the repository root, untracked (so editor
  and OS scratch files never refuse a run), ignored and generated files are
  not checked. Contracts with such gaps must declare the files, or move them,
  and re-lock.
- The GitHub Action verifies twice: the live check against the checkout, as
  before, and then `--commit`, by default against the pull request's head on
  `pull_request` events and against HEAD otherwise (new `commit` input; `none`
  skips the second check). **A workflow that checks out GitHub's merge commit
  now fails verify with exit 3 on pull requests**, and the annotation says the
  merge commit was checked out. Check out `github.event.pull_request.head.sha`
  instead; this repository's CI does.
- Commands shipped in 0.6.0 are removed (see Removed): `targets`, `produce`,
  `verify-production`, `diagnostics`, `inspect` and `assess`, and the
  production modes of `status` (a production ID), `resume` (a production ID),
  `plan` (`--target`) and `init` (`--production`). Each now fails as an
  unknown command or argument (exit 2). `status --run`, `status --commit` and
  `resume --run` are unchanged.
- Where a contract outside the repository is written or run (`validate`,
  `lock`, `plan`, `run`, the pre-commit hook), a task whose `run` or `env`
  value names the contract's own directory, as the contract's path spells it
  or resolved, is refused (`validate` exits 1, the others 2), since sykli
  cannot hash what the task reads there by that name (sykli#75). Declare the
  files in `contract_inputs` and name them as `$SYKLI_CONTRACT_DIR/PATH`.
  The text is read as the task's shell would read it: its `$HOME`, `${HOME}`
  and a leading `~` expanded (its declared `HOME`, else sykli's), quotes and
  `\` escapes removed, and `//` and `/./` read as `/`. A variable just before
  the path does not hide it. When the directory holds the repository, a path
  inside the repository is not refused unless it has a `..` in it. With no
  git repository around the current directory, nothing is refused. Where a
  contract is judged, nothing changes.
- A receipt's `inherited_digests` (and the cache key of a task that
  inherits) is now HMAC-SHA256 of `sykli-inherited.v1`, the name and the
  value, keyed by a secret of the clone's own: 64 hex digits in
  `<git-common-dir>/sykli/inherit-key` (0600 where files have modes), made by
  the first run that needs it and never rewritten. It was the plain SHA-256
  of the value, and CI uploads receipts: a short token could be found by
  trying every candidate against it. A salt recorded in the receipt would be
  as public as the digest, so the key is kept where no receipt goes. The
  field keeps its name and shape (64 hex digits, or `absent`) and changes
  meaning: digests are comparable only between runs of one clone (a fresh
  CI checkout has its own key), and nobody can check one against a value
  they know. `verify` never read the field. Each task that inherits misses
  the cache once; a contract that inherits nothing makes no key, and its
  keys are unchanged. A task's own output is not covered: a value it prints
  is in its captured output and `stdout_digest`.
- Where a contract is written or run (`validate` exit 1; `lock`, `plan`,
  `run` exit 2), `$worktree_cache` without braces in an `env` value is
  refused, and the refusal names `${worktree_cache}`. sykli never expanded
  it: the task got it as written, and a build tool made a directory named
  `$worktree_cache/` inside the work tree while the run passed. A name that
  goes on (`$worktree_cache_dir`) is not the placeholder and is kept.
- `status --run ID --contract PATH` is refused (exit 2, naming both flags).
  `--contract` names the contract `--commit` judges by; with `--run` it was
  accepted and ignored. `status --commit` and `verify --contract` are
  unchanged.
- Inside a git repository, `validate`, `plan --explain` and `run` (exit 2)
  refuse a task whose workdir, declared input or declared output a symbolic
  link on its way leads out of the repository: an input `ext/file.txt` with
  `ext` a link to a directory elsewhere validated, was hashed into the key
  and the receipt from outside, and passed, and an output or a workdir there
  was written outside (sykli#54). Each link on the way is judged as a
  declared link is: by where it resolves, or, when nothing is there yet (an
  output's link before the run that makes it), by where its text leads. It
  is judged against the work tree before any task runs, so a link a task
  makes while it runs is not seen. A link that stays inside the repository
  is not refused.

### Changed
- `sykli init` and `init --contract`: each cargo task (`fmt`, `clippy`,
  `test`) declares every file git tracks under the directory sykli runs in,
  documents included (a test may read a folder of Markdown cases), every
  tracked symbolic link but one that leads out of the repository (and
  the file a link to a file leads to), and the untracked files the Cargo
  completeness rule counts. `init` used to declare Rust sources and Cargo's
  files under `src/`, `tests/` and workspace members only, so it left out a
  vendored path dependency (and `run` refused the contract `init` had just
  written), a file the binary includes, a pin a test reads, and test data
  in a top-level `fixtures/`: a change to any of them kept a cached pass.
  Left out: `.sykli/`, Cargo's top-level `target/` (with a note when git
  lists files there), and sykli's own contract and lock and the contract
  being written. Editing a declared file that nothing reads, a document
  included, now costs a cache miss. Outside a git repository `init` walks the
  files, as before, without hidden entries, symbolic links, `node_modules`
  or `target/`. npm and Go detection are unchanged, and existing contracts
  are not rewritten: run `sykli init --force` (or `init --contract PATH
  --force`) to adopt this.
- The undeclared-sibling check no longer asks for documents (`*.md`,
  `LICENSE*`, `LICENCE*`, `COPYING*`, `NOTICE*`, anything under `docs/` or
  `.github/`, at any depth), in every graph, so a commit that adds
  documents never makes a contract refuse; with `init` declaring a file in
  nearly every folder, any new file there would have. What stays open: a
  new document beside declared ones, or one a hand-written contract leaves
  out, is in no key, so a test that reads its folder with `read_dir` keeps
  its cached pass until the contract declares it (`init --force`).
- On Linux and macOS each task runs in its own process group. Ctrl-C, SIGTERM
  and SIGHUP reach tasks through sykli, which stops them, records them as
  interrupted whatever they exit with (a task that traps the signal and exits
  0 never finished its work, and is never passed), writes no receipt and no
  cache entry, and exits 128+N, instead of dying and leaving them running
  unrecorded. A stop that reaches a task directly as well as sykli (a
  signal to a process group or control group holding both) is covered as far
  as sykli sees it: an attempt that ends after sykli's handler ran, or passes
  less than 100 ms before, is interrupted; a pass already recorded for it,
  and every pass decided after it, is withdrawn (`withdrawn` in `status
  --run`), and a resume runs them again. A run waits until 100 ms have passed
  since the last task it ran passed before it concludes (at most 100 ms per
  run), so the last task is covered too. A failure is never withdrawn. A
  process still in a task's group when its shell exits is killed then, so it
  can no longer hold the task open; one that left the group (`setsid`) is
  not, and a stop does not wait for it.
- A task whose declared inputs change while the run is in progress is
  `errored` with class `input_changed`, not passed or failed: its result is
  not evidence for the tree the run recorded.
- Receipts and cache entries are synced (`fsync(2)`) before they are used. A
  cache entry is no longer deleted before its replacement is renamed into
  place, so concurrent runs storing the same key can no longer fail ("Directory
  not empty") or lose the entry; an entry that still verifies is kept, and a
  restored output is hashed before it replaces anything.
- sykli starts far fewer git processes. The repository is found once per
  command (one `git rev-parse`), `hook pre-push` and `verify --commit` ask
  everything about the commits they judge in one `git cat-file --batch` and
  read receipts, contracts and locks as files, and `run` builds one snapshot
  of the working tree instead of two and lists files for both undeclared-file
  checks in one `git ls-files`. Measured on a small repository: `hook
  pre-push` went from 17 git processes to 2, `verify --commit` from 10 to 2,
  and `run` from 14 to 6. Exit codes, messages and receipts are unchanged;
  `--branch` names are now checked in-process by git's documented rules
  (and `@` alone is refused, as `git branch` refuses it).
- A run inside a git hook shares cached results with the same run made by
  hand. Git runs hooks with `GIT_EXEC_PATH` set and that directory prepended
  to `PATH`, and `PATH` is part of every task's runtime fingerprint, so a
  manual `sykli run` after a hooked commit (or the reverse) re-ran every
  task. When `GIT_EXEC_PATH` is set and is `PATH`'s first entry, that entry
  is dropped from the tasks' environment and their keys. Runs outside a hook
  are unchanged; runs inside one miss the cache once.
- A Cargo graph's cache keys include the Rust toolchain (sykli#64): what the
  task shell resolves `rustc -vV` and `cargo -V` to, in each task workdir,
  joins the runtime fingerprint. A pass recorded under one compiler is no
  longer reused after rustup's default moves. Existing Cargo-graph cache
  entries miss once; other graphs' keys are unchanged. A Cargo graph whose
  task shell cannot run those two commands exits 2 from `run` and
  `plan --explain`.

### Fixed
- A submodule, which is a directory in git's index and cannot be declared,
  no longer makes `run` refuse a contract: the Cargo completeness rule
  required one under a `tests` directory, and the undeclared-sibling check
  one beside a declared input. A tracked symbolic link to a directory in the
  repository, or to nothing, both rules required too, and now it can be
  declared (above); one that leads out of the repository, which cannot
  be, they no longer ask for.
- Durable runs (Linux and macOS): a lease or journal lock sykli lets go of
  is free at once. A `flock` lock belongs to the open file description, and
  a process another of sykli's threads had just forked kept a copy of the
  descriptor until it exec'd, so the lock stayed held that long after sykli
  closed it. Meanwhile the next holder was refused (`lease-busy`, which
  `resume` reports as `run-busy`), a journal append or `status --run` waited
  for the journal lock (and gave up with `journal-busy` after 2 seconds),
  and a released lease read as held. A coordinator forks its tasks from
  parallel threads while other threads append to the journal. sykli now
  unlocks (`LOCK_UN`) whatever it lets go of; a lease handed to a task or
  to a detached coordinator stays held by that process, as before.
- Existing v2 lock files must pin the named contract; empty maps and missing
  entries no longer silently run unpinned, including `verify --commit`.
  `sykli lock` can add the missing entry without discarding sibling pins.
- Journal lock acquisition fails with `journal-busy` after 2 seconds rather
  than hanging on a stopped writer.
- `verify --commit` and the pre-push hook: two forgeries the first fix for
  sykli#78 still let through are refused.
  - A symbolic link whose target has a `..` after a name (`sub/../x`, with
    `sub` itself a link) was judged as the file its names give, while the
    run read the file the file system resolved; a receipt rebound with only
    `tree_oid` edited verified and the hook kept it. Such a link is now
    refused; a `..` may leave only the link's own directories.
  - An input a checkout converts was compared with the checkout's
    `.gitattributes`, so an uncommitted attributes line could make a forged
    receipt verify. It is now compared with what a checkout of the commit
    gives, by the commit's own `.gitattributes`
    (`git --attr-source=COMMIT`); `.git/info/attributes` and
    `core.attributesFile` still apply, as configuration.
- `verify --commit` and the pre-push hook hash each declared input's blob as
  it is read instead of holding it, so sykli's own memory stays at a few
  MB however large an input is (it held each blob whole: 515 MB for a
  512 MB input). `git cat-file` itself may still load a large blob.
- `verify --commit` and the pre-push hook bind a receipt's inputs to the
  commit (sykli#78). They checked that the receipt's `tree_oid` was the
  commit's tree, but took its inputs on trust, so a passing receipt for one
  commit, edited to name a failing commit's tree, verified for it, and the
  hook accepted the push and kept the receipt as evidence. Now every
  declared input is judged against the commit at its exact path, from the
  commit's trees and blobs, read in a second round of the same single
  `git cat-file`. The path is the declared one from the top level, by its
  names alone, never resolved through a symbolic link in the checkout, which
  could point it at another file. The run's recorded tree entry must be the
  commit's, its recorded bytes the commit's blob (or, where a checkout
  converts the file, what a checkout of the commit makes of the blob, by
  its `.gitattributes` and this repository's configuration: one more git
  process per such input, and only for it), and its recorded execute bit
  the entry's. A symbolic link is judged by its entry and by the bytes of
  the declared input it names; a run that read the link's own text, as a
  checkout without symbolic links does, is refused. A receipt with another
  commit's tree and entries copied in, or with an execute bit (or none)
  that the commit does not have, is refused. So is one whose run read a
  file where the commit has nothing at the declared path (an ignored or
  untracked file, a name spelled differently from git's by case or Unicode
  normalization, a file inside a submodule or reached through a symbolic
  link to a directory, a symbolic link to an undeclared file), since the
  commit cannot show what it read. An input neither has is judged absent,
  and an input a task declares as an output is reported as generated and
  not judged (its bytes are the receipt's claim). The receipt's input
  records must also hash to its inputs digest, and without `--commit` each
  record must be what the checkout gives now. Otherwise exit 3, the
  documented tree-or-input mismatch. A receipt written before input records
  is checked by recomputing its inputs digest from the same blobs (no extra
  git process: `verify --commit` and the hook start `rev-parse` and
  `cat-file` for it too), never with an execute bit the commit does not
  have, and is refused (exit 3) when a declared input is not a regular file
  in the commit, a checkout converts it, or it is executable and the run's
  file system had no execute bits, since that cannot be done; run sykli
  again. A receipt edited consistently (an outcome rewritten to passed, or
  a receipt recomputed in full for a commit) still verifies: receipts are
  unsigned, and the README and `verify` now say plainly what verify
  establishes and what it does not.
- `sykli hook pre-push` without `--branch` takes a
  `refs/remotes/REMOTE/HEAD` that names a ref that does not exist (the host
  renamed its default branch and a fetch pruned the old one) as unknown: it
  gates `main` and `master` and always says so. Before, it gated the missing
  branch, so a push to the renamed default branch passed with no word.
- A refusal from `sykli hook pre-push` whose contract differs from its own
  lock names `sykli lock CONTRACT`, not a run that cannot pass until then.
- `verify --commit` with an absolute path to a contract inside the
  repository, spelled through a symbolic link (macOS's `/var` for
  `/private/var`), refused it as outside the repository (exit 2); it is now
  read from the commit.
- A contract kept in a folder beside the files its tasks read
  (`ci/sykli.json` with `ci/sykli.lock`, beside `ci/check.sh`) is no longer
  refused by its own run, `validate` or `plan --explain` (sykli#73): the
  undeclared-sibling check asked for the contract and its lock, and the
  Cargo completeness rule did the same for one under `tests/`. Neither rule,
  nor the one for a declared link to a directory, asks for the JSON
  contract being run or its lock: they are bound by the contract hash and
  the lock digest. A task that reads them still declares them, and any
  other file beside them is still asked for. A `sykli.rs` emitter is a Rust
  source of its crate and is still asked for in a Cargo graph.

### Removed
- Typed production, shipped in 0.6.0: `targets`, `produce`,
  `verify-production`, `diagnostics`, `plan --target`, `init --production`
  (with `--package`, `--bin` and `--smoke`), and the production forms of
  `status` and `resume` (a production ID, `--store`, `--summary`, `--retry`,
  `--abandon`, `--stop-after`, `--operation`, `--jobs`). `status` now takes
  `--run ID` or `--commit REV`, and `resume` takes `--run ID`; those modes,
  their output and their exit codes are unchanged.
- Pull-request evidence, shipped in 0.6.0: `inspect` and `assess`, with
  `sykli-requirements.v1`, `sykli-inspect.v1`, `sykli-assessment.v1`,
  `sykli-error.v1` and the evidence bundles under `.sykli/evidence`. sykli no
  longer runs `gh` or makes any network request.
- The CI shadow experiment: `examples/ci_shadow.rs`, its CI steps and
  `docs/ci-shadow.md`.
- What 0.6.0's removed features left in a worktree, `.sykli/production`
  (production attempts and their build trees) and `.sykli/evidence`
  (evidence bundles), is read, written and removed by nothing now. Neither
  grows any more, and both are safe to delete by hand:
  `rm -rf .sykli/production .sykli/evidence`. sykli does not delete them
  itself, so that no run carries a removal path for a one-time leftover.
- The removed code is kept on the `archive/delivery-production` branch.

## [0.6.0] - 2026-09-09

The first published release of this implementation. Version 0.2.0 was bumped
in the crate and described below but never tagged or published; everything
listed under it first ships here, together with the additions below. The
number continues the `sykli` crate on crates.io, whose 0.3.0 through 0.5.3
belong to the retired reference implementation, so a future publish is possible.

### Added
- Typed artifact production, opt-in beside the graph commands:
  `sykli init --production --smoke CMD` discovers a Cargo binary, a Go main
  package (`--package ./cmd/NAME`) or a standalone `main.rs` and writes
  `sykli.production.json`; `targets`, `plan --target`, `produce`, `status`,
  `resume`, `diagnostics` and `verify-production` operate on it. A production
  binds one captured source state to one target; `produce --stop-after` and
  `resume PRODUCTION_ID` let another worker finish from the store at
  `.sykli/production` without a handover. `produce`/`resume` exit 0 only for
  successful delivery, 1 for unfinished or failed work, 2 for an error.
- Agent loop on production: `produce --prepare`, `--summary --json` compact
  state with `ready` work and no embedded logs, `resume --operation NAME`,
  explicit `--retry NAME`, `--jobs N` bounded parallel waves, and
  `diagnostics PRODUCTION_ID ATTEMPT_ID` for recorded command output.
  See `docs/agents.md`.
- Readable default output for all production commands: operation states,
  artifact availability and path, identities, and labeled failure tails.
- Windows x86_64 release binaries (`sykli-vX.Y.Z-windows-x86_64.zip`). Graph
  runs, `verify`, `init`, `inspect` and `assess` build for Windows; tasks
  execute under a POSIX `sh` found on `PATH` (Git for Windows ships one).
  Typed production remains Linux and macOS. Windows test coverage runs on
  demand in CI, not on every change.
- Generated production targets and products carry the executable's name
  (`sykli produce tiny-cli`), not a fixed `app` alias; existing contracts and
  pinned productions keep their names.
- `sykli inspect --repo OWNER/NAME --pr N`: reads a pull request's workflow
  runs and reviews through the operator's `gh`, saves an immutable evidence
  bundle under `.sykli/evidence/<collection-id>`, and prints observations.
  With `--requirements FILE` (`sykli-requirements.v1`) it also assesses
  `workflow-reported-success` and `candidate-approval` obligations. Exit
  codes: 0 established/observations saved, 1 refuted, 2 invalid input or tool
  failure, 3 unproven, 4 conflict.
- `sykli assess BUNDLE --requirements FILE [--at TIME] [--json | --graph mermaid] [--why ID]`:
  deterministic offline replay of a saved bundle; evaluates at the collection
  end unless `--at` is given, and labels stale evidence as unproven.
- New versioned documents: `sykli-requirements.v1`, `sykli-request.v1`,
  `sykli-collection.v1`, `sykli-assessment.v1`, `sykli-inspect.v1`,
  `sykli-why.v1`, `sykli-error.v1`. Every assessment carries
  `trust: trusted-local-collector-and-store`, `authenticity: not-established`,
  `mode: advisory`. Existing receipt and production identities are unchanged.

### Changed
- Graph cache keys and receipt `inputs_digest` now include each input's
  Unix execute bits with its content hash. Removing a script's executable
  permission no longer returns a cached success; old content-only cache
  entries miss, and older receipts with declared inputs must be regenerated.
- Typed execution resolves each declared tool through the selected shell's own
  executable search before fingerprinting it, and rejects relative or empty
  `PATH` entries, so the recorded tool is the one that ran.
- The README presents sykli as one evaluator with three surfaces: graph runs
  and receipts, typed production, pull-request evidence.

- Tasks may `inherit` named environment variables from the invoking
  environment. Values reach the command; only their digests enter the cache
  key and the receipt (`inherited_digests`, `absent` when unset). A name
  cannot also appear in `env`.
- `sykli-lock.v2`: one `sykli.lock` per directory pins every contract file in
  it by name, so two contracts side by side can both be locked. v1 locks are
  still read and are upgraded on the next `sykli lock`.
- `sykli resume ID --abandon ATTEMPT` records a contact-lost attempt as
  abandoned (refused while its executor is alive), so its operation can be
  retried instead of freezing the production.

### Fixed
- `sykli verify` rejects a receipt that contradicts itself or its contract
  (a `passed` task with a non-zero exit code or an error, a recorded command
  that differs from the contract's, or a receipt outcome that disagrees with
  its task records) with exit 2 before judging the outcome. This catches
  corruption and careless forgery; the README states that `verify` does not
  establish who wrote a receipt.
- The production executor runs in its own session and each recipe shell in
  its own process group: a client's Ctrl-C no longer kills the executor, and
  a shell that dies by signal is recorded as `interrupted` after its group is
  killed, rather than as lost contact.
- A graph task's cache key includes the keys of the tasks it runs `after`,
  so a downstream task is never reported `cached` against upstream outputs it
  did not see.
- Tasks run with an empty stdin and as `sh -c -- CMD`; a command that starts
  with `-` is a command, and no task can prompt or read the terminal.
- `run`, `plan` and `lock` exit 2 when they cannot evaluate (1 stays for a
  failed task); every graph command lists its exit codes in `--help`.
- `subject.dirty` no longer flags a clean checkout whose tracked files match
  `.gitignore` or whose `.sykli/` is tracked; both trees exclude `.sykli/`.
- Truncated captures remain importable and cacheable: the digests cover the
  full streams, and `*_truncated` says what the text omits.
- Deterministic failures (missing inputs, blocked dependencies, spawn
  errors) are not marked `retryable`; a shell that fails to start is recorded
  as `spawn_error` and, in production, as a failed attempt rather than lost
  contact.
- `sykli init` declares `build.rs`, toolchain and lint configuration files,
  and all test fixtures; workspace members outside the repository are
  skipped; `sykli init --production` refuses a source it cannot capture
  instead of dropping it, requires a committed `Cargo.lock` instead of
  writing one, and never declares the contract as its own input.
- Evidence bundles publish on Windows (no directory fsync there); the same
  run attempt listed twice is one record, and disagreeing copies conflict;
  request identities bind only stable candidate coordinates; fork runs are
  reported as `association-missing`, not as absent; confirmation-pass
  failures are described as such; Mermaid node ids are positional.
- `sykli produce --stop-after X` stops when X is already terminal; a
  `.DS_Store` in a production's records is ignored; lease errors from network
  file systems are not reported as a busy peer.

### Removed
- Never shipped in a release, removed before the first one: the `Dockerfile`
  and `ghcr.io` container images, the `cargo xtask gate` helper (the
  repository's own `sykli run sykli.json` is the gate), the `sykli-mcp` shim,
  and the recorded demo outputs under `docs/demos`.
- All documentation under `docs/` except `docs/agents.md`, including the
  architecture decision records, and the `examples/` directory (the production
  fixture moved to `tests/fixtures/`). The README and `--help` are the
  documentation; AGENTS.md states the product boundaries.
- AGENTS.md now owns on-demand read-only acquisition of pull-request evidence;
  servers, webhooks, coordination and provider mutations remain excluded.

## [0.2.0] - 2026-09-05

Bumped in `Cargo.toml` but never tagged or published; first shipped in 0.6.0.

### Added
- Container images published on release: `ghcr.io/false-systems/sykli:<tag>`
  (the static binary on scratch) and `<tag>-tools` (Debian slim with `git`
  and `jq`, for CI). `latest` and `tools` move only for non-pre-release tags.
- A Homebrew formula (`sykli.rb`) attached to every release, generated from
  the released tarballs' checksums.
- `docs/install.md`, `CONTRIBUTING.md`, `SECURITY.md`, this changelog.
- `sykli plan` without `--changed` selects the whole graph.
- `sykli verify <receipt>`: checks a receipt against the current tree and
  contract with staged exit codes — 0 verified, 1 the work failed, 2 cannot
  verify, 3 stale tree or inputs, 4 contract drift.
- `inputs_digest` on the receipt subject: declared inputs bind the subject
  beyond the tree OID, so an undeclared input change cannot pass as the same
  evaluation.
- `sykli validate --json`: a `sykli-validate.v1` verdict with `valid`,
  `contract_hash`, and `errors`; exit 1 when invalid.
- The GitHub Action (`uses: false-systems/sykli@<tag>`): installs the release
  matching its ref, evaluates the contract, verifies the receipt, and attaches
  it to the run. See `docs/github-actions.md`.
- `tests/qa/sykli.json`: a second contract that is the QA graph for this
  repository's own changes, run with `sykli run tests/qa/sykli.json`.
- `docs/autonomous-cycle-v0.md`: the composition sykli takes part in, and how
  to run one; sykli itself gains no coordination surface.

### Changed
- Releases build on GitHub-hosted runners for Linux x86_64 and aarch64 (musl,
  static) and macOS x86_64 and aarch64.
- CI runs one job per ref and cancels superseded runs off `main`.

### Fixed
- `sykli run` no longer writes temporary Git objects into the repository's
  object database while materializing the tree: it uses a temporary object
  directory with the repository's objects as alternates.

## [0.1.0] - 2026-08-05

No tag was cut for this version; it is the state of the `main` branch at
commit `f536224`, the bootstrap of the Rust rewrite.

### Added
- The v0 engine slice: parse, validate, execute, local content-addressed
  cache, delta plan (`sykli plan --changed`), and a receipt per run.
- `sykli.lock`: the contract hash pinned so an unexpected contract change
  fails instead of silently changing the graph.
- The Rust SDK: repositories may emit the contract from `sykli.rs`.
- A self-hosted repository gate and `install.sh`, which fetches a tagged
  tarball and checks it against the release's `SHA256SUMS`.

[Unreleased]: https://github.com/false-systems/sykli/compare/v0.6.0...HEAD
[0.6.0]: https://github.com/false-systems/sykli/releases/tag/v0.6.0
[0.2.0]: https://github.com/false-systems/sykli/commit/1c4e247
[0.1.0]: https://github.com/false-systems/sykli/commit/f536224
