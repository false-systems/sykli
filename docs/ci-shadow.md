# CI shadow experiment

Requested by Yair: use knowledge of what changed to test whether Sykli can
avoid unnecessary CI work without hiding failures. This experiment observes
real pull requests; it never enables skipping or decides the required gate.

The Linux job first runs the existing full graph through the shipped Action.
No Sykli cache is restored into that checkout. Windows continues to run all
of its existing checks. After the Linux run, an advisory step compares that
receipt with cache evidence produced from the pull request's base commit.
Experiment failures do not turn a failed full gate into a pass or fail an
otherwise successful gate.

## What gets compared

The repository script, scripts/ci_shadow.py, uses disposable worktrees:

1. Resolve the supplied base and the current checkout's exact commit. On a
   pull request, the latter is the merge candidate Actions tested, which
   may differ from the PR head. Record both coordinates.
2. Record a NUL-delimited name/status diff between those commits, with rename
   detection disabled. Renames appear as deletion plus addition so both
   paths are accounted for. Deleted files, mode changes, spaces and newlines
   are retained.
3. Verify the full receipt against the candidate worktree and contract.
   A failed but consistent full run is useful evidence; a stale or invalid
   receipt makes the observation unavailable.
4. Run the base graph cold, with the same Sykli binary. Copy only that
   worktree's cache and receipts into the candidate inspection worktree.
5. Run plan --explain --json, then repeat with the changed paths for
   selection evidence. Neither command restores candidate outputs.
6. Compare proposed reuse with the independently executed task records in
   the full receipt. Retain the plans, source receipts, logs and report.

The baseline need not pass completely: only cache entries backed by its
passing task records can be proposed for reuse. The experiment introduces
no cross-run cache service or persistent CI cache.

## Reading an observation

Each task reports its affected flag, cache evidence, full result and comparison:

- agrees: baseline evidence is available, runtime and inherited environment
  digests match the full run, and the full task passed with matching declared
  output digests.
- disagrees: available evidence would have hidden a full task failure/error,
  or reused different declared output bytes. Investigate missing inputs
  and nondeterminism.
- context_mismatch: runtime or inherited environment differs; no conclusion.
- not_independently_executed: the reference task was cached or blocked.
- not_proposed: cache evidence is missing, invalid, deferred, or has an input
  error. It is not counted as reusable.
- unproven: available evidence lacks a comparable complete result.

The unmapped_paths field lists changes that match no exact input declaration
in the candidate contract. These are investigation leads, not proof that a
file matters or that a task is safe to skip. Changed-path selection and cache
eligibility are separate decisions. A docs-only PR can select no tasks while
all checks still run in the authoritative gate.

The potential_reused_task_ms field sums full-run durations only for
agreements. Tasks can overlap, so this is **not job wall time saved**.
The report separately records baseline cost, inspection time and total
experiment time. This first experiment spends an extra baseline run to
collect evidence; it does not claim a net CI saving.

Results appear in the job summary and the sykli-ci-shadow artifact.
The report.json file is a sykli-ci-shadow.v1 local observation, with commit
IDs, binary and full-receipt digests, changed paths, task comparisons and
timings. It is advisory and not authenticated. Logs and supporting receipts
allow disagreements to be inspected without rerunning the PR.

## Running locally

Use a fresh output directory outside the tracked tree, a committed candidate,
and a receipt from a full run of that candidate:

    python3 scripts/ci_shadow.py \
      --base BASE_COMMIT --binary "$(command -v sykli)" \
      --receipt .sykli/full-receipt.json --output .sykli/shadow-observation

This is Unix CI glue for Sykli's root sykli.json graph. It uses Python's
standard library, Git and Sykli. It executes the base graph, so baseline
toolchains must be available. The baseline attempt is bounded to ten minutes;
the CI step has a twelve-minute limit. Missing history, missing contracts,
unsupported filenames, failed evaluation or invalid receipts produce an
unavailable observation, never fabricated agreements.

Review the first ten PR observations before proposing skip behavior.
Investigate disagreements and unmapped paths, then evaluate toolchain identity,
environmental variation and graph completeness. No number of matching samples
proves declarations complete. AI can propose contract changes from this
evidence; the experiment does not generate or apply them.
