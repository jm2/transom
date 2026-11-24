// SPDX-License-Identifier: Apache-2.0
//! [`RdpEngine`] implementation on FreeRDP 3.
//!
//! One engine thread per session owns the FreeRDP context. FreeRDP may invoke some
//! callbacks (paint, display-control caps) from its channel threads, so everything
//! callbacks touch is in [`State`], which is `Sync`.

use std::ffi::{CStr, CString, c_char, c_void};
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use secrecy::ExposeSecret;
use transom_view_core::config::{CertPolicy, SecurityMode, SessionConfig};
use transom_view_core::engine::{
    AuthKind, AuthRequest, CertDecision, CertPrompt, ConnectStage, Credentials, DisconnectReason,
    EngineCommand, EngineError, EngineEvent, EngineHandle, EventSink, Finished, InputEvent,
    MouseButton, PointerEvent, PointerImage, RdpEngine, Rect, SharedFrame, Waker,
};
use transom_view_core::geometry::{ScaleFactors, Size};
use transom_view_core::monitors::{self, RemoteMonitor};
use transom_view_core::trust::{self, CertInfo};

use crate::ffi;

/// `FREERDP_PIXEL_FORMAT(32, BGRA, 0, 8, 8, 8)`: function-like macro, not bound.
/// `WAIT_FAILED`: a cast macro in the Windows SDK, which bindgen can't evaluate there.
const WAIT_FAILED: u32 = 0xFFFF_FFFF;
const PIXEL_FORMAT_BGRX32: u32 = (32 << 24) | (4 << 16) | (8 << 8) | (8 << 4) | 8;
/// `FREERDP_PIXEL_FORMAT(32, BGRA, 8, 8, 8, 8)`.
const PIXEL_FORMAT_BGRA32: u32 = PIXEL_FORMAT_BGRX32 | (8 << 12);
const TRUE: ffi::BOOL = 1;
const FALSE: ffi::BOOL = 0;
/// Windows rejects monitor layouts sent faster than this (xfreerdp uses the same).
const LAYOUT_MIN_INTERVAL: Duration = Duration::from_millis(200);
/// How often blocked prompts re-check for an abort.
const PROMPT_POLL: Duration = Duration::from_millis(100);

/// FreeRDP 3 backend.
#[derive(Debug, Clone)]
pub struct FreeRdpEngine {
    config_dir: PathBuf,
}

impl FreeRdpEngine {
    /// `config_dir` is a Transom-private directory handed to FreeRDP as
    /// `FreeRDP_ConfigPath` (licence cache etc.). It must not be FreeRDP's default so
    /// that `xfreerdp`'s `known_hosts2` never bypasses Transom's trust store (ADR 0001).
    pub fn new(config_dir: impl Into<PathBuf>) -> Self {
        Self {
            config_dir: config_dir.into(),
        }
    }

    /// The FreeRDP library version in use at runtime.
    pub fn freerdp_version() -> String {
        // SAFETY: returns a pointer to a static NUL-terminated string.
        unsafe { cstr_lossy(ffi::freerdp_get_version_string()) }
    }
}

impl RdpEngine for FreeRdpEngine {
    fn connect(&self, config: SessionConfig, sink: EventSink) -> Result<EngineHandle, EngineError> {
        if config.host.is_empty() || config.host.contains('\0') {
            return Err(EngineError::Config("invalid host name".into()));
        }
        std::fs::create_dir_all(&self.config_dir)
            .map_err(|e| EngineError::Init(format!("creating {:?}: {e}", self.config_dir)))?;
        let config_dir = self
            .config_dir
            .to_str()
            .ok_or_else(|| EngineError::Init("config dir is not UTF-8".into()))?
            .to_owned();

        // SAFETY: plain WinPR call; null attributes/name are allowed. Manual-reset,
        // initially unsignalled.
        let event = unsafe { ffi::CreateEventA(ptr::null_mut(), TRUE, FALSE, ptr::null()) };
        if event.is_null() {
            return Err(EngineError::Init("CreateEvent failed".into()));
        }
        let waker = Arc::new(FreeRdpWaker {
            event,
            ctx: Mutex::new(CtxPtr(ptr::null_mut())),
            aborted: AtomicBool::new(false),
        });
        let (tx, rx) = mpsc::channel();
        let thread_waker = waker.clone();
        let finished = Finished::new();
        let guard = finished.guard();
        thread::Builder::new()
            .name("transom-freerdp".into())
            .spawn(move || {
                let _finished = guard; // set last, even on panic
                run(config, sink, rx, thread_waker, config_dir)
            })
            .map_err(|e| EngineError::Init(format!("spawning engine thread: {e}")))?;
        Ok(EngineHandle::new(tx, waker, finished))
    }
}

#[derive(Debug)]
struct CtxPtr(*mut ffi::rdpContext);

/// Wakes the engine thread's `WaitForMultipleObjects` and aborts blocking connects.
#[derive(Debug)]
struct FreeRdpWaker {
    event: ffi::HANDLE,
    /// The live context, or null before creation / after free. Guarded so `abort`
    /// never races with `freerdp_client_context_free`.
    ctx: Mutex<CtxPtr>,
    aborted: AtomicBool,
}

// SAFETY: `event` is a WinPR event handle; SetEvent/ResetEvent/CloseHandle are
// thread-safe. `ctx` is only dereferenced under its mutex, and only to call
// `freerdp_abort_connect_context`, which FreeRDP documents as callable from any thread.
unsafe impl Send for FreeRdpWaker {}
// SAFETY: see above; all shared mutation is through atomics or the mutex.
unsafe impl Sync for FreeRdpWaker {}

impl Waker for FreeRdpWaker {
    fn wake(&self) {
        // SAFETY: `event` stays valid until `Drop`, which needs the last Arc.
        unsafe { ffi::SetEvent(self.event) };
    }

    fn abort(&self) {
        self.aborted.store(true, Ordering::SeqCst);
        let guard = self.ctx.lock().unwrap_or_else(|e| e.into_inner());
        if !guard.0.is_null() {
            // SAFETY: the context is alive while it's stored here (cleared under the
            // same lock before it's freed).
            unsafe { ffi::freerdp_abort_connect_context(guard.0) };
        }
        self.wake();
    }
}

impl Drop for FreeRdpWaker {
    fn drop(&mut self) {
        // SAFETY: last reference; nobody can signal the handle any more.
        unsafe { ffi::CloseHandle(self.event) };
    }
}

/// State shared between the engine thread and FreeRDP callbacks.
pub(crate) struct State {
    sink: Mutex<EventSink>,
    frame: Mutex<Option<SharedFrame>>,
    disp: AtomicPtr<ffi::DispClientContext>,
    disp_ready: AtomicBool,
    waker: Arc<FreeRdpWaker>,
    pub(crate) config: SessionConfig,
    /// Zero-copy H.264 state (ADR 0006).
    pub(crate) video: crate::gfxvideo::Video,
    /// Kerberos: whether the ticket cache was already tried for the server / gateway.
    kerberos_tried: [AtomicBool; 2],
    cert_rejected: AtomicBool,
    auth_cancelled: AtomicBool,
    pub(crate) clip: crate::cliprdr::ClipState,
    /// Set after the first PostConnect; a later PreConnect means a redirection.
    connected_once: AtomicBool,
    /// Set when the first Display Control channel connects; another one later
    /// means FreeRDP followed a redirection.
    disp_seen: AtomicBool,
    /// The last [`ConnectStage`] reported (as `u8`; `u8::MAX`: none yet).
    stage: AtomicU8,
}

impl State {
    pub(crate) fn emit(&self, ev: EngineEvent) {
        let sink = self.sink.lock().unwrap_or_else(|e| e.into_inner());
        sink(ev);
    }

    fn aborted(&self) -> bool {
        self.waker.aborted.load(Ordering::SeqCst)
    }

