#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Fullscreen GTK4 window inside the remote session that logs the input it
receives as JSON lines, so tests can check what actually arrived through RDP."""
import json
import sys

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Gdk", "4.0")
from gi.repository import Gdk, GLib, Gtk  # noqa: E402

LOG = open(sys.argv[1] if len(sys.argv) > 1 else "/tmp/input-probe.log", "a", buffering=1)


def log(**kw):
    LOG.write(json.dumps(kw) + "\n")


def on_activate(app):
    win = Gtk.ApplicationWindow(application=app, title="input-probe")
    area = Gtk.DrawingArea(hexpand=True, vexpand=True, focusable=True, can_focus=True)
    win.set_child(area)

    keys = Gtk.EventControllerKey()
    keys.connect("key-pressed", lambda c, kv, kc, st: (log(t="key", down=True, name=Gdk.keyval_name(kv), code=kc), True)[1])
    keys.connect("key-released", lambda c, kv, kc, st: log(t="key", down=False, name=Gdk.keyval_name(kv), code=kc))
    win.add_controller(keys)

    click = Gtk.GestureClick(button=0)
    click.connect("pressed", lambda g, n, x, y: log(t="button", down=True, button=g.get_current_button(), x=round(x), y=round(y)))
    click.connect("released", lambda g, n, x, y: log(t="button", down=False, button=g.get_current_button(), x=round(x), y=round(y)))
    area.add_controller(click)

    scroll = Gtk.EventControllerScroll(flags=Gtk.EventControllerScrollFlags.BOTH_AXES)
    scroll.connect("scroll", lambda c, dx, dy: (log(t="scroll", dx=dx, dy=dy), True)[1])
    area.add_controller(scroll)

    def ready():
        log(t="ready", w=win.get_width(), h=win.get_height())
        return False

    win.fullscreen()
    win.present()
    area.grab_focus()
    GLib.timeout_add(1500, ready)


app = Gtk.Application(application_id="io.github.jm2.Transom.InputProbe")
app.connect("activate", on_activate)
app.run([])
