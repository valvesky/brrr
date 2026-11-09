#!/bin/sh
# Render the tray PNGs in assets/tray/ from the glyph SVGs in assets/.
# Needs resvg and ImageMagick 7 (magick).
#
# Sizes cover Windows' small icon (16 px) at every standard display scale from
# 100% to 400%, so the tray never has to stretch a smaller image. Keep them in
# step with SIZES in src/windows/icon.rs.
set -eu
cd "$(dirname "$0")/.."

sizes="16 20 24 28 32 36 40 48 56 64"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

for glyph in bolt yin-yang recycle; do
  for size in $sizes; do
    # Fit the height, centre on a square canvas, then paint every pixel the
    # glyph colour and keep only the anti-aliased alpha.
    resvg -h "$size" "assets/$glyph.svg" "$tmp/glyph.png"
    for colour in black white; do
      magick "$tmp/glyph.png" -background none -gravity center -extent "${size}x${size}" \
        -channel RGB -fill "$colour" -colorize 100 +channel \
        -strip -define png:color-type=6 -define png:bit-depth=8 \
        "assets/tray/$glyph-$colour-$size.png"
    done
  done
done