    /// Wait for a prompt reply, giving up if the session is aborted.
    fn wait_reply<T>(&self, rx: &mpsc::Receiver<T>) -> Option<T> {
        loop {
            match rx.recv_timeout(PROMPT_POLL) {
                Ok(v) => return Some(v),
                Err(mpsc::RecvTimeoutError::Disconnected) => return None,
                Err(mpsc::RecvTimeoutError::Timeout) if self.aborted() => return None,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }
}

/// Our FreeRDP context: the client context followed by a pointer to [`State`].
/// FreeRDP allocates `ContextSize` bytes, zeroed.
#[repr(C)]
struct TransomContext {
    common: ffi::rdpClientContext,
    state: *const State,
}

/// Pointer object with our decoded image appended.
#[repr(C)]
struct TransomPointer {
    base: ffi::rdpPointer,
    image: *mut PointerImage,
}

/// # Safety
/// `ctx` must be a context created from our entry points, or null.
pub(crate) unsafe fn state<'a>(ctx: *mut ffi::rdpContext) -> Option<&'a State> {
    if ctx.is_null() {
        return None;
    }
    // SAFETY: per the contract, `ctx` points at a `TransomContext`; `state` is either
    // null or points at the `State` kept alive by `run` for the context's lifetime.
    unsafe { (*(ctx as *mut TransomContext)).state.as_ref() }
}

/// # Safety
/// `p` must be null or a valid NUL-terminated string.
unsafe fn cstr_lossy(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        // SAFETY: per the contract.
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }
}

/// # Safety
/// `p` must be null or a valid NUL-terminated string.
unsafe fn cstr_opt(p: *const c_char) -> Option<String> {
    // SAFETY: per the contract.
    let s = unsafe { cstr_lossy(p) };
    (!s.is_empty()).then_some(s)
}

/// Replace a malloc'd C string owned by FreeRDP. The old value and the temporary
/// copy are zeroed first: these slots hold user names and passwords.
///
/// # Safety
/// `slot` must point at a pointer that is null or was allocated with `malloc` and
/// points at a NUL-terminated string.
unsafe fn replace_cstring(slot: *mut *mut c_char, value: &str) -> bool {
    let Ok(c) = CString::new(value) else {
        return false;
    };
    // SAFETY: per the contract; FreeRDP frees these with `free`. The old string is
    // NUL-terminated, so `strlen` stays inside its allocation.
    let ok = unsafe {
        let old = *slot;
        if !old.is_null() {
            let len = libc::strlen(old);
            for i in 0..len {
                ptr::write_volatile(old.add(i), 0);
            }
        }
        libc::free(old.cast());
        *slot = libc::strdup(c.as_ptr());
        !(*slot).is_null()
    };
    wipe(c);
    ok
}

/// Zero a CString's bytes before dropping it (for secrets).
fn wipe(c: CString) {
    let mut bytes = c.into_bytes_with_nul();
    for b in bytes.iter_mut() {
        // SAFETY: writing to an owned, valid u8 location; volatile so the wipe isn't
        // optimised away.
        unsafe { ptr::write_volatile(b, 0) };
    }
}

fn run(
    config: SessionConfig,
    sink: EventSink,
    rx: mpsc::Receiver<EngineCommand>,
    waker: Arc<FreeRdpWaker>,
    config_dir: String,
) {
    let state = Arc::new(State {
        sink: Mutex::new(sink),
        frame: Mutex::new(None),
        disp: AtomicPtr::new(ptr::null_mut()),
        disp_ready: AtomicBool::new(false),
        waker: waker.clone(),
        config,
        video: Default::default(),
        kerberos_tried: [AtomicBool::new(false), AtomicBool::new(false)],
        cert_rejected: AtomicBool::new(false),
        auth_cancelled: AtomicBool::new(false),
        clip: Default::default(),
        connected_once: AtomicBool::new(false),
        disp_seen: AtomicBool::new(false),
        stage: AtomicU8::new(u8::MAX),
    });
    // SAFETY: `session` upholds FreeRDP's lifecycle; `state` outlives the context.
    let reason = unsafe { session(&state, &rx, &waker, &config_dir) };
    tracing::info!(%reason, "session ended");
    state.emit(EngineEvent::Disconnected(reason));
}

/// # Safety
/// Must run on the engine thread; `state` must outlive the call.
unsafe fn session(
    state: &Arc<State>,
    rx: &mpsc::Receiver<EngineCommand>,
    waker: &FreeRdpWaker,
    config_dir: &str,
) -> DisconnectReason {
    if state.aborted() {
        return DisconnectReason::Local;
    }
    let entry = ffi::RDP_CLIENT_ENTRY_POINTS {
        Size: std::mem::size_of::<ffi::RDP_CLIENT_ENTRY_POINTS>() as u32,
        Version: 1, // RDP_CLIENT_INTERFACE_VERSION
        ContextSize: std::mem::size_of::<TransomContext>() as u32,
        ClientNew: Some(client_new),
        ..Default::default()
    };
    // SAFETY: `entry` is fully initialised and outlives the call.
    let ctx = unsafe { ffi::freerdp_client_context_new(&entry) };
    if ctx.is_null() {
        return DisconnectReason::Protocol("could not create FreeRDP context".into());
    }
    // SAFETY: `ctx` was allocated with ContextSize = size_of::<TransomContext>().
    unsafe { (*(ctx as *mut TransomContext)).state = Arc::as_ptr(state) };
    waker.ctx.lock().unwrap_or_else(|e| e.into_inner()).0 = ctx;

    // SAFETY: ctx is valid; settings belong to it.
    let reason = match unsafe { apply_settings(ctx, &state.config, config_dir) } {
        Err(e) => DisconnectReason::Protocol(e),
        // SAFETY: ctx is valid and configured.
        Ok(()) => unsafe { connect_and_loop(ctx, state, rx, waker) },
    };

    waker.ctx.lock().unwrap_or_else(|e| e.into_inner()).0 = ptr::null_mut();
    // SAFETY: ctx is valid and no other thread can reach it any more.
    unsafe { ffi::freerdp_client_context_free(ctx) };
    reason
}

/// # Safety
/// `ctx` must be a valid, configured context owned by this thread.
unsafe fn connect_and_loop(
    ctx: *mut ffi::rdpContext,
    state: &State,
    rx: &mpsc::Receiver<EngineCommand>,
    waker: &FreeRdpWaker,
) -> DisconnectReason {
    // SAFETY: valid context, per the contract.
    unsafe {
        if ffi::freerdp_client_start(ctx) != 0 {
            return DisconnectReason::Protocol("freerdp_client_start failed".into());
        }
        let instance = (*ctx).instance;
        tracing::info!(address = %state.config.address(), "connecting");
        let mut connected = ffi::freerdp_connect(instance) != 0;
        if !connected && rcg_refused(ctx, state) {
            // Servers without Remote Credential Guard (gnome-remote-desktop, Windows
            // hosts whose policy doesn't allow it) drop the connection at negotiation
            // when it is requested. Connect again without it: NLA still uses the
            // ticket, and Windows asks for the password on its logon screen.
            tracing::warn!("server refused Remote Credential Guard; connecting without it");
            // Never silently: without RCG the password is typed on the remote's own
            // logon screen, which is what RCG avoids (an attacker on the path can
            // force this by dropping the first connection).
            state.emit(EngineEvent::Notice(
                "The server didn't accept Remote Credential Guard, so this session runs \
                 without it: Windows may ask for your password on its own sign-in screen."
                    .into(),
            ));
            ffi::freerdp_client_stop(ctx);
            let settings = (*ctx).settings;
            ffi::freerdp_settings_set_bool(settings, ffi::FreeRDP_RemoteCredentialGuard, FALSE);
            state
                .kerberos_tried
                .iter()
                .for_each(|t| t.store(false, Ordering::SeqCst));
            if ffi::freerdp_client_start(ctx) != 0 {
                return DisconnectReason::Protocol("freerdp_client_start failed".into());
            }
            connected = ffi::freerdp_connect(instance) != 0;
        }
        let reason = if !connected {
            classify_error(ctx, state)
        } else {
            let r = event_loop(ctx, state, rx, waker);
            ffi::freerdp_disconnect(instance);
            r
        };
        ffi::freerdp_client_stop(ctx);
        reason
    }
}

/// # Safety
/// `ctx` must be a valid, connected context owned by this thread.
unsafe fn event_loop(
    ctx: *mut ffi::rdpContext,
    state: &State,
    rx: &mpsc::Receiver<EngineCommand>,
    waker: &FreeRdpWaker,
) -> DisconnectReason {
    let mut handles: [ffi::HANDLE; 64] = [ptr::null_mut(); 64];
    let mut pending_layout: Option<Vec<RemoteMonitor>> = None;
    let mut last_layout: Option<Instant> = None;
    // The Display Control instance we last announced; a new one (e.g. after a
    // server redirection) must be announced again so the UI re-sends its layout.
    let mut announced: *mut ffi::DispClientContext = ptr::null_mut();

    loop {
        handles[0] = waker.event;
        // SAFETY: room for 63 handles after slot 0.
        let n = unsafe { ffi::freerdp_get_event_handles(ctx, handles.as_mut_ptr().add(1), 63) };
        if n == 0 {
            return DisconnectReason::Protocol("freerdp_get_event_handles failed".into());
        }
        // SAFETY: `n + 1` handles are initialised.
        let r = unsafe { ffi::WaitForMultipleObjects(n + 1, handles.as_ptr(), FALSE, 100) };
        if r == WAIT_FAILED {
            return DisconnectReason::Protocol("WaitForMultipleObjects failed".into());
        }
        // Reset before draining so a wake during the drain isn't lost.
        // SAFETY: valid event handle.
        unsafe { ffi::ResetEvent(waker.event) };

        loop {
            match rx.try_recv() {
                Ok(EngineCommand::Disconnect) | Err(mpsc::TryRecvError::Disconnected) => {
                    return DisconnectReason::Local;
                }
                Ok(EngineCommand::Layout(req)) => {
                    pending_layout = Some(vec![RemoteMonitor {
                        x: 0,
                        y: 0,
                        size: req.size,
                        scale: req.scale,
                        primary: true,
                    }])
                }
                Ok(EngineCommand::LayoutMonitors(m)) if !m.is_empty() => pending_layout = Some(m),
                Ok(EngineCommand::LayoutMonitors(_)) => {}
                // SAFETY: input belongs to the live context.
                Ok(EngineCommand::Input(ev)) => unsafe { send_input((*ctx).input, ev) },
                // SAFETY: as above.
                Ok(EngineCommand::VideoUnsupported) => state.video.request_disable(),
                Ok(EngineCommand::CtrlAltDel) => unsafe { send_ctrl_alt_del((*ctx).input) },
                Ok(EngineCommand::ClipboardLocalFormats(f)) => {
                    if state.config.clipboard.to_remote() {
                        crate::cliprdr::set_local_formats(state, f);
                    }
                }
                Ok(EngineCommand::ClipboardRequest(f)) => crate::cliprdr::request(state, f),
                Ok(EngineCommand::ClipboardReply { format, data }) => {
                    crate::cliprdr::reply(state, format, data)
                }
                Ok(EngineCommand::ClipboardFileRequest {
                    stream_id,
                    index,
                    range,
                }) => crate::cliprdr::file_request(state, stream_id, index, range),
                Ok(EngineCommand::ClipboardFileReply { stream_id, data }) => {
                    crate::cliprdr::file_reply(state, stream_id, data)
                }
                Err(mpsc::TryRecvError::Empty) => break,
            }
        }
        if state.aborted() {
            return DisconnectReason::Local;
        }

        let disp = state.disp.load(Ordering::SeqCst);
        if state.disp_ready.load(Ordering::SeqCst) && !disp.is_null() {
            if announced != disp {
                announced = disp;
                state.emit(EngineEvent::ResizeAvailable);
            }
            let due = last_layout.is_none_or(|t| t.elapsed() >= LAYOUT_MIN_INTERVAL);
            if let (Some(layout), true) = (pending_layout.as_ref(), due) {
                // SAFETY: `disp` is the live channel interface (cleared on disconnect).
                unsafe { send_layout(disp, layout) };
                pending_layout = None;
                last_layout = Some(Instant::now());
            }
        }

        // SAFETY: valid context.
        unsafe {
            if ffi::freerdp_check_event_handles(ctx) == 0 {
                return classify_error(ctx, state);
            }
            if ffi::freerdp_shall_disconnect_context(ctx) != 0 {
                return classify_error(ctx, state);
            }
        }
    }
}

/// # Safety
/// `ctx` must be valid.
/// Whether a failed connect was the server refusing Remote Credential Guard: it was
/// requested, and the connection failed at transport/negotiation level after reaching
/// the server (not DNS, not unreachable, not a certificate or credential decision).
///
/// # Safety
/// `ctx` must be a valid context.
unsafe fn rcg_refused(ctx: *mut ffi::rdpContext, state: &State) -> bool {
    // SAFETY: valid context and settings, per the contract; static strings.
    let (rcg, name) = unsafe {
        (
            ffi::freerdp_settings_get_bool((*ctx).settings, ffi::FreeRDP_RemoteCredentialGuard)
                != 0,
            cstr_lossy(ffi::freerdp_get_last_error_name(
                ffi::freerdp_get_last_error(ctx),
            )),
        )
    };
    rcg && !state.aborted()
        && !state.cert_rejected.load(Ordering::SeqCst)
        && !state.auth_cancelled.load(Ordering::SeqCst)
        && ["TRANSPORT", "NEGO", "SECURITY"]
            .iter()
            .any(|k| name.contains(k))
        // Exact names: SECURITY_NEGO_CONNECT_FAILED (a refusal) contains CONNECT_FAILED.
        && ![
            "ERRCONNECT_DNS_ERROR",
            "ERRCONNECT_DNS_NAME_NOT_FOUND",
            "ERRCONNECT_CONNECT_FAILED",
        ]
        .contains(&name.as_str())
}

/// Why the connection attempt failed, for the UI.
///
/// # Safety
/// `ctx` must be a valid context, owned by this thread, whose connection attempt
/// has just failed.
unsafe fn classify_error(ctx: *mut ffi::rdpContext, state: &State) -> DisconnectReason {
    if state.aborted() {
        return DisconnectReason::Local;
    }
    if state.cert_rejected.load(Ordering::SeqCst) {
        return DisconnectReason::CertificateRejected;
    }
    if state.auth_cancelled.load(Ordering::SeqCst) {
        return DisconnectReason::Local;
    }
    // SAFETY: valid context; the returned strings are static.
    let (code, name, text) = unsafe {
        let code = ffi::freerdp_get_last_error(ctx);
        (
            code,
            cstr_lossy(ffi::freerdp_get_last_error_name(code)),
            cstr_lossy(ffi::freerdp_get_last_error_string(code)),
        )
    };
    tracing::debug!(code, %name, "last error");
    if code == 0 {
        return DisconnectReason::Remote("connection closed".into());
    }
    let msg = if text.is_empty() { name.clone() } else { text };
    if [
        "AUTHENTICATION_FAILED",
        "LOGON_FAILURE",
        "WRONG_PASSWORD",
        "ACCESS_DENIED",
    ]
    .iter()
    .any(|k| name.contains(k))
    {
        DisconnectReason::AuthenticationFailed
    } else if name.contains("ERRINFO") || name.contains("LOGOFF") || name.contains("DISCONNECTED") {
        DisconnectReason::Remote(msg)
    } else if [
        "DNS",
        "CONNECT_FAILED",
        "TRANSPORT",
        "TLS_CONNECT",
        "CONNECT_CANCELLED",
    ]
    .iter()
    .any(|k| name.contains(k))
    {
        DisconnectReason::Network(msg)
    } else {
        DisconnectReason::Protocol(msg)
    }
}

/// # Safety
/// `ctx` must be valid and not yet connected.
unsafe fn apply_settings(
    ctx: *mut ffi::rdpContext,
    cfg: &SessionConfig,
    config_dir: &str,
) -> Result<(), String> {
    // SAFETY: valid context, per the contract.
    let s = unsafe { (*ctx).settings };
    let set_str = |key: ffi::FreeRDP_Settings_Keys_String, v: &str| -> Result<(), String> {
        let c = CString::new(v).map_err(|_| "setting contains NUL".to_string())?;
        // SAFETY: `s` is valid; FreeRDP copies the string.
        let ok = unsafe { ffi::freerdp_settings_set_string(s, key, c.as_ptr()) } != 0;
        wipe(c);
        ok.then_some(())
            .ok_or_else(|| format!("setting string key {key} failed"))
    };
    let set_u32 = |key: ffi::FreeRDP_Settings_Keys_UInt32, v: u32| -> Result<(), String> {
        // SAFETY: `s` is valid.
        let ok = unsafe { ffi::freerdp_settings_set_uint32(s, key, v) } != 0;
        ok.then_some(())
            .ok_or_else(|| format!("setting u32 key {key} failed"))
    };
    let set_bool = |key: ffi::FreeRDP_Settings_Keys_Bool, v: bool| -> Result<(), String> {
        // SAFETY: `s` is valid.
        let ok = unsafe { ffi::freerdp_settings_set_bool(s, key, v as ffi::BOOL) } != 0;
        ok.then_some(())
            .ok_or_else(|| format!("setting bool key {key} failed"))
    };

    set_str(
        ffi::FreeRDP_ServerHostname,
        cfg.host.trim_matches(['[', ']']),
    )?;
    set_u32(ffi::FreeRDP_ServerPort, cfg.port as u32)?;
    if let Some(u) = &cfg.username {
        set_str(ffi::FreeRDP_Username, u)?;
    }
    if let Some(d) = &cfg.domain {
        set_str(ffi::FreeRDP_Domain, d)?;
    }
    if let Some(p) = &cfg.password {
        set_str(ffi::FreeRDP_Password, p.expose_secret())?;
    }
    set_str(ffi::FreeRDP_ConfigPath, config_dir)?;

    if cfg.monitors.len() > 1 {
        let (origin, size) = monitors::bounds(&cfg.monitors);
        let primary = cfg
            .monitors
            .iter()
            .find(|m| m.primary)
            .unwrap_or(&cfg.monitors[0]);
        set_u32(ffi::FreeRDP_DesktopWidth, size.width)?;
        set_u32(ffi::FreeRDP_DesktopHeight, size.height)?;
        set_u32(ffi::FreeRDP_DesktopScaleFactor, primary.scale.desktop)?;
        set_u32(ffi::FreeRDP_DeviceScaleFactor, primary.scale.device)?;
        set_bool(ffi::FreeRDP_UseMultimon, true)?;
        let n = cfg.monitors.len();
        // SAFETY: `s` is valid; we size the array, then fill the entries FreeRDP owns.
        unsafe {
            if ffi::freerdp_settings_set_pointer_len(
                s,
                ffi::FreeRDP_MonitorDefArray,
                ptr::null(),
                n,
            ) == 0
            {
                return Err("allocating monitor array failed".into());
            }
            for (i, m) in cfg.monitors.iter().enumerate() {
                let def = ffi::freerdp_settings_get_pointer_array_writable(
                    s,
                    ffi::FreeRDP_MonitorDefArray,
                    i,
                ) as *mut ffi::rdpMonitor;
                if def.is_null() {
                    return Err("monitor array entry missing".into());
                }
                *def = ffi::rdpMonitor {
                    x: m.x,
                    y: m.y,
                    width: m.size.width as i32,
                    height: m.size.height as i32,
                    is_primary: m.primary as u32,
                    orig_screen: i as u32,
                    attributes: ffi::MONITOR_ATTRIBUTES {
                        physicalWidth: physical_mm(m.size.width, m.scale.desktop),
                        physicalHeight: physical_mm(m.size.height, m.scale.desktop),
                        orientation: 0,
                        desktopScaleFactor: m.scale.desktop,
                        deviceScaleFactor: m.scale.device,
                    },
                };
            }
        }
        set_u32(ffi::FreeRDP_MonitorCount, n as u32)?;
        tracing::debug!(?origin, ?size, n, "multi-monitor layout at connect");
    } else {
        set_u32(ffi::FreeRDP_DesktopWidth, cfg.initial.size.width)?;
        set_u32(ffi::FreeRDP_DesktopHeight, cfg.initial.size.height)?;
        set_u32(ffi::FreeRDP_DesktopScaleFactor, cfg.initial.scale.desktop)?;
        set_u32(ffi::FreeRDP_DeviceScaleFactor, cfg.initial.scale.device)?;
    }
    // Ticket-only login to Windows: NLA authenticates with Kerberos, but Windows also
    // needs a credential to log the user on interactively. Without a password that is
    // Remote Credential Guard: the client delegates its ticket, and Windows sends later
    // Kerberos operations back over MS-RDPEAR (FreeRDP loads that channel when this is
    // set). FreeRDP builds without the channel (e.g. Fedora's) would get a session
    // stuck at "The remote procedure call failed": there, connect without it and
    // Windows shows its own logon screen instead.
    if cfg.kerberos && cfg.password.is_none() {
        if rdpear_available() {
            set_bool(ffi::FreeRDP_RemoteCredentialGuard, true)?;
        } else {
            tracing::warn!(
                "FreeRDP lacks the MS-RDPEAR channel: no Remote Credential Guard, so \
                 Windows will ask for the password on its logon screen"
            );
        }
    }
    if let Some(gw) = &cfg.gateway {
        // RD Gateway: HTTP transport (with WebSocket upgrade when offered), falling
        // back to RPC over HTTP; the gateway decides.
        set_bool(ffi::FreeRDP_GatewayEnabled, true)?;
        set_str(ffi::FreeRDP_GatewayHostname, &gw.host)?;
        set_u32(ffi::FreeRDP_GatewayPort, gw.port as u32)?;
        set_bool(ffi::FreeRDP_GatewayHttpTransport, true)?;
        set_bool(ffi::FreeRDP_GatewayRpcTransport, true)?;
        set_bool(ffi::FreeRDP_GatewayHttpUseWebsockets, true)?;
        set_bool(ffi::FreeRDP_GatewayUseSameCredentials, gw.same_credentials)?;
        // FreeRDP asks (AuthenticateEx, GW_AUTH_*) for whatever is missing here, so
        // fill the gateway credentials in: the session's when shared.
        let (u, d, p) = if gw.same_credentials {
            (&cfg.username, &cfg.domain, &cfg.password)
        } else {
            (&gw.username, &gw.domain, &gw.password)
        };
        if let Some(u) = u {
            set_str(ffi::FreeRDP_GatewayUsername, u)?;
        }
        if let Some(d) = d {
            set_str(ffi::FreeRDP_GatewayDomain, d)?;
        }
        if let Some(p) = p {
            set_str(ffi::FreeRDP_GatewayPassword, p.expose_secret())?;
        }
    }
    set_u32(ffi::FreeRDP_ColorDepth, 32)?;

    set_bool(ffi::FreeRDP_SoftwareGdi, true)?;
    set_bool(ffi::FreeRDP_SupportGraphicsPipeline, true)?;
    // GFX codecs: AVC444 → AVC420 → RemoteFX/progressive → planar (PLAN §4). Only
    // advertise H.264 when a decoder really works (e.g. not a stub OpenH264).
    let h264 = h264_available();
    // Hardware decoding (ADR 0006) takes the AVC420 commands; AVC444 (two streams to
    // recombine) stays with FreeRDP's software decoder. Advertise both: with AVC420
    // alone FreeRDP offers only GFX 8.1, and Windows then picks RemoteFX for the
    // desktop. Which one the server sends is the server's choice: by default Windows
    // uses AVC420 for regions it classifies as video (the ones zero-copy is for), and
    // AVC444 for the whole desktop under the "Prioritize H.264/AVC 444" policy or in
    // thin-client mode (so we don't ask for thin-client mode).
    let hw = cfg.video_decode != transom_view_core::video::VideoDecode::Software
        && crate::gfxvideo::hw_available();
    set_bool(ffi::FreeRDP_GfxH264, h264 || hw)?;
    set_bool(ffi::FreeRDP_GfxAVC444, h264)?;
    set_bool(ffi::FreeRDP_GfxAVC444v2, h264)?;
    set_bool(ffi::FreeRDP_GfxProgressive, true)?;
    set_bool(ffi::FreeRDP_GfxProgressiveV2, true)?;
    set_bool(ffi::FreeRDP_RemoteFxCodec, true)?;
    set_bool(ffi::FreeRDP_SupportDisplayControl, true)?;
    set_bool(ffi::FreeRDP_DynamicResolutionUpdate, true)?;
    // Audio: rdpsnd (playback) and audin (microphone); FreeRDP picks the platform
    // backend (PulseAudio/PipeWire on Linux, CoreAudio on macOS, WinMM on Windows).
    use transom_view_core::config::AudioMode;
    set_bool(ffi::FreeRDP_AudioPlayback, cfg.audio == AudioMode::Local)?;
    set_bool(
        ffi::FreeRDP_RemoteConsoleAudio,
        cfg.audio == AudioMode::Remote,
    )?;
    set_bool(ffi::FreeRDP_AudioCapture, cfg.microphone)?;
    set_bool(
        ffi::FreeRDP_RedirectClipboard,
        cfg.clipboard != transom_view_core::config::ClipboardMode::Off,
    )?;
    // SAFETY: `s` is valid and not yet connected.
    unsafe { add_devices(s, &cfg.redirections)? };

    let (nla, tls, rdp) = match cfg.security {
        SecurityMode::Negotiate => (true, true, false),
        SecurityMode::Nla => (true, false, false),
        SecurityMode::Tls => (false, true, false),
        SecurityMode::Rdp => (true, true, true),
    };
    set_bool(ffi::FreeRDP_NlaSecurity, nla)?;
    set_bool(ffi::FreeRDP_TlsSecurity, tls)?;
    set_bool(ffi::FreeRDP_RdpSecurity, rdp)?;

    match cfg.cert_policy {
        CertPolicy::Ignore => set_bool(ffi::FreeRDP_IgnoreCertificate, true)?,
        CertPolicy::Verify => {
            set_bool(ffi::FreeRDP_IgnoreCertificate, false)?;
            set_bool(ffi::FreeRDP_AutoAcceptCertificate, false)?;
            set_bool(ffi::FreeRDP_CertificateCallbackPreferPEM, true)?;
        }
    }

    #[cfg(target_os = "windows")]
    let os = ffi::OSMAJORTYPE_WINDOWS;
    #[cfg(target_os = "macos")]
    let os = ffi::OSMAJORTYPE_MACINTOSH;
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let os = ffi::OSMAJORTYPE_UNIX;
    set_u32(ffi::FreeRDP_OsMajorType, os)?;
    Ok(())
}

/// Whether FreeRDP can create an H.264 decoder here (checked once per process).
pub fn h264_available() -> bool {
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        // SAFETY: creating and immediately freeing a decoder context has no other
        // effects; a null return means no working H.264 backend.
        let ok = unsafe {
            let ctx = ffi::h264_context_new(FALSE);
            if ctx.is_null() {
                false
            } else {
                ffi::h264_context_free(ctx);
                true
            }
        };
        tracing::info!(h264 = ok, "H.264 decoder");
        ok
    })
}

