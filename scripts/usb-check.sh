#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# USB redirection check (MS-RDPEUSB, urbdrc) against a Windows test VM:
#   USB=VID:PID (required)  TARGET=win11 (default) or server
# Unlike devices-check.sh this needs a real device on this computer, which is handed
# to the VM for the session: use a spare one (not an input device) whose device node
# you can write (docs/usb.md, Permissions). Checks:
#   redirect  the device appears in the Windows session (PnP) and leaves this computer
#   return    it comes back here when the session ends
#   refusal   with "Do not allow supported Plug and Play device redirection" set on the
#             server, the session still connects and the device stays here
set -euo pipefail
cd "$(dirname "$0")/.."
TARGET=${TARGET:-win11}
[[ ${USB:-} =~ ^[0-9a-fA-F]{4}:[0-9a-fA-F]{4}$ ]] || { echo "set USB=VID:PID" >&2; exit 2; }
source scripts/lib/target.sh
[[ $T_KIND == windows ]] || { echo "TARGET must be a Windows VM" >&2; exit 2; }
vid=${USB%:*}; pid=${USB#*:}
present() { # is the device on this computer, bound to a host driver?
    # (While redirected, libusb holds its interfaces through "usbfs".)
    local d i
    for d in /sys/bus/usb/devices/*; do
        [[ -f $d/idVendor && $(cat "$d/idVendor") == "${vid,,}" && $(cat "$d/idProduct") == "${pid,,}" ]] || continue
        for i in "$d"/*:*/driver; do
            [[ -e $i && $(basename "$(readlink "$i")") != usbfs ]] && return 0
        done
    done
    return 1
}
present || { echo "$USB isn't on this computer with a driver bound" >&2; exit 2; }
cargo build -q -p transom-view-freerdp --example spike
w() { scripts/winvm.sh "$1" "$TARGET" "${@:2}"; }
w push tests/servers/windows/device-probe.ps1 'C:\transom\device-probe.ps1'
probe() {
    local k r
    # A probe stuck from an earlier run (a device mid-install can stall PnP queries)
    # would keep the session task busy, and schtasks /Run then starts nothing.
    w exec 'schtasks /End /TN TransomSession 2>&1 | Out-Null; Remove-Item -Force -ErrorAction SilentlyContinue C:\transom\device-out.json; exit 0' >/dev/null
    w session-run "powershell -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File C:\\transom\\device-probe.ps1 $*" >/dev/null
    for k in $(seq 60); do
        r=$(w exec 'Get-Content -Raw -ErrorAction SilentlyContinue C:\transom\device-out.json' 2>/dev/null) || true
        [[ -n $r ]] && { tr -d '\r' <<<"$r" | sed '/^$/d'; return; }
        sleep 0.5
    done
    echo '{"t":"timeout"}'
}
D=$STATE/usb
rm -rf "$D"; mkdir -p "$D"
client() { # LOG (background; sets $client)
    # A fixed session length (--stay-ms), and the USB channel's messages in the log.
    WLOG_FILTER=com.freerdp.channels.urbdrc.client:DEBUG \
        target/debug/examples/spike "$T_ADDR" "$T_USER" --password-file "$T_PW" --trust "$T_FP" \
        --stay-ms 60000 --out "$D/usb.png" --usb "$USB" > "$1" 2>&1 &
    client=$!
}
ok=1
pass() { echo "$1: OK${2:+ ($2)}"; }
fail() { echo "$1: FAILED${2:+ ($2)}"; ok=0; }

echo "--- redirect $USB ($TARGET)"
client "$D/client.log"
gone=0
# The server claims the device a few seconds after the session starts.
for _ in $(seq 90); do present || { gone=1; break; }; sleep 0.5; done
sleep 5
r=$(probe -Action usb -Id "$USB")
wait "$client" || true
# In the session: a redirected (TSUSB) device with this ID, working, or with no
# driver installed (code 28: Windows has no driver for it, e.g. FTDI without Windows
# Update). Either way the redirection itself worked.
seen=$(python3 -I -c '
import json, sys
vid, pid = sys.argv[1].upper(), sys.argv[2].upper()
d = json.loads(sys.stdin.read()).get("devices") or []
for x in d:
    i = x.get("id", "").upper()
    if f"VID_{vid}&PID_{pid}" in i and "TSUSB" in i:
        print("working" if x.get("status") == "OK" else
              "no driver on the remote" if x.get("problem") == 28 else
              "status %s problem %s" % (x.get("status"), x.get("problem")))
        break
' "$vid" "$pid" <<<"$r" 2>/dev/null || true)
[[ ($seen == working || $seen == "no driver on the remote") && $gone == 1 ]] \
    && pass redirect "in the Windows session: $seen" || fail redirect "left this computer: $gone; ${seen:-not in the session}; $r"
back=0
for _ in $(seq 30); do present && { back=1; break; }; sleep 0.5; done
(( back )) && pass return "bound to its driver here again" || fail return "not back on this computer"

echo "--- refusal (server setting: no PnP device redirection)"
ts='HKLM:\SOFTWARE\Policies\Microsoft\Windows NT\Terminal Services'
w exec "Set-ItemProperty '$ts' fDisablePNPRedir 1; gpupdate /target:computer /force | Out-Null" >/dev/null
# Windows 11 opens the USB channel only with the policy explicitly off (0), not unset.
restore() { w exec "Set-ItemProperty '$ts' fDisablePNPRedir 0 -Type DWord; gpupdate /target:computer /force | Out-Null" >/dev/null; }
trap restore EXIT
client "$D/refusal.log"
sleep 25
r=$(probe -Action usb -Id "$USB")
wait "$client" || true
restore; trap - EXIT
sleep 3
grep -q '^connected' "$D/refusal.log" && grep -q '^frame update' "$D/refusal.log" \
    && [[ $r == *'"devices":[]'* || $r == *'"devices":null'* ]] && present \
    && pass refusal "session fine, device not redirected and still here" \
    || fail refusal "$r (log: $D/refusal.log)"
[[ $ok == 1 ]] && echo "USB OK" || { echo "USB FAILED"; exit 1; }
