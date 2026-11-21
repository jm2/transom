// SPDX-License-Identifier: Apache-2.0
//! Toolkit-independent core of transom-view.
//!
//! No GTK, no FFI, no platform-specific code: this crate builds and tests on Linux,
//! macOS and Windows.

pub mod clipboard;
pub mod config;
pub mod engine;
pub mod fsutil;
pub mod geometry;
pub mod keymap;
pub mod monitors;
pub mod profile;
pub mod rdpfile;
pub mod tiles;
pub mod trust;

pub use secrecy;
