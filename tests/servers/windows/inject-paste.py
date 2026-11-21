#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Click into the remote session (an Explorer window maximized in it) and press
Ctrl+V through the local headless mutter, i.e. through GTK and TransomView exactly
like a user's paste."""
import time

from gi.repository import Gio, GLib

bus = Gio.bus_get_sync(Gio.BusType.SESSION)
proxy = lambda path, iface: Gio.DBusProxy.new_sync(bus, 0, None, "org.gnome.Mutter.RemoteDesktop", path, iface)
rd = proxy("/org/gnome/Mutter/RemoteDesktop", "org.gnome.Mutter.RemoteDesktop")
s = proxy(rd.call_sync("CreateSession", None, 0, -1).unpack()[0], "org.gnome.Mutter.RemoteDesktop.Session")
s.call_sync("Start", None, 0, -1)
time.sleep(0.3)


def call(method, sig, *args):
    s.call_sync(method, GLib.Variant(sig, args), 0, -1)
    time.sleep(0.05)


call("NotifyPointerMotionRelative", "(dd)", -5000.0, -5000.0)
call("NotifyPointerMotionRelative", "(dd)", 500.0, 650.0)  # file list of the maximized window
call("NotifyPointerButton", "(ib)", 0x110, True)
call("NotifyPointerButton", "(ib)", 0x110, False)
time.sleep(0.5)
call("NotifyKeyboardKeycode", "(ub)", 42, True)   # warm-up (mutter drops the first key)
call("NotifyKeyboardKeycode", "(ub)", 42, False)
call("NotifyKeyboardKeycode", "(ub)", 29, True)   # Ctrl
call("NotifyKeyboardKeycode", "(ub)", 47, True)   # V
call("NotifyKeyboardKeycode", "(ub)", 47, False)
call("NotifyKeyboardKeycode", "(ub)", 29, False)
s.call_sync("Stop", None, 0, -1)
print("injected")
