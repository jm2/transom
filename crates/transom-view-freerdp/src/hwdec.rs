// SPDX-License-Identifier: Apache-2.0
//! Hardware H.264 decoding to dmabufs (ADR 0006): libavcodec's native H.264 decoder
//! with VA-API, frames exported as DRM PRIME (dmabuf) descriptors without copying.
//! Also downloads a frame to system memory when FreeRDP needs those pixels (a
//! surface-to-surface copy, a cache store, a partial update by another codec).

use std::ffi::{CStr, c_int};
use std::os::fd::{BorrowedFd, OwnedFd};
use std::ptr;
use std::sync::OnceLock;

use transom_view_core::engine::Rect;
use transom_view_core::video::{DmabufFrame, DmabufPlane, FOURCC_NV12};

#[allow(
    non_upper_case_globals,
    non_camel_case_types,
    non_snake_case,
    dead_code,
    unsafe_op_in_unsafe_fn,
    missing_debug_implementations,
    clippy::all
)]
mod av {
    include!(concat!(env!("OUT_DIR"), "/av_bindings.rs"));
}

const fn fourcc(c: &[u8; 4]) -> u32 {
    u32::from_le_bytes(*c)
}
const FOURCC_R8: u32 = fourcc(b"R8  ");
const FOURCC_GR88: u32 = fourcc(b"GR88");
const FOURCC_RG88: u32 = fourcc(b"RG88");
/// `AVERROR(EAGAIN)` on Linux.
const AVERROR_EAGAIN: c_int = -11;

/// Whether hardware H.264 decoding works here: libavcodec has its native H.264
/// decoder (distribution "free" builds may only wrap OpenH264, which can't use
/// VA-API), it supports VA-API, and a VA-API device opens. Probed once.
pub(crate) fn available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        // SAFETY: plain libavcodec calls on valid arguments.
        unsafe {
            av::av_log_set_level(av::AV_LOG_ERROR as c_int);
        }
        let ok = match H264Decoder::new(1920 * 1080) {
            Ok(_) => true,
            Err(e) => {
                tracing::info!("hardware H.264 decoding unavailable: {e}");
                false
            }
        };
        if ok {
            tracing::info!("hardware H.264 decoding available (VA-API)");
        }
        ok
    })
}

/// A libavcodec H.264 decoder on a VA-API device. One per graphics surface (the
/// decoder holds the stream's reference frames).
pub(crate) struct H264Decoder {
    ctx: *mut av::AVCodecContext,
    pkt: *mut av::AVPacket,
    frame: *mut av::AVFrame,
    buf: Vec<u8>,
}

// SAFETY: the decoder is only used from one thread at a time (behind a Mutex);
// libavcodec contexts aren't tied to the thread that created them.
unsafe impl Send for H264Decoder {}

impl std::fmt::Debug for H264Decoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("H264Decoder").finish_non_exhaustive()
    }
}

unsafe extern "C" fn get_format(
    ctx: *mut av::AVCodecContext,
    mut fmts: *const av::AVPixelFormat,
) -> av::AVPixelFormat {
    // SAFETY: libavcodec passes its context and a list terminated by AV_PIX_FMT_NONE.
    unsafe {
        // The hardware frame pool is sized by the coded picture, which libavcodec's
        // own `max_pixels` check doesn't cover (it checks the cropped size, and only
        // later): a small surface must not get pictures coded far larger.
        let coded = i64::from((*ctx).coded_width) * i64::from((*ctx).coded_height);
        if (*ctx).max_pixels > 0 && coded > (*ctx).max_pixels {
            return av::AV_PIX_FMT_NONE;
        }
        while *fmts != av::AV_PIX_FMT_NONE {
            if *fmts == av::AV_PIX_FMT_VAAPI {
                return av::AV_PIX_FMT_VAAPI;
            }
            fmts = fmts.add(1);
        }
    }
    // No silent software fallback inside libavcodec: fail, and let FreeRDP decode.
    av::AV_PIX_FMT_NONE
}

