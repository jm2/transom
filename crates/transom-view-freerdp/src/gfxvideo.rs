// SPDX-License-Identifier: Apache-2.0
//! Zero-copy H.264 in the graphics pipeline (ADR 0006).
//!
//! FreeRDP's GDI implementation of MS-RDPEGFX keeps every surface in system memory.
//! We wrap its callbacks: AVC420 commands are decoded on the GPU (`hwdec`) instead,
//! and each surface tracks the region where its latest video frame, not system
//! memory, holds the current pixels. The renderer layers the frame (a dmabuf) over the
//! framebuffer, clipped to that region.
//!
//! Correctness of the system-memory copy is kept where FreeRDP needs it:
//! * commands that *write* pixels (other codecs, fills, copies, cache blits) take
//!   their area out of the video region (the framebuffer is current there again);
//! * commands that *read* pixels (surface-to-surface, surface-to-cache, codecs that
//!   only partly cover their rectangle) first copy the video pixels they need back
//!   into the surface, converted exactly like FreeRDP's own decoder would have.
//!
//! The renderer can't import dmabufs (or decoding fails): the region is copied back
//! and FreeRDP decodes from then on. Locking: the hook state is never held while
//! calling into FreeRDP (its callbacks re-enter, e.g. EndFrame → EndPaint).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use transom_view_core::engine::{EngineEvent, Rect};
use transom_view_core::video::{Region, VideoDecode, VideoUpdate};

use crate::engine::{State, state};
use crate::ffi;
use crate::hwdec::{H264Decoder, HwFrame, Nv12};

const OK: ffi::UINT = ffi::CHANNEL_RC_OK;

/// FreeRDP's own callbacks, called through after (or instead of) ours.
#[derive(Clone, Copy)]
struct Orig {
    surface_command: ffi::pcRdpgfxSurfaceCommand,
    solid_fill: ffi::pcRdpgfxSolidFill,
    surface_to_surface: ffi::pcRdpgfxSurfaceToSurface,
    surface_to_cache: ffi::pcRdpgfxSurfaceToCache,
    cache_to_surface: ffi::pcRdpgfxCacheToSurface,
    create_surface: ffi::pcRdpgfxCreateSurface,
    delete_surface: ffi::pcRdpgfxDeleteSurface,
    reset_graphics: ffi::pcRdpgfxResetGraphics,
    map_output: ffi::pcRdpgfxMapSurfaceToOutput,
    map_scaled_output: ffi::pcRdpgfxMapSurfaceToScaledOutput,
    map_window: ffi::pcRdpgfxMapSurfaceToWindow,
    map_scaled_window: ffi::pcRdpgfxMapSurfaceToScaledWindow,
    end_frame: ffi::pcRdpgfxEndFrame,
}

/// Most region rectangles taken from one AVC420 command, and kept per surface (the
/// server controls both counts, and region bookkeeping and rendering cost grow with
/// them). Past these, the video pixels go to the framebuffer instead (`flatten`): the
/// hardware decoder keeps decoding, since a software decoder taking over mid-stream
/// would lack the stream's parameter sets and reference frames until the next IDR.
const MAX_COMMAND_RECTS: usize = 64;
const MAX_SURFACE_RECTS: usize = 256;
/// Hardware decoders (one VA-API device and frame pool each) per session; further
/// surfaces decode in software.
const MAX_HW_DECODERS: usize = 4;

#[derive(Default)]
struct Surface {
    /// Size from Create Surface (pixels).
    size: (u32, u32),
    decoder: Option<H264Decoder>,
    frame: Option<Arc<HwFrame>>,
    /// Download of `frame`, made on the first read-back from it.
    nv12: Option<Nv12>,
    region: Region,
    origin: (u32, u32),
    /// Mapped to the output (not scaled, not a RemoteApp window): only those layers
    /// are shown; anything else falls back to software.
    mapped: bool,
    software: bool,
    new_frame: bool,
    dirty: bool,
    /// The renderer currently has a layer for this surface.
    shown: bool,
}

#[derive(Default)]
struct Stats {
    frames: u64,
    readback_px: u64,
    verified: u64,
    mismatched_px: u64,
    /// Surface commands per codec id (verify-mode diagnostics).
    codecs: std::collections::BTreeMap<u32, u64>,
}

pub(crate) struct Hooks {
    orig: Orig,
    verify: bool,
    /// Hardware decoding was turned off: surfaces created later start in software.
    all_software: bool,
    surfaces: HashMap<u16, Surface>,
    /// Removed surfaces whose layer must be dropped by the renderer.
    removed: Vec<u16>,
    stats: Stats,
}

