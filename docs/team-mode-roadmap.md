# Sykli Team Mode Roadmap

## Status

Design document. Phase 0 of the work it describes. Defines the phased
implementation plan from local-only Sykli to a self-hosted coordinator
serving teams of humans, agents, and daemons.

Each phase below is one or more PRs. No phase past 0 is in this PR.

## Architecture sentence

> The daemon executes and records; the mesh dispatches inside trusted
> networks; the coordinator synchronizes team state across locations;
> `.sykli/` remains the local source of detailed evidence.

The roadmap below holds that sentence true at every step.

## Guiding rules across all phases

- **Local-first stays the floor.** Each phase must keep local-only mode
  identical in behavior to today's experience.
- **No phase introduces remote execution by default.** Remote work
  requires explicit `accepts_remote_work: true` per
  `docs/daemon-join-protocol.md`.
- **Every CLI surface gains `--json`** before it gets a network surface.
  `docs/done.md` applies.
- **Every CLI surface gains an MCP counterpart** unless explicitly
  documented as local-only.
- **No `expected_failure` shortcuts.** New black-box and conformance
  cases must pass on the slice that introduces them; bugs get fixed
  before merge or get tracking issues.
- **Defaults are minimal.** The first time a phase enables a sync, the
  default policy must be the safest of the options in
  `docs/team-mode-security.md`.

## Phase 0 — Design docs

**This PR.** No implementation. Six docs:

- `docs/coordination-modes.md`
- `docs/self-hosted-coordinator.md`
- `docs/daemon-join-protocol.md`
- `docs/team-mode-security.md`
- `docs/local-state-plane.md`
- `docs/team-mode-roadmap.md` (this file)

Exit criteria: docs land. Definition of done: `docs/done.md` aligned for
future phases; `git diff --check` passes; PR description complete.

## Phase 1 — Local work item model and CLI

Add local work item state **before** any networking. A team-less user
must benefit from work items the day they ship.

Status:

- PR #187 added `Sykli.WorkItem`, `Sykli.Work.Store`, and
  `.sykli/work/items/<id>.json` persistence.
- PR #188 adds the local `sykli work ...` CLI commands and JSON output.

Suggested modules:

- `Sykli.WorkItem` — struct + constructors + JSON shape.
- `Sykli.Work.Store` — file-backed, atomic write, list/show/claim.

Suggested local path:

```text
.sykli/work/items/<id>.json
```

Suggested CLI:

```bash
sykli work create "Investigate failing checkout deploy"
sykli work list
sykli work show <work-id>
sykli work claim <work-id>
sykli work note <work-id> "Found likely API breakage"
```

Each command must support `--json` and return the standard envelope.

Exit criteria:

- Black-box cases for the happy path and at least one negative case per
  command.
- `sykli context` recognizes the new directory.
- MCP tools `list_work_items`, `get_work_item`, `claim_work_item`,
  `create_work_item`, `append_work_note` exist (local-only).

## Phase 2 — Associate runs with work items

A run can belong to a work item. The local store gains a join.

Status:

- PR #189 adds `sykli run --work <work-id>`, stores `work_item_id` and a
  deterministic `sha256:` `contract_hash` over canonicalized emitted JSON in
  existing run manifests, and adds `sykli work runs <work-id>`.

Suggested CLI:

```bash
sykli run contract.json --work <work-id>
sykli work runs <work-id>
```

Run summary additions:

- `work_item_id`
- `contract_hash`
- `status`
- `nodes`
- `gates`
- `criteria_results`
- `evidence_refs`

Exit criteria:

- The summary block in `.sykli/runs/<run_id>.json` carries the new fields
  when a `--work` flag was supplied.
- `sykli work runs <work-id>` lists associated run summaries.
- `sykli explain` and `sykli fix` surfacing the work item id remains a
  follow-up once those renderers consume work metadata.
- Conformance cases unchanged (no SDK contract change in this phase).

## Phase 3 — Local gate decision state

Make approvals and rejections first-class locally. This is the dry run
of the coordinator's gate flow, but entirely on one machine.

Local CLI:

```bash
sykli gates list
sykli gate show <gate-id>
sykli gate approve <gate-id> --reason "Looks safe"
sykli gate reject <gate-id> --reason "API breakage not acceptable"
```

Local path:

```text
.sykli/gates/<gate-id>.json
```

Status values are `waiting`, `blocked`, `approved`, `rejected`, and `expired`.
Only `waiting` and `blocked` gates may be approved or rejected. Terminal
decisions are not silently overwritten.

Exit criteria:

- Local gate decisions persist under `.sykli/gates/<gate-id>.json`.
- `sykli gates list`, `sykli gate show`, `sykli gate approve`, and
  `sykli gate reject` support human and `--json` output.
- Invalid transitions are rejected.
- Runtime gate request persistence, run resumption, `sykli explain` gate
  rendering, and MCP gate tools remain follow-up layers.

