#!/bin/sh
# Check the receipt against the tree and contract that produced it, and
# translate sykli's staged exit codes into GitHub annotations. This script
# never exits non-zero itself: it records the code so the summary and the
# receipt artifact are still produced, and the action's gate step decides.
set -eu

binary=${SYKLI_BINARY:?SYKLI_BINARY is required}
contract=${SYKLI_CONTRACT:?SYKLI_CONTRACT is required}
receipt=${SYKLI_RECEIPT:?SYKLI_RECEIPT is required}
# The commit the receipt must be a clean run of; 'none' skips that check.
commit=${SYKLI_COMMIT:-HEAD}
output=${GITHUB_OUTPUT:-/dev/null}
summary=${GITHUB_STEP_SUMMARY:-/dev/null}

if [ ! -s "$receipt" ]; then
  echo "code=2" >>"$output"
  echo "::error title=sykli verify::no receipt to verify — the run produced none"
  exit 0
fi

set +e
if [ "$commit" = none ]; then
  "$binary" verify "$receipt" --contract "$contract"
else
  "$binary" verify "$receipt" --contract "$contract" --commit "$commit"
fi
code=$?
set -e
echo "code=$code" >>"$output"

# Exit-code policy. Sykli's verify codes are stages: checks run in
# order and the first failing stage decides. This block is the only place the
# Action interprets them for a CI consumer — every non-zero code withholds the
# gate, and the hint names the action that clears it.
case "$code" in
  0)
    title="verified"
    hint="the receipt matches this tree and contract"
    level=notice
    ;;
  1)
    title="the work failed"
    hint="a task failed or its evidence is incomplete; see the task table below"
    level=error
    ;;
  2)
    title="cannot verify"
    hint="the receipt, contract, or git state is unreadable, or commit $commit is not in this checkout — this is a broken gate, not a failed one"
    level=error
    ;;
  3)
    title="receipt is stale"
    hint="the receipt is not a clean run of commit $commit as checked out now: on pull requests check out the head (actions/checkout with ref: github.event.pull_request.head.sha), not the merge commit; otherwise a task most likely wrote a file that is neither gitignored nor declared as an output"
    level=error
    ;;
  4)
    title="contract drifted"
    hint="the contract no longer matches sykli.lock; run 'sykli lock' and commit the result"
    level=error
    ;;
  *)
    title="unexpected verify code $code"
    hint="this sykli binary reported a code this Action does not know"
    level=error
    ;;
esac

echo "::${level} title=sykli verify::${title} — ${hint}"
{
  echo "### sykli verify"
  echo
  echo "\`exit $code\` — **${title}**. ${hint}"
  echo
} >>"$summary"
