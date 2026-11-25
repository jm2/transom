// SPDX-License-Identifier: Apache-2.0
//! The backend-neutral RDP engine interface (engine trait v0, PLAN.md §2).
//!
//! A backend (FreeRDP today, maybe IronRDP later) runs the protocol on its own thread
//! and reports to the UI via an [`EventSink`]. The UI drives it with [`EngineCommand`]s
//! through an [`EngineHandle`]. Neither side depends on a GUI toolkit.

use std::collections::VecDeque;
use std::fmt;
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use secrecy::SecretString;

use crate::clipboard::{ClipFormat, FileRange};
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

    /// The overlap of two rectangles, if any.
    pub fn intersect(self, other: Rect) -> Option<Rect> {
        let x0 = self.x.max(other.x);
        let y0 = self.y.max(other.y);
        let x1 = self
            .x
            .saturating_add(self.width)
            .min(other.x.saturating_add(other.width));
        let y1 = self
            .y
            .saturating_add(self.height)
            .min(other.y.saturating_add(other.height));
        (x0 < x1 && y0 < y1).then(|| Rect::new(x0, y0, x1 - x0, y1 - y0))
    }

    /// Clip to a framebuffer of `size`; `None` if nothing remains.
    pub fn clip(self, size: Size) -> Option<Rect> {
        let x2 = self.x.saturating_add(self.width).min(size.width);
        let y2 = self.y.saturating_add(self.height).min(size.height);
        (self.x < x2 && self.y < y2).then(|| Rect::new(self.x, self.y, x2 - self.x, y2 - self.y))
    }

    pub fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }

    pub fn area(self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }

    /// The smallest rectangle containing both (an empty one adds nothing). Edges
    /// saturate at `u32::MAX`.
    pub fn bounding(self, other: Rect) -> Rect {
        if other.is_empty() {
            return self;
        }
        if self.is_empty() {
            return other;
        }
        let x0 = self.x.min(other.x);
        let y0 = self.y.min(other.y);
        let x1 = self
            .x
            .saturating_add(self.width)
            .max(other.x.saturating_add(other.width));
        let y1 = self
            .y
            .saturating_add(self.height)
            .max(other.y.saturating_add(other.height));
        Rect::new(x0, y0, x1 - x0, y1 - y0)
    }
}

/// Most damage rectangles one frame update carries; past this it carries their
/// bounding box instead. Each rectangle is copied out of the backend's framebuffer
/// and marks tiles separately, and the server decides how many there are.
pub const MAX_DAMAGE_RECTS: usize = 64;

