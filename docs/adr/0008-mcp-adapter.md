# ADR-0008: The MCP shim lives beside the binary

Status: accepted 2026-09-05; retired 2026-09-09. The shim was removed before
the first release (0.6.0). ADR-0005's re-entry condition for anything MCP-shaped
still applies; this record stays as the design if that condition is ever met.

## Context

Agent harnesses increasingly speak the Model Context Protocol: tools offered
over stdio as JSON-RPC, called instead of shelled out to. ADR-0005 records
"MCP server" as deleted, with the re-entry condition "an agent harness in
family use that cannot shell out". No such harness exists: every agent in
family use runs commands. The condition is unmet, so nothing MCP-shaped may
enter the `sykli` binary.

ADR-0005 also records the pattern for what may exist instead: "triggers are
external shims; a dumb Action invokes `sykli`. Shims live beside it." The
GitHub Action is that precedent.

## Decision

`sykli-mcp` is a separate workspace crate (`mcp/`), a separate binary, and
never a default member of the workspace. It is a shim in the exact sense the
Action is:

- it runs the `sykli` on PATH (or `SYKLI_BIN`) for every tool call, with the
  same arguments a human would type, and returns that command's stdout and
  exit status; it links no sykli code and has no second implementation of
  anything;
- it speaks JSON-RPC on the stdin/stdout of the one client that spawned it,
  opens no socket, and exits when stdin closes; it is not a server in the
  sense ADR-0005 deletes — no lifetime beyond its client, no port, no state
  between calls;
- its dependency list is `serde_json` and the standard library.

The `sykli` binary is untouched. The re-entry condition for an in-binary
server remains unmet, and this ADR does not claim it.

## Consequences

- Any tool the shim offers is a CLI subcommand with a `--json` surface
  first; the shim cannot expose what the CLI does not.
- When a family harness cannot shell out, the question of an in-binary
  server reopens under ADR-0005 with that harness named; this shim does not
  settle it.
- The shim is released beside `sykli` but is not required by anything; a
  repository that never installs it loses nothing.
