# ADR-0006: Content-addressed evaluation is the product model

Status: accepted, 2026-08-09.

## Context

Calling Sykli a CI runner describes one use but hides the reusable primitive.
Coding agents, local hooks, and hosted CI all need to ask the same questions:
what work applies to this repository state, what can be reused, what ran, and
what result was produced?

## Decision

Sykli is the content-addressed evaluator for declared work graphs:

```text
contract + declared inputs + runtime fingerprint -> plan -> result + receipt
```

- The contract is the query and its hash is the query identity.
- `plan` is the read-only explanation of affected work.
- Delta selection and caching optimize evaluation without changing meaning.
- `run` evaluates the graph.
- A receipt is the immutable result record and the only evidence artifact.

The local CLI and versioned JSON are the agent interface. Sykli does not launch
agents, hold sessions, or become a daemon. Toimija supplies repository context
and verification authority; Sykli supplies plans and run receipts.

Repeatability claims remain bounded. Equal contract, declared inputs, and
runtime fingerprint produce equal evaluation identity. Equal command outcomes
also require tasks to avoid undeclared and nondeterministic inputs. Sykli does
not call an execution hermetic unless a runtime enforces it.

## Consequences

- CI is one client of Sykli rather than Sykli's product boundary.
- Agent workflows use `plan --json` during work and a trusted Toimija gate at
  handoff.
- A future execution-provider protocol must be justified by daily family use;
  it is not required to establish the evaluation model.
