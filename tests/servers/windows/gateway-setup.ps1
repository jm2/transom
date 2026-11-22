# SPDX-License-Identifier: GPL-3.0-or-later
# RD Gateway on the Server test VM (role installed by transom-setup.ps1): self-signed
# certificate, a connection policy (CAP) and a resource policy (RAP). Idempotent.
# Prints the gateway certificate's SHA-256 fingerprint, which scripts/gateway-check.sh
# reads through the guest agent (out of band, so trusting it is not TOFU).
#
# The policies use a dedicated local group: with BUILTIN\Administrators the RAP check
# fails (event 301, error 23002), as an NTLM network logon of a local admin carries a
# UAC-filtered token. Targets are a gateway-managed group (the VM itself, by name and
# loopback): "any resource" was refused the same way on this workgroup server.
$ErrorActionPreference = 'Stop'
Import-Module RemoteDesktopServices
$cert = Get-ChildItem Cert:\LocalMachine\My | Where-Object Subject -eq 'CN=transom-gateway' | Select-Object -First 1
if (-not $cert) {
    $cert = New-SelfSignedCertificate -Subject 'CN=transom-gateway' -DnsName 'localhost', 'TRANSOM-SRV' `
        -CertStoreLocation Cert:\LocalMachine\My -NotAfter (Get-Date).AddYears(2)
}
Set-Item RDS:\GatewayServer\SSLCertificate\Thumbprint -Value $cert.Thumbprint
# Once the server is the test domain's DC (scripts/ad-setup.sh) local groups are gone:
# use the domain group TransomGateway (created there) and allow the domain member too.
$dc = (Get-CimInstance Win32_ComputerSystem).DomainRole -ge 4
$computers = @($env:COMPUTERNAME, 'localhost', '127.0.0.1')
if ($dc) {
    $group = 'TransomGateway@TRANSOM'
    $computers += 'TRANSOM-SRV.transom.test', 'TRANSOM-W11', 'TRANSOM-W11.transom.test'
} else {
    if (-not (Get-LocalGroup TransomGateway -ErrorAction SilentlyContinue)) {
        New-LocalGroup TransomGateway -Description 'Transom RD Gateway test users' | Out-Null
    }
    if (-not (Get-LocalGroupMember TransomGateway -ErrorAction SilentlyContinue | Where-Object Name -like '*\rdp')) {
        Add-LocalGroupMember TransomGateway -Member rdp
    }
    $group = "TransomGateway@$env:COMPUTERNAME"
}
# Recreated each run (the RAP refers to the computer group, so it goes first).
foreach ($p in 'CAP\TransomCAP', 'RAP\TransomRAP', 'GatewayManagedComputerGroups\TransomTargets') {
    if (Test-Path "RDS:\GatewayServer\$p") { Remove-Item -Recurse "RDS:\GatewayServer\$p" }
}
New-Item RDS:\GatewayServer\GatewayManagedComputerGroups -Name TransomTargets `
    -Description 'Transom test targets' -Computers $computers | Out-Null
New-Item RDS:\GatewayServer\CAP -Name TransomCAP -UserGroups $group -AuthMethod 1 | Out-Null
New-Item RDS:\GatewayServer\RAP -Name TransomRAP -UserGroups $group -ComputerGroupType 0 `
    -ComputerGroup TransomTargets | Out-Null
Restart-Service TSGateway
[BitConverter]::ToString([Security.Cryptography.SHA256]::Create().ComputeHash($cert.RawData)).Replace('-', ':')
