// SPDX-License-Identifier: Apache-2.0
//! Zero-copy video (ADR 0006): hardware-decoded H.264 frames handed from the engine
//! to the renderer as dmabufs, plus the region bookkeeping that says where on a
//! graphics surface the video frame (rather than the system-memory framebuffer)
//! holds the current pixels.

use std::sync::Arc;

use crate::engine::Rect;

/// How the engine decodes H.264 (AVC420) graphics updates.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum VideoDecode {
    /// Hardware decoding to dmabufs where available (Linux, VA-API), with a fallback
    /// to FreeRDP's software decoder.
    #[default]
    Auto,
    /// Always FreeRDP's software decoder (the system-memory framebuffer path).
    Software,
    /// Both: hardware frames are shown, and FreeRDP also decodes into the framebuffer
    /// so tests can compare the two (decoder output is checked per frame and the
    /// result logged). Testing only: double the decode work.
    Verify,
}

/// A set of pixels as non-overlapping rectangles.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Region {
    rects: Vec<Rect>,
}

impl Region {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.rects.is_empty()
    }

    pub fn rects(&self) -> &[Rect] {
        &self.rects
    }

    /// Add `r` (the parts already covered are not duplicated).
    pub fn union_rect(&mut self, r: Rect) {
        if r.width == 0 || r.height == 0 {
            return;
        }
        let mut pieces = vec![r];
        for have in &self.rects {
            pieces = pieces
                .into_iter()
                .flat_map(|p| subtract(p, *have))
                .collect();
            if pieces.is_empty() {
                return;
            }
        }
        self.rects.extend(pieces);
    }

    /// Remove `r`.
    pub fn subtract_rect(&mut self, r: Rect) {
        if r.width == 0 || r.height == 0 {
            return;
        }
        self.rects = self
            .rects
            .iter()
            .flat_map(|have| subtract(*have, r))
            .collect();
    }

    /// The part of the region inside `r`.
    pub fn intersect_rect(&self, r: Rect) -> Region {
        Region {
            rects: self
                .rects
                .iter()
                .filter_map(|have| have.intersect(r))
                .collect(),
        }
    }

    /// Shift every rectangle by `(dx, dy)` (e.g. surface → desktop coordinates),
    /// saturating at `u32::MAX`.
    pub fn translated(&self, dx: u32, dy: u32) -> Region {
        Region {
            rects: self.rects.iter().map(|r| shifted(*r, dx, dy)).collect(),
        }
    }

    /// Where a layer showing this region (surface coordinates) of a surface placed
    /// at `origin` may draw: each rectangle moved to desktop coordinates and clipped to
    /// `bounds` (the desktop, or the part of it a view shows). The origin comes from
    /// the server and is checked by nobody else against the desktop size.
    pub fn placed(&self, origin: (u32, u32), bounds: Rect) -> impl Iterator<Item = Rect> + '_ {
        self.rects
            .iter()
            .filter_map(move |r| shifted(*r, origin.0, origin.1).intersect(bounds))
    }

    pub fn area(&self) -> u64 {
        self.rects
            .iter()
            .map(|r| r.width as u64 * r.height as u64)
            .sum()
    }
}

fn shifted(r: Rect, dx: u32, dy: u32) -> Rect {
    Rect::new(
        r.x.saturating_add(dx),
        r.y.saturating_add(dy),
        r.width,
        r.height,
    )
}

/// `a` minus `b` as up to four rectangles (top, bottom, left, right bands).
fn subtract(a: Rect, b: Rect) -> Vec<Rect> {
    let Some(i) = a.intersect(b) else {
        return vec![a];
    };
    let mut out = Vec::with_capacity(4);
    let (ax1, ay1) = (a.x + a.width, a.y + a.height);
    let (ix1, iy1) = (i.x + i.width, i.y + i.height);
    if i.y > a.y {
        out.push(Rect::new(a.x, a.y, a.width, i.y - a.y));
    }
    if iy1 < ay1 {
        out.push(Rect::new(a.x, iy1, a.width, ay1 - iy1));
    }
    if i.x > a.x {
        out.push(Rect::new(a.x, i.y, i.x - a.x, i.height));
    }
    if ix1 < ax1 {
        out.push(Rect::new(ix1, i.y, ax1 - ix1, i.height));
    }
    out
}

