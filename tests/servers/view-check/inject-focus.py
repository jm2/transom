#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Give transom-view-demo keyboard focus in the headless mutter, like a user clicking
into the session, and keep it: the clipboard bridge only exchanges content while the
session has focus. The RemoteDesktop session stays open (its virtual keyboard is what
the window has focus with) until the container exits."""
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


# Top-left corner, then into the window, and click.
call("NotifyPointerMotionRelative", "(dd)", -5000.0, -5000.0)
call("NotifyPointerMotionRelative", "(dd)", 800.0, 500.0)
call("NotifyPointerButton", "(ib)", 0x110, True)
call("NotifyPointerButton", "(ib)", 0x110, False)
# mutter creates the virtual keyboard on its first key event, and keyboard focus (an
# active window) needs a keyboard: tap Shift, which does nothing on the remote.
for _ in range(2):
    call("NotifyKeyboardKeycode", "(ub)", 42, True)
    call("NotifyKeyboardKeycode", "(ub)", 42, False)
print("focused", flush=True)
time.sleep(600)
