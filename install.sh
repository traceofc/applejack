#!/usr/bin/env bash
# Builds Applejack from source and installs it. Most people should use the one-line
# installer from the README instead (it downloads a ready-made build).
#
#   ./install.sh                     into ~/.local (no sudo)
#   ./install.sh --prefix /usr/local for every user (asks for sudo only to copy files)
#   ./install.sh --no-deps           don't check/install build dependencies
#   ./install.sh --update-engine     download a newer castLabs Electron first
set -euo pipefail
cd "$(dirname "$0")"

DEPS=1
PASS=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --prefix) PASS+=(--prefix "$2"); shift 2 ;;
    --prefix=*) PASS+=("$1"); shift ;;
    --no-deps) DEPS=0; shift ;;
    --update-engine) rm -rf node_modules/electron; shift ;;
    -h|--help) sed -n '2,9p' "$0"; exit 0 ;;
    *) echo "Unknown option: $1 (see --help)" >&2; exit 2 ;;
  esac
done
[[ $EUID -eq 0 ]] && { echo "Run without sudo: the build runs as your user, sudo is only asked for when copying." >&2; exit 1; }

missing_tools() {
  local m=()
  command -v git >/dev/null || m+=(git)
  command -v node >/dev/null && (( $(node -p 'process.versions.node.split(".")[0]') >= 22 )) || m+=("nodejs>=22")
  command -v npm >/dev/null || m+=(npm)
  command -v cargo >/dev/null || m+=("rust (cargo)")
  command -v cc >/dev/null || m+=("C compiler")
  command -v python3 >/dev/null || m+=(python3)
  if command -v pkg-config >/dev/null; then
    pkg-config --exists 'gtk4 >= 4.14' || m+=("gtk4>=4.14")
    pkg-config --exists 'libadwaita-1 >= 1.6' || m+=("libadwaita>=1.6")
    pkg-config --exists sqlite3 || m+=(sqlite)
  else
    m+=(pkg-config)
  fi
  (( ${#m[@]} )) && printf '%s\n' "${m[@]}"
  return 0
}

if (( DEPS )); then
  mapfile -t missing < <(missing_tools)
  if (( ${#missing[@]} )) && command -v pacman >/dev/null; then
    echo "Missing: ${missing[*]}"
    sudo pacman -S --needed git nodejs npm rust gtk4 libadwaita sqlite pkgconf base-devel python
    mapfile -t missing < <(missing_tools)
  fi
  if (( ${#missing[@]} )); then
    echo "Missing build dependencies: ${missing[*]}" >&2
    cat >&2 <<'HINT'
Install them and run ./install.sh again:
  Arch:          sudo pacman -S --needed git nodejs npm rust gtk4 libadwaita sqlite pkgconf base-devel python
  Fedora:        sudo dnf install git nodejs npm cargo gtk4-devel libadwaita-devel sqlite-devel gcc python3
  Debian/Ubuntu: sudo apt install git nodejs npm cargo libgtk-4-dev libadwaita-1-dev libsqlite3-dev build-essential python3
(Node.js 22+ and libadwaita 1.6+ are required.)
HINT
    exit 1
  fi
fi

bash scripts/package.sh
exec dist/applejack/install.sh "${PASS[@]}"
