# Sykli for coding agents

Sykli answers two questions an agent otherwise guesses at: what work does
this change require, and what actually ran. Both answers are versioned JSON
(see [`spec.md`](spec.md)); neither is a claim the agent makes about itself.

## The order things happen

1. **Before editing, ask what applies.**

   ```bash
   sykli plan sykli.json --changed src/parser.rs --changed src/lexer.rs --json
   ```

   The plan lists the tasks whose declared inputs you are about to touch,
   plus everything downstream of them, in execution order. Run those, not the
   whole graph, while you work. No `--changed` means the whole graph.

2. **After editing, evaluate the graph.**

   ```bash
   sykli run sykli.json --json > receipt.json
   ```

   Progress and task output go to stderr; stdout is the receipt, also written
   under `.sykli/receipts/`. The exit code is 0 only for `passed` or
   `cached`. Read `tasks[].outcome`, `class`, and `retryable` before deciding
   anything: `errored` with `retryable: true` is the runtime, not your code.

3. **Hand over the receipt, not a sentence.** "Tests pass" is a claim. The
   receipt names the tree OID that ran, the contract hash, every command,
   every exit code, and the digest of every output. A reviewer who has never
   seen your session can check it.

4. **The reviewer verifies.**

   ```bash
   sykli verify receipt.json --contract sykli.json
   ```

   Exit 0 means the receipt matches this exact tree and this pinned contract.
   3 means the tree moved since the receipt: the work is not necessarily
   wrong, the evidence is stale, run again. 4 means the contract drifted:
   re-lock or ask why. 1 means the work is bad or incomplete. 2 means the
   reviewer could not verify at all. Branch on the code, never on the text.

## Cached is not observed

A task outcome of `cached` means sykli reused a record from an earlier
receipt whose inputs and runtime matched; nothing executed. That is correct
for a gate and wrong for evidence that something *was observed to happen*.
When a judge needs an observed outcome, run in a fresh worktree of the exact
tree so there is no `.sykli/cache` to hit:

```bash
git worktree add --detach /tmp/observe HEAD
(cd /tmp/observe && sykli run sykli.json --json) > receipt.json
git worktree remove --force /tmp/observe
```

The receipt's `subject.tree_oid` is the same as in the repository, because
it addresses content, not location.

## What sykli will not do for you

It does not know what the tasks mean, does not retry, does not decide whether
a failure matters, and does not talk to a server. Interpretation is the
reader's job; sykli's job is that the reader can trust what it reads.

## MCP

`sykli-mcp` exposes the same four commands as tools over stdio for clients
that prefer tool calls to a shell: `sykli_validate`, `sykli_plan`,
`sykli_run`, `sykli_verify`. Each tool returns the command's stdout as a text
block and sets `isError` when the command exited non-zero; `sykli_verify`
adds the exit code's meaning. It finds `sykli` through `SYKLI_BIN` or PATH
and runs in the directory it was spawned in. It is a shim beside the binary,
not a server inside it (ADR-0008).

```json
{ "mcpServers": { "sykli": { "command": "sykli-mcp" } } }
```

## A snippet for your repository's AGENTS.md

```markdown
## Verification
This repository declares its work graph in `sykli.json`.
- Before editing: `sykli plan sykli.json --changed <files> --json` tells you which tasks your change affects; run those.
- After editing: `sykli run sykli.json --json` and keep the receipt it prints.
- Report the receipt (its `subject.tree_oid` and `outcome`), never "tests pass".
- Reviewers run `sykli verify <receipt> --contract sykli.json`; exit 3 means stale, re-run; exit 4 means the contract drifted.
- A `cached` task was not observed; when evidence must be observed, run in a fresh `git worktree`.
```
