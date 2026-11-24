#!/usr/bin/env bash
# Starts a headless GNOME Shell and gnome-remote-desktop in headless mode.
# Credentials come from /run/transom/creds (two lines: user, password), bind-mounted
# by scripts/test-servers.sh. They are throwaway test values.
set -euo pipefail
mkdir -p -m 700 "$XDG_RUNTIME_DIR"

if [[ "${1:-}" != "--inner" ]]; then
    # No systemd or system bus in the container: give GNOME Shell an empty stand-in
    # system bus so its D-Bus proxies fail soft instead of aborting.
    sysbus="unix:path=$XDG_RUNTIME_DIR/system_bus_socket"
    dbus-daemon --session --address="$sysbus" --fork --nopidfile
    export DBUS_SYSTEM_BUS_ADDRESS=$sysbus
    exec dbus-run-session -- "$0" --inner
fi

{ read -r RDP_USER; read -r RDP_PASS; } < /run/transom/creds

tls=$HOME/.local/share/gnome-remote-desktop
mkdir -p "$tls"
if [[ ! -f $tls/tls.crt ]]; then
    openssl req -x509 -newkey rsa:2048 -nodes -days 365 -subj "/CN=transom-grd-test" \
        -keyout "$tls/tls.key" -out "$tls/tls.crt" 2>/dev/null
fi

pipewire & sleep 0.5
wireplumber & sleep 0.5
# A null sink as the default output, so session audio has somewhere to go (grd
# forwards what plays there; tests play a tone into it).
pipewire-pulse & sleep 0.5
pactl load-module module-null-sink sink_name=transom_null >/dev/null 2>&1 || true
pactl set-default-sink transom_null >/dev/null 2>&1 || true
gnome-shell --headless --no-x11 2>&1 | sed -u "s/^/[shell] /" &
sleep 4

grdctl --headless rdp set-tls-cert "$tls/tls.crt"
grdctl --headless rdp set-tls-key "$tls/tls.key"
# The password goes in on stdin, not argv: grdctl reads it there when it isn't given.
printf '%s\n' "$RDP_PASS" | grdctl --headless rdp set-credentials "$RDP_USER"
grdctl --headless rdp disable-view-only
# `grdctl rdp enable` also tries to enable a systemd unit; set the key directly.
# Headless mode uses its own relocated settings path.
for path in /org/gnome/desktop/remote-desktop/rdp/ /org/gnome/desktop/remote-desktop/rdp/headless/; do
    dconf write "${path}enable" true
    dconf write "${path}view-only" false
done
echo "TLS fingerprint: $(openssl x509 -in "$tls/tls.crt" -noout -fingerprint -sha256 | cut -d= -f2)"
exec /usr/libexec/gnome-remote-desktop-daemon --headless
