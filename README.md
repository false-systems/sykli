# sykli

**Know what actually ran. Not what someone says ran.**

Sykli is a small command-line tool that runs the work you declare, ties every
result to the exact files it was computed from, and writes that down in plain
JSON anyone can check. When someone, or some agent, says "tests passed" or "the
build is done" or "this PR is ready", sykli is how you know.

Runs locally on Linux, macOS and Windows. One binary. No server, no account.

## The problem

- A green check tells you a workflow finished. It does not tell you which
  commit, which tests, or whether the code changed since.
- A build that stops halfway leaves the next person guessing what was built,
  where it is, and what remains. The chat log is not evidence.
- Agents report success. Some of it is true. You have no cheap way to tell.

Sykli's answer is a **receipt**: a record of exactly what ran, on exactly which
inputs, with exactly which outcome. Change one input and the receipt no longer
applies. Nothing inherits a pass it did not earn.

## Three things you can do today

### 1. Run your checks and get a receipt

Declare the tasks a change needs and the files they depend on. Sykli runs only
what the change affects, caches by content, and records the result.

```sh
sykli init                  # detects Cargo, npm or Go and writes sykli.json
sykli run --json            # runs the graph, prints a receipt
sykli verify receipt.json   # is this receipt still true for the tree in front of me?
```

`verify` answers with an exit code you can trust: 0 verified, 1 the work
failed, 3 the tree changed since, 4 the declared tasks changed since. A stale
receipt cannot pass as a fresh one.

### 2. Build something, stop, let someone else finish

Declare a target: the source files, the build, the checks that must pass.
Sykli captures the source, builds, runs the checks, and saves the artifact with
its identity. If you stop early, another terminal or another agent resumes
from the saved state with one ID.

```sh
sykli init --production --smoke '"$SYKLI_INPUT_executable" --help'
sykli produce sykli --stop-after build    # exits 1: built, checks remain
sykli resume PRODUCTION_ID                # someone else finishes the checks
```

The next worker needs the ID and the local store, not the previous worker's
conversation. Edit the source and you get a new production; old passing checks
never count for new bytes. Linux and macOS.

### 3. Ask whether a pull request is actually ready

Point sykli at a pull request. It reads the CI runs and reviews through your
existing `gh` login, saves what GitHub said as an immutable bundle, and tells
you which of *your* requirements are established, which are refuted, and which
are still unproven, and why.

```sh
sykli inspect --repo false-systems/sykli --pr 25 --requirements review.json
```

```text
0e1982a… — UNPROVEN
Scope: declared review-readiness conditions; advisory

✓ ci      GitHub reports success for the required workflow
? review  No allowed reviewer has approved this candidate

Trust: local collector and store; receipt is not authenticated
```

Requirements are a small file naming exact workflow and reviewer IDs. An
approval on an older commit does not count. A newer failing run hides an older
green one. A run from a fork or a different workflow is excluded and says so.
Replay the saved bundle later, offline, and get the same answer.

## Why it is different

- **Content-addressed.** Inputs, contracts, artifacts and evidence are named by
  what they contain. Same bytes, same identity; different bytes, different
  result. There is no "latest" to point at the wrong thing.
- **Exit codes and versioned JSON.** Every command answers a human on the
  screen and an agent on stdout with the same facts. Scripts branch on exit
  codes; agents read `sykli-*.v1` documents with stable fields.
- **Honest about its limits.** A receipt says what ran, never what it meant.
  Every assessment prints its trust boundary. Sykli never merges, deploys,
  triggers or certifies anything.
- **Local.** Files in your repository, your `gh` login, your machine. Nothing
  to host, nothing to sign up for.

## Try it in a minute

```sh
curl -fsSLO https://raw.githubusercontent.com/false-systems/sykli/main/install.sh
sh install.sh v0.6.0
cd your-repo && sykli init && sykli run
```

Or `cargo install --git https://github.com/false-systems/sykli --tag v0.6.0 --locked sykli`
with Rust 1.85 or newer. Windows zips, Homebrew and the GitHub Action are in
[installation options](docs/install.md).

## What sykli is not

Not a CI service, a work tracker, an agent runner or a merge bot. It does not
run in the cloud, does not watch anything, and does not interpret results
beyond the condition you declared. Those jobs belong to other tools; sykli
gives them evidence. The [deletion record](docs/adr/0005-deletions.md) lists
what was removed on purpose and what it would take to bring anything back.

## Go deeper

- [Graph and receipt specification](docs/spec.md), [GitHub Action](docs/github-actions.md)
- [Typed production: contract, storage, resume, limits](docs/production.md)
- [Pull-request evidence: requirements, predicates, trust](docs/inspect.md) and its [design](docs/standalone-ci-evidence.md)
- [Working with agents](docs/agents.md)
- [Design decisions](docs/adr/)
- [Contributing](CONTRIBUTING.md), [changelog](CHANGELOG.md), [security](SECURITY.md)

MIT.
