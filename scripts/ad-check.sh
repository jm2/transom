#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Phase 4 DoD: Kerberos single sign-on through a Windows RD Gateway to a domain host.
# The test domain is transom.test (scripts/ad-setup.sh): DC + RD Gateway TRANSOM-SRV,
# member TRANSOM-W11, user TRANSOM\alice. The client gets a ticket with kinit (KDC
# forwarded to localhost:13088) and connects with *no password*:
#   client -> TRANSOM-SRV.transom.test:13443 (gateway, HTTP Negotiate/Kerberos)
#          -> TRANSOM-W11.transom.test:3389 (NLA/CredSSP, Kerberos)
# Pass/fail comes from the Windows side, not the client's log: gateway events 200/302
# for TRANSOM\alice, a Kerberos network logon *and* an interactive logon of alice on
# TRANSOM-W11 (with no password that takes Remote Credential Guard: the client
# delegates its ticket, and Windows calls back over MS-RDPEAR), and the DC's service
# ticket events.
#   scripts/ad-check.sh
#   FREERDP_LIBDIR=DIR scripts/ad-check.sh   # run against another FreeRDP build (lib64)
#   FREERDP_LIBDIR= scripts/ad-check.sh      # force the system FreeRDP
# The interactive logon needs a FreeRDP with the MS-RDPEAR channel (-DCHANNEL_RDPEAR=ON,
# as in the Flatpak; scripts/build-freerdp-rdpear.sh builds one); with one without it (Fedora's) this check fails at Windows' logon
# screen, by design: the engine then leaves the password to Windows.
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT=${TRANSOM_VM_DIR:-$HOME/.local/share/transom-testvm}
source scripts/lib/state.sh
K=$STATE/ad
w() { scripts/winvm.sh "$@"; }
[[ -f $ROOT/domain/alice.pw ]] || { echo "no test domain: scripts/ad-setup.sh" >&2; exit 2; }
w run server >/dev/null; w run win11 >/dev/null
for vm in server win11; do
    for _ in $(seq 60); do w exec $vm 'exit 0' >/dev/null 2>&1 && break; sleep 3; done
done
cargo build -q -p transom-view-freerdp --example spike
podman build -q -t transom-test-kdc tests/servers/kdc >/dev/null

gwfp=$(w exec server "$(cat tests/servers/windows/gateway-setup.ps1)" | tr -d '\r' | tail -1)
[[ $gwfp =~ ^([0-9A-F]{2}:){31}[0-9A-F]{2}$ ]] || { echo "gateway setup failed: $gwfp" >&2; exit 1; }
for _ in $(seq 30); do
    w exec server '(Get-Service TSGateway).Status' 2>/dev/null | grep -q Running && break; sleep 1
done
fp=$(w fingerprint win11)

mkdir -p -m 700 "$K"
rm -f "$K"/krb5cc_*
install -m 600 "$ROOT/domain/alice.pw" "$K/alice.pw"
# Names stay as given (no DNS canonicalisation or reverse lookups: the client knows the
# gateway only through --add-host, and the target only by the name the gateway resolves).
cat > "$K/krb5.conf" <<'EOF'
[libdefaults]
    default_realm = TRANSOM.TEST
    dns_lookup_kdc = false
    dns_lookup_realm = false
    dns_canonicalize_hostname = false
    rdns = false
    udp_preference_limit = 1
[realms]
    TRANSOM.TEST = {
        kdc = 127.0.0.1:13088
    }
[domain_realm]
    .transom.test = TRANSOM.TEST
    transom.test = TRANSOM.TEST
EOF
kenv=(-e KRB5_CONFIG=/k/krb5.conf -e KRB5CCNAME=FILE:/k/krb5cc_alice -v "$K:/k:Z")
podman run --rm --network host --userns=keep-id "${kenv[@]}" --entrypoint bash transom-test-kdc \
    -c 'kinit -f alice@TRANSOM.TEST < /k/alice.pw >/dev/null && klist | sed -n 2p'
rm -f "$K/alice.pw"   # from here on only the ticket exists on the client side
trap 'rm -f "$K"/krb5cc_*' EXIT

