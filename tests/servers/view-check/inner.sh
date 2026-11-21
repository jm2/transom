#!/usr/bin/env bash
# Runs inside the test image: headless mutter with one virtual monitor at $SCALE,
# then $BIN (default: transom-view-demo) with the given arguments.
set -euo pipefail
[[ -n "${TRACE:-}" ]] && set -x
export XDG_RUNTIME_DIR=/tmp/xdg-runtime
mkdir -p -m 700 "$XDG_RUNTIME_DIR"
if [[ "${1:-}" != "--inner" ]]; then
    sysbus="unix:path=$XDG_RUNTIME_DIR/system_bus_socket"
    dbus-daemon --session --address="$sysbus" --fork --nopidfile
    export DBUS_SYSTEM_BUS_ADDRESS=$sysbus
    exec dbus-run-session -- "$0" --inner "$@"
fi
shift

# Fractional scales need framebuffer scaling.
gsettings set org.gnome.mutter experimental-features "['scale-monitor-framebuffer']" 2>/dev/null || true  # pre-GNOME 51
mutter --headless --wayland --no-x11 --wayland-display=wayland-0 \
    --virtual-monitor "$MONITOR" ${MONITOR2:+--virtual-monitor "$MONITOR2"} >/tmp/mutter.log 2>&1 &
for _ in $(seq 50); do [[ -S $XDG_RUNTIME_DIR/wayland-0 ]] && break; sleep 0.1; done
sleep 1
connector=$(gdctl show 2>/dev/null | sed -n 's/.*Monitor \([^ ]*\).*/\1/p' | head -1)
if [[ -n "${MONITOR2:-}" ]]; then
    connector2=$(gdctl show 2>/dev/null | sed -n 's/.*Monitor \([^ ]*\).*/\1/p' | sed -n 2p)
    gdctl set --logical-monitor --primary --monitor "$connector" --scale "$SCALE" \
        --logical-monitor --monitor "$connector2" --scale "${SCALE2_STATIC:-1}" --right-of "$connector" \
        || { echo "gdctl could not set two monitors:"; gdctl show; exit 4; }
else
gdctl set --logical-monitor --primary --monitor "$connector" --scale "$SCALE" >/dev/null 2>&1 \
    || { echo "gdctl could not set scale $SCALE on $MONITOR:"; gdctl show; exit 4; }
fi
sleep 1
echo "monitor $connector $MONITOR, requested scale $SCALE"
if [[ -n "${SCALE2:-}" ]]; then
    # Simulate moving the window to a monitor with a different scale.
    ( sleep "${SCALE2_AFTER:-6}"
      echo "changing monitor scale to $SCALE2"
      gdctl set --logical-monitor --primary --monitor "$connector" --scale "$SCALE2" >/dev/null 2>&1 \
          || echo "gdctl could not set scale $SCALE2" ) &
fi
if [[ -n "${INJECT:-}" ]]; then
    ( sleep "${INJECT_AFTER:-8}"; python3 -u "$INJECT" 2>&1 | sed "s/^/[inject] /" ) &
fi
export WAYLAND_DISPLAY=wayland-0 GDK_BACKEND=wayland
exec "${BIN:-/demo/examples/transom-view-demo}" "$@"
