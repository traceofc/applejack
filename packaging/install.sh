#!/usr/bin/env bash
# Installs Applejack from this unpacked release bundle.
#   ./install.sh                     into ~/.local (no sudo)
#   ./install.sh --prefix /usr/local for every user (asks for sudo only to copy files)
set -euo pipefail
cd "$(dirname "$0")"

APP_ID=io.github.traceofc.Applejack
PREFIX="$HOME/.local"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --prefix) PREFIX="$2"; shift 2 ;;
    --prefix=*) PREFIX="${1#*=}"; shift ;;
    -h|--help) sed -n '2,4p' "$0"; exit 0 ;;
    *) echo "Unknown option: $1 (see --help)" >&2; exit 2 ;;
  esac
done
[[ $EUID -eq 0 && $PREFIX == "$HOME/.local" ]] && { echo "Run without sudo (use --prefix /usr/local for a system-wide install)." >&2; exit 1; }

bold() { printf '\033[1m%s\033[0m\n' "$*"; }
die() { printf '\033[31mError:\033[0m %s\n' "$*" >&2; exit 1; }

# ---- runtime libraries ----------------------------------------------------------------
missing=$( { ldd app/applejack; ldd app/electron/applejack-engine; } 2>/dev/null | awk '/not found/ {print $1}' | sort -u)
too_old=""
if [[ -z $missing ]] && ! LD_BIND_NOW=1 app/applejack --help >/dev/null 2>&1; then
  too_old=1
fi
if [[ -n $missing || -n $too_old ]]; then
  [[ -n $missing ]] && { echo "Missing system libraries:"; sed 's/^/  /' <<<"$missing"; }
  [[ -n $too_old ]] && echo "Your GTK4 or libadwaita is too old (Applejack needs GTK 4.14+ and libadwaita 1.6+)."
  cat <<'HINT'

Install them with your package manager, then run this again:
  Arch / Manjaro:   sudo pacman -S --needed gtk4 libadwaita nss alsa-lib
  Fedora:           sudo dnf install gtk4 libadwaita nss alsa-lib
  Debian / Ubuntu:  sudo apt install libgtk-4-1 libadwaita-1-0 libnss3 libasound2t64
  openSUSE:         sudo zypper install libgtk-4-1 libadwaita-1-0 mozilla-nss alsa
HINT
  exit 1
fi

# ---- copy -----------------------------------------------------------------------------
if pgrep -x am-gtk >/dev/null || pgrep -x applejack >/dev/null; then
  die "close Applejack (or the old client) first, then run this again"
fi

LIB="$PREFIX/lib/applejack"
SHARE="$PREFIX/share"
mkdir -p "$PREFIX" 2>/dev/null || true
SUDO=""
[[ -w "$PREFIX" ]] || SUDO=sudo

bold "Installing Applejack into $PREFIX"
$SUDO rm -rf "$LIB"
$SUDO mkdir -p "$PREFIX/lib" "$PREFIX/bin" "$SHARE/applications" "$SHARE/icons"
$SUDO cp -R --preserve=mode,timestamps,links app "$LIB"
$SUDO install -m755 uninstall.sh "$LIB/uninstall.sh"
$SUDO mkdir -p "$SHARE/licenses/applejack"
$SUDO cp -R LICENSE licenses/. "$SHARE/licenses/applejack/"
$SUDO ln -sfn "$LIB/applejack" "$PREFIX/bin/applejack"
$SUDO cp -R share/icons/hicolor "$SHARE/icons/"
sed "s|@BIN@|$LIB/applejack|" "share/applications/$APP_ID.desktop.in" \
  | $SUDO tee "$SHARE/applications/$APP_ID.desktop" >/dev/null

# Shortcut of the pre-rename version.
rm -f "${XDG_DATA_HOME:-$HOME/.local/share}/applications/dev.amlinux.Music.desktop"

command -v gtk-update-icon-cache >/dev/null && $SUDO gtk-update-icon-cache -qtf "$SHARE/icons/hicolor" || true
command -v update-desktop-database >/dev/null && $SUDO update-desktop-database -q "$SHARE/applications" || true

echo
bold "Done! Open Applejack from your app menu and press Sign In."
case ":$PATH:" in *":$PREFIX/bin:"*) echo "From a terminal: applejack" ;; *) echo "From a terminal: $PREFIX/bin/applejack" ;; esac
echo "To uninstall: $LIB/uninstall.sh"