// ---- FreeRDP callbacks -------------------------------------------------------------

unsafe extern "C" fn client_new(
    instance: *mut ffi::freerdp,
    _ctx: *mut ffi::rdpContext,
) -> ffi::BOOL {
    // SAFETY: FreeRDP passes a valid instance.
    unsafe {
        (*instance).PreConnect = Some(pre_connect);
        (*instance).PostConnect = Some(post_connect);
        (*instance).PostDisconnect = Some(post_disconnect);
        (*instance).AuthenticateEx = Some(authenticate_ex);
        (*instance).VerifyCertificateEx = Some(verify_certificate_ex);
        (*instance).VerifyChangedCertificateEx = Some(verify_changed_certificate_ex);
    }
    TRUE
}

type EventHandler = unsafe extern "C" fn(*mut c_void, *const ffi::ChannelConnectedEventArgs);
type DisconnectedHandler =
    unsafe extern "C" fn(*mut c_void, *const ffi::ChannelDisconnectedEventArgs);
type StateHandler = unsafe extern "C" fn(*mut c_void, *const StateChangedArgs);

/// FreeRDP 3.27's `StateChangedEventArgs`, declared here because the oldest headers
/// we build against lack its fields; libraries that old never send the event.
#[repr(C)]
struct StateChangedArgs {
    e: ffi::wEventArgs,
    old_state: ffi::CONNECTION_STATE,
    new_state: ffi::CONNECTION_STATE,
}

