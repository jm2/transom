// SPDX-License-Identifier: Apache-2.0
//! Builds without libavcodec (or not on Linux): no hardware H.264, nothing to hook.

use transom_view_core::engine::{EngineEvent, Rect};

use crate::engine::State;
use crate::ffi;

#[derive(Debug, Default)]
pub(crate) struct Video;

impl Video {
    pub(crate) fn request_disable(&self) {}
    pub(crate) fn emit_frame(&self, st: &State, damage: Vec<Rect>) {
        if !damage.is_empty() {
            st.emit(EngineEvent::FrameUpdate {
                damage,
                video: Vec::new(),
            });
        }
    }
    pub(crate) fn emit_reset(&self, st: &State, ev: EngineEvent) {
        st.emit(ev);
    }
}

/// # Safety
/// Nothing to uphold; signature matches the real module.
pub(crate) unsafe fn attach(_state: &State, _gfx: *mut ffi::RdpgfxClientContext) {}
pub(crate) fn detach(_state: &State) {}
pub(crate) fn hw_available() -> bool {
    false
}
