Product boundaries:

- Own graph parsing, validation, planning, execution, caching, receipts, and local typed production.
- Production assessment establishes declared products and checks, not task closure or publication authority. External tools and workers remain optional consumers.
- Stay a local CLI: no server, network service, daemon, coordination, or agent execution. The one network use is on-demand, read-only acquisition of pull-request evidence through the operator's `gh` (`sykli inspect`); it never mutates a provider, triggers work, or runs candidate code. A bounded executor (a production attempt, or the coordinator of a run started with `run --detach` or of a delivery started with `deliver --detach`) may finish its work after its initiating client exits; it is never a daemon: it ends with the attempt, run or delivery it was started for.
- Delivery (`sykli deliver`) is a journaled operation, not a deployment tool or controller. Sykli knows no kind of target: the caller's declared commands apply, observe and smoke-test it, and sykli records what happened and judges only what an observer reported, dimension by dimension. It never re-applies on its own (no reconcile loop), never repeats an unacknowledged apply, nor applies another delivery over it, before an observation that reached the target, and never calls an acknowledgement a deployment. The named user and condition for this growth: a caller putting a produced artifact on a disposable target needed the operation's state to survive a crash without fake success.
- Candidate assessment (`sykli inspect` / `sykli assess`) establishes only the declared review-readiness predicates under a printed trust designation. It is advisory: no merge or deployment authority, no claim that GitHub's full merge policy is met.
- A container runtime and appending receipts to an external store wait until consumer repositories run v0 daily.
- Contract growth requires a named user and a written condition for the growth. Removed capabilities (coordination, servers, occurrences, actors, review primitives, webhooks, GUI, tiered caches, attestations, extra SDKs, watch mode) do not return without one.
- **Anything written under `.sykli/` is bounded, and the bound is stated where it is written.** A directory that grows with use needs either a budget it is evicted against or a point at which it is discarded; "the operator can delete it" is not a bound. This rule exists because the boundaries above govern capabilities and dependencies and said nothing about retained state: production attempts kept every build tree they ran through — 2.0 GB against 14 MB of declared products — and the task cache carried `unbounded until family receipts define a real size/age eviction budget` as a comment rather than a limit. Both were invisible to every rule on this list.
- Treat `sykli plan --json` as the agent-facing query surface; before handoff run sykli's own graph, `cargo run --quiet --locked -- run sykli.json --json` (the same run CI does).

This file is the guidance for any coding agent working in this repository
(Codex, Claude Code, Gemini, Copilot, Cursor and others read it or are pointed
here). When your launch prompt names a workset packet, that packet is the
workset contract; run the check it names before committing.

## Commands

The repository gates itself with its own graph contract. This is the check to
run before any commit or handoff; it is what CI runs on `main`:

```sh
cargo run --quiet -- run sykli.json --json | jq '{outcome, tasks:[.tasks[]|{name,outcome}]}'
```

It runs `cargo fmt --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`,
`cargo test --workspace --locked`, `sh -n install.sh` and the Action script
checks, caching by content. Pieces on their own:

```sh
cargo build                                   # binary lands in the shared target dir, not ./target
cargo test --workspace --locked               # everything
cargo test --test cli                         # one integration suite: cli, init, assess, assess_portable, production
cargo test --test production -- executor_loss # one test by substring
cargo test --bin sykli assessment             # unit tests in one module
cargo check --workspace --all-targets --target x86_64-pc-windows-msvc   # Windows type-check (target is installed)
cargo run --quiet -- lock sykli.json          # re-pin after editing sykli.json
```

The CLI binary lands in Cargo's target directory, which `CARGO_TARGET_DIR` or a
Cargo config may move outside the checkout; prefer `cargo run --quiet --`.
macOS has no `timeout`; use `perl -e 'alarm 300; exec @ARGV' -- <cmd>` to bound
a run. `tests/production.rs` is Unix-only and takes about 25 s; a hang there
usually means a test re-ran a FIFO-latched recipe without feeding the latch.

