// SPDX-License-Identifier: Apache-2.0
//! C ABI for `TransomView` (see `include/transom-view.h`). All `unsafe` for the C
//! API lives here (PLAN §9).
#![allow(unsafe_code)]
#![allow(clippy::missing_safety_doc)] // documented in the C header

use std::ffi::{CStr, c_char, c_int};

use gtk::glib::translate::{FromGlibPtrBorrow, IntoGlib, IntoGlibPtr};
use gtk::glib::{self, gobject_ffi};
use gtk::prelude::*;
use transom_view::TransomView;
use transom_view::core::config::{
    AudioMode, CertPolicy, ClipboardMode, DEFAULT_PORT, SecurityMode, SessionConfig,
};
use transom_view::core::engine::CertDecision;
use transom_view::core::geometry::{FitMode, ScaleMode, desktop_request};
use transom_view::core::secrecy::SecretString;
use transom_view_freerdp::FreeRdpEngine;

type CView = <TransomView as glib::object::ObjectType>::GlibType;

/// # Safety
/// `p` must be a valid `TransomView*`.
unsafe fn view(p: *mut CView) -> glib::translate::Borrowed<TransomView> {
    // SAFETY: per the contract.
    unsafe { TransomView::from_glib_borrow(p) }
}

/// # Safety
/// `p` must be null or a valid NUL-terminated string.
unsafe fn opt_str(p: *const c_char) -> Option<String> {
    // SAFETY: per the contract.
    (!p.is_null()).then(|| unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
}

/// Make gtk-rs aware that GTK is initialised. In a C/Python/Vala app GTK is
/// initialised from C, which gtk-rs can't see, and it refuses to run widget class
/// setup otherwise.
fn mark_gtk_initialized() {
    if gtk::is_initialized() {
        return;
    }
    // SAFETY: plain query of GTK's own state.
    if unsafe { gtk::ffi::gtk_is_initialized() } != glib::ffi::GFALSE {
        // SAFETY: GTK really is initialised (by the C caller), on this thread: type
        // registration and widget creation happen on the GTK thread.
        unsafe { gtk::set_initialized() };
    } else {
        // Not initialised yet (e.g. the GIR dumper): do it properly.
        let _ = gtk::init();
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn transom_view_get_type() -> glib::ffi::GType {
    mark_gtk_initialized();
    TransomView::static_type().into_glib()
}

#[unsafe(no_mangle)]
pub extern "C" fn transom_view_new() -> *mut gtk::ffi::GtkWidget {
    mark_gtk_initialized();
    let view = TransomView::new();
    let ptr: *mut gtk::ffi::GtkWidget = view.upcast::<gtk::Widget>().into_glib_ptr();
    // SAFETY: we own the only reference; C constructors return floating widgets.
    unsafe { gobject_ffi::g_object_force_floating(ptr.cast()) };
    ptr
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn transom_view_connect_to(
    this: *mut CView,
    host: *const c_char,
    port: u16,
    username: *const c_char,
    domain: *const c_char,
    password: *const c_char,
    error: *mut *mut glib::ffi::GError,
) -> glib::ffi::gboolean {
    // SAFETY: per the C API contract (valid view, valid or null strings).
    let (v, host, username, domain, password) = unsafe {
        (
            view(this),
            opt_str(host),
            opt_str(username),
            opt_str(domain),
            opt_str(password),
        )
    };
    let set_error = |msg: String| {
        if !error.is_null() {
            let e = glib::Error::new(gtk::gio::IOErrorEnum::Failed, &msg);
            // SAFETY: `error` is a valid GError** per the GLib convention.
            unsafe { *error = e.into_glib_ptr() };
        }
        glib::ffi::GFALSE
    };
    let Some(host) = host.filter(|h| !h.is_empty()) else {
        return set_error("no host".into());
    };
    let config = SessionConfig {
        host,
        port: if port == 0 { DEFAULT_PORT } else { port },
        username,
        domain,
        password: password.map(SecretString::from),
        security: SecurityMode::Negotiate,
        cert_policy: CertPolicy::Verify,
        scale_mode: ScaleMode::MatchLocal,
        fit_mode: FitMode::Fit,
        clipboard: ClipboardMode::Both,
        audio: AudioMode::Local,
        microphone: false,
        kerberos: false,
        initial: desktop_request(ScaleMode::MatchLocal, Default::default(), 1.0),
        monitors: Vec::new(),
        gateway: None,
        video_decode: Default::default(),
        redirections: Default::default(),
    };
    let engine = FreeRdpEngine::new(glib::user_cache_dir().join("transom-view").join("freerdp"));
    match v.connect_session(&engine, config) {
        Ok(()) => glib::ffi::GTRUE,
        Err(e) => set_error(e.to_string()),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn transom_view_disconnect(this: *mut CView) {
    // SAFETY: valid view per the C API contract.
    unsafe { view(this) }.disconnect();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn transom_view_shutdown(
    this: *mut CView,
    timeout_ms: u32,
) -> glib::ffi::gboolean {
    // SAFETY: valid view per the C API contract.
    let ok = unsafe { view(this) }.shutdown(std::time::Duration::from_millis(timeout_ms as u64));
    ok.into_glib()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn transom_view_is_connected(this: *mut CView) -> glib::ffi::gboolean {
    // SAFETY: valid view per the C API contract.
    unsafe { view(this) }.is_connected().into_glib()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn transom_view_send_ctrl_alt_del(this: *mut CView) {
    // SAFETY: valid view per the C API contract.
    unsafe { view(this) }.send_ctrl_alt_del();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn transom_view_answer_certificate(this: *mut CView, decision: c_int) {
    let d = match decision {
        1 => CertDecision::AcceptOnce,
        2 => CertDecision::AcceptAndRemember,
        _ => CertDecision::Reject,
    };
    // SAFETY: valid view per the C API contract.
    unsafe { view(this) }.answer_certificate(d);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn transom_view_answer_credentials(
    this: *mut CView,
    username: *const c_char,
    domain: *const c_char,
    password: *const c_char,
) {
    // SAFETY: valid view and strings per the C API contract.
    unsafe {
        let user = opt_str(username).unwrap_or_default();
        let dom = opt_str(domain);
        let pw = SecretString::from(opt_str(password).unwrap_or_default());
        view(this).answer_credentials(&user, dom.as_deref(), pw);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn transom_view_cancel_credentials(this: *mut CView) {
    // SAFETY: valid view per the C API contract.
    unsafe { view(this) }.cancel_credentials();
}
