# Contributing

Sykli is small on purpose. Before proposing a change, read the product
boundaries in [`AGENTS.md`](AGENTS.md): they are the normative list of what
sykli is not. A feature that makes sykli a server, a scheduler, a datastore, or
an interpreter of what results mean belongs in another tool.

## The gate

Every change is evaluated by sykli's own declared graph. Run it before
opening a pull request:

```bash
cargo run --quiet --locked -- run sykli.json --json
```

That is what CI runs. If you have Toimija, `toimija gates run sykli-full`
runs the same graph against an isolated snapshot and records a receipt.

The QA graph for behaviour changes lives in `tests/qa/sykli.json`; add a
task there when you add behaviour, and make sure it fails before your change
and passes after.

## Pull requests

- Branch from `main`, one concern per pull request, targeting `main`.
- `cargo fmt --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`,
  and `cargo test --workspace --locked` are part of the gate; a red gate does
  not merge.
- A contract or receipt schema change needs a new schema version; existing
  identities never change meaning.
- Commit messages: conventional prefixes (`feat:`, `fix:`, `docs:`, `ci:`),
  the body says why.

## Releases

Bump `version` in `Cargo.toml`, add the section to `CHANGELOG.md`, and push a
tag `vX.Y.Z` that matches the crate version; the release workflow refuses a
mismatch. It builds the tarballs and the Homebrew formula.