/// Per-session video state, in `State`.
#[derive(Default)]
pub(crate) struct Video {
    hooks: Mutex<Option<Hooks>>,
    /// Set by the renderer (`EngineCommand::VideoUnsupported`); acted on by the next
    /// graphics callback, on the channel thread that owns the surfaces.
    disable: AtomicBool,
    /// Hardware decoding was turned off for this session (the renderer can't import
    /// the frames, or decoding failed): a later graphics channel (after a reconnect
    /// or resize) stays in software too.
    off: AtomicBool,
}

impl std::fmt::Debug for Video {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Video").finish_non_exhaustive()
    }
}

impl Video {
    pub(crate) fn request_disable(&self) {
        self.off.store(true, Ordering::SeqCst);
        self.disable.store(true, Ordering::SeqCst);
    }

    /// Video layer changes since the last call, for the next `FrameUpdate`.
    pub(crate) fn take_updates(&self) -> Vec<VideoUpdate> {
        let mut g = self.hooks.lock().unwrap_or_else(|e| e.into_inner());
        let Some(h) = g.as_mut() else {
            return Vec::new();
        };
        let mut out: Vec<VideoUpdate> = h
            .removed
            .drain(..)
            .map(|surface| VideoUpdate {
                surface,
                origin: (0, 0),
                frame: None,
                region: Region::new(),
            })
            .collect();
        let mut failed = Vec::new();
        for (&id, s) in h.surfaces.iter_mut() {
            if !s.dirty {
                continue;
            }
            s.dirty = false;
            let visible = s.mapped && !s.software && !s.region.is_empty();
            if !visible {
                if s.shown {
                    s.shown = false;
                    out.push(VideoUpdate {
                        surface: id,
                        origin: s.origin,
                        frame: None,
                        region: Region::new(),
                    });
                }
                continue;
            }
            let frame = if s.new_frame || !s.shown {
                match s.frame.as_ref().map(|f| f.export()) {
                    Some(Ok(d)) => Some(Arc::new(d)),
                    Some(Err(e)) => {
                        tracing::warn!("dmabuf export failed, using software decoding: {e}");
                        failed.push(id);
                        continue;
                    }
                    None => continue,
                }
            } else {
                None
            };
            s.new_frame = false;
            s.shown = true;
            out.push(VideoUpdate {
                surface: id,
                origin: s.origin,
                frame,
                region: s.region.clone(),
            });
        }
        if !failed.is_empty() {
            // Read-back needs the surface memory: leave it to the channel thread.
            self.request_disable();
        }
        out
    }
}

/// Install the hooks on a freshly initialised graphics pipeline (after FreeRDP's
/// gdi_graphics_pipeline_init), if hardware decoding is wanted and works.
///
/// # Safety
/// `gfx` must be the live RdpgfxClientContext set up by FreeRDP's GDI.
pub(crate) unsafe fn attach(state: &State, gfx: *mut ffi::RdpgfxClientContext) {
    if state.config.video_decode == VideoDecode::Software
        || !crate::hwdec::available()
        || state.video.off.load(Ordering::SeqCst)
    {
        return;
    }
    // SAFETY: valid context per the contract; we replace FreeRDP's function
    // pointers with ours and keep the originals to call through.
    unsafe {
        let g = &mut *gfx;
        let orig = Orig {
            surface_command: g.SurfaceCommand,
            solid_fill: g.SolidFill,
            surface_to_surface: g.SurfaceToSurface,
            surface_to_cache: g.SurfaceToCache,
            cache_to_surface: g.CacheToSurface,
            create_surface: g.CreateSurface,
            delete_surface: g.DeleteSurface,
            reset_graphics: g.ResetGraphics,
            map_output: g.MapSurfaceToOutput,
            map_scaled_output: g.MapSurfaceToScaledOutput,
            map_window: g.MapSurfaceToWindow,
            map_scaled_window: g.MapSurfaceToScaledWindow,
            end_frame: g.EndFrame,
        };
        g.SurfaceCommand = Some(surface_command);
        g.SolidFill = Some(solid_fill);
        g.SurfaceToSurface = Some(surface_to_surface);
        g.SurfaceToCache = Some(surface_to_cache);
        g.CacheToSurface = Some(cache_to_surface);
        g.CreateSurface = Some(create_surface);
        g.DeleteSurface = Some(delete_surface);
        g.ResetGraphics = Some(reset_graphics);
        g.MapSurfaceToOutput = Some(map_output);
        g.MapSurfaceToScaledOutput = Some(map_scaled_output);
        g.MapSurfaceToWindow = Some(map_window);
        g.MapSurfaceToScaledWindow = Some(map_scaled_window);
        g.EndFrame = Some(end_frame);
        *state.video.hooks.lock().unwrap_or_else(|e| e.into_inner()) = Some(Hooks {
            orig,
            verify: state.config.video_decode == VideoDecode::Verify,
            all_software: false,
            surfaces: HashMap::new(),
            removed: Vec::new(),
            stats: Stats::default(),
        });
    }
    tracing::info!("graphics pipeline: H.264 decoded on the GPU (zero-copy)");
}

