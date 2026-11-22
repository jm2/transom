// SPDX-License-Identifier: Apache-2.0
//! Hardware-decoded video frames (dmabufs) as GDK textures (ADR 0006).
//!
//! The only `unsafe` in this crate (ADR 0006 explains the exception): GDK's dmabuf
//! texture builder takes raw file descriptors, and `build` is unsafe because GDK can't
//! check them. We pass fds we own (in the `DmabufFrame`) and keep the frame alive
//! until GDK's release callback, so the fds and the buffer outlive the texture.
#![allow(unsafe_code)]

use std::os::fd::AsRawFd;
use std::sync::Arc;

use gtk::gdk;
use transom_view_core::video::DmabufFrame;

/// BT.709 matrix, full range, sRGB primaries and transfer: MS-RDPEGFX's YCbCr, and
/// the conversion FreeRDP's software decoder applies. `None` if this GTK can't
/// describe it (the caller then falls back to software decoding).
fn color_state() -> Option<gdk::ColorState> {
    thread_local! {
        static CS: Option<gdk::ColorState> = {
            let p = gdk::CicpParams::new();
            p.set_color_primaries(1);
            p.set_transfer_function(13);
            p.set_matrix_coefficients(1);
            p.set_range(gdk::CicpRange::Full);
            match p.build_color_state() {
                Ok(cs) => Some(cs),
                Err(e) => {
                    tracing::warn!("GTK can't describe BT.709 full-range video: {e}");
                    None
                }
            }
        };
    }
    CS.with(|cs| cs.clone())
}

/// A texture showing `frame`. Errors mean this display can't import it (no GPU
/// renderer, unsupported format or modifier).
pub(crate) fn texture(
    display: &gdk::Display,
    frame: Arc<DmabufFrame>,
) -> Result<gdk::Texture, String> {
    let cs = color_state().ok_or("no BT.709 colour state")?;
    let mut b = gdk::DmabufTextureBuilder::new()
        .set_display(display)
        .set_width(frame.width)
        .set_height(frame.height)
        .set_fourcc(frame.fourcc)
        .set_modifier(frame.modifier)
        .set_n_planes(frame.planes.len() as u32)
        .set_premultiplied(true)
        .set_color_state(Some(&cs));
    for (i, p) in frame.planes.iter().enumerate() {
        // SAFETY: the fd is owned by `frame`, which the release function below keeps
        // alive for as long as GDK uses the texture.
        b = unsafe { b.set_fd(i as u32, p.fd.as_raw_fd()) };
        b = b
            .set_offset(i as u32, p.offset)
            .set_stride(i as u32, p.stride);
    }
    let keep = frame.clone();
    // SAFETY: valid, owned fds (above), described by the frame's own layout; GDK
    // validates the format against the display and fails cleanly otherwise.
    unsafe { b.build_with_release_func(move || drop(keep)) }.map_err(|e| e.to_string())
}
