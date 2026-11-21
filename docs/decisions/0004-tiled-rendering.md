# 0004 — Render the remote frame as 256×256 tile textures, in device-pixel units

- Status: accepted
- Date: 2026-11-21

## Context

PLAN §4 suggested one `GdkMemoryTexture` updated through `GdkMemoryTextureBuilder`
`set_update_texture`/`set_update_region`. That only limits the GPU *upload*: the builder
still needs the whole frame as a fresh, immutable `GBytes`, which costs a full 33 MB copy
per update at 4K. Reusing buffers that GTK still references would need `unsafe`
aliasing, and PLAN §9 keeps `unsafe` out of the widget crate.

## Decision

1. The framebuffer is split into 256×256 tiles (`transom-view-core::tiles`), each with
   its own `GdkMemoryTexture` (`B8G8R8X8`). Damage marks tiles dirty, and only dirty tiles
   are copied and rebuilt (256 KiB each). A typed character costs one tile.
2. Tiles are drawn inside `translate(snap) · scale(1/s)`, so every rectangle edge is an
   **integer device pixel**. With logical coordinates (e.g. 256 / 1.25 = 204.8) f32
   rounding put edges just off the grid, and GSK antialiased tile seams.
3. When the viewport ratio is exactly 1, tiles are plain **texture nodes**
   (`append_texture`). `append_scaled_texture` (texture-scale nodes) was observed to go
   through a logical-size offscreen and come out visibly blurred at 200%. Scaled
   viewports use `append_scaled_texture` with linear filtering.
4. The image origin is snapped to the device grid in *surface* coordinates (including
   the CSD shadow offset). The desktop request is `floor` of the snapped size (PLAN §3.1
   amended), so the remote never gets a pixel that would be half-covered.

## Verification

`scripts/view-check.sh` runs the demo in a headless mutter at a given monitor scale and
renders the window through GSK at device resolution. It then compares the result with the
framebuffer pixel by pixel. On 2026-11-21 all of 100%, 125%, 150%, 166.7% and 200%, a
window resize, and a 100% → 200% scale change mid-session were **100% identical**.

## Consequences

- Scaled (non-exact) views may show faint seams at tile edges with linear filtering.
  That's acceptable for the fallback Fit mode. Revisit with an offscreen pass if users
  notice.
- The zero-copy path (dmabuf / GL texture with sub-image updates) is still the Phase 6
  spike. Tiles are the safe baseline it must beat.
