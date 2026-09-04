# Sykli in GitHub Actions

The Action is a shim. It installs a released `sykli`, evaluates the declared
graph, verifies the receipt against the tree that produced it, and attaches the
receipt to the run. Nothing about GitHub enters the binary:
[ADR-0005](adr/0005-deletions.md) records webhook receivers, GitHub Apps, the
Checks API, and SCM status as never inside sykli — "triggers are external
shims; a dumb Action invokes `sykli`". This is that Action.

A runner is a machine that runs `sykli` with a cold cache, not a place where
truth lives. The receipt is the output that matters.

## Use

```yaml
name: CI
on: [pull_request, push]

permissions:
  contents: read

jobs:
  gate:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v7
        with:
          fetch-depth: 0
      - uses: false-systems/sykli@v0.1.0
        with:
          contract: sykli.json
```

The Action installs the release matching the ref it was called with, so
`@v0.1.0` runs v0.1.0. Pin the ref; that pin is the version.

Whatever the contract's tasks need — a toolchain, a package manager, a
database — is the workflow's job to install before this step. Sykli executes
commands; provisioning the machine is somebody else's problem.

## Inputs

| Input | Default | Meaning |
|---|---|---|
| `contract` | `sykli.json` | Path to `sykli.rs` or a `sykli-contract.v1` JSON file, relative to `working-directory`. |
| `version` | the Action's own ref | Release tag (`vX.Y.Z`), or `source` to build the checkout with cargo. |
| `working-directory` | `.` | Directory to evaluate the contract in. |
| `plan` | `true` | On pull requests, report which tasks the changed files affect. |
| `verify` | `true` | Check the receipt against the tree and contract that produced it. |
| `upload-receipt` | `true` | Attach the receipt to the run as an artifact. |
| `artifact-name` | `sykli-receipt` | Name of that artifact. |

## Outputs

| Output | Meaning |
|---|---|
| `receipt` | Path to the receipt JSON on the runner. |
| `outcome` | `passed`, `cached`, `failed`, or `errored`. |
| `tree-oid` | OID of the working tree the receipt describes. |
| `contract-hash` | Hash of the contract that was evaluated. |
| `affected` | Tasks the pull request's changed files affect, comma-separated. |
| `verify-code` | Exit code from `sykli verify`. |

## Verify exit codes

Verify's codes are stages ([ADR-0007](adr/0007-verification.md)): checks run in
order and the first failing stage decides. Every non-zero code withholds the
gate; they differ in what clears it.

| Code | Meaning | What to do |
|---|---|---|
| 0 | verified | Nothing. The receipt matches this tree and contract. |
| 1 | the work failed | Read the task table in the job summary. A task failed or its evidence was incomplete. |
| 2 | cannot verify | The receipt, contract, or git state is unreadable. A broken gate, not a failed one. |
| 3 | receipt is stale | The tree or declared inputs changed after the run. Usually a task wrote a file that is neither gitignored nor declared as an `outputs` entry. |
| 4 | contract drifted | The contract no longer matches `sykli.lock`. Run `sykli lock` and commit it. |

Code 3 is the one that surprises people in CI. Sykli content-addresses the
working tree, not `HEAD`, so a task that leaves a stray file behind changes the
subject out from under its own receipt. Declare the file as an output, ignore
it, or stop writing it.

## Delta plans explain; they do not shrink

On a pull request the Action runs `sykli plan --changed` over the files that
differ from the base commit and reports the affected task set. It then runs the
whole graph anyway.

That is not a bug in the Action — `sykli run` has no task filter, by design.
Delta is a *plan* operation and the cache is an *execution* operation
([ADR-0004](adr/0004-cache-model.md)); they share the `inputs` contract, not a
code path. What skips work is a cache hit, not a plan. In CI, with a cold
cache, nothing skips. The plan tells you what a warm machine would have
reconsidered.

The plan needs both commits in the checkout, so set `fetch-depth: 0`. Without
it the Action warns and continues; a plan that cannot be computed never fails
the gate.

## Cold caches are the honest default

The Action does not restore `.sykli/`. Per ADR-0004 a cold runner
re-executing is acceptable and honest, and it keeps the cache-correctness bar
high before any cache is shared. If you cache `.sykli/` yourself, know what
you are trading:

- A cache entry only resolves if the receipt that produced it is present too,
  so `.sykli/cache` and `.sykli/receipts` must be restored together — a
  dangling provenance ref demotes the hit to a miss, deliberately.
- The cache key includes a runtime fingerprint over the shell binary and the
  inherited environment (`PATH`, `HOME`, `TMPDIR`). Any step that changes
  `PATH` — most `setup-*` actions do — changes the fingerprint and therefore
  every key. Expect misses across differently-ordered workflows.

Add `.sykli/` to `.gitignore`. Receipts and cache entries live there, and
while the tree OID always excludes that directory, `git status` should not
have to.

## Requirements

- Linux or macOS runners. The installer resolves `linux`/`macos` and
  `x86_64`/`aarch64`; Windows is unsupported.
- `jq` for the job summary and the `outcome`, `tree-oid`, and `contract-hash`
  outputs. It is present on GitHub-hosted runners; on a self-hosted runner
  without it the Action warns, attaches the receipt, and still gates.
- `git`, for the tree OID the receipt is bound to.

## Building from source

`version: source` builds the checkout with `cargo build --locked` instead of
installing a release, into a target directory outside the workspace. It exists
for one case: a repository that *is* sykli. That is how this repository's own
gate runs — every change here is evaluated by the Action it ships.

```yaml
      - uses: ./
        with:
          contract: sykli.json
          version: source
```
