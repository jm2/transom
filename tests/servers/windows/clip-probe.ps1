# SPDX-License-Identifier: GPL-3.0-or-later
# Clipboard probe for the Windows test VMs (one action per run, in the rdp user's
# session via `winvm.sh session-run`; the session's rdpclip bridges to the client).
# Writes its result as JSON to C:\transom\clip-out.json.
#   formats | paste-text | copy-text | paste-image | copy-image | copy-files
#   open-paste-target | hash-pasted
# copy-text copies the UTF-8 contents of C:\transom\clip-in.txt (no quoting issues).
param([Parameter(Mandatory)][string]$Action)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
$out = 'C:\transom\clip-out.json'
Remove-Item -Force -ErrorAction SilentlyContinue $out

function HashTree([string[]]$Roots) {
    # {relative name (with /): sha256}, like hash_tree() in the grd probe and the demo.
    $h = [ordered]@{}
    foreach ($r in $Roots) {
        $base = Split-Path $r
        $files = if (Test-Path $r -PathType Container) { Get-ChildItem $r -Recurse -File } else { Get-Item $r }
        foreach ($f in $files) {
            $rel = $f.FullName.Substring($base.Length).TrimStart('\').Replace('\', '/')
            $h[$rel] = (Get-FileHash $f.FullName -Algorithm SHA256).Hash.ToLower()
        }
    }
    $h
}

function Done($obj) {
    $obj | ConvertTo-Json -Depth 4 -Compress | Set-Content -Encoding UTF8 "$out.tmp"
    Move-Item -Force "$out.tmp" $out
}

try {
    switch ($Action) {
        'formats' { Done @{ t = 'formats'; formats = [Windows.Forms.Clipboard]::GetDataObject().GetFormats() } }
        'paste-text' { Done @{ t = 'paste'; text = (Get-Clipboard -Raw) } }
        'copy-text' {
            $text = [IO.File]::ReadAllText('C:\transom\clip-in.txt', [Text.Encoding]::UTF8)
            Set-Clipboard -Value $text; Done @{ t = 'copied'; text = $text }
        }
        'paste-image' {
            $img = [Windows.Forms.Clipboard]::GetImage()
            if ($img) { Done @{ t = 'paste-image'; w = $img.Width; h = $img.Height } }
            else { Done @{ t = 'paste-image'; error = 'no image'; formats = [Windows.Forms.Clipboard]::GetDataObject().GetFormats() } }
        }
        'copy-image' {
            $bmp = New-Object Drawing.Bitmap 64, 48
            for ($y = 0; $y -lt 48; $y++) { for ($x = 0; $x -lt 64; $x++) {
                $bmp.SetPixel($x, $y, [Drawing.Color]::FromArgb($x * 4, $y * 5, 128)) } }
            [Windows.Forms.Clipboard]::SetImage($bmp)
            Done @{ t = 'copied-image'; w = 64; h = 48 }
        }
        'copy-files' {
            $src = 'C:\transom\clip-src'
            Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $src
            New-Item -ItemType Directory -Force "$src\folder" | Out-Null
            [IO.File]::WriteAllText("$src\one.txt", "remote file one`n")
            $blob = New-Object byte[] (3MB + 17); (New-Object Random 7).NextBytes($blob)
            [IO.File]::WriteAllBytes("$src\folder\blob.bin", $blob)
            Set-Clipboard -Path "$src\one.txt", "$src\folder"
            Done @{ t = 'copied-files'; sha = (HashTree "$src\one.txt", "$src\folder") }
        }
        'open-paste-target' {
            # Files from the client are virtual (FileGroupDescriptorW + FileContents),
            # which only a real paste handler consumes: open an empty Explorer window
            # (closing any others) for the check to paste into with Ctrl+V.
            $dst = 'C:\transom\pasted'
            Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $dst
            New-Item -ItemType Directory -Force $dst | Out-Null
            Add-Type @'
using System; using System.Runtime.InteropServices;
public static class Win {
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr FindWindow(string cls, string title);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
}
'@
            # Dialogs that would sit over the click point (e.g. Shutdown Event Tracker).
            $h = [Win]::FindWindow($null, 'Shutdown Event Tracker')
            if ($h -ne [IntPtr]::Zero) { [Win]::PostMessage($h, 0x111, [IntPtr]2, [IntPtr]::Zero) | Out-Null }  # WM_COMMAND IDCANCEL
            $shell = New-Object -ComObject Shell.Application
            $shell.Windows() | ForEach-Object { $_.Quit() }
            Start-Sleep 1
            Start-Process explorer.exe $dst
            # Maximize it and bring it to the front, so a click anywhere lands in it.
            for ($i = 0; $i -lt 20; $i++) {
                Start-Sleep -Milliseconds 300
                $w = $shell.Windows() | Where-Object { $_.Document.Folder.Self.Path -eq $dst } | Select-Object -First 1
                if ($w) { $hw = [IntPtr]$w.HWND; [Win]::ShowWindow($hw, 3) | Out-Null; [Win]::SetForegroundWindow($hw) | Out-Null; break }
            }
            Done @{ t = 'opened'; path = $dst; found = [bool]$w }
        }
        'hash-pasted' {
            $roots = Get-ChildItem 'C:\transom\pasted' | ForEach-Object FullName
            Done @{ t = 'pasted-files'; sha = $(if ($roots) { HashTree $roots } else { @{} }) }
        }
        default { throw "unknown action $Action" }
    }
} catch {
    Done @{ t = 'error'; action = $Action; error = "$_" }
}