pub(crate) fn hw_available() -> bool {
    crate::hwdec::available()
}

/// The graphics channel closed: drop decoders and layers.
pub(crate) fn detach(state: &State) {
    let mut g = state.video.hooks.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(h) = g.take() {
        h.log_stats();
    }
}

impl Hooks {
    fn log_stats(&self) {
        let s = &self.stats;
        tracing::info!(
            frames = s.frames,
            readback_px = s.readback_px,
            verified = s.verified,
            mismatched_px = s.mismatched_px,
            "hardware H.264 statistics"
        );
        if self.verify {
            // Machine-readable for scripts/hwdec-check.sh.
            println!(
                "HWDEC VERIFY: frames={} verified={} mismatched_px={} readback_px={} codecs={:?}",
                s.frames, s.verified, s.mismatched_px, s.readback_px, s.codecs
            );
        }
    }
}

// ---- helpers -----------------------------------------------------------------

/// Our state from a graphics context (`custom` is FreeRDP's rdpGdi).
///
/// # Safety
/// `gfx` must be a valid graphics context set up by FreeRDP's GDI: `custom` is null
/// or its `rdpGdi`, whose context is a Transom context.
unsafe fn state_of<'a>(gfx: *mut ffi::RdpgfxClientContext) -> Option<&'a State> {
    // SAFETY: FreeRDP's GDI sets `custom` to its rdpGdi, whose context is ours.
    unsafe {
        let gdi = (*gfx).custom as *mut ffi::rdpGdi;
        if gdi.is_null() {
            return None;
        }
        state((*gdi).context)
    }
}

/// `r` clipped to a `w`×`h` surface; `None` if nothing is left.
fn clip(r: Rect, w: u32, h: u32) -> Option<Rect> {
    let x2 = r.x.saturating_add(r.width).min(w);
    let y2 = r.y.saturating_add(r.height).min(h);
    (r.x < x2 && r.y < y2).then(|| Rect::new(r.x, r.y, x2 - r.x, y2 - r.y))
}

fn rect16(r: &ffi::RECTANGLE_16) -> Rect {
    Rect::new(
        r.left as u32,
        r.top as u32,
        r.right.saturating_sub(r.left) as u32,
        r.bottom.saturating_sub(r.top) as u32,
    )
}

/// Run `f` on the hooks (unlocked again before returning), first acting on a pending
/// disable request. `None` if not installed.
///
/// # Safety
/// As for [`state_of`], and only on the graphics channel's thread, which owns the
/// surfaces' memory that falling back to software writes.
unsafe fn with_hooks<R>(
    gfx: *mut ffi::RdpgfxClientContext,
    f: impl FnOnce(&mut Hooks) -> R,
) -> Option<(R, Orig)> {
    // SAFETY: valid graphics context (FreeRDP callback contract).
    let st = unsafe { state_of(gfx) }?;
    let mut g = st.video.hooks.lock().unwrap_or_else(|e| e.into_inner());
    let h = g.as_mut()?;
    if st.video.disable.swap(false, Ordering::SeqCst) {
        h.all_software = true;
        let ids: Vec<u16> = h.surfaces.keys().copied().collect();
        for id in ids {
            // SAFETY: as above; we're on the channel thread that owns surface memory.
            unsafe { h.fall_back_to_software(gfx, id) };
        }
        tracing::info!("hardware H.264 off; FreeRDP decodes from now on");
    }
    let r = f(h);
    Some((r, h.orig))
}

