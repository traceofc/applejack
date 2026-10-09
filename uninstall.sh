#!/usr/bin/env bash
# Removes Applejack.
#   uninstall.sh            keeps your sign-in and library cache
#   uninstall.sh --purge    removes them too
#   uninstall.sh --prefix /usr/local   if it was installed there (detected automatically
#                                      when run from <prefix>/lib/applejack/uninstall.sh)
set -euo pipefail

APP_ID=io.github.traceofc.Applejack
here=$(cd "$(dirname "$0")" && pwd)
if [[ $here == */lib/applejack ]]; then PREFIX=${here%/lib/applejack}; else PREFIX="$HOME/.local"; fi
PURGE=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --prefix) PREFIX="$2"; shift 2 ;;
    --prefix=*) PREFIX="${1#*=}"; shift ;;
    --purge) PURGE=1; shift ;;
    -h|--help) sed -n '2,6p' "$0"; exit 0 ;;
    *) echo "Unknown option: $1 (see --help)" >&2; exit 2 ;;
  esac
done

SUDO=""
[[ -w "$PREFIX/lib" || ! -e "$PREFIX/lib/applejack" ]] || SUDO=sudo
SHARE="$PREFIX/share"

pkill -f "^$PREFIX/lib/applejack/applejack$" 2>/dev/null || true
$SUDO rm -rf "$PREFIX/lib/applejack" "$SHARE/licenses/applejack"
$SUDO rm -f "$PREFIX/bin/applejack" "$SHARE/applications/$APP_ID.desktop"
$SUDO rm -f "$SHARE"/icons/hicolor/*/apps/"$APP_ID".png
command -v gtk-update-icon-cache >/dev/null && $SUDO gtk-update-icon-cache -qtf "$SHARE/icons/hicolor" || true
command -v update-desktop-database >/dev/null && $SUDO update-desktop-database -q "$SHARE/applications" || true

if (( PURGE )); then
  rm -rf "${XDG_CONFIG_HOME:-$HOME/.config}/applejack" "${XDG_CACHE_HOME:-$HOME/.cache}/applejack"
  echo "Applejack removed, together with your sign-in and cache."
else
  echo "Applejack removed. Sign-in and cache are kept in ~/.config/applejack and ~/.cache/applejack (use --purge to delete them)."
fi
