# SPDX-License-Identifier: GPL-3.0-or-later
# RDP host policy for the Windows test VMs. Idempotent: run at first logon by
# transom-setup.ps1, and re-applied to an installed VM by `scripts/winvm.sh configure`.
$ErrorActionPreference = 'Stop'

function Set-Policy($Path, $Name, $Value) {
    # New-Item -Force on an existing key recreates it empty, so only create it if missing.
    if (-not (Test-Path $Path)) { New-Item -Path $Path -Force | Out-Null }
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
# Server: no "Shutdown Event Tracker" dialog after an unclean stop (it covers the screen).
Set-Policy 'HKLM:\SOFTWARE\Policies\Microsoft\Windows NT\Reliability' ShutdownReasonOn 0

# Report what is in effect, so `winvm.sh configure` shows a verifiable result.
Get-ItemProperty $ts | Select-Object AVC444ModePreferred, AVCHardwareEncodePreferred,
    fDisableAudioCapture, fDisableCdm, fDisableClip, fDisableCameraRedir | Format-List
