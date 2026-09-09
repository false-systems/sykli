# Changelog

All notable changes to sykli are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[SemVer](https://semver.org/). Every entry names what a receipt, plan, or
contract consumer can observe; internal refactors are not listed.

## [Unreleased]

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
