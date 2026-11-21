#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Key-press-to-frame latency against the grd test server (loopback, software
# rendering and encoding in the container: a baseline, not a real-network number).
set -euo pipefail
cd "$(dirname "$0")/.."
STATE="${TMPDIR:-/var/tmp}/transom-test"
cargo build -q --release -p transom-view-freerdp --example latency
fp=$(podman logs transom-grd 2>&1 | sed -n 's/^TLS fingerprint: //p' | tail -1)
podman exec transom-grd pkill -f input-probe || true
podman cp tests/servers/grd-headless/probe/input-probe.py transom-grd:/tmp/input-probe.py
target/release/examples/latency localhost:13389 transom --password-file "$STATE/grd.pw" --trust "$fp" "$@" &
client=$!
sleep 2.5
podman exec -d transom-grd bash -c '
    pid=$(pgrep -f "gnome-shell --headless" | head -1)
    export $(tr "\0" "\n" < /proc/$pid/environ | grep -E "^(DBUS_SESSION_BUS_ADDRESS|XDG_RUNTIME_DIR|WAYLAND_DISPLAY)=")
    export WAYLAND_DISPLAY=${WAYLAND_DISPLAY:-wayland-0} GDK_BACKEND=wayland
    exec python3 /tmp/input-probe.py /tmp/latency-probe.log'
wait "$client"
podman exec transom-grd pkill -f input-probe || true
