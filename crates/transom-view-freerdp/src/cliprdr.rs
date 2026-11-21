// SPDX-License-Identifier: GPL-3.0-or-later
//! Clipboard redirection (MS-RDPECLIP) on FreeRDP's cliprdr client.
//!
//! Format negotiation and data transfer only; the toolkit side (what's on the local
//! clipboard, MIME conversion) lives in the widget. Callbacks may run on a FreeRDP
//! channel thread, so all shared state is behind mutexes/atomics.

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char};
use std::ptr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

use transom_view_core::clipboard::{
    CF_DIB, CF_UNICODETEXT, ClipFormat, LOCAL_HTML_ID, LOCAL_PNG_ID,
};
use transom_view_core::engine::EngineEvent;

use crate::engine::{State, state};
use crate::ffi;

// MS-RDPECLIP message types (an anonymous enum bindgen doesn't emit).
const CB_FORMAT_LIST: u16 = 0x0002;
const CB_FORMAT_LIST_RESPONSE: u16 = 0x0003;
const CB_FORMAT_DATA_REQUEST: u16 = 0x0004;
const CB_FORMAT_DATA_RESPONSE: u16 = 0x0005;
const CB_CLIP_CAPS: u16 = 0x0007;
const CHANNEL_RC_OK: u32 = 0;

/// Clipboard state shared between callbacks and the engine thread.
#[derive(Debug, Default)]
pub(crate) struct ClipState {
    ctx: AtomicPtr<ffi::CliprdrClientContext>,
    ready: AtomicBool,
    /// What the local clipboard offers (sent on MonitorReady and on change).
    local: Mutex<Vec<ClipFormat>>,
    /// Server format ids for the formats it currently offers.
    remote_ids: Mutex<HashMap<ClipFormat, u32>>,
    /// Formats we've requested, in order (responses don't carry the format id).
    pending: Mutex<std::collections::VecDeque<ClipFormat>>,
}

impl ClipState {
    fn ctx(&self) -> Option<*mut ffi::CliprdrClientContext> {
        let p = self.ctx.load(Ordering::SeqCst);
        (!p.is_null() && self.ready.load(Ordering::SeqCst)).then_some(p)
    }
}

/// Hook up a newly connected cliprdr channel.
///
/// # Safety
/// `cliprdr` must be the channel's live interface and `rdp_ctx` our context.
pub(crate) unsafe fn attach(
    state: &State,
    cliprdr: *mut ffi::CliprdrClientContext,
    rdp_ctx: *mut ffi::rdpContext,
) {
    // SAFETY: per the contract.
    unsafe {
        (*cliprdr).custom = rdp_ctx.cast();
        (*cliprdr).MonitorReady = Some(monitor_ready);
        (*cliprdr).ServerCapabilities = Some(server_capabilities);
        (*cliprdr).ServerFormatList = Some(server_format_list);
        (*cliprdr).ServerFormatListResponse = Some(server_format_list_response);
        (*cliprdr).ServerFormatDataRequest = Some(server_format_data_request);
        (*cliprdr).ServerFormatDataResponse = Some(server_format_data_response);
    }
    state.clip.ctx.store(cliprdr, Ordering::SeqCst);
}

pub(crate) fn detach(state: &State) {
    state.clip.ready.store(false, Ordering::SeqCst);
    state.clip.ctx.store(ptr::null_mut(), Ordering::SeqCst);
    state
        .clip
        .pending
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
}

/// # Safety
/// `c` must be a cliprdr context set up by [`attach`].
unsafe fn state_of<'a>(c: *mut ffi::CliprdrClientContext) -> Option<&'a State> {
    // SAFETY: `custom` holds our rdpContext.
    unsafe { state((*c).custom.cast()) }
}

/// Announce local formats (if the channel is up).
pub(crate) fn set_local_formats(state: &State, formats: Vec<ClipFormat>) {
    *state.clip.local.lock().unwrap_or_else(|e| e.into_inner()) = formats;
    if let Some(c) = state.clip.ctx() {
        // SAFETY: live context.
        unsafe { send_format_list(state, c) };
    }
}

