Product boundaries:

- Own graph parsing, validation, planning, execution, caching, and receipts.
- Stay a local, offline CLI: no server, network service, daemon, coordination, or agent execution. The coordinator of a run started with `run --detach` may finish the run after its initiating client exits; it is never a daemon: it ends with the run it was started for.
- A container runtime and appending receipts to an external store wait until consumer repositories run v0 daily.
- Contract growth requires a named user and a written condition for the growth. Removed capabilities (coordination, servers, occurrences, actors, review primitives, webhooks, GUI, tiered caches, attestations, extra SDKs, watch mode, typed production, delivery, pull-request evidence) do not return without one. The last three are kept on the `archive/delivery-production` branch.
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
cargo test --test cli                         # one integration suite: cli, init, runs, boundary, worktree_cache
cargo test --test runs -- detached            # tests by substring
cargo test --bin sykli journal                # unit tests in one module
cargo check --workspace --all-targets --target x86_64-pc-windows-msvc   # Windows type-check (target is installed)
cargo run --quiet -- lock sykli.json          # re-pin after editing sykli.json
```

The CLI binary lands in Cargo's target directory, which `CARGO_TARGET_DIR` or a
Cargo config may move outside the checkout; prefer `cargo run --quiet --`.
macOS has no `timeout`; use `perl -e 'alarm 300; exec @ARGV' -- <cmd>` to bound
a run. `tests/runs.rs` is Unix-only and takes 10 to 15 s; a hang there
usually means a test waits on a FIFO-latched task it never released
(`release`, `unlatch`).

When adding a source, test or fixture file, add it to the `inputs` of the
`fmt`, `clippy` and `test` tasks in `sykli.json` (fixtures to `test` only),
then run `sykli lock sykli.json`. The contract lists files explicitly;
nothing is globbed.

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
- `src/journal.rs` and `src/runs.rs` are `#[cfg(unix)]` (on Windows `runs` is a
  stub of uninhabited types), and so are `status` and `resume`; anything only
  Unix code uses must be `cfg_attr(not(unix), allow(dead_code))`.

## Architecture

One binary, one identity model. Every identity is a
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
machinery in `src/journal.rs` (hash-chained envelopes, exclusive synced
`publish_with`, `journal-lock`, flock `Lease`), synced with `fsync(2)`
(`canonical::Durability`). A dropped `Lease` is unlocked (`LOCK_UN`), not
just closed: a child another thread forked has a copy of its descriptor
until it execs, and the lock with it. One handed to a child (`inherit`) is
only closed, so the child keeps it. The first record, `requested`, holds
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

**Tests.** `tests/runs.rs` (Unix) interrupts real runs: SIGKILL of the
coordinator at each checkpoint and of a task, SIGTERM/SIGINT, a stale
coordinator, a replayed completion, inputs changed mid-run, concurrent cache
stores; each ends on `nothing_unknown_passed`. Its tasks wait at FIFO
latches to hold a run mid-flight; any new test that runs a latched task again
must release the latch again. Its hook tests skip under `--release`.
`resume_refuses_a_run_whose_coordinator_is_alive` pins the `run-busy` words
that the lease's `lease-busy` tag becomes.
`tests/cli.rs` pins the help flags other tools detect
(`help_lists_the_flags_other_tools_detect`).

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
- `.sykli/` and other tools' local state directories are ignored.

## Languages

Code in this repository is **Rust, or Elixir/Erlang**. Never add Python, Go
or Node (JavaScript/TypeScript): not for tools, scripts, CI helpers, tests,
dashboards or glue. A thin shell step in a CI workflow is fine; anything with
logic is Rust. Existing Python or Node files are debt to be rewritten, not
precedent to follow.
