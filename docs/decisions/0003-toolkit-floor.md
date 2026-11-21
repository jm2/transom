# 0003 — GTK and libadwaita version floor

- Status: accepted (confirmed 2026-11-21)
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

## Policy: legacy-free

No compatibility code for anything older than these floors, matching the sibling
projects Tributary and Balun (both GTK 4.16 / libadwaita 1.6). Distros below the floor
(e.g. Ubuntu 24.04) use the Flatpak. Toolchain: Rust edition 2024 with an MSRV of
**1.98**, the same floor as Balun. It's raised deliberately, never just to follow
`stable`.
