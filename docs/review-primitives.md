# Review Primitives

Review primitives are deterministic checks attached to `kind: "review"` graph
nodes. They are not shell-command tasks and they do not call LLM providers.

The executor treats review nodes as first-class graph nodes:

- The review node invokes the primitive named by `primitive`.
- The primitive returns a structured `review_result`.
- `passed` results pass the review node.
- `failed`, `unsupported`, and `errored` results fail the review node.
- Unsupported primitives fail explicitly; they are never silently skipped.

The canonical primitive name is `api_breakage`. Hyphenated aliases such as
`api-breakage` are not accepted by the review primitive dispatcher.

## `review_result` Shape

All review primitive providers return the same result shape:

| Field | Type | Semantics |
|-------|------|-----------|
| `review_type` | string | Primitive identifier, such as `api_breakage`. |
| `status` | enum | One of `passed`, `failed`, `unsupported`, or `errored`. |
| `severity` | enum or null | One of `info`, `warning`, `breaking`, or `critical` when set. |
| `message` | string | Human-readable summary of the result. |
| `tool` | string or null | Adapter/tool name when known. |
| `findings` | array | Structured primitive-specific findings. |
| `evidence` | object | Structured evidence with the canonical keys below. |

Canonical `evidence` keys:

- `task`: review node name.
- `context`: review node context array.
- `agent`: review node agent string or null.

Adapters may add primitive-specific evidence keys, but they must preserve the
canonical keys above so downstream tools can enumerate the common surface.

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
