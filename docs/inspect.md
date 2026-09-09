# See what a change has established

`sykli inspect` reads one pull request's workflow runs and reviews through your
existing GitHub CLI, saves them as an immutable evidence bundle, and shows what
was observed. With a requirements file it also says which declared conditions
are established, refuted, or still unproven, and why. `sykli assess` replays a
saved bundle offline with the same evaluator.

Both commands are **read-only and advisory**. They do not merge, deploy, trigger
workflows, post statuses, or certify that GitHub's merge policy is met. They
establish only the conditions you name, from provider reports, under a trust
designation that is printed with every result:

```text
Trust: local collector and store; receipt is not authenticated
```

The bundle trusts the machine that collected it. Digests catch accidental
change and torn writes, not someone replacing the whole bundle. Enforcement in a
protected environment is a later deployment of the same evaluator, not this one.

## Prerequisites

- A `sykli` build that includes `inspect` (unreleased after 0.2.0; [install](install.md)).
- [GitHub CLI](https://cli.github.com/) on `PATH`, logged in (`gh auth login`).
  Sykli uses its authentication and never stores credentials. Private
  repositories need read access to pull requests and Actions.
- Only `github.com` is supported in this release.

## First look: observations without configuration

```sh
sykli inspect --repo false-systems/sykli --pr 25
```

```text
false-systems/sykli #25 at 0e1982a…

Observed
  CI (workflow 327406134) run 65 attempt 1 via pull_request: GitHub reports success
  Reviews: none observed
  Author account: 154441282
  Workflows: CI 327406134 (.github/workflows/ci.yml); Release 330221374 (.github/workflows/release.yml)
Gaps: none

Requirements: not configured
Saved observations: .sykli/evidence/f3a1d55b…
```

This is the actual shape of the output for that pull request on 2026-09-08.
Without requirements there is no verdict: a workflow named "CI" implies nothing
about tests, coverage, or safety. The numbers shown are the exact workflow and
account IDs a requirements file needs.

Exit code 0 means a valid snapshot of observations was saved, even if it records
provider gaps. Exit code 2 means the candidate itself could not be read, or the
requirements are bound to a different repository; that binding is checked
against the first response, before anything is listed or saved.

## Requirements

A requirements file names the finite conditions that matter for one
repository. It is content-addressed: any change produces a new identity and a
new request, so nothing is approved by accident.

```json
{
  "schema": "sykli-requirements.v1",
  "purpose": "review-readiness",
  "repository": {"host": "github.com", "id": 1323443147},
  "max_observation_age_seconds": 300,
  "requirements": {
    "ci": {
      "kind": "workflow-reported-success",
      "source": {"provider": "github", "workflow_id": 327406134, "event": "pull_request"},
      "selection": "latest-run-latest-attempt"
    },
    "review": {
      "kind": "candidate-approval",
      "source": {"provider": "github"},
      "allowed_user_ids": [789],
      "minimum": 1,
      "exclude_pr_author": true
    }
  }
}
```

- `repository.id` comes from `gh api repos/OWNER/NAME --jq .id`; `inspect`
  refuses requirements bound to another repository.
- `workflow_id` is shown by `inspect`, or `gh api repos/OWNER/NAME/actions/workflows`.
- `allowed_user_ids` are account IDs (`gh api users/LOGIN --jq .id`), never
  logins. IDs establish account identity, not independence from the author.
- Duplicate keys, empty requirement sets, unknown kinds or fields, zero or
  negative IDs, an age outside 1 second to ten years, and any selection other than
  `latest-run-latest-attempt` are rejected. Only `pull_request` events are
  supported for the workflow predicate; `pull_request_target`, merge queues, and
  dispatch events are not.

Keep the approved file outside the candidate checkout. Requirements inside a
pull request are proposals; the digest Sykli prints is the one you chose to
pass, and Sykli makes no claim that anyone ratified it.

## Assessment

```sh
sykli inspect --repo false-systems/sykli --pr 25 --requirements /trusted/sykli-review.json
```

```text
0e1982a… — UNPROVEN
Scope: declared review-readiness conditions; advisory

✓ ci      GitHub reports success for the required workflow
? review  No allowed reviewer has approved this candidate

Evidence window: 23:24:34–23:24:39 UTC (2026-09-08)
Trust: local collector and store; receipt is not authenticated
Details: sykli assess .sykli/evidence/8037ad3f… --requirements /trusted/sykli-review.json --why review
```

Every inspection appends a new bundle and never rewrites an earlier one. If the
pull request's head changed, the new bundle describes a new candidate. Run the
same evaluation later, offline, from the saved bundle:

```sh
sykli assess .sykli/evidence/8037ad3f… --requirements /trusted/sykli-review.json
sykli assess .sykli/evidence/8037ad3f… --requirements /trusted/sykli-review.json --why review
sykli assess .sykli/evidence/8037ad3f… --requirements /trusted/sykli-review.json --graph mermaid
sykli assess .sykli/evidence/8037ad3f… --requirements /trusted/sykli-review.json --json
```

`assess` evaluates at the collection's end by default and says so. `--at TIME`
(UTC, `YYYY-MM-DDTHH:MM:SSZ`) evaluates at another moment; a time past the
collection end plus `max_observation_age_seconds` makes every obligation
unproven with reason `stale`. A time before the collection started is an error.
A five-minute allowance is not proof that nothing changed within it: a consumer
that acts must reacquire state and enforce its own preconditions.

### Exit codes

| Code | Meaning |
|---|---|
| 0 | every requirement established (or, without requirements, observations saved) |
| 1 | a requirement is refuted |
| 2 | invalid requirements, unreadable candidate or bundle, tool failure |
| 3 | a requirement is unproven |
| 4 | admitted evidence about one subject conflicts |

Aggregation: conflict if any obligation conflicts; otherwise refuted if any is
refuted; otherwise established only if all are satisfied; otherwise unproven.

### What the two predicates mean

**workflow-reported-success.** Among the runs GitHub lists for the candidate's
head commit, keep those of the named workflow, in the base repository, from the
head repository, for the `pull_request` event, and associated with this pull
request. Select the greatest run number, then its latest attempt. A completed
`success` satisfies; `failure` refutes; anything else (in progress, cancelled,
skipped, timed out, action required) is unproven with the provider's outcome
retained. Older runs cannot hide a newer unresolved one. Runs that do not match
are kept as exclusions with a reason (`wrong-commit`, `wrong-head-repository`,
`wrong-workflow`, `association-missing`, `superseded`, …). An incomplete listing
(GitHub caps filtered listings at 1000) is `selection-incomplete`, never a
convenient green.

Provider success does not establish which jobs or tests ran, nor that the
candidate tree was the executed checkout. That is a later predicate.

**candidate-approval.** For each allowed account, take its latest decisive
review (approved, changes requested, or dismissed) by submission time. Comments
and pending reviews are not decisive. An approval counts only on this exact head
commit and not from the author when `exclude_pr_author` is set. A dismissed
latest record contributes nothing and does not resurrect an earlier approval. An
active change request from any allowed account refutes. Ambiguous ordering or an
unknown review state is unproven.

### Gaps and races

Sykli reads the pull request before and after acquisition, and lists runs and
reviews twice. Observed change becomes a gap (`candidate-moved`, `race`), which
makes the affected obligations unproven. This detects change; it is not an
atomic snapshot. Provider errors, timeouts, and page caps are gaps too, with the
HTTP status in the bundle's private diagnostics file.

## Bundles

`.sykli/evidence/<collection-id>/` (or `--store DIR`):

```text
manifest.json        sykli-collection.v1: endpoints, statuses, object digests, interval, gaps
objects/<sha256>     raw response bodies, content-addressed
diagnostics.json     per-request status, request IDs, sanitized stderr; opened explicitly
requirements/<id>.json, requests/<id>.json, assessments/<id>.json   saved by assess
```

`assess` saves its records after printing the verdict and only warns on stderr
if the bundle is read-only, so an archived bundle is still assessable. The
collection ID is the manifest's domain-separated SHA-256. Publication writes
into a private directory and renames it into place, so a torn write is never a
bundle. Loading verifies every object digest; a tampered object or a missing one
is a tool error, never completion. Raw responses may contain private repository
data: keep the store private and do not upload bundles.

A fresh worker with the bundle and the approved requirements reproduces the
assessment exactly. Every support and exclusion reference names the object and
JSON pointer it came from (`sha256:…#/workflow_runs/0`), so an explanation is
reproducible without a model.

## JSON

`--json` prints one document per invocation. Errors are `sykli-error.v1` with a
stable `code` on stdout.

- `sykli inspect --json`: `sykli-inspect.v1` with `kind` = `observations-only`
  (candidate, runs, reviews, workflows, coverage, gaps) or `assessment`
  (embedding the assessment below), plus `bundle` and `collection`.
- `sykli assess --json`: `sykli-assessment.v1`. Fields: `request`, `collection`,
  `requirements` (all `sha256:…`), `candidate`, `result`, `obligations` (each with
  `result`, `reason`, `support`, `counterevidence`, `excluded`, `missing`,
  `rule`), `evaluated_at`, `evaluation_basis`, `collection_interval`,
  `evaluator`, `coverage`, `gaps`, `limitations`, and always
  `trust: trusted-local-collector-and-store`, `authenticity: not-established`,
  `mode: advisory`.
- `sykli assess --why ID --json`: `sykli-why.v1` with one obligation.

## What this release does not do

No merge or deployment authority. No signing, no independent issuer, no
adversarially secure acceptance. No other hosts or CI providers. No job- or
test-level inference from workflow names. No servers, webhooks, polling, or
provider mutations. A local production receipt remains local execution evidence
and is never treated as independent CI evidence.

The design and its trust argument are in
[standalone-ci-evidence.md](standalone-ci-evidence.md).
