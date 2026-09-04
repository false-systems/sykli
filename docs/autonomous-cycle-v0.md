# Autonomous engineering cycle v0

Status: proposal. This document defines composition outside Sykli. It adds no
Sykli contract fields, coordinator, worker, or closure authority.

## Goal

After a human has ratified the design and its authority limits, the local
toolchain can progress one bounded engineering work item without further
human direction. It either closes the work with current evidence or stops in a
named `held` state. It never fills a missing decision with model judgment.

```text
Kisko decision ─┐
Teko work ──────┼─> sealed cycle ─> Ohjaa reducer ─> Toimija worker leg
Kelpo QA plan ──┘                                      │
                                                         └─> Sykli gate receipt
                                                               │
Kelpo verdict <───────────────────────────────────────────────┘
      │
      └─> Teko closure
```

Sykli stays the content-addressed evaluator. It runs a declared graph and
emits a receipt; Toimija binds that receipt to a repository point; Teko alone
decides closure.

## Ownership

| Tool | Owns | Does not own |
| --- | --- | --- |
| Kisko | Policy resolution and a traceable decision | Worker dispatch or closure |
| Teko | Ratified work, obligations, evidence acceptance, closure | Gates or worker execution |
| Kelpo | QA plan and QA verdict | Requirements or worker dispatch |
| Ohjaa | Derived next action and its launch | Durable workflow state or repository truth |
| Toimija | Repository points, sessions, trusted gates, sealed receipts | Policy or workflow decisions |
| Sykli | Declared graph evaluation and receipts | Meaning, QA, closure, or coordination |

Rauha, Ote, Selko, and Taso are not dependencies of this cycle. They may
later supply an execution provider, lifecycle boundary, witness, or merge
consumer without changing this ownership table.

## Sealed inputs

`cycle.v1` is an upstream composition artifact, not a Sykli contract. It is
sealed before any worker is launched and references immutable inputs:

```json
{
  "schema": "cycle.v1",
  "work": { "artifact": "teko-work.v1", "digest": "..." },
  "decision": { "artifact": "kisko-decision.v1", "digest": "..." },
  "qa_plan": { "artifact": "kelpo-plan.v1", "digest": "..." },
  "implementation": { "worker_role": "implementation", "max_attempts": 2 },
  "qa": { "worker_role": "qa", "must_differ_from": "implementation" },
  "effects": ["commit", "push", "open_pr"],
  "retry": { "gate_failure": 1, "worker_failure": 1 }
}
```

The actual envelope and canonical encoding belong to the composition owner.
The important v0 rules are:

- every reference has a digest and an input repository point;
- effects are an allowlist; an omitted effect is forbidden;
- retry budgets are integers fixed before the first attempt;
- the QA plan is sealed before implementation evidence exists;
- the implementation and QA sessions must differ;
- an unknown or stale artifact is `held`, never repaired by inference.

## Kisko decision

Kisko becomes operationally useful when it emits an executable choice rather
than prose. A request binds a Teko obligation, a Toimija repository point,
candidate actions, and the exact policy snapshot. Its result is one of:

```text
allow(action, required_evidence, expiry)
deny(action, reasons)
hold(missing_facts)
escalate(conflict)
```

Kisko resolves ratified rules over supplied facts deterministically. A model
may propose candidate actions before the request is sealed, but it has no
authority to create a decision. A missing rule, fact, or unresolvable conflict
is a `hold` or `escalate` result. The decision carries the selected rules,
excluded alternatives, fact digests, policy snapshot, and manifest digest so
the result can be replayed.

For v0, a cycle may start only with `allow`; `deny`, `hold`, and `escalate` are
terminal non-launch states.

## Ohjaa reducer

Ohjaa gains a composition command, conceptually `ohjaa cycle step`. It is a
pure reducer over `cycle.v1` and verified artifacts. It stores no workflow
database and chooses exactly one action:

```text
no current Kisko allow          -> held: decision
no current Kelpo plan           -> held: qa-plan
no implementation result        -> launch: implementation
stale or absent required gate   -> run: trusted gate
no independent QA result        -> launch: qa
no current Kelpo pass           -> judge: qa
all closure evidence current    -> close: Teko
otherwise                       -> held: named reason
```

`ohjaa cycle run` is only a loop over `step`; it is not a daemon or scheduler.
It exits on `closed`, `held`, or exhausted budget.

Each action has the deterministic identity:

```text
cycle digest / leg / attempt / input repository point
```

Before launching it, Ohjaa asks Toimija and Teko whether that identity already
has a session, sealed receipt, or accepted artifact. It consumes that result
instead of duplicating the action. A tree move, concurrent live leg, session
identity mismatch, or exhausted budget produces `held`.

## Toimija facts

Toimija stays a host of facts. The reducer needs versioned JSON for:

- the current repository point and its freshness relation to an artifact;
- a session's identity, lifecycle state, input and output points, and bound
  worker result;
- a trusted gate definition digest and its sealed receipt;
- an explicit failure to inspect any of those facts.

The reducer must treat unavailable inspection as `held`; it must not treat it
as an empty session list or a passing gate. Toimija does not choose a leg,
evaluate Kelpo evidence, or close Teko work.

## Evidence and closure

An implementation leg provides its Toimija session identity and current gate
receipts. Sykli receipts remain raw execution claims. Kelpo normalizes the
verified receipts against its pre-sealed plan and emits `pass`, `fail`, or
`inconclusive`. Teko accepts only a current passing verdict and independently
checks its own scope, obligations, and required gate freshness before closing.

The cycle has three terminal outcomes:

| Outcome | Meaning |
| --- | --- |
| `closed` | Teko closed the exact work with current required evidence. |
| `held` | A named decision, fact, capability, or budget is absent. No action remains legal. |
| `failed` | A declared terminal failure occurred, such as an exhausted retry budget or Kelpo failure. |

Opening a pull request is an optional declared effect. It is never implied by
closing work, and merging is outside v0.

## Golden cases

The composition owner should define fixtures before implementation:

1. Current allow, implementation, gate, independent QA pass, and Teko close.
2. Missing Kisko decision holds before a worker launch.
3. A gate receipt made before a tree move is stale and re-runs only within its budget.
4. QA evidence from the implementation session is inconclusive.
5. Re-running `step` after a crash consumes the existing matching session.
6. A second live leg for the same action identity holds instead of duplicating work.
7. An undeclared push or PR effect holds before the effect is attempted.
8. Exhausted retry budget fails without another launch.

## Delivery order

1. Ratify this ownership and the `allow | deny | hold | escalate` Kisko result.
2. Seal `cycle.v1` and the golden cases in the composition owner.
3. Implement the pure `ohjaa cycle step` reducer and fixture suite.
4. Add Toimija JSON facts and action adapters only where the reducer lacks a
   factual input.
5. Connect the existing Sykli, Kelpo, and Teko artifacts; do not add a Sykli
   coordination surface.
