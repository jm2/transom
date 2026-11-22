# Transom + transom-view — RDP client and GTK4 display library

Handoff plan for coding-agent sessions. Read this whole file before writing code.
Names: **Transom** (the app, binary `transom`) and **transom-view** (the library; crates
`transom-view*`, GObject type `TransomView`, GIR namespace `TransomView`). Both crate names were
free on crates.io as of 2026-11-21. Trademark, Flathub app ID and domain checks are still open (§10).

---

## 0. Handoff prompt (paste into the first session)

> You are implementing **transom-view**, a GTK4 RDP display library, and **Transom**, a focused
> libadwaita RDP client built on it. The spiritual target is "Remmina, RDP-only, done right
> for modern GNOME." The defining requirement is **correct HiDPI**: a 200% remote Windows or
> gnome-remote-desktop session on a 200% (or fractional) GNOME host must be pixel-exact, not
> upscaled. Work in `~/transom` (Cargo workspace). Read `PLAN.md` (this file) fully, then
> start at the first unchecked phase. Follow the working rules in §9. At the end of each
> session, update §11 (Progress log) and tick completed checklist items.

---

## 1. Goals and non-goals

**Goals**
- Pixel-exact HiDPI and fractional scaling: request remote desktop in device pixels, send
  matching `DesktopScaleFactor`/`DeviceScaleFactor`, render 1:1.
- First-class interop with **gnome-remote-desktop (GNOME 50+)** and **Windows 10/11 / Server**.
- A reusable widget library usable from Rust, C, and Vala (GObject Introspection), so GNOME
  Connections / RDW / Field Monitor could adopt it.