impl H264Decoder {
    /// `max_pixels`: the largest picture (width × height) the stream may declare; a
    /// bigger one is refused instead of allocating its frame pool.
    pub(crate) fn new(max_pixels: i64) -> Result<Self, String> {
        // SAFETY: libavcodec API used as documented; every allocation is checked and
        // owned by the returned value (freed in Drop) or freed on the error paths.
        unsafe {
            let codec = av::avcodec_find_decoder_by_name(c"h264".as_ptr());
            if codec.is_null() {
                return Err("libavcodec has no native H.264 decoder".into());
            }
            let mut vaapi = false;
            for i in 0.. {
                let cfg = av::avcodec_get_hw_config(codec, i);
                if cfg.is_null() {
                    break;
                }
                if (*cfg).device_type == av::AV_HWDEVICE_TYPE_VAAPI
                    && (*cfg).methods & av::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as c_int != 0
                {
                    vaapi = true;
                }
            }
            if !vaapi {
                return Err("libavcodec's H.264 decoder has no VA-API support".into());
            }
            let mut dev: *mut av::AVBufferRef = ptr::null_mut();
            let rc = av::av_hwdevice_ctx_create(
                &mut dev,
                av::AV_HWDEVICE_TYPE_VAAPI,
                ptr::null(),
                ptr::null_mut(),
                0,
            );
            if rc < 0 || dev.is_null() {
                return Err(format!("no VA-API device (error {rc})"));
            }
            let ctx = av::avcodec_alloc_context3(codec);
            if ctx.is_null() {
                av::av_buffer_unref(&mut dev);
                return Err("avcodec_alloc_context3 failed".into());
            }
            (*ctx).hw_device_ctx = dev; // the context takes our reference
            (*ctx).get_format = Some(get_format);
            // One frame out per access unit (RDP streams have no reordering), no
            // frame threads (they add latency), and spare surfaces for the frames the
            // renderer still holds.
            (*ctx).flags |= av::AV_CODEC_FLAG_LOW_DELAY as c_int;
            (*ctx).thread_count = 1;
            (*ctx).extra_hw_frames = 8;
            (*ctx).max_pixels = max_pixels;
            let me = Self {
                ctx,
                pkt: av::av_packet_alloc(),
                frame: av::av_frame_alloc(),
                buf: Vec::new(),
            };
            if me.pkt.is_null() || me.frame.is_null() {
                return Err("out of memory".into());
            }
            let rc = av::avcodec_open2(me.ctx, codec, ptr::null_mut());
            if rc < 0 {
                return Err(format!("avcodec_open2 failed ({rc})"));
            }
            Ok(me)
        }
    }

    /// Decode one H.264 access unit (Annex B, as AVC420 carries it). `Ok(None)` if
    /// the decoder produced no picture for it.
    pub(crate) fn decode(&mut self, data: &[u8]) -> Result<Option<HwFrame>, String> {
        // libavcodec's bitstream readers need zeroed padding after the data.
        let pad = av::AV_INPUT_BUFFER_PADDING_SIZE as usize;
        self.buf.clear();
        self.buf.extend_from_slice(data);
        self.buf.resize(data.len() + pad, 0);
        // SAFETY: `pkt`/`frame` are owned and valid; the packet borrows `buf` only for
        // the duration of avcodec_send_packet (not refcounted, so libavcodec copies).
        unsafe {
            (*self.pkt).data = self.buf.as_mut_ptr();
            (*self.pkt).size = data.len() as c_int;
            let rc = av::avcodec_send_packet(self.ctx, self.pkt);
            (*self.pkt).data = ptr::null_mut();
            (*self.pkt).size = 0;
            if rc < 0 {
                return Err(format!("avcodec_send_packet failed ({rc})"));
            }
            let mut out = None;
            loop {
                let rc = av::avcodec_receive_frame(self.ctx, self.frame);
                if rc == AVERROR_EAGAIN {
                    break;
                }
                if rc < 0 {
                    return Err(format!("avcodec_receive_frame failed ({rc})"));
                }
                if (*self.frame).format != av::AV_PIX_FMT_VAAPI {
                    av::av_frame_unref(self.frame);
                    return Err("decoder did not produce a VA-API frame".into());
                }
                let f = av::av_frame_clone(self.frame);
                av::av_frame_unref(self.frame);
                if f.is_null() {
                    return Err("out of memory".into());
                }
                out = Some(HwFrame(f));
            }
            Ok(out)
        }
    }
}

impl Drop for H264Decoder {
    fn drop(&mut self) {
        // SAFETY: each pointer is owned (or null, which the free functions accept).
        unsafe {
            av::av_frame_free(&mut self.frame);
            av::av_packet_free(&mut self.pkt);
            av::avcodec_free_context(&mut self.ctx);
        }
    }
}

/// A decoded picture in a VA-API surface (a reference: the surface stays out of the
/// decoder's pool until every reference is gone).
pub(crate) struct HwFrame(*mut av::AVFrame);

