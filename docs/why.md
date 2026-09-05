# Receipts, not logs

CI produces a green check and a log. The check says a pipeline passed; the log
says what some machine printed. Neither says what ran against which tree, nor
whether the same commands on your laptop would mean the same thing. When the
one reading the result is a coding agent, or a reviewer of an agent's work,
that gap is the whole problem.

Sykli produces a receipt instead.

## What a receipt is

A `sykli-receipt.v1` is a JSON document written by `sykli run`. It carries:

- the hash of the contract that was evaluated, so two receipts are comparable
  only if they evaluated the same declared graph;
- the OID of the working tree and a digest of the declared inputs, so the
  receipt is about one exact state of the repository and nothing else;
- every task: its command, exit code, duration, output digests, and whether
  it was observed now or reused from an earlier receipt whose contract,
  inputs, and runtime fingerprint were identical;
- one outcome for the whole graph.

It carries no opinion. `passed` means every command exited zero. What that
implies about the software is somebody else's claim to make, on top of the
receipt.

## What verify does with it

`sykli verify <receipt>` recomputes the tree OID and the contract hash where
it runs and compares. The exit code is the whole answer:

| Exit | Meaning |
|---|---|
| 0 | the receipt describes this tree and this contract, and the outcome is passed |
| 1 | the outcome or evidence failed — the work is bad or incomplete |
| 2 | this is not a receipt, or git or the contract could not be read |
| 3 | the tree or inputs differ — the receipt is stale, run again |
| 4 | the contract differs — the graph changed, re-lock |

A reviewer who has never seen sykli learns from a single number whether the
evidence in front of them is about the code in front of them.

## Why this matters for agents

An agent that runs "the tests" and reports "they pass" has made a claim. A
receipt is not a claim: it is content-addressed to the tree and the contract,
and a stale one fails `verify` on the reviewer's machine. Two consequences:

- Before editing, an agent asks `sykli plan --changed` which tasks its change
  will touch, and runs those, instead of everything or nothing.
- After editing, the agent hands over the receipt. If it edited after running,
  the receipt is stale and says so. If it ran a different graph, the hash says
  so. The agent's honesty is not load-bearing.

`cached` is the one outcome to read carefully: it means an earlier receipt
was reused because nothing relevant changed. When observed evidence is
required, run in a fresh worktree; the receipt will then say `passed` with
`source: task`.

## What sykli refuses to be

Not a server, not a scheduler, not a work tracker, not an interpreter of
results. Those exist as separate tools that consume receipts, and the
deletion record in `docs/adr/0005-deletions.md` keeps them out. A receipt is
useful precisely because the thing that wrote it has no opinion about it.