#[cfg(unix)]
pub use dmabuf::{DmabufFrame, DmabufPlane, FOURCC_NV12};

/// Dmabufs are a Linux (Unix fd) concept: elsewhere there are no video frames, and
/// [`VideoUpdate::frame`] is always `None` (ADR 0002: this crate builds everywhere).
#[cfg(not(unix))]
#[derive(Debug)]
pub enum DmabufFrame {}

#[cfg(unix)]
mod dmabuf {
    use std::fmt;
    use std::os::fd::OwnedFd;

    /// DRM fourcc `NV12` (Y plane + interleaved CbCr plane, 4:2:0).
    pub const FOURCC_NV12: u32 = u32::from_le_bytes(*b"NV12");

    /// One plane of a dmabuf.
    #[derive(Debug)]
    pub struct DmabufPlane {
        pub fd: OwnedFd,
        pub offset: u32,
        pub stride: u32,
    }

    /// A decoded video frame in GPU memory, as a dmabuf. The YCbCr → RGB conversion is
    /// BT.709, full range, as MS-RDPEGFX specifies (and FreeRDP's software path uses).
    ///
    /// The decoder's surface stays reserved until this is dropped, so the renderer
    /// keeps the frame (e.g. in its texture's release function) for as long as it may
    /// sample it.
    pub struct DmabufFrame {
        pub width: u32,
        pub height: u32,
        pub fourcc: u32,
        pub modifier: u64,
        pub planes: Vec<DmabufPlane>,
        keepalive: Box<dyn Send + Sync>,
    }

    impl DmabufFrame {
        /// `keepalive` is whatever must live while the buffer is in use (the engine's
        /// reference to the decoded surface).
        pub fn new(
            width: u32,
            height: u32,
            fourcc: u32,
            modifier: u64,
            planes: Vec<DmabufPlane>,
            keepalive: Box<dyn Send + Sync>,
        ) -> Self {
            Self {
                width,
                height,
                fourcc,
                modifier,
                planes,
                keepalive,
            }
        }
    }

    impl fmt::Debug for DmabufFrame {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            let _ = &self.keepalive;
            f.debug_struct("DmabufFrame")
                .field("width", &self.width)
                .field("height", &self.height)
                .field("fourcc", &format_args!("{:#010x}", self.fourcc))
                .field("modifier", &format_args!("{:#x}", self.modifier))
                .field("planes", &self.planes)
                .finish()
        }
    }
}

/// The video layer of one graphics surface, as of a frame update.
#[derive(Debug, Clone)]
pub struct VideoUpdate {
    /// GFX surface id (one layer per surface).
    pub surface: u16,
    /// Where the surface's top-left is on the desktop (remote pixels).
    pub origin: (u32, u32),
    /// A new frame, or `None` to keep showing the previous one (only the region
    /// changed).
    pub frame: Option<Arc<DmabufFrame>>,
    /// Where on the surface (surface coordinates) the video frame is current; the
    /// framebuffer holds the pixels everywhere else. Empty: remove the layer.
    pub region: Region,
}

