#!/usr/bin/env bash
# ============================================================================
# Sykli Oracle — ground-truth validation against the live binary.
#
# Each numbered case encodes a system guarantee. When a case fails, it means
# Sykli's public contract is broken. The oracle never reads source code.
#
# Usage:
#   eval/oracle/run.sh                    # run all cases
#   eval/oracle/run.sh --case 001         # run one case
#   eval/oracle/run.sh --category pipeline # run a category
#   eval/oracle/run.sh --verbose          # show command output on failure
#
# Requires:
#   - sykli binary (SYKLI_BIN or core/sykli)
#   - jq
#   - Elixir (for SDK fixture execution)
# ============================================================================

set -euo pipefail

# --- Configuration ---

SYKLI_BIN="${SYKLI_BIN:-$(cd "$(dirname "$0")/../../core" && pwd)/sykli}"
ORACLE_DIR="$(cd "$(dirname "$0")" && pwd)"
FIXTURES_DIR="$ORACLE_DIR/fixtures"
VERBOSE="${VERBOSE:-false}"
CASE_FILTER=""
CATEGORY_FILTER=""

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
CYAN='\033[0;36m'
DIM='\033[2m'
BOLD='\033[1m'
RESET='\033[0m'

# Counters
PASSED=0
FAILED=0
SKIPPED=0
FAILURES=""

# --- Argument parsing ---

usage() {
  echo "Usage: eval/oracle/run.sh [--case 001] [--category pipeline] [--verbose] [--help]"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --help|-h) usage; exit 0 ;;
    --case)
      if [[ $# -lt 2 || "$2" == --* ]]; then echo "Error: --case requires an argument" >&2; exit 1; fi
      CASE_FILTER="$2"; shift 2 ;;
    --case=*) CASE_FILTER="${1#--case=}"; shift ;;
    --category)
      if [[ $# -lt 2 || "$2" == --* ]]; then echo "Error: --category requires an argument" >&2; exit 1; fi
      CATEGORY_FILTER="$2"; shift 2 ;;
    --category=*) CATEGORY_FILTER="${1#--category=}"; shift ;;
    --verbose) VERBOSE=true; shift ;;
    *) echo "Unknown arg: $1" >&2; usage; exit 1 ;;
  esac
done

# --- Helpers ---

tmp_workdir() {
  local dir
  dir=$(mktemp -d "${TMPDIR:-/tmp}/sykli-oracle-XXXXXX")
  echo "$dir"
}

# Create a minimal sykli.exs fixture in a temp dir
make_pipeline() {
  local dir="$1"
  local fixture="$2"
  cp "$FIXTURES_DIR/$fixture" "$dir/sykli.exs"
}

run_sykli() {
  local dir="$1"
  shift
  (cd "$dir" && "$SYKLI_BIN" "$@" 2>&1)
}

# Settlement: wait for async effects (file writes, etc.)
settle() {
  sleep "${1:-0.2}"
}

# --- Test framework ---

current_case=""
case_desc=""

begin_case() {
  current_case="$1"
  case_desc="$2"

  # Filter
  if [[ -n "$CASE_FILTER" && "$current_case" != "$CASE_FILTER" ]]; then
    return 1
  fi
  if [[ -n "$CATEGORY_FILTER" ]]; then
    local cat
    cat=$(echo "$case_desc" | cut -d: -f1 | tr '[:upper:]' '[:lower:]')
    if [[ "$cat" != "$CATEGORY_FILTER" ]]; then
      return 1
    fi
  fi
  return 0
}

pass() {
  printf "  ${GREEN}✓ %-8s${RESET} %s ${DIM}(%dms)${RESET}\n" "$current_case" "$case_desc" "$1"
  PASSED=$((PASSED + 1))
}

fail() {
  local reason="$1"
  printf "  ${RED}✗ %-8s${RESET} %s\n" "$current_case" "$case_desc"
  printf "    ${DIM}%s${RESET}\n" "$reason"
  FAILED=$((FAILED + 1))
  FAILURES="$FAILURES\n  ${RED}• $current_case: $reason${RESET}"
  if [[ "$VERBOSE" == "true" && -n "${LAST_OUTPUT:-}" ]]; then
    printf "    ${DIM}--- output ---${RESET}\n"
    echo "$LAST_OUTPUT" | head -20 | sed 's/^/    /'
    printf "    ${DIM}--- end ---${RESET}\n"
  fi
}

