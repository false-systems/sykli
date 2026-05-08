# Review Primitives

Review primitives are deterministic checks attached to `kind: "review"` graph
nodes. They are not shell-command tasks and they do not call LLM providers.

The executor treats review nodes as first-class graph nodes:

- The review node invokes the primitive named by `primitive`.
- The primitive returns a structured `review_result`.
- `passed` results pass the review node.
- `failed`, `unsupported`, and `errored` results fail the review node.
- Unsupported primitives fail explicitly; they are never silently skipped.

## `api_breakage`

`api_breakage` is the first review primitive boundary. It is intended for
deterministic public API compatibility checks.

The current implementation provides the runtime interface and structured result
model. Real language/tool adapters are intentionally not bundled yet. Until an
adapter is configured, `api_breakage` returns an explicit unsupported result:

```json
{
  "review_type": "api_breakage",
  "status": "unsupported",
  "severity": "warning",
  "message": "api_breakage review primitive has no configured adapter",
  "tool": null,
  "findings": [],
  "evidence": {
    "task": "review-api",
    "context": ["lib/**/*.ex"],
    "agent": "local"
  }
}
```

This is deliberate. Sykli should not pretend an API review passed when no
deterministic adapter evaluated it.

Future adapters may use tools such as API Extractor, griffe, gorelease/apidiff,
or cargo-public-api, but each adapter must return the same structured result
shape.
