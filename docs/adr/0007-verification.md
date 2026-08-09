# ADR-0007: Verification — receipts must have a consumer

Status: accepted, 2026-08-09.

## Context

The Elixir predecessor reached 0.7.0 with a large feature surface and still
lost every internal user: nothing *demanded* what it uniquely produced. A
receipt nobody checks is overhead with extra JSON. Two gaps blocked checking:

1. On a dirty working tree the old subject carried HEAD's tree OID plus a
   `dirty: true` confession — an honest admission that the receipt could not
   say which bytes ran. Agent workflows are almost always dirty, so the
   strongest receipts sykli produced in practice were the weakest ones.
2. There was no verb that consumed a receipt. `run` produced; nothing checked.

## Decision

**Subjects address content.** `subject.tree_oid` is the git tree OID of the
working tree itself: every non-ignored file staged into an ephemeral index
(`GIT_INDEX_FILE`), `.sykli` evicted so receipts never perturb the tree they
witness, then `git write-tree`. Same content, same OID, on any machine —
git's own addressing, not a sykli-invented hash. `head_tree_oid` is kept for
reference and `dirty` is derived by comparison, never self-reported. An
`inputs_digest` additionally hashes every declared input, including files Git
ignores or files rooted outside the repository.

**`sykli verify <receipt> --contract <path>` is the consuming verb.** It
checks consistency in ordered stages and prints one line per check:

- schema is `sykli-receipt.v1`;
- the receipt's `contract_hash` equals the hash of the contract as loaded
  now (which itself must agree with `sykli.lock` when present);
- the receipt's `subject.tree_oid` equals the working-tree OID recomputed at
  verify time — a stale receipt fails here;
- its declared-input digest still matches;
- the outcome is `passed` or `cached`, and every expected task record is
  successful and importable.

**Exit codes are stages, like a CI pipeline**: checks run in order and the
first failing stage decides, so gates branch on the code instead of parsing
text.

| code | meaning | consumer's move |
|------|---------|-----------------|
| 0 | verified | proceed |
| 1 | outcome or evidence failed | the work is bad or incomplete — fix it |
| 2 | cannot verify (not a receipt, unreadable input, git or contract error) | usage/environment problem |
| 3 | tree or input mismatch | receipt is stale — re-run sykli |
| 4 | contract mismatch | contract drifted — re-lock or investigate |

Contract drift outranks tree staleness because re-running cannot fix it.
Code 2 extends the exit-2 misuse convention the emitter guard and
`install.sh` already established.

Verify proves **consistency, not authenticity**: this receipt matches this
exact tree and this pinned contract, and nothing changed since. It cannot
prove the commands truly ran; that would take signing or a sealed runtime,
and is a consumer built on receipts if ever needed (same posture as
ADR-0002's dropped DSSE/SLSA attestations).

**CI is the first enforcing consumer.** The gate runs
`sykli run sykli.json --json` with the receipt redirected outside the
workspace, then `sykli verify` against it, and uploads the receipt as an
artifact. A run whose receipt does not verify does not pass.

**Toimija integration contract** (for toimija to adopt, not sykli to build):
a handoff or pre-commit gate may demand a receipt file, verify it with
`sykli verify`, and branch on the exit code — 3 means *stale, re-run*;
1 means *the work is bad or its evidence is incomplete*. The receipt path and
the exit-code table are the whole interface; sykli never writes into toimija's
stores (ADR-0002).

## Consequences

- Receipts written on dirty trees are now exactly as strong as clean-tree
  receipts: the subject names the bytes that ran.
- `run` grew a fixed cost: hashing the working tree (git does the work;
  ignored directories such as `target/` are skipped).
- Ephemeral staging writes loose objects into `.git/objects`; they are
  unreachable and garbage-collected, the same footprint as `git stash create`.
- Verification is repo-local and offline; no server, no state, consistent
  with the founding non-goals.