- Linux/GNOME (Wayland) is the critical target, but keep the codebase ready for **macOS and
  Windows 11 builds of the same GTK4/libadwaita app**, the way the sibling projects Tributary
  and Balun ship. Never compromise the GNOME UX for them, but don't paint them out either. See
  `docs/decisions/0002-cross-platform.md`. Concretely:
  - `transom-view-core` and `transom-view-freerdp` contain no Linux-only code (FreeRDP 3 is
    found with pkg-config on all three OSes: distro, Homebrew, vcpkg/MSYS2).
  - Platform-specific behavior sits behind small seams with a per-OS module: secret storage
    (libsecret / Keychain / Credential Manager), keycode→scancode mapping, keyboard-shortcut
    inhibition, config/data paths (use GLib's user dirs, never hardcode XDG paths), and URI/MIME
    handler registration.
  - CI builds and tests `transom-view-core` on Linux, macOS and Windows from the start.

**Non-goals (do not add)**
- Other protocols (VNC, SPICE, SSH), plugin systems, zero-trust launchers, CLI-tool
  downloaders, monitoring. Large credential-backend matrices (the OS secret store only:
  libsecret on Linux, Keychain on macOS, Credential Manager on Windows).

## 2. Architecture

Cargo workspace, Rust 2024 edition:

| Crate | Kind | Responsibility |
|---|---|---|
| `transom-view-core` | lib, no GTK | Engine trait, session state machine, config model, geometry/scale math, `.rdp` file + `rdp://` URI parsing, cert-trust store (TOFU + system CA). Pure, heavily unit-tested. |
| `transom-view-freerdp` | lib | FreeRDP 3 backend implementing the engine trait via FFI. Owns the FreeRDP event thread. |
| `transom-view` | lib | `TransomView` GTK4 widget (subclass of `gtk::Widget`): rendering, input, cursor, clipboard bridge, resize/scale negotiation. |
| `transom-view-capi` | cdylib | C ABI + GIR + VAPI (librsvg is the precedent for Rust→C→GIR). Phase 5. |
| `transom` | bin | libadwaita app: connection list, profiles, credentials, tabs/windows, preferences. |

**Engine trait (sketch — refine in Phase 0):**
```rust
pub trait RdpEngine: Send {
    fn connect(&mut self, cfg: &SessionConfig, sink: EventSink) -> Result<()>;
    fn request_layout(&mut self, layout: MonitorLayout);   // Display Control
    fn send_input(&mut self, ev: InputEvent);
    fn clipboard(&mut self, op: ClipboardOp);
    fn disconnect(&mut self);
}
pub enum EngineEvent {
    Connected { desktop: Size, scale: ScaleFactors },
    FrameUpdate { surface: FrameRef, damage: Vec<Rect> },
    DesktopResized(Size), Pointer(PointerImage), PointerPos(Point),
    Clipboard(ClipboardEvent), AuthRequired(AuthRequest), CertificatePrompt(CertInfo),
    Redirect(RedirectInfo), Disconnected(Reason),
}
```
IronRDP may be added later as a second backend behind the same trait. **Do not** start with it:
it lacks CA validation and has incomplete client H.264 (AVC420 only) and redirection coverage.

## 3. HiDPI specification (the core requirement — implement exactly)

Let `s = gdk_surface_get_scale(surface)` (fractional, GTK ≥ 4.12; fall back to
`widget.scale_factor()`), and `(lw, lh)` = widget logical size.

1. **Requested desktop size** = `(floor(lw*s), floor(lh*s))` of the *snapped* widget size (amended
   2026-11-21: `round` could ask for a half-covered pixel at fractional scales), width forced even, clamped to
   MS-RDPEDISP limits (200–8192). Apply at connect (`FreeRDP_DesktopWidth/Height`) and in
   every Display Control monitor-layout PDU.
2. **Scale factors**: `DesktopScaleFactor = clamp(round(s*100), 100, 500)`;
   `DeviceScaleFactor` = nearest of {100, 140, 180} (a desktop factor is ignored by Windows
   unless the device factor is one of those). Send at connect (`FreeRDP_DesktopScaleFactor`,
   `FreeRDP_DeviceScaleFactor`) **and** in every monitor-layout update.
3. **Per-profile scale mode**: `MatchLocal` (default) | `Fixed(percent)` | `Legacy100`
   (logical-size desktop at 100%, stretched — for servers that ignore scale).
4. **Rendering**: the remote frame is a device-pixel texture drawn into a logical rect of
   `(w/s, h/s)` via `snapshot.append_texture` / `append_scaled_texture`. Use NEAREST filtering
   when the device→screen ratio is exactly 1, linear otherwise. Snap the origin to whole
   device pixels so fractional scales stay sharp.
5. **Fit modes** (when the remote can't resize): `Fit` (letterbox, preserve aspect),
   `Scroll` (1:1 with scrollbars). All math in device pixels.
6. **Input mapping**: widget logical coords → `(x*s - offset_x) / fit_ratio` in remote
   pixels; clamp to desktop. Cover pointer, wheel (incl. hi-res/smooth scroll, chunked to
   ±120 per event), and touch.
7. **Cursor**: remote pointer bitmaps arrive at device resolution. Build them with
   `gdk_cursor_new_from_callback` (GTK ≥ 4.16) so GTK picks the right scale, hotspot ÷ s.
   Fallback: downscale yourself.
8. **Scale changes**: listen for `notify::scale` on the surface and for monitor changes;
   debounce (~150 ms) and re-send the monitor layout. Debounce resize too; don't loop on odd
   dimensions (compare against the rounded, clamped request, not the raw allocation).

**Put all of 1, 2, 5 and 6 in `transom-view-core::geometry` as pure functions with property tests.**

## 4. Rendering pipeline and performance

- Phase 1: FreeRDP GDI/GFX writes to a system-memory buffer. Upload with
  `GdkMemoryTextureBuilder` using `set_update_texture` + `set_update_region` (GTK ≥ 4.16) so
  only damaged regions are re-uploaded. Never copy the full 4K frame per update.
- Negotiate the GFX pipeline with AVC444 → AVC420 → RemoteFX → planar fallback. Use FreeRDP's
  FFmpeg path with VA-API hardware decode where available.
- Later (Phase 6 spike): zero-copy decoded frames as `GdkDmabufTexture` (GTK ≥ 4.14) and/or
  `GtkGraphicsOffload`. This needs custom handling of FreeRDP's H.264 surface output.
- Budget: 3840×2160 at 200%, idle desktop < 5% of one core; full-screen video playback
  should not drop below 30 fps on antec.

## 5. gnome-remote-desktop interop requirements

- All three grd modes: remote assistance, single-user headless, headless remote login
  (GDM). Remote login hands off from the GDM session to the user's session with an
  **RDP Server Redirection PDU**: redirection must reconnect transparently, carrying the
  routing token and credentials.
- Scale: grd 50 forwards the client's scale (PipeWire `org.gnome.scale` /
  `org.gnome.preferred-scale` tags). Verify §3 gives a sharp grd session at 200%.
- Channels to support against grd: GFX (H.264 via grd's VA-API/Vulkan encoders), Display
  Control, clipboard incl. files, audio out/in, and **camera redirection (MS-RDPECAM)** in
  Phase 4.
- Auth: NLA with grd's configured credentials. Kerberos where configured (Phase 4).
- Cert trust: show the SHA-256 fingerprint in the same format `grdctl` prints, for
  first-use comparison.

## 6. App (Transom) scope

- libadwaita: `AdwApplicationWindow`, connection list with search, `AdwTabView` for sessions,
  each session detachable to its own window, true fullscreen (with keyboard-shortcut
  inhibition via `gtk::Window` / Wayland keyboard-shortcuts-inhibit), and a reveal-on-hover
  header in fullscreen.
- Profiles stored as TOML in `<user config dir>/transom/` (`$XDG_CONFIG_HOME` on Linux, via
  `glib::user_config_dir()`); secrets in the OS secret store only (**libsecret** on Linux).
  Import/export `.rdp` files; register as the handler for `application/x-rdp` and `rdp://`.
- Per-profile: host/port, gateway (RD Gateway, Phase 4), credentials, scale mode, fit mode,
  multimon, clipboard direction, audio, security (NLA/TLS/RDP), cert policy.
- Send Ctrl+Alt+Del, a toggle for passing through local keyboard shortcuts, screenshot.
- Flatpak (`org.gnome.Platform`), bundling FreeRDP 3 + FFmpeg (VA-API). Also native
  packaging scripts for Arch (AUR) and Fedora (COPR). *Done: `build-aux/flatpak`,
  `build-aux/rpm/transom.spec`, `build-aux/arch/PKGBUILD` + `scripts/dist.sh`; RPM and Arch
  packages test-built (with tests) in clean containers.*

## 7. Phases

Each phase ends with its Definition of Done (DoD) demonstrated and logged in §11.

### Phase 0 — Spikes and decisions (≤1 week)
- [x] Pick the FreeRDP 3 binding strategy: an existing Rust binding (e.g. the one RDW uses)
      vs. `bindgen` over the system FreeRDP 3. Write up the choice in `docs/decisions/0001-bindings.md`.
- [ ] Minimal binary: connect to Windows (antec-win) with FreeRDP, dump one frame to PNG.
- [x] Confirm GTK version floor (target ≥ 4.16 for memory-texture updates + cursor callback);
      document fallbacks if a distro is older.
- [x] Freeze the engine trait v0 (`transom-view-core::engine`).
- **DoD:** decisions recorded; the spike connects to both Windows and grd.

### Phase 1 — Widget MVP with correct HiDPI (2–3 weeks)
- [x] `transom-view-core::geometry` with property tests (§3 items 1, 2, 5, 6).
- [ ] FreeRDP backend: connect, NLA, TOFU cert (system-CA validation first; TOFU fallback
      prompt), GFX negotiation, frames, pointer, keyboard (scancode via XKB keycode→scancode),
      mouse, wheel.
- [x] `TransomView` rendering per §3–§4 with damage-region uploads (tiled, ADR 0004).
- [x] Display Control dynamic resize with scale factors; debounce; scale-change handling.
- [x] Demo binary `transom-view-demo host user` (`cargo run -p transom-view --example transom-view-demo`).
- **DoD:** on a 200% GNOME host, Windows 11 at 200% and grd at 200% both render pixel-exact
  (verified with a 1-px checkerboard / ClearType text test pattern and a screenshot diff),
  survive window resize and a move between 100% and 200% monitors, and keep cursor size correct.

### Phase 2 — Transom app v0.1 (2 weeks)
- [x] Connection list, profiles (TOML), libsecret, quick connect, tabs, fullscreen.
- [x] `.rdp` import, `rdp://` URI, desktop file + MIME registration.
- [x] Cert-trust UI (fingerprint, remember, changed-cert warning that blocks by default).
- [x] Flatpak manifest building in CI (`build-aux/flatpak/`, GNOME 51; built and smoke-tested locally).
- **DoD:** daily-drivable against antec-win and a grd host.

### Phase 3 — Channels (2–3 weeks)
- [x] Clipboard: text, HTML, images, **files** (lazy temp-dir staging, ADR 0005;
      `scripts/clipboard-check.sh`).
- [x] Audio out (rdpsnd) and in (audin) via PipeWire (`scripts/audio-check.sh`).
- [x] grd remote-login **server redirection** end to end (`scripts/login-check.sh`, systemd+GDM container).
- [x] Multi-monitor (span selected monitors, per-monitor scale in the layout PDU): one fullscreen
      view per monitor sharing one session (`scripts/multimon-check.sh`, 200% + 100%).
- **DoD:** grd remote login via GDM → user session works without a second prompt; file copy
  works in both directions with Windows.

### Phase 4 — Enterprise-shaped features (3–4 weeks)
- [x] RD Gateway (HTTP/RPC-over-HTTP + WebSocket transport): `SessionConfig.gateway`, editor
      group, gateway credential prompt; `scripts/gateway-check.sh` connects to the Server 2025 VM
      through its own RD Gateway (NTLM, HTTP transport; CAP/RAP events checked).
- [x] Kerberos/NLA with the system Kerberos ccache (`scripts/kerberos-check.sh`: MIT KDC + grd,
      ticket-only login); smartcard passthrough (optional, not done).
- [ ] RemoteApp (RAIL) windows as separate GTK windows (stretch).
- [ ] Camera redirection (MS-RDPECAM) via PipeWire camera portal. *Blocked on test hardware:
      Fedora's FreeRDP client lacks RDPECAM, so it needs our own channel + H.264 encoding, and
      grd decodes camera streams on the GPU; verify against Windows 11 or a GPU host.*
- **DoD:** connect through a Windows RD Gateway to a domain host using Kerberos. **Met
      2026-11-22:** `scripts/ad-check.sh`. Test domain `transom.test` (`scripts/ad-setup.sh`:
      Server 2025 DC + gateway, Windows 11 member, over a rootless VM-to-VM network). `kinit`,
      then no password: gateway (HTTP Negotiate) and NLA both use Kerberos, and alice's
      desktop session opens through Remote Credential Guard (MS-RDPEAR; needs a FreeRDP built
      with the channel, as the Flatpak is; see docs/known-issues.md for the fallbacks).
      Verified from the Windows side: DC ticket events, gateway events, Kerberos network logon
      and interactive logon on the target.

### Phase 4b — Device redirection
Every device type is opt-in per profile and per device, with explicit consent (a
remote host gets real hardware). Each one degrades gracefully: when the server refuses
or disables a channel (policy, missing role, server type), Transom shows the device as
"not available on this server" with the reason, and the session carries on without it.
It never fails the connection. Probe what the server announced, not what we asked
for, and test refusals as well as success.
- [ ] Capability reporting: per-session list of the redirections the server accepted or
      refused (static/dynamic channel open results), shown in the session info popover.
- [x] Drives (MS-RDPEFS): user-chosen folders via the file chooser (portal-friendly);
      `scripts/devices-check.sh` reads and writes `\\tsclient\transom` on Windows 11, and a
      server-side refusal leaves the session and other devices working. Read-only: not
      supported by FreeRDP's drive channel.
- [x] Smart cards (MS-RDPESC) via PC/SC: devices-check runs pcscd with a virtual CAC card
      (vsmartcard vpcd + virt_cacard) on the client; Windows 11 and Server 2025 list its
      readers in the session.
- [x] Printers (MS-RDPEPC): all local CUPS printers; devices-check prints from Windows to a
      throwaway CUPS-PDF printer and finds the text in the PDF. (Print portal for Flatpak:
      todo.)
- [~] Serial ports (MS-RDPEFS): explicit device paths (config/.rdp only); Windows accepts
      the port but doesn't expose it (docs/known-issues.md).
- [ ] Media Transfer Protocol (MTP) and Picture Transfer Protocol (PTP) devices
      (MS-RDPEPNP): FreeRDP has no client, so it would be our own channel. Lowest priority.
- [ ] Generic USB (MS-RDPEUSB / URBDRC, FreeRDP's libusb backend): device picker with
      hotplug, VID:PID allow/deny lists, never offer HID keyboards/mice or the device the
      session is using. Requires udev access (document a uaccess rule; Flatpak needs
      `--device=all`). Windows client: needs WinUSB bound to the device. macOS: only
      devices without a kernel driver. Server side needs Windows 10/11 Pro+ or RDSH; on
      refusal, fall back to the class channel above when one fits (drive, smartcard,
      camera, audio). Test: a virtual USB device (dummy_hcd + configfs gadget, needs root
      once) or a spare stick, against both VMs.

### Phase 5 — Library packaging for other consumers (1–2 weeks)
- [x] `transom-view-capi`: stable C ABI, GObject type `TransomView`, GIR + VAPI generation,
      pkg-config file, ABI versioning (`scripts/build-gir.sh`, libtransom-view-0.so.0).
- [x] A Vala example app proving it's consumable by Connections (plus Python; both connect to grd).
- **DoD:** a GIR-based Python or Vala sample shows a session.

### Phase 6 — Performance and polish (ongoing)
- [x] Zero-copy H.264 (ADR 0006): AVC420 decoded with VA-API (libavcodec) to dmabufs, shown
      as GdkDmabufTexture layers over the tiles; read-back keeps FreeRDP's surfaces correct
      for mixed codecs; fallback to software when the renderer can't import (non-Vulkan) or
      decoding fails. `scripts/hwdec-check.sh` (RX 6400 vs xrdp): decoder bit-exact vs
      FreeRDP on every frame, on-screen PSNR 36–39 dB at 100–200%, fallback pixel-exact.
      AVC444 (Windows' full-desktop mode) stays in software. Latency tooling done
      (`scripts/latency-check.sh`: 26–30 ms median vs grd, 31 ms vs Windows).
- [ ] GraphicsOffload (direct scanout of the video layer); AVC444 on the GPU (needs a
      shader to recombine the two streams).
- [x] Accessibility (screen-reader labels on all chrome, high-contrast): AT-SPI audit
      (`scripts/a11y-check.sh`); Ctrl+Alt+Home moves focus out of the session. High contrast
      comes from libadwaita (no custom colours).
- [ ] Optional IronRDP backend behind a feature flag.

## 8. Test matrix

| Server | Host | Must pass |
|---|---|---|
| Windows 11 Pro 24H2+ | throwaway eval VM on antec (needs SVM enabled in BIOS); antec-win only for manual checks | Phases 1–3 |
| Windows Server 2022/2025 + RDS/Gateway | VM | Phase 4 |
| gnome-remote-desktop 50+ (all 3 modes) | headless: `scripts/test-servers.sh` container; remote-login/assistance: Fedora VM | Phases 1–3 |
| xrdp 0.10.x | VM | Smoke test only |
| macOS server (`macrdp`, sibling project) | mbp16-m1max | When available |

Local hosts: develop on **antec** (Fedora). **No test credentials on primary machines** (antec, antec-win): automated testing uses throwaway containers/VMs with random credentials. Test 100%, 125%, 150%, 200% and mixed-scale dual
monitors. CI: unit + property tests, clippy `-D warnings`, rustfmt, `cargo deny`, Flatpak build.
Integration tests against a headless grd in a VM are optional, behind `--features it`.

## 9. Working rules for agents

- Run `scripts/verify.sh` (fmt, clippy -D warnings, tests, cargo-deny) before every commit;
  create it in Phase 0.
- `unsafe` lives **only** in `transom-view-freerdp` (FFI) and `transom-view-capi`. Every `unsafe` block
  gets a `// SAFETY:` comment.
- No new dependencies without a one-line justification in the commit message. Prefer
  well-maintained crates; no network access at runtime except the RDP connection itself.
- Secrets: never log credentials. Use `secrecy::SecretString`; never pass passwords on argv
  or in environment variables.
- Cert policy default = **verify** (system CA, then TOFU prompt). "Ignore certificate" exists
  only as an explicit per-profile opt-in with a warning.
- Small, reviewable commits; Conventional Commits style. Record architectural decisions in
  `docs/decisions/NNNN-*.md`.
- When blocked on a design choice listed in §10, stop and ask rather than guessing.

## 10. Open decisions (for John-Michael)

1. App ID and publishing names: proposed `io.github.jm2.Transom`; confirm the GitHub org/repo
   (`jm2/transom`?), reserve the `transom` and `transom-view*` crate names, and do a trademark /
   Flathub / domain check before the first public release.
2. ~~Licensing~~ **Decided 2026-11-21 (revised):** `transom-view*` libraries
   **Apache-2.0** (`LICENSE-APACHE`, no MIT); the `transom` app **GPL-3.0-or-later**
   (`COPYING`). FreeRDP (Apache-2.0) fits both.
3. Upstream ambitions: if GNOME Circle or adoption by Connections is a goal, decide how
   AI-assisted contributions square with GNOME's policy *before* public release.
4. ~~Minimum versions~~ **Decided 2026-11-21:** GTK 4.16 / libadwaita 1.6 / FreeRDP 3.15 /
   Rust 1.98 (edition 2024). Legacy-free, matching Tributary/Balun (ADR 0003).
5. ~~Cross-platform scope~~ **Decided 2026-11-21:** keep Transom itself portable to macOS
   and Windows 11 (same GTK app, like Tributary/Balun). Linux stays the priority; per-OS
   packaging comes after v0.1. See `docs/decisions/0002-cross-platform.md`.

## 11. Progress log

| Date | Session | Phase | Summary | Next |
|---|---|---|---|---|
| 2026-11-21 | 1 | 0 → 1 | Relicensed everything GPL-3.0-or-later. Cross-platform (macOS/Win11, Tributary/Balun-style) adopted as a design constraint (ADR 0002). Workspace + `verify.sh` + cargo-deny + CI (Linux full, macOS/Windows core). ADR 0001: own bindgen over system FreeRDP 3 (≥ 3.10). ADR 0003: GTK 4.16 / adw 1.6 floor (proposed). Core: geometry (§3.1/2/5/6, 24 unit+property tests), engine trait v0, config, cert fingerprints. FreeRDP backend: context, settings, TLS cert callback → `CertificatePrompt`, NLA auth, GDI damage → `SharedFrame`, DesktopResize, Display Control layouts (200 ms rate limit), pointer, keyboard/mouse/wheel input. Spike reaches antec-win: cert prompt OK (fingerprint `FE:ED:C4:52:…`), bad password → `AuthenticationFailed`. Headless grd 51 test container (`scripts/test-servers.sh`): spike frames at 100% (1280×800), 150% (1920×1200, 150/140) and 200% (2560×1600, 200/180), all honoured by grd and rendered at the requested scale. KVM unavailable on antec (SVM disabled in BIOS) → Windows VM pending. | Spike frame PNG with real creds (antec-win + grd). `TransomView` widget. Keycode→scancode tables. TOFU store. |
| 2026-11-21 | 1b | 1 | `TransomView` widget: tiled rendering in device px (ADR 0004), origin snapping, Display Control renegotiation (150 ms debounce), surface-scale tracking, keyboard (evdev/macOS scancode tables in core; Unicode fallback), mouse incl. back/forward, hi-res wheel, remote cursor via `Cursor::from_callback`, focus lock-sync and key release, clean `shutdown()`. Demo binary. `scripts/view-check.sh`: headless mutter + GSK render vs framebuffer: **pixel-exact at 100/125/150/166.7/200%, after window resize and after a 100→200% scale change** against grd 51. Found+fixed: texture-scale-node blur at 200%, fractional tile seams, half-covered-pixel request (§3.1 now floor), exit-time SIGSEGV. | Windows VM (needs SVM in BIOS) for the Windows half of Phase 0/1 DoD; cursor-size check; TOFU trust store; input tests; Windows keycode path. |
| 2026-11-21 | 1c | 2 | Pins raised legacy-free to match Tributary/Balun: Rust 1.98 (ed. 2024), GTK 4.16 / adw 1.6, FreeRDP ≥ 3.15. Core: `.rdp` (UTF-16/UTF-8) + `rdp://` (both flavours), TOML profiles (one file each, atomic 0600), TOFU `TrustStore`. App v0.1: connection list + search + quick connect, profile editor, `.rdp` import, `rdp://`/`.rdp` open, tabs (detachable), fullscreen with hover-reveal header and system-shortcut inhibition, app shortcuts handed to the remote while a session is focused (Ctrl+Alt+Enter/Break fullscreen, Ctrl+Alt+End = CAD stay local), cert dialog (changed cert blocks by default), credentials dialog with libsecret “remember”, screenshot, reconnect. Desktop/MIME/metainfo validated; `scripts/install-local.sh`. Headless screenshots via `scripts/app-shot.sh` (empty, list, cert prompt, live 125% session). | Flatpak manifest + CI; xrdp container; grd remote-login container (redirection); input tests; Windows VM once SVM is on. |
| 2026-11-21 | 1d | 3–6 | Licences: transom-view* Apache-2.0, app GPL-3.0-or-later. **Phase 3 done (non-Windows):** clipboard text/HTML/images/files (ADR 0005), audio out + mic, grd remote login with Server Redirection (systemd+GDM container), multi-monitor (per-monitor scale, follower views). Input verified end to end (engine + widget path). H.264 advertised when a decoder works. **Phase 4:** Kerberos SSO (KDC container). **Phase 5 done:** C API, GIR/typelib/VAPI, Python + Vala samples. **Phase 6:** AT-SPI a11y audit + Ctrl+Alt+Home focus escape; latency 26–30 ms median (loopback). Packaging: Flatpak (GNOME 51, smoke-tested), RPM + PKGBUILD (built in clean containers). Fixed: GIO mangling Microsoft rdp:// links, redirect false positive. Known server issues in docs/known-issues.md. | **VM/hardware:** Windows VM (SVM) for all Windows halves, RD Gateway, RemoteApp; camera (needs H.264 encode + GPU decode on grd, or Windows); dmabuf spike (hardware H.264); macOS/Windows builds (seams in ADR 0002: secrets, Windows keycodes). Optional: IronRDP, smartcard. |
| 2026-11-22 | 1e | 4 | **Windows VMs** (Win11 25H2, Server 2025; KVM, rootless QEMU): view/resize/multimon pixel-exact, input, clipboard incl. files, audio, latency (31 ms) all pass against both; RD Gateway. **Phase 4 DoD met:** AD test domain + Kerberos SSO through the gateway to a domain member, desktop via Remote Credential Guard (RDPEAR), with fallbacks for FreeRDP without RDPEAR, hosts without the policy, and grd. Phase 4b (device redirection: class channels + generic USB) planned. | Zero-copy H.264 (VA-API H.264 decode now available: RX 6400 + mesa freeworld); Phase 4b; RemoteApp. |
