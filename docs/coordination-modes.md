# Sykli Coordination Modes

## Status

This is a design document for Sykli Team Mode. It defines the four
coordination shapes Sykli supports as it grows from a single-machine tool
into a team tool. Phase 0 of `docs/team-mode-roadmap.md`.

No implementation in this PR. The local-only and trusted LAN mesh modes
exist today. The self-hosted coordinator and hybrid modes are designed
here and implemented in later phases.

## Architecture sentence

> The daemon executes and records; the mesh dispatches inside trusted
> networks; the coordinator synchronizes team state across locations;
> `.sykli/` remains the local source of detailed evidence.

This sentence is normative. Every mode below is a particular composition of
those four actors. The coordinator never executes work. The mesh never
crosses trust domains. `.sykli/` is always the per-machine source of truth.

## Why four modes

Sykli is local-first. Most users start with a single laptop and a `sykli.exs`
file. Some users grow into a trusted worker pool inside one network. Some
teams need to coordinate work across people, agents, and machines on
different networks. Each of those is a different trust posture and a
different deployment shape.

We refuse to collapse them into one "always-on cluster" model because that
would force every user to operate a network. Local-first means the network
is opt-in.

The four modes are:

1. **Local-only.** No network. One machine. The default.
2. **Trusted LAN mesh.** BEAM/libcluster mesh inside a trust domain.
3. **Self-hosted coordinator.** Team-state plane. Daemons connect outbound.
4. **Hybrid.** Mesh inside trust domains, coordinator across them.

The remainder of this document defines each.

## Mode A — Local-only

The default and the floor of the system.

```bash
sykli run
```

What runs:

- The detected SDK pipeline emits JSON.
- The engine executes locally through the configured target (Local or K8s).
- `.sykli/` accumulates occurrences, attestations, run history, context,
  and per-task evidence.
- The local MCP server (`sykli mcp`) exposes tools to a co-located agent.

What does not run:

- No daemon-to-daemon network.
- No outbound connection to a coordinator.
- No reliance on shared cluster state.

Trust boundary:

- The user trusts their own machine. That is the entire trust domain.

When this mode is enough:

- Solo work.
- Air-gapped environments.
- Pre-team experimentation.
- The first run on any new machine.

## Mode B — Trusted LAN mesh

The existing BEAM mesh (`Sykli.Mesh`, `Sykli.Mesh.Roles`,
`Sykli.Mesh.Transport.*`).

```bash
# on each worker
SYKLI_LABELS=docker,gpu sykli daemon start --role worker

# on a developer machine in the same trust domain
sykli --mesh
```

What runs:

- Multiple BEAM nodes form a cluster via `libcluster` gossip.
- Roles are advertised through `Sykli.Mesh.Roles`.
- The dispatcher places tasks on workers by labels and capabilities.
- Erlang distribution carries task dispatch over the local network.

What this mode requires:

- A shared trust domain. Every node in the mesh must hold the same
  Erlang cookie and reach every other node over BEAM distribution
  (TCP, EPMD-style port allocation, or a configured port).
- A network where everyone is on equal footing — typically a LAN, a VPN,
  or a Kubernetes cluster.

What this mode is good for:

- Internal CI clusters.
- A trusted worker pool inside one office or one VPC.
- A Kubernetes namespace whose pods run Sykli workers.
- A homelab where every machine is on Tailscale and every Tailscale node
  is trusted to the same degree.

What this mode is **not** for:

- Two people on different home networks.
- A laptop behind NAT collaborating with a server in the cloud.
- Cross-company collaboration.
- Untrusted networks where one node should not be able to RPC into
  another node.

The BEAM mesh is not the WAN/team coordination mechanism. It is a
trusted-execution-domain mechanism. Treat it as a single trust unit; if you
would not give a node the Erlang cookie, it does not belong in the mesh.

## Mode C — Self-hosted coordinator

This is the new mode. Designed in `docs/self-hosted-coordinator.md`.

```bash
sykli daemon join \
  --coordinator https://sykli.internal \
  --org false-systems \
  --team platform \
  --token $SYKLI_TEAM_TOKEN \
  --labels macos,docker,typescript \
  --name yair-mbp
```

What runs:

- The user (or the org) operates a single self-hosted **Sykli Coordinator**
  service, deployed into their own infrastructure (typically a Kubernetes
  cluster they already run).
- Each daemon connects **outbound** to the coordinator over HTTPS.
- The coordinator stores team-coordination state: org/team registry,
  daemon sessions, work items, run summaries, gates, evidence references,
  audit log.
- Each daemon continues to execute locally and continues to write
  detailed evidence into its own `.sykli/`.

What this mode does **not** do:

- It does not execute work.
- It does not own logs, artifacts, or source code.
- It does not require inbound access to developer laptops.
- It does not assume a shared network.
- It does not require everyone to share an Erlang cookie.

