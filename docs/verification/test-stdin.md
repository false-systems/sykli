# Finite test stdin without a writer/exit race

Sykli validates explicit repository facts before running work. Invalid facts
must be refused, whether or not stdin was consumed. The existing test asserted
that refusal but could panic first: the child exited while the parent wrote
pre-push input through a pipe (`tests/facts.rs`, `BrokenPipe`). The same
spawn-then-write pattern occurs in ten one-shot helpers across six suites.

## Three Lean checks before Rust

`lean docs/verification/TestStdin.lean` (Lean 4.35.0) checks:

1. Prepared input exposes exactly the original bytes at offset zero.
2. Early exit preserves the actual exit code, with no parent write after spawn.
3. Both exit and spawn failure close the retained input handle before removing
   the temporary file, without changing the collected outcome.

This is an ordering/ownership model, not a proof of Rust, filesystem I/O or
cleanup success. Rust must propagate preparation, spawn and cleanup failures.
No custom axioms, `sorry` or external Lean dependencies are used.

## Rust translation and scope

Use one standard-library-only test helper. Create a private, uniquely named
file outside repository fixtures, write finite input completely, rewind it,
then pass its handle to the command. Collect output, clear the command's
retained stdin handle (including on spawn failure), and remove the file.
A drop guard also handles early preparation failures. No input is silently
discarded, no BrokenPipe exception is added, and callers retain their existing
exit-code, refusal-message, receipt and side-effect assertions. The file's
lifetime is one invocation; no store, service or runtime dependency is added.

Apply it to the one-shot helpers in facts, forgery, git_calls, boundary,
external and cli. The concurrent-first-push test intentionally keeps stdin
pipes as a barrier: each successful child must wait for its push line. It is
not an early-refusal helper and must not be converted to eager input. Production
git-cat-file and output-capture pipes are separate streaming protocols.

The CLI reads stdin as bytes; these tests do not require pipe-specific seek
or terminal behavior. Tests specifically about a streaming boundary must keep
their pipes. The helper is not a general-purpose subprocess runner.