impl Hooks {
    /// Copy the video pixels of `area` (surface coordinates, may overlap) back into
    /// surface `id`'s memory and take them out of its video region.
    unsafe fn read_back(&mut self, gfx: *mut ffi::RdpgfxClientContext, id: u16, area: &[Rect]) {
        if area.is_empty() {
            return;
        }
        let Some(s) = self.surfaces.get_mut(&id) else {
            return;
        };
        let Some(frame) = s.frame.clone() else { return };
        if s.nv12.is_none() {
            match frame.download() {
                Ok(n) => s.nv12 = Some(n),
                Err(e) => {
                    tracing::warn!("read-back failed ({e}); dropping the video region");
                    s.region = Region::new();
                    s.dirty = true;
                    return;
                }
            }
        }
        // SAFETY: FreeRDP's surface for `id` (BGRX, `scanline` bytes per row,
        // `height` rows); we're on the channel thread, which owns its memory.
        unsafe {
            let get = (*gfx).GetSurfaceData.expect("GetSurfaceData");
            let surf = get(gfx, id) as *mut ffi::gdiGfxSurface;
            if surf.is_null() {
                return;
            }
            let surf = &mut *surf;
            let len = surf.scanline as usize * surf.height as usize;
            let dst = std::slice::from_raw_parts_mut(surf.data, len);
            let nv12 = s.nv12.as_ref().expect("downloaded");
            for r in area {
                let Some(r) = r.intersect(Rect::new(0, 0, surf.width, surf.height)) else {
                    continue;
                };
                nv12.to_bgrx(r, dst, surf.scanline as usize);
                let r16 = ffi::RECTANGLE_16 {
                    left: r.x as u16,
                    top: r.y as u16,
                    right: (r.x + r.width) as u16,
                    bottom: (r.y + r.height) as u16,
                };
                let inv = &mut surf.invalidRegion as *mut ffi::REGION16;
                ffi::region16_union_rect(inv, inv, &r16);
                s.region.subtract_rect(r);
                self.stats.readback_px += r.width as u64 * r.height as u64;
            }
        }
        s.dirty = true;
        // SAFETY: forwarded contract.
        unsafe { self.bound(gfx, id) };
    }

    /// Keep surface `id`'s video region within `MAX_SURFACE_RECTS`: a hole cut into it
    /// splits a rectangle, so repeated writes could fragment it without bound.
    unsafe fn bound(&mut self, gfx: *mut ffi::RdpgfxClientContext, id: u16) {
        let over = self
            .surfaces
            .get(&id)
            .is_some_and(|s| s.region.rects().len() > MAX_SURFACE_RECTS);
        if over {
            tracing::debug!(surface = id, "video region too fragmented; flattening");
            // SAFETY: forwarded contract.
            unsafe { self.flatten(gfx, id) };
        }
    }

    /// Before a command writes `r` of surface `id`: copy back the video pixels inside
    /// `r` if the command may not overwrite all of them (`partial`), then hand `r`
    /// to the framebuffer.
    unsafe fn before_write(
        &mut self,
        gfx: *mut ffi::RdpgfxClientContext,
        id: u16,
        r: Rect,
        partial: bool,
    ) {
        let Some(s) = self.surfaces.get_mut(&id) else {
            return;
        };
        if s.region.is_empty() {
            return;
        }
        if partial {
            let area = s.region.intersect_rect(r);
            // SAFETY: forwarded contract.
            unsafe { self.read_back(gfx, id, area.rects()) };
        }
        let Some(s) = self.surfaces.get_mut(&id) else {
            return;
        };
        s.region.subtract_rect(r);
        s.dirty = true;
        // SAFETY: forwarded contract.
        unsafe { self.bound(gfx, id) };
    }

    /// Copy surface `id`'s whole video region into its memory and empty the region.
    /// The surface keeps its hardware decoder: later frames start a new region.
    unsafe fn flatten(&mut self, gfx: *mut ffi::RdpgfxClientContext, id: u16) {
        let area = match self.surfaces.get(&id) {
            Some(s) => s.region.clone(),
            None => return,
        };
        // SAFETY: forwarded contract.
        unsafe { self.read_back(gfx, id, area.rects()) };
        if let Some(s) = self.surfaces.get_mut(&id) {
            s.region = Region::new();
            s.dirty = true;
        }
    }

    /// Stop hardware decoding for surface `id` (FreeRDP's software decoder takes over;
    /// the video pixels are copied back first).
    unsafe fn fall_back_to_software(&mut self, gfx: *mut ffi::RdpgfxClientContext, id: u16) {
        let area = match self.surfaces.get(&id) {
            Some(s) => s.region.clone(),
            None => return,
        };
        // SAFETY: forwarded contract.
        unsafe { self.read_back(gfx, id, area.rects()) };
        if let Some(s) = self.surfaces.get_mut(&id) {
            s.software = true;
            s.decoder = None;
            s.frame = None;
            s.nv12 = None;
            s.region = Region::new();
            s.dirty = true;
        }
    }
}

// ---- callbacks ---------------------------------------------------------------

/// Call FreeRDP's original callback (if it had one).
///
/// # Safety
/// `gfx` and `pdu` are what FreeRDP passed to the hook being forwarded.
unsafe fn through<P>(
    f: Option<unsafe extern "C" fn(*mut ffi::RdpgfxClientContext, *const P) -> ffi::UINT>,
    gfx: *mut ffi::RdpgfxClientContext,
    pdu: *const P,
) -> ffi::UINT {
    match f {
        // SAFETY: FreeRDP's own callback with the arguments it gave us.
        Some(f) => unsafe { f(gfx, pdu) },
        None => OK,
    }
}

