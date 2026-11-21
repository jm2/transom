// SPDX-License-Identifier: GPL-3.0-or-later
//! HiDPI geometry: desktop-size negotiation, scale factors, fit/viewport math and
//! input-coordinate mapping (PLAN.md §3 items 1, 2, 5, 6).
//!
//! Units:
//! - **logical**: GTK widget coordinates (`f64`), what `allocation()` and event
//!   coordinates use.
//! - **device**: physical screen pixels = logical × surface scale `s`.
//! - **remote**: pixels of the remote desktop framebuffer.
//!
//! Everything here is pure and platform-independent.

/// Minimum monitor dimension accepted by MS-RDPEDISP (2.2.2.2.1).
pub const MIN_DESKTOP_DIM: u32 = 200;
/// Maximum monitor dimension accepted by MS-RDPEDISP (2.2.2.2.1).
pub const MAX_DESKTOP_DIM: u32 = 8192;
/// Range of `DesktopScaleFactor` accepted by MS-RDPBCGR / MS-RDPEDISP.
pub const MIN_DESKTOP_SCALE: u32 = 100;
pub const MAX_DESKTOP_SCALE: u32 = 500;
/// `DeviceScaleFactor` values Windows honours; any other value makes it ignore the
/// desktop factor.
pub const DEVICE_SCALES: [u32; 3] = [100, 140, 180];
/// One wheel notch in RDP wheel units.
pub const WHEEL_NOTCH: i32 = 120;

/// An integer size in device or remote pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Size {
    pub width: u32,
    pub height: u32,
}

impl Size {
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }
}

/// A size in logical (GTK) units.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LogicalSize {
    pub width: f64,
    pub height: f64,
}

impl LogicalSize {
    pub const fn new(width: f64, height: f64) -> Self {
        Self { width, height }
    }
}

/// A point in remote-desktop pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Point {
    pub x: u32,
    pub y: u32,
}

/// Scale factors sent to the server at connect time and in every monitor layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScaleFactors {
    /// `DesktopScaleFactor`, percent, 100–500.
    pub desktop: u32,
    /// `DeviceScaleFactor`, percent, one of [`DEVICE_SCALES`].
    pub device: u32,
}

impl ScaleFactors {
    pub const UNSCALED: Self = Self {
        desktop: 100,
        device: 100,
    };
}

/// How the remote desktop scale is chosen for a profile (PLAN.md §3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScaleMode {
    /// Device-pixel desktop with the local scale forwarded. Pixel-exact.
    #[default]
    #[serde(rename = "match")]
    MatchLocal,
    /// Device-pixel desktop, but the remote UI scale is fixed to this percentage.
    Fixed(u32),
    /// Logical-size desktop at 100%, stretched locally. For servers that ignore scale.
    #[serde(rename = "legacy")]
    Legacy100,
}

/// How the remote framebuffer is placed when it doesn't match the widget (PLAN.md §3.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FitMode {
    /// Scale to fit, preserving aspect ratio, letterboxed and centred.
    #[default]
    Fit,
    /// Draw 1:1 and let the user scroll; centred if smaller than the widget.
    Scroll,
}

/// What the client asks the server for: a desktop size and scale factors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DesktopRequest {
    pub size: Size,
    pub scale: ScaleFactors,
}

/// Sanitise a surface scale coming from the toolkit. Non-finite or non-positive
/// values (seen briefly while a surface is being realised) map to 1.0.
pub fn sanitize_scale(s: f64) -> f64 {
    if s.is_finite() && s > 0.0 { s } else { 1.0 }
}

fn clamp_dim(v: f64) -> u32 {
    if !v.is_finite() {
        return MIN_DESKTOP_DIM;
    }
    // Floor (never ask for a pixel we can't show; the epsilon absorbs float noise
    // like 1250.9999999), saturating cast, then clamp.
    ((v + FLOOR_EPSILON).floor().max(0.0).min(u32::MAX as f64) as u32)
        .clamp(MIN_DESKTOP_DIM, MAX_DESKTOP_DIM)
}

/// Device sizes within this of the next integer count as that integer.
const FLOOR_EPSILON: f64 = 1e-3;