skip() {
  printf "  ${YELLOW}○ %-8s${RESET} %s ${DIM}(skipped: %s)${RESET}\n" "$current_case" "$case_desc" "$1"
  SKIPPED=$((SKIPPED + 1))
}

assert_exit() {
  local expected="$1" actual="$2" context="${3:-}"
  if [[ "$actual" -ne "$expected" ]]; then
    fail "exit code: expected=$expected actual=$actual${context:+ ($context)}"
    return 1
  fi
  return 0
}

assert_contains() {
  local haystack="$1" needle="$2" context="${3:-}"
  if ! echo "$haystack" | grep -qE "$needle"; then
    fail "expected output to contain '$needle'${context:+ ($context)}"
    return 1
  fi
  return 0
}

assert_json_field() {
  local json="$1" field="$2" expected="$3" context="${4:-}"
  local actual
  actual=$(echo "$json" | jq -r "$field" 2>/dev/null)
  if [[ "$actual" != "$expected" ]]; then
    fail "JSON $field: expected='$expected' actual='$actual'${context:+ ($context)}"
    return 1
  fi
  return 0
}

assert_file_exists() {
  local path="$1" context="${2:-}"
  if [[ ! -f "$path" ]]; then
    fail "file not found: $path${context:+ ($context)}"
    return 1
  fi
  return 0
}

assert_valid_json() {
  local path="$1" context="${2:-}"
  if ! jq empty "$path" 2>/dev/null; then
    fail "invalid JSON: $path${context:+ ($context)}"
    return 1
  fi
  return 0
}

# --- Pre-flight ---

if [[ ! -x "$SYKLI_BIN" ]]; then
  echo -e "${RED}Error: sykli binary not found at $SYKLI_BIN${RESET}"
  echo "Build it: cd core && mix escript.build"
  exit 1
fi

if ! command -v jq &>/dev/null; then
  echo -e "${RED}Error: jq not found${RESET}"
  exit 1
fi

printf "${BOLD}${CYAN}━━━ sykli oracle ━━━${RESET}\n"
printf "${DIM}Binary: $SYKLI_BIN${RESET}\n\n"

# ============================================================================
# PIPELINE EXECUTION (001-005)
# ============================================================================

printf "${BOLD}Pipeline Execution${RESET}\n"

# --- case_001: basic pipeline passes ---
if begin_case "001" "Pipeline: basic pass"; then
  dir=$(tmp_workdir)
  make_pipeline "$dir" "pass.exs"
  start_ms=$(($(date +%s) * 1000))
  LAST_OUTPUT=$(run_sykli "$dir" 2>&1) && exit_code=0 || exit_code=$?
  elapsed=$(( $(date +%s) * 1000 - start_ms ))
  if assert_exit 0 "$exit_code"; then
    pass "$elapsed"
  fi
  rm -rf "$dir"
fi

# --- case_002: failing task exits non-zero ---
if begin_case "002" "Pipeline: failing task exits non-zero"; then
  dir=$(tmp_workdir)
  make_pipeline "$dir" "fail.exs"
  LAST_OUTPUT=$(run_sykli "$dir" 2>&1) && exit_code=0 || exit_code=$?
  if assert_exit 1 "$exit_code"; then
    pass 0
  fi
  rm -rf "$dir"
fi

# --- case_003: occurrence.json written after run ---
if begin_case "003" "Pipeline: occurrence.json written"; then
  dir=$(tmp_workdir)
  make_pipeline "$dir" "pass.exs"
  run_sykli "$dir" >/dev/null 2>&1 || true
  settle
  if assert_file_exists "$dir/.sykli/occurrence.json"; then
    if assert_valid_json "$dir/.sykli/occurrence.json"; then
      json=$(cat "$dir/.sykli/occurrence.json")
      if assert_json_field "$json" ".protocol_version" "1.0" "occurrence protocol version"; then
        pass 0
      fi
    fi
  fi
  rm -rf "$dir"
