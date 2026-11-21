#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# gnome-remote-desktop remote login end to end: connect to the system daemon, log
# in at the GDM greeter by typing, follow the Server Redirection into the new
# session (no second prompt), and check the session exists.
#   scripts/test-servers.sh login-up && scripts/login-check.sh
set -euo pipefail
cd "$(dirname "$0")/.."
STATE="${TMPDIR:-/var/tmp}/transom-test"
cargo build -q -p transom-view-freerdp --example logintest
podman restart transom-grd-login >/dev/null   # fresh greeter, no existing session
for _ in $(seq 60); do podman exec transom-grd-login test -f /run/transom/fingerprint 2>/dev/null && break; sleep 1; done
sleep 8
fp=$(podman exec transom-grd-login cat /run/transom/fingerprint | sed 's/TLS fingerprint: //')
target/debug/examples/logintest localhost:13391 "$(sed -n 1p "$STATE/login.creds")" \
    --password-file "$STATE/login-rdp.pw" --trust "$fp" \
    --login-password-file "$STATE/login-user.pw" --out "$STATE/login-session.png" | grep -v '^\['
user=$(sed -n 3p "$STATE/login.creds")
if podman exec transom-grd-login loginctl list-sessions --no-legend | grep -q " $user .* user "; then
    echo "LOGIN OK: $user has a user session; screenshot $STATE/login-session.png"
else
    echo "LOGIN FAILED: no user session for $user"; exit 1
fi