since=$(w exec server '(Get-Date).ToUniversalTime().ToString("o")' | tr -d '\r')
# Default: the RDPEAR build from scripts/build-freerdp-rdpear.sh, when it exists and,
# like $STATE, is a directory of this user's (it goes on LD_LIBRARY_PATH).
[[ -z ${FREERDP_LIBDIR+x} ]] && owned_dir "$STATE/freerdp-rdpear" && owned_dir "$STATE/freerdp-rdpear/lib64" &&
    FREERDP_LIBDIR=$STATE/freerdp-rdpear/lib64
lib=()
[[ -n ${FREERDP_LIBDIR:-} ]] && echo "FreeRDP from $FREERDP_LIBDIR"
[[ -n ${FREERDP_LIBDIR:-} ]] && lib=(-e LD_LIBRARY_PATH=/fr -v "$(realpath "$FREERDP_LIBDIR"):/fr:ro,Z")
out=$(podman run --rm --network host --userns=keep-id "${kenv[@]}" "${lib[@]}" -e RUST_LOG=info \
    --add-host TRANSOM-SRV.transom.test:127.0.0.1 \
    -v "$PWD/target/debug/examples:/x:ro,Z" --entrypoint /x/spike transom-test-grd \
    TRANSOM-W11.transom.test:3389 alice --domain TRANSOM.TEST --kerberos --trust "$fp" \
    --gateway TRANSOM-SRV.transom.test:13443 --gateway-trust "$gwfp" --settle-ms 12000 \
    --out /k/ad.png 2>&1 || true)
echo "$out" | sed 's/\x1b\[[0-9;]*m//g' | grep -E "^(connected|wrote|disconnected)|RDPEAR|rdpear|Logon Error|Credential Guard" | head -20 || true
podman run --rm --network host --userns=keep-id "${kenv[@]}" --entrypoint klist transom-test-kdc \
    | awk '/@/ && !/principal/ {print "  ticket:", $NF}'

q="[datetime]::Parse('$since').ToLocalTime()"
gw=$(w exec server "Get-WinEvent -LogName 'Microsoft-Windows-TerminalServices-Gateway/Operational' |
    Where-Object { \$_.TimeCreated -ge $q -and \$_.Id -in 200,300,302 -and \$_.Message -match 'alice' } |
    ForEach-Object { \$_.Id } | Sort-Object -Unique" | tr -d '\r' | tr '\n' ' ')
logon=$(w exec win11 "Get-WinEvent -FilterHashtable @{LogName='Security'; Id=4624; StartTime=$q} -ErrorAction SilentlyContinue |
    ForEach-Object { \$x = [xml]\$_.ToXml(); \$d = @{}; \$x.Event.EventData.Data | ForEach-Object { \$d[\$_.Name] = \$_.'#text' }
    if (\$d.TargetUserName -eq 'alice') { '{0}/type{1}' -f \$d.AuthenticationPackageName, \$d.LogonType } } | Sort-Object -Unique" | tr -d '\r' | tr '\n' ' ')
tgs=$(w exec server "Get-WinEvent -FilterHashtable @{LogName='Security'; Id=4769; StartTime=$q} -ErrorAction SilentlyContinue |
    ForEach-Object { \$x = [xml]\$_.ToXml(); \$d = @{}; \$x.Event.EventData.Data | ForEach-Object { \$d[\$_.Name] = \$_.'#text' }
    if (\$d.TargetUserName -like 'alice@*') { \$d.ServiceName } } | Sort-Object -Unique" | tr -d '\r' | tr '\n' ' ')
echo "gateway events for alice: ${gw:-none} (200 CAP ok, 300 RAP ok, 302 connected)"
echo "TRANSOM-W11 logons of alice: ${logon:-none}"
echo "DC service tickets issued to alice: ${tgs:-none}"
# Network-level Kerberos (type 3) alone ends at Windows' logon screen: the session must
# also log alice on interactively (type 10, RemoteInteractive) without a password.
if grep -q '^wrote' <<<"$out" && [[ $gw == *302* && $logon == *Kerberos/type3* && $logon == */type10* ]]; then
    echo "AD KERBEROS VIA GATEWAY OK (no password)"
else
    echo "AD KERBEROS VIA GATEWAY FAILED"; exit 1
fi
