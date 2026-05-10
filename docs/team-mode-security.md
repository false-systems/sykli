# Sykli Team Mode Security Model

## Status

Design document for Phase 0 of `docs/team-mode-roadmap.md`. Defines the
trust model and security defaults for the self-hosted coordinator and
the daemon-to-coordinator protocol. Normative for future implementation
PRs.

## Architecture sentence

> The daemon executes and records; the mesh dispatches inside trusted
> networks; the coordinator synchronizes team state across locations;
> `.sykli/` remains the local source of detailed evidence.

The security model below preserves that sentence under attack: the
coordinator must never become a remote-execution surface, and a daemon
must never accidentally sync data the operator did not consent to share.

## Threat posture

The coordinator runs on infrastructure the team operates. Daemons run on
laptops, servers, and Kubernetes pods owned by their respective operators.

Assumed adversaries:

- A network observer between any daemon and the coordinator.
- A coordinator compromise (e.g. a stolen Postgres dump).
- A leaked team token.
- A malicious member of the team with `member` role.
- A daemon impersonator who learns a `daemon_id`.

Assumed *not* in scope for v0:

- A nation-state actor with a valid CA-signed certificate for your
  coordinator's hostname.
- An attacker with persistent root on a developer laptop. (At that
  point, the local-first guarantee is already gone; secrets and logs
  on the laptop are the bigger problem.)
- Insider threats with `owner` role. Owners can do everything; they are
  trusted by the design.

## Hard rules

These rules are enforced by code in implementation phases and tested in
the conformance and black-box suites. They are not aspirational.

1. **The coordinator is self-hosted.** No SaaS. No hosted control plane
   from upstream Sykli.
2. **The network is opt-in.** Local-only mode requires no network. The
   coordinator becomes a participant only when the operator explicitly
   joins a daemon.
3. **The daemon connects outbound.** The coordinator never opens a
   socket to a daemon.
4. **TLS is required** for any non-local-development deployment. TLS
   certificate verification follows the existing Sykli HTTP TLS rules
   (`Sykli.HTTP.ssl_opts/1`): `verify_peer` plus hostname checking.
5. **Raw logs are not synced by default.**
6. **Artifacts are not uploaded by default.**
7. **Remote execution is off by default.** A daemon must declare
   `accepts_remote_work: true` to be assigned work originated by anyone
   else.
8. **Secrets are never synced by default.** Sykli's existing
   `SecretMasker` rules apply to anything the daemon emits to the
   coordinator. Env vars matching `_TOKEN`, `_SECRET`, `_KEY`,
   `_PASSWORD`, `_URL`, `_DSN`, `_URI`, `_CONN`, etc. are masked before
   transmission.
9. **Source code is not synced by default.**
10. **Full stdout/stderr is not synced by default.** The daemon may sync
    a short failure excerpt for human review (e.g. last 10 lines), but
    only when the operator's policy permits it.
11. **The audit log is mandatory.** Every state-changing API call writes
    at least one row in the coordinator's append-only audit log. The
    coordinator never deletes audit log rows.
12. **No remote shell.** There is no API to run a command on a daemon.
    There is no API to upload code to a daemon. There is no API to
    fetch arbitrary files from a daemon.

Any violation of the above is a security bug, not a feature request.

## Authentication

### v0 — team join tokens

The coordinator issues team-scoped tokens through `sykli team token create`.

The coordinator skeleton bootstraps with a single bearer token supplied by
`--token` or `SYKLI_COORDINATOR_TOKEN`. This is intentionally minimal and
does not implement RBAC, OIDC, or GitHub org mapping yet. Daemon join and
heartbeat requests use the same bearer-token boundary. Every non-health
`/v1/*` endpoint rejects missing or incorrect bearer tokens.

The skeleton binds to `127.0.0.1` by default. Exposing it on `0.0.0.0` or
another non-loopback address requires an explicit `--bind` value and should
be paired with TLS termination at an ingress or proxy.

Properties:

- Bearer tokens, opaque, ≥ 256 bits of entropy.
- Issued per (org, team) pair.
- Carry no execution authority over individual daemons.
- Revocable at any time. Revocation invalidates all sessions on the
  next heartbeat.
