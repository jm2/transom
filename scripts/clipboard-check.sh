#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Clipboard round trip against the grd test server: the client offers text and the
# in-session probe pastes it; then the probe copies text and the client receives it.
# Then the same through transom-view's GDK bridge (text, images, files), and the focus
# rules: a copy made in another application isn't readable by the remote until the
# session has focus again, and a follower view (multi-monitor) counts as the session.
#   scripts/clipboard-check.sh                # grd (scripts/test-servers.sh up)
#   TARGET=win11 scripts/clipboard-check.sh   # or server (scripts/winvm.sh)
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/lib/target.sh
if [[ $T_KIND == windows ]]; then
    source tests/servers/windows/clipboard-check.sh
    exit
fi
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
# The bridge exchanges content only while the session has keyboard focus: the
# demo gets it as a user would, by a click (inject-focus.py).
cargo build -q -p transom-view --example transom-view-demo
podman exec transom-grd pkill -f input-probe || true
podman exec transom-grd bash -c 'rm -f /tmp/input-probe.log /tmp/probe-cmd'
podman run --rm --network host --userns=keep-id \
    -e SCALE=1 -e MONITOR=1600x1000 -e BIN=/demo/examples/transom-view-demo \
    -e RUST_LOG="${RUST_LOG:-}" \
    -e INJECT=/checks/inject-focus.py -e INJECT_AFTER=4 \
    -v "$PWD/tests/servers/view-check:/checks:ro,Z" \
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

# --- Files pass: transom-view-demo <-> remote probe (grd serves files via FUSE).
echo "--- files (both directions, contents compared by SHA-256)"
podman exec transom-grd pkill -f input-probe || true
podman exec transom-grd bash -c 'rm -rf /tmp/input-probe.log /tmp/probe-cmd /tmp/clip-src'
podman run --rm --network host --userns=keep-id \
    -e SCALE=1 -e MONITOR=1600x1000 -e BIN=/demo/examples/transom-view-demo \
    -e RUST_LOG="${RUST_LOG:-}" \
    -e INJECT=/checks/inject-focus.py -e INJECT_AFTER=4 \
    -v "$PWD/tests/servers/view-check:/checks:ro,Z" \
    -v "$PWD/target/debug:/demo:ro,Z" \
    -v "$PWD/tests/servers/view-check/inner.sh:/inner.sh:ro,Z" \
    -v "$STATE:/state:Z" \
    --entrypoint /inner.sh transom-test-grd \
    localhost:13389 transom --password-file /state/grd.pw --trust "$fp" \
    --size 1200x800 --file-clip-test --quit-after-ms 30000 >"$STATE/widget-files.log" 2>&1 &
demo=$!
sleep 5
podman cp tests/servers/grd-headless/probe/input-probe.py transom-grd:/tmp/input-probe.py
podman exec -d transom-grd bash -c '
    pid=$(pgrep -f "gnome-shell --headless" | head -1)
    export $(tr "\0" "\n" < /proc/$pid/environ | grep -E "^(DBUS_SESSION_BUS_ADDRESS|XDG_RUNTIME_DIR|WAYLAND_DISPLAY)=")
    export WAYLAND_DISPLAY=${WAYLAND_DISPLAY:-wayland-0} GDK_BACKEND=wayland
    exec python3 /tmp/input-probe.py /tmp/input-probe.log'
sleep 9   # demo copies its files ~6 s after connecting
probe_cmd paste-files
sleep 6
probe_cmd copy-files
wait "$demo" || true
podman exec transom-grd pkill -f input-probe || true
podman exec transom-grd cat /tmp/input-probe.log > "$STATE/files-probe.log" 2>/dev/null || true
python3 -I - "$STATE/widget-files.log" "$STATE/files-probe.log" <<'PY' || ok=0
import ast, json, sys
demo = open(sys.argv[1]).read().splitlines()
probe = [json.loads(l) for l in open(sys.argv[2]) if l.strip()]
def demo_map(prefix):
    # The last parseable report wins (an earlier read can fail while the lazy
    # provider is still downloading).
    found = None
    for l in demo:
        if l.startswith(prefix):
            try:
                found = ast.literal_eval(l[len(prefix):].strip())
            except (ValueError, SyntaxError):
                pass
    return found
local_set = demo_map("LOCAL FILES SET:")
pasted = next((e.get("sha") for e in probe if e["t"] == "pasted-files"), None)
copied = next((e.get("sha") for e in probe if e["t"] == "copied-files"), None)
received = demo_map("LOCAL CLIPBOARD FILES:")
ok = True
for name, a, b in [("local -> remote files", local_set, pasted), ("remote -> local files", copied, received)]:
    good = bool(a) and a == b
    ok &= good
    print(f"{name}: {'OK' if good else 'FAILED'} ({len(a or {})} files)")
    if not good:
        print("  want", a, "\n  got ", b)
sys.exit(0 if ok else 1)
PY

