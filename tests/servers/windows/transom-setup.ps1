# SPDX-License-Identifier: GPL-3.0-or-later
# First-logon setup of a throwaway Windows test VM (run once by autounattend.xml).
# Configures the RDP features Transom tests, installs the virtio guest tools (the
# QEMU guest agent lets scripts/winvm.sh run commands in the guest), removes the
# plaintext answer-file copies, then powers off so the host knows the install is done.
param([ValidateSet('client', 'server')][string]$Role = 'client')
$ErrorActionPreference = 'Continue'
Start-Transcript -Path C:\transom-setup.log -Append

# A policy failure must not skip the password cleanup below; it is logged, and
# `winvm.sh configure` can re-apply the policy later.
try { & (Join-Path $PSScriptRoot 'transom-policy.ps1') } catch { Write-Warning "policy: $_" }

if ($Role -eq 'server') {
    schtasks /Change /TN '\Microsoft\Windows\Server Manager\ServerManager' /Disable | Out-Null
    # RD Gateway role (configured later by the Phase 4 Gateway tests: cert, CAP, RAP).
    Install-WindowsFeature RDS-Gateway -IncludeManagementTools | Format-List
}

# virtio guest tools (drivers + QEMU guest agent), from the virtio-win CD.
$tools = Get-PSDrive -PSProvider FileSystem |
    ForEach-Object { Join-Path $_.Root 'virtio-win-guest-tools.exe' } |
    Where-Object { Test-Path $_ } | Select-Object -First 1
if ($tools) {
    Start-Process -FilePath $tools -ArgumentList '/install', '/quiet', '/norestart' -Wait
} else {
    Write-Warning 'virtio-win CD not found; no guest agent'
}

# The answer file held the password in plain text: remove every copy Windows kept, and
# the auto-logon password.
Remove-Item -Force -ErrorAction SilentlyContinue C:\Windows\Panther\unattend.xml,
    C:\Windows\Panther\Unattend\unattend.xml, C:\Windows\Panther\Unattend\autounattend.xml,
    C:\Windows\System32\Sysprep\unattend.xml
$wl = 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon'
Remove-ItemProperty -Path $wl -Name DefaultPassword -ErrorAction SilentlyContinue
Set-ItemProperty -Path $wl -Name AutoAdminLogon -Value '0'

Set-Content -Path C:\transom-ready.txt -Value (Get-Date -Format o)
Stop-Transcript
# Tell scripts/winvm.sh the install completed (COM1 is logged to a file on the host).
try {
    $com = [System.IO.Ports.SerialPort]::new('COM1', 115200)
    $com.Open(); $com.WriteLine('TRANSOM-READY'); $com.Close()
} catch { }
Stop-Computer -Force
