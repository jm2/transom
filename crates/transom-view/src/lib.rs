// SPDX-License-Identifier: Apache-2.0
//! `TransomView`: a GTK4 widget that displays and controls one RDP session with
//! pixel-exact HiDPI rendering (PLAN.md §3–§4).
//!
//! ```no_run
//! # use transom_view::TransomView;
//! # fn demo(engine: &dyn transom_view::core::engine::RdpEngine,
//! #         config: transom_view::core::config::SessionConfig) {
//! let view = TransomView::new();
//! view.connect_session(engine, config).unwrap();
//! # }
//! ```

mod clipboard;
#[cfg(target_os = "linux")]
mod dmabuf;
mod tiles;
mod view;

pub use clipboard::PlatformClipboard;
pub use transom_view_core as core;
pub use view::TransomView;
