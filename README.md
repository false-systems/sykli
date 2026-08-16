# sykli

**Sykli is the content-addressed evaluator for declared work graphs.**

Declare work once, ask what applies to a repository change, execute it
anywhere, and reuse results backed by receipts. CI is one client; coding agents
and local development use the same graph.

A receipt claims exactly what ran — never what it meant.

Sykli is the execution engine of the False Systems stack: pipelines declared
as real code, compiled to a typed DAG, executed in parallel with
content-addressed caching and delta selection, producing receipts bound to
the exact repository tree. Teko owns work. Toimija verifies repositories.
Kisko runs workers. Ahti stores records. Sykli runs graphs.

## Evaluation model

The contract is the query, `sykli plan` is `EXPLAIN`, delta selection and the
cache are the optimizer, `sykli run` evaluates the graph, and the receipt is
the immutable result record. Equal contract, declared inputs, and runtime
fingerprints have equal execution identity; equal command outcomes additionally
require the declared work itself to be deterministic.

## Status

**Bootstrapping.** The founding document and decision records are in
[`docs/founding.md`](docs/founding.md) and [`docs/adr/`](docs/adr/). The v0
engine slice — parse, validate, execute, local cache, delta plan, receipt — is
implemented with the Rust SDK, contract locking, and a self-hosted repository
gate; tagged Linux and macOS packages plus the checked installer complete the
v0 distribution path, and a composite Action carries the same graph onto hosted
runners.

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

## Build and use locally

```bash
cargo build --locked
cargo run --quiet --locked -- validate sykli.json
cargo run --quiet --locked -- plan sykli.json --changed src/main.rs --json
cargo run --quiet --locked -- run sykli.json --json
```

The first command builds the `sykli` binary. The next three validate, explain,
and evaluate this repository's locked graph without installing anything.

## How it works

A `sykli.json` contract declares commands, their inputs, and their dependencies:

```json
{
  "schema": "sykli-contract.v1",
  "tasks": [
    {
      "name": "fmt",
      "run": "cargo fmt --check",
      "inputs": ["Cargo.toml", "src/main.rs"]
    },
    {
      "name": "test",
      "run": "cargo test --workspace --locked",
      "after": ["fmt"],
      "inputs": ["Cargo.toml", "src/main.rs"]
    }
  ]
}
```

`validate` checks the graph. `plan --changed` selects tasks whose declared
inputs changed, plus their dependents. `run` executes independent tasks in
parallel, reuses results with matching contract, input, and runtime hashes,
then writes a receipt describing exactly what ran.

```text
contract -> validate -> plan -> run or cache -> receipt
```

`sykli.lock` pins the contract hash so an unexpected contract change fails
instead of silently changing the graph.

## Agent workflow

Start the agent through Toimija so it receives the live repository packet:

```bash
toimija run --intent "describe the change" --task "do the work" -- codex
```

During the session the agent queries affected work with `sykli plan --json`.
Before handoff it runs the authoritative graph:

```bash
toimija gates run sykli-full
```

## GitHub Actions

CI is a client, not the product. The bundled Action is a shim that installs
`sykli`, evaluates the same graph, and attaches the receipt:

```yaml
      - uses: actions/checkout@v7
        with:
          fetch-depth: 0
      - uses: false-systems/sykli@v0.1.0
        with:
          contract: sykli.json
```

The ref you pin is the version it installs. It reports the affected task set
for a pull request, renders the receipt into the job summary, and gates on
`sykli verify` — whose exit code says whether the work failed, the receipt went
stale, or the contract drifted. A runner is a machine that runs `sykli` with a
cold cache, not a place where truth lives.
[`docs/github-actions.md`](docs/github-actions.md) has the inputs, outputs, and
what the Action deliberately does not do.

## Rust contracts

Repositories may generate the same contract from Rust by exposing `sykli.rs`
as an opt-in Cargo binary:

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
sykli run --json
sykli plan --changed src/lib.rs
sykli plan --changed src/lib.rs --json
```

## Install

```bash
curl -fsSLO https://raw.githubusercontent.com/false-systems/sykli/main/install.sh
sh install.sh v0.1.0
```

Set `SYKLI_INSTALL_DIR` to install somewhere other than `~/.local/bin`.

## License

MIT.
