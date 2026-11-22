#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Test Active Directory domain on the Windows test VMs: the server VM becomes the
# domain controller (and DNS) of transom.test, win11 joins it, over the VMs' lab
# network (scripts/winvm.sh: 10.77.0.1 server, 10.77.0.2 win11). Creates the domain
# user TRANSOM\alice (random password, 0600 in the VM directory) and the
# TransomGateway group, for scripts/ad-check.sh (Kerberos SSO through the RD Gateway).
# Idempotent: every step checks whether it is already done.
#   scripts/ad-setup.sh            (needs both VMs installed: scripts/winvm.sh install)
#
# Secrets go into the guest as a file through the guest agent (never argv or env), and
# the guest script deletes it after reading.
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT=${TRANSOM_VM_DIR:-$HOME/.local/share/transom-testvm}
D=$ROOT/domain
SRC=tests/servers/windows
w() { scripts/winvm.sh "$@"; }
new_password() {
    local p
    while :; do
        p=$(openssl rand -base64 48 | tr -dc 'A-Za-z0-9' | head -c 20)
        [[ $p =~ [A-Z] && $p =~ [a-z] && $p =~ [0-9] ]] && { echo "$p"; return; }
    done
}
wait_agent() { # VM: wait until the guest agent answers (after a reboot)
    sleep 20
    for _ in $(seq 120); do w exec "$1" 'exit 0' >/dev/null 2>&1 && return; sleep 5; done
    echo "$1: guest agent did not come back" >&2; exit 1
}
reboot_vm() { w exec "$1" 'Restart-Computer -Force' >/dev/null 2>&1 || true; wait_agent "$1"; }
push_secret() { # VM FILE: copy a secret to C:\transom\secret.txt (the script deletes it)
    w push "$1" "$2" 'C:\transom\secret.txt'
}

mkdir -p -m 700 "$D"
(umask 077; [[ -f $D/alice.pw ]] || new_password > "$D/alice.pw")
w run server >/dev/null
w run win11 >/dev/null
wait_agent server
wait_agent win11

echo "--- server: lab network"
w exec server "$(cat "$SRC/ad-network.ps1"); Set-LabNetwork -Address 10.77.0.1 -Mac 52-54-00-77-00-01"

echo "--- server: domain controller for transom.test"
role=$(w exec server '(Get-CimInstance Win32_ComputerSystem).DomainRole' | tr -d '\r\n')
if (( role < 4 )); then
    w exec server 'Install-WindowsFeature AD-Domain-Services -IncludeManagementTools | Select-Object -ExpandProperty Success'
    # The local Administrator becomes the domain Administrator and must have a valid
    # password, as must Directory Services Restore Mode: random ones that nobody keeps
    # (rdp, a member of Administrators, administers the domain).
    w exec server '
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Web
function Rand { "Aa1" + [Web.Security.Membership]::GeneratePassword(24, 4) }
Set-LocalUser Administrator -Password (ConvertTo-SecureString (Rand) -AsPlainText -Force)
Import-Module ADDSDeployment
Install-ADDSForest -DomainName transom.test -DomainNetbiosName TRANSOM -InstallDns `
    -SafeModeAdministratorPassword (ConvertTo-SecureString (Rand) -AsPlainText -Force) `
    -Force -NoRebootOnCompletion -WarningAction SilentlyContinue | Select-Object Status, Message | Format-List'
    reboot_vm server
    # AD Web Services (the AD PowerShell module) start a little after the agent.
    for _ in $(seq 60); do w exec server 'Get-ADDomain | Out-Null' >/dev/null 2>&1 && break; sleep 5; done
fi

echo "--- server: DNS, users, groups"
push_secret server "$D/alice.pw"
w exec server "$(cat "$SRC/ad-dc.ps1")"

echo "--- win11: lab network, join transom.test"
w exec win11 "$(cat "$SRC/ad-network.ps1"); Set-LabNetwork -Address 10.77.0.2 -Mac 52-54-00-77-00-02 -Dns 10.77.0.1"
domain=$(w exec win11 '(Get-CimInstance Win32_ComputerSystem).Domain' | tr -d '\r\n')
if [[ $domain != transom.test ]]; then
    push_secret win11 "$ROOT/server/password"
    w exec win11 '
$ErrorActionPreference = "Stop"
try {
    $pw = ConvertTo-SecureString (Get-Content -Raw C:\transom\secret.txt).Trim() -AsPlainText -Force
} finally { Remove-Item -Force C:\transom\secret.txt }
$cred = New-Object PSCredential ("TRANSOM\rdp", $pw)
Add-Computer -DomainName transom.test -Credential $cred -Force -WarningAction SilentlyContinue
"joined"'
    reboot_vm win11
fi
# Allow Remote Credential Guard (the ticket-only interactive logon ad-check.sh tests;
# without it Windows says "Account restrictions are preventing this user from signing
# in"). Domains set this by GPO ("Remote host allows delegation of non-exportable
# credentials"); this is the same switch.
w exec win11 'New-ItemProperty HKLM:\System\CurrentControlSet\Control\Lsa -Name DisableRestrictedAdmin -Value 0 -PropertyType DWord -Force | Out-Null'
w exec win11 '
$m = Get-LocalGroupMember "Remote Desktop Users" -ErrorAction SilentlyContinue | Where-Object Name -eq "TRANSOM\alice"
if (-not $m) { Add-LocalGroupMember "Remote Desktop Users" -Member "TRANSOM\alice" }
"{0} in {1}; Remote Desktop Users: {2}" -f $env:COMPUTERNAME, (Get-CimInstance Win32_ComputerSystem).Domain,
    ((Get-LocalGroupMember "Remote Desktop Users" | ForEach-Object Name) -join ", ")'
# Joining may regenerate the RDP certificate (the name now has a DNS suffix).
w fingerprint win11 >/dev/null
echo "AD READY: TRANSOM\\alice (password: $D/alice.pw), DC TRANSOM-SRV.transom.test, member TRANSOM-W11"
