# SPDX-License-Identifier: GPL-3.0-or-later
# Domain controller side of scripts/ad-setup.sh (runs on TRANSOM-SRV once it is the DC
# of transom.test). Reads TRANSOM\alice's password from C:\transom\secret.txt and
# deletes it. Idempotent.
$ErrorActionPreference = 'Stop'
try {
    $pw = ConvertTo-SecureString (Get-Content -Raw C:\transom\secret.txt).Trim() -AsPlainText -Force
} finally { Remove-Item -Force -ErrorAction SilentlyContinue C:\transom\secret.txt }
Import-Module ActiveDirectory

# DNS: forward other names to the user-mode network's resolver; drop any record of the
# user-mode address (10.0.2.15 is every VM's own address there).
if (-not (Get-DnsServerForwarder).IPAddress) { Set-DnsServerForwarder -IPAddress 10.0.2.3 }
Get-DnsServerResourceRecord -ZoneName transom.test -RRType A |
    Where-Object { $_.RecordData.IPv4Address.IPAddressToString -like '10.0.2.*' } |
    Remove-DnsServerResourceRecord -ZoneName transom.test -Force
foreach ($z in 'DomainDnsZones', 'ForestDnsZones') {
    Get-DnsServerResourceRecord -ZoneName transom.test -Name $z -RRType A -ErrorAction SilentlyContinue |
        Where-Object { $_.RecordData.IPv4Address.IPAddressToString -like '10.0.2.*' } |
        Remove-DnsServerResourceRecord -ZoneName transom.test -Force
}

# The test user: a plain domain user (not an admin anywhere), so the checks exercise
# what a real user has. The password is reset each run to the host's copy.
if (Get-ADUser -Filter "SamAccountName -eq 'alice'") {
    Set-ADAccountPassword alice -Reset -NewPassword $pw
} else {
    New-ADUser alice -UserPrincipalName alice@transom.test -AccountPassword $pw -Enabled $true `
        -PasswordNeverExpires $true -DisplayName 'Alice (Transom test)'
}
# RD Gateway users (tests/servers/windows/gateway-setup.ps1 uses this group).
if (-not (Get-ADGroup -Filter "Name -eq 'TransomGateway'")) {
    New-ADGroup TransomGateway -GroupScope Global -Description 'Transom RD Gateway test users'
}
Add-ADGroupMember TransomGateway -Members alice, rdp
"DC {0}; TransomGateway: {1}; A records: {2}" -f (Get-ADDomain).DNSRoot,
    ((Get-ADGroupMember TransomGateway | ForEach-Object SamAccountName) -join ', '),
    ((Get-DnsServerResourceRecord -ZoneName transom.test -RRType A | ForEach-Object {
        "$($_.HostName)=$($_.RecordData.IPv4Address)" }) -join ' ')
