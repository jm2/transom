// SPDX-License-Identifier: Apache-2.0
//! Builds without libavcodec (or not on Linux): no hardware H.264, nothing to hook.

use transom_view_core::video::VideoUpdate;

use crate::engine::State;
use crate::ffi;

#[derive(Debug, Default)]
pub(crate) struct Video;

impl Video {
    pub(crate) fn request_disable(&self) {}
    pub(crate) fn take_updates(&self) -> Vec<VideoUpdate> {
        Vec::new()
    }
}

/// # Safety
/// Nothing to uphold; signature matches the real module.
pub(crate) unsafe fn attach(_state: &State, _gfx: *mut ffi::RdpgfxClientContext) {}
pub(crate) fn detach(_state: &State) {}
pub(crate) fn hw_available() -> bool {
    false
}
