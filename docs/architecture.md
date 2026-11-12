# Architecture

How sykli is built, for work on its internals. AGENTS.md keeps the rules
every change follows; this keeps the detail a change to one part needs.

One binary, one identity model. Every identity is a
domain-separated SHA-256 over canonical JSON (`canonical::identity`), computed
after `canonical::decode` has rejected duplicate keys and floats. Existing
domains and schema meanings never change; incompatible changes are a new `.vN`.

**Graph runs (`src/main.rs`, `src/lib.rs`, `src/init.rs`, `src/cargo_inputs.rs`).** A
`sykli-contract.v1` names tasks, commands, declared input files and `after`
ordering. `run` executes level by level under `sh -c --` with a null stdin and
an environment of only `PATH`/`HOME`/`TMPDIR` plus declared `env` and
`inherit` variables (inherited values reach the task; only their digests reach
the receipt, keyed: HMAC-SHA256 under 32 random bytes of the clone's own,
`sykli/inherit-key` in the common git directory, made only by a run,
`inherited_keyed_digests`; the plain `inherited_digests` is no longer written,
so no receipt holds a digest a guess can be checked against). A declared `env` value may name `${worktree_cache}`
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
pass recorded against upstream outputs it did not see. A task declaring
`"reuse": false` (`Task::reuse`, serialized only when false, so no other key
changes) is never offered to `Cache::restore` by `execute` and never stored by
`conclude`; its `TaskReceipt` carries `reuse: false`, and
`record_contradiction` refuses it recorded as `cached`. The receipt's subject
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
A task of such a contract declares the files beside it that it reads as
`contract_inputs`, paths from the contract's directory: the run reads them
once into the subject (`contract_input_records`, no tree entry), their
digests join the task's key (only when it has any, so other keys are
unchanged), each task's records go into its receipt and the journal's
subject, and its shell gets `SYKLI_CONTRACT_DIR`, a copy of exactly those
files hashed as they are copied, checked against the records and removed
when it ends (`ContractCopy`: 0700 under an unguessable name, made
exclusively, `fresh_directory`, `private_name`; the user's own stale ones,
unchanged for a day, pruned by `prune_stale_copies`). `placed` (`validate`,
`lock`, `plan`, `run`; nothing without a repository) refuses them in a
contract inside the repository, and refuses a task of an outside contract
that names the contract's directory in `run` or `env` (sykli#75), in text
read with the task's HOME and a leading `~` expanded, quotes and escapes
removed, and `//`, `/./` collapsed (`spelled_out`), a path in a repository
the directory holds excepted; `verify` and `judge` compare them with the
files beside the contract (`contract_inputs_check`, exit 3).
Git is asked as little as possible, and `tests/git_calls.rs` holds the
budgets (counted through `GIT_TRACE2_EVENT`). `src/repo.rs` finds the
repository once per process (one `git rev-parse`, or `--repo-root` and
`--git-common-dir` from the caller, then none) and asks object questions in
one `git cat-file --batch` (`ask`, `Answers`). `run` builds one snapshot
(`snapshot`: HEAD's tree from the batch, then `read-tree`, `add` excluding
`.sykli` by a glob pathspec, `write-tree`) and one `git ls-files` for both
undeclared-file checks (`Inventory`, through `repo::files`; the Cargo files by name and the
sibling check leave out the JSON contract being run and its lock, however
spelled, `evaluated`, sykli#73, and an include of either is still asked
for), which
`validate` asks too (`undeclared_refusal`, exit 2); `init` declares from the
same listing, made once from the top level (`Listed::of_repository`), and
judges the contract it would write with that refusal before writing it
(`Inventory::given`, `undeclared_refusal_of`); a refusal for files outside the
current directory names the top level (`outside_here`). What Cargo reads (by
name; what sources include, in code or a doc comment's code block; every file
of a package with a `.cargo-checksum.json`; what build scripts' string
literals name: `Reads`, over `Sources`, a small Rust lexer; a package is
vendored, its opaque includes exempt, only as `vendored` proves it from the
Cargo configuration and `Cargo.lock`) and what is a document (`docs`,
which the sibling check never asks for; `init` declares them) are
`cargo_inputs` for both; the documents string literals name, and what build
scripts may read unseen (`UnseenReads`, which counts nothing `init` leaves
out, `init::never_declared`), only warn. A declared symbolic link to a file in the repository is read through;
one to a directory in the repository or to nothing is recorded by its text
(`link_text`, `InputRecord::link`), with every file under such a directory
required of the same task, and one that leads out of the repository is
refused (`link_missing_inputs`), as is a workdir, input or output that a
link on its way leads out of it (`through_links_out`, `leads`: every link
followed by its text, chained or dangling, at most `LINK_HOPS`, and judged
where its chain ends, `Part::LinkEnd`; sykli#54;
judged before any task, so a link a task makes while it runs is not seen). A text record is judged by the commit's link
text (`judge_link`), after a third round of the same `cat-file` asks where it
leads (`link_target_names`, `PathFacts::around`).
`verify --commit` and `hook pre-push`
read every commit fact in one batch (`commit_names`, `ContractNames`) and
judge receipts against them (`judge`); the pre-push hook validates branch
names in-process and reads `refs/remotes/REMOTE/HEAD` from the common
directory.