When adding a source, test or fixture file, add it to the `inputs` of the
`fmt`, `clippy` and `test` tasks in `sykli.json` (fixtures to `test` only), to
the `paths` list in `sykli.production.json`, then run `sykli lock sykli.json`.
Both contracts list files explicitly; nothing is globbed.

## Toolchain constraints

- Edition 2024 with `rust-version = "1.85"` declared in `Cargo.toml`. CI does
  not test 1.85: the workflow installs it, but `rust-toolchain.toml` pins 1.98
  and overrides it, so CI builds with 1.98. Let-chains (`if let … && …`, stable
  since 1.88) therefore compile in CI without a warning, yet break the declared
  1.85. Keep them out (use `Option::filter`, `is_some_and`, or a nested `if`)
  and run `cargo +1.85 check --workspace --all-targets --locked` before pushing.
- No dependencies beyond clap, serde, serde_json, sha2. Unix syscalls
  (`flock`, `fcntl`, `setsid`, `setpgid`, `kill`, `killpg`, `signal`) are
  declared once, in `unsafe extern "C"` blocks in `src/journal.rs`, not via
  libc.
- `src/production` is `#[cfg(unix)]`; on Windows those commands do not exist and
  anything only they construct must be `cfg_attr(not(unix), allow(dead_code))`.

## Architecture

One binary, three surfaces, one identity model. Every identity is a
domain-separated SHA-256 over canonical JSON (`canonical::identity`), computed
after `canonical::decode` has rejected duplicate keys and floats. Existing
domains and schema meanings never change; incompatible changes are a new `.vN`.

