# Reviewing the FreeRDP engine's `unsafe` code

**Status: not yet reviewed by a person.** `crates/transom-view-freerdp` has about 210
`unsafe` sites: blocks, functions, C callbacks and `Send`/`Sync` impls. Every block
carries a `// SAFETY:` note, every `unsafe fn` a `# Safety` contract, and every site
was written with AI assistance. Treat the notes as claims to check, not as evidence.
This guide orders the review by risk: how directly data a server controls reaches the
code. The rest of the workspace denies `unsafe` (`unsafe_code = "deny"`); the C API
and macOS crates are small and not covered here.

`scripts/unsafe-inventory.py` prints every site by file and function, with line
numbers, and flags any without a note. For each site, check two things:
- the note states the real precondition;
- the code around it actually establishes that precondition.

Findings go in issues linked from [#18](https://github.com/jm2/transom/issues/18).

## What everything rests on

1. **The context and its lifetime.**
   - FreeRDP allocates `ContextSize` bytes for a `TransomContext`: its `rdpClientContext`, then a pointer to `State`.
   - `state(ctx)` casts back to it.
   - `run()` keeps `State` alive until after `freerdp_client_context_free`, so no callback can see a dangling pointer.
2. **Threads.**
   - The engine thread runs the connection and its event loop.
   - FreeRDP runs some channel callbacks on its own threads, and serialises paints.
   - Everything a callback touches in `State` must be safe to share: a `Mutex`, an atomic, or data that never changes.
3. **Buffers FreeRDP owns.** A Rust slice over FreeRDP memory must use exactly its bounds:
   - the GDI primary buffer is `stride × height`;
   - a GFX surface is `scanline × height`, with `scanline ≥ width × 4`;
   - a PDU array is its pointer plus its count field, allocated by FreeRDP's parser.

   Every rectangle that comes from a server is clipped to the surface before anything is indexed (`clip`, `Rect::clip`, `intersect`), and `rect16` saturates rather than wrapping.
4. **Strings.** `CStr::from_ptr` needs a NUL terminator. Use it only on strings FreeRDP produced (settings, error names, parsed names), never on raw PDU bytes.
5. **Ownership across the boundary.** Memory is freed by the side that allocated it:
   - credential strings FreeRDP owns are zeroed and then replaced (`replace_cstring`, `wipe`);
   - pointer images are `Box::into_raw` / `Box::from_raw` pairs;
   - FFmpeg frames go back to FFmpeg;
   - dmabuf file descriptors have exactly one owner.
6. **Signatures and layouts.**
   - Each `extern "C"` function must match the prototype the C side expects. PubSub handlers are registered through a variadic function, so the compiler checks nothing there.
   - `#[repr(C)]` mirrors must match the C layout. `StateChangedArgs` mirrors FreeRDP 3.27's `StateChangedEventArgs`; the event's recorded size guards it.

## Tier 1: server data reaches the code

| Area | Functions | Check especially |
|---|---|---|
| GFX surfaces, H.264 layers (`gfxvideo.rs`) | `surface_command`, `solid_fill`, `surface_to_surface`, `surface_to_cache`, `cache_to_surface`, `create_surface`, `delete_surface`, `reset_graphics`, `map_output`, `map_scaled_output`, `map_window`, `map_scaled_window`, `end_frame`, and the helpers `read_back`, `invalidate`, `verify`, `before_write`, `bound`, `flatten`, `through` | Surface IDs are looked up, never trusted. Rectangles and points are clipped before indexing. Counts: an AVC420 command with more than `MAX_REGION_RECTS` region rectangles is decoded but its pixels are skipped; `MAX_COMMAND_RECTS` (per command) and `MAX_SURFACE_RECTS` (per surface) only switch strategy, sending the video pixels to the framebuffer instead of refusing. `read_back` writes into FreeRDP's surface, so it needs `width × 4 ≤ scanline`, and adds to the surface's invalid region the way FreeRDP does (`invalidate`: its bounding box past 256 rectangles). FreeRDP checks a surface's output origin only against 32766, so the renderer clips layers to the desktop. The bitstream slice is bounded by FreeRDP's parsed length. Hardware decoders are capped (`MAX_HW_DECODERS`). |
| Screen updates (`engine.rs`) | `end_paint`, `copy_rects`, `begin_paint`, `desktop_resize`, `refuse`, `refuse_oversized`, `clear_primary`, `primary_hwnd` | Invalid regions are clipped to the GDI size. `ninvalid` stays within the region array FreeRDP allocated; past `MAX_DAMAGE_RECTS` only the bounding rectangle FreeRDP keeps is copied. The desktop size comes from the server: `refuse_oversized` ends the session before GDI or the shared frame allocate one past the cap, and the frame is allocated fallibly. After `gdi_resize`, the primary buffer is `stride × height`; a failed `gdi_resize` leaves `primary` NULL, which `primary_hwnd` reports as null, and the session is ended. |
| Pointer images (`engine.rs`) | `pointer_new`, `pointer_set`, `pointer_set_null`, `pointer_set_default`, `pointer_free`, `pointer_set_position` | The `width × height × 4` allocation is sized by the server: confirm FreeRDP caps pointer sizes before this point. The mask lengths are handed to FreeRDP's converter. The image `Box` is created in `pointer_new` and freed exactly once in `pointer_free`. |
| Clipboard (`cliprdr.rs`) | `server_format_list`, `server_format_list_response`, `server_format_data_request`, `server_format_data_response`, `server_file_contents_request`, `server_file_contents_response`, `server_capabilities`, `monitor_ready` | Format counts and names come from FreeRDP's parser (names NUL-terminated). The `dataLen` and `cbRequested` slices are bounded. A file-contents answer is taken only for a stream ID we asked for (`file_pending`), and only up to the length we asked for; the list indices and offsets a server asks for are checked against the file list that was offered, and offsets limited to each file's offered size, in transom-view's clipboard bridge. Sizes are checked before anything is copied (format data against `MAX_CLIP_BYTES`); FreeRDP itself has already reassembled the PDU at the length the server stated. |
| Certificates, credentials (`engine.rs`) | `verify_certificate_ex`, `verify_changed_certificate_ex`, `verify_cert`, `authenticate_ex` | The server's certificate fields arrive as C strings. Credential buffers are replaced and zeroed with FreeRDP's allocator, and a reply is never written into a buffer FreeRDP didn't size for it. |
| Channels, events (`engine.rs`) | `on_channel_connected`, `on_channel_disconnected`, `disp_caps`, `on_state_change` | A channel's interface pointer is cast only after its name matches. The server's Display Control limits are only logged (`disp_caps`), not applied: the layouts sent stay within MS-RDPEDISP's own limits (200–8192 pixels per monitor, at most 16 monitors) whatever the server announced. The event layout mirror is guarded by the size check. |
| Hardware decoding (`hwdec.rs`) | `decode`, `download`, `export`, `get_format`, `size` | Decoded planes are `linesize × rows`, with `ceil(height / 2)` chroma rows. `get_format` walks a terminated list. dmabuf export: descriptor and plane counts, and who owns the file descriptors. |

## Tier 2: lifetimes, threads and ownership

There's no server data here, but a mistake is still memory corruption.

| File | Functions |
|---|---|
| `engine.rs` | `client_new` (entry points, `ContextSize`), `run`, `session`, `connect_and_loop`, `event_loop` (64 handles: the waker, then up to 63 from FreeRDP), `state`, `pre_connect` / `post_disconnect` (subscribe and unsubscribe in pairs), `post_connect`, `connect`, `wake`, `abort`, `replace_cstring`, `wipe`; `impl Send`/`impl Sync for FreeRdpWaker` and its `Drop` (`drop`) |
| `cliprdr.rs` | `attach`, `state_of` |
| `gfxvideo.rs` | `attach`, `detach`, `state_of`, `with_hooks`, `fall_back_to_software`, `unmappable`. The hooks run on the graphics channel's thread. `attach` replaces FreeRDP's function pointers and nothing restores them: that is harmless only because FreeRDP's GDI sets every one again whenever a graphics channel opens (before `attach`, which won't wrap its own hooks), and a hook that finds no state (after `detach`, or once GDI has cleared `custom`) returns without doing anything. |
| `hwdec.rs` | `new`, the `Drop` impls (`drop`), `impl Send for H264Decoder` and `HwFrame` |