// SAFETY: the frame is immutable after decoding; FFmpeg frame references are
// refcounted atomically and may be released from any thread.
unsafe impl Send for HwFrame {}
// SAFETY: as for Send: shared references only read the immutable frame.
unsafe impl Sync for HwFrame {}

impl std::fmt::Debug for HwFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (w, h) = self.size();
        write!(f, "HwFrame({w}x{h})")
    }
}

impl Drop for HwFrame {
    fn drop(&mut self) {
        // SAFETY: owned frame reference.
        unsafe { av::av_frame_free(&mut self.0) };
    }
}

/// A frame mapped to DRM PRIME; holds the mapping (and through it the source
/// surface) for as long as the dmabuf is in use.
struct Mapped(*mut av::AVFrame);
// SAFETY: as for HwFrame.
unsafe impl Send for Mapped {}
// SAFETY: as for HwFrame.
unsafe impl Sync for Mapped {}
impl Drop for Mapped {
    fn drop(&mut self) {
        // SAFETY: owned frame (unmaps, then releases the source reference).
        unsafe { av::av_frame_free(&mut self.0) };
    }
}

impl HwFrame {
    pub(crate) fn size(&self) -> (u32, u32) {
        // SAFETY: valid frame.
        unsafe { ((*self.0).width as u32, (*self.0).height as u32) }
    }

    /// Export as a dmabuf (no copy). The frame stays reserved until the returned
    /// value is dropped.
    pub(crate) fn export(&self) -> Result<DmabufFrame, String> {
        // SAFETY: mapping API used as documented; the descriptor is valid while the
        // mapped frame lives, and we duplicate the fds we hand out.
        unsafe {
            let m = av::av_frame_alloc();
            if m.is_null() {
                return Err("out of memory".into());
            }
            let mapped = Mapped(m);
            (*m).format = av::AV_PIX_FMT_DRM_PRIME;
            let rc = av::av_hwframe_map(m, self.0, av::AV_HWFRAME_MAP_READ as c_int);
            if rc < 0 {
                return Err(format!("av_hwframe_map to DRM PRIME failed ({rc})"));
            }
            let desc = &*((*m).data[0] as *const av::AVDRMFrameDescriptor);
            let layers = &desc.layers[..desc.nb_layers as usize];
            // VA-API exports NV12 either as one NV12 layer with two planes, or as
            // separate R8 (Y) and GR88/RG88 (CbCr) layers.
            let fourcc = match layers {
                [l] if l.format == FOURCC_NV12 => FOURCC_NV12,
                [y, c]
                    if y.format == FOURCC_R8
                        && (c.format == FOURCC_GR88 || c.format == FOURCC_RG88) =>
                {
                    FOURCC_NV12
                }
                _ => {
                    let f: Vec<_> = layers.iter().map(|l| format!("{:#x}", l.format)).collect();
                    return Err(format!("unexpected DRM layers {f:?}"));
                }
            };
            let mut planes = Vec::new();
            let mut modifier = None;
            for l in layers {
                for p in &l.planes[..l.nb_planes as usize] {
                    let obj = &desc.objects[p.object_index as usize];
                    if *modifier.get_or_insert(obj.format_modifier) != obj.format_modifier {
                        return Err("planes with different modifiers".into());
                    }
                    let fd: OwnedFd = BorrowedFd::borrow_raw(obj.fd)
                        .try_clone_to_owned()
                        .map_err(|e| format!("dup: {e}"))?;
                    planes.push(DmabufPlane {
                        fd,
                        offset: p.offset as u32,
                        stride: p.pitch as u32,
                    });
                }
            }
            if planes.len() != 2 {
                return Err(format!("NV12 with {} planes", planes.len()));
            }
            let (w, h) = self.size();
            Ok(DmabufFrame::new(
                w,
                h,
                fourcc,
                modifier.unwrap_or(0),
                planes,
                Box::new(mapped),
            ))
        }
    }