/// # Safety
/// `c` must be the live cliprdr context.
unsafe fn send_format_list(state: &State, c: *mut ffi::CliprdrClientContext) {
    let local = state
        .clip
        .local
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let names: Vec<Option<CString>> = local
        .iter()
        .map(|f| f.wire().1.map(|n| CString::new(n).unwrap()))
        .collect();
    let mut formats: Vec<ffi::CLIPRDR_FORMAT> = local
        .iter()
        .zip(&names)
        .map(|(f, n)| ffi::CLIPRDR_FORMAT {
            formatId: f.wire().0,
            formatName: n
                .as_ref()
                .map_or(ptr::null_mut(), |n| n.as_ptr() as *mut c_char),
        })
        .collect();
    let list = ffi::CLIPRDR_FORMAT_LIST {
        common: ffi::CLIPRDR_HEADER {
            msgType: CB_FORMAT_LIST,
            msgFlags: 0,
            dataLen: 0,
        },
        numFormats: formats.len() as u32,
        formats: formats.as_mut_ptr(),
    };
    // SAFETY: `list` and the buffers it points into outlive the call; FreeRDP copies.
    unsafe {
        if let Some(f) = (*c).ClientFormatList {
            let rc = f(c, &list);
            tracing::debug!(?local, rc, "sent clipboard format list");
        }
    }
}

/// Ask the server for its content in `format`.
pub(crate) fn request(state: &State, format: ClipFormat) {
    let id = state
        .clip
        .remote_ids
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&format)
        .copied();
    let (Some(c), Some(id)) = (state.clip.ctx(), id) else {
        state.emit(EngineEvent::ClipboardRemoteData { format, data: None });
        return;
    };
    state
        .clip
        .pending
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push_back(format);
    let req = ffi::CLIPRDR_FORMAT_DATA_REQUEST {
        common: ffi::CLIPRDR_HEADER {
            msgType: CB_FORMAT_DATA_REQUEST,
            msgFlags: 0,
            dataLen: 4,
        },
        requestedFormatId: id,
    };
    // SAFETY: live context; FreeRDP copies the request.
    unsafe {
        if let Some(f) = (*c).ClientFormatDataRequest {
            f(c, &req);
        }
    }
}

/// Answer a server data request.
pub(crate) fn reply(state: &State, format: ClipFormat, data: Option<Vec<u8>>) {
    let Some(c) = state.clip.ctx() else { return };
    tracing::debug!(
        ?format,
        len = data.as_ref().map(Vec::len),
        "clipboard reply"
    );
    let (flags, len, ptr) = match &data {
        Some(d) => (ffi::CB_RESPONSE_OK as u16, d.len() as u32, d.as_ptr()),
        None => (ffi::CB_RESPONSE_FAIL as u16, 0, ptr::null()),
    };
    let resp = ffi::CLIPRDR_FORMAT_DATA_RESPONSE {
        common: ffi::CLIPRDR_HEADER {
            msgType: CB_FORMAT_DATA_RESPONSE,
            msgFlags: flags,
            dataLen: len,
        },
        requestedFormatData: ptr,
    };
    // SAFETY: live context; `data` outlives the call; FreeRDP copies.
    unsafe {
        if let Some(f) = (*c).ClientFormatDataResponse {
            f(c, &resp);
        }
    }
}

// ---- callbacks ----------------------------------------------------------------------

unsafe extern "C" fn monitor_ready(
    c: *mut ffi::CliprdrClientContext,
    _m: *const ffi::CLIPRDR_MONITOR_READY,
) -> u32 {
    // SAFETY: FreeRDP passes our context.
    unsafe {
        let Some(state) = state_of(c) else {
            return CHANNEL_RC_OK;
        };
        let mut general = ffi::CLIPRDR_GENERAL_CAPABILITY_SET {
            capabilitySetType: ffi::CB_CAPSTYPE_GENERAL as u16,
            capabilitySetLength: ffi::CB_CAPSTYPE_GENERAL_LEN as u16,
            version: ffi::CB_CAPS_VERSION_2,
            generalFlags: ffi::CB_USE_LONG_FORMAT_NAMES,
        };
        let caps = ffi::CLIPRDR_CAPABILITIES {
            common: ffi::CLIPRDR_HEADER {
                msgType: CB_CLIP_CAPS,
                msgFlags: 0,
                dataLen: 0,
            },
            cCapabilitiesSets: 1,
            capabilitySets: (&raw mut general).cast(),
        };
        if let Some(f) = (*c).ClientCapabilities {
            f(c, &caps);
        }
        state.clip.ready.store(true, Ordering::SeqCst);
        send_format_list(state, c);
        tracing::debug!("clipboard channel ready");
    }
    CHANNEL_RC_OK
}