/// [`State::stage`] once connected: later state changes (a reactivation) aren't
/// progress to report.
const STAGE_CONNECTED: u8 = u8::MAX - 1;

unsafe extern "C" fn pre_connect(instance: *mut ffi::freerdp) -> ffi::BOOL {
    // SAFETY: valid instance with a context; PubSub copies the handler pointers.
    unsafe {
        let ctx = (*instance).context;
        // freerdp_connect resets FreeRDP's abort event when it starts, so a cancel
        // that arrived during setup would be lost: check ours here, after the reset
        // and before anything is sent.
        if state(ctx).is_some_and(|s| s.aborted()) {
            tracing::info!("cancelled before connecting");
            return FALSE;
        }
        if let Some(state) = state(ctx)
            && state.connected_once.load(Ordering::SeqCst)
        {
            let target = cstr_lossy(ffi::freerdp_settings_get_string(
                (*ctx).settings,
                ffi::FreeRDP_ServerHostname,
            ));
            tracing::info!(%target, "server redirection");
            state.emit(EngineEvent::Redirecting { target });
        }
        let ps = (*ctx).pubSub;
        let on: EventHandler = on_channel_connected;
        let off: DisconnectedHandler = on_channel_disconnected;
        if ffi::PubSub_Subscribe(ps, c"ChannelConnected".as_ptr(), on) < 0
            || ffi::PubSub_Subscribe(ps, c"ChannelDisconnected".as_ptr(), off) < 0
        {
            return FALSE;
        }
        // FreeRDP 3.27+ announces each step of the connection sequence; older
        // versions don't, and the UI then shows fewer stages.
        let progress: StateHandler = on_state_change;
        if ffi::PubSub_Subscribe(ps, c"StateChanged".as_ptr(), progress) < 0 {
            tracing::debug!("no StateChanged event (FreeRDP < 3.27): fewer progress stages");
        }
        // DNS, TCP and TLS happen before FreeRDP's first state change.
        if let Some(state) = state(ctx) {
            state.stage.store(u8::MAX, Ordering::SeqCst);
            report_stage(state, ConnectStage::Contacting);
        }
    }
    TRUE
}

