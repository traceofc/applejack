#!/usr/bin/env bash
# Wraps the release bundle from scripts/package.sh into one self-extracting file:
#   dist/Applejack-<version>-x86_64.run
set -euo pipefail
cd "$(dirname "$0")/.."
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' ui/Cargo.toml | head -1)
ARCH=$(uname -m)
[[ -f "dist/applejack-$ARCH.tar.gz" ]] || bash scripts/package.sh
OUT="dist/Applejack-$VERSION-$ARCH.run"
sed "s/@VERSION@/$VERSION/g" packaging/run-header.sh > "$OUT"
cat "dist/applejack-$ARCH.tar.gz" >> "$OUT"
chmod +x "$OUT"
echo "$OUT ($(du -h "$OUT" | cut -f1))"
