# SPDX-License-Identifier: GPL-3.0-or-later
# Windows test VM: silent install of dist\transom-setup.exe (with the .rdp/rdp:// task)
# and checks of the files, registry associations, uninstall entry and shortcut.
param([switch]$Install)
if ($Install) {
    Start-Process C:\transom\build\src\dist\transom-setup.exe -ArgumentList '/VERYSILENT','/SUPPRESSMSGBOXES','/NORESTART','/TASKS=rdpfiles','/LOG=C:\transom\build\install.log' -Wait
}
$p = "$env:ProgramFiles\Transom\bin\transom.exe"
function Def($k) { $i = Get-Item $k -ErrorAction SilentlyContinue; if ($i) { $i.GetValue('') } }
"installed: $(Test-Path $p) $((Get-Item $p -ErrorAction SilentlyContinue).Length)"
"rdp handler: $(Def 'HKLM:\Software\Classes\rdp\shell\open\command')"
"rdp URL Protocol: $(@((Get-Item HKLM:\Software\Classes\rdp -ErrorAction SilentlyContinue).GetValueNames()) -contains 'URL Protocol')"
"progid: $(Def 'HKLM:\Software\Classes\Transom.rdp\shell\open\command')"
"openwith: $(@((Get-Item HKLM:\Software\Classes\.rdp\OpenWithProgids -ErrorAction SilentlyContinue).GetValueNames()) -contains 'Transom.rdp')"
"uninstall entry: $((Get-ChildItem HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall | Get-ItemProperty | Where-Object DisplayName -like 'Transom*').DisplayName)"
"start menu: $(Test-Path "$env:ProgramData\Microsoft\Windows\Start Menu\Programs\Transom\Transom.lnk")"
