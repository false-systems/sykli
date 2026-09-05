# ADR-0009: Local typed production

Status: accepted for implementation, 2026-09-06, requested by Yair in the
Sykli implementation handoff, reaffirmed in revision 0.3.

The named workflow is source → executable, with unit checks on the source and
a smoke check on the collected executable, continued by a fresh CLI invocation.
Exit status plus output existence cannot establish executable type, check
subject identity or continuation after an interrupted client. This is the
concrete Sykli workflow motivating contract growth under ADR-0005. The user's
handoff explicitly expands the older receipt-only boundary in ADR-0002/0006:
local production assessment and persistence belong here; task closure,
publication authority and worker orchestration still do not.

The implementation criterion is less coordination per delivered artifact: the
next worker needs a production identifier and its store, not a handover essay.
No new abstraction is justified solely by the larger future product model.

| Existing primitive | Location | Adaptation |
| --- | --- | --- |
| DAG validation | `src/main.rs::validate` | Reuse with edges derived from explicit ports. |
| Shell executor and bounded capture | `src/main.rs::run_task` | Run in fresh prepared directories; retain execution observations. |
| SHA-256 / sorted JSON | `src/main.rs` | Reuse with explicit domains and duplicate-key rejection for new documents. |
| File cache | `src/main.rs::LocalCache` | Preserve for legacy graphs; do not import into typed productions. |
| Receipts / verify | `src/main.rs` | Preserve v1; production records have their own schema and verification scope. |
| Ecosystem init | `src/init.rs` | Preserve graph detection; provide a compact reviewed production example. |
| Durable requests, artifacts, attempts | Gap | Atomic files and a per-production OS lease, no database or service. |

The opt-in `sykli-production-contract.v1` embeds recipes, collection rules and
the local profile. Existing `sykli-contract.v1` is unchanged. All new support
is local Linux/macOS; executable inspection supports native ELF and thin Mach-O
on x86_64/aarch64. Other execution modes and reuse policies are rejected.

A bounded executor subprocess holds the production lease and records one
attempt even if its initiating CLI disappears. It is not a daemon and does
not execute agents. If that executor itself disappears without a terminal
record, the attempt remains indeterminate: there is no blind retry or PID-based
claim that all children stopped. A live executor can finish and be reconciled
by the next client. Continuation skips completed operations, not instructions
inside a running command.

Sources are explicit regular-file selections, including edited and untracked
files. They are captured into a canonical manifest and materialized afresh for
each attempt. Symlinks, special files, submodules and ambiguous paths are
rejected. Commands are trusted local foreground commands; prepared inputs do
not establish hermeticity. Host dependencies remain possible, so cross-production
reuse is disabled. The current local user can alter the store or executor:
verification establishes consistency, never authenticity or execution truth.

The first controller is serial. Existing graph parallelism is unchanged.
Historical completion and current artifact availability are separate; missing
bytes cause delivery failure. Explicit rebuilds select new attempt lineages
and invalidate downstream checks by their actual subject bindings.
