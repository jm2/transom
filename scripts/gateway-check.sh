#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# RD Gateway check: connect to the Server test VM *through* its own RD Gateway
# (localhost:13443 -> TRANSOM-SRV:3389; only the gateway can resolve the target name).
# Configures the gateway (tests/servers/windows/gateway-setup.ps1), reads its
# certificate fingerprint through the guest agent, connects with the spike, and checks
# the gateway's event log for the resource connection.
#   scripts/gateway-check.sh        (needs scripts/winvm.sh install server)
set -euo pipefail
cd "$(dirname "$0")/.."
TARGET=server
source scripts/lib/target.sh
cargo build -q -p transom-view-freerdp --example spike
gwfp=$(scripts/winvm.sh exec server "$(cat tests/servers/windows/gateway-setup.ps1)" | tr -d '\r' | tail -1)
[[ $gwfp =~ ^([0-9A-F]{2}:){31}[0-9A-F]{2}$ ]] || { echo "gateway setup failed: $gwfp" >&2; exit 1; }
echo "gateway certificate: $gwfp"
for _ in $(seq 30); do  # TSGateway restarting
    scripts/winvm.sh exec server '(Get-Service TSGateway).Status' 2>/dev/null | grep -q Running && break
    sleep 1
done
since=$(scripts/winvm.sh exec server '(Get-Date).ToString("o")' | tr -d '\r')
target/debug/examples/spike TRANSOM-SRV:3389 "$T_USER" --password-file "$T_PW" --trust "$T_FP" \
    --gateway localhost:13443 --gateway-trust "$gwfp" --settle-ms 4000 \
    --out "$STATE/gateway.png" 2>&1 | grep -E '^(connected|wrote|disconnected)' | tee "$STATE/gateway.out"
events=$(scripts/winvm.sh exec server "Get-WinEvent -LogName 'Microsoft-Windows-TerminalServices-Gateway/Operational' |
    Where-Object { \$_.TimeCreated -ge [datetime]'$since' -and \$_.Id -in 200,300,302 } |
    ForEach-Object { \$_.Id }" | tr -d '\r' | sort -u | tr '\n' ' ')
echo "gateway events: $events(200 CAP ok, 300 RAP ok, 302 connected)"
if grep -q '^wrote' "$STATE/gateway.out" && [[ $events == *302* ]]; then
    echo "GATEWAY OK"
else
    echo "GATEWAY FAILED"; exit 1
fi
