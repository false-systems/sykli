# sykli

**Build an artifact. Keep the progress.**

Sykli runs your build and tests, saves the resulting artifact, and records which
source and checks belong to it. If you stop halfway through, another terminal
or agent can finish the same work from its saved state.

Your build tools still do the building. Sykli keeps track of the inputs, results,
and unfinished work. It runs locally on Linux and macOS, with no account or server.

## Why use it?

A build can outlive the person or agent that started it. When someone picks up
the work, they need to know which source was built, where the executable is,
which checks passed, and what remains. Sykli saves those facts together so the
next worker can inspect them and continue.

A **production** is one target bound to one captured source state and its build
instructions. Commands and workers can come and go; that saved work keeps its
identity. Change the source or instructions and you get a different production.

## How it fits together

Here is the executable example below. The first worker stops after building;
the next worker runs the remaining checks against the saved source and binary.

```mermaid
flowchart LR
    build["Worker 1<br/>Build the executable"]
    saved[("Saved locally<br/>Source, executable, progress")]
    resume["Worker 2<br/>Run the remaining checks"]
    result["Ready to use<br/>Executable + passing checks"]

    build -->|stops after building| saved
    saved -->|resume with the same ID| resume
    resume --> result
```

Sykli records each attempt and its result in `.sykli/production`. `status`
reconstructs progress from those records; `resume` runs work still needed.
The next worker needs the production ID and access to that store. It does not
need a handover explanation or a running Sykli server.

## Install

The artifact and resume commands below require a build from this repository;
the v0.2.0 release does not include them. With Rust and Cargo installed:

```sh
git clone https://github.com/false-systems/sykli.git
cd sykli
cargo install --path . --locked --bin sykli --force
```

Make sure Cargo's bin directory (normally `~/.cargo/bin`) is on your `PATH`.
`--force` replaces an existing Cargo-installed Sykli.

## Build something

Start in a Cargo workspace with a binary or a Go module with a main package.
Generated commands run offline. Prepare dependencies first with `cargo fetch`
(Rust) or `go mod download` (Go), if needed.

```sh
sykli init --production --smoke '"$SYKLI_INPUT_executable" --help'
```

This writes **`sykli.production.json`**: a file describing what to build and how
to check it. Review its source-file list and commands before running them.
The target and product use your binary's name: `sykli`, `tiny-cli`, or the name
reported by `init`. The commands below use `sykli`; substitute your target.
Existing contracts named `app` keep working; `init` does not migrate saved productions.

The `--smoke` command is a quick check of the built executable. The example
checks that `--help` exits successfully. Replace it with a command appropriate
to your program. Sykli sets `$SYKLI_INPUT_executable` to the exact binary it built;
keep the outer single quotes so your current shell does not expand it.

Now look at the plan and build:

```sh
sykli targets                                  # what can this repo produce?
sykli plan sykli.production.json --target sykli # what will run?
sykli produce sykli                            # build and check it
```

Sykli captures the selected source files, including local edits, then runs:

| Operation | What it does |
| --- | --- |
| `build` | Builds an executable from the captured source and saves its bytes. |
| `unit_tests` | Runs Cargo binary/library unit tests or Go module tests against that source. |
| `smoke_test` | Runs your smoke command against the saved executable. |

Success means the artifact is available and both checks passed. Integration
tests and doctests for Cargo are not included automatically.

By default, Sykli prints a summary like this (IDs and paths abbreviated):

```text
sykli: complete, artifact available
  build: satisfied
  smoke_test: satisfied
  unit_tests: satisfied

Artifact sykli: available
  SHA-256: 2196f82c...
  Path: /your/repo/.sykli/production/blobs/2196f82c...

Production: 7937abae...
Input source: 80712fc0...
```

The actual output contains the full path and IDs, ready to copy. On failure,
the summary shows the failed operation, its error and a diagnostics command.
Command output stays recorded until you explicitly request it.
An available artifact can still have unfinished or failed checks; the first line
reports whether production is complete.

For agents and scripts, add `--json` to get the complete structured response,
including execution records and captured output. It includes these fields:

| Field | What you use it for |
| --- | --- |
| `production` | The ID to use when inspecting or resuming this work. |
| `delivery.sykli.availability.locations[0]` | The path to the executable. Run it directly or copy it where you need it. |
| `delivery.sykli.artifact.content` | The SHA-256 identity of the executable's bytes. |
| `assessment.satisfied_checks` | The recorded attempts that passed the required checks. |

For multiple Cargo binaries, add `--package NAME --bin NAME` to `init`.
For multiple Go executables, select `--package ./cmd/NAME`. Go discovery builds
a native executable with CGO disabled and runs `go test ./...`; a Go SDK is
not required. A standalone Rust `main.rs` can use `sykli init --production` without
Cargo. Other build tools can be used through an explicit contract; see the
[production guide](docs/production.md).

