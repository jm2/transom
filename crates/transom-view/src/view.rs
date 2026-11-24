// SPDX-License-Identifier: Apache-2.0
//! The `TransomView` widget.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::time::Duration;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene};
use transom_view_core::config::SessionConfig;
use transom_view_core::engine::{
    AuthRequest, CertDecision, CertPrompt, Credentials, EngineCommand, EngineError, EngineEvent,
    EngineHandle, Finished, InputEvent, MouseButton, PointerEvent, RdpEngine, Rect, SharedFrame,
};
use transom_view_core::geometry::{
    self, DesktopRequest, FitMode, LayoutTracker, LogicalSize, Point, ScaleMode, Size, Viewport,
    WheelAccumulator,
};
use transom_view_core::keymap::{self, Scancode};
use transom_view_core::monitors::RemoteMonitor;
use transom_view_core::secrecy::SecretString;

use crate::tiles::TileCache;

/// Debounce for resize and scale changes before a new monitor layout is sent (§3.8).
const LAYOUT_DEBOUNCE: Duration = Duration::from_millis(150);
/// Logical size used for the initial request if the widget isn't allocated yet.
const FALLBACK_LOGICAL: LogicalSize = LogicalSize::new(1024.0, 768.0);
/// RDP wheel units per surface pixel of smooth (touchpad) scrolling.
const WHEEL_UNITS_PER_PIXEL: f64 = 120.0 / 12.0;

type AuthHandler = Box<dyn Fn(&TransomView, AuthRequest)>;
type CertHandler = Box<dyn Fn(&TransomView, CertPrompt)>;

/// Geometry snapshot used by both rendering and input mapping.
#[derive(Debug, Clone, Copy)]
struct Geometry {
    scale: f64,
    /// Logical offset that puts the image origin on a whole device pixel.
    snap: (f64, f64),
    viewport: Viewport,
    desktop: Size,
}

mod imp {
    use super::*;
    use std::sync::OnceLock;

    use glib::subclass::Signal;

    #[derive(Default)]
    #[allow(missing_debug_implementations)] // holds boxed closures
    pub struct TransomView {
        pub(super) inner: RefCell<Inner>,
        pub(super) auth_handler: RefCell<Option<AuthHandler>>,
        pub(super) clipboard: crate::clipboard::ClipboardBridge,
        pub(super) cert_handler: RefCell<Option<CertHandler>>,
        /// The part of the remote desktop this view shows (multi-monitor); `None`
        /// shows all of it.
        pub(super) region: Cell<Option<Rect>>,
        /// For a follower view: the view that owns the session.
        pub(super) leader: glib::WeakRef<super::TransomView>,
        /// For a leader: views showing other regions of the same session.
        pub(super) followers: RefCell<Vec<glib::WeakRef<super::TransomView>>>,
        /// Prompts waiting for `answer_certificate` / `answer_credentials` (when no
        /// Rust handler is set; used by the GObject signals / C API).
        pub(super) pending_cert: RefCell<Option<CertPrompt>>,
        pub(super) pending_auth: RefCell<Option<AuthRequest>>,
    }

    #[derive(Default)]
    pub(super) struct Inner {
        pub handle: Option<EngineHandle>,
        pub session: u64,
        pub scale_mode: ScaleMode,
        pub fit_mode: FitMode,
        pub frame: Option<SharedFrame>,
        pub desktop: Size,
        pub tiles: TileCache,
        /// Hardware-decoded video layers by GFX surface (ADR 0006), drawn over the
        /// tiles where their region says the video frame is current.
        pub video: std::collections::HashMap<u16, VideoLayer>,
        /// The renderer couldn't import a video frame; the engine was told to stop.
        pub video_failed: bool,
        pub resize_available: bool,
        pub layout: LayoutTracker,
        pub layout_timer: Option<glib::SourceId>,
        pub wheel: WheelAccumulator,
        pub hwheel: WheelAccumulator,
        pub pressed_keys: HashSet<Scancode>,
        pub pressed_buttons: HashSet<u32>,
        pub pointer: Point,
        pub scale_watch: Option<(gdk::Surface, glib::SignalHandlerId)>,
        /// Multi-monitor layout chosen by the application (overrides the
        /// widget-size-based single-monitor layout), and the last one sent.
        pub monitors: Option<Vec<RemoteMonitor>>,
        pub last_monitors: Option<Vec<RemoteMonitor>>,
    }

    #[derive(Debug)]
    pub(super) struct VideoLayer {
        pub texture: gdk::Texture,
        pub origin: (u32, u32),
        pub region: transom_view_core::video::Region,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TransomView {
        const NAME: &'static str = "TransomView";
        type Type = super::TransomView;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("transom-view");
            klass.set_accessible_role(gtk::AccessibleRole::Application);
        }
    }

