# SPDX-License-Identifier: GPL-3.0-or-later
# Device redirection probe for the Windows test VMs (Phase 4b), run in the rdp user's
# session (`winvm.sh session-run`), where redirected devices appear. Writes JSON to
# C:\transom\device-out.json.
#   drive -Name N      read \\tsclient\N\hello.txt, write \\tsclient\N\from-windows.txt
#   printers           redirected printers (Get-Printer)
#   print              print a line of text to the first redirected printer
#   serial -Name COMn  open COMn, write "hello COM", read a reply line for 10 s
#   smartcard          smart card readers the session sees (WinSCard)
#   usb -Id VID:PID    is a device with that ID present (PnP)?
param([Parameter(Mandatory)][string]$Action, [string]$Name, [string]$Id)
$ErrorActionPreference = 'Stop'
$out = 'C:\transom\device-out.json'
Remove-Item -Force -ErrorAction SilentlyContinue $out
function Done($obj) {
    $obj | ConvertTo-Json -Depth 4 -Compress | Set-Content -Encoding UTF8 "$out.tmp"
    Move-Item -Force "$out.tmp" $out
}
try {
    switch ($Action) {
        'drive' {
            $root = "\\tsclient\$Name"
            $text = Get-Content -Raw "$root\hello.txt"
            Set-Content -NoNewline -Encoding ASCII "$root\from-windows.txt" 'written by Windows'
            Done @{ t = 'drive'; read = $text.Trim(); listing = @(Get-ChildItem $root | ForEach-Object Name) }
        }
        'printers' {
            Done @{ t = 'printers'; printers = @(Get-Printer | Where-Object { $_.Name -like '*redirected*' } |
                ForEach-Object { @{ name = $_.Name; driver = $_.DriverName; port = $_.PortName } }) }
        }
        'print' {
            $p = Get-Printer | Where-Object { $_.Name -like '*redirected*' } | Select-Object -First 1
            if (-not $p) { throw 'no redirected printer' }
            'TRANSOM PRINT TEST' | Out-Printer -Name $p.Name
            Done @{ t = 'print'; printer = $p.Name }
        }
        'serial' {
            $tsclient = (cmd /c "net view \\tsclient 2>&1") -join ' '
            $mode = (cmd /c "mode $Name 2>&1") -join ' '
            $mapped = ''
            if ($mode -match 'Illegal|not|invalid') {
                # Not a session DOS device yet: map the redirected port like a user would.
                $mapped = (cmd /c "net use ${Name}: \\tsclient\$Name 2>&1") -join ' '
            }
            $p = New-Object System.IO.Ports.SerialPort $Name, 9600
            $p.ReadTimeout = 10000
            $p.Open()
            $p.WriteLine('hello COM')
            $reply = try { $p.ReadLine().Trim() } catch { "(no reply: $_)" }
            $p.Close()
            Done @{ t = 'serial'; port = $Name; reply = $reply; mapped = $mapped; mode = $mode }
        }
        'smartcard' {
            Add-Type @'
using System; using System.Runtime.InteropServices; using System.Text;
public static class SCard {
    [DllImport("winscard.dll")] public static extern int SCardEstablishContext(int scope, IntPtr r1, IntPtr r2, out IntPtr ctx);
    [DllImport("winscard.dll", CharSet = CharSet.Unicode)] public static extern int SCardListReadersW(IntPtr ctx, string groups, char[] readers, ref int len);
    [DllImport("winscard.dll")] public static extern int SCardReleaseContext(IntPtr ctx);
    public static string[] Readers() {
        IntPtr ctx; int rc = SCardEstablishContext(2, IntPtr.Zero, IntPtr.Zero, out ctx);
        if (rc != 0) return new[] { "error:" + rc.ToString("x") };
        int len = 0; rc = SCardListReadersW(ctx, null, null, ref len);
        if (rc != 0) { SCardReleaseContext(ctx); return new[] { "error:" + rc.ToString("x") }; }
        var buf = new char[len]; SCardListReadersW(ctx, null, buf, ref len); SCardReleaseContext(ctx);
        return new string(buf, 0, len).Split(new[] { '\0' }, StringSplitOptions.RemoveEmptyEntries);
    }
}
'@
            Done @{ t = 'smartcard'; readers = @([SCard]::Readers()) }
        }
        'usb' {
            $vid, $pid_ = $Id.ToUpper().Split(':')
            # Win32_PnPEntity with a timeout: a device that is still installing can stall
            # PnP queries, and the probe must still answer.
            $devs = @(Get-CimInstance Win32_PnPEntity -OperationTimeoutSec 20 -ErrorAction SilentlyContinue |
                Where-Object { $_.PNPDeviceID -like "USB\VID_$vid&PID_$pid_*" } |
                ForEach-Object { @{ id = $_.PNPDeviceID; name = $_.Name; status = "$($_.Status)"; problem = $_.ConfigManagerErrorCode } })
            Done @{ t = 'usb'; devices = $devs }
        }
        default { throw "unknown action $Action" }
    }
} catch {
    Done @{ t = 'error'; action = $Action; error = "$_"; tsclient = $tsclient; mode = $mode; mapped = $mapped }
}