    /// Copy the picture to system memory.
    pub(crate) fn download(&self) -> Result<Nv12, String> {
        // SAFETY: transfer API used as documented; the software frame is owned here
        // and its planes are read within their bounds (stride × rows).
        unsafe {
            let mut sw = av::av_frame_alloc();
            if sw.is_null() {
                return Err("out of memory".into());
            }
            (*sw).format = av::AV_PIX_FMT_NV12;
            let rc = av::av_hwframe_transfer_data(sw, self.0, 0);
            if rc < 0 {
                av::av_frame_free(&mut sw);
                return Err(format!("av_hwframe_transfer_data failed ({rc})"));
            }
            let (w, h) = ((*sw).width as u32, (*sw).height as u32);
            let ys = (*sw).linesize[0] as usize;
            let cs = (*sw).linesize[1] as usize;
            let y = std::slice::from_raw_parts((*sw).data[0], ys * h as usize).to_vec();
            let c = std::slice::from_raw_parts((*sw).data[1], cs * h.div_ceil(2) as usize).to_vec();
            av::av_frame_free(&mut sw);
            Ok(Nv12 {
                width: w,
                height: h,
                y,
                y_stride: ys,
                c,
                c_stride: cs,
            })
        }
    }
}

/// A picture in system memory: Y plane, then interleaved Cb/Cr at half resolution.
#[derive(Debug)]
pub(crate) struct Nv12 {
    pub width: u32,
    pub height: u32,
    y: Vec<u8>,
    y_stride: usize,
    c: Vec<u8>,
    c_stride: usize,
}

#[inline]
fn clip(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

impl Nv12 {
    /// Convert `r` to BGRX into `dst` (a surface with `stride` bytes per row), with
    /// exactly FreeRDP's integer BT.709 full-range conversion and nearest chroma, so
    /// the result is what FreeRDP's software path would have written.
    pub(crate) fn to_bgrx(&self, r: Rect, dst: &mut [u8], stride: usize) {
        self.convert(r, dst, stride, false);
    }

    /// As [`Self::to_bgrx`], but pairing rows and columns from the rectangle's own
    /// origin like FreeRDP's converter does (for a rectangle at an odd x or y it
    /// reuses the previous chroma sample for one pixel of each pair). Only for
    /// comparing against FreeRDP's output in verify mode: the plain conversion is the
    /// correct 4:2:0 sampling.
    pub(crate) fn to_bgrx_freerdp_pairing(&self, r: Rect, dst: &mut [u8], stride: usize) {
        self.convert(r, dst, stride, true);
    }

    fn convert(&self, r: Rect, dst: &mut [u8], stride: usize, rect_pairs: bool) {
        let Some(r) = r.intersect(Rect::new(0, 0, self.width, self.height)) else {
            return;
        };
        let (cx0, cy0) = (r.x / 2, r.y / 2);
        for py in r.y..r.y + r.height {
            let cy = if rect_pairs {
                cy0 + (py - r.y) / 2
            } else {
                py / 2
            };
            let yrow = &self.y[py as usize * self.y_stride..];
            let crow = &self.c[cy as usize * self.c_stride..];
            let out = &mut dst[py as usize * stride..];
            for px in r.x..r.x + r.width {
                let cx = if rect_pairs {
                    cx0 + (px - r.x) / 2
                } else {
                    px / 2
                };
                let yv = yrow[px as usize] as i32;
                let u = crow[cx as usize * 2] as i32 - 128;
                let v = crow[cx as usize * 2 + 1] as i32 - 128;
                let o = px as usize * 4;
                out[o] = clip((256 * yv + 475 * u) >> 8);
                out[o + 1] = clip((256 * yv - 48 * u - 120 * v) >> 8);
                out[o + 2] = clip((256 * yv + 403 * v) >> 8);
                out[o + 3] = 0xff;
            }
        }
    }
}

#[allow(dead_code)]
fn cstr(p: *const std::ffi::c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        // SAFETY: non-null C string from libavcodec.
        unsafe { CStr::from_ptr(p).to_string_lossy().into_owned() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversion_matches_freerdp_formula() {
        // 2x2, one chroma sample: grey, and a saturated colour.
        let mut n = Nv12 {
            width: 2,
            height: 2,
            y: vec![128, 128, 128, 128],
            y_stride: 2,
            c: vec![128, 128],
            c_stride: 2,
        };
        let mut dst = vec![0u8; 16];
        n.to_bgrx(Rect::new(0, 0, 2, 2), &mut dst, 8);
        assert_eq!(&dst[0..4], &[128, 128, 128, 255]);
        n.c = vec![0, 255];
        n.to_bgrx(Rect::new(0, 0, 2, 2), &mut dst, 8);
        // B = (256*128 + 475*-128) >> 8 = -109 → 0; G = (32768 + 6144 - 15240)>>8 = 92;
        // R = (32768 + 403*127) >> 8 = 327 → 255
        assert_eq!(&dst[0..4], &[0, 92, 255, 255]);
    }
}
