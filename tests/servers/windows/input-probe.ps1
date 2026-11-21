# SPDX-License-Identifier: GPL-3.0-or-later
# Windows counterpart of tests/servers/grd-headless/probe/input-probe.py: logs the
# keyboard/mouse input that reaches the RDP session as JSON lines in the same format
# (key names are X keysym names so check-input.py can compare both servers).
# Low-level hooks see the input before any window does, and swallow it, so the test
# script's clicks and keys (F5, Delete, ...) have no side effects on the desktop.
# Runs in the user's interactive session (started by scripts/input-check.sh through
# a scheduled task); exits after $Seconds.
param([string]$Log = 'C:\transom\input-probe.log', [int]$Seconds = 60)
Add-Type -ReferencedAssemblies System.Windows.Forms -TypeDefinition @'
using System;
using System.IO;
using System.Runtime.InteropServices;
using System.Windows.Forms;

public static class TransomProbe {
    delegate IntPtr HookProc(int code, IntPtr wParam, IntPtr lParam);
    [DllImport("user32.dll")] static extern IntPtr SetWindowsHookEx(int id, HookProc fn, IntPtr mod, uint thread);
    [DllImport("user32.dll")] static extern IntPtr CallNextHookEx(IntPtr h, int code, IntPtr w, IntPtr l);
    [DllImport("kernel32.dll")] static extern IntPtr GetModuleHandle(string name);
    [DllImport("user32.dll")] static extern bool SetProcessDPIAware();

    [StructLayout(LayoutKind.Sequential)] struct KBD { public uint vk, scan, flags, time; public IntPtr extra; }
    [StructLayout(LayoutKind.Sequential)] struct MSLL { public int x, y; public uint data, flags, time; public IntPtr extra; }

    static StreamWriter log;
    static HookProc kbProc = KeyHook, msProc = MouseHook; // keep delegates alive
    static bool shift;

    static void Emit(string json) { log.WriteLine(json); log.Flush(); }

    static string Name(uint vk, uint scan, bool ext) {
        if (vk >= 0x41 && vk <= 0x5A) return ((char)(vk + 32)).ToString();
        switch (vk) {
            case 0xE7: // VK_PACKET: Unicode input, the character is in scan
                if (scan == 0xE9) return "eacute";
                return ((char)scan).ToString();
            case 0x31: return shift ? "exclam" : "1";
            case 0xA0: return "Shift_L";   case 0xA1: return "Shift_R";
            case 0xA2: return "Control_L"; case 0xA3: return "Control_R";
            case 0x25: return "Left";  case 0x27: return "Right";
            case 0x26: return "Up";    case 0x28: return "Down";
            case 0x24: return "Home";  case 0x23: return "End";
            case 0x2E: return "Delete";
            case 0x0D: return ext ? "KP_Enter" : "Return";
        }
        if (vk >= 0x70 && vk <= 0x7B) return "F" + (vk - 0x6F);
        return "vk" + vk.ToString("X2");
    }

    static IntPtr KeyHook(int code, IntPtr w, IntPtr l) {
        if (code >= 0) {
            var k = (KBD)Marshal.PtrToStructure(l, typeof(KBD));
            int msg = w.ToInt32();
            bool down = msg == 0x100 || msg == 0x104;
            bool ext = (k.flags & 1) != 0;
            if (k.vk == 0xA0 || k.vk == 0xA1) shift = down;
            Emit(string.Format("{{\"t\":\"key\",\"name\":\"{0}\",\"down\":{1},\"vk\":{2},\"scan\":{3},\"ext\":{4}}}",
                Name(k.vk, k.scan, ext), down ? "true" : "false", k.vk, k.scan, ext ? "true" : "false"));
            return (IntPtr)1;
        }
        return CallNextHookEx(IntPtr.Zero, code, w, l);
    }

    static IntPtr MouseHook(int code, IntPtr w, IntPtr l) {
        if (code >= 0) {
            var m = (MSLL)Marshal.PtrToStructure(l, typeof(MSLL));
            int msg = w.ToInt32();
            int button = 0; bool down = false;
            switch (msg) {
                case 0x201: button = 1; down = true; break; case 0x202: button = 1; break;
                case 0x204: button = 3; down = true; break; case 0x205: button = 3; break;
                case 0x207: button = 2; down = true; break; case 0x208: button = 2; break;
            }
            short delta = (short)(m.data >> 16);
            if (button != 0)
                Emit(string.Format("{{\"t\":\"button\",\"button\":{0},\"x\":{1},\"y\":{2},\"down\":{3}}}",
                    button, m.x, m.y, down ? "true" : "false"));
            else if (msg == 0x20A) // vertical wheel: + is away from the user (X: dy -1)
                Emit(string.Format("{{\"t\":\"scroll\",\"dx\":0,\"dy\":{0}}}", -delta / 120.0));
            else if (msg == 0x20E) // horizontal wheel: + is right
                Emit(string.Format("{{\"t\":\"scroll\",\"dx\":{0},\"dy\":0}}", delta / 120.0));
            if (msg != 0x200) return (IntPtr)1; // let plain moves through
        }
        return CallNextHookEx(IntPtr.Zero, code, w, l);
    }

    public static void Run(string path, int seconds) {
        SetProcessDPIAware(); // physical (= RDP desktop) coordinates
        log = new StreamWriter(path, false);
        IntPtr mod = GetModuleHandle(null);
        SetWindowsHookEx(13, kbProc, mod, 0); // WH_KEYBOARD_LL
        SetWindowsHookEx(14, msProc, mod, 0); // WH_MOUSE_LL
        Emit("{\"t\":\"ready\"}");
        var t = new Timer { Interval = seconds * 1000 };
        t.Tick += (s, e) => Application.Exit();
        t.Start();
        Application.Run();
        log.Close();
    }
}
'@
[TransomProbe]::Run($Log, $Seconds)
