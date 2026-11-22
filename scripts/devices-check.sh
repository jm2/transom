#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Device redirection check (PLAN Phase 4b) against a Windows test VM:
# TARGET=win11 (default) or server. The client runs in a container with its own
# devices (tests/servers/devices: a throwaway CUPS with a PDF printer, a pty pair as
# a serial port), so nothing on this computer is redirected. A probe in the
# Windows session (device-probe.ps1) uses each device:
#   drive    reads a file from \\tsclient\transom and writes one back
#   printer  prints a line; the PDF that comes out on the client must contain it
#   serial   writes a line to COM9 and reads the client's reply (known issue: see
#            docs/known-issues.md)
#   refusal  with drive redirection disabled on the server, the session still
#            connects and the printer still works
set -euo pipefail
cd "$(dirname "$0")/.."
TARGET=${TARGET:-win11}
source scripts/lib/target.sh
[[ $T_KIND == windows ]] || { echo "TARGET must be a Windows VM" >&2; exit 2; }
cargo build -q -p transom-view-freerdp --example spike
podman build -q -t transom-test-devices tests/servers/devices >/dev/null
w() { scripts/winvm.sh "$1" "$TARGET" "${@:2}"; }
w push tests/servers/windows/device-probe.ps1 'C:\transom\device-probe.ps1'
probe() { # ARGS… → JSON
    local k r
    w exec 'Remove-Item -Force -ErrorAction SilentlyContinue C:\transom\device-out.json; exit 0' >/dev/null
    w session-run "powershell -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File C:\\transom\\device-probe.ps1 $*" >/dev/null
    for k in $(seq 60); do
        r=$(w exec 'Get-Content -Raw -ErrorAction SilentlyContinue C:\transom\device-out.json' 2>/dev/null) || true
        [[ -n $r ]] && { tr -d '\r' <<<"$r" | sed '/^$/d'; return; }
        sleep 0.5
    done
    echo '{"t":"timeout"}'
}
jget() { python3 -I -c 'import json,sys; d=json.loads(sys.stdin.read()); print(d.get(sys.argv[1], ""))' "$1"; }
D=$STATE/devices
rm -rf "$D"; mkdir -p "$D/share" "$D/pdf"
echo "from the client $$" > "$D/share/hello.txt"
client() { # LOG extra spike args… (background; sets $client)
    local log=$1; shift
    podman run --rm --network host \
        -v "$PWD/target/debug/examples:/x:ro,Z" -v "$STATE:/state:Z" \
        -v "$D/share:/share:Z" -v "$D/pdf:/out/pdf:Z" transom-test-devices \
        /x/spike "$T_ADDR" "$T_USER" --password-file /state/target.pw --trust "$T_FP" \
        --settle-ms 40000 --out /state/devices.png "$@" > "$log" 2>&1 &
    client=$!
}
ok=1
pass() { echo "$1: OK${2:+ ($2)}"; }
fail() { echo "$1: FAILED${2:+ ($2)}"; ok=0; }

echo "--- all devices ($TARGET)"
client "$D/client.log" --drive transom=/share --printers --serial COM9=/dev/ttyTRANSOM
sleep 12
r=$(probe -Action drive -Name transom)
[[ $(jget read <<<"$r") == "from the client $$" && $(cat "$D/share/from-windows.txt" 2>/dev/null) == "written by Windows" ]] \
    && pass drive "read and write \\\\tsclient\\transom" || fail drive "$r"
r=$(probe -Action print)
printer=$(jget printer <<<"$r")
r2=$(probe -Action serial -Name COM9)
wait "$client" || true
pdf=$(ls "$D/pdf" | grep -i '\.pdf$' | head -1 || true)
text=$([[ -n $pdf ]] && podman run --rm -v "$D/pdf:/p:ro,Z" --entrypoint pdftotext transom-test-devices "/p/$pdf" - 2>/dev/null || true)
[[ $text == *"TRANSOM PRINT TEST"* ]] && pass printer "$printer -> $pdf" || fail printer "$r; pdf: ${pdf:-none}"
if [[ $(jget reply <<<"$r2") == "reply: hello COM" ]]; then
    pass serial "COM9 <-> client pty"
else
    # docs/known-issues.md: Windows accepts the port but never opens it.
    echo "serial: KNOWN ISSUE (Windows accepted COM9 but doesn't expose it: $(jget mode <<<"$r2"))"
fi

echo "--- refusal (server setting: no drive redirection)"
# "Do not allow drive redirection", applied with a policy refresh.
ts='HKLM:\SOFTWARE\Policies\Microsoft\Windows NT\Terminal Services'
w exec "Set-ItemProperty '$ts' fDisableCdm 1; gpupdate /target:computer /force | Out-Null" >/dev/null
restore() { w exec "Set-ItemProperty '$ts' fDisableCdm 0; gpupdate /target:computer /force | Out-Null" >/dev/null; }
trap restore EXIT
rm -f "$D/pdf"/*.pdf
client "$D/refusal.log" --drive transom=/share --printers
sleep 12
r=$(probe -Action drive -Name transom)
r2=$(probe -Action print)
wait "$client" || true
restore; trap - EXIT
pdf=$(ls "$D/pdf" | grep -i '\.pdf$' | head -1 || true)
grep -q '^connected' "$D/refusal.log" && grep -q '^frame update' "$D/refusal.log" && [[ $(jget t <<<"$r") == error && -n $pdf ]] \
    && pass refusal "drive refused by the server; session and printer fine" \
    || fail refusal "drive: $r; print: $r2; pdf: ${pdf:-none}"
[[ $ok == 1 ]] && echo "DEVICES OK" || { echo "DEVICES FAILED"; exit 1; }