/// §3.1: requested remote desktop size for a widget of logical size `logical` on a
/// surface of scale `s`: `floor(l × s)` whole device pixels, clamped to 200–8192, width
/// forced even.
///
/// PLAN §3.1 says `round`; we floor so the request never exceeds the whole device
/// pixels actually available (at fractional scales a widget edge can fall mid-pixel,
/// and that half pixel can't show remote content 1:1). Callers should pass the
/// logical size *after* origin snapping.
pub fn requested_desktop_size(logical: LogicalSize, s: f64) -> Size {
    let s = sanitize_scale(s);
    let mut width = clamp_dim(logical.width * s);
    let height = clamp_dim(logical.height * s);
    // Both bounds are even, so rounding down an odd width stays in range.
    width &= !1;
    Size { width, height }
}

/// §3.2: scale factors for a local scale `s`.
///
/// `DesktopScaleFactor = clamp(round(s × 100), 100, 500)`. `DeviceScaleFactor` is the
/// entry of [`DEVICE_SCALES`] nearest to the desktop factor; ties go to the larger value.
pub fn scale_factors(s: f64) -> ScaleFactors {
    let s = sanitize_scale(s);
    let desktop = ((s * 100.0).round().min(MAX_DESKTOP_SCALE as f64) as u32)
        .clamp(MIN_DESKTOP_SCALE, MAX_DESKTOP_SCALE);
    ScaleFactors {
        desktop,
        device: device_scale_for(desktop),
    }
}

/// Nearest valid `DeviceScaleFactor` for a desktop factor (ties round up).
pub fn device_scale_for(desktop: u32) -> u32 {
    let mut best = DEVICE_SCALES[0];
    for &d in &DEVICE_SCALES {
        if d.abs_diff(desktop) <= best.abs_diff(desktop) {
            best = d;
        }
    }
    best
}

/// Combine §3.1–3.3: what to request from the server for a scale mode.
pub fn desktop_request(mode: ScaleMode, logical: LogicalSize, s: f64) -> DesktopRequest {
    let s = sanitize_scale(s);
    match mode {
        ScaleMode::MatchLocal => DesktopRequest {
            size: requested_desktop_size(logical, s),
            scale: scale_factors(s),
        },
        ScaleMode::Fixed(percent) => {
            let desktop = percent.clamp(MIN_DESKTOP_SCALE, MAX_DESKTOP_SCALE);
            DesktopRequest {
                size: requested_desktop_size(logical, s),
                scale: ScaleFactors {
                    desktop,
                    device: device_scale_for(desktop),
                },
            }
        }
        ScaleMode::Legacy100 => DesktopRequest {
            size: requested_desktop_size(logical, 1.0),
            scale: ScaleFactors::UNSCALED,
        },
    }
}

/// Where and how large the remote framebuffer is drawn inside the widget.
///
/// All values are in **device** pixels relative to the widget origin. `offset_*`
/// are whole numbers so the image stays on the device-pixel grid.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    /// Remote pixels → device pixels.
    pub ratio: f64,
    pub offset_x: f64,
    pub offset_y: f64,
    /// Drawn size in device pixels (`desktop × ratio`).
    pub width: f64,
    pub height: f64,
}

impl Viewport {
    /// True when one remote pixel maps to exactly one device pixel. Renderers use
    /// nearest-neighbour filtering in that case and linear otherwise (§3.4).
    pub fn is_pixel_exact(&self) -> bool {
        self.ratio == 1.0
    }

    /// The destination rectangle in logical units for a surface of scale `s`:
    /// `(x, y, width, height)`.
    pub fn logical_rect(&self, s: f64) -> (f64, f64, f64, f64) {
        let s = sanitize_scale(s);
        (
            self.offset_x / s,
            self.offset_y / s,
            self.width / s,
            self.height / s,
        )
    }
}

/// In Fit mode, a desktop within this many device pixels of the widget on both axes is
/// drawn 1:1 (centred, cropping at most a pixel per edge) instead of being scaled. That
/// covers the even-width rule and the device-pixel origin snap, which would otherwise
/// turn a pixel-exact session into a 2559/2560 blur.
pub const CROP_TOLERANCE: u32 = 2;

