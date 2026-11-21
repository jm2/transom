# 0003 — GTK and libadwaita version floor

- Status: proposed (PLAN §10.4 awaits confirmation)
- Date: 2026-11-21

## Decision

Build against **GTK 4.16** and **libadwaita 1.6** (`gtk4` crate feature `v4_16`, `libadwaita`
feature `v1_6`). That's the first GTK with:

- `GdkMemoryTextureBuilder` with `set_update_texture` / `set_update_region`, for
  damage-only uploads (PLAN §4).
- `gdk::Cursor::from_callback`, for scale-aware remote cursors (PLAN §3.7).
- Fractional `gdk::Surface::scale()` (since 4.12) and `GdkDmabufTexture` (since 4.14).

## Distro check (2026-11)

| Distro | GTK | libadwaita | Native build? |
|---|---|---|---|
| Fedora 45 (dev host *antec*) | 4.24 | 1.10 | yes |
| Fedora 43/44, Arch | ≥ 4.20 | ≥ 1.8 | yes |
| Debian 13 | 4.18 | 1.7 | yes |
| Ubuntu 24.04 LTS | 4.14 | 1.5 | no: use the Flatpak |
| GNOME Platform 49+ (Flatpak) | ≥ 4.20 | ≥ 1.8 | yes |
| macOS (Homebrew), Windows (MSYS2 / gvsbuild) | current | current | yes |

## Fallbacks

We don't plan to support older GTK in-tree. If a downstream needs it:

- Without 4.16 memory-texture updates: build a full `GdkMemoryTexture` per frame
  (correct, but costs a 4K copy per update).
- Without the cursor callback: pre-scale the cursor bitmap to `ceil(scale)` and use
  `Cursor::from_texture` with the hotspot divided by the scale.
