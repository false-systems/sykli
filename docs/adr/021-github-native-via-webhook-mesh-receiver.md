# ADR-021: GitHub-Native Integration via Webhook + Mesh Receiver

**Status:** Proposed
**Date:** 2026-04-27
**Supersedes:** ADR-004 (GitHub Integration via Commit Status API + run-inside-Actions)

---

## Context

ADR-004 (Accepted, 2024-12-03) chose the smallest viable GitHub integration for v1: Sykli runs *inside* a GitHub Actions runner, reads `GITHUB_TOKEN` from the runner environment, and reports per-task pass/fail via the Commit Status API. No GitHub App, no webhook receiver, no Checks API.

That decision was correct for v1. It got Sykli onto PRs without requiring users to register an app or expose an endpoint. But it has a structural problem: **Sykli doesn't replace GitHub Actions; it lives inside it.** The Actions runner is the execution authority. Actions YAML is required. The Actions log UI is where users still go to read failures. Sykli is a guest in someone else's CI.

For the v0.6+ thesis (ADR-020 — local-first CI for the next generation of developers), this is a contradiction. A local-first CI tool cannot have its execution authority owned by GitHub Actions. The audience we are targeting will not accept "you need a GitHub Actions runner to use Sykli."

This ADR replaces ADR-004 with a model where **GitHub fires events at the user's mesh, the mesh executes the run, and the mesh reports back via Checks API.** Actions is no longer in the picture.

---

## Decision

**Sykli ships a GitHub App. Users install it against their repos. One node in the user's mesh plays the `:webhook_receiver` role and exposes an HTTPS endpoint reachable from `api.github.com`. Push and PR events arrive at the receiver, the mesh executes the pipeline, and results are reported back via the Checks API using the App's installation token.**

### Topology

```
┌────────────────────────────────────────────────────────────────────┐
│                                                                    │
│  GitHub                                                            │
│    │                                                               │
│    │  webhook (push, pull_request, check_run)                      │
│    ▼                                                               │
│  ┌────────────────────────────────────────────────────────────┐    │
│  │                  USER'S MESH                                │    │
│  │  ┌──────────────┐    ┌──────────────┐    ┌──────────────┐  │    │
│  │  │  receiver    │───▶│  scheduler   │───▶│  worker      │  │    │
│  │  │  (HTTPS)     │    │  (placement) │    │  (executor)  │  │    │
│  │  │  :webhook_   │    │              │    │              │  │    │
│  │  │  receiver    │    │              │    │              │  │    │
│  │  └──────────────┘    └──────────────┘    └──────────────┘  │    │
│  │         │                                       │           │    │
│  │         └───────────── OTP cluster (peers) ─────┘           │    │
│  └────────────────────────────────────────────────────────────┘    │
│                          │                                         │
│                          │  Checks API (status, annotations)       │
│                          ▼                                         │
│  GitHub                                                            │
│                                                                    │
└────────────────────────────────────────────────────────────────────┘
```

The receiver is **not a service**. It is a node in the user's OTP cluster with a `:webhook_receiver` capability label. Placement (ADR-017), occurrence broadcast, fault tolerance, and graceful shutdown all apply to it for free.

### Why this is local-first

- The receiver runs on hardware the user controls (laptop, office NAS, VPS, Raspberry Pi, anything with an IP and an Erlang VM).
- Sykli (the project) operates zero servers in this model.
- No user data, no source code, no secrets ever transit Sykli-owned infrastructure.
- A user with no internet still has a working CI tool — the GitHub-native path is opt-in.
- Users pick their own exposure mechanism: public IP + TLS, Tailscale Funnel, Cloudflare Tunnel, ngrok, port-forward on their router. The receiver is HTTPS-only and signature-verified; the exposure layer is the user's choice.

### Why GitHub App, not OAuth or PAT

| Option | Token scope | UX | Verdict |
|---|---|---|---|
| **PAT (current ADR-004)** | User's full account | "Add this token to env" | Insecure. Wide blast radius. No installation model. |
| **OAuth App** | User-token, broad | "Authorize this app to act as you" | Acts *as* the user; not what we want. |
| **GitHub App** | Installation-scoped, narrow | "Install this app on these repos" | ✓ Right model. |

