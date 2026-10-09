#!/usr/bin/env bash
# Regenerates data/icons/hicolor/* from data/icons/applejack.svg (an apple filled with
# applejack brandy, stem and leaf drawn as a music note, on a dark barrel-brown ground).
# Needs ImageMagick 7 (magick), rsvg-convert and python3. The generated PNGs are committed,
# so install.sh does not need either tool.
set -euo pipefail
cd "$(dirname "$0")/.."
SVG=data/icons/applejack.svg
SRC=data/icons/applejack-source.png
OUT=data/icons/hicolor
ID=io.github.traceofc.Applejack
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

# Continuous-curvature squircle (superellipse |x|^n + |y|^n = 1, n = 5, close to the
# iOS icon shape), as an SVG path in a 1024 box, rasterised as the alpha mask.
python3 - "$TMP/mask.svg" <<'PY'
import math, sys
n, c, r, steps = 5.0, 512.0, 448.0, 720  # 448 radius: 64 px margin like GNOME app icons
pts = []
for i in range(steps):
    t = 2 * math.pi * i / steps
    ct, st = math.cos(t), math.sin(t)
    x = c + r * math.copysign(abs(ct) ** (2 / n), ct)
    y = c + r * math.copysign(abs(st) ** (2 / n), st)
    pts.append(f"{x:.2f},{y:.2f}")
open(sys.argv[1], "w").write(
    '<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024">'
    f'<path d="M{" L".join(pts)} Z" fill="#fff"/></svg>')
PY
rsvg-convert -w 1024 -h 1024 "$TMP/mask.svg" -o "$TMP/mask.png"
rsvg-convert -w 1000 -h 1000 "$SVG" -o "$SRC"

# Content square (1024 px): the artwork scaled into the squircle's box.
magick "$SRC" -filter Lanczos -resize 896x896 -gravity center -background none -extent 1024x1024 "$TMP/full-c.png"

shape() { # content -> squircle-masked icon with a soft drop shadow
  magick "$1" "$TMP/mask.png" -alpha off -compose CopyOpacity -composite "$TMP/shape.png"
  magick "$TMP/shape.png" \( +clone -background black -shadow 35x14+0+10 \) +swap \
    -background none -layers merge +repage -gravity center -extent 1024x1024 "$2"
}
shape "$TMP/full-c.png" "$TMP/full.png"

rm -rf "$OUT"
for s in 16 24 32 48 64 128 256 512; do
  src="$TMP/full.png"
  mkdir -p "$OUT/${s}x${s}/apps"
  magick "$src" -filter Lanczos -resize "${s}x${s}" -strip "PNG32:$OUT/${s}x${s}/apps/$ID.png"
done
echo "icons written to $OUT"
