# sykli

**Know what actually ran. Not what someone says ran.**

## What it is

Sykli is a small command-line tool you point at a repository. In one JSON file
you name the commands that matter (`cargo test`, `npm run lint`, `go build`)
and the files each one depends on. Sykli runs them and writes a **receipt**: a
plain JSON record of exactly what ran, on exactly which files, with exactly
which result. Later, anyone, human or agent, can ask `sykli verify` whether
that receipt is still true for the code in front of them.

That is the whole idea. Your build tools still do the building and testing.
Sykli only remembers, precisely, and lets others check. Everything else in this
repository is that one idea applied to three situations.

## Thirty seconds, end to end

A repository with two checks. The file is `sykli.json`; `sykli init` writes one
like it for Cargo, npm and Go projects.

```json
{"schema":"sykli-contract.v1","tasks":[
  {"name":"lint","run":"grep -qv TODO src.txt","inputs":["src.txt"]},
  {"name":"test","run":"sh test.sh","inputs":["src.txt","test.sh"],"after":["lint"]}]}
```

```sh
$ sykli run --json > .sykli/receipt.json
running: lint
passed: lint
running: test
passed: test
```

The receipt, trimmed:

```json
{
  "schema": "sykli-receipt.v1",
  "contract_hash": "c5a2bbbde46a…",
  "subject": { "tree_oid": "596e8c346e48…", "inputs_digest": "9daed25e15ee…", "dirty": false },
  "tasks": [
    { "name": "lint", "command": "grep -qv TODO src.txt", "outcome": "passed", "exit_code": 0, "stdout_digest": "e3b0c44298fc…" },
    { "name": "test", "command": "sh test.sh",            "outcome": "passed", "exit_code": 0, "stdout_digest": "e3b0c44298fc…" }
  ],
  "outcome": "passed"
}
```

Every field is a fact, not a summary: which tree (`tree_oid`), which declared
inputs (`inputs_digest`), which contract (`contract_hash`), which command,
which exit code, a digest of what it printed. Now check it, then change a file
and check again:

```sh
$ sykli verify .sykli/receipt.json
verified: receipt matches this tree and contract        # exit 0
$ echo "// TODO" >> src.txt
$ sykli verify .sykli/receipt.json
mismatch: tree expected 596e8c34… but got 21fa0d9e…     # exit 3: the code moved on
```

Run again and only what the change touched runs; the rest is served from a
content-addressed cache and marked `cached` in the new receipt.

## Three words

- **Contract.** The JSON file: tasks, their commands, the files they read,
  and `after` for ordering. Nothing else. No plugins, no DSL, no interpretation
  of what a command means.
- **Receipt.** What one run established, bound by hashes to the exact tree,
  inputs and contract. Change any of them and the receipt no longer applies.
- **Verify.** The question "is this receipt still true here?", answered with
  an exit code: 0 yes, 1 the work failed, 3 the code changed, 4 the contract
  changed, 2 that is not a valid receipt.

## Where it sits

- **Under your tools.** Sykli runs the commands you already have. It does not
  replace cargo, npm, go, or your CI runner.
- **Beside your CI.** The GitHub Action in this repository just calls `sykli`
  and attaches the receipt. Any CI can do the same; so can a laptop.
- **In front of agents.** An agent gets the same exit codes and the same JSON
  a script does. It does not need to trust its own memory of what it ran, and
  you do not need to trust its summary.
- **On your machine.** Files in `.sykli/` inside the repository. No server,
  no account, no network for the graph and production surfaces; the
  pull-request surface reads GitHub through the `gh` login you already have.

## The one rule

Everything is named by its content. A contract, a receipt, an input, a built
artifact, a saved GitHub response: each has an identity that is a hash of its
bytes. Same bytes, same identity; one byte different, a different identity and
a different result. There is no "latest", no timestamp to trust, no name that
can quietly point at something else. That is what makes a receipt worth more
than a green check or a chat message saying "done".

## Why this exists

- A green check says a workflow finished. It does not say which commit, which
  tests, or whether the code changed since.
- Work that stops halfway leaves the next person guessing what was built, where
  it is, and what remains. A chat log is not evidence.
- Agents report success. Some of it is true. You need a cheap way to tell.

Runs on Linux, macOS and Windows. One binary. The rest of this page is the
three situations the idea applies to.

## Three things you can do today

### 1. Run your checks and get a receipt

The walkthrough above, for real projects. `init` detects the ecosystem and
writes the contract; `run` executes only what a change affects and records the
result; `verify` checks a receipt against the code in front of you.

```sh
sykli init                                   # detects Cargo, npm or Go; writes sykli.json, ignores .sykli/
sykli run --json > .sykli/receipt.json       # runs the graph; the receipt lives outside the tree it describes
sykli verify .sykli/receipt.json             # is this receipt still true for the tree in front of me?
```

`verify` answers with an exit code you can trust: 0 verified, 1 the work
failed, 3 the tree changed since, 4 the declared tasks changed since. A stale
receipt cannot pass as a fresh one. What `verify` proves is that a receipt
matches the tree and contract in front of you and agrees with itself; it does
not prove who wrote the receipt. Run it where the receipt was produced, as the
GitHub Action does, or sign receipts before trusting them across a boundary. Tasks see only `PATH`, `HOME` and `TMPDIR`
unless they declare `env` values or `inherit` named variables from your
environment; inherited values reach the command, and only their digests reach
the receipt.

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

- **Exit codes and versioned JSON.** Every command answers a human on the
  screen and an agent on stdout with the same facts. Scripts branch on exit
  codes; agents read `sykli-*.v1` documents with stable fields.
- **Honest about its limits.** A receipt says what ran, never what it meant.
  Every assessment prints its trust boundary. Sykli never merges, deploys,
  triggers or certifies anything.

## Try it in a minute

```sh
curl -fsSLO https://raw.githubusercontent.com/false-systems/sykli/main/install.sh
sh install.sh v0.6.0
cd your-repo && sykli init && sykli run
```

Or `cargo install --git https://github.com/false-systems/sykli --tag v0.6.0 --locked sykli`
with Rust 1.85 or newer. Each release also carries a Windows zip, a Homebrew
formula and `SHA256SUMS`; `action.yml` is a GitHub Action that installs the
release matching its ref and runs the graph.

## What sykli is not

Not a CI service, a work tracker, an agent runner or a merge bot. It does not
run in the cloud, does not watch anything, and does not interpret results
beyond the condition you declared. Those jobs belong to other tools; sykli
gives them evidence. `AGENTS.md` states the boundaries and what does not come
back without a named user.

## Go deeper

- [Working with agents](docs/agents.md)
- `sykli <command> --help` for every flag and exit code
- [Contributing](CONTRIBUTING.md), [changelog](CHANGELOG.md), [security](SECURITY.md)

MIT.
