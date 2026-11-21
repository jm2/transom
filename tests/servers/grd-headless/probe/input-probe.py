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

    # Clipboard commands from a file (written by the test harness):
    #   copy <text>   put text on the session clipboard
    #   paste         read the session clipboard and log it
    #   paste-image   read an image from the clipboard and log its size
    clipboard = win.get_clipboard()
    cmd_path = "/tmp/probe-cmd"

    def on_text(cb, res):
        try:
            log(t="paste", text=cb.read_text_finish(res))
        except GLib.Error as e:
            log(t="paste", error=e.message)

    def on_texture(cb, res):
        try:
            tex = cb.read_texture_finish(res)
            log(t="paste-image", w=tex.get_width(), h=tex.get_height())
        except GLib.Error as e:
            log(t="paste-image", error=e.message)

    def poll():
        try:
            with open(cmd_path) as f:
                lines = f.read().splitlines()
            open(cmd_path, "w").close()
        except FileNotFoundError:
            return True
        for line in lines:
            if line.startswith("copy "):
                clipboard.set(line[5:])
                log(t="copied", text=line[5:])
            elif line == "paste":
                clipboard.read_text_async(None, on_text)
            elif line.startswith("paste-raw "):
                mime = line[10:]

                def on_raw(cb, res, mime=mime):
                    try:
                        stream, _ = cb.read_finish(res)
                        data = stream.read_bytes(1 << 20, None).get_data()
                        log(t="paste-raw", mime=mime, hex=data.hex())
                    except GLib.Error as e:
                        log(t="paste-raw", mime=mime, error=e.message)

                clipboard.read_async([mime], 0, None, on_raw)
            elif line == "copy-image":
                # 64x48 gradient as image/bmp: grd converts images as BMP <-> CF_DIB
                # only, so offer exactly that.
                import struct
                w, h = 64, 48
                row = lambda y: bytes(b for x in range(w) for b in (128, y * 5, x * 4))  # BGR
                pixels = b"".join(row(y) for y in reversed(range(h)))  # bottom-up, 192 B/row
                info = struct.pack("<IiiHHIIiiII", 40, w, h, 1, 24, 0, len(pixels), 2835, 2835, 0, 0)
                bmp = b"BM" + struct.pack("<IHHI", 14 + len(info) + len(pixels), 0, 0, 14 + len(info)) + info + pixels
                clipboard.set_content(Gdk.ContentProvider.new_for_bytes("image/bmp", GLib.Bytes.new(bmp)))
                log(t="copied-image", w=w, h=h)
            elif line == "formats":
                log(t="formats", formats=clipboard.get_formats().to_string())
            elif line == "paste-image":
                clipboard.read_texture_async(None, on_texture)
        return True

    GLib.timeout_add(200, poll)

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
