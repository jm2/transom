#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Zero-copy H.264 check (ADR 0006), on this machine's GPU, without touching the host
# desktop: transom-view-demo in a headless mutter (container with the GPU render node
# and RPM Fusion's full libavcodec + VA-API driver) against the xrdp test server,
# which sends the whole desktop as AVC420 while the screen changes.
#   1. verify: every frame decoded on the GPU *and* by FreeRDP; the decoders must
#      agree on every pixel, the frames must be on screen as dmabuf textures, and the
#      rendering must match FreeRDP's (PSNR; the YCbCr→RGB is the GPU's).
#   2. fallback: GTK's GL renderer, which can't import these (tiled) buffers: the
#      session must fall back to software decoding and stay pixel-exact.
#   3. auto: hardware decoding only (no fallback taken).
# SCALE=2 MONITOR=2400x1500 scripts/hwdec-check.sh: the same at a HiDPI monitor scale.
# Needs scripts/test-servers.sh up and a VA-API GPU with H.264 decode.
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/lib/state.sh
RENDER=${RENDER:-/dev/dri/renderD128}
[[ -e $RENDER ]] || { echo "no GPU render node ($RENDER)" >&2; exit 2; }
[[ -f $STATE/xrdp.pw ]] || { echo "run scripts/test-servers.sh up first" >&2; exit 2; }
cargo build -q -p transom-view --example transom-view-demo
podman build -q -t transom-test-hwdec tests/servers/hwdec >/dev/null
xfp=$(podman logs transom-xrdp 2>&1 | sed -n 's/^TLS fingerprint: //p' | tail -1)
install -m 600 "$STATE/xrdp.pw" "$STATE/target.pw"

motion() { # change the whole xrdp desktop for ~3 s (the encoder sends AVC420 frames)
    podman exec -u rdp transom-xrdp bash -c 'export DISPLAY=:10 XAUTHORITY=$HOME/.Xauthority
        for i in $(seq 30); do xsetroot -solid "#$(printf %02x%02x%02x $((i*7%256)) $((i*3%256)) $((i*11%256)))"; sleep 0.1; done'
}
demo() { # NAME extra-env… -- demo args… ; log in $STATE/hwdec-NAME.log
    local name=$1; shift; local env=()
    while [[ $1 != -- ]]; do env+=(-e "$1"); shift; done; shift
    podman run --rm --network host --userns=keep-id --device "$RENDER" \
        -e TRANSOM_CHECK_DEBUG -e SCALE="${SCALE:-1}" -e MONITOR="${MONITOR:-1600x1000}" -e RUST_LOG=transom_view=info,transom_view_freerdp=info "${env[@]}" \
        -v "$PWD/target/debug:/demo:ro,Z" -v "$PWD/tests/servers/view-check/inner.sh:/inner.sh:ro,Z" \
        -v "$STATE:/state:Z" --entrypoint /inner.sh transom-test-hwdec \
        localhost:13390 rdp --password-file /state/target.pw --trust "$xfp" --size 1280x800 "$@" \
        > "$STATE/hwdec-$name.log" 2>&1 &
    local pid=$!
    sleep 6; motion; wait "$pid" || true
    sed 's/\x1b\[[0-9;]*m//g' "$STATE/hwdec-$name.log" |
        grep -E 'HWDEC VERIFY|^check: (PSNR|scale)|can.t show|hardware H.264 off' || true
}
ok=1
field() { sed -n "s/.*$2=\([0-9]*\).*/\1/p" "$STATE/hwdec-$1.log" | tail -1; }

echo "--- 1. verify (GPU decode + FreeRDP decode, compared)"
demo verify -- --video-decode verify --check --check-psnr 30 --check-after-ms 9000 \
    --screenshot /state/hwdec-verify.png
frames=$(field verify frames); mism=$(field verify mismatched_px)
area=$(sed -n 's/.*video layers cover \([0-9]*\) px.*/\1/p' "$STATE/hwdec-verify.log" | tail -1)
if (( ${frames:-0} > 0 )) && [[ $mism == 0 ]] && (( ${area:-0} > 0 )) &&
        grep -q 'PSNR-OK' "$STATE/hwdec-verify.log" && ! grep -q "can't show" "$STATE/hwdec-verify.log"; then
    echo "verify: OK ($frames frames bit-exact vs FreeRDP; layer on screen: $area px)"
else
    echo "verify: FAILED"; ok=0
fi

echo "--- 2. fallback (GL renderer can't import the frames)"
demo fallback GSK_RENDERER=gl -- --video-decode auto --check --check-after-ms 9000
if grep -qE "can.t show hardware-decoded video" "$STATE/hwdec-fallback.log" &&
        grep -q 'PIXEL-EXACT' "$STATE/hwdec-fallback.log"; then
    echo "fallback: OK (software decoding took over, pixel-exact)"
else
    echo "fallback: FAILED"; ok=0
fi

echo "--- 3. auto (hardware only)"
demo auto -- --video-decode auto --quit-after-ms 12000
frames=$(field auto frames)
if (( ${frames:-0} > 0 )) && ! grep -qE "can't show|hardware H.264 off" "$STATE/hwdec-auto.log"; then
    echo "auto: OK ($frames frames decoded on the GPU, no fallback)"
else
    echo "auto: FAILED"; ok=0
fi
[[ $ok == 1 ]] && echo "HWDEC OK" || { echo "HWDEC FAILED"; exit 1; }
