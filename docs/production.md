# Local typed production

Give Sykli a source snapshot and a target. It produces an identified artifact,
or reports the work still needed. Another invocation can finish the same request
using its stored facts and artifacts. No agent session or external service is
required.

This is an **opt-in local path**, alongside the existing graph commands. It is
not available in previously published v0.2.0 binaries; build this checkout with
`cargo build --release --locked` first. Linux and macOS are supported hosts.

## Try it

In a Cargo workspace with a binary:

```sh
sykli init --production --smoke '"$SYKLI_INPUT_executable" --help'
sykli targets --json
sykli plan sykli.production.json --target app --json
sykli produce app --stop-after build --json  # exits 1: checks remain
```

The smoke command is yours: use an observable check appropriate to your program.
For Sykli itself, `--smoke '"$SYKLI_INPUT_executable" produce --help'` exercises
the collected CLI. A generated contract selects the current native target;
regenerate it on another host rather than pretending its executable type is portable.

A standalone Rust `main.rs` also works, using:

```sh
sykli init --production
sykli targets --json
sykli plan sykli.production.json --target app --json
sykli produce app --stop-after build --json  # exits 1: checks remain
```

Keep the returned `production` identifier. Start a new shell or worker:

```sh
sykli status PRODUCTION_ID --json
sykli resume PRODUCTION_ID --json
sykli verify-production PRODUCTION_ID --json
```

The completed view returns `delivery.app.artifact` (identity and type),
`delivery.app.availability.locations` and `assessment.satisfied_checks`.
The executable location is directly runnable. Each check's `finished` record
identifies its subject: the source snapshot for unit tests, the collected
executable for the smoke test.

Cargo discovery uses `cargo metadata --offline --no-deps` for workspace members
and binary targets, and `cargo package --list --allow-dirty --offline` for their
source file selections. It selects the only binary in the workspace's default
members; `--package NAME --bin NAME` resolves ambiguity. Library-only workspaces,
feature-gated binaries, external/excluded path dependencies and initialization
outside the workspace root require an explicit contract. A missing lockfile is
generated offline; uncached dependencies require preparing Cargo's cache first.

Generated build and unit-test recipes use `--offline --locked`, the native
rustc host target, and fresh output directories. Unit checks cover the selected
package's binary/library unit tests; integration tests and doctests are not
silently added as requirements. Workspace files, build scripts and packaged
assets are captured. Other hidden/generated paths remain excluded, except the
workspace's `.cargo/config` or `.cargo/config.toml`; Cargo credentials are never
selected automatically. Review the generated file list and configuration.
Re-run init when new files need to enter the declared source selection.

Cargo requires an explicit `--smoke` command. Standalone `main.rs` keeps its
default check that the executable exits successfully without arguments, which
can also be overridden with `--smoke`. Other build tools work through explicit
shell recipes. Legacy `init` continues to detect Cargo/npm/Go graphs.

## Go modules

Try the dependency-free [Go example](../examples/go), or use the same interface
in a module with one main package:

```sh
sykli init --production --smoke '"$SYKLI_INPUT_executable" --help'
sykli targets
sykli plan sykli.production.json --target app
sykli produce app --stop-after build  # exits 1; copy the production ID
sykli resume PRODUCTION_ID
```

Choose a smoke command your program supports. With several executables, select
`--package ./cmd/NAME` (or the package import path). No Go SDK is needed.
Discovery asks `go list` for the module's packages, active source and test files,
and embedded assets. It also includes each package's `testdata` directory and
`go.mod`/`go.sum`. Review the paths and re-run init when new inputs are added.
Other runtime files used by tests must be added explicitly. Symlinks, excluded
hidden paths and nested repositories are rejected rather than silently captured.

Generated recipes build the selected package and run `go test -count=1 ./...`
against the captured module. Both use the native OS/architecture, CGO disabled,
`-mod=readonly`, no VCS stamping, and a fresh attempt-local Go build cache.
Workspace mode, persisted Go settings and automatic toolchain downloads are
disabled (`GOWORK=off`, `GOENV=off`, `GOTOOLCHAIN=local`). Discovery uses those
same settings. This deliberately selects a pure-Go build; CGO, custom build
flags, replacements and vendoring need an explicit contract. Cached external
module dependencies are supported; prepare them with `go mod download` first.
Go dependency network access is disabled (`GOPROXY=off`, `GOSUMDB=off`), but this
is not a sandbox: test commands may still access host services or the network.
The host toolchain and module cache remain trusted external inputs, not captured
source. Cross-production reuse stays disabled.

