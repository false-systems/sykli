# Complete-or-error delivery inventory

README's **Busy journals** requires unreadable deliveries never to disappear
from health judgments or target sequence reconstruction. **A lost
acknowledgement is never answered by applying again** requires unresolved
applies to block another delivery until reconciled (or explicitly overridden
under the existing restricted override rules).

## Reproduced bug

Discovery decoded only the first record and silently omitted any delivery
whose record could not be read. Directory enumeration errors also became an
empty or partial list. A real CLI regression kills a delivery after its apply
exits but before its acknowledgement, corrupts its first record, then asks
for another delivery. Before the fix it applies again and returns exit 0,
`VERIFIED HEALTHY`, instead of refusing with exit 2.

## Three Lean checks before Rust

Run `lean docs/verification/DeliveryInventory.lean` with Lean 4.35.0.

1. An unreadable entry anywhere makes inventory unsuccessful.
2. A completely valid inventory preserves every entry, not a filtered subset.
3. An inventory error cannot authorize apply, sequence rebuild or pruning.

The executable model also distinguishes a genuinely absent store from an I/O
failure. Proofs use no custom axioms, `sorry` or external dependencies. It is a
specification of error propagation, not a proof of Rust or filesystem behavior.

## Rust translation

Use `Result<Vec<Listed>, String>` and propagate directory and entry errors.
For each delivery-shaped entry reuse the existing locked `History::load`
validator, including record hashes, ownership, chain and fact validation,
before trusting its request. Native staging/bookkeeping names are not published
delivery identities. Only a genuinely absent store is empty.

All decision callers propagate errors: new delivery, resuming a dispatch,
status/supersession and sequence reconstruction. Best-effort retention may
decline pruning with a diagnostic, but never prune from a partial inventory.
Existing target, lease, reconciliation and retention checks remain in place.

An unreadable request cannot safely identify its target, so it blocks discovery
even for another target. Concurrent creation or pruning may cause a conservative
transient error: retry after the writer finishes. No lock is stolen, corruption
is not repaired automatically, and no unknown delivery is discarded. This
trades availability for preserving evidence and preventing an unproven apply.
Lean remains a development-only check, not a Rust build/runtime dependency.
