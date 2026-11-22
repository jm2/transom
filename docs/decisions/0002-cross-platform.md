# 0002 — Keep Transom portable to macOS and Windows 11

- Status: accepted
- Date: 2026-11-21

## Context

GNOME on Wayland is the primary target. The sibling projects Tributary and Balun ship
the *same* GTK4/libadwaita application on Linux, macOS and Windows, with per-OS
packaging (Flatpak/RPM/AUR, DMG, Windows installer). We want that option for Transom
without slowing down the GNOME work.

## Decision

1. **One app, one toolkit.** There are no separate native frontends. Transom is a GTK4 +
   libadwaita app on every OS.
2. **No Linux-only code in `transom-view-core` or `transom-view-freerdp`.** Both crates
   must build on all three OSes. FreeRDP 3 is cross-platform and found with
   pkg-config (ADR 0001).
3. **Platform seams.** Each OS-specific behaviour gets a small trait or function with a
   `#[cfg(target_os)]` module per OS. A missing implementation is a documented no-op or
   a fallback, never a compile error:

   | Seam | Linux | macOS | Windows |
   |---|---|---|---|
   | Secret storage | libsecret | Keychain | Credential Manager |
   | Hardware keycode → RDP scancode | evdev (XKB keycode − 8) | macOS virtual keycode table | Windows scancode (from GDK) |
   | Keyboard-shortcut inhibition | `gdk::Toplevel::inhibit_system_shortcuts` (Wayland/X11) | best effort | best effort |
   | Config/data paths | `glib::user_config_dir()` (XDG) | same API | same API |
   | `rdp://` / `.rdp` handler | desktop file + MIME | `Info.plist` | registry (installer) |

   The keycode→scancode tables are pure data, so they live in `transom-view-core`
   (testable everywhere). Only the choice of table is per-OS.
4. **HiDPI math is toolkit-neutral.** GTK on macOS reports integer scales (1 or 2), and on
   Windows it can report fractional scales. `transom-view-core::geometry` already treats
   scale as an arbitrary positive `f64`.
5. **CI** builds and tests the whole workspace on Linux, macOS (arm64) and Windows
   (x64, arm64), and packages the app on each (since 2026-11-22).
6. **Toolchains (as Tributary/Balun):**
   - **Windows:** MSYS2 CLANG64/CLANGARM64, which packages GTK 4, libadwaita, FreeRDP 3
     and FFmpeg for both architectures. Rust targets `*-pc-windows-gnullvm`. Not vcpkg:
     its MSVC FreeRDP would clash with the MSYS2 GTK stack.
   - **macOS:** Homebrew, arm64 only.
   - **Passwords:** keyring-core's native stores on macOS and Windows; libsecret stays
     on Linux, because it works through the Secret portal in Flatpak.

## Consequences

- Wayland-specific niceties (fractional-scale protocol, shortcut inhibition) stay
  behind GTK APIs, which degrade gracefully on other backends.
- macOS/Windows packaging follows the sibling scripts, in a leaner form:
  - `scripts/build-macos.sh` makes a relocatable, ad-hoc-signed `.app` and `.dmg`;
  - `scripts/build-windows.ps1` makes a portable zip and an Inno Setup installer;
  - `docs/packaging.md` has the full matrix.
- Not yet: Developer ID signing and notarisation, and Authenticode (neither sibling
  has them either).