The reproducible demonstration uses the tiny repository in
[`examples/production`](../examples/production). From the Sykli checkout:

```sh
cargo build --locked
# Use the binary in cargo metadata's target_directory if CARGO_TARGET_DIR is set.
python3 examples/production/demo.py --binary target/debug/sykli --output /tmp/sykli-demo.json
```

It saves the actual commands, JSON responses, executable output, identities and
check bindings. It leaves the artifacts and store in the printed temporary
directory. A first client stops after compilation; a separate client finishes
the checks. Candidate B compiles but fails its checks while A remains complete.
Native runs captured for this change are in
[`demos/production.json`](demos/production.json) (macOS arm64) and
[`demos/production-linux.json`](demos/production-linux.json) (Linux arm64,
Lima `linux1`). The [Linux gate log](demos/linux-validation.txt) records passing
formatting, Clippy, all workspace tests and the installer checks through
`cargo xtask gate`; all 11 production tests passed. Machine-specific paths there
are historical locations, not permanent downloads. Re-run the script to obtain
your own artifact. Tests separately kill a client and an executor during a
FIFO-latched compilation; no timing assumption stages the interruption.

Sykli also builds itself with the generated Cargo target. The
[self-production transcript](demos/cargo-self-production.json) records compilation,
client exit, a fresh status/resume, source-bound unit tests and a smoke check of
the collected `sykli produce --help` command. This is the ordinary Cargo path;
no Sykli-specific build adapter is involved. Its
[Linux Cargo-discovery gate](demos/cargo-linux-validation.txt) covers the same
generation and execution behavior on Linux arm64.

## Authoring contract: `sykli-production-contract.v1`

The canonical implementation types are in
[`src/production/contract.rs`](../src/production/contract.rs). As with the legacy
surface, this document specifies the public versioned JSON schema. Unknown
fields, duplicate object keys at any depth, floats, malformed references and
unsupported modes are errors.

The top level is `{ "schema": "sykli-production-contract.v1", "targets": {...} }`.
Each target has:

| Field | Meaning |
| --- | --- |
| `inputs` | Port → `{type, paths}`. Initially source trees selected by explicit regular-file paths. |
| `profile` | `{kind: "local-shell.v1", tools: ["rustc"]}`. Required tool names; no credentials. |
| `operations` | Name → operation described below. |
| `products` | Product port → explicit binding; at least one required. |
| `required_checks` | Unique names resolving to check operations. |

An operation has `kind` (`transform` or `check`), `inputs`, `run`, and
`reuse: "never"`. Its `inputs` map port names to `{expects: TYPE, from: BINDING}`.
Transforms require an `outputs` map: port → `{type: TYPE, collect: RELATIVE_PATH,
validator: "builtin.v1"}`. Checks require `subject_input` naming one of their
bound inputs, and a nonempty `assertion`. Check operations have no outputs;
transform operations have no check fields. The assertion labels what the
declared command tests; it does not invoke a built-in theorem or test oracle.

Bindings have precisely one of these shapes:

```json
{"kind":"target-input","port":"source"}
{"kind":"operation-output","operation":"build","port":"executable"}
```

Supported types:

```json
{"kind":"source-tree"}
{"kind":"directory"}
{"kind":"file","media_type":"application/octet-stream"}
{"kind":"executable","format":"elf","architecture":"x86_64"}
```

Executables support `elf` or thin `macho`, with `x86_64` or `aarch64` architecture.
`builtin.v1` checks a little-endian 64-bit ELF executable/shared-object header or
Mach-O executable header, the machine field and executable mode. This does not
establish OS version, ABI compatibility, linking success or loadability. A smoke
check establishes that the collected bytes actually ran under its recorded
local context. PE, fat Mach-O, other architectures/media validators, remote or
container profiles, freshness policies and `exact-invocation` reuse are rejected.

Port and operation names use ASCII letters, digits and underscores. Paths are
relative, nonempty, slash-separated and normalized: no empty, `.` or `..`
component, absolute path or backslash. Every declared operation is validated,
including unreachable ones. Only products, required checks and their transitive
dependencies are selected. The plan keeps selection reasons. There are no
order-only edges in this schema; legacy `after` remains unchanged.

## Identity and execution binding

