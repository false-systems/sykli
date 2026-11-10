# Agent guide

The guide for every coding agent here (Codex, Claude Code, Gemini, Copilot, Cursor and others read it or are pointed to it). When your launch prompt names a workset packet, that packet is the workset contract; run the check it names before committing. How sykli is built is in `docs/architecture.md`: read the part a change touches.

## Product boundaries

- Own graph parsing, validation, planning, execution, caching, and receipts.
- Stay a local, offline CLI: no server, network service, daemon, coordination, or agent execution. The coordinator of a run started with `run --detach` may finish the run after its initiating client exits; it is never a daemon: it ends with the run it was started for.
- The one exception, by the owner's decision: `review`, `land`, `deploy`, `deployed`. `docs/architecture.md` states its limits.
- A container runtime and appending receipts to an external store wait until consumer repositories run v0 daily.
- Contract growth requires a named user and a written condition for the growth. Removed capabilities (coordination, servers, occurrences, actors, review primitives, webhooks, GUI, tiered caches, attestations, extra SDKs, watch mode, typed production, delivery, pull-request evidence) do not return without one. The last three are kept on the `archive/delivery-production` branch.
- **Anything written under `.sykli/` is bounded, and the bound is stated where it is written.** A directory that grows with use needs a budget it is evicted against or a point at which it is discarded; "the operator can delete it" is not a bound. (Production attempts once kept 2.0 GB of build trees against 14 MB of declared products.)
- Treat `sykli plan --json` as the agent-facing query surface.

## Commands

The repository gates itself with its own contract, as CI does on `main`. Run it before any commit or handoff:

```sh
cargo run --quiet --locked -- run sykli.json --json | jq '{outcome, tasks:[.tasks[]|{name,outcome}]}'
```

It runs `cargo fmt --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`, `sh -n install.sh` and the Action script checks, cached by content. Pieces on their own:

```sh
cargo test --test cli               # one suite: cli, init, runs, boundary, worktree_cache
cargo test --test runs -- detached  # tests by substring
cargo test --bin sykli journal      # one module's unit tests
cargo check --workspace --all-targets --target x86_64-pc-windows-msvc   # Windows type-check
cargo run --quiet -- lock sykli.json   # re-pin after editing sykli.json
```

Prefer `cargo run --quiet --`: `CARGO_TARGET_DIR` or a Cargo config may put the binary outside the checkout. macOS has no `timeout`; bound a run with `perl -e 'alarm 300; exec @ARGV' -- <cmd>`. `tests/runs.rs` is Unix-only and takes 10 to 15 s; a hang there usually means a test waits on a FIFO-latched task it never released (`release`, `unlatch`).

A new source, test or fixture file goes into the `inputs` of the `fmt`, `clippy` and `test` tasks in `sykli.json` (fixtures to `test` only), then `sykli lock sykli.json`. Nothing is globbed.

## Toolchain constraints

- Edition 2024, `rust-version = "1.85"`. `rust-toolchain.toml` pins 1.98, so CI builds with 1.98, where let-chains (`if let … && …`, stable since 1.88) compile without a warning yet break 1.85. Keep them out (`Option::filter`, `is_some_and`, a nested `if`) and run `cargo +1.85 check --workspace --all-targets --locked` before pushing.
- No dependencies beyond clap, serde, serde_json, sha2. Unix syscalls are declared once, in `unsafe extern "C"` blocks in `src/journal.rs`, not via libc; on Windows, `BCryptGenRandom` once, in `windows_random` in `src/main.rs`.
- `src/journal.rs`, `src/runs.rs`, `status` and `resume` are `#[cfg(unix)]` (on Windows `runs` is a stub of uninhabited types); anything only Unix code uses must be `cfg_attr(not(unix), allow(dead_code))`.

## Working rules

- Never push to `main`; branch, push, open a PR. Never merge without the owner's explicit approval of that PR. Push a branch only when it is complete and say so: merges here have repeatedly caught branches mid-push.
- Validate locally before pushing (the gate above, and any check your launch prompt names). CI runs the Linux gate and the Windows tests on every pull request and on `main`, on `ubuntu-latest` and `windows-latest` (the repository is public, so hosted runners are free; macOS is covered locally). Do not push a release tag (`release.yml` builds five targets) unless told to.
- `.sykli/` and other tools' local state directories are ignored.
- Code is Rust or Elixir/Erlang; never add Python, Go or Node.

## Lessons

<!-- This repository's accepted knowledge: one line per node, each marked with its id, written here through a pull request when a person accepts one. A line changed here by hand is replaced the next time it is written: change a lesson where it is decided. -->

- <!-- lesson: sykli-doc-comment-includes-count --> **fact**: sykli's Cargo inputs check counts a file include written in a doc comment as a real include, so the contract then asks for that file. Reword the comment, or declare the file in a task's inputs. (as of 2026-10-27)
- <!-- lesson: sykli-pre-push-only-verifies-receipt --> **fact**: sykli's pre-commit hook runs the whole configured graph, but its pre-push hook only checks for an existing receipt on gated branches and runs no checks itself. Feature branches are not gated by default. Project detection covers only Cargo, npm and Go, so Elixir/Erlang repos need tasks written by hand. (as of 2026-10-27)
- <!-- lesson: sykli-runs-on-windows-quote-paths-in-test-shells --> **lesson**: sykli is also checked on Windows: windows-latest CI, plus clippy and a type check for the Windows target. Never split a path at ':', because drive letters contain one. Quote every path in shell commands that tests build, because sh on Windows reads backslashes as escapes. (as of 2026-10-27)
