#!/usr/bin/env bash
# Applejack @VERSION@ installer (x86_64). The app is packed inside this file.
#
#   ./Applejack-@VERSION@-x86_64.run                 install into ~/.local (no sudo)
#   ./Applejack-@VERSION@-x86_64.run --prefix /usr/local   install for every user
#   ./Applejack-@VERSION@-x86_64.run --uninstall     remove (add --purge to also forget sign-in)
#   ./Applejack-@VERSION@-x86_64.run --extract DIR   just unpack
set -euo pipefail

SELF=$(readlink -f "$0")
SKIP=$(awk '/^__APPLEJACK_PAYLOAD__$/ { print NR + 1; exit }' "$SELF")
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

case "${1:-}" in
  -h|--help) sed -n '2,8p' "$SELF"; exit 0 ;;
  --extract)
    DEST="${2:-applejack}"; mkdir -p "$DEST"
    tail -n +"$SKIP" "$SELF" | tar -xz -C "$DEST"
    echo "Unpacked into $DEST/applejack"; exit 0 ;;
esac

echo "Unpacking Applejack @VERSION@…"
tail -n +"$SKIP" "$SELF" | tar -xz -C "$TMP"
if [[ "${1:-}" == --uninstall ]]; then
  shift
  bash "$TMP/applejack/uninstall.sh" "$@"
else
  bash "$TMP/applejack/install.sh" "$@"
fi
exit 0
__APPLEJACK_PAYLOAD__
