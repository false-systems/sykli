# sykli

**Source to artifact, across replaceable workers.** Sykli captures declared
source inputs, executes a typed target, and returns an identified artifact
with its required checks. A fresh worker can inspect the stored production
and finish the remaining operations without the previous worker's session.

The new production path is opt-in in this checkout. In a Cargo workspace with
a binary, using a Sykli binary built from this source:

```sh
sykli init --production --smoke '"$SYKLI_INPUT_executable" --help'
sykli targets --json
sykli plan sykli.production.json --target app --json
sykli produce app --json
# In a new client, using the returned production ID:
sykli status PRODUCTION_ID --json
sykli resume PRODUCTION_ID --json
```

Choose a smoke command appropriate to your program. For multiple Cargo binaries,
select `--package NAME --bin NAME`. Standalone Rust `main.rs` is also supported.

The result names the source, collected executable, source-unit-test result and
executable-smoke-test result. Changing source or the contract creates a different
production. Failed or unresolved work remains visible; missing artifact bytes
cannot count as successful delivery. See the [short walkthrough and contract](docs/production.md)
and [reproducible demonstration](examples/production/demo.py).

This is trusted local execution on Linux/macOS, with prepared input copies and
no cross-production reuse. It is not a security sandbox. If the executor itself
disappears without a result, its attempt remains indeterminate; Sykli does not
blindly retry it. No other False Systems tool is needed.

The existing graph interface remains supported, including its local cache and
v1 receipts:

```bash
sykli init                              # detect Cargo / npm / Go, write and lock sykli.json
sykli plan --changed src/lib.rs --json  # which tasks does this change touch?
sykli run --json                        # run them (in parallel, cached), write a receipt
sykli verify .sykli/receipts/rcpt_….json   # is that receipt about this tree and this contract?
```

One static binary. No server, no account, no daemon, nothing to host.

## Existing graph workflow

CI answers "did the pipeline pass" with logs nobody reads. Sykli answers two
better questions with data:

- **What does this change require?** `plan` selects the tasks whose declared
  inputs changed, plus everything downstream, from the graph. An agent asks
  this before touching code instead of running everything or guessing.
- **What actually ran?** `run` writes a `sykli-receipt.v1`: every task, its
  command, exit code, outputs, and whether it was observed or reused from an
  earlier receipt with identical contract, inputs, and runtime fingerprints.
  The receipt is bound to the working tree's OID and the contract's hash.
  `verify` tells a reviewer, in one exit code, whether a receipt is good,
  stale, drifted, or not evidence at all.

A receipt claims exactly what ran. It never claims what it meant.

## Getting started

Install a release (Linux, macOS; x86_64 and arm64):

```bash
curl -fsSLO https://raw.githubusercontent.com/false-systems/sykli/main/install.sh
sh install.sh v0.2.0
```

Other paths — `cargo install`, Docker, Homebrew, the GitHub Action — are in
[`docs/install.md`](docs/install.md).

In a repository:

```bash
sykli init          # writes sykli.json from what it finds, then locks it
sykli run           # first run is cold; the next one reuses what did not change
```

`init` detects Cargo, npm, and Go manifests in the current directory, declares
the files it finds as inputs, refuses to overwrite a contract without
`--force`, pins the result in `sykli.lock` unless `--no-lock`, and skips
symlinks and the build directories (`target` at the root, `node_modules`,
`vendor`). It writes only into the current directory, because inputs are
resolved from wherever `sykli` runs.

`sykli.json` is small and yours to edit:

```json
{
  "schema": "sykli-contract.v1",
  "tasks": [
    { "name": "fmt",  "run": "cargo fmt --check", "inputs": ["Cargo.toml", "src/main.rs"] },
    { "name": "test", "run": "cargo test", "after": ["fmt"], "inputs": ["Cargo.toml", "src/main.rs"] }
  ]
}
```

Every command takes the contract path as its first argument and defaults to
`sykli.json` when it exists, otherwise to `sykli.rs`, the Rust emitter.

Tasks are commands. `after` orders them, `inputs` are the files whose change
means the task must run again. `sykli.lock` pins the contract's hash so an
unexpected edit to the graph fails instead of silently changing what CI means.

## In CI

The [GitHub Action](docs/github-actions.md) is a shim: it installs the tagged
release, runs the same `sykli.json`, verifies the receipt, and attaches it to
the run.

```yaml
- uses: false-systems/sykli@v0.2.0
  with:
    contract: sykli.json
```

The runner is a machine with a cold cache, not where truth lives. The receipt
is the output.

## For agents

Agents get versioned JSON from every command and a reviewer gets `verify`.
[`docs/agents.md`](docs/agents.md) is the short version: plan before editing,
run after, hand over the receipt rather than a claim, and remember that a
`cached` outcome is reuse, not observation. A stdio MCP shim, `sykli-mcp`,
wraps the same commands for harnesses that prefer tools to shells.

## Contracts

The machine surface is versioned and documented in [`docs/spec.md`](docs/spec.md)
and [`docs/production.md`](docs/production.md):
`sykli-contract.v1`, `sykli-lock.v1`, `sykli-plan.v1`, `sykli-receipt.v1`,
`sykli-validate.v1`, and every exit code. Rust projects can also emit the
contract from code with the `sykli` crate's `Pipeline`, compiled on demand
from a `sykli.rs` binary; see [`docs/spec.md`](docs/spec.md).
[`docs/why.md`](docs/why.md) is the argument in one page: receipts, not logs.

## What sykli is not

Not a work tracker, not a verification authority, not an agent runner, not a
datastore, not a server, not an interpreter of what results mean.
[`docs/adr/0005-deletions.md`](docs/adr/0005-deletions.md) is the normative
list; [`ADR-0009`](docs/adr/0009-local-production.md) records the user-requested
local production extension and its boundaries.
Everything that needs one of those things is a separate tool that consumes
receipts.

## Project

- [`docs/founding.md`](docs/founding.md) and [`docs/adr/`](docs/adr/): why it
  is shaped like this.
- [`CHANGELOG.md`](CHANGELOG.md), [`CONTRIBUTING.md`](CONTRIBUTING.md),
  [`SECURITY.md`](SECURITY.md).
- Sykli's own CI is a `sykli.json`; every merge to `main` produces a receipt.

MIT.
