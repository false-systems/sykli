<h1 align="center">sykli</h1>

<p align="center"><strong>Run your checks. Reuse the work. Explain the result.</strong></p>

<p align="center">
  <img alt="License: MIT" src="https://img.shields.io/badge/license-MIT-blue">
  <img alt="Rust 1.85+" src="https://img.shields.io/badge/rust-1.85%2B-orange">
  <img alt="Linux, macOS, Windows" src="https://img.shields.io/badge/platforms-linux%20%7C%20macos%20%7C%20windows-lightgrey">
  <img alt="Runs locally" src="https://img.shields.io/badge/runs-locally-success">
</p>

Sykli gives your repository one declared graph for checks, a local cache for
reusable results, and a receipt for every run. Use the same commands on your
laptop, in CI, or from a coding agent.

Keep Cargo, npm, Go and your shell scripts. Declare what each task reads and
what it depends on; Sykli handles ordering, checks cached evidence, and records
which tasks ran or reused a result. Before running anything, ask it to explain
what a change affects and what the cache can supply.

**One binary. Local execution. Readable CLI, structured JSON when you need it.**

## Start in your repository

Install from the current source with Rust 1.85 or newer:

```sh
cargo install --git https://github.com/false-systems/sykli --branch main --locked sykli
```

In an existing Cargo, npm or Go repository:

```sh
sykli init                                  # generate sykli.json and pin it in sykli.lock
sykli plan --explain                         # inspect selection and cache evidence
sykli run sykli.json --json > .sykli/receipt.json
sykli verify .sykli/receipt.json --contract sykli.json
```

`init` creates `.sykli/` for local state and adds it to `.gitignore`. Review the
input declarations it generates, especially configuration, fixtures and files
you add later. Task execution needs Git, a POSIX `sh`, and your project's tools.
On Windows, Git for Windows supplies `sh`.

