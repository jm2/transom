#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Moves keyboard focus around the headless mutter for scripts/clipboard-check.sh's
focus checks, like a user clicking between windows. A RemoteDesktop session clicks
where it's told, and a second application ("other") copies text while the session is
in the background, as a password manager would. Commands, one per line, appended to
/tmp/clip-focus-cmd:
  click X Y        click at X,Y (logical pixels from the top-left of the desktop)
  other-show       show the other application, fullscreen (it covers the session)
  other-hide       hide it again
  other-copy TEXT  the other application copies TEXT (as text/plain and text/html)
"""
import os
import time

os.environ.setdefault("WAYLAND_DISPLAY", "wayland-0")
os.environ["GDK_BACKEND"] = "wayland"

import gi  # noqa: E402

gi.require_version("Gtk", "4.0")
gi.require_version("Gdk", "4.0")
from gi.repository import Gdk, Gio, GLib, Gtk  # noqa: E402

CMD = "/tmp/clip-focus-cmd"
bus = Gio.bus_get_sync(Gio.BusType.SESSION)


def proxy(path, iface):
    return Gio.DBusProxy.new_sync(bus, 0, None, "org.gnome.Mutter.RemoteDesktop", path, iface)


rd = proxy("/org/gnome/Mutter/RemoteDesktop", "org.gnome.Mutter.RemoteDesktop")
session = proxy(rd.call_sync("CreateSession", None, 0, -1).unpack()[0],
                "org.gnome.Mutter.RemoteDesktop.Session")
session.call_sync("Start", None, 0, -1)
time.sleep(0.3)


def call(method, sig, *args):
    session.call_sync(method, GLib.Variant(sig, args), 0, -1)
    time.sleep(0.03)


# mutter creates the virtual keyboard on its first key event, and keyboard focus (an
# active window) needs a keyboard: tap Shift, which does nothing anywhere.
for _ in range(2):
    call("NotifyKeyboardKeycode", "(ub)", 42, True)
    call("NotifyKeyboardKeycode", "(ub)", 42, False)

Gtk.init()
other = Gtk.Window(title="other application")
other.connect("notify::is-active", lambda w, _p: print(f"other active={w.is_active()}", flush=True))


def click(x, y):
    call("NotifyPointerMotionRelative", "(dd)", -10000.0, -10000.0)
    call("NotifyPointerMotionRelative", "(dd)", float(x), float(y))
    call("NotifyPointerButton", "(ib)", 0x110, True)
    call("NotifyPointerButton", "(ib)", 0x110, False)


def copy(text):
    html = f"<b>{GLib.markup_escape_text(text)}</b>"
    other.get_clipboard().set_content(Gdk.ContentProvider.new_union([
        Gdk.ContentProvider.new_for_bytes("text/plain;charset=utf-8", GLib.Bytes.new(text.encode())),
        Gdk.ContentProvider.new_for_bytes("text/html", GLib.Bytes.new(html.encode())),
    ]))


offset = 0  # bytes of CMD already dispatched (the file is append-only)


def poll():
    global offset
    try:
        with open(CMD, "rb") as f:
            f.seek(offset)
            data = f.read()
    except FileNotFoundError:
        return True
    # Only whole lines: one still being appended is read on the next poll.
    end = data.rfind(b"\n") + 1
    offset += end
    for line in data[:end].decode().splitlines():
        cmd, _, arg = line.partition(" ")
        if cmd == "click":
            x, y = arg.split()
            click(x, y)
        elif cmd == "other-show":
            other.fullscreen()
            other.present()
        elif cmd == "other-hide":
            other.set_visible(False)
        elif cmd == "other-copy":
            copy(arg)
        else:
            print(f"unknown command {line!r}", flush=True)
            continue
        print(f"did {cmd}", flush=True)
    return True


GLib.timeout_add(100, poll)
print("ready", flush=True)
loop = GLib.MainLoop()
GLib.timeout_add_seconds(600, loop.quit)
loop.run()