Identities are lowercase 64-digit SHA-256 hex strings. Artifacts address raw
file bytes or a canonical `sykli-tree.v1` manifest. Semantic identities hash
`UTF8(domain) + NUL + compact_sorted_JSON(payload)`. Domains are
`sykli-production-contract.v1`, `sykli-production-request.v1`, `sykli-profile.v1`,
`sykli-recipe.v1`, `sykli-context.v1`, `sykli-attempt.v1` and
`sykli-production-record.v1`. Object keys sort recursively; array order is
retained; UTF-8 strings use serde_json's compact encoding. Unknown fields and
duplicate keys are rejected before canonicalization. Optional operation
`outputs`, `subject_input`, `assertion` normalize to `{}`, `null`, `null` whether
omitted or explicitly supplied. No identity includes itself in its hash input.

The contract embeds recipes, profile and versioned collection rules; authoring
requires no hand-written hashes. Recipe identity covers the entire normalized
operation, including bindings and output validators. The request embeds the
contract, its identity, selected target, exact input references and resolved
context. A changed source, contract, tool image/version or recorded environment
produces a different request identity. Attempt IDs additionally include the
production, next sequence, timestamp and process ID; retry never reuses an
attempt ID. IDs are checked in their owning field/domain and references are
validated during reconstruction, rather than introducing a Rust wrapper for
every ID noun.

Source paths are relative to the authoring contract's directory. Every declared
file's current bytes and executable bit are captured, including dirty and
untracked files. HEAD is not used as a substitute. Inputs are explicit files,
not globs or a recursive capture of the entire repository. Hidden path
components (except explicit root `.cargo/config` and `.cargo/config.toml`), common
generated directories (`target`, `node_modules`, `vendor`),
symlinks, special files and paths entering nested repositories/submodules are
rejected. Other files, credentials and undeclared untracked files are not
automatically collected. Explicitly select the integrated candidate when
testing a merge; individual branch results do not establish the combined result.

Tree manifests are `{schema: "sykli-tree.v1", entries: {PATH: {content, executable}}}`.
`content` is a blob digest for a regular file and `null` for a directory.
Directory outputs preserve empty directories. Snapshot parent directories are
implicit. File modes normalize to executable/non-executable (755/644 when
materialized); ownership, timestamps and other mode bits are not identified.
Source capture reads selected files sequentially. It identifies exactly the
captured bytes, not an atomic filesystem-wide point in time.

Each attempt receives fresh copies at `SYKLI_INPUT_PORT`, an empty
`SYKLI_OUTPUT` directory, and a separate empty working directory. Commands run
through the existing `sh -c` executor, with its PATH/HOME/TMPDIR environment
allowlist. A recipe can `cd "$SYKLI_INPUT_source"` and invoke an existing build
tool, directing results to `SYKLI_OUTPUT`. Collection never searches prior
attempts or the original workspace. Downstream inputs are rematerialized from
identified blobs, never shared mutable output paths. Input trees remain writable
for build-tool compatibility; recipe changes to their private copies do not
change the captured source or another operation's inputs.

The context records the requested profile digest, host OS/architecture, existing
shell/environment fingerprint, required tool-image digests and, for known
rustc/cargo/cc/clang/gcc probes, digests of `--version` output. Missing resolutions
are `null`, not invented versions. Tool images/version strings do not capture
the entire SDK, dynamic libraries, services or host. `undeclared_inputs_excluded`
is always false. `input_binding: "materialized-snapshot"` is recorded by the
executor after preparation. User JSON cannot enable stronger guarantees.

## Durable records, recovery and retries

The default store is `.sykli/production` relative to the invocation directory;
`--store PATH` selects the same store from another directory. It contains:

```text
blobs/DIGEST                         immutable file or tree-manifest bytes
requests/PRODUCTION/request.json      pinned request
requests/PRODUCTION/records/SEQ.json   immutable record envelopes
requests/PRODUCTION/attempts/ATTEMPT/  private prepared execution locations
requests/PRODUCTION/logs/ATTEMPT.log   supplementary diagnostic stream
requests/PRODUCTION/lease             controlling-writer OS lock, never a PID lock
requests/PRODUCTION/attempt-leases/ATTEMPT  per-executor liveness OS lock
requests/PRODUCTION/journal-lock      short shared-reader/exclusive-writer lock
```

Canonical files publish by fsynced temporary write and atomic hard-link creation,
then directory fsync. Existing different bytes at an immutable name are rejected.
`.tmp-*` files are uncommitted and ignored; missing/corrupt committed sequence
entries cannot imply completion. No automatic garbage collection is implemented.

