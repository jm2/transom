#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Inject evdev key presses (and one click) into the local headless mutter via
org.gnome.Mutter.RemoteDesktop, so they reach transom-view-demo exactly like
physical input."""
import time

from gi.repository import Gio, GLib

bus = Gio.bus_get_sync(Gio.BusType.SESSION)


def proxy(path, iface):
    return Gio.DBusProxy.new_sync(bus, 0, None, "org.gnome.Mutter.RemoteDesktop", path, iface)


rd = proxy("/org/gnome/Mutter/RemoteDesktop", "org.gnome.Mutter.RemoteDesktop")
path = rd.call_sync("CreateSession", None, 0, -1).unpack()[0]
s = proxy(path, "org.gnome.Mutter.RemoteDesktop.Session")
s.call_sync("Start", None, 0, -1)
time.sleep(0.3)


def call(method, sig, *args):
    s.call_sync(method, GLib.Variant(sig, args), 0, -1)
    time.sleep(0.03)


# Move to a known spot (clamp to the top-left corner, then to the monitor centre,
# which is inside the session view) and click to focus the remote probe.
call("NotifyPointerMotionRelative", "(dd)", -5000.0, -5000.0)
call("NotifyPointerMotionRelative", "(dd)", 800.0, 500.0)
call("NotifyPointerButton", "(ib)", 0x110, True)
call("NotifyPointerButton", "(ib)", 0x110, False)
time.sleep(0.3)


def tap(code):
    call("NotifyKeyboardKeycode", "(ub)", code, True)
    call("NotifyKeyboardKeycode", "(ub)", code, False)


# Warm-up: mutter drops the very first key event of a new virtual keyboard (seen
# with a plain GTK app too, no Transom involved), so start with a harmless Shift.
tap(42)
for code in [35, 18, 38, 38, 24]:  # h e l l o
    tap(code)
call("NotifyKeyboardKeycode", "(ub)", 42, True)  # Shift_L
tap(2)  # 1 -> exclam
call("NotifyKeyboardKeycode", "(ub)", 42, False)
for code in [105, 106, 102, 111, 96, 97, 63]:  # Left Right Home Delete KP_Enter Control_R F5
    tap(code)
s.call_sync("Stop", None, 0, -1)
print("injected")
