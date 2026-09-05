# Machine contracts

The versioned JSON surfaces of `sykli`, written from the code. Anything a
tool, an agent, or a reviewer parses is here; anything not here is text for
humans and may change without notice. Each schema string is the contract:
a reader checks it first and refuses what it does not know.

Stability before v1: **stable** means a field's name, type, and meaning will
not change under this schema string; a change gets a new string. **May
change** means the field exists but its shape or rules are still moving.

## Canonical contract hash

`contract_hash` is the SHA-256, lowercase hex, of the contract as sykli
parsed it: the JSON document read into a `serde_json::Value` and written back
compact, with object keys sorted at every level (`load_unlocked`). Whitespace,
key order, and the emitter that produced the JSON do not change it; a
different value for any field does. The same rule yields the same hash on any
machine, which is what lets a lock file, a plan, a receipt, and a reviewer
agree on which graph they are talking about. Stable.

## sykli-contract.v1

The declared work graph. Read from a `.json` path directly; any other path is
treated as a Rust emitter and run as `cargo run --features sykli --bin sykli
-- --emit` in its directory, and the emitted JSON is what gets hashed.

```json
{
  "schema": "sykli-contract.v1",
  "tasks": [
    {
      "name": "test",
      "run": "cargo test --workspace --locked",
      "after": ["fmt"],
      "inputs": ["Cargo.toml", "src/main.rs"],
      "outputs": ["target/report.txt"],
      "workdir": "crates/core",
      "env": {"RUST_LOG": "info"},
      "runtime": "shell"
    }
  ]
}
```

