#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Throwaway RDP test servers in rootless podman, so no real machine needs test
# credentials.
#
#   scripts/test-servers.sh up      # build + start; prints host, user, fingerprint
#   scripts/test-servers.sh info    # print connection details again
#   scripts/test-servers.sh down    # stop and remove containers + credentials
#
# Credentials are random per `up`, stored 0600 under ${TMPDIR:-/var/tmp}/transom-test.
set -euo pipefail
cd "$(dirname "$0")/.."

STATE="${TMPDIR:-/var/tmp}/transom-test"
GRD_PORT=13389
XRDP_PORT=13390

info() {
    local fp
    fp=$(podman logs transom-grd 2>&1 | sed -n 's/^TLS fingerprint: //p' | tail -1)
    cat <<INFO
gnome-remote-desktop (headless, GNOME $(podman exec transom-grd rpm -q --qf '%{VERSION}' gnome-shell 2>/dev/null))
  host:          localhost:$GRD_PORT
  user:          $(sed -n 1p "$STATE/grd.creds")
  password file: $STATE/grd.pw
  SHA-256:       $fp
  spike:         target/debug/examples/spike localhost:$GRD_PORT $(sed -n 1p "$STATE/grd.creds") --password-file $STATE/grd.pw --trust $fp
INFO
    local xfp
    xfp=$(podman logs transom-xrdp 2>&1 | sed -n 's/^TLS fingerprint: //p' | tail -1)
    cat <<INFO
xrdp 0.10 (smoke test)
  host:          localhost:$XRDP_PORT
  user:          rdp
  password file: $STATE/xrdp.pw
  SHA-256:       $xfp
INFO
}

new_creds() { # name user
    (umask 077
     printf '%s\n%s\n' "$2" "$(openssl rand -base64 24 | tr -d '/+=')" > "$STATE/$1.creds"
     sed -n 2p "$STATE/$1.creds" > "$STATE/$1.pw")
}

case "${1:-}" in
up)
    mkdir -p -m 700 "$STATE"
    new_creds grd transom
    new_creds xrdp rdp
    podman build -q -t transom-test-grd tests/servers/grd-headless >/dev/null
    podman build -q -t transom-test-xrdp tests/servers/xrdp >/dev/null
    podman rm -f transom-grd transom-xrdp >/dev/null 2>&1 || true
    podman run -d --name transom-grd --userns=keep-id \
        -p "127.0.0.1:$GRD_PORT:3389" \
        -v "$STATE/grd.creds:/run/transom/creds:ro,Z" \
        transom-test-grd >/dev/null
    podman run -d --name transom-xrdp \
        -p "127.0.0.1:$XRDP_PORT:3389" \
        -v "$STATE/xrdp.creds:/run/transom/creds:ro,Z" \
        transom-test-xrdp >/dev/null
    for _ in $(seq 30); do
        podman logs transom-grd 2>&1 | grep -q '^TLS fingerprint:' && break
        sleep 1
    done
    sleep 2
    info
    ;;
info) info ;;
down)
    podman rm -f transom-grd transom-xrdp >/dev/null 2>&1 || true
    rm -rf "$STATE"
    ;;
*) echo "usage: $0 up|info|down" >&2; exit 2 ;;
esac
