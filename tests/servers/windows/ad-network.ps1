# SPDX-License-Identifier: GPL-3.0-or-later
# Lab network of the Windows test VMs (scripts/winvm.sh, scripts/ad-setup.sh): a static
# address on the lab NIC (found by its MAC), the lab trusted by the firewall (it is a
# private link between two throwaway VMs), and no DNS registration of the user-mode
# NIC: both VMs have 10.0.2.15 there, so a DC that registered it would send clients to
# themselves. Idempotent. Dot-source, then: Set-LabNetwork -Address A -Mac M [-Dns S]
function Set-LabNetwork([string]$Address, [string]$Mac, [string]$Dns) {
    $ErrorActionPreference = 'Stop'
    $lab = Get-NetAdapter | Where-Object MacAddress -eq $Mac
    if (-not $lab) { throw "no lab NIC with MAC $Mac (start the VM with scripts/winvm.sh run)" }
    $wan = Get-NetAdapter | Where-Object { $_.MacAddress -ne $Mac -and $_.Status -eq 'Up' }
    Rename-NetAdapter -InputObject $lab -NewName Lab -ErrorAction SilentlyContinue
    if (-not (Get-NetIPAddress -InterfaceIndex $lab.ifIndex -IPAddress $Address -ErrorAction SilentlyContinue)) {
        Set-NetIPInterface -InterfaceIndex $lab.ifIndex -Dhcp Disabled
        New-NetIPAddress -InterfaceIndex $lab.ifIndex -IPAddress $Address -PrefixLength 24 | Out-Null
    }
    foreach ($nic in $wan) { Set-DnsClient -InterfaceIndex $nic.ifIndex -RegisterThisConnectionsAddress $false }
    if ($Dns) {
        # Every NIC asks the DC, so names in transom.test resolve whichever is tried first
        # (the DC forwards everything else to the user-mode network's resolver).
        foreach ($nic in @($lab) + @($wan)) { Set-DnsClientServerAddress -InterfaceIndex $nic.ifIndex -ServerAddresses $Dns }
    }
    if (-not (Get-NetFirewallRule -Name TransomLab -ErrorAction SilentlyContinue)) {
        New-NetFirewallRule -Name TransomLab -DisplayName 'Transom lab network' -Direction Inbound `
            -RemoteAddress 10.77.0.0/24 -Action Allow | Out-Null
    }
    "{0}: lab {1} ({2})" -f $env:COMPUTERNAME, $Address, (Get-NetAdapter -InterfaceIndex $lab.ifIndex).Status
}
