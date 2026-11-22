#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Regenerate the raster icons from the scalable app icon (same set as Tributary and
# Balun): hicolor PNGs 16–512 (Linux packages), data/transom.iconset (macOS .icns,
# built by iconutil in scripts/build-macos.sh), data/transom.ico (Windows exe
# resource and installer) and data/transom.png. Needs ImageMagick with librsvg.
set -euo pipefail
cd "$(dirname "$0")/.."
SVG=data/icons/hicolor/scalable/apps/io.github.jm2.Transom.svg
render() { magick -background none -density 384 "$SVG" -resize "${1}x${1}" "PNG32:$2"; }
for s in 16 24 32 48 64 128 256 512; do
    mkdir -p "data/icons/hicolor/${s}x${s}/apps"
    render "$s" "data/icons/hicolor/${s}x${s}/apps/io.github.jm2.Transom.png"
done
rm -rf data/transom.iconset; mkdir -p data/transom.iconset
for s in 16 32 64 128 256 512; do
    render "$s" "data/transom.iconset/icon_${s}x${s}.png"
    render $((s * 2)) "data/transom.iconset/icon_${s}x${s}@2x.png"
done
render 512 data/transom.png
tmp=$(mktemp -d -p "${TMPDIR:-/var/tmp}")
for s in 16 24 32 48 64 256; do render "$s" "$tmp/$s.png"; done
magick "$tmp"/{16,24,32,48,64,256}.png data/transom.ico
rm -rf "$tmp"
echo "icons regenerated from $SVG"