GitHub Apps give us:
- Installation-scoped tokens (rotate hourly, scoped to the installation's repos).
- A clean install UX (org admins click "Install" against specific repos).
- Webhook delivery with HMAC signatures.
- Checks API write access.
- No "act as a user" semantics — Sykli speaks as itself.

### Reporting: Checks API, not Commit Status API

Per-task statuses become **Check Runs**, not commit statuses. Why:

- Check Runs support **annotations** (file + line + message), which makes `sykli fix` output renderable inline on the PR diff. This is a major UX win.
- Check Runs support **structured output** (title, summary, text), which maps cleanly to the FALSE Protocol occurrence shape.
- Check Runs support **rerequesting** ("Re-run failed checks") natively.
- Commit statuses are a flat list with no annotations. They were the v1 compromise; they don't carry the data Sykli now produces.

The mapping:

| Sykli concept | Check Runs concept |
|---|---|
| Pipeline run | One *check suite* |
| Task | One *check run* within the suite |
| Task occurrence (FALSE Protocol) | Check run's `output.summary` (Markdown) + annotations |
| `sykli fix` analysis | Annotations on the offending file/line |

### Security

The receiver is the most exposed component in the mesh. Hard requirements:

- **HTTPS only.** No plaintext webhook endpoint.
- **Signature verification.** Every incoming webhook MUST be HMAC-verified against the App's webhook secret before any processing. Reject mismatches without logging the body.
- **Installation token rotation.** The receiver requests installation tokens on demand and caches them for ≤55 minutes (under the 1-hour expiry).
- **No persisted secrets in webhooks.** Webhook bodies are processed in memory; only the resolved run record is persisted (and that is masked via `SecretMasker`, per existing convention).
- **Replay protection.** Track `X-GitHub-Delivery` IDs in a bounded LRU; reject duplicates.
- **Rate limiting.** The receiver applies a per-installation rate limit before dispatching to the mesh. A spammed installation cannot starve other installations.

### Fallback: self-hosted Actions runner mode

ADR-004's "run inside Actions" model is *not* deleted. It remains as a documented fallback path for users who:

- Cannot expose any HTTPS endpoint to the public internet.
- Are on a corporate network with no Tailscale/Cloudflare Tunnel option.
- Want to evaluate Sykli without registering a GitHub App.

In this mode, Sykli still installs as a binary; the user runs it inside their Actions YAML; status reports go through the legacy Commit Status API path. This is documented as the *fallback*, not the headline.

---

## Consequences

### Net new components

- `Sykli.GitHub.App` — installation token management, JWT signing for App auth, webhook secret storage.
- `Sykli.GitHub.Webhook.Receiver` — HTTPS endpoint, signature verification, replay protection, dispatch into the mesh.
- `Sykli.GitHub.Checks` — Checks API client, suite/run mapping, annotation upload.
- `Sykli.Mesh.Roles` — capability label `:webhook_receiver` and placement rule (only one node per mesh holds this role at a time).

### Modified components

- `Sykli.Mesh.Transport` (ADR-013) gains a `:webhook` event type for receiver→scheduler dispatch.
- The existing `Sykli.SCM.GitHub` module (ADR-004 Commit Status path) is kept and marked as the fallback path. It becomes secondary to the Checks API path.
- The existing `.github/workflows/sykli-ci.yml` dogfooding pipeline is migrated to demonstrate both modes.

### Migration path

1. **0.6:** Receiver, App registration flow, Checks API client, mesh role. Documented as opt-in alongside the existing in-Actions path. Both modes work; in-Actions is still default.
2. **0.7:** GitHub-native (this ADR's path) becomes the documented default. In-Actions is moved to a "fallback" page.
3. **0.8+:** GitHub-native is the only path advertised in marketing. In-Actions remains supported but undocumented in the hero.

### Operational responsibilities

The user operates their own receiver. This means:

- They expose an endpoint (their choice of mechanism).
- They register the App against their repos (one-time install flow).
- They are responsible for the receiver's uptime — but if it goes down, GitHub auto-retries failed deliveries for up to 8 hours, and operators can manually redeliver any event within 30 days via the GitHub API. The mesh catches up on recovery.

Sykli (the project) is responsible for:

- Publishing and maintaining the GitHub App manifest.
- Documenting the install flow.
- Shipping the receiver code.
- Nothing else.

### What this enables for the user

- **`sykli fix` annotations on the PR diff.** The killer feature appears inline on the offending file:line, not just in terminal output.
- **Real Checks UI integration.** The PR's Checks tab shows per-task status with structured output, not just dots.
- **Re-run from GitHub UI.** Standard GitHub "re-run failed checks" works.
- **No Actions YAML required.** A user can install the App, push a `sykli.go` to their repo, and have CI working — without ever writing `.github/workflows/*.yml`.

---

## Open questions

- **Multi-mesh installations.** What happens if a user runs two separate meshes (e.g., one at home, one at work) and installs the App on the same repo from both? First-receiver-wins, or explicit installation-to-mesh binding?
- **Webhook ordering.** GitHub does not guarantee webhook delivery order. The receiver must reconcile out-of-order events (e.g., a `pull_request.synchronize` arriving before its preceding `push`). Likely solution: idempotent state derived from the commit SHA, not from event order. Worth a spike.
- **App marketplace listing.** Eventually the App should be listable on the GitHub Marketplace. Out of scope for v0.6.

---

## References

- **GitHub Apps** — <https://docs.github.com/en/apps>
- **Checks API** — <https://docs.github.com/en/rest/checks>
- **ADR-004** — superseded by this ADR
- **ADR-006** — Cluster peers (the receiver is just another peer)
- **ADR-013** — Mesh swarm design (the receiver uses existing transport)
- **ADR-017** — Task placement (the receiver is placed via existing rules)
- **ADR-020** — Positioning and visual direction (the local-first commitment that constrains this ADR)
