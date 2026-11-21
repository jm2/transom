// SPDX-License-Identifier: GPL-3.0-or-later
//! The backend-neutral RDP engine interface (engine trait v0, PLAN.md §2).
//!
//! A backend (FreeRDP today, maybe IronRDP later) runs the protocol on its own thread
//! and reports to the UI via an [`EventSink`]. The UI drives it with [`EngineCommand`]s
//! through an [`EngineHandle`]. Neither side depends on a GUI toolkit.

use std::fmt;
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use secrecy::SecretString;

use crate::config::SessionConfig;
use crate::geometry::{DesktopRequest, ScaleFactors, Size};
use crate::trust::CertInfo;

/// A rectangle in remote pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Clip to a framebuffer of `size`; `None` if nothing remains.
    pub fn clip(self, size: Size) -> Option<Rect> {
        let x2 = self.x.saturating_add(self.width).min(size.width);
        let y2 = self.y.saturating_add(self.height).min(size.height);
        (self.x < x2 && self.y < y2).then(|| Rect::new(self.x, self.y, x2 - self.x, y2 - self.y))
    }
}

/// Pixel layout of the shared framebuffer. Matches FreeRDP `PIXEL_FORMAT_BGRX32`
/// (= GDK `B8G8R8X8` on little endian).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    Bgrx8888,
}

/// The remote framebuffer, written by the engine thread and read by the renderer.
#[derive(Debug)]
pub struct FrameData {
    pub size: Size,
    pub stride: usize,
    pub format: PixelFormat,
    pub pixels: Vec<u8>,
    /// Bumped on every resize so renderers can drop cached textures.
    pub generation: u64,
}

impl FrameData {
    pub fn new(size: Size) -> Self {
        let stride = size.width as usize * 4;
        Self {
            size,
            stride,
            format: PixelFormat::Bgrx8888,
            pixels: vec![0; stride * size.height as usize],
            generation: 0,
        }
    }

    pub fn resize(&mut self, size: Size) {
        self.size = size;
        self.stride = size.width as usize * 4;
        self.pixels.clear();
        self.pixels.resize(self.stride * size.height as usize, 0);
        self.generation += 1;
    }
}

/// Shared handle to the framebuffer.
#[derive(Debug, Clone)]
pub struct SharedFrame(Arc<Mutex<FrameData>>);

impl SharedFrame {
    pub fn new(size: Size) -> Self {
        Self(Arc::new(Mutex::new(FrameData::new(size))))
    }

    /// Lock the frame. A poisoned lock (engine thread panicked mid-write) still
    /// yields the data; at worst a frame is torn.
    pub fn lock(&self) -> MutexGuard<'_, FrameData> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// A pointer (cursor) image at remote device resolution: BGRA, straight (not
/// premultiplied) alpha, stride = width × 4.
#[derive(Debug, Clone)]
pub struct PointerImage {
    pub size: Size,
    pub hotspot: (u32, u32),
    pub bgra: Arc<[u8]>,
}

/// Pointer shape changes.
#[derive(Debug, Clone)]
pub enum PointerEvent {
    Image(PointerImage),
    Hidden,
    Default,
}

/// Why a session ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisconnectReason {
    /// The user asked to disconnect.
    Local,
    /// The server closed the session (logoff, admin disconnect…).
    Remote(String),
    AuthenticationFailed,
    CertificateRejected,
    Network(String),
    Protocol(String),
}

impl fmt::Display for DisconnectReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local => write!(f, "disconnected"),
            Self::Remote(m) => write!(f, "the server ended the session: {m}"),
            Self::AuthenticationFailed => write!(f, "authentication failed"),
            Self::CertificateRejected => write!(f, "the server certificate was not trusted"),
            Self::Network(m) => write!(f, "network error: {m}"),
            Self::Protocol(m) => write!(f, "protocol error: {m}"),
        }
    }
}

/// Credentials supplied in reply to [`EngineEvent::AuthRequired`].
#[derive(Debug, Clone)]
pub struct Credentials {
    pub username: String,
    pub domain: Option<String>,
    pub password: SecretString,
}