/// Tell the UI about a new [`ConnectStage`] (once per stage, however many FreeRDP
/// states it spans), and log it: the timestamps show where a slow connection waits.
fn report_stage(state: &State, stage: ConnectStage) {
    let last = state.stage.load(Ordering::SeqCst);
    if last != STAGE_CONNECTED && state.stage.swap(stage as u8, Ordering::SeqCst) != stage as u8 {
        tracing::info!(stage = stage.nick(), "connecting");
        state.emit(EngineEvent::Progress(stage));
    }
}

unsafe extern "C" fn on_state_change(ctx: *mut c_void, e: *const StateChangedArgs) {
    // SAFETY: FreeRDP passes our context and the event's arguments, whose recorded
    // size is checked before the fields past the header are read. It sends the event
    // before it switches state, so freerdp_get_state would still give the old one.
    unsafe {
        let Some(state) = state(ctx.cast()) else {
            return;
        };
        if e.is_null() || ((*e).e.Size as usize) < std::mem::size_of::<StateChangedArgs>() {
            return;
        }
        let stage = match (*e).new_state {
            ffi::CONNECTION_STATE_INITIAL | ffi::CONNECTION_STATE_NEGO => ConnectStage::Contacting,
            ffi::CONNECTION_STATE_NLA | ffi::CONNECTION_STATE_AAD => ConnectStage::Authenticating,
            // Connected (post_connect) follows.
            ffi::CONNECTION_STATE_ACTIVE => return,
            _ => ConnectStage::Configuring,
        };
        report_stage(state, stage);
    }
}

unsafe extern "C" fn post_connect(instance: *mut ffi::freerdp) -> ffi::BOOL {
    // SAFETY: valid instance; called on the engine thread after the connection is up.
    unsafe {
        let ctx = (*instance).context;
        let Some(state) = state(ctx) else {
            return FALSE;
        };
        if ffi::gdi_init(instance, PIXEL_FORMAT_BGRX32) == 0 {
            return FALSE;
        }
        clear_primary((*ctx).gdi);
        let proto = ffi::rdpPointer {
            size: std::mem::size_of::<TransomPointer>(),
            New: Some(pointer_new),
            Free: Some(pointer_free),
            Set: Some(pointer_set),
            SetNull: Some(pointer_set_null),
            SetDefault: Some(pointer_set_default),
            SetPosition: Some(pointer_set_position),
            ..Default::default()
        };
        ffi::graphics_register_pointer((*ctx).graphics, &proto);
        let update = (*ctx).update;
        (*update).BeginPaint = Some(begin_paint);
        (*update).EndPaint = Some(end_paint);
        (*update).DesktopResize = Some(desktop_resize);

        let gdi = (*ctx).gdi;
        let size = Size::new((*gdi).width as u32, (*gdi).height as u32);
        // Black, like the primary buffer (clear_primary): nothing is drawn yet.
        let frame = SharedFrame::new(size);
        *state.frame.lock().unwrap_or_else(|e| e.into_inner()) = Some(frame.clone());

        let s = (*ctx).settings;
        let scale = ScaleFactors {
            desktop: ffi::freerdp_settings_get_uint32(s, ffi::FreeRDP_DesktopScaleFactor),
            device: ffi::freerdp_settings_get_uint32(s, ffi::FreeRDP_DeviceScaleFactor),
        };
        state.connected_once.store(true, Ordering::SeqCst);
        state.stage.store(STAGE_CONNECTED, Ordering::SeqCst);
        tracing::info!(?size, ?scale, "connected");
        state.emit(EngineEvent::Connected {
            desktop: size,
            scale,
            frame,
        });
    }
    TRUE
}

unsafe extern "C" fn post_disconnect(instance: *mut ffi::freerdp) {
    // SAFETY: valid instance; mirrors pre_connect/post_connect.
    unsafe {
        let ctx = (*instance).context;
        let on: EventHandler = on_channel_connected;
        let off: DisconnectedHandler = on_channel_disconnected;
        let progress: StateHandler = on_state_change;
        ffi::PubSub_Unsubscribe((*ctx).pubSub, c"ChannelConnected".as_ptr(), on);
        ffi::PubSub_Unsubscribe((*ctx).pubSub, c"ChannelDisconnected".as_ptr(), off);
        ffi::PubSub_Unsubscribe((*ctx).pubSub, c"StateChanged".as_ptr(), progress);
        ffi::gdi_free(instance);
    }
}

