#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Build Transom and install it for the current user under ~/.local (binary, desktop
# entry, rdp:// and .rdp handler, icon, metainfo). `--uninstall` removes it again.
set -euo pipefail
cd "$(dirname "$0")/.."
ID=io.github.jm2.Transom
P="${XDG_DATA_HOME:-$HOME/.local/share}"
BIN="$HOME/.local/bin"
files=(
    "$BIN/transom"
    "$P/applications/$ID.desktop"
    "$P/mime/packages/$ID.xml"
    "$P/icons/hicolor/scalable/apps/$ID.svg"
    "$P/metainfo/$ID.metainfo.xml"
)
if [[ "${1:-}" == "--uninstall" ]]; then
    rm -fv "${files[@]}"
else
    cargo build --release -p transom
    install -Dm755 target/release/transom "${files[0]}"
    install -Dm644 data/$ID.desktop "${files[1]}"
    install -Dm644 data/$ID.mime.xml "${files[2]}"
    install -Dm644 data/icons/hicolor/scalable/apps/$ID.svg "${files[3]}"
    install -Dm644 data/$ID.metainfo.xml "${files[4]}"
    echo "installed; run 'transom' or open an rdp:// link"
fi
update-desktop-database -q "$P/applications" 2>/dev/null || true
update-mime-database "$P/mime" 2>/dev/null || true
gtk-update-icon-cache -q -t "$P/icons/hicolor" 2>/dev/null || true
