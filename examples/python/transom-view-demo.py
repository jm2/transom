#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""transom-view from Python via GObject introspection.

  GI_TYPELIB_PATH=build/capi/lib/girepository-1.0 LD_LIBRARY_PATH=build/capi/lib \
      examples/python/transom-view-demo.py HOST[:PORT] USER PASSWORD_FILE [SHA256]
"""
import sys

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("TransomView", "0")
from gi.repository import GLib, Gtk, TransomView  # noqa: E402

target, user, password_file = sys.argv[1:4]
trust = sys.argv[4] if len(sys.argv) > 4 else None
host, _, port = target.partition(":")
password = open(password_file).readline().rstrip("\n")


def on_certificate(view, host, port, subject, issuer, fingerprint, previous):
    print(f"certificate for {host}:{port}: {fingerprint}")
    ok = fingerprint == trust
    view.answer_certificate(
        TransomView.CertDecision.ACCEPT_ONCE if ok else TransomView.CertDecision.REJECT)


def on_activate(app):
    win = Gtk.ApplicationWindow(application=app, title="transom-view (Python)",
                                default_width=1100, default_height=700)
    view = TransomView.View.new()
    win.set_child(view)
    view.connect("certificate-prompt", on_certificate)
    view.connect("desktop-resized", lambda v, w, h: print(f"desktop {w}x{h}"))
    view.connect("disconnected", lambda v, reason: (print("disconnected:", reason), app.quit()))

    def on_connected(v):
        print("connected:", v.is_connected())

        def done():
            v.shutdown(5000)
            app.quit()
            return False

        GLib.timeout_add_seconds(4, done)

    view.connect("connected", on_connected)
    win.present()
    GLib.timeout_add(300, lambda: view.connect_to(host, int(port or 0), user, None, password) and False)


app = Gtk.Application(application_id="io.github.jm2.Transom.PyDemo")
app.connect("activate", on_activate)
app.run([])
