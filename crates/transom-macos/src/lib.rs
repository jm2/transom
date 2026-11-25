// SPDX-License-Identifier: GPL-3.0-or-later
//! macOS glue for the Transom app.
//!
//! Links (`rdp://…`) reach a macOS app as a `kAEGetURL` Apple Event, not as
//! arguments. GTK (as of 4.24) forwards only file opens (`application:openFiles:`)
//! to `GApplication`, so Transom registers its own handler for URL events.
//!
//! The clipboard bridge also asks the pasteboard directly what GDK can't tell it
//! (password managers' markers, changes GDK didn't notice): [`pasteboard`].
//!
//! On other systems this crate is empty.
#![cfg_attr(target_os = "macos", allow(unsafe_code))] // Objective-C FFI

#[cfg(target_os = "macos")]
mod imp;
#[cfg(target_os = "macos")]
mod pasteboard;

#[cfg(target_os = "macos")]
pub use imp::on_open_url;
#[cfg(target_os = "macos")]
pub use pasteboard::{Pasteboard, pasteboard};