unsafe extern "C" fn on_channel_connected(
    ctx: *mut c_void,
    e: *const ffi::ChannelConnectedEventArgs,
) {
    // SAFETY: FreeRDP passes our context and valid event args.
    unsafe {
        ffi::freerdp_client_OnChannelConnectedEventHandler(ctx, e);
        let ctx = ctx as *mut ffi::rdpContext;
        let Some(state) = state(ctx) else { return };
        let name = CStr::from_ptr((*e).name);
        tracing::debug!(channel = %name.to_string_lossy(), "channel connected");
        if name.to_bytes_with_nul() == ffi::DISP_DVC_CHANNEL_NAME {
            if state.disp_seen.swap(true, Ordering::SeqCst) {
                // FreeRDP follows Server Redirection PDUs internally; a second Display
                // Control channel is how we notice (e.g. grd remote login handover).
                let target = cstr_lossy(ffi::freerdp_settings_get_string(
                    (*ctx).settings,
                    ffi::FreeRDP_ServerHostname,
                ));
                tracing::info!(%target, "channels reconnected after redirection");
                state.emit(EngineEvent::Redirecting { target });
            }
            let disp = (*e).pInterface as *mut ffi::DispClientContext;
            (*disp).custom = ctx.cast();
            (*disp).DisplayControlCaps = Some(disp_caps);
            state.disp.store(disp, Ordering::SeqCst);
        } else if name.to_bytes_with_nul() == ffi::CLIPRDR_SVC_CHANNEL_NAME {
            crate::cliprdr::attach(state, (*e).pInterface.cast(), ctx);
        } else if name.to_bytes_with_nul() == ffi::RDPGFX_DVC_CHANNEL_NAME {
            // FreeRDP's handler (above) has just set up its GDI graphics pipeline.
            crate::gfxvideo::attach(state, (*e).pInterface.cast());
        }
    }
}

unsafe extern "C" fn on_channel_disconnected(
    ctx: *mut c_void,
    e: *const ffi::ChannelDisconnectedEventArgs,
) {
    // SAFETY: FreeRDP passes our context and valid event args.
    unsafe {
        let rctx = ctx as *mut ffi::rdpContext;
        if let Some(state) = state(rctx) {
            let name = CStr::from_ptr((*e).name).to_bytes_with_nul();
            if name == ffi::DISP_DVC_CHANNEL_NAME {
                state.disp_ready.store(false, Ordering::SeqCst);
                state.disp.store(ptr::null_mut(), Ordering::SeqCst);
            } else if name == ffi::CLIPRDR_SVC_CHANNEL_NAME {
                crate::cliprdr::detach(state);
            } else if name == ffi::RDPGFX_DVC_CHANNEL_NAME {
                crate::gfxvideo::detach(state);
            }
        }
        ffi::freerdp_client_OnChannelDisconnectedEventHandler(ctx, e);
    }
}

unsafe extern "C" fn disp_caps(
    disp: *mut ffi::DispClientContext,
    max_monitors: u32,
    factor_a: u32,
    factor_b: u32,
) -> ffi::UINT {
    // SAFETY: `custom` was set to our context in on_channel_connected.
    unsafe {
        tracing::debug!(max_monitors, factor_a, factor_b, "display control caps");
        if let Some(state) = state((*disp).custom.cast()) {
            state.disp_ready.store(true, Ordering::SeqCst);
            state.waker.wake();
        }
    }
    0 // CHANNEL_RC_OK
}

/// Physical size in mm for a monitor of `px` device pixels at `desktop_scale`
/// percent, as if its logical size were at 96 DPI. MS-RDPEDISP accepts
/// 10–10000 mm; Windows derives DPI from the scale factors, not from these.
fn physical_mm(px: u32, desktop_scale: u32) -> u32 {
    let logical = px as f64 * 100.0 / desktop_scale.max(100) as f64;
    ((logical / 96.0 * 25.4).round() as u32).clamp(10, 10000)
}

/// # Safety
/// `disp` must be the live display-control interface.
unsafe fn send_layout(disp: *mut ffi::DispClientContext, layout: &[RemoteMonitor]) {
    let mut defs: Vec<ffi::DISPLAY_CONTROL_MONITOR_LAYOUT> = layout
        .iter()
        .map(|m| ffi::DISPLAY_CONTROL_MONITOR_LAYOUT {
            Flags: if m.primary {
                ffi::DISPLAY_CONTROL_MONITOR_PRIMARY
            } else {
                0
            },
            Left: m.x,
            Top: m.y,
            Width: m.size.width,
            Height: m.size.height,
            PhysicalWidth: physical_mm(m.size.width, m.scale.desktop),
            PhysicalHeight: physical_mm(m.size.height, m.scale.desktop),
            Orientation: 0,
            DesktopScaleFactor: m.scale.desktop,
            DeviceScaleFactor: m.scale.device,
        })
        .collect();
    // SAFETY: per the contract; `defs` outlives the call.
    unsafe {
        if let Some(send) = (*disp).SendMonitorLayout {
            let rc = send(disp, defs.len() as u32, defs.as_mut_ptr());
            tracing::debug!(?layout, rc, "sent monitor layout");
        }
    }
}

/// Add the device channels for `r` (PLAN Phase 4b). Devices that can't be offered
/// (a missing folder, a bad port name) are logged and left out; the server decides
/// about the rest and refusals don't fail the connection.
///
/// # Safety
/// `s` must be the settings of a context that is not yet connected.
unsafe fn add_devices(
    s: *mut ffi::rdpSettings,
    r: &transom_view_core::devices::Redirections,
) -> Result<(), String> {
    use transom_view_core::devices::{UsbSelector, usable};
    let (r, problems) = usable(r);
    for p in problems {
        tracing::warn!("not redirecting: {p}");
    }
    let add = |dynamic: bool, args: &[&str]| -> Result<(), String> {
        let c: Vec<CString> = args
            .iter()
            .map(|a| CString::new(*a).map_err(|_| "device argument contains NUL".to_string()))
            .collect::<Result<_, _>>()?;
        let ptrs: Vec<*const std::ffi::c_char> = c.iter().map(|a| a.as_ptr()).collect();
        // SAFETY: valid settings; FreeRDP copies the arguments.
        let ok = unsafe {
            if dynamic {
                ffi::freerdp_client_add_dynamic_channel(s, ptrs.len(), ptrs.as_ptr())
            } else {
                ffi::freerdp_client_add_device_channel(s, ptrs.len(), ptrs.as_ptr())
            }
        } != 0;
        ok.then_some(())
            .ok_or_else(|| format!("adding device channel {} failed", args[0]))
    };
    let set = |key: ffi::FreeRDP_Settings_Keys_Bool, v: bool| -> Result<(), String> {
        // SAFETY: valid settings.
        let ok = unsafe { ffi::freerdp_settings_set_bool(s, key, v as ffi::BOOL) } != 0;
        ok.then_some(())
            .ok_or_else(|| format!("setting bool key {key} failed"))
    };
    let mut any = false;
    for d in &r.drives {
        let path = d.path.to_str().ok_or("drive path is not UTF-8")?;
        add(false, &["drive", &d.name, path])?;
        set(ffi::FreeRDP_RedirectDrives, true)?;
        any = true;
    }
    if r.smartcards {
        add(false, &["smartcard"])?;
        set(ffi::FreeRDP_RedirectSmartCards, true)?;
        any = true;
    }
    if r.printers {
        add(false, &["printer"])?;
        set(ffi::FreeRDP_RedirectPrinters, true)?;
        any = true;
    }
    for p in &r.serial_ports {
        let path = p.path.to_str().ok_or("serial port path is not UTF-8")?;
        add(false, &["serial", &p.name, path])?;
        set(ffi::FreeRDP_RedirectSerialPorts, true)?;
        any = true;
    }
    if any {
        set(ffi::FreeRDP_DeviceRedirection, true)?;
    }
    if !r.usb.is_empty() {
        // One URBDRC channel; devices by vendor:product (hex, '#'-separated), which
        // also picks up matching devices plugged in during the session.
        let ids: Vec<String> = r
            .usb
            .iter()
            .map(|u| match u {
                UsbSelector::VidPid { vid, pid } => format!("{vid:04x}:{pid:04x}"),
            })
            .collect();
        add(true, &["urbdrc", &format!("id:{}", ids.join("#"))])?;
    }
    Ok(())
}

/// Whether FreeRDP has the MS-RDPEAR (authentication redirection) client channel,
/// built in or as a plugin. Upstream builds it only with `-DCHANNEL_RDPEAR=ON`.
fn rdpear_available() -> bool {
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        // SAFETY: a lookup in FreeRDP's addin tables (and plugin directory); the name is
        // a valid C string and the optional arguments may be null.
        let entry = unsafe {
            ffi::freerdp_load_channel_addin_entry(
                c"rdpear".as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                ffi::FREERDP_ADDIN_CHANNEL_DYNAMIC,
            )
        };
        entry.is_some()
    })
}

