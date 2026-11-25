// SPDX-License-Identifier: GPL-3.0-or-later
//! Transom: a focused, HiDPI-correct RDP client.
// No console window behind the app on Windows (release builds).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod dev;
mod devices_ui;
mod dialogs;
mod secrets;
mod session;
mod window;

fn main() -> gtk::glib::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                "transom=info,transom_view=info,transom_view_freerdp=info".into()
            }),
        )
        .with_writer(std::io::stderr)
        .init();
    // Before GTK or FreeRDP load any DLL.
    transom_view_freerdp::restrict_dll_search();
    app::run()
}
