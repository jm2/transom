# Transom

A focused, HiDPI-correct RDP client built with GTK4 + libadwaita, and **transom-view**,
the reusable GTK4 RDP display widget it is built on.

- Pixel-exact remote sessions on 100%, 200% and fractional-scale displays
- First-class interop with Windows and gnome-remote-desktop
- RDP only: no plugins, no other protocols
- GNOME first; the same GTK app is designed to also ship on macOS and Windows 11

**Status:** pre-alpha. See [PLAN.md](PLAN.md).

## Layout

| Crate | Purpose |
|---|---|
| `crates/transom-view-core` | Pure Rust, no GTK: geometry/scale math, engine trait, config, `.rdp` parsing, cert trust |
| `crates/transom-view-freerdp` | FreeRDP 3 backend (FFI) |
| `crates/transom-view` | `TransomView` GTK4 widget |
| `crates/transom` | The libadwaita application |

## Building

Fedora:

```sh
sudo dnf install freerdp-devel libwinpr-devel gtk4-devel libadwaita-devel clang-devel
scripts/verify.sh
```

## License

- The **transom-view** libraries (`transom-view-core`, `transom-view-freerdp`,
  `transom-view`) are licensed under the Apache License, Version 2.0
  ([LICENSE-APACHE](LICENSE-APACHE)).
- The **Transom** application (`crates/transom`, `data/`) is licensed under the GNU General
  Public License, version 3 or (at your option) any later version ([COPYING](COPYING)).
