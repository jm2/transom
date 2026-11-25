# 0006 — Zero-copy H.264: VA-API decode to dmabuf textures

- Status: accepted
- Date: 2026-11-22

## Context

FreeRDP's GDI decodes every graphics update into system memory. Transom then copies
damaged tiles into GDK memory textures (ADR 0004). For H.264 that costs:
- a CPU decode (OpenH264);
- a CPU YCbCr→RGB conversion;
- an upload per frame.

GPUs decode H.264 natively, and GTK (4.14+) can show a decoded frame directly as a
`GdkDmabufTexture`. PLAN Phase 6 asks for the zero-copy path.

## Decision

**Decoder: libavcodec's native H.264 decoder with VA-API** (Linux). It's found with
pkg-config at build time.
- If FFmpeg's development files are missing (or `TRANSOM_NO_HWDEC=1`), the build
  leaves it out (`cfg(transom_hwdec)`), and a stand-in module keeps the engine
  unchanged.
- **Runtime probe:** the decoder must exist (distribution "free" builds may only wrap
  OpenH264), support VA-API, and open a device. The probe runs once, and any failure
  means software decoding.
- **Why libavcodec:** one API that also has VideoToolbox (macOS) and D3D11VA
  (Windows), for later. It's LGPL and dynamically linked, which is compatible with
  transom-view's Apache-2.0.
- **Frames out:** frames are exported with `av_hwframe_map` to DRM PRIME. The fds are
  duplicated into a `DmabufFrame`, which keeps the VA surface reserved until GTK
  releases the texture.

**Where: wrapping FreeRDP's RDPGFX callbacks** (`gfxvideo.rs`). AVC420 surface commands
are decoded on the GPU instead of by FreeRDP. Each surface tracks the region where its
latest video frame, not system memory, holds the current pixels.
- **Commands that write** (other codecs, solid fill, surface-to-surface destination,
  cache-to-surface) take their area out of the region.
- **Commands that read, or may write only part of their rectangle** (surface-to-surface
  source, surface-to-cache, RemoteFX tiles, alpha) first copy the video pixels they
  need back into the surface. The copy-back is converted exactly like FreeRDP's
  integer BT.709 full-range decoder.
- **Delivery:** video layer changes ride on `EngineEvent::FrameUpdate`, so a frame's
  framebuffer damage and video change arrive together. When a graphics channel closes
  or opens, `EngineEvent::VideoReset` makes the view drop every layer: the surfaces
  they belonged to are gone. A server redirection does the same, and the engine then
  sends the layers that are still current again.
- **Scope:** only output-mapped, unscaled surfaces become layers. Scaled outputs and
  RemoteApp windows copy back and stay in software.

**Rendering:** the view draws, per surface, the whole frame texture clipped to each
rectangle of its region, over the tiles. Each rectangle is also clipped to the desktop
(or to the part of it a view shows), since the server's surface origin is checked only
against FreeRDP's 32766-pixel limit. It uses the same device-pixel rounding as the
tiles. The colour state is CICP BT.709 / sRGB transfer / full range: MS-RDPEGFX's
YCbCr.
- **Renderer:** only GTK's Vulkan renderer is used for this. It imports VA-API's tiled
  NV12 layouts. With the GL renderer, the builder still succeeds (the display
  advertises Vulkan's formats), but drawing reads the tiled buffer linearly, which is
  garbage.
- **Any other renderer, or a failed import:** the view sends
  `EngineCommand::VideoUnsupported`. The engine copies the video pixels back on its
  channel thread and decodes in software for the rest of the session, including after
  graphics-channel reconnects.

**AVC444 stays in software.** Showing it from the GPU would mean recombining two
streams in a shader, which GTK doesn't offer. Decoding only the main stream would show
subsampled chroma, visibly worse on coloured text.
- **Windows' choice:** both are advertised (with AVC420 alone FreeRDP only offers GFX
  8.1, and Windows then picks RemoteFX). In its default mode, Windows uses AVC420 for
  regions it classifies as video, which is exactly what zero-copy is for. With the
  "Prioritize H.264/AVC 444" policy, or in thin-client mode (which we don't request),
  it sends the whole desktop as AVC444.
- **Other servers:** xrdp and gnome-remote-desktop (with GPU encoding) send AVC420.

**`unsafe` in transom-view** (exception to PLAN §9's "FFI crates only"). One module,
`dmabuf.rs`, is the only place in the crate allowed `unsafe`, because gtk-rs marks
`DmabufTextureBuilder::set_fd`/`build` unsafe. The fds come from the frame, and the
frame is kept alive by the texture's release function.

**Portability (ADR 0002):**
- The core's dmabuf types are Unix-only, with an empty stand-in elsewhere.
- The view's dmabuf module is Linux-only.
- The engine's hooks need `cfg(transom_hwdec)`, which is only set on Linux.
- macOS (VideoToolbox → IOSurface) and Windows (D3D11) need GTK texture import APIs
  that GTK 4.16 doesn't have. They keep the software path.

## Verification

`scripts/hwdec-check.sh` runs on this machine's GPU (RX 6400, Mesa radeonsi with
RPM Fusion's H.264), in a container with headless mutter, against xrdp:
1. **`--video-decode verify`:** FreeRDP decodes every frame too.
   - **Decoders:** the VA-API frames, converted with FreeRDP's formula and its
     rectangle-relative chroma pairing, must match FreeRDP's pixels exactly: 0
     mismatches over every frame.
   - **On screen:** the rendered widget must show the layer, at PSNR ≥ 30 dB against
     FreeRDP's framebuffer. It measures 36–39 dB at 100/125/150/200%; the difference is
     the GPU's bilinear chroma upsampling at colour edges, with luma 1:1. A misplaced
     layer scores far lower.
2. **GL renderer:** the fallback engages, and the result is pixel-exact.
3. **Auto mode:** frames are decoded on the GPU with no fallback.

Also checked: Windows 11 (AVC444) stays pixel-exact with the hooks installed.

FreeRDP's own converter pairs chroma rows and columns from each rectangle's origin.
For a rectangle at an odd x or y, that gives one pixel of each pair the neighbouring
chroma sample. The read-back uses the correct 4:2:0 sampling instead.
