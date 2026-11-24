# Platforms and packages

| Platform | Package | Built by | Notes |
|---|---|---|---|
| Linux x86_64, aarch64 | Flatpak (GNOME 51) | `build-aux/flatpak/io.github.jm2.Transom.yml` | Builds its own FreeRDP (with RDPEAR); H.264 from codecs-extra |
| Fedora 44+ | RPM | `build-aux/rpm/transom.spec` (offline, `scripts/dist.sh` tarballs); Packit → COPR (`.packit.yaml`) | Release workflow also builds an RPM with `cargo generate-rpm` |
| Debian 13+, Ubuntu 25.10+ | .deb | `cargo deb -p transom` | Needs FreeRDP ≥ 3.15: Debian 12, Ubuntu 24.04 and 25.04 are too old |
| Arch Linux | pkg.tar.zst | `build-aux/arch/PKGBUILD` (checkout); `build-aux/arch/PKGBUILD.aur` (AUR, release tarball) | |
| macOS 13+ (Apple silicon) | .app / .dmg | `scripts/build-macos.sh --dmg` (Homebrew) | Ad-hoc signed; not notarised yet |
| Windows 10/11 x64, arm64 | portable .zip, Inno Setup installer | `scripts/build-windows.ps1 [-InnoSetup]` (MSYS2 CLANG64/CLANGARM64) | The installer registers `.rdp` and `rdp://` (optional task, off by default) |

`.github/workflows/release.yml` builds all of these from a release tag. It writes
`SHA256SUMS.txt` over the exact asset list, attests build provenance
(`gh attestation verify <file> --repo jm2/transom --signer-workflow
jm2/transom/.github/workflows/release.yml`), and attaches everything to the GitHub
release.

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
