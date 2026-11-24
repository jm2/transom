# SPDX-License-Identifier: GPL-3.0-or-later
# Sourced by the check scripts: picks the RDP test server with TARGET=grd|win11|server
# (default grd) and sets
#   T_ADDR  host:port            T_USER  user name
#   T_PW    password file (host)  T_FP    SHA-256 certificate fingerprint
#   T_KIND  grd | windows
source scripts/lib/state.sh
TARGET=${TARGET:-grd}
case "$TARGET" in
grd)
    [[ -f $STATE/grd.pw ]] || { echo "run scripts/test-servers.sh up first" >&2; exit 2; }
    T_KIND=grd
    T_ADDR=localhost:13389
    T_USER=$(sed -n 1p "$STATE/grd.creds")
    T_PW=$STATE/grd.pw
    T_FP=$(podman logs transom-grd 2>&1 | sed -n 's/^TLS fingerprint: //p' | tail -1)
    ;;
win11|server)
    T_KIND=windows
    _vm=${TRANSOM_VM_DIR:-$HOME/.local/share/transom-testvm}/$TARGET
    [[ -f $_vm/installed ]] || { echo "no $TARGET VM: scripts/winvm.sh prep/install $TARGET" >&2; exit 2; }
    scripts/winvm.sh run "$TARGET" >/dev/null
    T_ADDR=localhost:$([[ $TARGET == win11 ]] && echo 13392 || echo 13393)
    T_USER=rdp
    T_PW=$_vm/password
    # Waits for the guest to boot (the guest agent answers once Windows is up).
    for _ in $(seq 60); do scripts/winvm.sh fingerprint "$TARGET" >/dev/null 2>&1 && break; sleep 3; done
    T_FP=$(cat "$_vm/fingerprint")
    ;;
*) echo "TARGET must be grd, win11 or server" >&2; exit 2 ;;
esac
# Containers get the password through $STATE (mounted at /state).
install -m 600 "$T_PW" "$STATE/target.pw"
