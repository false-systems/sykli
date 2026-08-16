#!/bin/sh
# Delta EXPLAIN for a pull request: which declared tasks the changed files
# affect. Informational only — `sykli run` evaluates the whole graph, so a
# plan that cannot be computed never fails the gate.
set -eu

binary=${SYKLI_BINARY:?SYKLI_BINARY is required}
contract=${SYKLI_CONTRACT:?SYKLI_CONTRACT is required}
base=${SYKLI_BASE:-}
head=${SYKLI_HEAD:-}
output=${GITHUB_OUTPUT:-/dev/null}
summary=${GITHUB_STEP_SUMMARY:-/dev/null}

warn() { echo "::warning title=sykli plan::$1"; }

if [ -z "$base" ] || [ -z "$head" ]; then
  echo "no pull request base and head; skipping delta plan"
  exit 0
fi

for commit in "$base" "$head"; do
  if ! git rev-parse --verify --quiet "$commit^{commit}" >/dev/null 2>&1; then
    warn "commit $commit is not in this checkout; set 'fetch-depth: 0' on actions/checkout to enable the delta plan"
    exit 0
  fi
done

# --relative reports paths against this directory and drops anything outside
# it, matching how `sykli plan --changed` resolves them.
if ! changed=$(git diff --relative --name-only "$base...$head" 2>&1); then
  warn "git diff failed: $changed"
  exit 0
fi
if [ -z "$changed" ]; then
  echo "no files changed between $base and $head; skipping delta plan"
  exit 0
fi

# Accumulate one --changed flag per path. POSIX sh has no arrays; the
# positional parameters are the array, and a heredoc keeps the loop in this
# shell so `set --` survives it.
set --
while IFS= read -r file; do
  [ -n "$file" ] || continue
  set -- "$@" --changed "$file"
done <<CHANGED
$changed
CHANGED

if ! plan=$("$binary" plan "$contract" "$@" --json 2>&1); then
  warn "plan failed: $plan"
  exit 0
fi

files=$(printf '%s\n' "$changed" | wc -l | tr -d ' ')
if command -v jq >/dev/null 2>&1; then
  affected=$(printf '%s' "$plan" | jq -r '.tasks | join(", ")')
  count=$(printf '%s' "$plan" | jq -r '.tasks | length')
else
  warn "jq is not installed; reporting the raw plan"
  affected=$plan
  count="?"
fi
[ -n "$affected" ] || affected="(none)"

echo "affected=$affected" >>"$output"
{
  echo "### sykli plan"
  echo
  echo "\`$files\` changed file(s) affect \`$count\` task(s): \`$affected\`"
  echo
  echo "_Delta selection explains the graph; it does not shrink it. The run below evaluates every task._"
  echo
} >>"$summary"

echo "affected tasks: $affected"
