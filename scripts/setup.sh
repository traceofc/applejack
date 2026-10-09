#!/usr/bin/env bash
# Installs castLabs Electron (Widevine CDM, "+wvcus" build) and the project deps.
#   scripts/setup.sh                 latest stable +wvcus tag
#   scripts/setup.sh --prerelease    allow alpha/beta tags
#   scripts/setup.sh --tag v44.5.1+wvcus
set -euo pipefail
cd "$(dirname "$0")/.."

REPO=https://github.com/castlabs/electron-releases
TAG=""
PRE=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --tag) TAG="$2"; shift 2 ;;
    --prerelease) PRE=1; shift ;;
    *) echo "unknown option $1" >&2; exit 2 ;;
  esac
done

if ! command -v node >/dev/null || ! command -v npm >/dev/null; then
  echo "Node.js >= 22 and npm are required. On Arch: sudo pacman -S nodejs npm" >&2
  exit 1
fi
NODE_MAJOR=$(node -p 'process.versions.node.split(".")[0]')
if (( NODE_MAJOR < 22 )); then
  echo "Node.js >= 22 required (found $(node -v))" >&2
  exit 1
fi

if [[ -z "$TAG" ]]; then
  if (( PRE )); then PATTERN='v[0-9][^^]*\+wvcus$'; else PATTERN='v[0-9.]+\+wvcus$'; fi
  TAG=$(git ls-remote --tags "$REPO" | sed 's#.*refs/tags/##' | grep -E "^$PATTERN" | sort -V | tail -1)
  [[ -n "$TAG" ]] || { echo "could not resolve a +wvcus tag from $REPO" >&2; exit 1; }
fi
echo "castLabs Electron: $TAG"

# npm >= 12 refuses git dependencies by default, so use the tag tarball instead.
npm pkg set "dependencies.electron=$REPO/archive/refs/tags/${TAG/+/%2B}.tar.gz"
npm install
# npm >= 12 skips install scripts; fetch the Electron binary explicitly.
node node_modules/electron/install.js
echo
echo "Done. Try:  npm run cli"