- Supplied to daemons via `SYKLI_TEAM_TOKEN` (env var) or a one-shot
  `--token` flag. The current daemon session file stores coordinator
  URL, org/team, `session_id`, policy, labels, and capabilities, but does
  not persist the bearer token.
- Reused by team work CLI calls through `SYKLI_TEAM_TOKEN` after
  `sykli daemon join`. The token is not printed in normal or JSON
  command output.

Tokens are not user identities. They authenticate a daemon's right to
participate in a team. Member identities (who claimed a work item, who
approved a gate) are separately recorded based on the originating CLI
or MCP call.

Team work commands are explicit. Local work commands remain the default,
and `sykli work ... --team <team>` fails rather than falling back to
local state when no coordinator session exists, the requested team does
not match the joined session, the token is missing, or authorization
fails.

### Future — OIDC and GitHub org mapping

Out of scope for v0. The design admits a future where:

- Members authenticate via OIDC (Google Workspace, GitHub OIDC, etc.).
- Team membership maps automatically from a GitHub org or an SSO group.
- Tokens are short-lived and refreshed against an identity provider.

That is not part of this PR. The v0 token model must not preclude it.

## Authorization

Three roles in v0: `owner`, `member`, `approver`.

Permissions:

| Action                                | owner | member | approver |
|---------------------------------------|:-----:|:------:|:--------:|
| Create org                            | ✓     | —      | —        |
| Create team                           | ✓     | —      | —        |
| Mint team tokens                      | ✓     | —      | —        |
| Add/remove members                    | ✓     | —      | —        |
| Create work items                     | ✓     | ✓      | ✓        |
| Claim/assign work items               | ✓     | ✓      | ✓        |
| Append work notes                     | ✓     | ✓      | ✓        |
| Create runs from a daemon             | ✓ (as daemon) | ✓ (as daemon) | ✓ (as daemon) |
| Approve gates                         | ✓     | —      | ✓        |
| Reject gates                          | ✓     | —      | ✓        |
| Read audit log                        | ✓     | own actions only | ✓ |

Per-resource ACLs are deferred. A team-scoped token implies access to
all work items inside that team. If a team needs finer-grained access,
they create more teams.

## LAN mesh vs coordinator: different trust models

This is the single most important distinction in Team Mode. The two
mechanisms exist for different trust postures and must not be conflated.

### LAN mesh

- Mechanism: BEAM/libcluster, Erlang distribution, shared cookie.
- Trust unit: the cluster. Every node holds the cookie. Every node can
  RPC into every other node.
- Network: trusted (LAN, VPN, K8s namespace, Tailscale tailnet treated
  as one trust domain).
- Authentication: Erlang cookie + reachability.
- Authorization: implicit. If you are in the mesh, you are in.
- Failure mode if compromised: full RCE across every mesh node.
- Appropriate for: internal CI clusters, trusted worker pools, K8s
  worker daemons inside one namespace.
- **Not** appropriate for: WAN, untrusted networks, cross-company work,
  or any setting where mutual RPC trust is not warranted.

### Coordinator

- Mechanism: HTTPS, JSON, bearer tokens.
- Trust unit: each daemon's connection to the coordinator. Daemons do
  **not** transitively trust each other.
- Network: untrusted by default. The coordinator may be reachable over
  the public internet through an Ingress.
- Authentication: TLS + team token.
- Authorization: explicit per role and per `accepts_remote_work` flag.
- Failure mode if compromised: the coordinator's database is exposed
  (work items, run summaries, evidence references). Daemons remain
  uncompromised; the coordinator cannot RCE into them.
- Appropriate for: remote teams, NAT, home networks, cloud workers
  outside the LAN, cross-org collaboration.

The coordinator is **not** a substitute for the mesh. The mesh is **not**
a substitute for the coordinator. Hybrid mode combines them as
documented in `docs/coordination-modes.md`.

### What this means in practice

- Do not give a non-Sykli machine the BEAM cookie just to "extend the
  mesh over the WAN." That breaks the mesh's trust model.
- Do not trust a coordinator just because it is reachable. The token
  determines what the coordinator may know.
