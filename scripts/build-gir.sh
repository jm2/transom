#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Build libtransom-view-0.so and generate TransomView-0.gir, .typelib, the Vala
# binding and a pkg-config file into build/capi/ (PLAN Phase 5).
#   scripts/build-gir.sh [--debug]
set -euo pipefail
cd "$(dirname "$0")/.."
profile=release; dir=release
[[ "${1:-}" == "--debug" ]] && { profile=dev; dir=debug; }
cargo build -q --profile "$profile" -p transom-view-capi
out=build/capi
rm -rf "$out"; mkdir -p "$out/lib/girepository-1.0" "$out/share/gir-1.0" "$out/share/vala/vapi" "$out/include/transom-view-0" "$out/lib/pkgconfig"
cp "target/$dir/libtransom_view_capi.so" "$out/lib/libtransom-view-0.so.0"
ln -sf libtransom-view-0.so.0 "$out/lib/libtransom-view-0.so"
cp crates/transom-view-capi/include/transom-view.h "$out/include/transom-view-0/"
cat > "$out/lib/pkgconfig/transom-view-0.pc" <<PC
prefix=$(realpath "$out")
libdir=\${prefix}/lib
includedir=\${prefix}/include/transom-view-0

Name: transom-view
Description: HiDPI-correct GTK 4 RDP display widget
Version: $(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
Requires: gtk4
Libs: -L\${libdir} -ltransom-view-0
Cflags: -I\${includedir}
PC
export LD_LIBRARY_PATH="$PWD/$out/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
# g-ir-scanner runs a dumper that initialises GTK, which needs a display. Without
# one (CI, ssh), use a private headless mutter for the duration.
scanner_env=()
if [[ -z "${WAYLAND_DISPLAY:-}${DISPLAY:-}" ]]; then
    rt=$(mktemp -d -p "${TMPDIR:-/var/tmp}"); chmod 700 "$rt"
    scanner_env=(env XDG_RUNTIME_DIR="$rt" WAYLAND_DISPLAY=transom-gir GDK_BACKEND=wayland
                 GIO_USE_VFS=local GVFS_DISABLE_FUSE=1)
    "${scanner_env[@]}" dbus-run-session -- mutter --headless --no-x11 --wayland \
        --wayland-display=transom-gir --virtual-monitor 640x480 >/dev/null 2>&1 &
    mutter_pid=$!
    trap 'kill $mutter_pid 2>/dev/null; sleep 0.3; rm -rf "$rt" 2>/dev/null || true' EXIT
    for _ in $(seq 50); do [[ -S $rt/transom-gir ]] && break; sleep 0.1; done
fi
"${scanner_env[@]}" g-ir-scanner --quiet --warn-all \
    --namespace=TransomView --nsversion=0 \
    --identifier-prefix=Transom --symbol-prefix=transom \
    --include=Gtk-4.0 --pkg=gtk4 \
    --library=transom-view-0 --library-path="$out/lib" \
    --c-include=transom-view.h \
    --output="$out/share/gir-1.0/TransomView-0.gir" \
    "$out/include/transom-view-0/transom-view.h"
g-ir-compiler "$out/share/gir-1.0/TransomView-0.gir" -o "$out/lib/girepository-1.0/TransomView-0.typelib"
vapigen --quiet --library=transom-view-0 --pkg=gtk4 \
    --girdir="$out/share/gir-1.0" --directory="$out/share/vala/vapi" \
    "$out/share/gir-1.0/TransomView-0.gir"
echo "$out/:"; find "$out" -type f -o -type l | sort | sed 's/^/  /'
