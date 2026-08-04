# sykli

**Sykli executes declared graphs and proves what ran.**

A receipt claims exactly what ran — never what it meant.

Sykli is the execution engine of the False Systems stack: pipelines declared
as real code, compiled to a typed DAG, executed in parallel with
content-addressed caching and delta selection, producing receipts bound to
the exact repository tree. Teko owns work. Toimija verifies repositories.
Kisko runs workers. Ahti stores records. Sykli runs graphs.

## Status

**Bootstrapping.** The founding document and decision records are in
[`docs/founding.md`](docs/founding.md) and [`docs/adr/`](docs/adr/). The v0
slice — parse, validate, execute, cache, delta, receipt — is not implemented
yet; the CLI says so honestly.

The predecessor (Elixir implementation, five SDKs, schema v1–v5) lives at
[false-systems/sykli-elixir](https://github.com/false-systems/sykli-elixir)
as the reference implementation; its test suite is the executable
specification for this rewrite.

## Not

Not a work tracker. Not a verification authority. Not an agent runner. Not a
datastore. Not a server. Not an interpreter of what results mean.
[`docs/adr/0005-deletions.md`](docs/adr/0005-deletions.md) is the normative
list — capabilities recorded there do not return without meeting their
stated re-entry condition.

## Build

```bash
cargo build
cargo test
```

## License

MIT.
