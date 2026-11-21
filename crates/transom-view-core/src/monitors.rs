// SPDX-License-Identifier: Apache-2.0
//! Multi-monitor layout: local monitors (logical geometry + per-monitor scale) →
//! an RDP monitor layout in device pixels (MS-RDPEDISP / TS_MONITOR_DEF).
//!
//! Each monitor gets a device-pixel size and its *own* scale factors, so a 100%
//! monitor next to a 200% one is pixel-exact on both. Positions can't simply be
//! `logical × scale` (mixed scales would overlap or leave gaps); instead monitors
//! keep their logical arrangement and are packed edge to edge in device space:
//! a monitor's left edge is the rightmost device edge of all monitors entirely to its
//! left; its top edge is the bottom edge of the monitors directly above it. The
//! primary ends up at (0, 0) as RDP requires.

use crate::engine::Rect;
use crate::geometry::{self, ScaleFactors, ScaleMode, Size};

/// A local monitor as the toolkit reports it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LocalMonitor {
    /// Logical geometry (GDK monitor geometry).
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    /// Surface scale of a window on this monitor (fractional).
    pub scale: f64,
    pub primary: bool,
}

/// One monitor of the remote layout, in device (= remote) pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteMonitor {
    pub x: i32,
    pub y: i32,
    pub size: Size,
    pub scale: ScaleFactors,
    pub primary: bool,
}

impl RemoteMonitor {
    /// This monitor's area of the remote framebuffer, given the layout's
    /// bounding-box origin (the framebuffer's (0,0)).
    pub fn region(&self, origin: (i32, i32)) -> Rect {
        Rect::new(
            (self.x - origin.0) as u32,
            (self.y - origin.1) as u32,
            self.size.width,
            self.size.height,
        )
    }
}

/// Bounding box origin and size of a remote layout (the remote framebuffer).
pub fn bounds(monitors: &[RemoteMonitor]) -> ((i32, i32), Size) {
    let x0 = monitors.iter().map(|m| m.x).min().unwrap_or(0);
    let y0 = monitors.iter().map(|m| m.y).min().unwrap_or(0);
    let x1 = monitors
        .iter()
        .map(|m| m.x + m.size.width as i32)
        .max()
        .unwrap_or(0);
    let y1 = monitors
        .iter()
        .map(|m| m.y + m.size.height as i32)
        .max()
        .unwrap_or(0);
    ((x0, y0), Size::new((x1 - x0) as u32, (y1 - y0) as u32))
}

fn overlaps(a0: i32, a1: i32, b0: i32, b1: i32) -> bool {
    a0 < b1 && b0 < a1
}

