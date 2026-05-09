# Local State Plane

## Status

Design document for Phase 0 of `docs/team-mode-roadmap.md`. Defines the
relationship between each daemon's local `.sykli/` directory and the
self-hosted coordinator's stored state. Normative for future
implementation PRs.

The on-disk schema for `.sykli/` is owned by `docs/false-protocol-schema.md`.
This document specifies what crosses the boundary into the coordinator
and, crucially, what does not.

## Architecture sentence

> The daemon executes and records; the mesh dispatches inside trusted
> networks; the coordinator synchronizes team state across locations;
> `.sykli/` remains the local source of detailed evidence.

The local state plane is the "records" half of the daemon's job. The
coordinator is a downstream projection of part of it.

## The split

> Local `.sykli/` = detailed local truth.
>
> Coordinator = shared projection for team coordination.

Two stores. Different responsibilities. Different audiences. Different
retention rules.

`.sykli/` is the source of truth for everything Sykli observed on this
machine. It carries the raw FALSE Protocol occurrences, attestations,
artifacts, criterion outputs, and run history necessary for `sykli
explain`, `sykli fix`, MCP tools, and AI-readable analysis.

The coordinator's database is a projection: just enough metadata so that
multiple humans, agents, and daemons can coordinate work without each
needing to read every other machine's filesystem.

The local store is rich. The coordinator's view is intentionally thin.

## What lives in `.sykli/`

Documented in detail in `docs/false-protocol-schema.md`. Summary:

- `.sykli/occurrence.json` — the latest terminal occurrence.
- `.sykli/occurrences_json/<run_id>.json` — last 20 archived runs (JSON).
- `.sykli/occurrences/<run_id>.etf` — last 50 archived runs (ETF, fast
  BEAM reload).
- `.sykli/attestation.json` — DSSE envelope with SLSA v1.0 provenance
  for the latest run.
- `.sykli/attestations/` — per-task DSSE envelopes.
- `.sykli/context.json` — pipeline structure and health from
  `sykli context`.
- `.sykli/test-map.json` — file → tasks mapping.
- `.sykli/runs/` — per-run manifests for history.
- `.sykli/work/items/<id>.json` — local work item state.
- Work/run links are stored on existing `.sykli/runs/*.json` manifests as
  `work_item_id` and `contract_hash`; work item files do not copy run logs.
- (Phase 3 of the roadmap) `.sykli/gates/<gate-id>.json` — local gate
  decision state.

The local store is self-contained. A daemon with no coordinator
connection writes the same `.sykli/` it always did. The local-only mode
remains identical in shape to today.

## What the coordinator stores

Documented in detail in `docs/self-hosted-coordinator.md`. Summary:

- Org and team registry.
- Daemon sessions (id, labels, capabilities, last seen, online status).
- Work items, claims, assignments, notes.
- Run summaries (status, target, error code, started/finished).
- Per-node run state (kind, status, error code).
- Success criteria results (kind, status, message).
- Review result summaries.
- Gate state and decisions.
- Evidence references (URI + hash + visibility).
- Contract hashes and short summaries.
- Audit log.

The coordinator's view is a structured index. It tells the team **what**
happened, **who** did it, and **where the evidence lives**. It does not
duplicate the evidence itself.

## What the coordinator syncs by default

| Concept                          | Synced by default? |
|----------------------------------|:------------------:|
| Work item title, intent, status  | yes                |
| Work item assignment / claim     | yes                |
| Daemon heartbeat / liveness      | yes                |
| Daemon labels and capabilities   | yes                |
| Run status (`passed`/`failed`/...) | yes              |
| Per-node status                  | yes                |
| Gate state                       | yes                |
| Approval / rejection decision    | yes                |
| Review result **summary**        | yes                |
| Run-level error code             | yes                |
| Evidence reference (URI + hash)  | yes                |
| Contract **hash** and **summary**| yes                |

Each of these is small, structured, and free of operator secrets when
combined with the masking rules in `docs/team-mode-security.md`.

## What the coordinator does **not** sync by default

| Concept                          | Synced by default? |
|----------------------------------|:------------------:|
| Secrets (env, files, config)     | no                 |
| Full logs (stdout/stderr bodies) | no                 |
| Source code                      | no                 |
| Raw build artifacts              | no                 |
| Environment dumps                | no                 |
| Full review findings (beyond summary) | no            |
| Raw contract JSON                | no                 |

These remain on the originating daemon, in `.sykli/`. The coordinator
records a reference, not the bytes.

## The evidence reference pattern

Whenever a daemon has a piece of evidence the team may need to consult
later — an occurrence file, a DSSE attestation, a Check Run result, an
artifact in object storage — it tells the coordinator about it via a
single record:

```jsonc
{
  "type": "occurrence",            // or attestation / artifact / github_check / local_ref / object_ref
  "uri": "file:///Users/yair/proj/.sykli/occurrence.json",
  "hash": "sha256:...",
  "summary": "run 01KQPF... failed at task lint",
  "visibility": "local_only"       // or team / external
}
```

Resolution:

