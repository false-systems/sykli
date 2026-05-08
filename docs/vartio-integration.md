# Sykli And Vartio Integration Model

## Summary

Sykli and Vartio address adjacent parts of agentic execution.

Sykli is the execution contract layer. It defines and runs scoped contracts:
task graphs, review nodes, gates, verify nodes, success criteria, evidence, and
FALSE Protocol occurrences for a single run.

Vartio is the fleet supervisor and behavioral envelope layer. It observes actor
behavior across runs and systems, learns or applies expected envelopes, and
judges whether an action is inside, outside, or unknown relative to those
envelopes.

The intended integration is evidence-based, not a merger. Keeping these
systems separate avoids turning Sykli into a workflow-policy engine or Vartio
into a task runner. Sykli should be able to ask Vartio for judgment at review
or gate boundaries, and Vartio should be able to consume Sykli run evidence as
observations.

## Why The Systems Are Separate

Sykli owns contract execution:

- Contract JSON and SDK-emitted pipeline shape.
- Task, review, gate, and verify graph nodes.
- Target-aware execution and success criteria.
- Run-local evidence, occurrences, attestations, and context files.
- Deterministic review primitives where Sykli can evaluate them directly.

Vartio owns behavioral judgment:

- Actor identity and actor history.
- Behavioral envelopes across runs, repositories, tools, and environments.
- Fleet/runtime judgment about whether actions are expected.
- Cross-run observation and policy-adjacent decision support.

Keeping these systems separate avoids turning Sykli into a workflow-policy
engine or Vartio into a task runner. The boundary is simple: Sykli runs a
contract and produces evidence; Vartio interprets actor behavior over time.

## Shared Vocabulary

| Term | Sykli Meaning | Vartio Meaning |
|------|---------------|----------------|
| Actor | The agent, user, service, or automation responsible for a run or node. | The identity whose behavior is observed and compared to an envelope. |
| Mission / intent | The operator request or contract purpose that a graph is meant to satisfy. | The contextual goal used to judge whether actions fit expected behavior. |
| Contract | The Sykli JSON graph and its schema-validated execution requirements. | An envelope context or constraint set used when judging actor actions. |
| Action | A task, review, gate, verify step, command, or declared operation in a run. | An observed behavior attributed to an actor. |
| Evidence | Structured run data: results, criteria outcomes, review results, artifacts, attestations, and occurrences. | Observation input and support for envelope decisions. |
| Gate | A Sykli graph boundary that may require approval or external judgment. | A decision point where envelope status can advise allow, block, or escalate. |
| Review | A Sykli non-executable graph node that runs a review primitive or external judgment. | A possible envelope check over an actor action or proposed action. |
| Envelope | Not owned by Sykli. Sykli may reference envelope decisions as evidence. | Expected behavior model for an actor in context. |
| Decision | A gate/review outcome inside a Sykli run. | An in-envelope, out-of-envelope, or unknown judgment with rationale. |

## Integration Shape

### A. Vartio As Review Primitive Provider

A Sykli review node can ask Vartio a scoped question:

> Is this actor action inside the expected envelope for this mission?

The Sykli node remains a review node. Vartio is one provider behind the review
primitive boundary. A Vartio-backed review primitive must return the same
`review_result` shape and field semantics as deterministic Sykli review
primitives; it must not introduce a bespoke result envelope. The review result
should be structured evidence, not only free-text commentary.

Example contract role:

```json
{
  "name": "review-envelope",
  "kind": "review",
  "primitive": "vartio_envelope",
  "agent": "vartio",
  "context": [".sykli/occurrence.json"],
  "depends_on": ["propose-patch"],
  "deterministic": false
}
```

This is illustrative only. `vartio_envelope` is not currently part of the
canonical schema.

### B. Vartio As Gate Advisor

A Sykli gate can use a Vartio decision as advisory or blocking input.

The gate remains a Sykli graph concept. Vartio supplies judgment such as
`in_envelope`, `out_of_envelope`, or `unknown`, with evidence and rationale.
Sykli decides how that judgment affects the run according to the gate contract.

This lets Sykli express boundaries such as:

