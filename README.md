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
engine slice — parse, validate, execute, local cache, delta plan, receipt — is
implemented with the Rust SDK, contract locking, and release guardrails;
distribution packaging remains.

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
cargo xtask gate
```

## Contract

Repositories expose `sykli.rs` as an opt-in Cargo binary:

```toml
[features]
sykli = ["dep:sykli"]

[[bin]]
name = "sykli"
path = "sykli.rs"
required-features = ["sykli"]

[dependencies]
sykli = { git = "https://github.com/false-systems/sykli", optional = true }
```

The emitter uses `sykli::Pipeline`:

```rust
use sykli::Pipeline;

fn main() {
    let mut pipeline = Pipeline::new();
    let _ = pipeline.task("test").run("cargo test");
    pipeline.emit();
}
```

The CLI compiles it automatically:

```bash
sykli lock
sykli validate
sykli run
sykli plan --changed src/lib.rs
```

## License

MIT.