This README describes `main`, including `plan --explain`. For reproducible
installs, replace `--branch main` with `--rev COMMIT`. Tagged downloads, when
published, are listed on the [releases page](https://github.com/false-systems/sykli/releases).
The repository includes an installer that verifies release archive checksums;
see [install.sh](install.sh). Installing Sykli does not install your build tools.

## Why use Sykli?

- **See the work before running it.** Explain why a task is selected, whether
  cache evidence is available, and which dependencies still need resolution.
- **Reuse passing work.** Cache keys cover the task declaration, declared
  input contents and execute bits, runtime fingerprint, inherited environment
  digests, and dependency keys. A matching entry must validate before reuse.
- **Keep a record tied to the code.** Each receipt binds task outcomes to the
  repository tree, declared inputs and contract. Verify it before relying on it.
- **Give agents a concrete answer.** The CLI does the querying; `--json` gives
  scripts and agents the same information with versioned schemas and exit codes.
- **Use it wherever your commands run.** Local development and CI share the
  graph. Sykli stays a local CLI; your CI service owns runners and scheduling.

```mermaid
flowchart LR
    graph["Declare tasks and inputs"] --> plan["Explain the plan"]
    plan --> run["Run or reuse validated results"]
    run --> receipt["Record a receipt"]
    receipt --> verify["Verify against the current code"]
```

## A small graph you can try

In a scratch Git repository, create two scripts and this `sykli.json`:

```sh
printf 'echo lint passed\n' > lint.sh
printf 'echo tests passed\n' > test.sh
```

```json
{
  "schema": "sykli-contract.v1",
  "tasks": [
    { "name": "lint", "run": "sh lint.sh", "inputs": ["lint.sh"] },
    { "name": "test", "run": "sh test.sh", "inputs": ["test.sh"], "after": ["lint"] }
  ]
}
```

`inputs` names the files a task reads. `after` makes `test` wait for `lint`
and binds its cache key to that dependency. Independent tasks can run in parallel.

```sh
mkdir -p .sykli
sykli run sykli.json --json > .sykli/first.json
sykli run sykli.json --json > .sykli/second.json
sykli verify .sykli/second.json --contract sykli.json
```

The first run executes both scripts. With unchanged inputs and valid local
cache entries, the second reuses both passing results. Change `test.sh` and
run again: `lint` can be reused while `test` executes.

A **contract** declares the work. A **receipt** records one evaluation,
including commands, outcomes, output digests and whether results were cached.
**Verify** checks that the record is consistent with the current tree and contract.
Keep receipts under `.sykli/` or outside the repository so they do not change
the tree they describe.

## Ask what changed—and what can be reused

```sh
sykli plan --explain
sykli plan --changed test.sh --explain
sykli plan --changed test.sh --explain --json
```

`--explain` reads `sykli.json` by default. It runs no task or Rust emitter,
restores no outputs, and writes no cache entries or receipts. An explicit
JSON contract path selects another graph.

| Cache state | What it tells you |
|---|---|
| `available` | Cached evidence and artifacts validate; execution can attempt reuse. |
| `missing` | No entry exists for the current content key. |
| `invalid` | An entry exists but its evidence or artifacts failed validation. |
| `deferred` | A dependency needs execution, restoration or error resolution first. |
| `input_error` | A declared input cannot currently be evaluated. |

`--changed` takes paths **you supply** and filters the displayed plan to
matching declared inputs and downstream tasks. It does not discover a Git diff
or restrict a later `run`. The full run evaluates the graph and decides reuse
from content keys and valid cache evidence.

Input declarations matter: Sykli does not observe every file or tool your
commands read. Undeclared dependencies can make cached results misleading.
An explanation describes the current local evidence, not a guaranteed future
outcome. See [the agent guide](docs/agents.md) for selection reasons, JSON fields
and error behavior.

## Use the same graph in CI

After checkout and installation of Sykli and your project toolchain:

```sh
sykli run sykli.json --json > .sykli/receipt.json
sykli verify .sykli/receipt.json --contract sykli.json
```

Create `.sykli/` before redirecting if this is a fresh checkout. Configure your
CI to fail on either command's nonzero exit and retain the receipt as an artifact.
The included [GitHub Action](action.yml) handles running, verification, a job
summary and receipt upload. It also reports affected tasks on pull requests.

### Measuring smarter CI

This repository now runs a [shadow experiment](docs/ci-shadow.md) on pull
requests. It records changed paths, runs the full candidate graph, then compares
that result with reuse proposed from a separate base-commit run. It reports
agreements, disagreements, unmapped paths and potential reused task time.

**All required checks still run.** The experiment adds a baseline run to measure
whether skipping could be justified later. It does not claim CI savings today,
and matching observations do not prove that declarations are complete.

## Build artifacts and resume unfinished checks

On Linux and macOS, typed production captures declared source, builds an artifact
and runs required checks. A production ID lets another terminal or agent resume
from the same local store.

For a Cargo binary or Go main package:

```sh
sykli init --production --smoke '"$SYKLI_INPUT_executable" --help'
sykli targets
sykli produce TARGET --stop-after build
sykli resume PRODUCTION_ID
```

Use the discovered target name and the production ID printed by Sykli.
Stopping after build returns exit 1 when checks remain. Artifact availability
and completed checks are recorded separately; failed work requires an explicit
retry, and an unresolved lost attempt cannot silently become a success.

See [the production agent loop](docs/agents.md) for preparation without execution,
individual operations, bounded parallel work and diagnostics.

## Inspect pull-request evidence

With an authenticated GitHub CLI (`gh`), collect CI and review evidence for an
exact candidate and assess requirements you declare:

```sh
sykli inspect --repo OWNER/REPO --pr NUMBER --requirements review.json
sykli assess BUNDLE --requirements review.json
```

The requirements file names exact workflow and reviewer IDs. Sykli saves the
collected responses so the assessment can be replayed offline. Results explain
which requirements are established, refuted or unproven, including gaps and
excluded evidence. An approval of an older commit does not satisfy approval of
the current candidate.

These assessments are advisory. They establish declared review-readiness
conditions, not permission to merge or deploy. `inspect` makes bounded,
read-only requests through `gh`; it does not post reviews or trigger work.

## What a receipt establishes

`verify` checks record consistency, contract identity, the repository tree and
declared inputs, then the recorded outcome. It does **not** authenticate the
author of a receipt or prove that the declared checks are sufficient.

| `verify` exit code | Meaning |
|---|---|
| `0` | Receipt matches and work passed or was reused. |
| `1` | The recorded work failed. |
| `2` | Receipt cannot be verified. |
| `3` | Tree or declared inputs changed. |
| `4` | Contract or lock differs. |

Tasks receive `PATH`, `HOME` and `TMPDIR`, plus declared `env` values and named
`inherit` variables. Inherited values reach the task; their digests are recorded
for identity. Task output is captured, so commands remain responsible for what
they print. Sykli executes local commands with your permissions.

Machine-readable documents have versioned schemas. Incompatible changes get a
new schema version; command help documents flags and exit codes.

## Learn more and contribute

- [Agent guide](docs/agents.md): JSON queries, explanations, production and assessment.
- [CI experiment](docs/ci-shadow.md): methodology, artifacts and limitations.
- [Changelog](CHANGELOG.md): version history.
- [Contributing](CONTRIBUTING.md) and [product boundaries](AGENTS.md).
- [Security policy](SECURITY.md): report vulnerabilities privately.

Part of False Systems. Licensed under [MIT](LICENSE).