**Durable graph runs (`src/runs.rs`, `src/journal.rs`, Unix only).** Every
`run` is journaled in `<git-common-dir>/sykli/runs/<run-id>/` with the
machinery in `src/journal.rs` (hash-chained envelopes, exclusive synced
`publish`, `journal-lock`, flock `Lease`), synced with `fsync(2)`
(`canonical::sync`). A dropped `Lease` is unlocked (`LOCK_UN`), not
just closed: a child process another thread is spawning (a task or `git`,
through `fork` or `posix_spawn` alike) has a copy of its descriptor until it
execs, and the lock with it. One handed to a child (`inherit`) is
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
(`publish`, named by digest so a fenced coordinator cannot replace it)
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


**Review and landing (`src/review.rs`, `src/land.rs`, Unix only).** Both take
the worktree's root and run git there (`review::git`), so their tests use
real repositories with a bare remote and no process-wide state. `review`
judges `merge-base(refs/remotes/REMOTE/BRANCH, HEAD)..HEAD` (the branch by
`default_branch`, which reuses `hook::default_branches`), picks the reviewer
(`authors` from `Co-Authored-By` trailers, `choose`), and talks to it in
`converse`: the prompt is written, and both pipes are read, on threads beside
one deadline, so a reviewer that stalls, exits non-zero, leaves its prompt
unread or leaves a child holding its pipes is no review (exit 2). The change
is untrusted text handed to a model, so `prompt` puts the commit messages and
the diff between boundary lines that carry `Tokens::boundary` (random per
run, checked not to occur in the change: `token_not_in`), declares the block
material before it and ended after it, and gives `Tokens::verdict` only after
it. The reply is parsed by `parse_verdict` (exactly one line `REVIEW <verdict
token>: LEVEL`, raised to the worst finding; a `REVIEW` line without the
token is ignored, so one quoted or dictated by the change is no verdict).
`verdict_like_lines` counts the whole-line verdicts (`verdict_shaped`) in the
commit messages and the diff's added lines before the reviewer runs; the
record keeps the count, `addressed_reviewer` and `PROMPT`, the version of
these defences (absent on older notes). None of it detects a reviewer that
was persuaded. The record is kept as a `sykli-review.v1` note on the tree under
`refs/notes/sykli/review` (`write_note` stores the bytes as a blob first, so
a note holds them exactly). A kept record answers only for its `base`.
`AGENT_MARKERS` are removed from the reviewer's environment and refuse
`--overrule`, which also needs a terminal (`person_at_terminal`). `land`
takes its proof from a `Prover` (injected in tests; `sykli_proof` asks
`commit_status` and, only where its verdict is `none`, `verify` on the shared
receipt note, both in this process), requires the fetched branch tip to be an
ancestor of HEAD and the kept review's `base` to be that tip, attaches the
receipt under `refs/notes/sykli/receipt`, copies it to the worktree's
`.sykli/receipts` for the pre-push hook (`keep_for_hook`), merges and pushes
the notes (`fetch_notes`, `push_notes`, strategy `ours`) and then pushes with
`--force-with-lease` on the checked tip. `tests/land.rs` runs the binary with
a script as the `codex` on a `PATH` that holds no real agent.

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


## The path to the default branch

The path from a passing receipt to the default branch is sykli's too, by the repository owner's decision, so that proving, reviewing and landing a commit are one tool's records about one tree. `review` starts an installed agent CLI as a reviewer, with reading tools only; `land` fetches from and pushes to the git remote the person names. A deployment is not sykli's: it is an ordinary contract the person runs (`sykli run deploy.json`), its tasks marked `"reuse": false`. These are the only commands that start an agent or use the network, they do so only while they run, and they add nothing to the contract or receipt schemas. Every other command stays offline and starts no agent; anything further on this path (a queue, a service, a second reviewer protocol) needs its own written condition.
