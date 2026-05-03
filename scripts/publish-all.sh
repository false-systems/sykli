#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="${1:-}"
DRY_RUN=0

shift || true
for arg in "$@"; do
  case "$arg" in
    --dry-run) DRY_RUN=1 ;;
    *) echo "[publish-all] error: unknown argument: $arg" >&2; exit 1 ;;
  esac
done

[[ -n "$VERSION" ]] || { echo "Usage: scripts/publish-all.sh <version> [--dry-run]" >&2; exit 1; }

echo "[publish-all] target version: $VERSION"
"$ROOT/scripts/check-version.sh"

if [[ "$DRY_RUN" -eq 0 ]]; then
  missing=0
  [[ -n "${CARGO_REGISTRY_TOKEN:-}" ]] || { echo "[publish-all] error: CARGO_REGISTRY_TOKEN is required" >&2; missing=1; }
  [[ -n "${NPM_TOKEN:-}" ]] || { echo "[publish-all] error: NPM_TOKEN is required" >&2; missing=1; }
  [[ -n "${PYPI_API_TOKEN:-}" || -n "${TWINE_PASSWORD:-}" ]] || { echo "[publish-all] error: PYPI_API_TOKEN or TWINE_PASSWORD is required" >&2; missing=1; }
  [[ -n "${HEX_API_KEY:-}" ]] || { echo "[publish-all] error: HEX_API_KEY is required" >&2; missing=1; }
  git remote get-url origin >/dev/null || { echo "[publish-all] error: git remote 'origin' is required for Go tag publishing" >&2; missing=1; }

  if [[ "$missing" -ne 0 ]]; then
    exit 1
  fi
fi

args=()
if [[ "$DRY_RUN" -eq 1 ]]; then
  args+=(--dry-run)
fi

"$ROOT/scripts/publish-go.sh" "$VERSION" "${args[@]}"
"$ROOT/scripts/publish-rust.sh" "$VERSION" "${args[@]}"
"$ROOT/scripts/publish-ts.sh" "$VERSION" "${args[@]}"
"$ROOT/scripts/publish-python.sh" "$VERSION" "${args[@]}"
"$ROOT/scripts/publish-elixir.sh" "$VERSION" "${args[@]}"