/// Compute the remote layout for `local` monitors (at most 16, as RDP allows).
/// Returns an empty layout for an empty input.
pub fn layout(local: &[LocalMonitor], mode: ScaleMode) -> Vec<RemoteMonitor> {
    let local: Vec<&LocalMonitor> = local.iter().take(16).collect();
    if local.is_empty() {
        return Vec::new();
    }
    // Sizes and factors per monitor, exactly as for a single fullscreen view.
    let reqs: Vec<_> = local
        .iter()
        .map(|m| {
            geometry::desktop_request(
                mode,
                geometry::LogicalSize::new(m.width as f64, m.height as f64),
                m.scale,
            )
        })
        .collect();

    // Pack along x in logical-x order, then along y in logical-y order.
    let n = local.len();
    let mut dx = vec![0i32; n];
    let mut dy = vec![0i32; n];
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&i| (local[i].x, local[i].y));
    for (k, &i) in order.iter().enumerate() {
        let m = local[i];
        dx[i] = order[..k]
            .iter()
            // Everything entirely to the left, whatever its vertical position: that
            // keeps diagonal arrangements from collapsing onto each other.
            .filter(|&&j| local[j].x + local[j].width <= m.x)
            .map(|&j| dx[j] + reqs[j].size.width as i32)
            .max()
            .unwrap_or(0);
    }
    order.sort_by_key(|&i| (local[i].y, local[i].x));
    for (k, &i) in order.iter().enumerate() {
        let m = local[i];
        dy[i] = order[..k]
            .iter()
            .filter(|&&j| {
                let o = local[j];
                o.y + o.height <= m.y && overlaps(o.x, o.x + o.width, m.x, m.x + m.width)
            })
            .map(|&j| dy[j] + reqs[j].size.height as i32)
            .max()
            .unwrap_or(0);
    }

    let primary = local.iter().position(|m| m.primary).unwrap_or(0);
    let (px, py) = (dx[primary], dy[primary]);
    (0..n)
        .map(|i| RemoteMonitor {
            x: dx[i] - px,
            y: dy[i] - py,
            size: reqs[i].size,
            scale: reqs[i].scale,
            primary: i == primary,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn mon(x: i32, y: i32, w: i32, h: i32, scale: f64, primary: bool) -> LocalMonitor {
        LocalMonitor {
            x,
            y,
            width: w,
            height: h,
            scale,
            primary,
        }
    }

    #[test]
    fn laptop_200_plus_external_100() {
        // 2560x1600 @200% laptop (logical 1280x800) left of a 1920x1080 @100% monitor.
        let l = layout(
            &[
                mon(0, 0, 1280, 800, 2.0, true),
                mon(1280, 0, 1920, 1080, 1.0, false),
            ],
            ScaleMode::MatchLocal,
        );
        assert_eq!(
            l[0],
            RemoteMonitor {
                x: 0,
                y: 0,
                size: Size::new(2560, 1600),
                scale: ScaleFactors {
                    desktop: 200,
                    device: 180
                },
                primary: true
            }
        );
        assert_eq!(l[1].x, 2560);
        assert_eq!(l[1].size, Size::new(1920, 1080));
        assert_eq!(
            l[1].scale,
            ScaleFactors {
                desktop: 100,
                device: 100
            }
        );
        assert_eq!(bounds(&l), ((0, 0), Size::new(4480, 1600)));
    }

    #[test]
    fn primary_on_the_right_gives_negative_x() {
        let l = layout(
            &[
                mon(0, 0, 1920, 1080, 1.0, false),
                mon(1920, 0, 1920, 1080, 1.0, true),
            ],
            ScaleMode::MatchLocal,
        );
        assert_eq!((l[1].x, l[0].x), (0, -1920));
        assert_eq!(l[0].region(bounds(&l).0), Rect::new(0, 0, 1920, 1080));
        assert_eq!(l[1].region(bounds(&l).0), Rect::new(1920, 0, 1920, 1080));
    }

    #[test]
    fn stacked_vertically() {
        let l = layout(
            &[
                mon(0, 0, 1600, 900, 1.25, true),
                mon(0, 900, 1600, 900, 1.0, false),
            ],
            ScaleMode::MatchLocal,
        );
        assert_eq!(l[1].y, l[0].size.height as i32);
        assert_eq!(l[1].x, 0);
    }

    fn arrangement() -> impl Strategy<Value = Vec<LocalMonitor>> {
        // A row of 1–4 monitors with random sizes, scales and vertical offsets.
        proptest::collection::vec(
            (
                640i32..3000,
                480i32..2000,
                prop_oneof![Just(1.0), Just(1.25), Just(1.5), Just(2.0)],
                -300i32..300,
            ),
            1..5,
        )
        .prop_flat_map(|ms| {
            let n = ms.len();
            (Just(ms), 0..n)
        })
        .prop_map(|(ms, p)| {
            let mut x = 0;
            ms.into_iter()
                .enumerate()
                .map(|(i, (w, h, s, y))| {
                    let m = mon(x, y, w, h, s, i == p);
                    x += w;
                    m
                })
                .collect()
        })
    }

    proptest! {
        #[test]
        fn layouts_are_valid(local in arrangement()) {
            let l = layout(&local, ScaleMode::MatchLocal);
            prop_assert_eq!(l.len(), local.len());
            prop_assert_eq!(l.iter().filter(|m| m.primary).count(), 1);
            let p = l.iter().find(|m| m.primary).unwrap();
            prop_assert_eq!((p.x, p.y), (0, 0));
            for (i, a) in l.iter().enumerate() {
                prop_assert_eq!(a.size.width % 2, 0);
                for b in &l[i + 1..] {
                    let overlap = overlaps(a.x, a.x + a.size.width as i32, b.x, b.x + b.size.width as i32)
                        && overlaps(a.y, a.y + a.size.height as i32, b.y, b.y + b.size.height as i32);
                    prop_assert!(!overlap, "{:?} overlaps {:?}", a, b);
                }
            }
            // Horizontal neighbours touch exactly (no gaps in a row).
            for w in l.windows(2) {
                prop_assert_eq!(w[0].x + w[0].size.width as i32, w[1].x);
            }
            let (origin, size) = bounds(&l);
            for m in &l {
                let r = m.region(origin);
                prop_assert!(r.x + r.width <= size.width && r.y + r.height <= size.height);
            }
        }
    }
}
