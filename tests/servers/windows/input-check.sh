# SPDX-License-Identifier: GPL-3.0-or-later
# Windows half of scripts/input-check.sh (sourced; scripts/lib/target.sh has set T_*).
# The probe (input-probe.ps1) runs in the rdp user's session via a scheduled task.
probe_log='C:\transom\input-probe.log'
win() { scripts/winvm.sh "$1" "$TARGET" "${@:2}"; }
start_probe() { # SECONDS
    # End a previous probe (it holds the log open), then start fresh.
    win exec "schtasks /End /TN TransomSession 2>\$null | Out-Null
Get-CimInstance Win32_Process -Filter \"Name='powershell.exe'\" |
    Where-Object CommandLine -like '*input-probe.ps1*' | ForEach-Object { Stop-Process -Id \$_.ProcessId -Force }
Start-Sleep -Milliseconds 300
Remove-Item -Force -ErrorAction SilentlyContinue '$probe_log'; exit 0"
    win session-run "powershell -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File C:\\transom\\input-probe.ps1 -Seconds $1" >/dev/null
    for _ in $(seq 60); do
        win exec "Get-Content -ErrorAction SilentlyContinue '$probe_log'" 2>/dev/null | grep -q ready && return
        sleep 0.5
    done
    echo "probe did not start" >&2; exit 1
}
fetch_log() { win exec "Get-Content '$probe_log'" | tr -d '\r' > "$1"; }

win push tests/servers/windows/input-probe.ps1 'C:\transom\input-probe.ps1'

echo "--- engine path ($TARGET)"
cargo build -q -p transom-view-freerdp --example inputtest
# The client connects first (creating or reattaching the rdp session), the probe starts
# inside that session, and the script is sent after --wait-ms.
target/debug/examples/inputtest "$T_ADDR" "$T_USER" --password-file "$T_PW" --trust "$T_FP" \
    --wait-ms 12000 > "$STATE/inputtest.log" 2>&1 &
client=$!
sleep 3
start_probe 40
wait "$client" || { tail -20 "$STATE/inputtest.log"; exit 1; }
fetch_log "$STATE/input-probe.log"
python3 -I tests/servers/grd-headless/probe/check-input.py --windows "$STATE/input-probe.log" || exit 1

echo "--- widget path ($TARGET: GTK keycodes -> TransomView keymap -> RDP)"
cargo build -q -p transom-view --example transom-view-demo
podman run --rm --network host --userns=keep-id \
    -e SCALE=1 -e MONITOR=1600x1000 -e BIN=/demo/examples/transom-view-demo \
    -e INJECT=/inject-keys.py -e INJECT_AFTER=14 \
    -v "$PWD/target/debug:/demo:ro,Z" \
    -v "$PWD/tests/servers/view-check/inner.sh:/inner.sh:ro,Z" \
    -v "$PWD/tests/servers/view-check/inject-keys.py:/inject-keys.py:ro,Z" \
    -v "$STATE:/state:Z" \
    --entrypoint /inner.sh transom-test-grd \
    "$T_ADDR" "$T_USER" --password-file /state/target.pw --trust "$T_FP" \
    --size 1200x800 --quit-after-ms 20000 >"$STATE/widget-input.log" 2>&1 &
demo=$!
sleep 7
start_probe 30
wait "$demo" || true
fetch_log "$STATE/input-probe-widget.log"
grep -q "injected" "$STATE/widget-input.log" || { echo "injection failed:"; tail -20 "$STATE/widget-input.log"; exit 1; }
python3 -I tests/servers/grd-headless/probe/check-input.py --windows --widget "$STATE/input-probe-widget.log"
