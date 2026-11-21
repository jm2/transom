#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Clipboard round trip against the grd test server: the client offers text and the
# in-session probe pastes it; then the probe copies text and the client receives it.
#   scripts/clipboard-check.sh      (needs scripts/test-servers.sh up)
set -euo pipefail
cd "$(dirname "$0")/.."
STATE="${TMPDIR:-/var/tmp}/transom-test"
podman build -q -t transom-test-grd tests/servers/grd-headless >/dev/null
cargo build -q -p transom-view-freerdp --example cliptest
fp=$(podman logs transom-grd 2>&1 | sed -n 's/^TLS fingerprint: //p' | tail -1)
podman exec transom-grd pkill -f input-probe || true
podman exec transom-grd bash -c 'rm -f /tmp/input-probe.log /tmp/probe-cmd'
probe_cmd() { podman exec transom-grd bash -c "printf '%s\n' \"\$1\" >> /tmp/probe-cmd" _ "$1"; }
target/debug/examples/cliptest localhost:13389 transom --password-file "$STATE/grd.pw" \
    --trust "$fp" --seconds 26 > "$STATE/cliptest.out" 2>&1 &
client=$!
sleep 2.5
podman cp tests/servers/grd-headless/probe/input-probe.py transom-grd:/tmp/input-probe.py
podman exec -d transom-grd bash -c '
    pid=$(pgrep -f "gnome-shell --headless" | head -1)
    export $(tr "\0" "\n" < /proc/$pid/environ | grep -E "^(DBUS_SESSION_BUS_ADDRESS|XDG_RUNTIME_DIR|WAYLAND_DISPLAY)=")
    export WAYLAND_DISPLAY=${WAYLAND_DISPLAY:-wayland-0} GDK_BACKEND=wayland
    exec python3 /tmp/input-probe.py /tmp/input-probe.log'
sleep 9
probe_cmd formats
probe_cmd "paste-raw text/plain;charset=utf-8"
sleep 1
probe_cmd paste
sleep 3
probe_cmd "copy hello from grd ✓"
wait "$client" || true
podman exec transom-grd pkill -f input-probe || true
podman exec transom-grd cat /tmp/input-probe.log > "$STATE/clip-probe.log" 2>/dev/null || true
echo "--- client"; grep -v '^\[' "$STATE/cliptest.out"
echo "--- probe"; grep -E '"t": "(paste|copied|formats|paste-raw)' "$STATE/clip-probe.log" || true
ok=1
if grep -q '"t": "paste", "text": "hello from transom \\u2713\\nline two"' "$STATE/clip-probe.log"; then
    echo "local -> remote: OK"
elif grep -q '"t": "paste", "text": "hello from transom \\\\u2713\\nline two"' "$STATE/clip-probe.log"; then
    echo "local -> remote: OK (non-ASCII escaped by grd: see docs/known-issues.md)"
else
    echo "local -> remote: FAILED"; ok=0
fi
grep -q 'REMOTE TEXT: "hello from grd ✓"' "$STATE/cliptest.out" \
    && echo "remote -> local: OK" || { echo "remote -> local: FAILED"; ok=0; }

# --- Widget pass: transom-view-demo's GDK clipboard <-> remote probe.
echo "--- widget path (GDK clipboard <-> TransomView bridge <-> RDP)"
cargo build -q -p transom-view --example transom-view-demo
podman exec transom-grd pkill -f input-probe || true
podman exec transom-grd bash -c 'rm -f /tmp/input-probe.log /tmp/probe-cmd'
podman run --rm --network host --userns=keep-id \
    -e SCALE=1 -e MONITOR=1600x1000 -e BIN=/demo/examples/transom-view-demo \
    -e RUST_LOG="${RUST_LOG:-}" \
    -v "$PWD/target/debug:/demo:ro,Z" \
    -v "$PWD/tests/servers/view-check/inner.sh:/inner.sh:ro,Z" \
    -v "$STATE:/state:Z" \
    --entrypoint /inner.sh transom-test-grd \
    localhost:13389 transom --password-file /state/grd.pw --trust "$fp" \
    --size 1200x800 --clip-test --quit-after-ms 25000 >"$STATE/widget-clip.log" 2>&1 &
demo=$!
sleep 5
podman exec -d transom-grd bash -c '
    pid=$(pgrep -f "gnome-shell --headless" | head -1)
    export $(tr "\0" "\n" < /proc/$pid/environ | grep -E "^(DBUS_SESSION_BUS_ADDRESS|XDG_RUNTIME_DIR|WAYLAND_DISPLAY)=")
    export WAYLAND_DISPLAY=${WAYLAND_DISPLAY:-wayland-0} GDK_BACKEND=wayland
    exec python3 /tmp/input-probe.py /tmp/input-probe.log'
sleep 9   # demo sets its local clipboard ~6 s after connecting
podman exec transom-grd bash -c 'pid=$(pgrep -f input-probe.py); [ -n "$pid" ]' || echo "probe not running"
probe_cmd paste
sleep 3
probe_cmd "copy remote says hi"
sleep 3
probe_cmd copy-image
sleep 5   # demo puts a 40x30 image on its clipboard ~18 s after connecting
probe_cmd "paste-raw image/bmp"
wait "$demo" || true
podman exec transom-grd pkill -f input-probe || true
podman exec transom-grd cat /tmp/input-probe.log > "$STATE/widget-clip-probe.log" 2>/dev/null || true
grep -E "^(set local|LOCAL CLIPBOARD)" "$STATE/widget-clip.log" || true
grep -E '"t": "(paste|copied)' "$STATE/widget-clip-probe.log" || true
grep -q '"t": "paste", "text": "from the local side' "$STATE/widget-clip-probe.log" \
    && echo "widget local -> remote: OK" || { echo "widget local -> remote: FAILED"; ok=0; }
grep -q 'LOCAL CLIPBOARD TEXT: "remote says hi"' "$STATE/widget-clip.log" \
    && echo "widget remote -> local text: OK" || { echo "widget remote -> local text: FAILED"; ok=0; }
grep -q 'LOCAL CLIPBOARD IMAGE: 64x48' "$STATE/widget-clip.log" \
    && echo "widget remote -> local image: OK" || { echo "widget remote -> local image: FAILED"; ok=0; }
# grd offers our image as image/bmp: BITMAPINFOHEADER width 40 (0x28), height 30 (0x1e).
grep -q '"mime": "image/bmp", "hex": "424d.\{24\}28000000\(28\|00\)0000001e000000' "$STATE/widget-clip-probe.log" \
    && echo "widget local -> remote image: OK" || { echo "widget local -> remote image: FAILED"; ok=0; }
[[ $ok == 1 ]]
