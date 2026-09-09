#!/bin/sh
set -eu

tag=${1:-}
case "$tag" in
  v[0-9]* ) ;;
  * ) echo "usage: install.sh v<version>" >&2; exit 2 ;;
esac
case "$tag" in
  *[!A-Za-z0-9._-]* ) echo "invalid tag: $tag" >&2; exit 2 ;;
esac

case "$(uname -s)" in
  Linux) platform=linux ;;
  Darwin) platform=macos ;;
  MINGW*|MSYS*|CYGWIN*) platform=windows ;;
  *) echo "unsupported platform: $(uname -s)" >&2; exit 2 ;;
esac
case "$(uname -m)" in
  x86_64|amd64) arch=x86_64 ;;
  aarch64|arm64) arch=aarch64 ;;
  *) echo "unsupported architecture: $(uname -m)" >&2; exit 2 ;;
esac

repository=${SYKLI_REPOSITORY:-false-systems/sykli}
install_dir=${SYKLI_INSTALL_DIR:-"$HOME/.local/bin"}
binary=sykli
extension=tar.gz
if [ "$platform" = windows ]; then
  binary=sykli.exe
  extension=zip
fi
archive="sykli-$tag-$platform-$arch.$extension"
url="https://github.com/$repository/releases/download/$tag"
temporary=$(mktemp -d)
trap 'rm -rf "$temporary"' EXIT HUP INT TERM

curl -fsSLo "$temporary/$archive" "$url/$archive"
curl -fsSLo "$temporary/SHA256SUMS" "$url/SHA256SUMS"
(
  cd "$temporary"
  grep "  $archive\$" SHA256SUMS > "$archive.sha256"
  if [ "$platform" = macos ]; then
    shasum -a 256 -c "$archive.sha256"
  else
    sha256sum -c "$archive.sha256"
  fi
  if [ "$platform" = windows ]; then
    # Git for Windows' sh has no unzip; Windows itself has one.
    powershell.exe -NoProfile -Command "Expand-Archive -Force -LiteralPath '$(cygpath -w "$temporary/$archive")' -DestinationPath '$(cygpath -w "$temporary")'"
  else
    tar -xzf "$archive"
  fi
)
install -d "$install_dir"
install -m 0755 "$temporary/$binary" "$install_dir/$binary"
echo "installed sykli $tag to $install_dir/$binary"
