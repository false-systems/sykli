#!/bin/sh
# Render the receipt into the job summary and expose its subject as step
# outputs. Reporting only: a receipt is a claim about what ran, and this
# script never decides whether that claim is good enough.
set -eu

receipt=${SYKLI_RECEIPT:?SYKLI_RECEIPT is required}
output=${GITHUB_OUTPUT:-/dev/null}
summary=${GITHUB_STEP_SUMMARY:-/dev/null}

if [ ! -s "$receipt" ]; then
  {
    echo "### sykli run"
    echo
    echo "No receipt was produced — sykli could not evaluate the contract. See the step log."
    echo
  } >>"$summary"
  echo "outcome=errored" >>"$output"
  exit 0
fi

if ! command -v jq >/dev/null 2>&1; then
  echo "::warning title=sykli summary::jq is not installed; the receipt is attached but not rendered"
  echo "outcome=unknown" >>"$output"
  exit 0
fi

outcome=$(jq -r '.outcome' "$receipt")
tree_oid=$(jq -r '.subject.tree_oid' "$receipt")
contract_hash=$(jq -r '.contract_hash' "$receipt")
{
  echo "outcome=$outcome"
  echo "tree-oid=$tree_oid"
  echo "contract-hash=$contract_hash"
} >>"$output"

{
  jq -r '
    "### sykli run",
    "",
    "**outcome:** `\(.outcome)` · **contract:** `\(.contract_hash[0:12])` · **tree:** `\(.subject.tree_oid[0:12])`\(if .subject.dirty then " (dirty)" else " (clean)" end)",
    "",
    "| task | outcome | exit | duration | source |",
    "|---|---|---|---|---|",
    (.tasks[] | "| `\(.name)` | \(.outcome) | \(.exit_code // "—") | \(.duration_ms)ms | \(.source) |"),
    ""
  ' "$receipt"

  # Failing tasks carry their own evidence; show the tail of it rather than
  # making the reader open the artifact.
  jq -r '
    .tasks[]
    | select(.outcome != "passed" and .outcome != "cached")
    | "<details><summary>" + .name + " — " + .outcome + "</summary>",
      "",
      "```",
      ((.error // "no error recorded") + "\n\n" + ((.stderr + .stdout) | split("\n") | .[-40:] | join("\n"))),
      "```",
      "",
      "</details>",
      ""
  ' "$receipt"
} >>"$summary"

# One annotation per task that did not pass, so failures surface in the run
# view without opening the summary. Newlines are percent-encoded because a
# workflow command is a single line.
jq -r '
  .tasks[]
  | select(.outcome != "passed" and .outcome != "cached")
  | "::error title=sykli " + .name + "::" + ((.error // .outcome) | gsub("%";"%25") | gsub("\r";"%0D") | gsub("\n";"%0A"))
' "$receipt"