## Stop now, finish later

For your first build, use this instead of the `produce` command above to stop
after compilation:

```sh
sykli produce sykli --stop-after build
```

This intentionally exits **1**: the executable is saved, but checks remain.
Copy the `Production:` ID from the summary. In a fresh terminal, in the same
repository, replace `PRODUCTION_ID` below with that ID:

```sh
sykli status PRODUCTION_ID  # see completed and unfinished work
sykli resume PRODUCTION_ID  # run the remaining work
```

Sykli reuses the saved executable and runs the remaining checks. It needs the ID
and the local store at `.sykli/production`, not the previous worker's chat.
Keep that directory. From another directory, pass `--store /path/to/.sykli/production`.
An already-complete production stays complete; `--stop-after` does not undo it.

Editing source and running `produce` creates a new production. `resume` always
uses the original captured source, even if your working files have changed.
Old passing checks cannot complete work for different source or executable bytes.

## A small loop for agents

Prepare the exact source without starting a build:

```sh
sykli produce sykli --prepare --summary --json
```

Keep the returned production ID. Compact state includes `ready`, `work`, input
identities and artifact delivery, with no embedded execution logs:

```sh
sykli status PRODUCTION_ID --summary --json
sykli resume PRODUCTION_ID --operation build --summary --json
sykli status PRODUCTION_ID --summary --json
```

`--operation` runs only the named operation when its inputs are available. It
never builds dependencies implicitly or retries a failed attempt. It still
exits 1 while the overall production is incomplete; inspect the operation state
for its result. Repeating a satisfied operation does not run it again.

To finish all remaining work with up to two independent operations at once:

```sh
sykli resume PRODUCTION_ID --jobs 2 --summary --json
```

A busy production or unresolved attempt leaves `ready` empty and explains why
in `execution_blockers`. Readiness is a snapshot; execution checks it again.
Retrieve recorded output only when needed, using the attempt ID from `work`:

```sh
sykli diagnostics PRODUCTION_ID ATTEMPT_ID --json
```

Humans can omit `--json`. Existing full JSON responses remain available by
omitting `--summary`. No server or agent SDK is required.

## When something fails

- **A command failed:** inspect `status`. To retry a failed build explicitly,
  run `sykli resume PRODUCTION_ID --retry build`. Earlier attempts stay recorded.
- **You changed the code to fix it:** run `sykli produce sykli` for the new source.
- **Execution is indeterminate:** Sykli cannot establish whether everything stopped.
  It refuses a retry that could overlap surviving work. Continuation is between
  operations; it does not resume a compiler halfway through an instruction.
- **The artifact is missing or no longer executable:** historical checks stay
  recorded, but delivery does not report success.

`produce` and `resume` exit 0 only for successful delivery, 1 for unfinished or
unsuccessful work, and 2 for an error. `status` can exit 0 while showing unfinished
work: it successfully inspected the records.

`sykli verify-production PRODUCTION_ID` checks record integrity, bindings,
completion, and current artifact availability. The local executor and store are
trusted. This is not a security sandbox or a proof that the software is correct.
There is no automatic publication, remote execution, or reuse across productions.

## Try the full example

From this checkout, with Python 3 and the installed Sykli on your `PATH`:

```sh
python3 examples/production/demo.py --binary "$(command -v sykli)" --output /tmp/sykli-demo.json
```

It builds a tiny program that prints `42`, exits after compilation, finishes
from a fresh invocation, then changes the source and shows that the old checks
do not make the new version pass. It prints the artifact location and saves
all commands and results in `/tmp/sykli-demo.json`.
The demonstration uses the agent loop: prepare, select the build, inspect,
resume the remaining checks, and retrieve their diagnostics. Add `--language go`
to run the same workflow with the dependency-free Go example.

## Existing graph workflow

The original task-graph commands still work for Cargo, npm, and Go repositories:

```sh
sykli init                              # write and lock sykli.json
sykli plan --changed src/lib.rs --json  # inspect tasks affected by a change
sykli run --json                        # run the graph and record a receipt
```

This path uses `sykli.json` and its existing cache and receipts. The artifact
workflow above uses `sykli.production.json`. Existing graphs do not automatically
gain artifact validation or continuation.

See [installation options](docs/install.md), the [GitHub Action](docs/github-actions.md),
and the [graph/receipt specification](docs/spec.md) for the existing interfaces.

## Further reading

- [Production contract, storage, and execution limits](docs/production.md)
- [Agent interface](docs/agents.md)
- [Design decisions](docs/adr/) and [local production scope](docs/adr/0009-local-production.md)
- [Contributing](CONTRIBUTING.md), [changelog](CHANGELOG.md), [security](SECURITY.md)

MIT.