/// What the engine needs credentials for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthKind {
    Server,
    Gateway,
}

/// A request for credentials. Reply exactly once; dropping it cancels the connection.
#[derive(Debug)]
pub struct AuthRequest {
    pub kind: AuthKind,
    pub username: Option<String>,
    pub domain: Option<String>,
    pub reply: mpsc::SyncSender<Option<Credentials>>,
}

/// User decision for a certificate the trust store couldn't settle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CertDecision {
    Reject,
    AcceptOnce,
    AcceptAndRemember,
}

/// A certificate that needs a user decision. Dropping it rejects.
#[derive(Debug)]
pub struct CertPrompt {
    pub info: CertInfo,
    /// Set when the host previously presented a different certificate.
    pub previous_fingerprint: Option<String>,
    pub reply: mpsc::SyncSender<CertDecision>,
}

/// Events from the engine to the UI. Sent from the engine thread.
#[derive(Debug)]
pub enum EngineEvent {
    Connected {
        desktop: Size,
        scale: ScaleFactors,
        frame: SharedFrame,
    },
    /// Regions of the shared frame that changed (remote pixels).
    FrameUpdate {
        damage: Vec<Rect>,
    },
    /// The remote desktop size changed; the shared frame was resized.
    DesktopResized(Size),
    Pointer(PointerEvent),
    /// The server moved the pointer (remote pixels).
    PointerPosition {
        x: u32,
        y: u32,
    },
    AuthRequired(AuthRequest),
    CertificatePrompt(CertPrompt),
    /// Server redirection in progress; the engine reconnects by itself.
    Redirecting {
        target: String,
    },
    /// Display Control channel became available: dynamic resize now possible.
    ResizeAvailable,
    Disconnected(DisconnectReason),
}

/// Receives engine events. Implementations must be cheap and non-blocking (e.g. push
/// onto a channel that the UI main loop drains).
pub type EventSink = Box<dyn Fn(EngineEvent) + Send + 'static>;

/// Mouse buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

/// Input in remote coordinates / RDP scancodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEvent {
    /// `scancode` is a set-1 scancode; `extended` selects the E0 prefix.
    Key {
        scancode: u8,
        extended: bool,
        down: bool,
    },
    /// A Unicode character for keys without a scancode mapping.
    Unicode {
        code_unit: u16,
        down: bool,
    },
    MouseMove {
        x: u32,
        y: u32,
    },
    MouseButton {
        button: MouseButton,
        down: bool,
        x: u32,
        y: u32,
    },
    /// Positive = away from the user. At most ±120 per event (see `WheelAccumulator`).
    Wheel {
        delta: i16,
        x: u32,
        y: u32,
    },
    /// Positive = right.
    HWheel {
        delta: i16,
        x: u32,
        y: u32,
    },
    /// Keyboard lock state (sent on focus-in).
    SyncLocks {
        caps: bool,
        num: bool,
        scroll: bool,
        kana: bool,
    },
}

/// Commands from the UI to the engine.
#[derive(Debug, Clone)]
pub enum EngineCommand {
    /// Display Control monitor layout (single monitor for now).
    Layout(DesktopRequest),
    Input(InputEvent),
    /// Send the secure attention sequence.
    CtrlAltDel,
    Disconnect,
}

/// Errors starting an engine.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("invalid configuration: {0}")]
    Config(String),
    #[error("backend initialisation failed: {0}")]
    Init(String),
}

/// Set by the engine thread as the very last thing it does, so applications can
/// wait for a clean exit (FreeRDP must not be torn down by process exit).
#[derive(Debug, Clone, Default)]
pub struct Finished(Arc<(Mutex<bool>, Condvar)>);