/// `rects` as a frame update should carry them: unchanged, or their bounding box
/// alone when there are more than [`MAX_DAMAGE_RECTS`] of them or they overlap so much
/// that copying each would move more pixels than the box holds.
pub fn coalesce_damage(rects: Vec<Rect>) -> Vec<Rect> {
    let bounds = rects.iter().fold(Rect::default(), |b, r| b.bounding(*r));
    let area: u64 = rects.iter().map(|r| r.area()).sum();
    if rects.len() > MAX_DAMAGE_RECTS || area > bounds.area() {
        if bounds.is_empty() {
            Vec::new()
        } else {
            vec![bounds]
        }
    } else {
        rects
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

    /// Like [`new`](Self::new), but a size that can't be allocated is an error rather
    /// than an abort. For sizes a server chose.
    pub fn try_new(size: Size) -> Result<Self, FrameAllocError> {
        let (stride, pixels) = zeroed_pixels(size)?;
        Ok(Self {
            size,
            stride,
            format: PixelFormat::Bgrx8888,
            pixels,
            generation: 0,
        })
    }

    /// Like [`resize`](Self::resize), but fallible; on error the frame is unchanged.
    pub fn try_resize(&mut self, size: Size) -> Result<(), FrameAllocError> {
        let (stride, pixels) = zeroed_pixels(size)?;
        self.size = size;
        self.stride = stride;
        self.pixels = pixels;
        self.generation += 1;
        Ok(())
    }

    /// Resize, keeping the pixels the old and new sizes share (the rest black): a
    /// renderer then shows the old picture until the remote repaints at the new size.
    ///
    /// # Panics
    /// If the new frame can't be allocated; see
    /// [`try_resize_keeping`](Self::try_resize_keeping).
    pub fn resize_keeping(&mut self, size: Size) {
        if let Err(e) = self.try_resize_keeping(size) {
            panic!("{e}");
        }
    }

    /// Like [`resize_keeping`](Self::resize_keeping), but a size that can't be
    /// allocated is an error rather than an abort; on error the frame is unchanged.
    pub fn try_resize_keeping(&mut self, size: Size) -> Result<(), FrameAllocError> {
        let mut next = Self::try_new(size)?;
        next.format = self.format;
        next.generation = self.generation + 1;
        let row = self.size.width.min(size.width) as usize * 4;
        for y in 0..self.size.height.min(size.height) as usize {
            next.pixels[y * next.stride..][..row]
                .copy_from_slice(&self.pixels[y * self.stride..][..row]);
        }
        *self = next;
        Ok(())
    }
}

/// A framebuffer of this size couldn't be allocated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("can't allocate a {}×{} framebuffer", .0.width, .0.height)]
pub struct FrameAllocError(pub Size);

/// Stride and zeroed pixels for a BGRX frame of `size`, allocated fallibly.
fn zeroed_pixels(size: Size) -> Result<(usize, Vec<u8>), FrameAllocError> {
    let err = || FrameAllocError(size);
    let stride = (size.width as usize).checked_mul(4).ok_or_else(err)?;
    let len = stride.checked_mul(size.height as usize).ok_or_else(err)?;
    let mut pixels = Vec::new();
    pixels.try_reserve_exact(len).map_err(|_| err())?;
    pixels.resize(len, 0);
    Ok((stride, pixels))
}

/// Shared handle to the framebuffer.
#[derive(Debug, Clone)]
pub struct SharedFrame(Arc<Mutex<FrameData>>);

impl SharedFrame {
    pub fn new(size: Size) -> Self {
        Self(Arc::new(Mutex::new(FrameData::new(size))))
    }

    /// Like [`new`](Self::new), but a size that can't be allocated is an error.
    pub fn try_new(size: Size) -> Result<Self, FrameAllocError> {
        Ok(Self(Arc::new(Mutex::new(FrameData::try_new(size)?))))
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

/// How far a connection attempt has got, for progress feedback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectStage {
    /// Reaching the server (or its RD Gateway) and securing the connection (TLS).
    Contacting,
    /// Network Level Authentication, or a Microsoft Entra ID sign-in.
    Authenticating,
    /// The RDP connection sequence: channels, licensing, capabilities.
    Configuring,
}

impl ConnectStage {
    /// A stable name, for the view's `progress` signal.
    pub fn nick(self) -> &'static str {
        match self {
            Self::Contacting => "contacting",
            Self::Authenticating => "authenticating",
            Self::Configuring => "configuring",
        }
    }

    /// Inverse of [`nick`](Self::nick).
    pub fn from_nick(nick: &str) -> Option<Self> {
        [Self::Contacting, Self::Authenticating, Self::Configuring]
            .into_iter()
            .find(|s| s.nick() == nick)
    }
}

/// Events from the engine to the UI. Sent from the engine thread.
#[derive(Debug)]
pub enum EngineEvent {
    /// The connection attempt reached a new stage (before `Connected`; again for each
    /// reconnection, such as a server redirection).
    Progress(ConnectStage),
    Connected {
        desktop: Size,
        scale: ScaleFactors,
        frame: SharedFrame,
    },
    /// Regions of the shared frame that changed (remote pixels), and the video layers
    /// (hardware-decoded H.264, ADR 0006) that changed in the same frame.
    FrameUpdate {
        damage: Vec<Rect>,
        video: Vec<crate::video::VideoUpdate>,
    },
    /// Drop every video layer (ADR 0006): the graphics channel closed or opened again,
    /// so none of the surfaces shown before exist any more. Later `FrameUpdate`s start
    /// from no layers.
    VideoReset,
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
    /// Something the user should know about the session, in plain text (for example,
    /// that it connected with weaker protection than requested).
    Notice(String),
    /// Server redirection in progress; the engine reconnects by itself.
    Redirecting {
        target: String,
    },
    /// Display Control channel became available: dynamic resize now possible.
    ResizeAvailable,
    /// The remote clipboard changed; these formats are available (may be empty).
    ClipboardRemoteFormats(Vec<ClipFormat>),
    /// Reply to [`EngineCommand::ClipboardRequest`]; `None` if the remote failed.
    ClipboardRemoteData {
        format: ClipFormat,
        data: Option<Vec<u8>>,
    },
    /// The remote wants our clipboard content in `format`. Answer with
    /// [`EngineCommand::ClipboardReply`] (always, even on failure).
    ClipboardDataRequest(ClipFormat),
    /// The remote wants (part of) file `index` of the file list we offered. Answer
    /// with [`EngineCommand::ClipboardFileReply`] using the same `stream_id`.
    ClipboardFileRequest {
        stream_id: u32,
        index: u32,
        range: FileRange,
    },
    /// Reply to [`EngineCommand::ClipboardFileRequest`].
    ClipboardFileData {
        stream_id: u32,
        data: Option<Vec<u8>>,
    },
    Disconnected(DisconnectReason),
}

/// Receives engine events. Implementations must be cheap and non-blocking (e.g. push
/// onto a channel that the UI main loop drains).
pub type EventSink = Box<dyn Fn(EngineEvent) + Send + 'static>;

/// Engine events on their way to the UI, merged while they wait. If the UI falls
/// behind, a frame update folds into the one already queued (damage and video layer
/// changes combined) and a pointer change replaces the queued one, so the queue
/// doesn't grow with the server's frame rate. Every other event keeps its place.
#[derive(Debug, Default)]
pub struct EventQueue {
    events: VecDeque<EngineEvent>,
}

impl EventQueue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, ev: EngineEvent) {
        // The events this one may merge with: the trailing run of frame updates and
        // pointer changes, whose order relative to each other doesn't matter.
        let tail = self
            .events
            .iter()
            .rev()
            .take_while(|e| {
                matches!(
                    e,
                    EngineEvent::FrameUpdate { .. }
                        | EngineEvent::Pointer(_)
                        | EngineEvent::PointerPosition { .. }
                )
            })
            .count();
        let start = self.events.len() - tail;
        let mut tail = self.events.range_mut(start..);
        match ev {
            EngineEvent::FrameUpdate { damage, video } => {
                if let Some(EngineEvent::FrameUpdate {
                    damage: queued,
                    video: queued_video,
                }) = tail.find(|e| matches!(e, EngineEvent::FrameUpdate { .. }))
                {
                    queued.extend(damage);
                    *queued = coalesce_damage(std::mem::take(queued));
                    crate::video::merge_updates(queued_video, video);
                } else {
                    self.events
                        .push_back(EngineEvent::FrameUpdate { damage, video });
                }
            }
            EngineEvent::Pointer(p) => match tail.find(|e| matches!(e, EngineEvent::Pointer(_))) {
                Some(queued) => *queued = EngineEvent::Pointer(p),
                None => self.events.push_back(EngineEvent::Pointer(p)),
            },
            EngineEvent::PointerPosition { x, y } => {
                match tail.find(|e| matches!(e, EngineEvent::PointerPosition { .. })) {
                    Some(queued) => *queued = EngineEvent::PointerPosition { x, y },
                    None => self.events.push_back(EngineEvent::PointerPosition { x, y }),
                }
            }
            ev => self.events.push_back(ev),
        }
    }

    /// Everything queued, in order, leaving the queue empty.
    pub fn take(&mut self) -> VecDeque<EngineEvent> {
        std::mem::take(&mut self.events)
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

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
    /// Display Control monitor layout, single monitor.
    Layout(DesktopRequest),
    /// Display Control monitor layout, several monitors (see `monitors::layout`).
    LayoutMonitors(Vec<crate::monitors::RemoteMonitor>),
    Input(InputEvent),
    /// Send the secure attention sequence.
    CtrlAltDel,
    /// The local clipboard changed; announce these formats to the remote.
    ClipboardLocalFormats(Vec<ClipFormat>),
    /// Fetch remote clipboard content (answered by `ClipboardRemoteData`).
    ClipboardRequest(ClipFormat),
    /// Answer a `ClipboardDataRequest`; `None` reports failure.
    ClipboardReply {
        format: ClipFormat,
        data: Option<Vec<u8>>,
    },
    /// Fetch (part of) file `index` of the remote's file list.
    ClipboardFileRequest {
        stream_id: u32,
        index: u32,
        range: FileRange,
    },
    /// Answer a `ClipboardFileRequest`; `None` reports failure.
    ClipboardFileReply {
        stream_id: u32,
        data: Option<Vec<u8>>,
    },
    /// The renderer can't show the dmabufs of `FrameUpdate::video` (e.g. no GPU
    /// import): stop hardware decoding and use the framebuffer for everything.
    VideoUnsupported,
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
    fn resize_keeping_keeps_the_shared_pixels() {
        let mut f = FrameData::new(Size::new(3, 2));
        for (i, b) in f.pixels.iter_mut().enumerate() {
            *b = i as u8 + 1;
        }
        let generation = f.generation;
        f.resize_keeping(Size::new(2, 3));
        assert_eq!(f.size, Size::new(2, 3));
        assert_eq!(f.stride, 8);
        assert_eq!(f.generation, generation + 1);
        // Row 0 and 1: the first two pixels of the old rows; row 2: black.
        assert_eq!(&f.pixels[0..8], &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(&f.pixels[8..16], &[13, 14, 15, 16, 17, 18, 19, 20]);
        assert_eq!(&f.pixels[16..24], &[0; 8]);
    }

    #[test]
    fn connect_stage_nicks_round_trip() {
        for s in [
            ConnectStage::Contacting,
            ConnectStage::Authenticating,
            ConnectStage::Configuring,
        ] {
            assert_eq!(ConnectStage::from_nick(s.nick()), Some(s));
        }
        assert_eq!(ConnectStage::from_nick("bogus"), None);
    }

    #[test]
    fn rect_intersect() {
        let a = Rect::new(0, 0, 10, 10);
        assert_eq!(
            a.intersect(Rect::new(5, 5, 10, 10)),
            Some(Rect::new(5, 5, 5, 5))
        );
        assert_eq!(a.intersect(Rect::new(10, 0, 5, 5)), None);
    }

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
    fn frame_allocation_failure_is_an_error() {
        // Overflows the byte count, and one no allocator can satisfy.
        let huge = Size::new(u32::MAX, u32::MAX);
        assert_eq!(FrameData::try_new(huge).unwrap_err(), FrameAllocError(huge));
        let huge = Size::new(1 << 30, 1 << 30);
        assert!(FrameData::try_new(huge).is_err());
        assert!(SharedFrame::try_new(huge).is_err());
        // A failed resize leaves the frame as it was.
        let mut f = FrameData::try_new(Size::new(4, 2)).unwrap();
        f.pixels[0] = 7;
        assert!(f.try_resize_keeping(huge).is_err());
        assert!(f.try_resize(huge).is_err());
        assert_eq!((f.size, f.stride, f.generation), (Size::new(4, 2), 16, 0));
        assert_eq!((f.pixels.len(), f.pixels[0]), (32, 7));
        f.try_resize(Size::new(2, 2)).unwrap();
        assert_eq!((f.stride, f.pixels.len(), f.generation), (8, 16, 1));
        assert!(f.pixels.iter().all(|&b| b == 0));
    }

    #[test]
    fn rect_bounding() {
        let a = Rect::new(10, 10, 5, 5);
        assert_eq!(a.bounding(Rect::new(0, 20, 2, 2)), Rect::new(0, 10, 15, 12));
        assert_eq!(a.bounding(Rect::new(100, 100, 0, 5)), a);
        assert_eq!(Rect::default().bounding(a), a);
        assert_eq!(
            Rect::new(u32::MAX - 1, 0, 10, 1).bounding(Rect::new(0, 0, 1, 1)),
            Rect::new(0, 0, u32::MAX, 1)
        );
    }

    #[test]
    fn damage_coalescing() {
        // Few, disjoint: unchanged.
        let few = vec![Rect::new(0, 0, 10, 10), Rect::new(100, 100, 10, 10)];
        assert_eq!(coalesce_damage(few.clone()), few);
        // Overlapping copies cost more than their bounding box.
        let same = vec![Rect::new(5, 5, 10, 10); 3];
        assert_eq!(coalesce_damage(same), vec![Rect::new(5, 5, 10, 10)]);
        // Too many: the bounding box, however sparse.
        let many: Vec<Rect> = (0..=MAX_DAMAGE_RECTS as u32)
            .map(|i| Rect::new(i * 20, i, 1, 1))
            .collect();
        assert_eq!(
            coalesce_damage(many),
            vec![Rect::new(0, 0, MAX_DAMAGE_RECTS as u32 * 20 + 1, 65)]
        );
        let empty = vec![Rect::default(); MAX_DAMAGE_RECTS + 1];
        assert!(coalesce_damage(empty).is_empty());
        assert!(coalesce_damage(Vec::new()).is_empty());
    }

    fn frame_update(damage: &[Rect]) -> EngineEvent {
        EngineEvent::FrameUpdate {
            damage: damage.to_vec(),
            video: Vec::new(),
        }
    }

    #[test]
    fn event_queue_merges_frame_updates() {
        let mut q = EventQueue::new();
        q.push(frame_update(&[Rect::new(0, 0, 1, 1)]));
        q.push(EngineEvent::Pointer(PointerEvent::Hidden));
        q.push(frame_update(&[Rect::new(5, 5, 1, 1)]));
        q.push(EngineEvent::Pointer(PointerEvent::Default));
        q.push(EngineEvent::PointerPosition { x: 1, y: 2 });
        q.push(EngineEvent::PointerPosition { x: 3, y: 4 });
        for i in 0..1000 {
            q.push(frame_update(&[Rect::new(i, 0, 1, 1)]));
        }
        assert_eq!(q.len(), 3);
        let ev: Vec<_> = q.take().into_iter().collect();
        assert!(q.is_empty());
        match &ev[..] {
            [
                EngineEvent::FrameUpdate { damage, .. },
                EngineEvent::Pointer(PointerEvent::Default),
                EngineEvent::PointerPosition { x: 3, y: 4 },
            ] => {
                assert!(damage.len() <= MAX_DAMAGE_RECTS);
                let bounds = damage.iter().fold(Rect::default(), |b, r| b.bounding(*r));
                assert_eq!(bounds, Rect::new(0, 0, 1000, 6));
                // Nothing that was damaged is lost.
                for p in [Rect::new(5, 5, 1, 1), Rect::new(999, 0, 1, 1)] {
                    assert!(damage.iter().any(|r| r.intersect(p) == Some(p)));
                }
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn event_queue_keeps_order_across_other_events() {
        let mut q = EventQueue::new();
        q.push(frame_update(&[Rect::new(0, 0, 1, 1)]));
        q.push(EngineEvent::VideoReset);
        q.push(frame_update(&[Rect::new(1, 0, 1, 1)]));
        q.push(EngineEvent::DesktopResized(Size::new(10, 10)));
        q.push(EngineEvent::Pointer(PointerEvent::Hidden));
        q.push(frame_update(&[Rect::new(2, 0, 1, 1)]));
        let ev: Vec<_> = q.take().into_iter().collect();
        assert!(matches!(
            &ev[..],
            [
                EngineEvent::FrameUpdate { .. },
                EngineEvent::VideoReset,
                EngineEvent::FrameUpdate { .. },
                EngineEvent::DesktopResized(_),
                EngineEvent::Pointer(_),
                EngineEvent::FrameUpdate { .. },
            ]
        ));
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
