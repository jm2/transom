// SPDX-License-Identifier: GPL-3.0-or-later
//! Per-tile GDK textures for the remote framebuffer.

use gtk::{gdk, glib};
use transom_view_core::engine::{FrameData, Rect};
use transom_view_core::geometry::Size;
use transom_view_core::tiles::TileGrid;

/// One texture per tile, rebuilt only when damaged.
#[derive(Debug)]
pub struct TileCache {
    grid: TileGrid,
    generation: Option<u64>,
    textures: Vec<Option<gdk::Texture>>,
    dirty: Vec<bool>,
    any_dirty: bool,
}

impl Default for TileCache {
    fn default() -> Self {
        Self {
            grid: TileGrid::new(Size::default()),
            generation: None,
            textures: Vec::new(),
            dirty: Vec::new(),
            any_dirty: false,
        }
    }
}

impl TileCache {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn mark(&mut self, damage: &[Rect]) {
        for r in damage {
            for i in self.grid.tiles_for(*r) {
                if let Some(d) = self.dirty.get_mut(i) {
                    *d = true;
                    self.any_dirty = true;
                }
            }
        }
    }

    /// Bring textures up to date with `frame`. Copies only dirty tiles.
    pub fn refresh(&mut self, frame: &FrameData) {
        if self.generation != Some(frame.generation) || self.grid.size() != frame.size {
            self.grid = TileGrid::new(frame.size);
            self.generation = Some(frame.generation);
            self.textures = vec![None; self.grid.len()];
            self.dirty = vec![true; self.grid.len()];
            self.any_dirty = true;
        }
        if !self.any_dirty {
            return;
        }
        for i in 0..self.grid.len() {
            if std::mem::take(&mut self.dirty[i]) {
                self.textures[i] = Some(tile_texture(frame, self.grid.tile_rect(i)));
            }
        }
        self.any_dirty = false;
    }

    /// `(tile rectangle in remote pixels, texture)` for every built tile.
    pub fn iter(&self) -> impl Iterator<Item = (Rect, &gdk::Texture)> {
        self.textures
            .iter()
            .enumerate()
            .filter_map(|(i, t)| t.as_ref().map(|t| (self.grid.tile_rect(i), t)))
    }
}

fn tile_texture(frame: &FrameData, r: Rect) -> gdk::Texture {
    let row_bytes = r.width as usize * 4;
    let mut buf = Vec::with_capacity(row_bytes * r.height as usize);
    for y in r.y..r.y + r.height {
        let start = y as usize * frame.stride + r.x as usize * 4;
        buf.extend_from_slice(&frame.pixels[start..start + row_bytes]);
    }
    let bytes = glib::Bytes::from_owned(buf);
    gdk::MemoryTexture::new(
        r.width as i32,
        r.height as i32,
        gdk::MemoryFormat::B8g8r8x8,
        &bytes,
        row_bytes,
    )
    .upcast()
}

use gtk::prelude::*;