macro_rules! call {
    ($orig:expr, $field:ident, $gfx:expr, $pdu:expr) => {
        through($orig.$field, $gfx, $pdu)
    };
}

unsafe extern "C" fn surface_command(
    gfx: *mut ffi::RdpgfxClientContext,
    cmd: *const ffi::RDPGFX_SURFACE_COMMAND,
) -> ffi::UINT {
    // SAFETY: FreeRDP passes a valid context and command.
    let c = unsafe { &*cmd };
    let id = c.surfaceId as u16;
    let rect = Rect::new(
        c.left,
        c.top,
        c.right.saturating_sub(c.left),
        c.bottom.saturating_sub(c.top),
    );
    enum Next {
        Software,
        Done,
        Verify(Arc<HwFrame>, Vec<Rect>),
    }
    // SAFETY: valid context; the closure runs with the hooks locked, calling only
    // libavcodec and our own surface bookkeeping (no FreeRDP callbacks).
    let r = unsafe {
        with_hooks(gfx, |h| {
            *h.stats.codecs.entry(c.codecId).or_default() += 1;
            let avc = c.codecId == ffi::RDPGFX_CODECID_AVC420 && !c.extra.is_null();
            let usable = h.surfaces.get(&id).is_some_and(|s| !s.software);
            if !(avc && usable) {
                // Another codec (or a software surface): it may write only parts of
                // its rectangle (RemoteFX tiles, alpha), so keep what it doesn't.
                // Progressive commands come with an empty rectangle but may paint
                // anywhere on the surface: treat them as covering all of it.
                let r = if rect.width == 0 || rect.height == 0 {
                    let (w, hh) = h.surfaces.get(&id).map_or((0, 0), |s| s.size);
                    Rect::new(0, 0, w, hh)
                } else {
                    rect
                };
                h.before_write(gfx, id, r, true);
                return Next::Software;
            }
            let bs = &*(c.extra as *const ffi::RDPGFX_AVC420_BITMAP_STREAM);
            let data = std::slice::from_raw_parts(bs.data, bs.length as usize);
            let n = if bs.meta.regionRects.is_null() {
                0
            } else {
                bs.meta.numRegionRects as usize
            };
            let (sw, sh) = h.surfaces.get(&id).map_or((0, 0), |s| s.size);
            // Clipped to the surface: FreeRDP checks only that each is well formed.
            let rects: Vec<Rect> = if n == 0 {
                Vec::new()
            } else {
                std::slice::from_raw_parts(bs.meta.regionRects, n)
                    .iter()
                    .filter_map(|r| clip(rect16(r), sw, sh))
                    .collect()
            };
            let decoders = h.surfaces.values().filter(|s| s.decoder.is_some()).count();
            let s = h.surfaces.get_mut(&id).expect("checked");
            if s.decoder.is_none() && decoders >= MAX_HW_DECODERS {
                tracing::info!(surface = id, "hardware decoder limit reached; software");
                h.fall_back_to_software(gfx, id);
                return Next::Software;
            }
            if s.decoder.is_none() {
                // The decoder refuses streams larger than the surface they draw on.
                // H.264 codes whole macroblocks, so a 1280×761 surface arrives as a
                // 1280×768 picture: allow for alignment (64 covers any encoder's).
                let align = |v: u32| u64::from(v.div_ceil(64) * 64);
                let max_pixels = (align(sw) * align(sh)).max(64 * 64) as i64;
                match H264Decoder::new(max_pixels) {
                    Ok(d) => s.decoder = Some(d),
                    Err(e) => {
                        tracing::warn!("hardware decoder: {e}");
                        h.fall_back_to_software(gfx, id);
                        return Next::Software;
                    }
                }
            }
            match s.decoder.as_mut().expect("set").decode(data) {
                Ok(Some(frame)) => {
                    let frame = Arc::new(frame);
                    s.frame = Some(frame.clone());
                    s.nv12 = None;
                    if rects.len() > MAX_COMMAND_RECTS {
                        // Too many to track: this frame goes to the framebuffer, as
                        // FreeRDP's decoder would put it (the earlier region from the
                        // same frame first, then the command's rectangles).
                        tracing::debug!(
                            surface = id,
                            rects = n,
                            "many video rectangles; flattening"
                        );
                        h.flatten(gfx, id);
                        // Converting each rectangle repeats the work where they
                        // overlap (FreeRDP accepts any number of copies of the whole
                        // surface). An encoder's rectangles don't overlap much: past
                        // twice the surface, skip this frame's pixels instead (the
                        // decoder keeps its state; nothing outside the mask is drawn).
                        let area: u64 = rects
                            .iter()
                            .map(|r| u64::from(r.width) * u64::from(r.height))
                            .sum();
                        if area <= 2 * u64::from(sw) * u64::from(sh) {
                            h.read_back(gfx, id, &rects);
                        } else {
                            tracing::debug!(
                                surface = id,
                                rects = n,
                                "video rectangles overlap; frame skipped"
                            );
                        }
                    } else {
                        for r in &rects {
                            s.region.union_rect(*r);
                        }
                        h.bound(gfx, id);
                    }
                    let s = h.surfaces.get_mut(&id).expect("checked");
                    s.new_frame = true;
                    s.dirty = true;
                    h.stats.frames += 1;
                    if h.verify {
                        Next::Verify(frame, rects)
                    } else {
                        Next::Done
                    }
                }
                Ok(None) => Next::Done,
                Err(e) => {
                    tracing::warn!("hardware H.264 decode failed ({e}); software from now on");
                    h.fall_back_to_software(gfx, id);
                    Next::Software
                }
            }
        })
    };
    match r {
        None => OK, // not installed: unreachable (hooks are only set with state)
        Some((Next::Done, _)) => OK,
        // SAFETY: forwarding FreeRDP's arguments.
        Some((Next::Software, orig)) => unsafe { call!(orig, surface_command, gfx, cmd) },
        Some((Next::Verify(frame, rects), orig)) => {
            // FreeRDP decodes too (into the framebuffer, which the test compares the
            // rendered video against); check the decoders agree on every pixel.
            // SAFETY: forwarding FreeRDP's arguments.
            let rc = unsafe { call!(orig, surface_command, gfx, cmd) };
            // SAFETY: as above; surface memory read on its owning thread.
            unsafe { verify(gfx, id, &frame, &rects) };
            rc
        }
    }
}

