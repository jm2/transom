# Platforms and packages

| Platform | Package | Built by | Notes |
|---|---|---|---|
| Linux x86_64, aarch64 | Flatpak (GNOME 51) | `build-aux/flatpak/io.github.jm2.Transom.yml` | Builds its own FreeRDP (with RDPEAR); H.264 from codecs-extra |
| Fedora 44+ | RPM | `build-aux/rpm/transom.spec` (offline, `scripts/dist.sh` tarballs); Packit → COPR (`.packit.yaml`) | Release workflow also builds an RPM with `cargo generate-rpm` |
| Debian 13+, Ubuntu 25.04+ | .deb | `cargo deb -p transom` | Needs FreeRDP ≥ 3.15: Debian 12 and Ubuntu 24.04 are too old |
| Arch Linux | pkg.tar.zst | `build-aux/arch/PKGBUILD` (checkout); `build-aux/arch/PKGBUILD.aur` (AUR, release tarball) | |
| macOS 13+ (Apple silicon) | .app / .dmg | `scripts/build-macos.sh --dmg` (Homebrew) | Ad-hoc signed; not notarised yet |
| Windows 10/11 x64, arm64 | portable .zip, Inno Setup installer | `scripts/build-windows.ps1 [-InnoSetup]` (MSYS2 CLANG64/CLANGARM64) | The installer registers `.rdp` and `rdp://` (optional task, off by default) |

`.github/workflows/release.yml` builds all of these from a release tag. It writes
`SHA256SUMS.txt` over the exact asset list, attests build provenance
(`gh attestation verify <file> --repo jm2/transom --signer-workflow
jm2/transom/.github/workflows/release.yml`), and attaches everything to the GitHub
release.

## Licence notices

Every package carries Transom's licence (`COPYING`) and
`RUST-CRATE-LICENSES.txt`: the licence texts and copyright notices of the Rust crates
compiled into the binary, the transom-view libraries' Apache-2.0 among them.
`scripts/gen-licenses.sh` writes it to `build-aux/packaging/` with
[cargo-about](https://github.com/EmbarkStudios/cargo-about) (`about.toml`, template
`build-aux/packaging/about.hbs`), over the union of the shipped targets. The file is
committed, so the offline builds (Flatpak, RPM spec, Arch) need no extra tool: rerun the
script after changing `Cargo.lock`. `scripts/verify.sh` fails while the file is stale
(CI installs cargo-about 0.9.3; locally,
`cargo install --locked cargo-about --version 0.9.3 --features cli`).

| Package | Licence files |
|---|---|
| Windows .zip / installer | top of the folder (the install folder): `COPYING.txt`, `THIRD-PARTY-NOTICES.txt`, `RUST-CRATE-LICENSES.txt`, `BUNDLED-PACKAGES.txt`, `licenses\` |
| macOS .app | `Transom.app/Contents/Resources/`: `COPYING`, `THIRD-PARTY-NOTICES.txt`, `RUST-CRATE-LICENSES.txt`, `BUNDLED-PACKAGES.txt`, `licenses/` |
| .deb | `/usr/share/doc/transom/`: `copyright` (COPYING), `RUST-CRATE-LICENSES.txt` |
| .rpm (`cargo generate-rpm`) | `/usr/share/licenses/transom/`: `COPYING`, `RUST-CRATE-LICENSES.txt` |
| RPM spec, Arch | `/usr/share/licenses/transom/`: `COPYING`, `LICENSE-APACHE`, `RUST-CRATE-LICENSES.txt` |
| Flatpak | `/app/share/licenses/io.github.jm2.Transom/transom/`: the same three; flatpak-builder puts FreeRDP's and pcsc-lite's beside it |

`THIRD-PARTY-NOTICES.txt` covers the native libraries (GTK, FreeRDP, FFmpeg, …) that only
the Windows and macOS packages bundle; Linux packages use the system's. It carries the
written source offer and points to `BUNDLED-PACKAGES.txt`, which the bundlers write at
build time: every MSYS2 package (from pacman's local database) or Homebrew formula (from
the Cellar keg each file resolves into) the bundle took files from, with its exact
version, declared licence and source location. The licence files those packages ship go
to `licenses/`. A bundled DLL, program or dylib that no package owns fails the build, since
its source could not be named.

## Feature availability by platform

| Feature | Linux | macOS | Windows |
|---|---|---|---|
| Remembered passwords | Secret Service / portal | Keychain | Credential Manager |
| Zero-copy H.264 (ADR 0006) | VA-API + Vulkan renderer | software decode | software decode |
| Kerberos SSO, Remote Credential Guard | FreeRDP build with RDPEAR (Flatpak) | Homebrew FreeRDP (no RDPEAR) | MSYS2 FreeRDP (no RDPEAR) |
| USB redirection | libusb (udev permission, `docs/usb.md`) | not yet: no device list (FreeRDP could; only devices without a kernel driver) | not yet: no device list (devices would need WinUSB) |
| Drives, printers, smart cards | yes | yes | yes |
| `rdp://` links, `.rdp` files | desktop file (`x-scheme-handler/rdp`) | Info.plist; links via an Apple Event handler (`transom-macos`) | installer registry entries |

Verified in the Windows 11 test VM (2026-11-22): `build-windows.ps1 -InnoSetup` builds
the installer (Inno Setup 6.7); a silent install with `/TASKS=rdpfiles` registers `rdp:`
and the `Transom.rdp` ProgID; an `rdp://` link opened through the shell and an `.rdp`
file opened through the ProgID both start a connection in the installed app; the
uninstaller removes every registration and leaves mstsc's `.rdp` default alone
(`tests/servers/windows/installer-*.ps1`).

Regenerate the raster icons after changing the SVG with `scripts/gen-icons.sh`.
