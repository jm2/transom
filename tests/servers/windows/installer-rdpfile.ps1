# SPDX-License-Identifier: GPL-3.0-or-later
# Windows test VM: open an .rdp file through the installed Transom.rdp ProgID command
# (open), and a silent uninstall that must remove every registration (uninstall).
param([string]$Step)
switch ($Step) {
  'open' {
    Stop-Process -Name transom -Force -ErrorAction SilentlyContinue
    "full address:s:10.77.0.2`r`nusername:s:TRANSOM\alice`r`n" | Set-Content -Encoding Unicode C:\transom\test.rdp
    $cmd = (Get-Item 'HKLM:\Software\Classes\Transom.rdp\shell\open\command').GetValue('')
    $line = $cmd.Replace('%1', 'C:\transom\test.rdp')
    Set-Content -Encoding ASCII C:\transom\open-rdp.cmd "@echo off`r`nstart `"`" $line`r`n"
    "registered: $line"
  }
  'uninstall' {
    Stop-Process -Name transom -Force -ErrorAction SilentlyContinue
    $u = (Get-ChildItem HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall | Get-ItemProperty | Where-Object DisplayName -like 'Transom*').UninstallString.Trim('"')
    Start-Process $u -ArgumentList '/VERYSILENT','/SUPPRESSMSGBOXES','/NORESTART' -Wait
    Start-Sleep 3
    "exe gone: $(-not (Test-Path "$env:ProgramFiles\Transom\bin\transom.exe"))"
    "rdp key gone: $(-not (Test-Path HKLM:\Software\Classes\rdp))"
    "progid gone: $(-not (Test-Path HKLM:\Software\Classes\Transom.rdp))"
    "openwith value gone: $(-not (@((Get-Item HKLM:\Software\Classes\.rdp\OpenWithProgids -ErrorAction SilentlyContinue).GetValueNames()) -contains 'Transom.rdp'))"
    "mstsc .rdp default intact: $((Get-Item HKLM:\Software\Classes\.rdp -ErrorAction SilentlyContinue).GetValue(''))"
  }
}
