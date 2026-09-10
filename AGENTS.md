<!-- toimija:adopt:v1:begin -->
Read `.toimija/current.md` before reading anything else in this repo; it is your workset contract. Under a toimija session your launch prompt names your live packet — that file supersedes this one.
<!-- toimija:adopt:v1:end -->

Product boundaries:

- Own graph parsing, validation, planning, execution, caching, receipts, and local typed production.
- Production assessment establishes declared products and checks, not task closure or publication authority. External tools and workers remain optional consumers.
- Stay a local CLI: no server, network service, daemon, coordination, or agent execution. The one network use is on-demand, read-only acquisition of pull-request evidence through the operator's `gh` (`sykli inspect`); it never mutates a provider, triggers work, or runs candidate code. A bounded executor may finish its attempt after its initiating client exits.
- Candidate assessment (`sykli inspect` / `sykli assess`) establishes only the declared review-readiness predicates under a printed trust designation. It is advisory: no merge or deployment authority, no claim that GitHub's full merge policy is met.
- Container runtime, Toimija gate mode, and Ahti append wait until family repositories run v0 daily.
- Contract growth requires a named user and a written condition for the growth. Removed capabilities (coordination, servers, occurrences, actors, review primitives, webhooks, GUI, tiered caches, attestations, extra SDKs, watch mode) do not return without one.
- Treat `sykli plan --json` as the agent-facing query surface; before handoff run `toimija gates run sykli-full`.

This file is the guidance for any coding agent working in this repository
(Codex, Claude Code, Gemini, Copilot, Cursor and others read it or are pointed
here). Under a toimija session, the packet named in your launch prompt is the
workset contract; run `toimija verify` before committing.

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

`CARGO_TARGET_DIR` is `/Users/yair/.cargo-target` on this machine, so the CLI
binary is `/Users/yair/.cargo-target/debug/sykli`; prefer `cargo run --quiet --`.
macOS has no `timeout`; use `perl -e 'alarm 300; exec @ARGV' -- <cmd>` to bound
a run. `tests/production.rs` is Unix-only and takes about 25 s; a hang there
usually means a test re-ran a FIFO-latched recipe without feeding the latch.

When adding a source, test or fixture file, add it to the `inputs` of the
`fmt`, `clippy` and `test` tasks in `sykli.json` (fixtures to `test` only), to
the `paths` list in `sykli.production.json`, then run `sykli lock sykli.json`.
Both contracts list files explicitly; nothing is globbed.

## Toolchain constraints

- Edition 2024 with `rust-version = "1.85"` pinned in CI. Let-chains
  (`if let … && …`) need 1.88 and do not compile in CI; use `Option::filter`,
  `is_some_and`, or a nested `if`.
- No dependencies beyond clap, serde, serde_json, sha2. Unix syscalls
  (`flock`, `fcntl`, `setsid`, `setpgid`, `killpg`, `signal`) are declared as
  `unsafe extern "C"` blocks, not via libc.
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
the receipt). A task's cache key covers its declaration, input digests
(content plus execute bits), runtime fingerprint, inherited-value digests, and
the cache keys of its `after` dependencies, so a downstream task never reuses a
pass recorded against upstream outputs it did not see. The receipt's subject
binds the git tree OID (computed from a temporary index seeded from HEAD, with
`.sykli/` excluded on both sides) and the declared-inputs digest. `verify` is
staged: schema and record consistency (2), contract lock and contract hash (4),
tree and inputs (3), outcome (1). It proves consistency, not authorship; the
README says so. `sykli.lock` is `sykli-lock.v2`, one entry per contract file in
its directory. `run`/`plan`/`lock` exit 2 when they cannot evaluate, 1 for a
failed task.

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

**Tests.** `tests/assess.rs` drives `inspect` through a fake `gh` shell script
on `PATH` serving `tests/fixtures/github/pr25` (real responses from this
repository's PR #25); it is Unix-only. `tests/assess_portable.rs` replays the
committed bundle in `tests/fixtures/bundle` on every platform. Production tests
use FIFO latches to hold a build mid-flight; any new test that retries a
latched operation must feed the latch again.

## Working rules in this repository

- Never push to `main`; branch, push, open a PR. Never merge without the
  owner's explicit approval of that PR. Push a branch only when it is complete
  and say so: merges here have repeatedly caught branches mid-push.
- Validate locally, not in CI. Hosted Actions minutes are a hard budget:
  `ci.yml` runs only on pushes to `main` and a manual Windows job; do not add
  `pull_request` triggers or macOS jobs. Do not push a release tag
  (`release.yml` builds five targets) unless told to.
- `.sykli/`, `.toimija/`, `.teko/` and `.claude/` are local state and ignored;
  the production store and toimija packets live there.