**Graph runs (`src/main.rs`, `src/lib.rs`, `src/init.rs`).** A
`sykli-contract.v1` names tasks, commands, declared input files and `after`
ordering. `run` executes level by level under `sh -c --` with a null stdin and
an environment of only `PATH`/`HOME`/`TMPDIR` plus declared `env` and
`inherit` variables (inherited values reach the task; only their digests reach
the receipt). A declared `env` value may name `${worktree_cache}`
(`src/worktree_cache.rs`): `<git-common-dir>/sykli/worktrees/<id>/cache`, the
16-hex id from the worktree's incarnation: its resolved top level and git
directory (read from its `.git`, no git process) and the token in that git
directory's `sykli-incarnation`, so a worktree made again at one path gets a
new directory. `<id>/` is made aside and renamed into place with a
`worktree.json` marker and a `lease` beside `cache/`, which the tasks own (a
`cargo clean` takes nothing of sykli's). Placeholder rules (`check`, through
`authored`) apply only where a contract is written or run (`validate`, `lock`,
`plan`, `run`, resume, the coordinator), never in `parse_contract`/`validate`,
which judging shares: `verify` and `status --commit` judge an old contract as
it ran. `prepare` (run, resume, detached coordinator) makes and holds it shared
before the run is recorded; `launch` expands it and lets tasks that name it
inherit the lease; `prune`, called where runs are pruned, renames the
directories of gone (and not `locked`) incarnations aside under an exclusive
lease, and `remove_pruned` deletes them at the end of `conclude`. Keys cover
the value as declared, never the expanded path. Inside a git hook, the directory git prepends to `PATH`
(`GIT_EXEC_PATH`) is taken off again (`without_git_prepend`), so hooked and
manual runs share keys. A task's cache key covers its declaration, input digests
(content plus execute bits), runtime fingerprint (for a Cargo graph, including
the `rustc -vV`/`cargo -V` the task shell resolves: `graph_runtime`, sykli#64),
inherited-value digests, and
the cache keys of its `after` dependencies, so a downstream task never reuses a
pass recorded against upstream outputs it did not see. The receipt's subject
binds the git tree OID (computed from a temporary index seeded from HEAD, with
`.sykli/` excluded on both sides) and the declared-inputs digest. `verify` is
staged: schema and record consistency (2), contract lock and contract hash (4),
tree and inputs (3), outcome (1). It proves consistency, not authorship; the
README says so. With `--commit` the inputs stage (`inputs_check`,
`InputsAt::judge`) judges each declared input at its exact path (`found_at`
walks the trees by exact bytes: `Found`, `Absence`): the run's record
(`InputRecord`: `sha256`, `mode` as git has it, `object` from the run's own
snapshot, read in a second round of the snapshot's `cat-file`, with a third
only for symbolic links) must have the commit's entry, the commit's blob bytes
(or what `git --attr-source=COMMIT cat-file --filters` makes of them, by the
commit's attributes, one process per converted input, streamed: `checked_out`),
and the entry's execute bit; a symbolic link is followed to the declared input
it names (`PathFacts::file_behind`; a `..` may only leave the link's own
directories, `link_target`). Input blobs are hashed as they are read and kept
only up to `INPUT_BLOB_KEPT` (`Session::ask_bounded`). The commit's trees and
blobs come from a second round of the verify or hook `cat-file` session
(`repo::Session`, `input_names`, `inputs_from`). An input the run read where
the commit has nothing is refused, unless a task declares it as an output.
The records must hash to `inputs_digest` (`records_check`, also without
`--commit`, where `checkout_records_check` also compares each record with the
checkout's); older receipts are recomputed from the same blobs
(`older_inputs_check`). Declared paths are named lexically from the top
level (`declared_path`), never through the checkout. Exit 3. `sykli.lock` is
`sykli-lock.v2`, one entry per contract file in its directory.
`run`/`plan`/`lock` exit 2 when they cannot evaluate, 1 for a failed task.
Declared paths resolve against the current directory, so a
contract outside the git toplevel of the current directory (decided on
resolved paths, `outside_repository`) makes `main` absolutize command-line
paths and enter that toplevel first (`enter_for_contract`); hooks absolutize
`--contract` and enter the root themselves. Receipts record `lock_digest`
(SHA-256 of the lock file, read once with the pin in `load_pinned`); `verify`
reads an outside contract and its lock from their files even with
`--commit`, and only for such a contract compares `lock_digest` (exit 4).
Git is asked as little as possible, and `tests/git_calls.rs` holds the
budgets (counted through `GIT_TRACE2_EVENT`). `src/repo.rs` finds the
repository once per process (one `git rev-parse`, or `--repo-root` and
`--git-common-dir` from the caller, then none) and asks object questions in
one `git cat-file --batch` (`ask`, `Answers`). `run` builds one snapshot
(`snapshot`: HEAD's tree from the batch, then `read-tree`, `add` excluding
`.sykli` by a glob pathspec, `write-tree`) and one `git ls-files` for both
undeclared-file checks (`Inventory`). `verify --commit` and `hook pre-push`
read every commit fact in one batch (`commit_names`, `ContractNames`) and
judge receipts against them (`judge`); the pre-push hook validates branch
names in-process and reads `refs/remotes/REMOTE/HEAD` from the common
directory.

**Durable graph runs (`src/runs.rs`, `src/journal.rs`, Unix only).** Every
`run` is journaled in `<git-common-dir>/sykli/runs/<run-id>/` with the
production journal's machinery, lifted unchanged into `src/journal.rs`
(hash-chained envelopes, exclusive synced `publish`, `journal-lock`, flock
`Lease`); production keeps its own record type and `F_FULLFSYNC`, runs use
`fsync(2)` (`canonical::Durability`). The first record, `requested`, holds
the contract as run (canonical JSON), the subject with its input records,
the runtime fingerprint and the task list, and is durable before any task is
dispatched; the run id is its identity. Each task is then `reused`,
`settled` (blocked, or errored before running) or attempted: `dispatched`
(attempt lease taken first), `started` (process group), and one end:
`finished` (exit or signal; the task record kept first as
`outputs/<sha256>.json`), `interrupted` (sykli's signal handler stopped it)
or `lost` (a later coordinator found it unfinished). Every record carries the
coordinator's generation (`requested` 1, each `resumed` +1); `History::apply`
refuses a stale generation, a second end, a pass without exit 0, a retry
that does not supersede the last attempt, and a conclusion before every task
is decided. A coordinator's generation is fixed when it creates, receives or
takes over the run; once a newer one has written, it is `fenced` and writes
nothing more. The receipt is built only from accepted records (`coordinate`,
`conclude`), so it round-trips (`TaskReceipt` is `Deserialize`) and gains
only `run`. Tasks run in their own process group holding their attempt's
lease (so an orphan reads as running; so does one whose recorded process
group still exists, `group_exists`, which `resume` also refuses unless
`--assume-ended`); SIGINT/SIGTERM/SIGHUP stop every
group (TERM, KILL after 3 s), an attempt sykli signalled is `interrupted`
whatever it exits with, a pass accepted less than 100 ms before
sykli's own handler ran is `withdrawn`, with every pass decided after it
(`withdraw_before_stop`: the same stop may have reached the task first; a run
waits that long after its last pass before concluding, `wait_out_stop_window`), and the run records `stopped`, exit 128+N, no
receipt.
`conclude` keeps the receipt as `rcpt_<digest>.json` in the run's directory
(`publish_with`, named by digest so a fenced coordinator cannot replace it)
before it records `concluded`; `status --run` reports it as `receipt.kept`.
`status --run` is a pure view of the journal and leases; `resume --run`
re-checks worktree, contract, lock, runtime and subject before taking over;
`run --detach` hands the lease to a `setsid` coordinator
(`__run_coordinator`), and nothing else: every other descriptor is marked
close-on-exec first (`close_inherited_on_exec`). `drifted` errors a task
whose declared inputs changed
during the run. `status --commit` judges every kept receipt of a commit's
tree with `judge`; a receipt is `recorded` when its run (or the run's
tombstone in `sykli/pruned-runs/`, left by `prune`: a run is created aside and
renamed into place with its lease held, and `remove_run` takes a lease only if
it exists and moves the run aside before deleting it) concluded with it, and
the verdict lets the newest recorded result decide, never an unrecorded pass
over a recorded failure (`conflict`). `checkpoint(POINT)` is a debug-build
test hook
(`SYKLI_TEST_PAUSE_AT`/`SYKLI_TEST_PAUSE_DIR`, `SYKLI_TEST_FAIL_AT`) at each
crash window; release builds have none. On Windows `runs` is a stub of
uninhabited types and a run is in memory, as before.

**Typed production (`src/production/`, Unix only).** Opt-in local artifact
production from a `sykli-production-contract.v1`. `contract.rs` is the schema
and validation; `discovery.rs` generates contracts from Cargo, Go or a bare
`main.rs`; `store.rs` is the content-addressed blob store, atomic `publish`,
and `flock`-based leases; `mod.rs` is the journal and executor. State is an
append-only journal of facts (`Started`, `Finished`, `ContactLost`,
`Abandoned`), each envelope hashed and chained; `status` is a pure function of
the journal. A controller acquires the production lease, appends `Started`,
and spawns a detached executor (`__production_attempt`, its own session via
`setsid`) that inherits the lease fd and takes a per-attempt liveness lease;
recipe shells run in their own process group, which the executor kills after
the shell exits, so a signalled shell is a recorded `interrupted` failure and
only a failed `wait` is `ContactLost`. `--retry` reopens failed operations;
`--abandon ATTEMPT` records a lost attempt as abandoned (refused while its
executor is alive). Never turn an unknown into a success: an unresolved attempt
blocks the production until it is resolved or abandoned.

**Delivery (`src/production/delivery.rs`, Unix only).** A
`sykli-delivery-contract.v1` (its own file, so changing how an artifact is
delivered never changes the production that built it) names, per target, a
production target's product and the caller's commands: `identity`
(`content`, or a command printing what the target will report),
`apply`, `observe` (prints `sykli-observation.v1`), optional `smoke`,
`freshness_seconds`, `settle_seconds`. Commands are `sykli::Task`s run through
`launch` (env and `inherit` rules of runs; `SYKLI_DELIVERY`, `SYKLI_INTENDED`,
`SYKLI_DELIVERY_TARGET`, and for apply `SYKLI_ARTIFACT` and
`SYKLI_DELIVERY_ATTEMPT`); `launch` takes a `Journaled` attempt, implemented by
a run's attempt and a delivery's apply. A delivery lives in
`<git-common-dir>/sykli/deliveries/<id>/` (`sykli-delivery-record.v1`, the
shared `journal`, `Durability::Fsync`, created aside and renamed with its lease
held); its id is the identity (`sykli-delivery.v1`) of its `Request`, which
holds the contract as read, the delivered product (production, store,
artifact) and the intended identity. Facts: `requested`; `dispatched` (attempt
lease first, durable before the shell); `started`; one end: `acknowledged`
(exit 0), `failed` (an exit code: refused; a foreign signal: unknown; neither:
never started), `interrupted` (sykli's stop) or `lost` (a later coordinator
found it unfinished); `observed` (the observer's execution with its stdout
digest, the validated observation or why none, the smoke run; any time,
generation `None` for `sykli observe`); `reconciled` (an unknown end, judged
from a cited observation, which `reached` the target when its observer exited
0 with a reachable report). `History::apply` refuses a stale generation, a
second end, an exit-0 failure, an `observed` effect its observation does not
show (`Observation::shows`: desired artifact intended, operation this
delivery; the artifact alone attributes nothing), and any second `dispatched`
unless it is `--reapply` after a refused apply or an unknown one reconciled
from an observation that reached the target (invariant H). `resume --delivery`
takes over (generation + 1, unfinished attempts `lost`), observes and records
`reconciled` before anything else, and dispatches again only on `--reapply`.
Across deliveries, `clear_to_apply` (under the target lock, in `deliver` and
in any `resume` that would dispatch) refuses while another delivery to the
target `hold`s it: busy (`target-busy`), or an unknown effect never
reconciled from an observation that reached the target
(`target-unreconciled`); `deliver --reapply-over ID` passes the latter only
when its reconciliation could not reach the target, recorded in
`Request.reapply_over`. `status --delivery` is a pure function of the journal,
the leases, the clock and the clone's other deliveries to the target
(`superseded_by`, by the target's sequence): `judge` computes dimensions A to
F and one label (VERIFIED HEALTHY, NOT VERIFIED HEALTHY, DEGRADED, UNKNOWN,
STALE). It takes observations in the order their observers began (`by_start`;
`Execution::of` dates one before its spawn), never journal order; C is this
delivery's only when attributed; D is done only on the controller's
`complete`; a `uid` other than the first after the apply is "target
recreated"; an observation naming an operation the clone does not know marks
the delivery superseded (`superseded_seen`); an observation dated more than
`CLOCK_SKEW_MS` ahead is never fresh. `deliver --detach` hands the lease to
`__delivery_coordinator`. Checkpoints: `after-request`, `after-dispatch`,
`after-apply-start`, `after-apply-exit`, `after-ack`, `before-observed`,
`after-lost`, `after-reconcile`, `coordinator-start`. Retention:
`SYKLI_DELIVERY_BUDGET` (100) per clone, oldest settled first (`prunable`: no
live coordinator or apply, no unknown effect, not the newest per target);
`MAX_RECORDS` (2000) per delivery. The per-target directory
(`deliveries/.targets/<digest>`) holds the lock `deliver` takes from its
check until its delivery is recorded (a dispatching `resume` takes it for its
check), and `sequence/` (`next_in_sequence`, under its own short lock): every
request and dispatch to the target gets the next number, so the order of
applies never depends on the clock. An observation counts for an apply only when it began after the
apply's recorded end (`observable_from`), and a started apply without an exit
or signal gets no end (it becomes lost, never refused). Records name inherited
variables (`set`/`absent`), never their digests.

**Pull-request evidence (`src/assessment.rs`, `src/github.rs`,
`src/evidence.rs`, `src/inspect.rs`).** `github.rs` makes bounded, read-only
`gh api` GETs (fixed endpoints from validated components, page/byte/time caps,
token redaction), records why each listing stopped, reads the pull request and
lists runs/reviews twice, and stores raw bodies as content-addressed objects.
`evidence.rs` publishes a bundle (manifest + objects + private diagnostics)
into `.sykli/evidence/<collection-id>` by writing a private temp dir and
renaming it; loading re-verifies every digest. `github::normalize` is the only
path from raw objects to `Observations` (with `sha256:<object>#/json/pointer`
references), used identically live and on replay. `assessment.rs` is pure:
`Requirements` (two predicates: `workflow-reported-success`,
`candidate-approval`) plus a `Request` (stable candidate coordinates) plus
`Observations` give an `Assessment` with per-obligation verdicts, exclusions
with reasons, gaps, and the fixed trust designation
(`trusted-local-collector-and-store`, `not-established`, `advisory`). Gaps and
incompleteness are always unproven, never a convenient selection; a race or an
incomplete listing blocks the affected obligation. `inspect.rs` maps results to
exit codes 0 established, 1 refuted, 2 tool error, 3 unproven, 4 conflict, and
persists records beside the bundle best-effort after printing the verdict.

**Tests.** `tests/delivery.rs` (Unix) delivers to a scripted FAKE target, a
test adapter whose commands print what each test writes; it kills the
coordinator at each delivery checkpoint, during the apply and during the
observer, and covers every reconcile case; its last test is `#[ignore]`d
and opt-in (`cargo test --test delivery -- --ignored real_target`, with
`SYKLI_TEST_REAL_TARGET`, `SYKLI_TEST_REAL_TARGET_NAME`): it runs the lost
acknowledgement against a real target the caller's own commands drive.
`tests/runs.rs` (Unix) interrupts real runs: SIGKILL of the coordinator at each
checkpoint and of a task, SIGTERM/SIGINT, a stale coordinator, a replayed
completion, inputs changed mid-run, concurrent cache stores; each ends on
`nothing_unknown_passed`. Its hook tests skip under `--release`. `tests/assess.rs` drives `inspect` through a fake `gh` shell script
on `PATH` serving `tests/fixtures/github/pr25` (real responses from this
repository's PR #25); it is Unix-only. `tests/assess_portable.rs` replays the
committed bundle in `tests/fixtures/bundle` on every platform. Production tests
use FIFO latches to hold a build mid-flight; any new test that retries a
latched operation must feed the latch again.

## Working rules in this repository

- Never push to `main`; branch, push, open a PR. Never merge without the
  owner's explicit approval of that PR. Push a branch only when it is complete
  and say so: merges here have repeatedly caught branches mid-push.
- Validate locally before pushing (`sykli run sykli.json`, and any workset
  check your launch prompt names);
  CI then runs the Linux gate and the Windows tests on every pull request and
  on `main`. The repository is public, so standard hosted runners are free;
  keep to `ubuntu-latest` and `windows-latest` (macOS is the development host
  and is covered locally). Do not push a release tag (`release.yml` builds
  five targets) unless told to.
- `.sykli/` and other tools' local state directories are ignored; the
  production store lives under `.sykli/`.

## Languages

Code in this repository is **Rust, or Elixir/Erlang**. Never add Python, Go
or Node (JavaScript/TypeScript): not for tools, scripts, CI helpers, tests,
dashboards or glue. A thin shell step in a CI workflow is fine; anything with
logic is Rust. Existing Python or Node files are debt to be rewritten, not
precedent to follow.
