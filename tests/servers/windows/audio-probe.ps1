# SPDX-License-Identifier: GPL-3.0-or-later
# Audio probe for the Windows test VMs, run in the rdp user's session (where the RDP
# "Remote Audio" devices live). Writes its result as JSON to C:\transom\audio-out.json.
#   play SECONDS     440 Hz tone on the default output (redirected to the client)
#   record SECONDS   record the default input (the client's microphone); report RMS
param([Parameter(Mandatory)][ValidateSet('play', 'record')][string]$Action, [int]$Seconds = 6)
$ErrorActionPreference = 'Stop'
$out = 'C:\transom\audio-out.json'
Remove-Item -Force -ErrorAction SilentlyContinue $out
function Done($obj) {
    $obj | ConvertTo-Json -Compress | Set-Content -Encoding UTF8 "$out.tmp"
    Move-Item -Force "$out.tmp" $out
}
Add-Type -TypeDefinition @'
using System;
using System.IO;
using System.Runtime.InteropServices;
using System.Threading;
public static class Audio {
    [StructLayout(LayoutKind.Sequential)] struct WAVEFORMATEX { public ushort tag, ch; public uint rate, bytesPerSec; public ushort align, bits, cb; }
    [StructLayout(LayoutKind.Sequential)] struct WAVEHDR { public IntPtr data; public uint len, recorded; public IntPtr user; public uint flags, loops; public IntPtr next, reserved; }
    [DllImport("winmm.dll")] static extern int waveInOpen(out IntPtr h, uint dev, ref WAVEFORMATEX f, IntPtr cb, IntPtr inst, uint flags);
    [DllImport("winmm.dll")] static extern int waveInPrepareHeader(IntPtr h, IntPtr hdr, int size);
    [DllImport("winmm.dll")] static extern int waveInAddBuffer(IntPtr h, IntPtr hdr, int size);
    [DllImport("winmm.dll")] static extern int waveInStart(IntPtr h);
    [DllImport("winmm.dll")] static extern int waveInStop(IntPtr h);
    [DllImport("winmm.dll")] static extern int waveInReset(IntPtr h);
    [DllImport("winmm.dll")] static extern int waveInUnprepareHeader(IntPtr h, IntPtr hdr, int size);
    [DllImport("winmm.dll")] static extern int waveInClose(IntPtr h);
    [DllImport("winmm.dll")] static extern uint waveInGetNumDevs();

    static WAVEFORMATEX Fmt() { return new WAVEFORMATEX { tag = 1, ch = 2, rate = 48000, bits = 16, align = 4, bytesPerSec = 48000 * 4 }; }

    public static byte[] Tone(int seconds, double hz) {
        int n = 48000 * seconds; var ms = new MemoryStream(); var w = new BinaryWriter(ms);
        w.Write(0x46464952); w.Write(36 + n * 4); w.Write(0x45564157); w.Write(0x20746d66); w.Write(16);
        w.Write((short)1); w.Write((short)2); w.Write(48000); w.Write(48000 * 4); w.Write((short)4); w.Write((short)16);
        w.Write(0x61746164); w.Write(n * 4);
        for (int i = 0; i < n; i++) { short v = (short)(12000 * Math.Sin(2 * Math.PI * hz * i / 48000)); w.Write(v); w.Write(v); }
        return ms.ToArray();
    }

    // Record `seconds` from the default input; returns RMS of the samples (or -code).
    public static double Record(int seconds) {
        if (waveInGetNumDevs() == 0) return -1;
        var f = Fmt(); IntPtr h;
        int rc = waveInOpen(out h, 0xFFFFFFFF, ref f, IntPtr.Zero, IntPtr.Zero, 0); if (rc != 0) return -100 - rc;
        int len = 48000 * 4 * seconds; IntPtr buf = Marshal.AllocHGlobal(len);
        var hdr = new WAVEHDR { data = buf, len = (uint)len };
        int hs = Marshal.SizeOf(typeof(WAVEHDR)); IntPtr ph = Marshal.AllocHGlobal(hs); Marshal.StructureToPtr(hdr, ph, false);
        waveInPrepareHeader(h, ph, hs); waveInAddBuffer(h, ph, hs); waveInStart(h);
        Thread.Sleep(seconds * 1000 + 300);
        waveInStop(h); waveInReset(h);
        hdr = (WAVEHDR)Marshal.PtrToStructure(ph, typeof(WAVEHDR));
        var data = new byte[hdr.recorded]; Marshal.Copy(buf, data, 0, data.Length);
        waveInUnprepareHeader(h, ph, hs); waveInClose(h); Marshal.FreeHGlobal(buf); Marshal.FreeHGlobal(ph);
        double sum = 0; int count = data.Length / 2;
        for (int i = 0; i < count; i++) { double s = BitConverter.ToInt16(data, i * 2); sum += s * s; }
        return count == 0 ? 0 : Math.Sqrt(sum / count);
    }
}
'@
try {
    switch ($Action) {
        'play' {
            $player = New-Object System.Media.SoundPlayer (New-Object IO.MemoryStream (, [Audio]::Tone($Seconds, 440)))
            $player.PlaySync()
            Done @{ t = 'played'; seconds = $Seconds }
        }
        'record' {
            $devices = (Get-CimInstance Win32_SoundDevice | ForEach-Object Name) -join ', '
            Done @{ t = 'recorded'; rms = [int][Audio]::Record($Seconds); devices = $devices }
        }
    }
} catch {
    Done @{ t = 'error'; action = $Action; error = "$_" }
}