- `visibility: local_only` — the URI is meaningful only on the originating
  daemon. The team sees the reference and the hash but cannot fetch the
  bytes. This is the **default** for `.sykli/`-resident evidence.
- `visibility: team` — the URI is reachable from the team (e.g. a shared
  object store the team operates). The reference is enough.
- `visibility: external` — the URI is publicly resolvable (e.g. a
  GitHub Check Run permalink). Anyone with the link can fetch.

The coordinator never resolves the URI itself. It is metadata.

This pattern keeps the coordinator small, keeps secrets local, and lets
the team correlate runs across machines without uploading the world.

## Why hash-only by default

A `contract_hash` plus `summary` lets the team:

- Compare two runs to confirm they ran the same contract.
- Detect when a run drifted from the agreed-upon plan.
- Audit "did anyone run an unfamiliar contract?"

Without ever uploading the contract content. This is the same trade-off
the engine already makes with cache keys: hashes are durable identifiers,
and the bytes stay where they were generated.

## Survivability

The local plane survives the coordinator.

- A daemon that has never seen a coordinator writes `.sykli/` exactly
  as today.
- A daemon whose coordinator is unreachable continues to write `.sykli/`.
  Sync events queue in an outbox until reconnect. Local execution is
  unaffected.
- A daemon whose token is revoked stops syncing but keeps writing
  `.sykli/`.
- A coordinator whose database is wiped does not corrupt any daemon's
  `.sykli/`. The coordinator's projection can be rebuilt by re-syncing
  from each daemon (a future operations doc covers the backfill flow).

The asymmetry is deliberate: the coordinator is convenient, but
`.sykli/` is essential.

## Survivability the other way

The coordinator survives a daemon.

- A daemon's machine dies. The coordinator still holds the work item,
  the claim, the run summaries it received, the gate decisions, and the
  evidence references with `visibility: local_only`.
- Those local-only references become unreadable. The team learns the
  evidence existed but cannot inspect it. This is the right outcome:
  the coordinator did not steal the data; the team simply lost a
  machine.
- A new daemon can be assigned the same work item, run a new contract,
  and create new evidence references. The audit log retains the prior
  history.

## Dual-surface dual-store

`docs/done.md` requires every command to satisfy a human surface and an
agent surface. With the coordinator, the same command may touch:

- The local plane (`.sykli/`) — for `sykli explain`, `sykli fix`, MCP
  tools, full evidence.
- The coordinator — for `sykli work list --team`, `sykli gate approve`,
  team-wide queries.

A command that operates on both must be explicit about which it is
reading. The CLI surface in `docs/team-mode-roadmap.md` uses `--team` (or
the equivalent `--coordinator <url>`) to mean "operate against the
coordinator." Without that flag, every command remains a local
operation, exactly as today.

This rule must hold for `--json` output as well: the envelope's `data`
payload must carry a clear marker for which plane the answer came from
(e.g. `"source": "local"` vs `"source": "coordinator"`).

## What the local plane gains in Phase 1+

Phase 1 adds local work item files before any coordinator or network
behavior. The persisted file is versioned and intentionally small:

```json
{
  "id": "01KQPF7Q4W6J2M7V6YF2N0H6A2",
  "version": "1",
  "title": "Review PR #176",
  "intent": "Check timeout and success criteria behavior",
  "status": "open",
  "created_by_type": "member",
  "created_by_id": "yair",
  "assigned_to_type": null,
  "assigned_to_id": null,
  "created_at": "2026-05-08T10:00:00Z",
  "updated_at": "2026-05-08T10:00:00Z",
  "notes": []
}
```

Allowed `status` values are `open`, `claimed`, `running`, `blocked`,
`done`, `failed`, and `cancelled`.

Allowed actor type values for `created_by_type` and `assigned_to_type` are
`member`, `agent`, `daemon`, or `null` when the actor is not known.

The local work item file must not contain logs, artifacts, secrets, source
code, environment dumps, or full stdout/stderr. It is coordination state,
not execution evidence.

Local work items are exposed through:

```bash
sykli work create "Review PR #176"
sykli work list
sykli work show <work-id>
sykli work claim <work-id>
sykli work note <work-id> "Found likely API breakage"
sykli run <contract-or-dir> --work <work-id>
sykli work runs <work-id>
```

Each command supports `--json` and returns the shared CLI JSON envelope.

`--work` validates the local work item before execution starts. The run
manifest records the work item id and a deterministic `sha256:` contract
hash computed from canonicalized emitted contract JSON. Detailed evidence remains in
the normal `.sykli/` run, occurrence, log, and attestation stores; work state
only points at the run summary.

The roadmap adds local-only state for work items and gates **before**
networking turns on. That is intentional: a user should be able to:

- Create and claim local work items in solo mode.
- Approve local gates from the CLI.
- Track per-run evidence locally.

…and only later opt in to syncing those concepts to a coordinator. The
local plane is the foundation; the coordinator is a sync target.

## Cross-references

- `docs/coordination-modes.md`
- `docs/self-hosted-coordinator.md`
- `docs/daemon-join-protocol.md`
- `docs/team-mode-security.md`
- `docs/team-mode-roadmap.md`
- `docs/false-protocol-schema.md`