/// Verify mode: hardware-decoded pixels (converted like FreeRDP) vs FreeRDP's own.
unsafe fn verify(gfx: *mut ffi::RdpgfxClientContext, id: u16, frame: &HwFrame, rects: &[Rect]) {
    let Ok(nv12) = frame.download() else { return };
    // SAFETY: FreeRDP's surface memory, on the channel thread.
    let (mismatch, total) = unsafe {
        let get = (*gfx).GetSurfaceData.expect("GetSurfaceData");
        let surf = get(gfx, id) as *mut ffi::gdiGfxSurface;
        if surf.is_null() {
            return;
        }
        let surf = &*surf;
        let stride = surf.scanline as usize;
        let have = std::slice::from_raw_parts(surf.data, stride * surf.height as usize);
        let mut mine = have.to_vec();
        let mut mismatch = 0u64;
        let mut total = 0u64;
        let mut maxd = 0;
        let mut at = (0, 0, [0u8; 3], [0u8; 3]);
        for r in rects {
            let Some(r) = r.intersect(Rect::new(0, 0, surf.width, surf.height)) else {
                continue;
            };
            nv12.to_bgrx_freerdp_pairing(r, &mut mine, stride);
            for y in r.y..r.y + r.height {
                for x in r.x..r.x + r.width {
                    let o = y as usize * stride + x as usize * 4;
                    total += 1;
                    if mine[o..o + 3] != have[o..o + 3] {
                        mismatch += 1;
                        let d = (0..3)
                            .map(|k| (mine[o + k] as i32 - have[o + k] as i32).abs())
                            .max()
                            .unwrap_or(0);
                        if d > maxd {
                            maxd = d;
                            at = (
                                x,
                                y,
                                [mine[o], mine[o + 1], mine[o + 2]],
                                [have[o], have[o + 1], have[o + 2]],
                            );
                        }
                    }
                }
            }
        }
        if mismatch > 0 {
            tracing::debug!(
                maxd,
                ?at,
                ?rects,
                w = surf.width,
                h = surf.height,
                fw = nv12.width,
                fh = nv12.height,
                "verify detail (x, y, hw BGR, sw BGR)"
            );
        }
        (mismatch, total)
    };
    // SAFETY: as above.
    unsafe {
        with_hooks(gfx, |h| {
            h.stats.verified += 1;
            h.stats.mismatched_px += mismatch;
            if mismatch > 0 && h.stats.verified <= 5 {
                tracing::warn!(
                    mismatch,
                    total,
                    "hardware and software H.264 decodes differ"
                );
            }
        });
    }
}

unsafe extern "C" fn solid_fill(
    gfx: *mut ffi::RdpgfxClientContext,
    pdu: *const ffi::RDPGFX_SOLID_FILL_PDU,
) -> ffi::UINT {
    // SAFETY: valid context and PDU (FreeRDP callback contract).
    unsafe {
        let p = &*pdu;
        let rects: Vec<Rect> = if p.fillRects.is_null() {
            Vec::new()
        } else {
            std::slice::from_raw_parts(p.fillRects, p.fillRectCount as usize)
                .iter()
                .map(rect16)
                .collect()
        };
        let orig = with_hooks(gfx, |h| {
            for r in rects {
                h.before_write(gfx, p.surfaceId, r, false);
            }
        });
        match orig {
            Some((_, o)) => call!(o, solid_fill, gfx, pdu),
            None => OK,
        }
    }
}