    impl ObjectImpl for TransomView {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_focusable(true);
            obj.set_can_focus(true);
            obj.set_overflow(gtk::Overflow::Hidden);
            obj.update_property(&[gtk::accessible::Property::Label("Remote desktop")]);
            self.setup_input();
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![
                    Signal::builder("connected").build(),
                    Signal::builder("disconnected")
                        .param_types([String::static_type()])
                        .build(),
                    Signal::builder("desktop-resized")
                        .param_types([u32::static_type(), u32::static_type()])
                        .build(),
                    // (host, port, subject, issuer, sha256, previous sha256 or "").
                    // Answer with answer_certificate(); unanswered = rejected.
                    Signal::builder("certificate-prompt")
                        .param_types([
                            String::static_type(),
                            u32::static_type(),
                            String::static_type(),
                            String::static_type(),
                            String::static_type(),
                            String::static_type(),
                        ])
                        .build(),
                    // (suggested username or "", domain or ""). Answer with
                    // answer_credentials() or cancel_credentials().
                    Signal::builder("credentials-required")
                        .param_types([String::static_type(), String::static_type()])
                        .build(),
                    // (text): something the user should know about the session.
                    Signal::builder("notice")
                        .param_types([String::static_type()])
                        .build(),
                ]
            })
        }

        fn dispose(&self) {
            let mut inner = self.inner.borrow_mut();
            if let Some(id) = inner.layout_timer.take() {
                id.remove();
            }
            inner.handle = None;
        }
    }

    impl WidgetImpl for TransomView {
        fn measure(&self, _orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            (1, 1, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            self.schedule_layout();
        }

        fn realize(&self) {
            self.parent_realize();
            if let Some(surface) = self.obj().native().and_then(|n| n.surface()) {
                let weak = self.obj().downgrade();
                let id = surface.connect_scale_notify(move |_| {
                    if let Some(obj) = weak.upgrade() {
                        obj.imp().on_scale_changed();
                    }
                });
                self.inner.borrow_mut().scale_watch = Some((surface, id));
            }
        }

        fn unrealize(&self) {
            if let Some((surface, id)) = self.inner.borrow_mut().scale_watch.take() {
                surface.disconnect(id);
            }
            self.parent_unrealize();
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let bounds = graphene::Rect::new(0.0, 0.0, obj.width() as f32, obj.height() as f32);
            snapshot.append_color(&gdk::RGBA::BLACK, &bounds);

            // A follower draws its region from the leader's frame and tiles.
            let src = self.source();
            let mut inner = src.imp().inner.borrow_mut();
            let Some(frame) = inner.frame.clone() else {
                return;
            };
            let full = {
                let f = frame.lock();
                inner.tiles.refresh(&f);
                f.size
            };
            let region = self.region.get().and_then(|r| r.clip(full));
            let (desktop, origin) = match region {
                Some(r) => (Size::new(r.width, r.height), (r.x as f64, r.y as f64)),
                None => (full, (0.0, 0.0)),
            };
            let fit = inner.fit_mode;
            let g = self.geometry(desktop, fit);
            let exact = g.viewport.is_pixel_exact();
            let vp = g.viewport;
            snapshot.push_clip(&bounds);
            // Draw in device pixels: every tile edge is then an exact integer, so
            // fractional scales can't introduce seams (tile edges at e.g. 204.8
            // logical px are antialiased against their neighbours).
            snapshot.save();
            snapshot.translate(&graphene::Point::new(g.snap.0 as f32, g.snap.1 as f32));
            snapshot.scale((1.0 / g.scale) as f32, (1.0 / g.scale) as f32);
            for (r, tex) in inner.tiles.iter() {
                if let Some(reg) = region
                    && reg.intersect(r).is_none()
                {
                    continue;
                }
                let (rx, ry) = (r.x as f64 - origin.0, r.y as f64 - origin.1);
                // Round both edges to whole device pixels: neighbouring tiles then
                // share exact edges, so nothing is antialiased into a seam. (At
                // ratio 1 these are already integers.)
                let x0 = (vp.offset_x + rx * vp.ratio).round();
                let y0 = (vp.offset_y + ry * vp.ratio).round();
                let x1 = (vp.offset_x + (rx + r.width as f64) * vp.ratio).round();
                let y1 = (vp.offset_y + (ry + r.height as f64) * vp.ratio).round();
                let rect =
                    graphene::Rect::new(x0 as f32, y0 as f32, (x1 - x0) as f32, (y1 - y0) as f32);
                if exact {
                    // 1 texel = 1 device pixel on the device grid: a plain texture
                    // node is sampled 1:1. (A texture-scale node may be drawn via a
                    // logical-size offscreen and come out blurred at scale > 1.)
                    snapshot.append_texture(tex, &rect);
                } else {
                    snapshot.append_scaled_texture(tex, gtk::gsk::ScalingFilter::Linear, &rect);
                }
            }
            // Video layers: the whole frame, clipped to each rectangle of its region,
            // with the same device-pixel rounding as the tiles underneath.
            let dev = |x: f64, y: f64| {
                (
                    (vp.offset_x + (x - origin.0) * vp.ratio).round(),
                    (vp.offset_y + (y - origin.1) * vp.ratio).round(),
                )
            };
            for layer in inner.video.values() {
                let (ox, oy) = (layer.origin.0 as f64, layer.origin.1 as f64);
                let (fx0, fy0) = dev(ox, oy);
                let (fx1, fy1) = dev(
                    ox + layer.texture.width() as f64,
                    oy + layer.texture.height() as f64,
                );
                let frect = graphene::Rect::new(
                    fx0 as f32,
                    fy0 as f32,
                    (fx1 - fx0) as f32,
                    (fy1 - fy0) as f32,
                );
                for r in layer.region.rects() {
                    // The origin comes from the server.
                    let r = Rect::new(
                        r.x.saturating_add(layer.origin.0),
                        r.y.saturating_add(layer.origin.1),
                        r.width,
                        r.height,
                    );
                    let r = match region {
                        Some(reg) => match reg.intersect(r) {
                            Some(r) => r,
                            None => continue,
                        },
                        None => r,
                    };
                    let (x0, y0) = dev(r.x as f64, r.y as f64);
                    let (x1, y1) = dev((r.x + r.width) as f64, (r.y + r.height) as f64);
                    snapshot.push_clip(&graphene::Rect::new(
                        x0 as f32,
                        y0 as f32,
                        (x1 - x0) as f32,
                        (y1 - y0) as f32,
                    ));
                    if exact {
                        snapshot.append_texture(&layer.texture, &frect);
                    } else {
                        snapshot.append_scaled_texture(
                            &layer.texture,
                            gtk::gsk::ScalingFilter::Linear,
                            &frect,
                        );
                    }
                    snapshot.pop();
                }
            }
            snapshot.restore();
            snapshot.pop();
        }
    }

    impl TransomView {
        /// Current surface scale (fractional where supported).
        pub(super) fn scale(&self) -> f64 {
            let obj = self.obj();
            obj.native()
                .and_then(|n| n.surface())
                .map(|s| s.scale())
                .filter(|s| s.is_finite() && *s > 0.0)
                .unwrap_or(obj.scale_factor() as f64)
        }

        /// Logical size usable for whole device pixels: the allocation minus the
        /// origin snap.
        fn logical_size(&self) -> LogicalSize {
            let obj = self.obj();
            if obj.width() > 0 && obj.height() > 0 {
                let snap = self.origin_snap(self.scale());
                LogicalSize::new(obj.width() as f64 - snap.0, obj.height() as f64 - snap.1)
            } else {
                FALLBACK_LOGICAL
            }
        }

        pub(super) fn desktop_request(&self, mode: ScaleMode) -> DesktopRequest {
            geometry::desktop_request(mode, self.logical_size(), self.scale())
        }

        /// Logical shift that moves our origin onto the device-pixel grid (§3.4).
        fn origin_snap(&self, s: f64) -> (f64, f64) {
            let obj = self.obj();
            let Some(native) = obj.native() else {
                return (0.0, 0.0);
            };
            let (sx, sy) = native.surface_transform();
            let p = obj
                .compute_point(&native, &graphene::Point::new(0.0, 0.0))
                .unwrap_or(graphene::Point::new(0.0, 0.0));
            let shift = |v: f64| {
                let d = v * s;
                let frac = d - d.floor();
                if !(1e-6..=1.0 - 1e-6).contains(&frac) {
                    0.0
                } else {
                    (1.0 - frac) / s
                }
            };
            (shift(p.x() as f64 + sx), shift(p.y() as f64 + sy))
        }

        fn geometry(&self, desktop: Size, fit: FitMode) -> Geometry {
            let obj = self.obj();
            let scale = self.scale();
            let snap = self.origin_snap(scale);
            let dev = Size::new(
                ((obj.width() as f64 - snap.0) * scale + 1e-3)
                    .floor()
                    .max(0.0) as u32,
                ((obj.height() as f64 - snap.1) * scale + 1e-3)
                    .floor()
                    .max(0.0) as u32,
            );
            Geometry {
                scale,
                snap,
                viewport: geometry::viewport(desktop, dev, fit, (0.0, 0.0)),
                desktop,
            }
        }

        /// The view that owns the session: the leader for a follower, else self.
        pub(super) fn source(&self) -> super::TransomView {
            self.leader.upgrade().unwrap_or_else(|| self.obj().clone())
        }

        /// This view's region of a desktop of `full` size: `(origin, size)`.
        fn region_of(&self, full: Size) -> ((u32, u32), Size) {
            match self.region.get().and_then(|r| r.clip(full)) {
                Some(r) => ((r.x, r.y), Size::new(r.width, r.height)),
                None => ((0, 0), full),
            }
        }

        fn current_geometry(&self) -> Option<Geometry> {
            let src = self.source();
            let inner = src.imp().inner.borrow();
            inner.frame.as_ref()?;
            let (full, fit) = (inner.desktop, inner.fit_mode);
            drop(inner);
            let (_, desktop) = self.region_of(full);
            Some(self.geometry(desktop, fit))
        }

        fn to_remote(&self, x: f64, y: f64) -> Option<(Point, bool)> {
            let g = self.current_geometry()?;
            let full = self.source().imp().inner.borrow().desktop;
            let (origin, _) = self.region_of(full);
            let (p, inside) =
                geometry::map_input(x - g.snap.0, y - g.snap.1, g.scale, &g.viewport, g.desktop);
            Some((
                Point {
                    x: p.x + origin.0,
                    y: p.y + origin.1,
                },
                inside,
            ))
        }

        /// Apply the video layer changes of a frame update (ADR 0006). A frame this
        /// display can't import turns hardware decoding off for the session: the
        /// engine copies the video pixels back and decodes in software from then on.
        fn update_video(&self, updates: Vec<transom_view_core::video::VideoUpdate>) {
            if updates.is_empty() {
                return;
            }
            let display = self.obj().display();
            // Only GTK's Vulkan renderer imports YCbCr dmabufs with the tiled layouts
            // VA-API decodes into. With the GL renderer the texture is created anyway
            // (the display advertises the Vulkan formats) and then drawn from a linear
            // read of the tiled buffer: garbage. So anything else uses software.
            let renderer_ok = self
                .obj()
                .native()
                .and_then(|n| n.renderer())
                .is_some_and(|r| r.type_().name().contains("Vulkan"));
            let mut failed = false;
            if !renderer_ok && !self.inner.borrow().video_failed {
                tracing::info!("renderer can't show hardware-decoded video; software decoding");
                failed = true;
            }
            {
                let mut inner = self.inner.borrow_mut();
                for u in updates {
                    if failed || u.region.is_empty() || inner.video_failed {
                        inner.video.remove(&u.surface);
                        continue;
                    }
                    #[cfg(target_os = "linux")]
                    let texture = match u.frame {
                        Some(frame) => match crate::dmabuf::texture(&display, frame) {
                            Ok(t) => Some(t),
                            Err(e) => {
                                tracing::warn!(
                                    "can't show hardware-decoded video ({e}); software decoding"
                                );
                                failed = true;
                                break;
                            }
                        },
                        None => None,
                    };
                    // No dmabufs elsewhere (the engine never sends frames there).
                    #[cfg(not(target_os = "linux"))]
                    let texture: Option<gdk::Texture> = {
                        let _ = (&display, u.frame);
                        None
                    };
                    match (inner.video.get_mut(&u.surface), texture) {
                        (Some(l), t) => {
                            if let Some(t) = t {
                                l.texture = t;
                            }
                            l.origin = u.origin;
                            l.region = u.region;
                        }
                        (None, Some(texture)) => {
                            inner.video.insert(
                                u.surface,
                                VideoLayer {
                                    texture,
                                    origin: u.origin,
                                    region: u.region,
                                },
                            );
                        }
                        (None, None) => {}
                    }
                }
                if failed {
                    inner.video_failed = true;
                    inner.video.clear();
                }
            }
            if failed {
                self.send(EngineCommand::VideoUnsupported);
            }
        }

        pub(super) fn send(&self, cmd: EngineCommand) {
            if let Some(leader) = self.leader.upgrade() {
                leader.imp().send(cmd);
            } else if let Some(h) = &self.inner.borrow().handle {
                h.send(cmd);
            }
        }

        fn connected(&self) -> bool {
            let src = self.source();
            let inner = src.imp().inner.borrow();
            inner.handle.is_some() && inner.frame.is_some()
        }

        /// Run `f` on every live follower.
        fn each_follower(&self, f: impl Fn(&super::TransomView)) {
            self.followers
                .borrow_mut()
                .retain(|w| w.upgrade().is_some());
            for w in self.followers.borrow().iter() {
                if let Some(v) = w.upgrade() {
                    f(&v);
                }
            }
        }

        // ---- resize / scale ----------------------------------------------------

        fn on_scale_changed(&self) {
            tracing::debug!(scale = self.scale(), "surface scale changed");
            self.obj().queue_draw();
            self.schedule_layout();
        }

        pub(super) fn schedule_layout(&self) {
            let mut inner = self.inner.borrow_mut();
            if let Some(id) = inner.layout_timer.take() {
                id.remove();
            }
            if inner.handle.is_none() || !inner.resize_available {
                return;
            }
            let weak = self.obj().downgrade();
            inner.layout_timer = Some(glib::timeout_add_local_once(LAYOUT_DEBOUNCE, move || {
                if let Some(obj) = weak.upgrade() {
                    obj.imp().inner.borrow_mut().layout_timer = None;
                    obj.imp().send_layout();
                }
            }));
        }

        fn send_layout(&self) {
            // Multi-monitor: the application's layout, not this widget's size.
            let monitors = self.inner.borrow().monitors.clone();
            if let Some(m) = monitors {
                let mut inner = self.inner.borrow_mut();
                if inner.last_monitors.as_ref() != Some(&m) {
                    inner.last_monitors = Some(m.clone());
                    drop(inner);
                    tracing::debug!(?m, "requesting multi-monitor layout");
                    self.send(EngineCommand::LayoutMonitors(m));
                }
                return;
            }
            let mode = self.inner.borrow().scale_mode;
            let req = self.desktop_request(mode);
            let changed = self.inner.borrow_mut().layout.update(req);
            if let Some(req) = changed {
                tracing::debug!(?req, "requesting layout");
                self.send(EngineCommand::Layout(req));
            }
        }

        // ---- engine events -----------------------------------------------------

        pub(super) fn handle_event(&self, ev: EngineEvent) {
            let obj = self.obj();
            match ev {
                EngineEvent::Connected { desktop, frame, .. } => {
                    {
                        let mut inner = self.inner.borrow_mut();
                        inner.frame = Some(frame);
                        inner.desktop = desktop;
                        inner.tiles.clear();
                        inner.video.clear();
                        inner.video_failed = false;
                    }
                    obj.queue_draw();
                    self.each_follower(|v| v.queue_draw());
                    obj.emit_by_name::<()>("connected", &[]);
                }
                EngineEvent::FrameUpdate { damage, video } => {
                    self.inner.borrow_mut().tiles.mark(&damage);
                    self.update_video(video);
                    obj.queue_draw();
                    self.each_follower(|v| v.queue_draw());
                }
                EngineEvent::DesktopResized(size) => {
                    self.inner.borrow_mut().desktop = size;
                    obj.queue_draw();
                    self.each_follower(|v| v.queue_draw());
                    obj.emit_by_name::<()>("desktop-resized", &[&size.width, &size.height]);
                }
                EngineEvent::Pointer(p) => {
                    self.each_follower(|v| v.imp().set_remote_cursor(p.clone()));
                    self.set_remote_cursor(p);
                }
                EngineEvent::PointerPosition { .. } => {}
                EngineEvent::AuthRequired(req) => {
                    if let Some(h) = self.auth_handler.borrow().as_ref() {
                        h(&obj, req);
                        return;
                    }
                    // No Rust handler: ask through the signal and keep the request
                    // until answer_credentials()/cancel_credentials().
                    let u = req.username.clone().unwrap_or_default();
                    let d = req.domain.clone().unwrap_or_default();
                    *self.pending_auth.borrow_mut() = Some(req);
                    obj.emit_by_name::<()>("credentials-required", &[&u, &d]);
                }
                EngineEvent::CertificatePrompt(p) => {
                    if let Some(h) = self.cert_handler.borrow().as_ref() {
                        h(&obj, p);
                        return;
                    }
                    let i = &p.info;
                    let (host, port, subject, issuer, fp, prev) = (
                        i.host.clone(),
                        i.port as u32,
                        i.subject.clone(),
                        i.issuer.clone(),
                        i.fingerprint_sha256.clone(),
                        p.previous_fingerprint.clone().unwrap_or_default(),
                    );
                    *self.pending_cert.borrow_mut() = Some(p);
                    obj.emit_by_name::<()>(
                        "certificate-prompt",
                        &[&host, &port, &subject, &issuer, &fp, &prev],
                    );
                }
                EngineEvent::Notice(text) => {
                    self.obj().emit_by_name::<()>("notice", &[&text]);
                }
                EngineEvent::Redirecting { target } => {
                    tracing::info!(%target, "server redirection");
                    self.inner.borrow_mut().layout.reset();
                }
                EngineEvent::ResizeAvailable => {
                    let mut inner = self.inner.borrow_mut();
                    inner.resize_available = true;
                    // A new Display Control channel (first connect or after a
                    // redirection) knows nothing of our layout: send it again.
                    inner.layout.reset();
                    inner.last_monitors = None;
                    drop(inner);
                    self.schedule_layout();
                }
                EngineEvent::ClipboardRemoteFormats(f) => self.clipboard.remote_formats(&obj, f),
                EngineEvent::ClipboardRemoteData { format, data } => {
                    self.clipboard.remote_data(&obj, format, data)
                }
                EngineEvent::ClipboardDataRequest(f) => self.clipboard.data_requested(&obj, f),
                EngineEvent::ClipboardFileRequest {
                    stream_id,
                    index,
                    range,
                } => self.clipboard.file_requested(&obj, stream_id, index, range),
                EngineEvent::ClipboardFileData { stream_id, data } => {
                    self.clipboard.file_data(stream_id, data)
                }
                EngineEvent::Disconnected(reason) => {
                    self.reset_session();
                    obj.set_cursor(None);
                    obj.queue_draw();
                    self.each_follower(|v| {
                        v.set_cursor(None);
                        v.queue_draw();
                    });
                    obj.emit_by_name::<()>("disconnected", &[&reason.to_string()]);
                }
            }
        }

        pub(super) fn reset_session(&self) {
            self.clipboard.stop();
            self.pending_cert.borrow_mut().take();
            self.pending_auth.borrow_mut().take();
            let mut inner = self.inner.borrow_mut();
            if let Some(id) = inner.layout_timer.take() {
                id.remove();
            }
            inner.handle = None;
            inner.frame = None;
            inner.tiles.clear();
            inner.video.clear();
            inner.video_failed = false;
            inner.resize_available = false;
            inner.layout.reset();
            inner.pressed_keys.clear();
            inner.pressed_buttons.clear();
            inner.wheel.reset();
            inner.hwheel.reset();
        }

        fn set_remote_cursor(&self, p: PointerEvent) {
            let obj = self.obj();
            match p {
                PointerEvent::Image(img) if img.size.width > 0 && img.size.height > 0 => {
                    let Some(g) = self.current_geometry() else {
                        return;
                    };
                    let (w, h) = (img.size.width as i32, img.size.height as i32);
                    let bytes = glib::Bytes::from(&img.bgra[..]);
                    let tex: gdk::Texture = gdk::MemoryTexture::new(
                        w,
                        h,
                        gdk::MemoryFormat::B8g8r8a8,
                        &bytes,
                        w as usize * 4,
                    )
                    .upcast();
                    // Remote pixels -> logical units (§3.7).
                    let (lw, lh, hx, hy) =
                        geometry::cursor_logical(img.size, img.hotspot, g.viewport.ratio, g.scale);
                    let fallback = gdk::Cursor::from_name("default", None);
                    let cursor = gdk::Cursor::from_callback(
                        move |_, _size, _scale, out_w, out_h, out_hx, out_hy| {
                            *out_w = lw;
                            *out_h = lh;
                            *out_hx = hx;
                            *out_hy = hy;
                            tex.clone()
                        },
                        fallback.as_ref(),
                    );
                    obj.set_cursor(cursor.as_ref());
                }
                PointerEvent::Image(_) | PointerEvent::Hidden => {
                    obj.set_cursor_from_name(Some("none"))
                }
                PointerEvent::Default => obj.set_cursor_from_name(Some("default")),
            }
        }

        // ---- input -------------------------------------------------------------

        fn setup_input(&self) {
            let obj = self.obj();

            let motion = gtk::EventControllerMotion::new();
            let weak = obj.downgrade();
            motion.connect_motion(move |_, x, y| {
                if let Some(obj) = weak.upgrade() {
                    obj.imp().on_motion(x, y);
                }
            });
            obj.add_controller(motion);

            // Legacy controller: GestureClick tracks one button at a time, but RDP
            // needs independent press/release for every button.
            let buttons = gtk::EventControllerLegacy::new();
            let weak = obj.downgrade();
            buttons.connect_event(move |_, ev| match weak.upgrade() {
                Some(obj) => obj.imp().on_legacy_event(ev),
                None => glib::Propagation::Proceed,
            });
            obj.add_controller(buttons);

            let scroll =
                gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
            let weak = obj.downgrade();
            scroll.connect_scroll(move |c, dx, dy| match weak.upgrade() {
                Some(obj) => obj.imp().on_scroll(c.unit(), dx, dy),
                None => glib::Propagation::Proceed,
            });
            obj.add_controller(scroll);

            let keys = gtk::EventControllerKey::new();
            let weak = obj.downgrade();
            keys.connect_key_pressed(move |_, keyval, keycode, _| match weak.upgrade() {
                Some(obj) => obj.imp().on_key(keyval, keycode, true),
                None => glib::Propagation::Proceed,
            });
            let weak = obj.downgrade();
            keys.connect_key_released(move |_, keyval, keycode, _| {
                if let Some(obj) = weak.upgrade() {
                    obj.imp().on_key(keyval, keycode, false);
                }
            });
            obj.add_controller(keys);

            let focus = gtk::EventControllerFocus::new();
            let weak = obj.downgrade();
            focus.connect_enter(move |_| {
                if let Some(obj) = weak.upgrade() {
                    obj.imp().on_focus_in();
                }
            });
            let weak = obj.downgrade();
            focus.connect_leave(move |_| {
                if let Some(obj) = weak.upgrade() {
                    obj.imp().release_all();
                }
            });
            obj.add_controller(focus);
        }

        fn on_motion(&self, x: f64, y: f64) {
            if !self.connected() {
                return;
            }
            if let Some((p, _)) = self.to_remote(x, y) {
                let moved = {
                    let mut inner = self.inner.borrow_mut();
                    let moved = inner.pointer != p;
                    inner.pointer = p;
                    moved
                };
                if moved {
                    self.send(EngineCommand::Input(InputEvent::MouseMove {
                        x: p.x,
                        y: p.y,
                    }));
                }
            }
        }

        /// Event position (surface coordinates) → widget coordinates.
        fn event_position(&self, ev: &gdk::Event) -> Option<(f64, f64)> {
            let obj = self.obj();
            let (x, y) = ev.position()?;
            let native = obj.native()?;
            let (sx, sy) = native.surface_transform();
            let p = native.compute_point(
                &*obj,
                &graphene::Point::new((x - sx) as f32, (y - sy) as f32),
            )?;
            Some((p.x() as f64, p.y() as f64))
        }

        fn on_legacy_event(&self, ev: &gdk::Event) -> glib::Propagation {
            let down = match ev.event_type() {
                gdk::EventType::ButtonPress => true,
                gdk::EventType::ButtonRelease => false,
                _ => return glib::Propagation::Proceed,
            };
            let Some(bev) = ev.downcast_ref::<gdk::ButtonEvent>() else {
                return glib::Propagation::Proceed;
            };
            if down {
                self.obj().grab_focus();
            }
            if !self.connected() {
                return glib::Propagation::Proceed;
            }
            let button = match bev.button() {
                1 => MouseButton::Left,
                2 => MouseButton::Middle,
                3 => MouseButton::Right,
                8 => MouseButton::Back,
                9 => MouseButton::Forward,
                _ => return glib::Propagation::Stop,
            };
            {
                let mut inner = self.inner.borrow_mut();
                if down {
                    inner.pressed_buttons.insert(bev.button());
                } else if !inner.pressed_buttons.remove(&bev.button()) {
                    // Release without a press we forwarded (e.g. focus click).
                    return glib::Propagation::Stop;
                }
            }
            let p = self
                .event_position(ev)
                .and_then(|(x, y)| self.to_remote(x, y))
                .map(|(p, _)| p)
                .unwrap_or(self.inner.borrow().pointer);
            self.send(EngineCommand::Input(InputEvent::MouseButton {
                button,
                down,
                x: p.x,
                y: p.y,
            }));
            glib::Propagation::Stop
        }

        fn on_scroll(&self, unit: gdk::ScrollUnit, dx: f64, dy: f64) -> glib::Propagation {
            if !self.connected() {
                return glib::Propagation::Proceed;
            }
            let k = match unit {
                gdk::ScrollUnit::Surface => WHEEL_UNITS_PER_PIXEL,
                _ => geometry::WHEEL_NOTCH as f64,
            };
            let (v, hz, p) = {
                let mut inner = self.inner.borrow_mut();
                // GTK: +dy = down; RDP: + = away from the user. Horizontal: both + = right.
                let v = inner.wheel.push(-dy * k);
                let hz = inner.hwheel.push(dx * k);
                (v, hz, inner.pointer)
            };
            for delta in v {
                self.send(EngineCommand::Input(InputEvent::Wheel {
                    delta,
                    x: p.x,
                    y: p.y,
                }));
            }
            for delta in hz {
                self.send(EngineCommand::Input(InputEvent::HWheel {
                    delta,
                    x: p.x,
                    y: p.y,
                }));
            }
            glib::Propagation::Stop
        }

        fn on_key(&self, keyval: gdk::Key, keycode: u32, down: bool) -> glib::Propagation {
            if !self.connected() {
                return glib::Propagation::Proceed;
            }
            if let Some(sc) = keymap::from_hardware_keycode(keycode) {
                {
                    let mut inner = self.inner.borrow_mut();
                    if down {
                        inner.pressed_keys.insert(sc);
                    } else if !inner.pressed_keys.remove(&sc) {
                        return glib::Propagation::Stop;
                    }
                }
                self.send(EngineCommand::Input(InputEvent::Key {
                    scancode: sc.code,
                    extended: sc.extended,
                    down,
                }));
            } else if down && let Some(ch) = keyval.to_unicode() {
                let mut buf = [0u16; 2];
                for &unit in ch.encode_utf16(&mut buf).iter() {
                    for d in [true, false] {
                        self.send(EngineCommand::Input(InputEvent::Unicode {
                            code_unit: unit,
                            down: d,
                        }));
                    }
                }
            }
            glib::Propagation::Stop
        }

        fn on_focus_in(&self) {
            if !self.connected() {
                return;
            }
            let Some(kbd) = self
                .obj()
                .display()
                .default_seat()
                .and_then(|s| s.keyboard())
            else {
                return;
            };
            self.send(EngineCommand::Input(InputEvent::SyncLocks {
                caps: kbd.is_caps_locked(),
                num: kbd.is_num_locked(),
                scroll: kbd.is_scroll_locked(),
                kana: false,
            }));
        }

        /// Release everything we think is held, so nothing sticks on the remote
        /// side when focus leaves.
        pub(super) fn release_all(&self) {
            let (keys, buttons, p) = {
                let mut inner = self.inner.borrow_mut();
                (
                    std::mem::take(&mut inner.pressed_keys),
                    std::mem::take(&mut inner.pressed_buttons),
                    inner.pointer,
                )
            };
            for sc in keys {
                self.send(EngineCommand::Input(InputEvent::Key {
                    scancode: sc.code,
                    extended: sc.extended,
                    down: false,
                }));
            }
            for b in buttons {
                let button = match b {
                    1 => MouseButton::Left,
                    2 => MouseButton::Middle,
                    3 => MouseButton::Right,
                    8 => MouseButton::Back,
                    _ => MouseButton::Forward,
                };
                self.send(EngineCommand::Input(InputEvent::MouseButton {
                    button,
                    down: false,
                    x: p.x,
                    y: p.y,
                }));
            }
        }
    }
}