An envelope is `{id, record}`; the record has `schema:
"sykli-production-record.v1"`, `production`, `sequence` (starting at 1),
`previous` (preceding record ID or null), `recorded_at` (Unix milliseconds),
and `fact`. Its ID hashes the record payload under the record domain above.

| Fact kind | Fields and acceptance |
| --- | --- |
| `started` | `attempt`, `operation`, resolved `inputs`, `recipe`, `context`, `supersedes` (previous selected attempt or null), `executor`. Persisted before command execution. |
| `finished` | `attempt`, `result`, `observation`. Requires the start and matching operation, ports, check subject and assertion; success requires successful bound execution observations. |
| `contact-lost` | `attempt`, `reason`. A reconciliation observation about an unresolved start, never a terminal success or interruption. |

Result kinds are `produced` with `outputs`, `checked` with `subject`, `assertion`
and `outcome` (`passed`, `failed`, `unknown`), `execution-failed` with `code`, or
`interrupted`. `observation.execution` reuses the existing task receipt's actual
command, exit status, runtime fingerprint, bounded captures and digests. Capture
incompleteness cannot pass. Collection errors retain observations and a
`collection_error`; preparation/spawn errors retain their diagnostic. These
codes identify observations, not inferred root causes.

One controller holds an OS file-description lease per production. Bounded
executor subprocesses inherit it; each executes one persisted attempt and
finishes its record even if the initiating client dies. No service or daemon
is started. A second controller refuses while that lease is held. `status`
can inspect the atomic record prefix during execution. Each executor separately
holds a per-attempt liveness lock, which is not inherited by recipe processes.
An unfinished attempt is running only while its own lock is held and no contact
loss was recorded. A missing or released liveness lock means indeterminate,
even while a different executor holds the production-wide lease. Older unfinished
attempts without per-attempt locks are conservatively indeterminate. A successful subsequent `resume` reconstructs that terminal record.

`--jobs N` permits up to N independent operations in a wave (default 1).
Starts and terminal records use a separate short-lived OS journal lock: each
writer reloads the validated prefix under that lock before appending. Inspection
uses a shared read-only lock to obtain a coherent prefix; writers lock exclusively.
Read-only queries never create locks. Older stores without a journal lock are
read as validated atomic prefixes. Missing lease files mean no observed lease;
permission errors are reported rather than interpreted as liveness. Lease probes
release their locks immediately, before reading history or validating artifacts;
their results are observations, not reservations. Executors retain the controlling
lease after client loss and commit their own results. No new work is started
after an unresolved executor result. `--retry` and `--stop-after` require one job;
parallel histories require this version of Sykli to read them.

If the executor itself dies, descendants may still exist. Releasing its lease
does not prove termination of those descendants: resume records contact loss,
remains incomplete and refuses retry. There is deliberately no “assume stopped”
switch. A signalled recipe shell also remains indeterminate: observing its termination
does not establish termination of its foreground children. This executor does
not emit terminal `interrupted` results without that stronger guarantee. Older
local `interrupted` records are also projected as indeterminate and cannot
authorize a retry.
Foreground local recipes are the supported execution model; detached jobs and
irreversible publishing recipes have no retry guarantee.

`resume ID --retry OP` explicitly selects a new attempt after a terminal result,
including an explicit rebuild of a previously successful transform. A changed
output invalidates checks/dependents with old input bindings. Ordinary resume
does not automatically retry failed work. Conflicting terminal reports are
rejected with a diagnostic identifying the attempt as indeterminate; no
successful assessment is returned from a conflicting store. Re-publishing the
identical envelope at the same sequence is idempotent. Distinct envelopes
claiming another terminal outcome for the same attempt are not accepted.

There is no cross-production import, shared-result lookup or remote executor.
Legacy graph caching is preserved. Same-production continuation uses accepted
facts with exact subject bindings and matching recorded context. An unknown
host dependency is not made immutable by this implementation; explicit retry
is the way to request a new observation in that same trusted-local context.

## Views, verification and exits

`sykli-targets.v1` returns `contract` and normalized `targets` without executing
recipes. `sykli-production-plan.v1` returns contract/target, selected operations
with reasons, requested inputs/products, `resolved_inputs`, resolved `context`,
prerequisite `blockers` and the prospective `production` (null for missing
source). Planning is read-only; it may run the known version probes above.