fi

# --- case_004: validate command exits 0 for valid pipeline ---
if begin_case "004" "Pipeline: validate accepts valid pipeline"; then
  dir=$(tmp_workdir)
  make_pipeline "$dir" "pass.exs"
  LAST_OUTPUT=$(run_sykli "$dir" validate --json 2>&1) && exit_code=0 || exit_code=$?
  if assert_exit 0 "$exit_code"; then
    pass 0
  fi
  rm -rf "$dir"
fi

# --- case_005: multi-task DAG executes in correct order ---
if begin_case "005" "Pipeline: DAG dependency ordering"; then
  dir=$(tmp_workdir)
  make_pipeline "$dir" "dag.exs"
  LAST_OUTPUT=$(run_sykli "$dir" 2>&1) && exit_code=0 || exit_code=$?
  if assert_exit 0 "$exit_code"; then
    if assert_contains "$LAST_OUTPUT" "passed"; then
      pass 0
    fi
  fi
  rm -rf "$dir"
fi

# ============================================================================
# AI CONTEXT (006-010)
# ============================================================================

printf "\n${BOLD}AI Context${RESET}\n"

# --- case_006: occurrence has error block for failed run ---
if begin_case "006" "AI Context: error block for failed run"; then
  dir=$(tmp_workdir)
  make_pipeline "$dir" "fail.exs"
  run_sykli "$dir" >/dev/null 2>&1 || true
  settle
  if assert_file_exists "$dir/.sykli/occurrence.json"; then
    json=$(cat "$dir/.sykli/occurrence.json")
    error=$(echo "$json" | jq -r '.error' 2>/dev/null)
    if [[ "$error" != "null" && -n "$error" ]]; then
      pass 0
    else
      fail "occurrence.json missing error block for failed run"
    fi
  fi
  rm -rf "$dir"
fi

# --- case_007: occurrence has history block with steps ---
if begin_case "007" "AI Context: history block with steps"; then
  dir=$(tmp_workdir)
  make_pipeline "$dir" "pass.exs"
  run_sykli "$dir" >/dev/null 2>&1 || true
  settle
  if assert_file_exists "$dir/.sykli/occurrence.json"; then
    json=$(cat "$dir/.sykli/occurrence.json")
    steps=$(echo "$json" | jq '.history.steps | length' 2>/dev/null)
    if [[ "$steps" -gt 0 ]]; then
      pass 0
    else
      fail "occurrence.json history.steps is empty"
    fi
  fi
  rm -rf "$dir"
fi

# --- case_008: occurrence has git context ---
if begin_case "008" "AI Context: git context in occurrence"; then
  dir=$(tmp_workdir)
  # Initialize a git repo so git context is available
  (cd "$dir" && git init -q && git -c user.name="Oracle" -c user.email="oracle@test" commit --allow-empty -m "init" -q)
  make_pipeline "$dir" "pass.exs"
  run_sykli "$dir" >/dev/null 2>&1 || true
  settle
  if assert_file_exists "$dir/.sykli/occurrence.json"; then
    json=$(cat "$dir/.sykli/occurrence.json")
    sha=$(echo "$json" | jq -r '.data.git.sha' 2>/dev/null)
    if [[ -n "$sha" && "$sha" != "null" ]]; then
      pass 0
    else
      fail "occurrence.json missing data.git.sha"
    fi
  fi
  rm -rf "$dir"
fi

# --- case_009: explain command produces output ---
if begin_case "009" "AI Context: explain command works"; then
  dir=$(tmp_workdir)
  make_pipeline "$dir" "pass.exs"
  run_sykli "$dir" >/dev/null 2>&1 || true
  settle
  LAST_OUTPUT=$(run_sykli "$dir" explain 2>&1) && exit_code=0 || exit_code=$?
  if assert_exit 0 "$exit_code"; then
    if assert_contains "$LAST_OUTPUT" "passed|success|run"; then
      pass 0
    fi
  fi
  rm -rf "$dir"