unsafe extern "C" fn surface_to_surface(
    gfx: *mut ffi::RdpgfxClientContext,
    pdu: *const ffi::RDPGFX_SURFACE_TO_SURFACE_PDU,
) -> ffi::UINT {
    // SAFETY: valid context and PDU.
    unsafe {
        let p = &*pdu;
        let src = rect16(&p.rectSrc);
        let pts = if p.destPts.is_null() {
            &[][..]
        } else {
            std::slice::from_raw_parts(p.destPts, p.destPtsCount as usize)
        };
        let orig = with_hooks(gfx, |h| {
            if let Some(s) = h.surfaces.get(&p.surfaceIdSrc) {
                let area = s.region.intersect_rect(src);
                h.read_back(gfx, p.surfaceIdSrc, area.rects());
            }
            for pt in pts {
                let d = Rect::new(pt.x as u32, pt.y as u32, src.width, src.height);
                h.before_write(gfx, p.surfaceIdDest, d, false);
            }
        });
        match orig {
            Some((_, o)) => call!(o, surface_to_surface, gfx, pdu),
            None => OK,
        }
    }
}

unsafe extern "C" fn surface_to_cache(
    gfx: *mut ffi::RdpgfxClientContext,
    pdu: *const ffi::RDPGFX_SURFACE_TO_CACHE_PDU,
) -> ffi::UINT {
    // SAFETY: valid context and PDU.
    unsafe {
        let p = &*pdu;
        let src = rect16(&p.rectSrc);
        let orig = with_hooks(gfx, |h| {
            if let Some(s) = h.surfaces.get(&p.surfaceId) {
                let area = s.region.intersect_rect(src);
                h.read_back(gfx, p.surfaceId, area.rects());
            }
        });
        match orig {
            Some((_, o)) => call!(o, surface_to_cache, gfx, pdu),
            None => OK,
        }
    }
}

unsafe extern "C" fn cache_to_surface(
    gfx: *mut ffi::RdpgfxClientContext,
    pdu: *const ffi::RDPGFX_CACHE_TO_SURFACE_PDU,
) -> ffi::UINT {
    // SAFETY: valid context and PDU; the cache entry belongs to FreeRDP's GDI.
    unsafe {
        let p = &*pdu;
        let (w, h) = match (*gfx).GetCacheSlotData {
            Some(get) => {
                let e = get(gfx, p.cacheSlot) as *const ffi::gdiGfxCacheEntry;
                if e.is_null() {
                    (0, 0)
                } else {
                    ((*e).width, (*e).height)
                }
            }
            None => (0, 0),
        };
        let pts = if p.destPts.is_null() {
            &[][..]
        } else {
            std::slice::from_raw_parts(p.destPts, p.destPtsCount as usize)
        };
        let orig = with_hooks(gfx, |hk| {
            for pt in pts {
                hk.before_write(
                    gfx,
                    p.surfaceId,
                    Rect::new(pt.x as u32, pt.y as u32, w, h),
                    false,
                );
            }
        });
        match orig {
            Some((_, o)) => call!(o, cache_to_surface, gfx, pdu),
            None => OK,
        }
    }
}

unsafe extern "C" fn create_surface(
    gfx: *mut ffi::RdpgfxClientContext,
    pdu: *const ffi::RDPGFX_CREATE_SURFACE_PDU,
) -> ffi::UINT {
    // SAFETY: valid context and PDU.
    unsafe {
        let id = (*pdu).surfaceId;
        let size = ((*pdu).width as u32, (*pdu).height as u32);
        match with_hooks(gfx, |h| {
            let new = Surface {
                size,
                software: h.all_software,
                ..Default::default()
            };
            if let Some(old) = h.surfaces.insert(id, new)
                && old.shown
            {
                h.removed.push(id);
            }
        }) {
            Some((_, o)) => call!(o, create_surface, gfx, pdu),
            None => OK,
        }
    }
}

unsafe extern "C" fn delete_surface(
    gfx: *mut ffi::RdpgfxClientContext,
    pdu: *const ffi::RDPGFX_DELETE_SURFACE_PDU,
) -> ffi::UINT {
    // SAFETY: valid context and PDU.
    unsafe {
        let id = (*pdu).surfaceId;
        match with_hooks(gfx, |h| {
            if h.surfaces.remove(&id).is_some_and(|s| s.shown) {
                h.removed.push(id);
            }
        }) {
            Some((_, o)) => call!(o, delete_surface, gfx, pdu),
            None => OK,
        }
    }
}