For each, check:
- every stored raw pointer outlives its uses;
- every `Send`/`Sync` impl is true, given what actually crosses threads and what is locked.

## Tier 3: plain calls into FreeRDP and FFmpeg

`apply_settings`, `add_devices`, `send_input`, `send_ctrl_alt_del`, `send_layout`,
`classify_error`, `rcg_refused`, `freerdp_version`, `h264_available`,
`rdpear_available`, `cstr_lossy`, `cstr_opt` (`engine.rs`); `set_local_formats`,
`send_format_list`, `request`, `reply`, `file_request`, `file_reply` (`cliprdr.rs`);
`available`, `cstr` (`hwdec.rs`).

Check that each argument is valid for its call: pointers non-null, `CString`s kept
alive across the call, and lengths matching their buffers.

## Tools

- **Nothing in CI yet.** No sanitizer or fuzzing runs on this code today.
- **AddressSanitizer:** build with `RUSTFLAGS=-Zsanitizer=address cargo +nightly build --target x86_64-unknown-linux-gnu -p transom-view-freerdp --example spike`. Run the spike against the test servers (`scripts/test-servers.sh up`, `scripts/winvm.sh`) with the clipboard, drives and H.264 in use. For coverage inside FreeRDP too, build FreeRDP with its `WITH_SANITIZE_ADDRESS` option.
- **Fuzzing:** the pure Rust code next to the `unsafe` sites suits `cargo fuzz` (rectangle clipping, the region arithmetic in `transom-view-core::video`, clipboard format parsing in `transom-view-core::clipboard`). FreeRDP itself is fuzzed by OSS-Fuzz.
- **Miri** can't run the FFI.

## Recording the review

Review a tier, or an area within one, in a pull request that adds the reviewer and
date below. File what needs fixing as issues.

| Area | Reviewed by | Date |
|---|---|---|
| (none yet) | | |