- Do not let the coordinator open inbound connections to your daemon.
  If a future feature needs that, the design must be revisited; the
  current model says no.
- Do not rely on coordinator state being secret. Treat it as
  team-visible by definition.

## Data minimization

The coordinator's job is coordination. It is not a log warehouse, an
artifact registry, or a code mirror. The default sync set is:

Synced by default:

- Work item metadata (title, intent, status, assignment).
- Daemon labels, capabilities, heartbeat liveness.
- Run summaries (status, target, started/finished, error code).
- Per-node run state (kind, status, error code).
- Success criteria results (kind, status, message).
- Review result summaries (review type, status, severity, summary).
- Gate state (waiting, approved, rejected) and decision reason.
- Evidence references (URI + sha256 + visibility), never the bytes.
- Contract hashes and short summaries.

Not synced by default:

- Full stdout / stderr.
- Local logs.
- Source code.
- Raw artifacts.
- Environment variable dumps.
- Secrets.
- Full review findings (only the summary, unless explicitly opted in).

A team may opt in to richer sync via the `policy` block returned at
join time (see `docs/daemon-join-protocol.md`). Opting in must be a
deliberate per-team configuration; the coordinator must not flip these
defaults silently.

## Secret handling

- Secrets are not part of the contract synced to the coordinator. The
  coordinator stores `contract_hash` and `summary`. If a team opts to
  upload `raw_contract_json`, the daemon must run the existing
  `Sykli.Occurrence.SecretMasker` on it first.
- Environment variables are not synced. Capability advertisements
  enumerate runtime presence (`docker`, `shell`, `k8s`), not the
  configuration of those runtimes.
- Tokens for SCM integrations (`GITHUB_TOKEN`, `GITLAB_TOKEN`, etc.)
  remain on the daemon. The coordinator does not store them.
- TLS private keys, signing keys, OIDC client secrets, and database
  credentials live on the coordinator side only and are mounted from
  Kubernetes Secrets.

## Audit log

Mandatory. Append-only. Written for at least:

- Daemon join and rejoin.
- Token issue and revocation.
- Work item create, claim, assign, status change.
- Run start and terminal status.
- Gate request, approve, reject, expire.
- Evidence ref creation.
- Member add/remove and role change.

The audit log row carries the actor `(type, id)`, the action, the
subject `(type, id)`, a timestamp, and a `metadata_json` blob with
contextual data. The blob must already have been masked by the
`SecretMasker`; the audit log is no place to leak a token.

Operators may export the audit log. The coordinator must offer a paged
read endpoint behind owner-only authorization. Live streaming is
deferred.

## Replay and idempotency

- Webhooks (`POST /v1/...`) require idempotency keys for safe retry.
  The coordinator de-duplicates by key + actor + endpoint.
- Heartbeats are naturally idempotent.
- The daemon's outbox flush after a network outage must use the same
  idempotency keys it generated when the event was first attempted.

## Transport notes

- HTTPS only.
- TLS 1.2 minimum, TLS 1.3 preferred.
- Hostname verification mandatory.
- Certificate pinning is **not** required and **not** recommended for
  v0; operators rotate certificates with their normal infrastructure
  process.
- Compression-as-a-side-channel risks (BREACH, CRIME) are mitigated by
  TLS 1.3 and by avoiding HTTP-level compression on tokenful responses.
  The coordinator must not enable compression on responses that include
  bearer tokens or session ids.

## What is explicitly allowed

The model above does not forbid:

- A team operating multiple coordinators (one per environment, one per
  customer engagement, etc.). Each is its own trust domain.
- A daemon connecting to multiple coordinators simultaneously, if a
  future config supports it. The current join protocol supports one
  active coordinator session at a time per daemon process.
- Operating a coordinator behind an authenticating proxy (mTLS at the
  edge, identity-aware proxy, etc.). The protocol is HTTPS-shaped, so
  proxies fit.

## Cross-references

- `docs/coordination-modes.md`
- `docs/self-hosted-coordinator.md`
- `docs/daemon-join-protocol.md`
- `docs/local-state-plane.md`
- `docs/error-codes.md`
- `docs/false-protocol-schema.md`
