# Platforms and packages

| Platform | Package | Built by | Notes |
|---|---|---|---|
| Linux x86_64, aarch64 | Flatpak (GNOME 51) | `build-aux/flatpak/io.github.jm2.Transom.yml` | Builds its own FreeRDP (with RDPEAR); H.264 from codecs-extra |
| Fedora 44+ | RPM | `build-aux/rpm/transom.spec` (offline, `scripts/dist.sh` tarballs); Packit → COPR (`.packit.yaml`) | Release workflow also builds an RPM with `cargo generate-rpm` |
| Debian 13+, Ubuntu 25.04+ | .deb | `cargo deb -p transom` | Needs FreeRDP ≥ 3.15: Debian 12 and Ubuntu 24.04 are too old |
| Arch Linux | pkg.tar.zst | `build-aux/arch/PKGBUILD` (checkout); `build-aux/arch/PKGBUILD.aur` (AUR, release tarball) | |
| macOS 13+ (Apple silicon) | .app / .dmg | `scripts/build-macos.sh --dmg` (Homebrew) | Ad-hoc signed; not notarised yet |
| Windows 10/11 x64, arm64 | portable .zip, Inno Setup installer | `scripts/build-windows.ps1 [-InnoSetup]` (MSYS2 CLANG64/CLANGARM64) | The installer registers `.rdp` and `rdp://` (optional task) |

`.github/workflows/release.yml` builds all of these from a release tag. It writes
`SHA256SUMS.txt` over the exact asset list, attests build provenance
(`gh attestation verify <file> --repo jm2/transom`), and attaches everything to the
GitHub release.

## Feature availability by platform

| Feature | Linux | macOS | Windows |
|---|---|---|---|
| Remembered passwords | Secret Service / portal | Keychain | Credential Manager |
| Zero-copy H.264 (ADR 0006) | VA-API + Vulkan renderer | software decode | software decode |
| Kerberos SSO, Remote Credential Guard | FreeRDP build with RDPEAR (Flatpak) | Homebrew FreeRDP (no RDPEAR) | MSYS2 FreeRDP (no RDPEAR) |
| USB redirection | libusb (udev permission, `docs/usb.md`) | not yet: no device list (FreeRDP could; only devices without a kernel driver) | not yet: no device list (devices would need WinUSB) |
| Drives, printers, smart cards | yes | yes | yes |

Regenerate the raster icons after changing the SVG with `scripts/gen-icons.sh`.