Trust boundary:

- Each daemon trusts the coordinator at the level granted by its team
  token and policy.
- Daemons do not trust each other transitively. Two daemons connected to
  the same coordinator are not in the same execution trust domain.
- Remote execution between daemons is **off** by default. A daemon must
  explicitly declare `accepts_remote_work: true` before the coordinator
  is allowed to assign work to it on behalf of a different originator.

When this mode is right:

- Remote teams.
- Laptops behind NAT.
- Home networks.
- Cloud workers outside the LAN.
- Cross-company collaboration where each side operates its own coordinator.
- Anywhere you would otherwise reach for a SaaS dashboard.

The coordinator's job is to coordinate metadata and state. The daemon's
job remains to execute work where the user trusts it to run.

See:

- `docs/self-hosted-coordinator.md` — coordinator design.
- `docs/daemon-join-protocol.md` — outbound connection protocol.
- `docs/team-mode-security.md` — security defaults.
- `docs/local-state-plane.md` — what stays local versus what is synced.

## Mode D — Hybrid

The composition that real organizations end up with.

Shape:

- Developer laptops connect **only** to the coordinator. They do not
  participate in any BEAM mesh.
- Inside a Kubernetes cluster, a pool of Sykli worker daemons forms a
  trusted LAN mesh. They share an Erlang cookie and dispatch work among
  themselves.
- One node in the cluster mesh — or the daemon attached to the
  coordinator — is responsible for translating coordinator instructions
  into mesh dispatch decisions.
- The coordinator tracks work items, claims, run summaries, gate state,
  and evidence references across both the laptops and the cluster.

Concretely:

```text
              ┌──────────────────────┐
              │   Sykli Coordinator  │   self-hosted, HTTPS only
              │      (your K8s)      │
              └─────────┬────────────┘
                        │ outbound only
   ┌────────────────────┼─────────────────────┐
   │                    │                     │
   ▼                    ▼                     ▼
  laptop A             laptop B           K8s daemon (gateway)
  daemon                daemon                 │
                                               │ Erlang dist (LAN mesh)
                                       ┌───────┴───────┐
                                       ▼               ▼
                                     worker          worker
                                     daemon          daemon
```

Properties:

- The coordinator has **no** Erlang cookie. It speaks HTTPS only.
- The cluster mesh is one trust domain. The coordinator is a different
  trust domain. The two communicate through a designated daemon, not
  through Erlang distribution.
- Laptops never join the mesh. They never receive `:rpc.call` requests
  from anywhere.
- Coordinator-recorded run summaries can come from either a laptop daemon
  or a cluster gateway daemon. Both look the same to the coordinator.

This is the mode the design optimizes for. Solo and small teams stay in
modes A, B, or C. Production usage tends to be D.

## What changes per mode

| Capability                  | A: Local | B: LAN mesh | C: Coordinator | D: Hybrid |
|-----------------------------|:--------:|:-----------:|:--------------:|:---------:|
| Run pipelines               | ✓        | ✓           | ✓              | ✓         |
| `.sykli/` evidence          | ✓        | ✓           | ✓              | ✓         |
| Multi-node dispatch         | —        | ✓           | —              | ✓ (mesh)  |
| Cross-network coordination  | —        | —           | ✓              | ✓         |
| Shared work items           | —        | —           | ✓              | ✓         |
| Shared gate decisions       | —        | —           | ✓              | ✓         |
| Requires shared cookie      | —        | yes         | no             | mesh only |
| Inbound network to laptops  | —        | mesh-only   | no             | no        |

## Failure modes per layer

- Local-only failures stay in `.sykli/`. No external system is affected.
- Mesh failures (split brain, partial reachability, dropped Erlang
  distribution) degrade dispatch but do not delete history. The user
  falls back to local execution.
- Coordinator failures (the service is down, the network is broken,
  the token is rejected) must not stop a daemon from running locally.
  The daemon continues to write `.sykli/` and queues sync events for
  the coordinator's recovery. See `docs/daemon-join-protocol.md` for
  reconnect semantics.
- Hybrid failures degrade gracefully: if the coordinator is unreachable,
  the cluster mesh keeps executing; if the cluster mesh is unhealthy,
  laptops can still record work in the coordinator.

The design rule is: **no mode of failure removes a user's ability to run
locally.** Local-first is preserved at every layer.

## Non-goals across all modes

The following remain out of scope for every mode in this document:

- No SaaS hosted control plane.
- No mandatory upload of source code, full logs, raw artifacts, or
  secrets.
- No remote shell.
- No global scheduler that pre-empts a daemon's local execution.
- No web UI in this phase.
- No replacement of `.sykli/` as the local source of truth.

Future phases may expand individual modes, but they may not violate the
above without a new design doc.