glib::wrapper! {
    /// A GTK4 widget showing one RDP session.
    ///
    /// Signals: `connected`, `disconnected(reason: String)`,
    /// `desktop-resized(width: u32, height: u32)`.
    pub struct TransomView(ObjectSubclass<imp::TransomView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for TransomView {
    fn default() -> Self {
        Self::new()
    }
}

impl TransomView {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// Start a session. `config.initial` is replaced with a request computed from
    /// the widget's current size and scale (§3.1–3.3). Any existing session is closed.
    pub fn connect_session(
        &self,
        engine: &dyn RdpEngine,
        mut config: SessionConfig,
    ) -> Result<(), EngineError> {
        self.disconnect();
        let imp = self.imp();
        config.initial = imp.desktop_request(config.scale_mode);
        let initial = config.initial;
        let (scale_mode, fit_mode, clip_mode) =
            (config.scale_mode, config.fit_mode, config.clipboard);

        let monitors = (config.monitors.len() > 1).then(|| config.monitors.clone());
        let (tx, rx) = async_channel::unbounded();
        let handle = engine.connect(
            config,
            Box::new(move |ev| {
                let _ = tx.send_blocking(ev);
            }),
        )?;

        let session = {
            let mut inner = imp.inner.borrow_mut();
            inner.session += 1;
            inner.handle = Some(handle);
            inner.scale_mode = scale_mode;
            inner.fit_mode = fit_mode;
            inner.layout.reset();
            inner.layout.update(initial);
            inner.last_monitors = monitors.clone();
            inner.monitors = monitors;
            inner.session
        };

        imp.clipboard.start(self, clip_mode);

        let weak = self.downgrade();
        glib::spawn_future_local(async move {
            while let Ok(ev) = rx.recv().await {
                let Some(view) = weak.upgrade() else { break };
                if view.imp().inner.borrow().session != session {
                    break;
                }
                view.imp().handle_event(ev);
            }
        });
        Ok(())
    }

    /// How much of the desktop (remote pixels) is currently shown from
    /// hardware-decoded video layers (ADR 0006); 0 when everything comes from the
    /// framebuffer. For diagnostics and tests.
    pub fn video_area(&self) -> u64 {
        self.imp()
            .source()
            .imp()
            .inner
            .borrow()
            .video
            .values()
            .map(|l| l.region.area())
            .sum()
    }

    /// Close the session, if any. `disconnected` is *not* emitted for a local
    /// disconnect started here.
    pub fn disconnect(&self) {
        let imp = self.imp();
        imp.release_all();
        imp.inner.borrow_mut().session += 1;
        imp.reset_session();
        self.set_cursor(None);
        self.queue_draw();
    }

    /// Disconnect without blocking. Returns a [`Finished`] that becomes set when the
    /// backend has fully shut down; wait on it before the process exits.
    pub fn close(&self) -> Option<Finished> {
        let imp = self.imp();
        imp.release_all();
        let handle = {
            let mut inner = imp.inner.borrow_mut();
            inner.session += 1;
            inner.handle.take()
        };
        imp.reset_session();
        self.set_cursor(None);
        self.queue_draw();
        handle.map(|h| h.finished())
    }

    /// Disconnect and wait up to `timeout` for the backend to shut down cleanly.
    /// Applications should call this before quitting. Returns whether it finished.
    pub fn shutdown(&self, timeout: Duration) -> bool {
        let imp = self.imp();
        imp.release_all();
        let handle = {
            let mut inner = imp.inner.borrow_mut();
            inner.session += 1;
            inner.handle.take()
        };
        imp.reset_session();
        self.set_cursor(None);
        self.queue_draw();
        handle.is_none_or(|h| h.shutdown(timeout))
    }

    pub fn is_connected(&self) -> bool {
        let inner = self.imp().inner.borrow();
        inner.handle.is_some() && inner.frame.is_some()
    }

    /// Remote desktop size in remote pixels, once connected.
    pub fn desktop_size(&self) -> Option<(u32, u32)> {
        let inner = self.imp().inner.borrow();
        inner
            .frame
            .as_ref()
            .map(|_| (inner.desktop.width, inner.desktop.height))
    }

    /// The shared remote framebuffer, once connected (for screenshots and tests).
    pub fn frame(&self) -> Option<SharedFrame> {
        self.imp().source().imp().inner.borrow().frame.clone()
    }

    /// A view that shows another region of `leader`'s session (one per monitor in
    /// multi-monitor fullscreen). It has no session of its own; input goes to the
    /// leader's session and it draws from the leader's frame.
    pub fn new_follower(leader: &TransomView) -> Self {
        let v = Self::new();
        v.imp().leader.set(Some(leader));
        leader.imp().followers.borrow_mut().push(v.downgrade());
        v
    }

    /// Show only `(x, y, width, height)` of the remote desktop (remote pixels), or
    /// all of it with `None`.
    pub fn set_region(&self, region: Option<(u32, u32, u32, u32)>) {
        self.imp()
            .region
            .set(region.map(|(x, y, w, h)| Rect::new(x, y, w, h)));
        self.queue_draw();
    }

    /// Multi-monitor: request this layout from the server (instead of one monitor
    /// sized to this widget). `None` returns to single-monitor behaviour.
    pub fn set_monitor_layout(&self, monitors: Option<Vec<RemoteMonitor>>) {
        let imp = self.imp();
        {
            let mut inner = imp.inner.borrow_mut();
            inner.monitors = monitors.filter(|m| m.len() > 1);
            // Whatever we sent before no longer describes the server's layout.
            inner.last_monitors = None;
            inner.layout.reset();
        }
        imp.schedule_layout();
    }

    /// Drop all cached tile textures; they're rebuilt from the framebuffer on the
    /// next frame.
    pub fn invalidate(&self) {
        self.imp().inner.borrow_mut().tiles.clear();
        self.queue_draw();
    }

    /// Queue a raw engine command (crate-internal: clipboard bridge).
    pub(crate) fn send_command(&self, cmd: EngineCommand) {
        self.imp().send(cmd);
    }

    pub fn send_ctrl_alt_del(&self) {
        self.imp().send(EngineCommand::CtrlAltDel);
    }

    /// Answer the pending `certificate-prompt`.
    pub fn answer_certificate(&self, decision: CertDecision) {
        if let Some(p) = self.imp().pending_cert.borrow_mut().take() {
            let _ = p.reply.send(decision);
        }
    }

    /// Answer the pending `credentials-required` prompt.
    pub fn answer_credentials(&self, username: &str, domain: Option<&str>, password: SecretString) {
        if let Some(r) = self.imp().pending_auth.borrow_mut().take() {
            let _ = r.reply.send(Some(Credentials {
                username: username.to_string(),
                domain: domain.filter(|d| !d.is_empty()).map(str::to_string),
                password,
            }));
        }
    }

    /// Cancel the pending `credentials-required` prompt (aborts the connection).
    pub fn cancel_credentials(&self) {
        self.imp().pending_auth.borrow_mut().take();
    }

    /// Called when the server needs (new) credentials. Reply on the request, or
    /// drop it to cancel. Without a handler, the `credentials-required` signal is
    /// emitted instead (see [`TransomView::answer_credentials`]).
    pub fn set_auth_handler(&self, f: impl Fn(&TransomView, AuthRequest) + 'static) {
        *self.imp().auth_handler.borrow_mut() = Some(Box::new(f));
    }

    /// Called when the server certificate needs a decision. Reply on the prompt,
    /// or drop it to reject. Without a handler, certificates are rejected.
    pub fn set_cert_handler(&self, f: impl Fn(&TransomView, CertPrompt) + 'static) {
        *self.imp().cert_handler.borrow_mut() = Some(Box::new(f));
    }

    pub fn connect_connected<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_closure(
            "connected",
            false,
            glib::closure_local!(move |v: &TransomView| f(v)),
        )
    }

    /// Something the user should know about the session (plain text).
    pub fn connect_notice<F: Fn(&Self, &str) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_closure(
            "notice",
            false,
            glib::closure_local!(move |v: &TransomView, text: String| f(v, &text)),
        )
    }

    pub fn connect_disconnected<F: Fn(&Self, &str) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "disconnected",
            false,
            glib::closure_local!(move |v: &TransomView, reason: String| f(v, &reason)),
        )
    }

    pub fn connect_desktop_resized<F: Fn(&Self, u32, u32) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "desktop-resized",
            false,
            glib::closure_local!(move |v: &TransomView, w: u32, h: u32| f(v, w, h)),
        )
    }
}
