#!/bin/sh
# Resolve the sykli binary this run will use, and prove it runs. Either a
# checked release tarball (the normal path) or a build of the caller's own
# checkout (how sykli dogfoods this Action before a release exists).
set -eu

version=${SYKLI_VERSION:-}
action_path=${SYKLI_ACTION_PATH:?SYKLI_ACTION_PATH is required}
install_dir=${SYKLI_INSTALL_DIR:-${RUNNER_TEMP:?RUNNER_TEMP is required}/sykli-bin}
output=${GITHUB_OUTPUT:-/dev/null}

fail() {
  echo "::error title=sykli install::$1"
  exit 1
}

if [ -z "$version" ]; then
  fail "no version to resolve — pin the Action to a release tag (uses: false-systems/sykli@v0.1.0) or set the 'version' input"
fi

if [ "$version" = source ]; then
  command -v cargo >/dev/null 2>&1 ||
    fail "version 'source' needs a Rust toolchain on the runner"
  # Build outside the workspace: the shim's own artifacts must not land in the
  # tree the receipt describes, and the target directory is otherwise wherever
  # the caller's cargo config says it is.
  target=${CARGO_TARGET_DIR:-$install_dir/target}
  CARGO_TARGET_DIR="$target" cargo build --locked --quiet
  binary=$target/debug/sykli
  [ -f "$binary.exe" ] && binary=$binary.exe
else
  case "$version" in
    v[0-9]*) ;;
    *) fail "version '$version' is not a release tag; use vX.Y.Z, or 'source' to build the checkout" ;;
  esac
  SYKLI_INSTALL_DIR="$install_dir" sh "$action_path/install.sh" "$version"
  binary=$install_dir/sykli
  case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*) binary=$install_dir/sykli.exe ;;
  esac
fi

[ -x "$binary" ] || fail "no executable sykli at $binary"
"$binary" --version
echo "binary=$binary" >>"$output"
