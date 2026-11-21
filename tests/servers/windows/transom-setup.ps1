# SPDX-License-Identifier: GPL-3.0-or-later
# First-logon setup of a throwaway Windows test VM (run once by autounattend.xml).
# Configures the RDP features Transom tests, installs the virtio guest tools (the
# QEMU guest agent lets scripts/winvm.sh run commands in the guest), removes the
# plaintext answer-file copies, then powers off so the host knows the install is done.
param([ValidateSet('client', 'server')][string]$Role = 'client')
$ErrorActionPreference = 'Continue'
Start-Transcript -Path C:\transom-setup.log -Append

function Set-Policy($Path, $Name, $Value) {
    New-Item -Path $Path -Force | Out-Null
    New-ItemProperty -Path $Path -Name $Name -Value $Value -PropertyType DWord -Force | Out-Null
}
$ts = 'HKLM:\SOFTWARE\Policies\Microsoft\Windows NT\Terminal Services'

# Graphics: H.264/AVC444 for the whole desktop (exercises Transom's H.264 path), and
# hardware encode when a GPU is ever attached.
Set-Policy $ts AVC444ModePreferred 1
Set-Policy $ts AVCHardwareEncodePreferred 1
# Redirections Transom implements: audio capture (mic), clipboard, drives (file copy),
# camera. These are allowed by default; set explicitly so a policy change elsewhere
# can't silently turn a test into a no-op.
Set-Policy $ts fDisableAudioCapture 0
Set-Policy $ts fDisableCdm 0
Set-Policy $ts fDisableClip 0
Set-Policy $ts fDisableCameraRedir 0
# RemoteApp without a Connection Broker (Transom's RemoteApp tests): allow any program.
Set-Policy 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Terminal Server\TSAppAllowList' fDisabledAllowList 1

# Keep a test VM quiet and awake: no sleep, no surprise update reboots mid-test.
powercfg /change standby-timeout-ac 0
powercfg /change monitor-timeout-ac 0
powercfg /hibernate off
Set-Policy 'HKLM:\SOFTWARE\Policies\Microsoft\Windows\WindowsUpdate\AU' NoAutoUpdate 1

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