# --- Focus passes: the remote reads the local clipboard only while the session has
# focus. clip-focus.py clicks between windows in the client's headless mutter and runs
# a second application that copies while the session is in the background. mutter
# doesn't tell an unfocused client about the new selection, yet a read through its
# old offer returns it, so only the focus check keeps the remote from reading it.
start_probe() {
    podman exec transom-grd pkill -f input-probe || true
    podman exec transom-grd bash -c 'rm -rf /tmp/input-probe.log /tmp/probe-cmd'
    podman exec -d transom-grd bash -c '
        pid=$(pgrep -f "gnome-shell --headless" | head -1)
        export $(tr "\0" "\n" < /proc/$pid/environ | grep -E "^(DBUS_SESSION_BUS_ADDRESS|XDG_RUNTIME_DIR|WAYLAND_DISPLAY)=")
        export WAYLAND_DISPLAY=${WAYLAND_DISPLAY:-wayland-0} GDK_BACKEND=wayland
        exec python3 /tmp/input-probe.py /tmp/input-probe.log'
}
fc=transom-clip-focus
focus_cmd() { podman exec "$fc" bash -c "printf '%s\n' \"\$1\" >> /tmp/clip-focus-cmd" _ "$1"; }
run_focus_demo() { # log-file monitor2 demo-args...
    local log=$1 m2=$2; shift 2
    podman rm -f "$fc" >/dev/null 2>&1 || true
    podman run --rm --name "$fc" --network host --userns=keep-id \
        -e SCALE=1 -e MONITOR=1600x1000 -e MONITOR2="$m2" -e SCALE2_STATIC=1 \
        -e BIN=/demo/examples/transom-view-demo -e RUST_LOG="${RUST_LOG:-}" \
        -e INJECT=/checks/clip-focus.py -e INJECT_AFTER=1 \
        -v "$PWD/tests/servers/view-check:/checks:ro,Z" \
        -v "$PWD/target/debug:/demo:ro,Z" \
        -v "$PWD/tests/servers/view-check/inner.sh:/inner.sh:ro,Z" \
        -v "$STATE:/state:Z" \
        --entrypoint /inner.sh transom-test-grd \
        localhost:13389 transom --password-file /state/grd.pw --trust "$fp" "$@" >"$log" 2>&1
}

echo "--- focus (a copy made in another app isn't readable until the session has focus)"
canary="transom-canary-$RANDOM$RANDOM"
run_focus_demo "$STATE/focus-clip.log" "" --size 1200x800 --quit-after-ms 30000 &
demo=$!
sleep 5
start_probe
sleep 4
focus_cmd other-show; sleep 1; focus_cmd "click 800 500"; sleep 1
focus_cmd "other-copy first local copy"; sleep 1
focus_cmd other-hide; sleep 1; focus_cmd "click 800 500"; sleep 2   # back: offered
probe_cmd paste; sleep 2                                             # 1: the remote pastes it
focus_cmd other-show; sleep 1; focus_cmd "click 800 500"; sleep 1    # to the other app,
focus_cmd "other-copy $canary"; sleep 2                              # which copies
# 2: the remote reads again, in a format it hasn't fetched yet (grd caches the others).
probe_cmd "paste-raw text/html"; sleep 3
focus_cmd other-hide; sleep 1; focus_cmd "click 800 500"; sleep 2    # back: offered
probe_cmd paste; sleep 2                                             # 3
wait "$demo" || true
podman exec transom-grd pkill -f input-probe || true
podman exec transom-grd cat /tmp/input-probe.log > "$STATE/focus-probe.log" 2>/dev/null || true
python3 -I - "$STATE/focus-probe.log" "$canary" <<'PY' || ok=0
import json, sys
reads = [e for e in (json.loads(l) for l in open(sys.argv[1]) if l.strip())
         if e["t"] in ("paste", "paste-raw")]
def text(e):
    if e["t"] == "paste":
        return e.get("text") or ""
    try:
        return bytes.fromhex(e.get("hex", "")).decode("utf-8", "replace")
    except ValueError:
        return ""
checks = [("offered copy pasted", "first local copy", True),
          ("background copy not readable", sys.argv[2], False),
          ("background copy offered on return", sys.argv[2], True)]
ok = len(reads) == len(checks)
for i, (label, want, present) in enumerate(checks):
    good = i < len(reads) and (want in text(reads[i])) == present
    ok &= good
    print(f"focus {label}: {'OK' if good else 'FAILED'}")
sys.exit(0 if ok else 1)
PY

# A follower view (multi-monitor) is the session too: copying and pasting work while
# only the second monitor's window has focus.
echo "--- follower (multi-monitor: copy and paste while another monitor's window has focus)"
run_focus_demo "$STATE/follower-clip.log" 1600x1000 --multimon --clip-test --quit-after-ms 22000 &
demo=$!
sleep 5
start_probe
focus_cmd "click 2400 500"; sleep 9   # the follower; the demo copies ~6 s after connecting
probe_cmd paste; sleep 3
probe_cmd "copy follower paste back"; sleep 3
wait "$demo" || true
podman exec transom-grd pkill -f input-probe || true
podman exec transom-grd cat /tmp/input-probe.log > "$STATE/follower-probe.log" 2>/dev/null || true
grep -q '"t": "paste", "text": "from the local side' "$STATE/follower-probe.log" \
    && echo "follower local -> remote: OK" || { echo "follower local -> remote: FAILED"; ok=0; }
grep -q 'LOCAL CLIPBOARD TEXT: "follower paste back"' "$STATE/follower-clip.log" \
    && echo "follower remote -> local: OK" || { echo "follower remote -> local: FAILED"; ok=0; }
[[ $ok == 1 ]]
