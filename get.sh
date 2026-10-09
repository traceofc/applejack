#!/usr/bin/env bash
# One-line installer for Applejack:
#   curl -fsSL https://raw.githubusercontent.com/traceofc/applejack/main/get.sh | bash
# Downloads the latest release from GitHub and installs it into ~/.local (no sudo).
# Pass options after "bash -s --", e.g. "| bash -s -- --prefix /usr/local".
set -euo pipefail

REPO="${APPLEJACK_REPO:-traceofc/applejack}"
ARCH=$(uname -m)
[[ $ARCH == x86_64 ]] || { echo "Sorry, only x86_64 builds are published for now (you have $ARCH)." >&2; exit 1; }
for tool in curl tar; do
  command -v $tool >/dev/null || { echo "Please install $tool first." >&2; exit 1; }
done

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
if [[ -n ${APPLEJACK_TARBALL:-} ]]; then
  cp "$APPLEJACK_TARBALL" "$TMP/applejack.tar.gz"   # local build, for testing
else
  URL="https://github.com/$REPO/releases/latest/download/applejack-$ARCH.tar.gz"
  echo "Downloading $URL"
  curl -fL --progress-bar "$URL" -o "$TMP/applejack.tar.gz"
fi
tar -xzf "$TMP/applejack.tar.gz" -C "$TMP"
bash "$TMP/applejack/install.sh" "$@"
