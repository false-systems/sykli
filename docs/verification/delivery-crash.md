# Delivery crash synchronization

The delivery spec is README's **The order is durable**, **The verdict has
dimensions**, and **A lost acknowledgement is never answered by applying
again**, plus AGENTS.md's invariant against repeating an unacknowledged apply.

## Bug and fix

`launch` spawns the shell before `Attempt::started` records `Started`.
The shell may write `apply.ready` before that append. The old `kill_while`
helper waited only for this child signal, then killed the coordinator and
expected `applying`. A live lease with no Started record correctly reads
`dispatched`; a live lease with that record reads `applying`. Neither is success.

Synchronize the test on **both** child readiness and an explicit, paused
journal boundary. Add a debug-only checkpoint before the Started append;
reuse the checkpoint after it. Keep both windows as separate exact tests.
Release builds have no pause hooks; the existing post-Started test instead
waits for the recorded start while the child remains latched. Only the new
pre-Started injection case requires debug hooks.
Do not broaden the expected state, sleep to guess scheduling, or change the
production state machine. The child latch keeps the orphan alive while its
coordinator is killed. The observer-only helper has a different boundary and
does not establish Started.

## Lean first: three spec checks

`DeliveryCrash.lean` uses only Lean's standard library (checked with 4.35.0).
Run `lean docs/verification/DeliveryCrash.lean` from the repository root.

1. `before_started_crash`: at the pre-append barrier, crash preserves
   `dispatched`, resume refuses the live orphan, apply count is unchanged.
2. `after_started_crash`: at the post-append barrier, crash preserves
   `applying`, resume refuses the live orphan, apply count is unchanged.
3. `recovery_without_duplicate`: after the orphan ends without an end record,
   either journal window is `lost`; resume reconciles. Any finite sequence of
   observations preserves apply count. Inconclusive evidence stays unknown;
   attributed evidence establishes effect, not another apply.

`old_wait_is_not_a_journal_barrier` provides the counterexample: identical
child readiness, different recorded phases. These proofs use no `sorry`,
custom axioms, external libraries or runtime dependency.

## Rust translation and limits

The two barrier cases translate to real CLI tests with SIGKILL, FIFOs,
lease checks and exact journal-state assertions. Each then exercises the
third property by releasing the orphan and resuming against the fake target,
checking the effect and that apply count remains one. Polling is bounded.

This is a deliberately small executable specification, not a proof of Rust
equivalence, OS scheduling, leases, fsync, complete observation attribution,
health classification or explicit `--reapply` / `--assume-ended`. Those are
covered by the existing Rust suites. The model assumes accurate liveness and
an already-dispatched attempt without a recorded end. Its attributed
observation abstracts the existing reconciliation predicate. Lean validation
is a separate local development check; building, testing and releasing the
Rust product does not require Lean.
