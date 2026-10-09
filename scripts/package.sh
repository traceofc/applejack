#!/usr/bin/env bash
# Builds the release bundle that people download from GitHub Releases:
#   dist/applejack-x86_64.tar.gz  ->  applejack/{install.sh, uninstall.sh, app/, share/, licenses}
# Needs the build dependencies (Rust, Node.js >= 22, GTK4/libadwaita headers). Used by
# ./install.sh (build from source) and by .github/workflows/release.yml.
set -euo pipefail
cd "$(dirname "$0")/.."

APP_ID=io.github.traceofc.Applejack
ARCH=$(uname -m)
OUT=dist/applejack
rm -rf "$OUT" "dist/applejack-$ARCH.tar.gz"
mkdir -p "$OUT/app/engine" "$OUT/share/applications" "$OUT/licenses"

if [[ ! -x node_modules/electron/dist/electron ]]; then
  echo "==> Downloading the engine (castLabs Electron with Widevine)"
  bash scripts/setup.sh
fi

echo "==> Building the client (cargo build --release)"
cargo build --release --locked --manifest-path ui/Cargo.toml
CARGO_OUT=$(cargo metadata --manifest-path ui/Cargo.toml --format-version 1 --no-deps \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')

echo "==> Staging $OUT"
install -m755 "$CARGO_OUT/release/applejack" "$OUT/app/applejack"
cp engine/*.js engine/*.html "$OUT/app/engine/"
cp package.json "$OUT/app/"
cp -a --reflink=auto node_modules/electron/dist "$OUT/app/electron"
mv "$OUT/app/electron/electron" "$OUT/app/electron/applejack-engine"
mkdir -p "$OUT/share/icons"
cp -r data/icons/hicolor "$OUT/share/icons/"
cp "data/$APP_ID.desktop.in" "$OUT/share/applications/"
install -m755 packaging/install.sh "$OUT/install.sh"
install -m755 uninstall.sh "$OUT/uninstall.sh"
cp LICENSE "$OUT/LICENSE"
cp node_modules/electron/dist/LICENSE "$OUT/licenses/electron-LICENSE"
cp node_modules/electron/dist/LICENSES.chromium.html "$OUT/licenses/"

# Licenses of the Rust crates compiled into the client.
cargo metadata --manifest-path ui/Cargo.toml --format-version 1 --filter-platform "$ARCH-unknown-linux-gnu" \
  | python3 packaging/crate-licenses.py > "$OUT/licenses/rust-crates.txt"

echo "==> Packing dist/applejack-$ARCH.tar.gz"
tar -C dist -czf "dist/applejack-$ARCH.tar.gz" applejack
du -h "dist/applejack-$ARCH.tar.gz" | cut -f1