`sykli-production-view.v1` returns production/contract/target, inputs, context,
`through_sequence`, `evaluator_version: "local.v1"`, `policy: "all-selected.v1"`,
`evaluated_at` (last record timestamp or null), explicit
`evaluation_inputs.executor_lease_held`, `evaluation_inputs.attempt_lease_held`
(an attempt-ID map for unresolved attempts), `work`, `assessment`, `delivery`,
`delivery_success`, full record envelopes and the trust statement. Each work
entry contains `state` and `required_because`. Work states are ready, running,
satisfied, blocked with structured reasons, failed or indeterminate.

The historical assessment deterministically uses the pinned request, record
prefix and named policy/version. Live lease observation affects unresolved work
state and is exposed as an additional evaluation input. Current blob existence, digest checks and executable permissions affect delivery
separately. No time-based freshness policy is
supported. A historically complete result with missing product bytes stays
complete but delivery is unavailable and cannot exit successfully from produce,
resume or verify-production. For file products, locations point to the blob;
for directory/source products, they point to the tree manifest whose entries
address sibling blobs. Copy the whole store for offline continuation elsewhere.

`verify-production` validates request/record schemas, digests, chains, reference
bindings, applicable checks, completeness and present product integrity. It
does not compare a production to today's working tree: its source is pinned.
It requires the local store, including the request and records. A view JSON by
itself is an inspection receipt, not an independently trusted store import.
Verification does **not** establish issuer authenticity, hermeticity or execution
truth. The local executor and store are trusted; a user who can replace them
can fabricate a consistent history. No API accepts a worker's completion flag
or an externally submitted output/terminal record as authoritative input.

All new `--json` responses are one JSON document. Logs remain in attempt logs
and execution observations; they do not contaminate JSON stdout. Without `--json`,
targets and typed plans show concise summaries; produce,
status, resume and verify-production show work states, artifact locations and
identities. Failed or indeterminate operations point to `sykli diagnostics
PRODUCTION_ID ATTEMPT_ID`; captured command output is never printed implicitly
in human summaries. Full records and captures remain available with `--json`. Typed init retains its existing output. These presentations use the
same evaluated data and exit codes. Errors use `sykli-production-error.v1` with
`error`. The exit-code table for both paths is in [`spec.md`](spec.md#exit-codes-all-commands).

## Agent work loop

`produce TARGET --prepare` publishes the captured request without starting an
attempt (exit 0 on successful preparation, even if work is blocked).
`resume ID --operation NAME` considers only that selected operation. Missing
inputs stay blocked, satisfied work is left alone, and failed work requires
`--retry NAME`. With both options they must name the same operation. Operation
selection and `--stop-after` are mutually exclusive. Production commands still
exit 1 until the entire production is complete and deliverable.

`--summary` on produce/status/resume returns `sykli-production-state.v1` with
the same evaluated fields except `records`, plus:

- `ready`: operation names ready for a new controlling invocation; empty while
  the controlling lease is held or any operation is indeterminate.
- `execution_blockers`: `production-busy` or `attempt-unresolved`, when applicable.
- `work.OP.diagnostics`: `{production, attempt}` for each selected attempt.
- `work.OP.failure`: the recorded error/code, when one exists.

State is an observation, not a reservation. The executor validates readiness,
context, exact inputs and the controlling lease again. No worker-authored state
or completion flag is accepted.

`diagnostics ID ATTEMPT` loads validated records for that attempt, including
historical retries. JSON uses `sykli-production-diagnostics.v1` with `production`,
`attempt`, `operation` and matching `records`. Human output prints the saved
stdout/stderr only in this explicit command. Captures retain the executor's
existing size limits and truncation metadata; an unresolved attempt may have
no terminal capture. Exit 0 means records were retrieved, not that work passed.

This extends the CLI for Yair's workflow: prepare Sykli's own source, inspect
ready work, choose a build or check, and let another invocation continue.
It does not reintroduce the agent execution, server or SDK capabilities deleted
by ADR-0005; workers remain external and recipes remain ordinary commands.

## Scope of this implementation

The five relationships are concrete: content and contract identities, typed
dependency bindings, declared shell transformations, subject-bound checks,
and durable attempt history. This is not task closure or publication authority.
There are no external False Systems dependencies, LLMs, remote workers,
automatic publishing operations, cross-production reuse or security sandbox.

Compared with handoff revision 0.3, one shared local profile lives on each
target; source input file selections accompany their types; recipes are inline;
validators use a pinned built-in version; timestamps and record references live
in envelopes. Unknown execution is represented by an unresolved start and
contact-loss observation, not a fabricated terminal `indeterminate` result.
`verify-production` is separate because legacy `verify` checks the current Git
tree and v1 receipts. No shipped graph schema or exit code was reinterpreted.