- Continue automatically when Vartio says the action is in-envelope.
- Require human approval when Vartio says unknown.
- Fail or block when Vartio says out-of-envelope.

### C. Sykli As Evidence Producer

Every Sykli run can produce structured evidence for Vartio:

- Contract version and graph shape.
- Actor and run labels.
- Task, review, gate, and verify node results.
- Success criteria outcomes.
- Review primitive results.
- Artifact attestations.
- FALSE Protocol occurrences from `.sykli/`.

Vartio can ingest that evidence as observations without Sykli depending on
Vartio at runtime.

## Disagreement Semantics

Before any wire integration, Sykli must decide how Vartio judgments compose
with deterministic Sykli review primitives.

The recommended first rule is conservative:

- Deterministic Sykli review primitives remain authoritative for the facts they
  evaluate.
- Vartio remains authoritative only for envelope judgment about actor behavior.
- A Vartio `out_of_envelope` decision must not rewrite a deterministic
  primitive result from `passed` to `failed`.
- A gate may still treat `out_of_envelope` as blocking according to its own
  contract.
- If deterministic evidence and Vartio judgment appear to conflict, Sykli
  should preserve both results as separate evidence and let the gate decide.

This keeps review facts and behavioral-envelope decisions separate. A future
wire-format PR must lock the exact composition rule before adding runtime
integration.

## Example Flow

1. An operator requests a patch.
2. Sykli creates or receives a contract for the patch workflow.
3. An agent generates the patch as an executable task.
4. Sykli runs deterministic review primitives, such as `api_breakage`.
5. Sykli asks Vartio whether the proposed patch action is inside the actor's
   expected envelope. The call mechanism is not settled; it may be a review
   primitive call, a gate-advisor call, or offline evidence ingestion.
6. Vartio returns `in_envelope`, `out_of_envelope`, or `unknown` with evidence.
7. A Sykli gate decides whether to continue, fail, or require approval.
8. Sykli writes FALSE Protocol occurrences and run evidence.
9. Vartio may ingest the completed run evidence as future observation input.

## Future Wire Format

Any future Sykli/Vartio wire shape should be additive, schema-validated, and
version-gated when it changes the canonical contract. Vartio-specific fields
must follow the same version discipline as `task_type` and `success_criteria`.
They should not overload existing `task_type`, `primitive`, `success_criteria`,
or `ai_hooks` fields.

The `review_result` shape uses the review primitive result contract. Severity
values should use the review primitive vocabulary: `info`, `warning`,
`breaking`, or `critical`.

An illustrative review result could look like:

```json
{
  "review_type": "vartio_envelope",
  "status": "failed",
  "severity": "warning",
  "message": "action is outside the expected actor envelope",
  "tool": "vartio",
  "findings": [
    {
      "actor": "codex",
      "decision": "out_of_envelope",
      "reason": "attempted production credential access during docs-only task"
    }
  ],
  "evidence": {
    "run_id": "01...",
    "occurrence": ".sykli/occurrence.json"
  }
}
```

This is not a committed schema. It shows the expected direction: Vartio
judgment enters Sykli as structured evidence attached to review or gate nodes.

## Non-Goals

- Sykli does not become Vartio.
- Vartio does not become a workflow runner.
- No runtime coupling is required in the first integration.
- No mandatory UI dependency.
- No mandatory LLM provider dependency.
- No provider calls are specified here.
- No backend schema is implemented here.
- No compact agent projection format is defined here.

## Open Questions

1. Who owns actor identity stitching between Sykli run labels, agent provider
   identities, SCM identities, and Vartio actors?
2. Where is envelope state stored, and how is it versioned?
3. How are Vartio decisions audited alongside Sykli occurrences?
4. What transport should the first integration use: synchronous review
   primitive call, gate-advisor call, webhook, async occurrence ingestion, or a
   combination?
5. Should Vartio judgments be represented as review results, gate inputs, FALSE
   Protocol occurrences, or all three?
6. How does this map to the public FALSE Protocol event vocabulary over time?
7. Which Vartio decisions are advisory warnings, and which are blocking gate
   decisions?