| field | required | rule |
|---|---|---|
| `schema` | yes | exactly `sykli-contract.v1` |
| `tasks[].name` | yes | non-blank; unique within the contract |
| `tasks[].run` | yes | non-blank; executed as `sh -c <run>` |
| `tasks[].after` | no | names of tasks in this contract; the graph must be acyclic |
| `tasks[].inputs` | no | file paths relative to `workdir` (or the contract's working directory); files, not directories or globs |
| `tasks[].outputs` | no | file paths the task must leave behind; a missing output fails the task |
| `tasks[].workdir` | no | directory the command runs in and that `inputs`/`outputs` resolve from |
| `tasks[].env` | no | extra environment; the task otherwise sees only `PATH`, `HOME`, `TMPDIR` |
| `tasks[].runtime` | no | only `shell` is accepted |

Unknown fields are rejected. Validation errors are strings; the first one
wins. Stable: `schema`, `name`, `run`, `after`, `inputs`, `outputs`,
`workdir`, `env`. May change: `runtime` (a second runtime would add a value,
not change the field), the "files only" rule for inputs and outputs (tree
hashing is a stated future change).

Execution order: tasks are grouped into levels by `after`; a level runs in
parallel; a task whose dependency did not end `passed` or `cached` is
`blocked` without running.

## sykli-lock.v1

`sykli.lock` beside the contract pins its hash. When present, every command
that loads the contract refuses a contract whose hash differs, with
"run `sykli lock` to accept it". Stable.

```json
{ "schema": "sykli-lock.v1", "contract_hash": "<64 hex>" }
```

## sykli-plan.v1

`sykli plan <contract> [--changed <path>]... --json`. Read-only; never an
evidence artifact.

```json
{ "schema": "sykli-plan.v1", "contract_hash": "<64 hex>", "tasks": ["fmt", "clippy", "test"] }
```

`tasks` is in execution order. A task is affected when one of its declared
inputs is a changed path (compared as normalized absolute paths, so relative
and absolute spellings agree) or when a task it is `after` is affected. With
no `--changed`, every task is listed. Changed paths need not exist; inputs
need not exist for planning, only for running. Stable.

## sykli-validate.v1

`sykli validate <contract> --json`. Exit 0 when valid, 1 when not; the JSON
is printed either way.

```json
{ "schema": "sykli-validate.v1", "contract": "sykli.json", "valid": true, "contract_hash": "<64 hex>", "errors": [] }
{ "schema": "sykli-validate.v1", "contract": "bad.json", "valid": false, "contract_hash": null, "errors": ["task \"a\" depends on unknown task \"missing\""] }
```

`contract` is the path as given. `errors` holds at most one message today.
Stable: `schema`, `valid`, `contract_hash`, `errors`. May change: `contract`
(may become the resolved path).

## sykli-receipt.v1

`sykli run <contract> --json` prints the receipt it also wrote to
`.sykli/receipts/rcpt_<sha256-of-file>.json` in the repository root. It is
the only evidence artifact. With `--json`, task output and progress go to
stderr so stdout is one JSON document; without it, they go to stdout and the
receipt path is printed last.

```json
{
  "schema": "sykli-receipt.v1",
  "contract_hash": "<64 hex>",
  "subject": {
    "repository": "/abs/path/to/repo",
    "tree_oid": "<git tree OID of the working tree that ran>",
    "inputs_digest": "<64 hex>",
    "head_tree_oid": "<git tree OID of HEAD>",
    "dirty": false
  },
  "tasks": [ ...task records in contract order... ],
  "outcome": "passed"
}
```

### subject

| field | meaning |
|---|---|
| `repository` | `git rev-parse --show-toplevel` of the working directory |
| `tree_oid` | git's own address of the content that ran: every non-ignored file staged into an ephemeral index, `.sykli` removed, `git write-tree`. Same bytes, same OID, any machine |
| `head_tree_oid` | `HEAD^{tree}`, for reference |
| `dirty` | `tree_oid != head_tree_oid`, derived, never self-reported |
| `inputs_digest` | SHA-256 over `{task name: {input path: sha256 of file or null}}` for every declared input, so ignored and out-of-tree inputs bind too. A missing input hashes as `null`, it does not error here |

All stable.

### task records

One per contract task, in contract order.

| field | type | meaning |
|---|---|---|
| `name`, `command` | string | the task and its `run` string |
| `runtime_fingerprint` | string | `shell:<abs sh path>:sha256:<sh binary>:env:sha256:<digest of PATH, HOME, TMPDIR>` |
| `exit_code` | int or null | the process exit code; null when it did not run or was signalled |
| `duration_ms` | int | wall time; for `cached`, the restore time |
| `stdout`, `stderr` | string | captured output as lossy UTF-8, up to 1 MiB per stream |
| `stdout_digest`, `stderr_digest` | string | SHA-256 of the raw bytes of the full stream, even when truncated |
| `stdout_truncated`, `stderr_truncated` | bool | more than 1 MiB arrived |
| `stdout_bytes_dropped`, `stderr_bytes_dropped` | int | how much was not kept |
| `output_digests` | object | declared output path → SHA-256 of the file |
| `outcome` | string | see below |
| `importable` | bool | the record is complete: no dropped bytes, no capture or runtime error. A non-importable record never seeds the cache and fails `verify` |
| `class` | string or null | `command_failed`, `missing_output`, `runtime_error`, `capture_error`, `input_error`, `dependency_failed` |
| `retryable` | bool | true for `runtime_error` and `capture_error`: the task may pass if run again |
| `source` | string | `task` (it ran) or `cache` (restored) |
| `error` | string or null | the exit status or error text |
| `provenance` | string or null | for `cache`: the receipt file name (`rcpt_….json`) whose run produced the artifact |

### outcomes

| task outcome | when |
|---|---|
| `passed` | exit 0, every declared output present, output captured completely |
| `failed` | non-zero exit (`command_failed`), or a declared output missing (`missing_output`) |
| `errored` | the runtime could not run or observe it: spawn or wait error, capture failure, or a declared input missing at run time (`input_error`) |
| `cached` | not run; restored from `.sykli/cache/<key>` with provenance to a passed, importable record whose command, fingerprint, and output digests match |
| `blocked` | not run because an `after` dependency did not end `passed` or `cached` |

Run outcome: `errored` if any task errored; else `cached` if every task was
cached; else `passed` if every task is `passed` or `cached`; else `failed`.
`run` exits 0 for `passed` or `cached`, 1 otherwise.

The cache key is the SHA-256 of the task definition, the digests of its
declared inputs, and the runtime fingerprint; a cached record is reuse of an
earlier receipt, not a new observation.

Stable: every field above. May change: the `class` vocabulary may grow; the
1 MiB capture limit. Note that ADR-0002 lists a `skipped` outcome that the
code never emits; only the five above exist.

## sykli verify

`sykli verify <receipt> --contract <contract>` prints one line per check and
exits with the first failing stage:

| exit | stage | meaning |
|---|---|---|
| 0 | all | verified: receipt matches this tree and contract |
| 1 | outcome | receipt outcome is not `passed`/`cached`, or a task record is missing, out of order, non-importable, or not successful |
| 2 | schema / usage | not a receipt, unreadable input, git or contract error |
| 3 | tree, inputs | `subject.tree_oid` or `inputs_digest` differ from the tree now: stale, re-run |
| 4 | contract, lock | the contract hash differs from the receipt or from `sykli.lock`: drifted, re-lock |

The table is ordered by exit code; the checks do not run in that order. They
run schema (2), then contract lock and contract (4), then tree and inputs (3),
then outcome (1), and stop at the first failing stage. So a receipt that is
both drifted and stale reports 4, and a stale receipt's outcome is never
judged. Output is text (`ok: …` / `mismatch: …` / `verified: …`); there is no
JSON form yet, which is why the exit code is the machine contract. Stable.

## Exit codes, all commands

| command | 0 | 1 | 2 | 3 | 4 |
|---|---|---|---|---|---|
| `validate` | valid | invalid | | | |
| `plan` | ok | invalid contract | | | |
| `run` | passed or cached | failed, errored, or could not evaluate | | | |
| `lock` | written | error | | | |
| `verify` | verified | outcome failed | cannot verify | stale | drifted |
| emitter binary without `--emit` | | | misuse | | |
| `install.sh` with a bad target | | | misuse | | |