unsafe extern "C" fn server_capabilities(
    _c: *mut ffi::CliprdrClientContext,
    _caps: *const ffi::CLIPRDR_CAPABILITIES,
) -> u32 {
    CHANNEL_RC_OK
}

unsafe extern "C" fn server_format_list(
    c: *mut ffi::CliprdrClientContext,
    list: *const ffi::CLIPRDR_FORMAT_LIST,
) -> u32 {
    // SAFETY: FreeRDP passes a valid list with `numFormats` entries.
    unsafe {
        let Some(state) = state_of(c) else {
            return CHANNEL_RC_OK;
        };
        let mut ids = HashMap::new();
        let mut offered = Vec::new();
        if !(*list).formats.is_null() {
            for f in std::slice::from_raw_parts((*list).formats, (*list).numFormats as usize) {
                let name = (!f.formatName.is_null())
                    .then(|| CStr::from_ptr(f.formatName).to_string_lossy().into_owned());
                if let Some(cf) = ClipFormat::from_wire(f.formatId, name.as_deref())
                    && !ids.contains_key(&cf)
                {
                    ids.insert(cf, f.formatId);
                    offered.push(cf);
                }
            }
        }
        tracing::debug!(?offered, "remote clipboard formats");
        *state
            .clip
            .remote_ids
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = ids;
        let resp = ffi::CLIPRDR_FORMAT_LIST_RESPONSE {
            common: ffi::CLIPRDR_HEADER {
                msgType: CB_FORMAT_LIST_RESPONSE,
                msgFlags: ffi::CB_RESPONSE_OK as u16,
                dataLen: 0,
            },
        };
        if let Some(f) = (*c).ClientFormatListResponse {
            f(c, &resp);
        }
        state.emit(EngineEvent::ClipboardRemoteFormats(offered));
    }
    CHANNEL_RC_OK
}

unsafe extern "C" fn server_format_list_response(
    _c: *mut ffi::CliprdrClientContext,
    _r: *const ffi::CLIPRDR_FORMAT_LIST_RESPONSE,
) -> u32 {
    CHANNEL_RC_OK
}

unsafe extern "C" fn server_format_data_request(
    c: *mut ffi::CliprdrClientContext,
    req: *const ffi::CLIPRDR_FORMAT_DATA_REQUEST,
) -> u32 {
    // SAFETY: FreeRDP passes our context and a valid request.
    unsafe {
        let Some(state) = state_of(c) else {
            return CHANNEL_RC_OK;
        };
        let id = (*req).requestedFormatId;
        let format = match id {
            CF_UNICODETEXT => Some(ClipFormat::Text),
            CF_DIB => Some(ClipFormat::Dib),
            LOCAL_HTML_ID => Some(ClipFormat::Html),
            LOCAL_PNG_ID => Some(ClipFormat::Png),
            _ => None,
        };
        let offered = format.is_some_and(|f| {
            state
                .clip
                .local
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains(&f)
        });
        match format {
            Some(f) if offered => state.emit(EngineEvent::ClipboardDataRequest(f)),
            _ => {
                tracing::debug!(id, "server requested a format we don't offer");
                reply(state, ClipFormat::Text, None);
            }
        }
    }
    CHANNEL_RC_OK
}

unsafe extern "C" fn server_format_data_response(
    c: *mut ffi::CliprdrClientContext,
    resp: *const ffi::CLIPRDR_FORMAT_DATA_RESPONSE,
) -> u32 {
    // SAFETY: FreeRDP passes a valid response with `dataLen` bytes of data.
    unsafe {
        let Some(state) = state_of(c) else {
            return CHANNEL_RC_OK;
        };
        let Some(format) = state
            .clip
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front()
        else {
            return CHANNEL_RC_OK;
        };
        let r = &*resp;
        let ok = r.common.msgFlags as u32 & ffi::CB_RESPONSE_OK != 0;
        let data = (ok && !r.requestedFormatData.is_null()).then(|| {
            std::slice::from_raw_parts(r.requestedFormatData, r.common.dataLen as usize).to_vec()
        });
        tracing::debug!(
            ?format,
            len = data.as_ref().map(Vec::len),
            "remote clipboard data"
        );
        state.emit(EngineEvent::ClipboardRemoteData { format, data });
    }
    CHANNEL_RC_OK
}
