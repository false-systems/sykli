<!-- toimija:adopt:v1:begin -->
Read `.toimija/current.md` before reading anything else in this repo; it is your workset contract. Under a toimija session your launch prompt names your live packet — that file supersedes this one.
<!-- toimija:adopt:v1:end -->

Product boundaries:

- Own graph parsing, validation, planning, execution, caching, receipts, and local typed production (ADR-0009).
- Production assessment establishes declared products and checks, not task closure or publication authority. External tools and workers remain optional consumers.
- Stay a local CLI: no server, network service, daemon, coordination, or agent execution. The one network use is on-demand, read-only acquisition of pull-request evidence through the operator's `gh` (`sykli inspect`, docs/inspect.md, ADR-0005 amendment); it never mutates a provider, triggers work, or runs candidate code. A bounded executor may finish its attempt after its initiating client exits.
- Candidate assessment (`sykli inspect` / `sykli assess`) establishes only the declared review-readiness predicates under a printed trust designation. It is advisory: no merge or deployment authority, no claim that GitHub's full merge policy is met.
- Container runtime, Toimija gate mode, and Ahti append wait until family repositories run v0 daily.
- Contract growth requires a named user and the re-entry conditions in docs/adr/0005-deletions.md.
- Treat `sykli plan --json` as the agent-facing query surface; before handoff run `toimija gates run sykli-full`.