unsafe extern "C" fn authenticate_ex(
    instance: *mut ffi::freerdp,
    username: *mut *mut c_char,
    password: *mut *mut c_char,
    domain: *mut *mut c_char,
    reason: ffi::rdp_auth_reason,
) -> ffi::BOOL {
    // SAFETY: valid instance and string slots, per FreeRDP's contract.
    unsafe {
        let Some(state) = state((*instance).context) else {
            return FALSE;
        };
        let kind = match reason {
            ffi::GW_AUTH_HTTP | ffi::GW_AUTH_RDG | ffi::GW_AUTH_RPC => AuthKind::Gateway,
            _ => AuthKind::Server,
        };
        // Kerberos single sign-on: answer the first request of each kind (server, and
        // the RD Gateway when there is one) with empty credentials so NLA uses the ticket
        // cache (FreeRDP: "empty credentials = proceed without"). A second request of
        // the same kind means the ticket didn't work: ask the user.
        let slot = usize::from(kind == AuthKind::Gateway);
        if state.config.kerberos && !state.kerberos_tried[slot].swap(true, Ordering::SeqCst) {
            let user = cstr_opt(*username)
                .or_else(|| state.config.username.clone())
                .unwrap_or_default();
            let dom = cstr_opt(*domain)
                .or_else(|| state.config.domain.clone())
                .unwrap_or_default();
            tracing::info!(?kind, "using the Kerberos ticket cache");
            let ok = replace_cstring(username, &user)
                && replace_cstring(domain, &dom)
                && replace_cstring(password, "");
            return if ok { TRUE } else { FALSE };
        }
        let (tx, rx) = mpsc::sync_channel(1);
        state.emit(EngineEvent::AuthRequired(AuthRequest {
            kind,
            username: cstr_opt(*username),
            domain: cstr_opt(*domain),
            reply: tx,
        }));
        match state.wait_reply(&rx).flatten() {
            Some(Credentials {
                username: u,
                domain: d,
                password: p,
            }) => {
                let ok = replace_cstring(username, &u)
                    && replace_cstring(domain, d.as_deref().unwrap_or(""))
                    && replace_cstring(password, p.expose_secret());
                if ok { TRUE } else { FALSE }
            }
            None => {
                state.auth_cancelled.store(true, Ordering::SeqCst);
                FALSE
            }
        }
    }
}

/// Shared by both certificate callbacks.
///
/// # Safety
/// All string pointers must be null or valid NUL-terminated strings.
#[allow(clippy::too_many_arguments)]
unsafe fn verify_cert(
    instance: *mut ffi::freerdp,
    host: *const c_char,
    port: u16,
    subject: *const c_char,
    issuer: *const c_char,
    fingerprint: *const c_char,
    old_fingerprint: Option<*const c_char>,
    flags: u32,
) -> u32 {
    // SAFETY: per the contract.
    unsafe {
        let Some(state) = state((*instance).context) else {
            return 0;
        };
        let to_fp = |p: *const c_char| {
            let raw = cstr_lossy(p);
            if flags & ffi::VERIFY_CERT_FLAG_FP_IS_PEM != 0 {
                trust::pem_to_der(&raw).map(|der| trust::fingerprint_sha256(&der))
            } else {
                trust::normalize_fingerprint(&raw)
            }
        };
        let Some(fp) = to_fp(fingerprint) else {
            tracing::warn!("server certificate could not be decoded; rejecting");
            state.cert_rejected.store(true, Ordering::SeqCst);
            return 0;
        };
        let info = CertInfo {
            host: cstr_lossy(host),
            port,
            subject: cstr_lossy(subject),
            issuer: cstr_lossy(issuer),
            fingerprint_sha256: fp,
        };
        let (tx, rx) = mpsc::sync_channel(1);
        state.emit(EngineEvent::CertificatePrompt(CertPrompt {
            info,
            previous_fingerprint: old_fingerprint.and_then(to_fp),
            reply: tx,
        }));
        match state.wait_reply(&rx) {
            // 2 = accept for this session only: the TOFU store is ours (ADR 0001).
            Some(CertDecision::AcceptOnce | CertDecision::AcceptAndRemember) => 2,
            Some(CertDecision::Reject) | None => {
                state.cert_rejected.store(true, Ordering::SeqCst);
                0
            }
        }
    }
}

unsafe extern "C" fn verify_certificate_ex(
    instance: *mut ffi::freerdp,
    host: *const c_char,
    port: u16,
    _common_name: *const c_char,
    subject: *const c_char,
    issuer: *const c_char,
    fingerprint: *const c_char,
    flags: u32,
) -> u32 {
    // SAFETY: FreeRDP passes valid strings.
    unsafe {
        verify_cert(
            instance,
            host,
            port,
            subject,
            issuer,
            fingerprint,
            None,
            flags,
        )
    }
}

unsafe extern "C" fn verify_changed_certificate_ex(
    instance: *mut ffi::freerdp,
    host: *const c_char,
    port: u16,
    _common_name: *const c_char,
    subject: *const c_char,
    issuer: *const c_char,
    new_fingerprint: *const c_char,
    _old_subject: *const c_char,
    _old_issuer: *const c_char,
    old_fingerprint: *const c_char,
    flags: u32,
) -> u32 {
    // SAFETY: FreeRDP passes valid strings.
    unsafe {
        verify_cert(
            instance,
            host,
            port,
            subject,
            issuer,
            new_fingerprint,
            Some(old_fingerprint),
            flags,
        )
    }
}

// ---- painting ----------------------------------------------------------------------

/// # Safety
/// `gdi` must be valid with an initialised primary surface.
unsafe fn primary_hwnd(gdi: *mut ffi::rdpGdi) -> *mut ffi::GDI_WND {
    // SAFETY: per the contract.
    unsafe { (*(*(*gdi).primary).hdc).hwnd }
}

/// Paint FreeRDP's primary buffer black. GDI starts it, and every resized one, white
/// (0xFF), and the server paints over it only after a round trip: copied as is, the
/// session would flash white on connecting and on every resize.
///
/// # Safety
/// `gdi` must be valid.
unsafe fn clear_primary(gdi: *mut ffi::rdpGdi) {
    // SAFETY: per the contract; the primary buffer is `stride × height` bytes.
    unsafe {
        let buf = (*gdi).primary_buffer;
        if !buf.is_null() {
            ptr::write_bytes(buf, 0, (*gdi).stride as usize * (*gdi).height as usize);
        }
    }
}

/// # Safety
/// `gdi` must be valid.
unsafe fn copy_rects(gdi: *mut ffi::rdpGdi, frame: &SharedFrame, rects: &[Rect]) {
    // SAFETY: per the contract; the primary buffer is `stride × height` bytes.
    unsafe {
        let size = Size::new((*gdi).width as u32, (*gdi).height as u32);
        let src_stride = (*gdi).stride as usize;
        let src = (*gdi).primary_buffer;
        if src.is_null() {
            return;
        }
        let mut f = frame.lock();
        if f.size != size {
            f.resize(size);
        }
        let dst_stride = f.stride;
        for r in rects.iter().filter_map(|r| r.clip(size)) {
            let row_bytes = r.width as usize * 4;
            for y in r.y..r.y + r.height {
                let so = y as usize * src_stride + r.x as usize * 4;
                let d = y as usize * dst_stride + r.x as usize * 4;
                let src_row = std::slice::from_raw_parts(src.add(so), row_bytes);
                f.pixels[d..d + row_bytes].copy_from_slice(src_row);
            }
        }
    }
}

unsafe extern "C" fn begin_paint(ctx: *mut ffi::rdpContext) -> ffi::BOOL {
    // SAFETY: valid context with an initialised GDI.
    unsafe {
        let hwnd = primary_hwnd((*ctx).gdi);
        (*(*hwnd).invalid).null = TRUE;
        (*hwnd).ninvalid = 0;
    }
    TRUE
}

unsafe extern "C" fn end_paint(ctx: *mut ffi::rdpContext) -> ffi::BOOL {
    // SAFETY: valid context with an initialised GDI; may run on a channel thread,
    // which FreeRDP serialises against other paints.
    unsafe {
        let Some(state) = state(ctx) else { return TRUE };
        let gdi = (*ctx).gdi;
        let hwnd = primary_hwnd(gdi);
        let inv = (*hwnd).invalid;
        if (*inv).null != 0 {
            return TRUE;
        }
        let to_rect = |g: &ffi::GDI_RGN| {
            (g.w > 0 && g.h > 0 && g.x >= 0 && g.y >= 0)
                .then(|| Rect::new(g.x as u32, g.y as u32, g.w as u32, g.h as u32))
        };
        let n = (*hwnd).ninvalid.max(0) as usize;
        let mut damage: Vec<Rect> = if n > 0 && !(*hwnd).cinvalid.is_null() {
            std::slice::from_raw_parts((*hwnd).cinvalid, n)
                .iter()
                .filter_map(to_rect)
                .collect()
        } else {
            Vec::new()
        };
        if damage.is_empty() {
            damage.extend(to_rect(&*inv));
        }
        (*hwnd).ninvalid = 0;
        (*inv).null = TRUE;

        let frame = state
            .frame
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let (Some(frame), false) = (frame, damage.is_empty()) {
            copy_rects(gdi, &frame, &damage);
            state.emit(EngineEvent::FrameUpdate {
                damage,
                video: state.video.take_updates(),
            });
        }
    }
    TRUE
}

