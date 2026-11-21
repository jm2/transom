// SPDX-License-Identifier: GPL-3.0-or-later
//! Toolkit-independent core of transom-view.
//!
//! No GTK, no FFI, no platform-specific code: this crate builds and tests on Linux,
//! macOS and Windows.

pub mod config;
pub mod engine;
pub mod geometry;
pub mod keymap;
pub mod tiles;
pub mod trust;

pub use secrecy;
