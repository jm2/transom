// SPDX-License-Identifier: GPL-3.0-or-later
//! Developer aids for headless UI checks:
//! - `TRANSOM_DEBUG_SCREENSHOT=PATH[:SECONDS]` renders the active window to a PNG
//!   after SECONDS (default 5) and quits.
//! - `TRANSOM_DEBUG_ACTIONS=win.fullscreen@8,win.send-cad@10` activates actions on the
//!   active window after the given seconds.

use std::time::Duration;

use adw::prelude::*;
use gtk::{glib, graphene};

pub fn install(app: &adw::Application) {
    if let Ok(actions) = std::env::var("TRANSOM_DEBUG_ACTIONS") {
        for item in actions.split(',') {
            let Some((name, secs)) = item.split_once('@') else {
                continue;
            };
            let Ok(secs) = secs.parse::<u64>() else {
                continue;
            };
            let (app, name) = (app.clone(), name.to_string());
            glib::timeout_add_local_once(Duration::from_secs(secs), move || {
                eprintln!("debug action: {name}");
                if let Some(w) = app.active_window() {
                    let _ = w.activate_action(&name, None);
                }
            });
        }
    }
    let Ok(spec) = std::env::var("TRANSOM_DEBUG_SCREENSHOT") else {
        return;
    };
    let (path, secs) = match spec.rsplit_once(':') {
        Some((p, s)) if s.parse::<u64>().is_ok() => (p.to_string(), s.parse().unwrap()),
        _ => (spec, 5),
    };
    let app = app.clone();
    glib::timeout_add_local_once(Duration::from_secs(secs), move || {
        if let Some(win) = app.active_window() {
            // Render in surface coordinates so the device-pixel grid matches the
            // real surface (the window content sits at the CSD shadow offset).
            let s = win.surface().map(|s| s.scale()).unwrap_or(1.0);
            let (sx, sy) = win.surface_transform();
            let (w, h) = (win.width() as f64, win.height() as f64);
            let snap = gtk::Snapshot::new();
            snap.scale(s as f32, s as f32);
            snap.translate(&graphene::Point::new(sx as f32, sy as f32));
            gtk::WidgetPaintable::new(Some(&win)).snapshot(&snap, w, h);
            if let (Some(node), Some(renderer)) = (snap.to_node(), win.renderer()) {
                let tex = renderer.render_texture(
                    &node,
                    // From the surface origin: a viewport starting at the content
                    // offset (e.g. 17.5 device px) would itself be off the grid.
                    Some(&graphene::Rect::new(
                        0.0,
                        0.0,
                        ((w + sx) * s).ceil() as f32,
                        ((h + sy) * s).ceil() as f32,
                    )),
                );
                match tex.save_to_png(&path) {
                    Ok(()) => eprintln!("debug screenshot: {path}"),
                    Err(e) => eprintln!("debug screenshot failed: {e}"),
                }
            }
        }
        app.activate_action("quit", None);
    });
}