impl Finished {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self) {
        let (lock, cv) = &*self.0;
        *lock.lock().unwrap_or_else(|e| e.into_inner()) = true;
        cv.notify_all();
    }

    pub fn is_set(&self) -> bool {
        *self.0.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Wait up to `timeout`; returns whether the engine finished.
    pub fn wait(&self, timeout: Duration) -> bool {
        let (lock, cv) = &*self.0;
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let (guard, _) = cv
            .wait_timeout_while(guard, timeout, |done| !*done)
            .unwrap_or_else(|e| e.into_inner());
        *guard
    }

    /// A guard that calls [`Finished::set`] when dropped, including on panic.
    pub fn guard(&self) -> FinishedGuard {
        FinishedGuard(self.clone())
    }
}

/// See [`Finished::guard`].
#[derive(Debug)]
pub struct FinishedGuard(Finished);

impl Drop for FinishedGuard {
    fn drop(&mut self) {
        self.0.set();
    }
}

/// UI-side handle to a running engine. Dropping it disconnects (asynchronously).
#[derive(Debug)]
pub struct EngineHandle {
    tx: mpsc::Sender<EngineCommand>,
    waker: Arc<dyn Waker>,
    finished: Finished,
}

/// Lets the UI wake the engine thread after queueing a command (the engine thread
/// usually sleeps in a backend-specific wait, e.g. `WaitForMultipleObjects`).
pub trait Waker: Send + Sync + fmt::Debug {
    fn wake(&self);
    /// Abort a connection attempt that is blocked inside the backend (DNS, TCP, TLS,
    /// NLA). Must be safe to call at any time from any thread.
    fn abort(&self) {}
}

impl EngineHandle {
    pub fn new(tx: mpsc::Sender<EngineCommand>, waker: Arc<dyn Waker>, finished: Finished) -> Self {
        Self {
            tx,
            waker,
            finished,
        }
    }

    /// Disconnect and wait up to `timeout` for the engine thread to exit. Call this
    /// before the process exits. Returns whether the engine finished in time.
    pub fn shutdown(self, timeout: Duration) -> bool {
        let finished = self.finished.clone();
        drop(self); // sends Disconnect + abort
        finished.wait(timeout)
    }

    pub fn is_finished(&self) -> bool {
        self.finished.is_set()
    }

    /// A handle to wait on the engine thread's exit after this handle is dropped.
    pub fn finished(&self) -> Finished {
        self.finished.clone()
    }

    /// Queue a command. Returns false if the engine has already stopped.
    pub fn send(&self, cmd: EngineCommand) -> bool {
        let disconnect = matches!(cmd, EngineCommand::Disconnect);
        let ok = self.tx.send(cmd).is_ok();
        self.waker.wake();
        if disconnect {
            self.waker.abort();
        }
        ok
    }
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        let _ = self.tx.send(EngineCommand::Disconnect);
        self.waker.wake();
        self.waker.abort();
    }
}

/// An RDP backend. `connect` spawns the engine thread and returns immediately.
pub trait RdpEngine {
    fn connect(&self, config: SessionConfig, sink: EventSink) -> Result<EngineHandle, EngineError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_clip() {
        let s = Size::new(100, 50);
        assert_eq!(
            Rect::new(90, 40, 20, 20).clip(s),
            Some(Rect::new(90, 40, 10, 10))
        );
        assert_eq!(Rect::new(100, 0, 5, 5).clip(s), None);
        assert_eq!(Rect::new(0, 0, 0, 5).clip(s), None);
        assert_eq!(Rect::new(u32::MAX, 0, 5, 5).clip(s), None);
    }

    #[test]
    fn finished_guard_sets_on_panic() {
        let f = Finished::new();
        let g = f.guard();
        let r = std::thread::spawn(move || {
            let _g = g;
            panic!("engine died");
        })
        .join();
        assert!(r.is_err());
        assert!(f.wait(Duration::from_secs(1)));
    }

    #[test]
    fn frame_resize_bumps_generation() {
        let f = SharedFrame::new(Size::new(4, 4));
        f.lock().resize(Size::new(8, 2));
        let g = f.lock();
        assert_eq!(g.generation, 1);
        assert_eq!(g.pixels.len(), 8 * 4 * 2);
    }
}
