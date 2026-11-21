// SPDX-License-Identifier: Apache-2.0
//! Fixed-size tiling of the remote framebuffer.
//!
//! Renderers keep one texture per tile and rebuild only the tiles a damage
//! rectangle touches, so a small update never costs a full-frame copy (PLAN §4).

use crate::engine::Rect;
use crate::geometry::Size;

/// Tile edge length in pixels. 256 × 256 × 4 bytes = 256 KiB per tile.
pub const TILE_SIZE: u32 = 256;

/// A grid of tiles covering a framebuffer of `size`. Edge tiles may be smaller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileGrid {
    size: Size,
    cols: u32,
    rows: u32,
}

impl TileGrid {
    pub fn new(size: Size) -> Self {
        Self {
            size,
            cols: size.width.div_ceil(TILE_SIZE),
            rows: size.height.div_ceil(TILE_SIZE),
        }
    }

    pub fn size(&self) -> Size {
        self.size
    }

    pub fn len(&self) -> usize {
        (self.cols * self.rows) as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The pixel rectangle covered by tile `index` (row-major).
    pub fn tile_rect(&self, index: usize) -> Rect {
        let (col, row) = (index as u32 % self.cols, index as u32 / self.cols);
        let (x, y) = (col * TILE_SIZE, row * TILE_SIZE);
        Rect::new(
            x,
            y,
            TILE_SIZE.min(self.size.width - x),
            TILE_SIZE.min(self.size.height - y),
        )
    }

    /// Indices of the tiles that intersect `rect` (clipped to the framebuffer).
    pub fn tiles_for(&self, rect: Rect) -> impl Iterator<Item = usize> + '_ {
        let clipped = rect.clip(self.size);
        let (c0, c1, r0, r1) = match clipped {
            Some(r) => (
                r.x / TILE_SIZE,
                (r.x + r.width - 1) / TILE_SIZE,
                r.y / TILE_SIZE,
                (r.y + r.height - 1) / TILE_SIZE,
            ),
            None => (1, 0, 1, 0), // empty ranges
        };
        (r0..=r1).flat_map(move |r| (c0..=c1).map(move |c| (r * self.cols + c) as usize))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn grid_shape() {
        let g = TileGrid::new(Size::new(2560, 1600));
        assert_eq!(g.len(), 10 * 7);
        assert_eq!(g.tile_rect(0), Rect::new(0, 0, 256, 256));
        // Bottom-right tile is partial: 1600 = 6 × 256 + 64.
        assert_eq!(g.tile_rect(69), Rect::new(2304, 1536, 256, 64));
    }

    #[test]
    fn small_damage_touches_one_tile() {
        let g = TileGrid::new(Size::new(3840, 2160));
        let t: Vec<_> = g.tiles_for(Rect::new(300, 300, 10, 16)).collect();
        assert_eq!(t, vec![g.cols as usize + 1]);
        assert_eq!(g.tiles_for(Rect::new(5000, 0, 10, 10)).count(), 0);
        assert_eq!(g.tiles_for(Rect::new(0, 0, 0, 10)).count(), 0);
    }

    proptest! {
        #[test]
        fn tiles_partition_the_frame(w in 1u32..3000, h in 1u32..3000) {
            let g = TileGrid::new(Size::new(w, h));
            let area: u64 = (0..g.len())
                .map(|i| { let r = g.tile_rect(i); r.width as u64 * r.height as u64 })
                .sum();
            prop_assert_eq!(area, w as u64 * h as u64);
        }

        #[test]
        fn damage_is_covered(w in 1u32..3000, h in 1u32..3000,
                             x in 0u32..3000, y in 0u32..3000, rw in 1u32..800, rh in 1u32..800) {
            let g = TileGrid::new(Size::new(w, h));
            let damage = Rect::new(x, y, rw, rh);
            let hit: std::collections::HashSet<_> = g.tiles_for(damage).collect();
            // Every pixel of the clipped damage lies in a returned tile, and every
            // returned tile intersects the damage.
            if let Some(c) = damage.clip(g.size()) {
                for (px, py) in [(c.x, c.y), (c.x + c.width - 1, c.y + c.height - 1)] {
                    let idx = (py / TILE_SIZE * g.cols + px / TILE_SIZE) as usize;
                    prop_assert!(hit.contains(&idx));
                }
                for i in &hit {
                    let t = g.tile_rect(*i);
                    prop_assert!(t.x < c.x + c.width && c.x < t.x + t.width);
                    prop_assert!(t.y < c.y + c.height && c.y < t.y + t.height);
                }
            } else {
                prop_assert!(hit.is_empty());
            }
        }
    }
}