fi

# --- case_010: context command generates context.json ---
if begin_case "010" "AI Context: context command writes context.json"; then
  dir=$(tmp_workdir)
  make_pipeline "$dir" "pass.exs"
  run_sykli "$dir" >/dev/null 2>&1 || true
  settle
  run_sykli "$dir" context >/dev/null 2>&1 || true
  settle
  if assert_file_exists "$dir/.sykli/context.json"; then
    if assert_valid_json "$dir/.sykli/context.json"; then
      pass 0
    fi
  fi
  rm -rf "$dir"
fi

# ============================================================================
# CACHING (011)
# ============================================================================

printf "\n${BOLD}Caching${RESET}\n"

# --- case_011: second run uses cache ---
if begin_case "011" "Caching: second run hits cache"; then
  dir=$(tmp_workdir)
  make_pipeline "$dir" "cached.exs"
  # First run
  run_sykli "$dir" >/dev/null 2>&1 || true
  settle
  # Second run — should cache
  LAST_OUTPUT=$(run_sykli "$dir" 2>&1) && exit_code=0 || exit_code=$?
  if assert_exit 0 "$exit_code"; then
    if assert_contains "$LAST_OUTPUT" "CACHED|cached"; then
      pass 0
    fi
  fi
  rm -rf "$dir"
fi

# ============================================================================
# SUPPLY CHAIN (014-016)
# ============================================================================

printf "\n${BOLD}Supply Chain${RESET}\n"

# --- case_014: attestation.json written for passing run with outputs ---
if begin_case "014" "Supply Chain: attestation.json for passing run"; then
  dir=$(tmp_workdir)
  make_pipeline "$dir" "with_outputs.exs"
  run_sykli "$dir" >/dev/null 2>&1 || true
  settle
  if assert_file_exists "$dir/.sykli/attestation.json"; then
    if assert_valid_json "$dir/.sykli/attestation.json"; then
      json=$(cat "$dir/.sykli/attestation.json")
      if assert_json_field "$json" ".payloadType" "application/vnd.in-toto+json" "DSSE payload type"; then
        pass 0
      fi
    fi
  fi
  rm -rf "$dir"
fi

# --- case_015: attestation payload is valid SLSA ---
if begin_case "015" "Supply Chain: SLSA v1 provenance in payload"; then
  dir=$(tmp_workdir)
  make_pipeline "$dir" "with_outputs.exs"
  run_sykli "$dir" >/dev/null 2>&1 || true
  settle
  if assert_file_exists "$dir/.sykli/attestation.json"; then
    # Decode the DSSE payload
    payload=$(cat "$dir/.sykli/attestation.json" | jq -r '.payload' 2>/dev/null)
    decoded=$(echo "$payload" | base64 --decode 2>/dev/null || echo "$payload" | base64 -D 2>/dev/null) || decoded=""
    if [[ -n "$decoded" ]]; then
      stmt_type=$(echo "$decoded" | jq -r '._type' 2>/dev/null)
      if [[ "$stmt_type" == "https://in-toto.io/Statement/v1" ]]; then
        pred_type=$(echo "$decoded" | jq -r '.predicateType' 2>/dev/null)
        if assert_json_field "$decoded" ".predicateType" "https://slsa.dev/provenance/v1" "SLSA predicate type"; then
          pass 0
        fi
      else
        fail "attestation _type is '$stmt_type', expected in-toto Statement v1"
      fi
    else
      fail "could not base64-decode attestation payload"
    fi
  fi
  rm -rf "$dir"
fi

# --- case_016: failed run still generates attestation ---
if begin_case "016" "Supply Chain: attestation for failed run"; then
  dir=$(tmp_workdir)
  make_pipeline "$dir" "fail.exs"
  run_sykli "$dir" >/dev/null 2>&1 || true
  settle
  if assert_file_exists "$dir/.sykli/attestation.json"; then
    if assert_valid_json "$dir/.sykli/attestation.json"; then
      pass 0
    fi
  fi
  rm -rf "$dir"
fi

# ============================================================================
# VALIDATION (017-019)
# ============================================================================

