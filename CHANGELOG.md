# Changelog

All notable changes to sykli are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[SemVer](https://semver.org/). Every entry names what a receipt, plan, or
contract consumer can observe; internal refactors are not listed.

## [Unreleased]

### Added
- Container images published on release: `ghcr.io/false-systems/sykli:<tag>`
  (the static binary on scratch) and `<tag>-tools` (Debian slim with `git`
  and `jq`, for CI).
- A Homebrew formula (`sykli.rb`) attached to every release, generated from
  the released tarballs' checksums.
- `docs/install.md`, `CONTRIBUTING.md`, `SECURITY.md`, this changelog.

## [0.2.0] - unreleased

### Added
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

[Unreleased]: https://github.com/false-systems/sykli/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/false-systems/sykli/releases/tag/v0.2.0
[0.1.0]: https://github.com/false-systems/sykli/commit/f536224
