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
5. **CI** builds and tests `transom-view-core` on Linux, macOS and Windows from Phase 1.
   The full app is added on macOS and Windows once the widget is stable.

## Consequences

- Wayland-specific niceties (fractional-scale protocol, shortcut inhibition) stay
  behind GTK APIs, which degrade gracefully on other backends.
- macOS/Windows packaging (DMG, installer) is post-v0.1. The Balun/Tributary build
  scripts are the template.