/// §3.5: compute the viewport for a remote `desktop` shown in a widget of `widget`
/// device pixels. `scroll` is the scroll position in device pixels (Scroll mode
/// only; clamped to the valid range).
pub fn viewport(desktop: Size, widget: Size, mode: FitMode, scroll: (f64, f64)) -> Viewport {
    let (dw, dh) = (desktop.width.max(1) as f64, desktop.height.max(1) as f64);
    let (ww, wh) = (widget.width as f64, widget.height as f64);
    match mode {
        FitMode::Fit => {
            let near = desktop.width.abs_diff(widget.width) <= CROP_TOLERANCE
                && desktop.height.abs_diff(widget.height) <= CROP_TOLERANCE;
            // Near-exact is the common, pixel-exact case: no scaling, no float drift.
            let ratio = if near { 1.0 } else { (ww / dw).min(wh / dh) };
            let (w, h) = (dw * ratio, dh * ratio);
            // Negative offsets (cropping) only in the `near` case; otherwise clamp
            // float noise (e.g. -1e-12) so it can't floor to -1.
            let off = |space: f64| {
                let o = space / 2.0;
                if near { o.floor() } else { o.max(0.0).floor() }
            };
            Viewport {
                ratio,
                offset_x: off(ww - w),
                offset_y: off(wh - h),
                width: w,
                height: h,
            }
        }
        FitMode::Scroll => {
            let axis = |d: f64, w: f64, pos: f64| {
                if d <= w {
                    ((w - d) / 2.0).floor()
                } else {
                    let max = d - w;
                    let pos = if pos.is_finite() { pos } else { 0.0 };
                    -pos.clamp(0.0, max).round()
                }
            };
            Viewport {
                ratio: 1.0,
                offset_x: axis(dw, ww, scroll.0),
                offset_y: axis(dh, wh, scroll.1),
                width: dw,
                height: dh,
            }
        }
    }
}

/// §3.6: map a widget-logical point to remote pixels, clamped to the desktop.
/// Returns the clamped point and whether the original point was inside the image.
pub fn map_input(lx: f64, ly: f64, s: f64, vp: &Viewport, desktop: Size) -> (Point, bool) {
    let s = sanitize_scale(s);
    let ratio = if vp.ratio > 0.0 { vp.ratio } else { 1.0 };
    let rx = (lx * s - vp.offset_x) / ratio;
    let ry = (ly * s - vp.offset_y) / ratio;
    let max_x = desktop.width.saturating_sub(1) as f64;
    let max_y = desktop.height.saturating_sub(1) as f64;
    let inside = rx >= 0.0 && ry >= 0.0 && rx < desktop.width as f64 && ry < desktop.height as f64;
    let clamp = |v: f64, max: f64| {
        if v.is_finite() {
            v.floor().clamp(0.0, max)
        } else {
            0.0
        }
    };
    (
        Point {
            x: clamp(rx, max_x) as u32,
            y: clamp(ry, max_y) as u32,
        },
        inside,
    )
}

/// Turns smooth/hi-res scroll deltas into RDP wheel events of at most ±120 units each,
/// carrying sub-unit remainders so slow scrolling isn't lost (§3.6).
///
/// Input is in RDP units, positive = away from the user / right. Callers convert
/// toolkit deltas (GTK: 1.0 per notch, positive = down) with `-dy × 120`.
#[derive(Debug, Default, Clone)]
pub struct WheelAccumulator {
    remainder: f64,
}

impl WheelAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add `units` (RDP wheel units) and return the events to send.
    pub fn push(&mut self, units: f64) -> Vec<i16> {
        if !units.is_finite() {
            return Vec::new();
        }
        self.remainder += units;
        let mut whole = self.remainder.trunc();
        self.remainder -= whole;
        let mut out = Vec::new();
        while whole != 0.0 {
            let step = whole.clamp(-(WHEEL_NOTCH as f64), WHEEL_NOTCH as f64);
            out.push(step as i16);
            whole -= step;
        }
        out
    }

    pub fn reset(&mut self) {
        self.remainder = 0.0;
    }
}

/// Decides when a new monitor-layout PDU is needed (§3.8). Compares the *negotiated*
/// request (rounded, clamped, even) rather than raw allocations, so odd widths or
/// sub-pixel jitter don't cause resize loops. Time-based debouncing lives in the
/// toolkit layer; this is the pure "has it really changed?" half.
#[derive(Debug, Default, Clone)]
pub struct LayoutTracker {
    last_sent: Option<DesktopRequest>,
}