/// Fold `later` into `earlier` so that applying the result in order does what applying
/// both in turn would, carrying the newest frame and region. `earlier` may name a
/// surface more than once (an engine's update list can remove a surface and then show
/// its replacement), so each update merges into the *last* entry for its surface.
pub fn merge_updates(earlier: &mut Vec<VideoUpdate>, later: Vec<VideoUpdate>) {
    for u in later {
        match earlier.iter_mut().rev().find(|e| e.surface == u.surface) {
            None => earlier.push(u),
            // After a removal, an update without a frame has no layer to change.
            Some(e) if e.region.is_empty() && u.frame.is_none() => {}
            Some(e) => {
                e.origin = u.origin;
                if u.frame.is_some() {
                    e.frame = u.frame;
                }
                e.region = u.region;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(r: &Region) -> u64 {
        r.area()
    }

    fn overlap_free(r: &Region) -> bool {
        let v = r.rects();
        (0..v.len()).all(|i| (i + 1..v.len()).all(|j| v[i].intersect(v[j]).is_none()))
    }

    #[test]
    fn union_does_not_double_count() {
        let mut r = Region::new();
        r.union_rect(Rect::new(0, 0, 100, 100));
        r.union_rect(Rect::new(50, 50, 100, 100));
        assert_eq!(area(&r), 100 * 100 * 2 - 50 * 50);
        assert!(overlap_free(&r));
        r.union_rect(Rect::new(10, 10, 20, 20)); // already covered
        assert_eq!(area(&r), 100 * 100 * 2 - 50 * 50);
    }

    #[test]
    fn subtract_punches_holes() {
        let mut r = Region::new();
        r.union_rect(Rect::new(0, 0, 100, 100));
        r.subtract_rect(Rect::new(25, 25, 50, 50));
        assert_eq!(area(&r), 100 * 100 - 50 * 50);
        assert!(overlap_free(&r));
        assert!(r.intersect_rect(Rect::new(30, 30, 10, 10)).is_empty());
        r.subtract_rect(Rect::new(0, 0, 1000, 1000));
        assert!(r.is_empty());
    }

    #[test]
    fn intersect_and_translate() {
        let mut r = Region::new();
        r.union_rect(Rect::new(0, 0, 10, 10));
        r.union_rect(Rect::new(20, 0, 10, 10));
        let i = r.intersect_rect(Rect::new(5, 0, 20, 5));
        assert_eq!(area(&i), 5 * 5 * 2);
        let t = r.translated(100, 7);
        assert_eq!(t.rects()[0], Rect::new(100, 7, 10, 10));
        let t = r.translated(u32::MAX - 5, 0);
        assert_eq!(t.rects()[0], Rect::new(u32::MAX - 5, 0, 10, 10));
        assert_eq!(t.rects()[1], Rect::new(u32::MAX, 0, 10, 10)); // saturated
    }

    #[test]
    fn placed_layers_stay_inside_the_bounds() {
        let mut r = Region::new();
        r.union_rect(Rect::new(0, 0, 100, 50));
        r.union_rect(Rect::new(0, 50, 10, 10));
        let desktop = Rect::new(0, 0, 1920, 1080);
        let at = |origin| r.placed(origin, desktop).collect::<Vec<_>>();
        // Inside: just moved.
        assert_eq!(
            at((10, 20)),
            [Rect::new(10, 20, 100, 50), Rect::new(10, 70, 10, 10)]
        );
        // Straddling the right and bottom edges: cut at the desktop.
        assert_eq!(at((1870, 1040)), [Rect::new(1870, 1040, 50, 40)]);
        // Entirely outside (the letterbox), and an origin that would overflow.
        assert!(at((32766, 0)).is_empty());
        assert!(at((u32::MAX, u32::MAX)).is_empty());
        // A follower's part of the desktop.
        let part = Rect::new(1920, 0, 1920, 1080);
        let v: Vec<_> = r.placed((1900, 0), part).collect();
        assert_eq!(v, [Rect::new(1920, 0, 80, 50)]);
    }

    #[cfg(unix)]
    fn region(r: Rect) -> Region {
        let mut g = Region::new();
        g.union_rect(r);
        g
    }

    #[cfg(unix)]
    fn update(surface: u16, frame: Option<Arc<DmabufFrame>>, r: Rect) -> VideoUpdate {
        VideoUpdate {
            surface,
            origin: (r.x, 0),
            frame,
            region: region(r),
        }
    }

    #[cfg(unix)]
    fn frame(width: u32) -> Option<Arc<DmabufFrame>> {
        Some(Arc::new(DmabufFrame::new(
            width,
            16,
            FOURCC_NV12,
            0,
            Vec::new(),
            Box::new(()),
        )))
    }

    #[cfg(unix)]
    #[test]
    fn merged_updates_keep_the_newest_frame_and_region() {
        let (a, b) = (Rect::new(0, 0, 8, 8), Rect::new(4, 0, 8, 8));
        let mut v = vec![update(1, frame(1), a), update(2, frame(2), a)];
        merge_updates(
            &mut v,
            vec![
                update(1, None, b),               // region only: frame 1 stays
                update(2, frame(3), b),           // new frame
                update(3, None, Rect::default()), // removal of another layer
            ],
        );
        assert_eq!(v.len(), 3);
        assert_eq!(v[0].frame.as_ref().map(|f| f.width), Some(1));
        assert_eq!((v[0].origin, &v[0].region), ((4, 0), &region(b)));
        assert_eq!(v[1].frame.as_ref().map(|f| f.width), Some(3));
        assert!(v[2].region.is_empty());

        // Removed, then shown again with a frame: the new frame; removed, then a
        // region-only change: still removed.
        let mut v = vec![
            update(1, None, Rect::default()),
            update(2, None, Rect::default()),
        ];
        merge_updates(&mut v, vec![update(1, frame(4), b), update(2, None, b)]);
        assert_eq!(v[0].frame.as_ref().map(|f| f.width), Some(4));
        assert_eq!(v[0].region, region(b));
        assert!(v[1].region.is_empty());
        // Shown, then removed: removed.
        let mut v = vec![update(1, frame(5), a)];
        merge_updates(&mut v, vec![update(1, None, Rect::default())]);
        assert!(v[0].region.is_empty());
    }

    /// Apply updates as the view does: an empty region removes the layer; a frame
    /// creates or replaces it; otherwise an existing layer takes the new region.
    /// Layers are (frame width, region) by surface.
    #[cfg(unix)]
    fn apply(layers: &mut std::collections::BTreeMap<u16, (u32, Region)>, updates: &[VideoUpdate]) {
        for u in updates {
            if u.region.is_empty() {
                layers.remove(&u.surface);
            } else if let Some(f) = &u.frame {
                layers.insert(u.surface, (f.width, u.region.clone()));
            } else if let Some(l) = layers.get_mut(&u.surface) {
                l.1 = u.region.clone();
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn merging_matches_applying_in_turn() {
        let (a, b) = (Rect::new(0, 0, 8, 8), Rect::new(4, 0, 8, 8));
        let gone = Rect::default();
        // Batches as the engine emits them, in order. The first names surface 1
        // twice: removed, then its replacement shown.
        let cases = vec![
            vec![
                vec![update(1, None, gone), update(1, frame(1), a)],
                vec![update(1, None, gone)],
            ],
            vec![
                vec![update(1, None, gone), update(1, frame(1), a)],
                vec![update(1, None, b)],
                vec![update(2, frame(2), a), update(1, None, gone)],
                vec![update(1, frame(3), b)],
            ],
            vec![
                vec![update(1, frame(1), a)],
                vec![update(1, None, gone), update(1, None, b)],
            ],
        ];
        for batches in cases {
            let mut start = std::collections::BTreeMap::new();
            start.insert(1, (9, region(a))); // a layer shown before
            let mut in_turn = start.clone();
            // The queue stores the first batch as it came.
            let mut merged = batches[0].clone();
            apply(&mut in_turn, &batches[0]);
            for batch in &batches[1..] {
                apply(&mut in_turn, batch);
                merge_updates(&mut merged, batch.clone());
            }
            let mut at_once = start;
            apply(&mut at_once, &merged);
            assert_eq!(at_once, in_turn);
        }
    }

    #[cfg(unix)]
    #[test]
    fn fourcc() {
        assert_eq!(FOURCC_NV12, 0x3231_564e);
    }
}