unsafe extern "C" fn reset_graphics(
    gfx: *mut ffi::RdpgfxClientContext,
    pdu: *const ffi::RDPGFX_RESET_GRAPHICS_PDU,
) -> ffi::UINT {
    // SAFETY: valid context and PDU.
    unsafe {
        match with_hooks(gfx, |h| {
            for (id, s) in h.surfaces.drain() {
                if s.shown {
                    h.removed.push(id);
                }
            }
        }) {
            Some((_, o)) => call!(o, reset_graphics, gfx, pdu),
            None => OK,
        }
    }
}

unsafe extern "C" fn map_output(
    gfx: *mut ffi::RdpgfxClientContext,
    pdu: *const ffi::RDPGFX_MAP_SURFACE_TO_OUTPUT_PDU,
) -> ffi::UINT {
    // SAFETY: valid context and PDU.
    unsafe {
        let p = &*pdu;
        let Some((_, o)) = with_hooks(gfx, |_| ()) else {
            return OK;
        };
        // FreeRDP validates the mapping (surface exists, origin within the output);
        // record it only once it's accepted.
        let rc = call!(o, map_output, gfx, pdu);
        if rc == OK {
            with_hooks(gfx, |h| {
                if let Some(s) = h.surfaces.get_mut(&p.surfaceId) {
                    s.origin = (p.outputOriginX, p.outputOriginY);
                    s.mapped = true;
                    s.dirty = true;
                }
            });
        }
        rc
    }
}

/// Scaled outputs and RemoteApp windows aren't shown as layers: software for those.
///
/// # Safety
/// As for [`with_hooks`].
unsafe fn unmappable(gfx: *mut ffi::RdpgfxClientContext, id: u16) -> Option<Orig> {
    // SAFETY: forwarded contract.
    unsafe {
        with_hooks(gfx, |h| {
            h.fall_back_to_software(gfx, id);
            if let Some(s) = h.surfaces.get_mut(&id) {
                s.mapped = false;
            }
        })
        .map(|(_, o)| o)
    }
}

unsafe extern "C" fn map_scaled_output(
    gfx: *mut ffi::RdpgfxClientContext,
    pdu: *const ffi::RDPGFX_MAP_SURFACE_TO_SCALED_OUTPUT_PDU,
) -> ffi::UINT {
    // SAFETY: valid context and PDU.
    match unsafe { unmappable(gfx, (*pdu).surfaceId) } {
        // SAFETY: forwarding FreeRDP's arguments.
        Some(o) => unsafe { call!(o, map_scaled_output, gfx, pdu) },
        None => OK,
    }
}

unsafe extern "C" fn map_window(
    gfx: *mut ffi::RdpgfxClientContext,
    pdu: *const ffi::RDPGFX_MAP_SURFACE_TO_WINDOW_PDU,
) -> ffi::UINT {
    // SAFETY: valid context and PDU.
    match unsafe { unmappable(gfx, (*pdu).surfaceId) } {
        // SAFETY: forwarding FreeRDP's arguments.
        Some(o) => unsafe { call!(o, map_window, gfx, pdu) },
        None => OK,
    }
}

unsafe extern "C" fn map_scaled_window(
    gfx: *mut ffi::RdpgfxClientContext,
    pdu: *const ffi::RDPGFX_MAP_SURFACE_TO_SCALED_WINDOW_PDU,
) -> ffi::UINT {
    // SAFETY: valid context and PDU.
    match unsafe { unmappable(gfx, (*pdu).surfaceId) } {
        // SAFETY: forwarding FreeRDP's arguments.
        Some(o) => unsafe { call!(o, map_scaled_window, gfx, pdu) },
        None => OK,
    }
}

unsafe extern "C" fn end_frame(
    gfx: *mut ffi::RdpgfxClientContext,
    pdu: *const ffi::RDPGFX_END_FRAME_PDU,
) -> ffi::UINT {
    // SAFETY: valid context and PDU. FreeRDP's EndFrame flushes surfaces to the
    // framebuffer (→ our end_paint, which attaches the video updates); a frame with
    // only video changes paints nothing, so send those here.
    unsafe {
        let Some((_, o)) = with_hooks(gfx, |_| ()) else {
            return OK;
        };
        let rc = call!(o, end_frame, gfx, pdu);
        if let Some(st) = state_of(gfx) {
            let video = st.video.take_updates();
            if !video.is_empty() {
                st.emit(EngineEvent::FrameUpdate {
                    damage: Vec::new(),
                    video,
                });
            }
        }
        rc
    }
}