printf "\n${BOLD}Validation${RESET}\n"

# --- case_017: cycle detection ---
if begin_case "017" "Validation: cycle in DAG rejected"; then
  dir=$(tmp_workdir)
  make_pipeline "$dir" "cycle.exs"
  LAST_OUTPUT=$(run_sykli "$dir" validate 2>&1) && exit_code=0 || exit_code=$?
  if [[ "$exit_code" -ne 0 ]]; then
    if assert_contains "$LAST_OUTPUT" "cycle|circular|Cycle"; then
      pass 0
    fi
  else
    fail "validate should reject cyclic DAG"
  fi
  rm -rf "$dir"
fi

# --- case_018: no SDK file detected ---
if begin_case "018" "Validation: no SDK file exits with error"; then
  dir=$(tmp_workdir)
  LAST_OUTPUT=$(run_sykli "$dir" 2>&1) && exit_code=0 || exit_code=$?
  if [[ "$exit_code" -ne 0 ]]; then
    if assert_contains "$LAST_OUTPUT" "No sykli file|no_sdk_file|not found"; then
      pass 0
    fi
  else
    fail "should fail when no sykli.* file exists"
  fi
  rm -rf "$dir"
fi

# --- case_019: empty task graph ---
if begin_case "019" "Validation: empty task graph"; then
  dir=$(tmp_workdir)
  make_pipeline "$dir" "empty.exs"
  LAST_OUTPUT=$(run_sykli "$dir" validate --json 2>&1) && exit_code=0 || exit_code=$?
  # Empty graph should pass validation (valid JSON, zero tasks)
  if assert_exit 0 "$exit_code"; then
    pass 0
  fi
  rm -rf "$dir"
fi

# ============================================================================
# CLI COMMANDS (020-022)
# ============================================================================

printf "\n${BOLD}CLI Commands${RESET}\n"

# --- case_020: history command ---
if begin_case "020" "CLI: history command"; then
  dir=$(tmp_workdir)
  make_pipeline "$dir" "pass.exs"
  run_sykli "$dir" >/dev/null 2>&1 || true
  settle
  LAST_OUTPUT=$(run_sykli "$dir" history 2>&1) && exit_code=0 || exit_code=$?
  if assert_exit 0 "$exit_code"; then
    pass 0
  fi
  rm -rf "$dir"
fi

# --- case_021: graph command produces mermaid ---
if begin_case "021" "CLI: graph command produces mermaid"; then
  dir=$(tmp_workdir)
  make_pipeline "$dir" "dag.exs"
  LAST_OUTPUT=$(run_sykli "$dir" graph 2>&1) && exit_code=0 || exit_code=$?
  if assert_exit 0 "$exit_code"; then
    if assert_contains "$LAST_OUTPUT" "graph|flowchart|-->"; then
      pass 0
    fi
  fi
  rm -rf "$dir"
fi

# --- case_022: --help exits 0 ---
if begin_case "022" "CLI: --help exits 0"; then
  dir=$(tmp_workdir)
  LAST_OUTPUT=$(cd "$dir" && "$SYKLI_BIN" --help 2>&1) && exit_code=0 || exit_code=$?
  if assert_exit 0 "$exit_code"; then
    if assert_contains "$LAST_OUTPUT" "sykli|SYKLI|Usage|usage"; then
      pass 0
    fi
  fi
  rm -rf "$dir"
fi

# ============================================================================
# Summary
# ============================================================================

printf "\n${BOLD}━━━ Results ━━━${RESET}\n"
printf "  ${GREEN}Passed:  $PASSED${RESET}\n"
printf "  ${RED}Failed:  $FAILED${RESET}\n"
if [[ "$SKIPPED" -gt 0 ]]; then
  printf "  ${YELLOW}Skipped: $SKIPPED${RESET}\n"
fi
printf "  Total:   $((PASSED + FAILED + SKIPPED))\n"

if [[ "$FAILED" -gt 0 ]]; then
  printf "\n${BOLD}${RED}Failures:${RESET}"
  printf "$FAILURES\n"
  exit 1
fi