## Phase 4 — Coordinator skeleton

First server-side phase. Stand up a minimal self-hosted service.

Status: the first skeleton slice is implemented with an in-memory store,
minimal bearer-token auth, health/org/team/work item endpoints, and JSON
envelopes. Durable Postgres storage and migrations remain follow-up work.

Suggested modules:

- `Sykli.Coordinator.Application`
- `Sykli.Coordinator.Router`
- `Sykli.Coordinator.Store`

Suggested storage: Postgres.

Initial endpoints:

```text
GET  /healthz
POST /v1/orgs
POST /v1/teams
POST /v1/daemon-sessions
POST /v1/daemon-sessions/:id/heartbeat
POST /v1/work-items
GET  /v1/work-items
GET  /v1/work-items/:id
```

Token issuance lands here too: `sykli team token create` mints a bearer
token for a (org, team) pair.

Exit criteria:

- A coordinator process starts under `sykli coordinator start`.
- `sykli coordinator migrate` runs schema migrations. Not implemented in
  the skeleton slice because storage is still in-memory.
- `sykli coordinator status` reports liveness. Not implemented in the
  skeleton slice; use `GET /health` or `GET /healthz`.
- TLS termination is documented (recommended: terminate at Ingress;
  optional in-process TLS for direct deploys).
- Audit log row written for every state-changing call.

## Phase 5 — Daemon join and heartbeat sync

Daemons connect outbound and announce themselves.

Suggested CLI:

```bash
sykli daemon join \
  --coordinator https://sykli.internal \
  --org false-systems \
  --team platform \
  --token $SYKLI_TEAM_TOKEN \
  --labels macos,docker,typescript \
  --name yair-mbp

sykli daemon status
```

Coordinator tracks:

- daemon online/offline
- labels
- capabilities
- `last_seen_at`
- `accepts_remote_work` flag

Exit criteria:

- `daemon join` succeeds against the coordinator from Phase 4.
- Heartbeats keep `daemon_sessions.status` in `online`.
- A killed daemon transitions to `offline` after the documented
  liveness cutoff.
- `sykli daemon status` reflects coordinator-side state when joined,
  local-only state when not.
- Black-box cases cover token rejection, TLS failure, and reconnect.

## Phase 6 — Work sync

The local work store gains a `--team` mode.

Suggested CLI:

```bash
sykli work create --team "Investigate failing checkout deploy"
sykli work list --team
sykli work claim <work-id> --team
sykli work assign <work-id> --to agent:claude --team
sykli work note <work-id> --team "Investigated, see local evidence"
```

Without `--team`, every command is local exactly as in Phase 1. With
`--team`, operations write to (or read from) the coordinator and the
audit log records each one.

Exit criteria:

- `sykli work list --team` returns the coordinator's view, paginated.
- `sykli work claim <id> --team` is atomic at the coordinator: only one
  successful claim per work item.
- `--json` envelope distinguishes `"source": "coordinator"` from
  `"source": "local"`.

## Phase 7 — Run summary sync

After a local or K8s run, a joined daemon publishes a summary to the
coordinator.

Sync set:

- run status
- node statuses
- error codes
- success criteria results summary
- review results summary
- gate state
- evidence refs (with `visibility` per
  `docs/local-state-plane.md`)

Exit criteria:

- A passing run produces one `runs` row plus N `run_nodes` rows on the
  coordinator.
- A failing run includes the error code and the originating evidence
  ref.
- A run with success criteria produces `success_criteria_results` rows.
- Evidence refs with `visibility: local_only` are stored as references,
  never as bytes.
- The daemon's outbox successfully replays after a coordinator outage.

## Phase 8 — Gate approval sync

The coordinator becomes the place where one person approves something
another person ran.

Flow:

1. The daemon executing a run reaches a gate. It POSTs `gate.requested`
   and pauses the run.
2. The coordinator records the gate in `waiting`.
3. A reviewer runs `sykli gate approve <gate-id> --reason "..."` against
   the coordinator (CLI or MCP).
4. The coordinator records the decision in the audit log and surfaces
   it to the daemon on the next heartbeat.
5. The daemon resumes the run (or marks it `rejected`).

Exit criteria:

- A reviewer on a different machine successfully approves a gate
  initiated by another daemon.
- The local `.sykli/gates/<gate-id>.json` is updated by the daemon when
  it picks up the decision.
- `sykli gates list --team` shows waiting gates across the team.
- The audit log reflects the full lifecycle.

## Phase 9 — Kubernetes deployment

Ship a deployable bundle.

Suggested layout:

```text
deploy/kubernetes/
  base/
    deployment.yaml
    service.yaml
    serviceaccount.yaml
    configmap.yaml
    secret.example.yaml
  overlays/
    dev/
    prod/
```

Or, equivalently:

```text
helm/sykli-coordinator/
  Chart.yaml
  values.yaml
  templates/
```

Required components:

- `Deployment: sykli-coordinator`
- `Service: sykli-coordinator`
- Optional `Ingress`
- Postgres reference (in-cluster operator or external URL via Secret)
- `Secret: sykli-coordinator-token`
- `Secret: sykli-db-url`
- `ConfigMap: sykli-coordinator-config`
- `ServiceAccount` with minimal RBAC

Exit criteria:

- A documented `kubectl apply` (or `helm install`) flow brings up a
  coordinator that survives a pod restart.
- The coordinator's TLS termination is documented for both Ingress and
  direct deploys.
- Liveness/readiness probes hit `/healthz`.

## Phase 10 — MCP team tools

Expose team coordination to agents.

Suggested tools:

- `list_work_items`
- `get_work_item`
- `claim_work_item`
- `create_work_item`
- `append_work_note`
- `list_runs`
- `get_run`
- `get_run_nodes`
- `get_run_evidence`
- `list_gates`
- `approve_gate`
- `reject_gate`
- `list_daemons`
- `get_daemon_capabilities`

Important rule:

> MCP must not expose dangerous execution by default.

Tools that drive execution (`run_pipeline`, anything that triggers a
remote daemon) require explicit per-deployment configuration and remain
off by default. The default tool set is read-mostly: list, get, append
note, approve gate, reject gate.

Exit criteria:

- The MCP server documents each tool in `docs/mcp-tools.md`.
- Each tool returns the standard `JsonResponse`-shaped envelope.
- Black-box and MCP tests cover at least one tool per coordinator
  endpoint added in Phases 4–8.

## CLI surface (target)

Once Phases 1–10 are complete, the CLI looks like this:

Coordinator lifecycle:

```bash
sykli coordinator start
sykli coordinator migrate
sykli coordinator status
```

Org and team:

```bash
sykli org create false-systems
sykli team create platform --org false-systems
sykli team token create platform
```

Daemon:

```bash
sykli daemon start
sykli daemon join --coordinator https://sykli.internal --org false-systems --team platform
sykli daemon status
```

Work:

```bash
sykli work create "Investigate failing checkout deploy"
sykli work list
sykli work show <work-id>
sykli work claim <work-id>
sykli work assign <work-id> --to agent:claude
sykli work note <work-id> "Found likely API breakage"
```

Run:

```bash
sykli work run <work-id> contract.json
sykli runs list --work <work-id>
sykli run show <run-id>
sykli run evidence <run-id>
```

Gates:

```bash
sykli gates list
sykli gate show <gate-id>
sykli gate approve <gate-id> --reason "Looks safe"
sykli gate reject <gate-id> --reason "API breakage not acceptable"
```

Every relevant command supports `--json` because agents need
machine-readable output. Every command's `--help` documents whether it
operates on local state, coordinator state, or both (via `--team`).

## First useful demo

Concrete scenario the design optimizes for, achievable after Phases 6–8:

1. Yair creates a work item:

   ```bash
   sykli work create "Review PR #176 for timeout and success criteria behavior" --team platform
   ```

2. Dima sees it and claims it:

   ```bash
   sykli work list --team platform
   sykli work claim <work-id>
   ```

3. Dima's daemon runs a Sykli contract:

   ```bash
   sykli work run <work-id> review-contract.json
   ```

4. The run reaches a review/gate/verify node and pauses. The daemon
   reports `gate.requested` to the coordinator.

5. The gate blocks. Yair (or any approver) inspects the run:

   ```bash
   sykli gates list --team platform
   sykli gate show <gate-id>
   ```

6. Yair approves or rejects from the CLI:

   ```bash
   sykli gate approve <gate-id> --reason "Evidence reviewed"
   ```

7. Dima's daemon picks up the decision on its next heartbeat and
   continues the run.

8. The work item records the outcome and the evidence references. Both
   Yair and Dima can see the same run summary, the same gate decision,
   and the same per-task statuses.

This demo crosses two machines, two networks, and two humans, without
either machine being on the same LAN. That is the whole point.

## Non-goals throughout the roadmap

These do not appear in any phase here. Each requires its own design doc
if reconsidered:

- SaaS hosted control plane.
- Billing.
- Web UI.
- Complex RBAC beyond `owner`/`member`/`approver`.
- Artifact or log hosting by default.
- Remote shell.
- Global scheduler that pre-empts local execution.
- LLM provider integration on the coordinator.
- OAuth enterprise admin work in v0.
- Refactor of BEAM mesh transport.
- Mandatory upload of full run data.
- Secrets synced by default.
- Federation between coordinators.

## Cross-references

- `docs/coordination-modes.md`
- `docs/self-hosted-coordinator.md`
- `docs/daemon-join-protocol.md`
- `docs/team-mode-security.md`
- `docs/local-state-plane.md`
- `docs/done.md`
- `docs/error-codes.md`
- `docs/false-protocol-schema.md`
- `docs/mcp-tools.md`