impl LayoutTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `Some(request)` if it differs from the last one sent, and records it.
    pub fn update(&mut self, request: DesktopRequest) -> Option<DesktopRequest> {
        if self.last_sent == Some(request) {
            None
        } else {
            self.last_sent = Some(request);
            Some(request)
        }
    }

    pub fn last_sent(&self) -> Option<DesktopRequest> {
        self.last_sent
    }

    /// Forget the last request (e.g. after a reconnect or redirection).
    pub fn reset(&mut self) {
        self.last_sent = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn hidpi_200_is_device_pixels() {
        let r = desktop_request(ScaleMode::MatchLocal, LogicalSize::new(1280.0, 800.0), 2.0);
        assert_eq!(r.size, Size::new(2560, 1600));
        assert_eq!(
            r.scale,
            ScaleFactors {
                desktop: 200,
                device: 180
            }
        );
    }

    #[test]
    fn fractional_125() {
        let r = desktop_request(ScaleMode::MatchLocal, LogicalSize::new(1001.0, 701.0), 1.25);
        // 1251.25 -> floor 1251 -> forced even 1250; 876.25 -> 876
        assert_eq!(r.size, Size::new(1250, 876));
        assert_eq!(
            r.scale,
            ScaleFactors {
                desktop: 125,
                device: 140
            }
        );
    }

    #[test]
    fn device_scale_table() {
        assert_eq!(device_scale_for(100), 100);
        assert_eq!(device_scale_for(119), 100);
        assert_eq!(device_scale_for(120), 140); // tie rounds up
        assert_eq!(device_scale_for(150), 140);
        assert_eq!(device_scale_for(160), 180);
        assert_eq!(device_scale_for(500), 180);
    }

    #[test]
    fn legacy_and_fixed() {
        let l = LogicalSize::new(1280.0, 800.0);
        let r = desktop_request(ScaleMode::Legacy100, l, 2.0);
        assert_eq!(r.size, Size::new(1280, 800));
        assert_eq!(r.scale, ScaleFactors::UNSCALED);
        let r = desktop_request(ScaleMode::Fixed(150), l, 2.0);
        assert_eq!(r.size, Size::new(2560, 1600));
        assert_eq!(
            r.scale,
            ScaleFactors {
                desktop: 150,
                device: 140
            }
        );
    }

    #[test]
    fn bogus_scale_is_one() {
        for s in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(scale_factors(s), ScaleFactors::UNSCALED);
        }
    }

    #[test]
    fn clamps() {
        let r = requested_desktop_size(LogicalSize::new(10.0, 10.0), 1.0);
        assert_eq!(r, Size::new(200, 200));
        let r = requested_desktop_size(LogicalSize::new(5000.0, 5000.0), 2.0);
        assert_eq!(r, Size::new(8192, 8192));
    }

    #[test]
    fn fit_exact_is_pixel_exact() {
        let d = Size::new(2560, 1600);
        let vp = viewport(d, d, FitMode::Fit, (0.0, 0.0));
        assert!(vp.is_pixel_exact());
        assert_eq!((vp.offset_x, vp.offset_y), (0.0, 0.0));
        assert_eq!(vp.logical_rect(2.0), (0.0, 0.0, 1280.0, 800.0));
    }

    #[test]
    fn odd_widget_width_has_one_px_border_not_scaling() {
        // Widget 2561 device px wide, desktop forced even to 2560: Fit would scale by
        // 2561/2560 horizontally if height allowed, but height limits ratio to 1.
        let vp = viewport(
            Size::new(2560, 1600),
            Size::new(2561, 1600),
            FitMode::Fit,
            (0.0, 0.0),
        );
        assert!(vp.is_pixel_exact());
        assert_eq!(vp.offset_x, 0.0);
    }

    #[test]
    fn near_miss_is_cropped_not_scaled() {
        // Snapped origin at 125% leaves 875 device rows for an 876-row desktop.
        let vp = viewport(
            Size::new(1250, 876),
            Size::new(1250, 875),
            FitMode::Fit,
            (0.0, 0.0),
        );
        assert!(vp.is_pixel_exact());
        assert_eq!((vp.offset_x, vp.offset_y), (0.0, -1.0));
        // Three pixels off is a real mismatch: scale.
        let vp = viewport(
            Size::new(1250, 876),
            Size::new(1250, 873),
            FitMode::Fit,
            (0.0, 0.0),
        );
        assert!(!vp.is_pixel_exact());
    }

    #[test]
    fn wheel_chunks() {
        let mut w = WheelAccumulator::new();
        assert_eq!(w.push(300.0), vec![120, 120, 60]);
        assert_eq!(w.push(-0.4), Vec::<i16>::new());
        assert_eq!(w.push(-0.7), vec![-1]);
        assert_eq!(w.push(0.5), Vec::<i16>::new());
    }

    #[test]
    fn layout_tracker_dedupes() {
        let mut t = LayoutTracker::new();
        let a = desktop_request(ScaleMode::MatchLocal, LogicalSize::new(1000.5, 700.0), 1.0);
        let b = desktop_request(ScaleMode::MatchLocal, LogicalSize::new(1001.4, 700.2), 1.0);
        assert!(t.update(a).is_some());
        assert!(
            t.update(b).is_none(),
            "jitter must not trigger a new layout"
        );
    }

    fn logical() -> impl Strategy<Value = LogicalSize> {
        (1.0f64..6000.0, 1.0f64..6000.0).prop_map(|(w, h)| LogicalSize::new(w, h))
    }

    fn scale() -> impl Strategy<Value = f64> {
        prop_oneof![
            Just(1.0),
            Just(1.25),
            Just(1.5),
            Just(1.75),
            Just(2.0),
            Just(3.0),
            0.5f64..5.0
        ]
    }

    fn size() -> impl Strategy<Value = Size> {
        (1u32..9000, 1u32..9000).prop_map(|(w, h)| Size::new(w, h))
    }

    proptest! {
        #[test]
        fn near_sizes_are_pixel_exact(w in 200u32..8192, h in 200u32..8192, dx in -2i32..=2, dy in -2i32..=2) {
            let d = Size::new(w, h);
            let widget = Size::new((w as i32 + dx) as u32, (h as i32 + dy) as u32);
            let vp = viewport(d, widget, FitMode::Fit, (0.0, 0.0));
            prop_assert!(vp.is_pixel_exact());
            prop_assert!(vp.offset_x.abs() <= 1.0 && vp.offset_y.abs() <= 1.0);
        }

        #[test]
        fn requested_size_is_valid(l in logical(), s in scale()) {
            let r = requested_desktop_size(l, s);
            prop_assert_eq!(r.width % 2, 0);
            prop_assert!((MIN_DESKTOP_DIM..=MAX_DESKTOP_DIM).contains(&r.width));
            prop_assert!((MIN_DESKTOP_DIM..=MAX_DESKTOP_DIM).contains(&r.height));
            let (dw, dh) = (l.width * s, l.height * s);
            // Never more than available (beyond float noise), at most 2 px less.
            if (MIN_DESKTOP_DIM as f64 + 1.0..MAX_DESKTOP_DIM as f64 - 1.0).contains(&dw) {
                prop_assert!(r.width as f64 <= dw + FLOOR_EPSILON && dw - (r.width as f64) < 2.0);
            }
            if (MIN_DESKTOP_DIM as f64 + 1.0..MAX_DESKTOP_DIM as f64 - 1.0).contains(&dh) {
                prop_assert!(r.height as f64 <= dh + FLOOR_EPSILON && dh - (r.height as f64) < 1.0);
            }
        }

        #[test]
        fn scale_factors_are_valid(s in -10.0f64..20.0) {
            let f = scale_factors(s);
            prop_assert!((MIN_DESKTOP_SCALE..=MAX_DESKTOP_SCALE).contains(&f.desktop));
            prop_assert!(DEVICE_SCALES.contains(&f.device));
        }

        #[test]
        fn device_scale_is_nearest(d in 100u32..=500) {
            let pick = device_scale_for(d);
            for c in DEVICE_SCALES {
                prop_assert!(pick.abs_diff(d) <= c.abs_diff(d));
            }
        }

        #[test]
        fn fit_stays_inside_and_keeps_aspect(d in size(), w in size()) {
            let near = d.width.abs_diff(w.width) <= CROP_TOLERANCE
                && d.height.abs_diff(w.height) <= CROP_TOLERANCE;
            prop_assume!(!near);
            let vp = viewport(d, w, FitMode::Fit, (0.0, 0.0));
            prop_assert!(vp.offset_x >= 0.0 && vp.offset_y >= 0.0);
            prop_assert_eq!(vp.offset_x.fract(), 0.0);
            prop_assert_eq!(vp.offset_y.fract(), 0.0);
            prop_assert!(vp.offset_x + vp.width <= w.width as f64 + 1e-6);
            prop_assert!(vp.offset_y + vp.height <= w.height as f64 + 1e-6);
            let a = d.width as f64 / d.height as f64;
            prop_assert!((vp.width / vp.height - a).abs() / a < 1e-9);
            // It touches at least one pair of edges.
            let tw = (vp.width - w.width as f64).abs() < 1e-6;
            let th = (vp.height - w.height as f64).abs() < 1e-6;
            prop_assert!(tw || th);
        }

        #[test]
        fn scroll_is_one_to_one(d in size(), w in size(), sx in -1e5f64..1e5, sy in -1e5f64..1e5) {
            let vp = viewport(d, w, FitMode::Scroll, (sx, sy));
            prop_assert!(vp.is_pixel_exact());
            prop_assert_eq!(vp.offset_x.fract(), 0.0);
            prop_assert_eq!(vp.offset_y.fract(), 0.0);
            // Never scrolled past the image: the image always covers the widget, or
            // is centred within it.
            if d.width > w.width {
                prop_assert!(vp.offset_x <= 0.0);
                prop_assert!(vp.offset_x + d.width as f64 >= w.width as f64);
            } else {
                prop_assert!(vp.offset_x >= 0.0);
            }
        }

        #[test]
        fn input_is_clamped(d in size(), w in size(), s in scale(),
                            lx in -1e4f64..1e5, ly in -1e4f64..1e5,
                            mode in prop_oneof![Just(FitMode::Fit), Just(FitMode::Scroll)]) {
            let vp = viewport(d, w, mode, (0.0, 0.0));
            let (p, _) = map_input(lx, ly, s, &vp, d);
            prop_assert!(p.x < d.width && p.y < d.height);
        }

        #[test]
        fn input_round_trips(d in size(), w in size(), s in scale(), fx in 0.0f64..1.0, fy in 0.0f64..1.0) {
            // Pick a remote pixel, find the centre of where it's drawn, map back.
            let vp = viewport(d, w, FitMode::Fit, (0.0, 0.0));
            let rx = (fx * d.width as f64).floor().min(d.width as f64 - 1.0);
            let ry = (fy * d.height as f64).floor().min(d.height as f64 - 1.0);
            let lx = (vp.offset_x + (rx + 0.5) * vp.ratio) / s;
            let ly = (vp.offset_y + (ry + 0.5) * vp.ratio) / s;
            let (p, inside) = map_input(lx, ly, s, &vp, d);
            prop_assert!(inside);
            prop_assert_eq!((p.x as f64, p.y as f64), (rx, ry));
        }

        #[test]
        fn wheel_conserves_units(deltas in proptest::collection::vec(-500.0f64..500.0, 0..50)) {
            let mut w = WheelAccumulator::new();
            let mut sent: i64 = 0;
            for d in &deltas {
                for e in w.push(*d) {
                    prop_assert!(e.unsigned_abs() as i32 <= WHEEL_NOTCH && e != 0);
                    sent += e as i64;
                }
            }
            let total: f64 = deltas.iter().sum();
            prop_assert!((total - sent as f64).abs() < 1.0 + 1e-6);
        }

        #[test]
        fn match_local_round_trip_is_pixel_exact(l in logical(), s in scale()) {
            // A server that honours our request yields a 1:1 viewport, apart from at
            // most one column lost to even-width rounding.
            let req = requested_desktop_size(l, s);
            let widget = Size::new(
                ((l.width * s + 1e-3).floor() as u32).max(1),
                ((l.height * s + 1e-3).floor() as u32).max(1),
            );
            let inside_limits = widget.width >= MIN_DESKTOP_DIM && widget.width <= MAX_DESKTOP_DIM
                && widget.height >= MIN_DESKTOP_DIM && widget.height <= MAX_DESKTOP_DIM;
            prop_assume!(inside_limits);
            let vp = viewport(req, widget, FitMode::Fit, (0.0, 0.0));
            prop_assert!(vp.is_pixel_exact(), "ratio {} for {:?} in {:?}", vp.ratio, req, widget);
        }
    }
}
