#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# End-to-end input test: the inputtest example sends a fixed keyboard/mouse script
# through the FreeRDP engine to the grd test server; a probe app inside the remote
# session logs what arrived; this script compares the two.
#
#   scripts/input-check.sh        (needs scripts/test-servers.sh up)
set -euo pipefail
cd "$(dirname "$0")/.."
STATE="${TMPDIR:-/var/tmp}/transom-test"
cargo build -q -p transom-view-freerdp --example inputtest
fp=$(podman logs transom-grd 2>&1 | sed -n 's/^TLS fingerprint: //p' | tail -1)
podman exec transom-grd rm -f /tmp/input-probe.log
target/debug/examples/inputtest localhost:13389 transom \
    --password-file "$STATE/grd.pw" --trust "$fp" --wait-ms 6000 &
client=$!
sleep 2.5
# Start the probe inside the remote session (same Wayland display and session bus
# as gnome-shell).
podman exec -d transom-grd bash -c '
    pid=$(pgrep -f "gnome-shell --headless" | head -1)
    export $(tr "\0" "\n" < /proc/$pid/environ | grep -E "^(DBUS_SESSION_BUS_ADDRESS|XDG_RUNTIME_DIR|WAYLAND_DISPLAY)=")
    export WAYLAND_DISPLAY=${WAYLAND_DISPLAY:-wayland-0} GDK_BACKEND=wayland
    exec input-probe /tmp/input-probe.log'
wait "$client"
sleep 0.5
podman exec transom-grd pkill -f input-probe || true
podman exec transom-grd cat /tmp/input-probe.log > "$STATE/input-probe.log" || true
python3 -I tests/servers/grd-headless/probe/check-input.py "$STATE/input-probe.log" || exit 1

# --- Second pass: through the widget. Keys are injected into a headless mutter as
# evdev codes, go through GTK and the TransomView scancode table, then RDP.
echo "--- widget path (GTK keycodes -> TransomView keymap -> RDP)"
cargo build -q -p transom-view --example transom-view-demo
podman exec transom-grd rm -f /tmp/input-probe.log
podman run --rm --network host --userns=keep-id \
    -e SCALE=1 -e MONITOR=1600x1000 -e BIN=/demo/examples/transom-view-demo \
    -e INJECT=/inject-keys.py -e INJECT_AFTER=8 \
    -v "$PWD/target/debug:/demo:ro,Z" \
    -v "$PWD/tests/servers/view-check/inner.sh:/inner.sh:ro,Z" \
    -v "$PWD/tests/servers/view-check/inject-keys.py:/inject-keys.py:ro,Z" \
    -v "$STATE:/state:Z" \
    --entrypoint /inner.sh transom-test-grd \
    localhost:13389 transom --password-file /state/grd.pw --trust "$fp" \
    --size 1200x800 --quit-after-ms 12000 >"$STATE/widget-input.log" 2>&1 &
demo=$!
sleep 5
podman exec -d transom-grd bash -c '
    pid=$(pgrep -f "gnome-shell --headless" | head -1)
    export $(tr "\0" "\n" < /proc/$pid/environ | grep -E "^(DBUS_SESSION_BUS_ADDRESS|XDG_RUNTIME_DIR|WAYLAND_DISPLAY)=")
    export WAYLAND_DISPLAY=${WAYLAND_DISPLAY:-wayland-0} GDK_BACKEND=wayland
    exec input-probe /tmp/input-probe.log'
wait "$demo" || true
podman exec transom-grd pkill -f input-probe || true
podman exec transom-grd cat /tmp/input-probe.log > "$STATE/input-probe-widget.log" || true
grep -q "injected" "$STATE/widget-input.log" || { echo "injection failed:"; tail -20 "$STATE/widget-input.log"; exit 1; }
python3 -I tests/servers/grd-headless/probe/check-input.py --widget "$STATE/input-probe-widget.log"
