#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Kerberos single sign-on against the grd test server: a throwaway MIT KDC
# (realm TRANSOM.TEST) runs in grd's network namespace, grd gets the TERMSRV
# keytab and kerberos-only auth, the client gets a ticket with kinit and connects
# with no password. grd is switched back to password auth afterwards.
# Needs scripts/test-servers.sh up.
set -euo pipefail
cd "$(dirname "$0")/.."
STATE="${TMPDIR:-/var/tmp}/transom-test"
K="$STATE/kdc"
cargo build -q -p transom-view-freerdp --example spike
podman build -q -t transom-test-kdc tests/servers/kdc >/dev/null
mkdir -p "$K"
sed 's/localhost:13388/localhost:88/g' tests/servers/kdc/krb5.conf > "$K/krb5.conf"
# The principal must be the local account grd runs as (it maps principals to users).
(umask 077; printf 'rdp\n%s\n' "$(openssl rand -base64 18 | tr -d '/+=')" > "$STATE/kdc.creds"
 sed -n 2p "$STATE/kdc.creds" > "$STATE/kdc-user.pw")
rm -f "$K/termsrv.keytab" "$K"/krb5cc_*
podman rm -f transom-kdc >/dev/null 2>&1 || true
podman run -d --name transom-kdc --network container:transom-grd \
    -v "$STATE/kdc.creds:/run/transom/creds:ro,Z" -v "$K:/out:Z" transom-test-kdc >/dev/null
for _ in $(seq 30); do [[ -f $K/termsrv.keytab ]] && break; sleep 0.5; done
grd_env='export $(tr "\0" "\n" < /proc/$(pgrep -f gnome-remote-desktop-daemon | head -1)/environ | grep -E "^DBUS_SESSION_BUS_ADDRESS=")'
restore() {
    podman exec transom-grd bash -c "$grd_env; grdctl --headless rdp set-auth-methods credentials" >/dev/null 2>&1 || true
    podman rm -f transom-kdc >/dev/null 2>&1 || true
}
trap restore EXIT
podman cp "$K/termsrv.keytab" transom-grd:/tmp/termsrv.keytab
podman cp "$K/krb5.conf" transom-grd:/tmp/krb5.conf
podman exec -u 0 transom-grd bash -c 'cp /tmp/krb5.conf /etc/krb5.conf; chown rdp /tmp/termsrv.keytab'
podman exec transom-grd bash -c "$grd_env; grdctl --headless rdp set-kerberos-keytab /tmp/termsrv.keytab; grdctl --headless rdp set-auth-methods kerberos" >/dev/null 2>&1
podman run --rm --network container:transom-grd --userns=keep-id \
    -e KRB5_CONFIG=/out/krb5.conf -e KRB5CCNAME=FILE:/out/krb5cc_rdp \
    -v "$K:/out:Z" -v "$STATE/kdc-user.pw:/pw:ro,Z" --entrypoint bash transom-test-kdc \
    -c 'kinit rdp@TRANSOM.TEST < /pw >/dev/null && klist | sed -n 2p'
fp=$(podman logs transom-grd 2>&1 | sed -n 's/^TLS fingerprint: //p' | tail -1)
out=$(podman run --rm --network container:transom-grd --userns=keep-id \
    -e KRB5_CONFIG=/out/krb5.conf -e KRB5CCNAME=FILE:/out/krb5cc_rdp -e RUST_LOG=info \
    -v "$K:/out:Z" -v "$PWD/target/debug/examples:/x:ro,Z" --entrypoint /x/spike transom-test-grd \
    localhost:3389 rdp --domain TRANSOM.TEST --kerberos --trust "$fp" --out /out/kerberos.png 2>&1 || true)
echo "$out" | grep -E "^(connected|wrote|disconnected)|Kerberos ticket" | sed 's/\x1b\[[0-9;]*m//g'
grep -q "^wrote" <<<"$out" && echo "KERBEROS OK (no password)" || { echo "KERBEROS FAILED"; exit 1; }