unsafe extern "C" fn desktop_resize(ctx: *mut ffi::rdpContext) -> ffi::BOOL {
    // SAFETY: valid context with an initialised GDI.
    unsafe {
        let Some(state) = state(ctx) else {
            return FALSE;
        };
        let s = (*ctx).settings;
        let w = ffi::freerdp_settings_get_uint32(s, ffi::FreeRDP_DesktopWidth);
        let h = ffi::freerdp_settings_get_uint32(s, ffi::FreeRDP_DesktopHeight);
        let gdi = (*ctx).gdi;
        if ffi::gdi_resize(gdi, w, h) == 0 {
            return FALSE;
        }
        clear_primary(gdi);
        // Keep showing the old picture (cropped or padded) until the server repaints,
        // rather than a blank one.
        let frame = state
            .frame
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(frame) = frame {
            frame.lock().resize_keeping(Size::new(w, h));
        }
        tracing::info!(w, h, "desktop resized");
        state.emit(EngineEvent::DesktopResized(Size::new(w, h)));
    }
    TRUE
}

// ---- pointer -----------------------------------------------------------------------

unsafe extern "C" fn pointer_new(
    ctx: *mut ffi::rdpContext,
    pointer: *mut ffi::rdpPointer,
) -> ffi::BOOL {
    // SAFETY: FreeRDP allocated `pointer` with our registered size.
    unsafe {
        let p = &mut *(pointer as *mut TransomPointer);
        let (w, h) = (p.base.width, p.base.height);
        let mut bgra = vec![0u8; w as usize * h as usize * 4];
        if w > 0 && h > 0 {
            let ok = ffi::freerdp_image_copy_from_pointer_data(
                bgra.as_mut_ptr(),
                PIXEL_FORMAT_BGRA32,
                w * 4,
                0,
                0,
                w,
                h,
                p.base.xorMaskData,
                p.base.lengthXorMask,
                p.base.andMaskData,
                p.base.lengthAndMask,
                p.base.xorBpp,
                &(*(*ctx).gdi).palette,
            );
            if ok == 0 {
                return FALSE;
            }
        }
        p.image = Box::into_raw(Box::new(PointerImage {
            size: Size::new(w, h),
            hotspot: (p.base.xPos, p.base.yPos),
            bgra: bgra.into(),
        }));
    }
    TRUE
}

unsafe extern "C" fn pointer_free(_ctx: *mut ffi::rdpContext, pointer: *mut ffi::rdpPointer) {
    // SAFETY: `image` is null or was created by Box::into_raw in pointer_new.
    unsafe {
        let p = &mut *(pointer as *mut TransomPointer);
        if !p.image.is_null() {
            drop(Box::from_raw(p.image));
            p.image = ptr::null_mut();
        }
    }
}

unsafe extern "C" fn pointer_set(
    ctx: *mut ffi::rdpContext,
    pointer: *mut ffi::rdpPointer,
) -> ffi::BOOL {
    // SAFETY: see pointer_new.
    unsafe {
        let p = &*(pointer as *const TransomPointer);
        if let (Some(state), Some(img)) = (state(ctx), p.image.as_ref()) {
            state.emit(EngineEvent::Pointer(PointerEvent::Image(img.clone())));
        }
    }
    TRUE
}

unsafe extern "C" fn pointer_set_null(ctx: *mut ffi::rdpContext) -> ffi::BOOL {
    // SAFETY: valid context.
    if let Some(state) = unsafe { state(ctx) } {
        state.emit(EngineEvent::Pointer(PointerEvent::Hidden));
    }
    TRUE
}

unsafe extern "C" fn pointer_set_default(ctx: *mut ffi::rdpContext) -> ffi::BOOL {
    // SAFETY: valid context.
    if let Some(state) = unsafe { state(ctx) } {
        state.emit(EngineEvent::Pointer(PointerEvent::Default));
    }
    TRUE
}

unsafe extern "C" fn pointer_set_position(ctx: *mut ffi::rdpContext, x: u32, y: u32) -> ffi::BOOL {
    // SAFETY: valid context.
    if let Some(state) = unsafe { state(ctx) } {
        state.emit(EngineEvent::PointerPosition { x, y });
    }
    TRUE
}

// ---- input -------------------------------------------------------------------------

/// # Safety
/// `input` must belong to a live, connected context; call on the engine thread.
unsafe fn send_input(input: *mut ffi::rdpInput, ev: InputEvent) {
    let c16 = |v: u32| v.min(u16::MAX as u32) as u16;
    // SAFETY: per the contract.
    unsafe {
        match ev {
            InputEvent::Key {
                scancode,
                extended,
                down,
            } => {
                let code = scancode as u32 | if extended { 0x100 } else { 0 };
                ffi::freerdp_input_send_keyboard_event_ex(input, down as ffi::BOOL, FALSE, code);
            }
            InputEvent::Unicode { code_unit, down } => {
                let flags = if down {
                    0
                } else {
                    ffi::KBD_FLAGS_RELEASE as u16
                };
                ffi::freerdp_input_send_unicode_keyboard_event(input, flags, code_unit);
            }
            InputEvent::MouseMove { x, y } => {
                ffi::freerdp_input_send_mouse_event(
                    input,
                    ffi::PTR_FLAGS_MOVE as u16,
                    c16(x),
                    c16(y),
                );
            }
            InputEvent::MouseButton { button, down, x, y } => {
                let down_flag = |f: u32| if down { f } else { 0 };
                match button {
                    MouseButton::Left | MouseButton::Right | MouseButton::Middle => {
                        let b = match button {
                            MouseButton::Left => ffi::PTR_FLAGS_BUTTON1,
                            MouseButton::Right => ffi::PTR_FLAGS_BUTTON2,
                            _ => ffi::PTR_FLAGS_BUTTON3,
                        };
                        let flags = (b | down_flag(ffi::PTR_FLAGS_DOWN)) as u16;
                        ffi::freerdp_input_send_mouse_event(input, flags, c16(x), c16(y));
                    }
                    MouseButton::Back | MouseButton::Forward => {
                        let b = if button == MouseButton::Back {
                            ffi::PTR_XFLAGS_BUTTON1
                        } else {
                            ffi::PTR_XFLAGS_BUTTON2
                        };
                        let flags = (b | down_flag(ffi::PTR_XFLAGS_DOWN)) as u16;
                        ffi::freerdp_input_send_extended_mouse_event(input, flags, c16(x), c16(y));
                    }
                }
            }
            InputEvent::Wheel { delta, x, y } | InputEvent::HWheel { delta, x, y } => {
                if delta == 0 {
                    return;
                }
                let axis = if matches!(ev, InputEvent::Wheel { .. }) {
                    ffi::PTR_FLAGS_WHEEL
                } else {
                    ffi::PTR_FLAGS_HWHEEL
                };
                // 9-bit two's complement rotation; bit 8 is PTR_FLAGS_WHEEL_NEGATIVE.
                let rotation = (delta.clamp(-255, 255) as u16 as u32) & 0x1FF;
                ffi::freerdp_input_send_mouse_event(
                    input,
                    (axis | rotation) as u16,
                    c16(x),
                    c16(y),
                );
            }
            InputEvent::SyncLocks {
                caps,
                num,
                scroll,
                kana,
            } => {
                let flags =
                    (scroll as u32) | (num as u32) << 1 | (caps as u32) << 2 | (kana as u32) << 3;
                ffi::freerdp_input_send_synchronize_event(input, flags);
            }
        }
    }
}

/// # Safety
/// As for [`send_input`].
unsafe fn send_ctrl_alt_del(input: *mut ffi::rdpInput) {
    const CTRL: (u8, bool) = (0x1D, false);
    const ALT: (u8, bool) = (0x38, false);
    const DEL: (u8, bool) = (0x53, true);
    for (down, keys) in [(true, [CTRL, ALT, DEL]), (false, [DEL, ALT, CTRL])] {
        for (scancode, extended) in keys {
            // SAFETY: per the contract.
            unsafe {
                send_input(
                    input,
                    InputEvent::Key {
                        scancode,
                        extended,
                        down,
                    },
                )
            };
        }
    }
}
