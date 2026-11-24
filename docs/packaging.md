# Platforms and packages

| Platform | Package | Built by | Notes |
|---|---|---|---|
| Linux x86_64, aarch64 | Flatpak (GNOME 51) | `build-aux/flatpak/io.github.jm2.Transom.yml` | Builds its own FreeRDP (with RDPEAR); H.264 from codecs-extra |
| Fedora 44+ | RPM | `build-aux/rpm/transom.spec` (offline, `scripts/dist.sh` tarballs); Packit → COPR (`.packit.yaml`) | Release workflow also builds an RPM with `cargo generate-rpm` |
| Debian 13+, Ubuntu 25.04+ | .deb | `cargo deb -p transom` | Needs FreeRDP ≥ 3.15: Debian 12 and Ubuntu 24.04 are too old |
| Arch Linux | pkg.tar.zst | `build-aux/arch/PKGBUILD` (checkout); `build-aux/arch/PKGBUILD.aur` (AUR, release tarball) | |
| macOS 13+ (Apple silicon) | .app / .dmg | `scripts/build-macos.sh --dmg` (Homebrew) | Ad-hoc signed; not notarised yet |
| Windows 10/11 x64, arm64 | portable .zip, Inno Setup installer | `scripts/build-windows.ps1 [-InnoSetup]` (MSYS2 CLANG64/CLANGARM64) | The installer registers `.rdp` and `rdp://` (optional task) |

`.github/workflows/release.yml` builds all of these. Run with no tag, it packages the
selected branch as a dry run. Run with a release tag, it builds every package from the
tagged source, writes `SHA256SUMS.txt` over the exact asset list, attests build provenance
(`gh attestation verify <file> --repo jm2/transom --signer-workflow
jm2/transom/.github/workflows/release.yml`), and creates a **draft** release with the
assets and the tag's changelog section.

### Cutting a release

1. Open a pull request "release: prepare vX.Y.Z" that sets the version everywhere:
   `[workspace.package]` and the `transom-*` path dependencies in `Cargo.toml` (then
   `cargo update -w` for `Cargo.lock`),
   `pkgver` in both `build-aux/arch/PKGBUILD*`, `upstream_version` and `Version` in
   `build-aux/rpm/transom.spec` (a prerelease `0.2.0-rc.1` is `Version: 0.2.0~rc.1`), a
   `## [X.Y.Z] — date` section and its link in `CHANGELOG.md`, and a `<release>` in the
   metainfo. `python3 scripts/release_check.py --tag vX.Y.Z` must pass. Merge it.
2. Tag the merge commit on `main` with an annotated tag and push it:
   `git tag -a vX.Y.Z -m "Transom vX.Y.Z" && git push origin vX.Y.Z`.
3. Run the workflow: `gh workflow run release.yml -f tag=vX.Y.Z`.
4. Check the draft release (assets, notes), then publish it. Publishing starts Packit's
   COPR builds (`.packit.yaml`); release tags can't be moved or deleted afterwards.

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
the installer (Inno Setup 6.7); a silent install registers `rdp:` and the `Transom.rdp`
ProgID; an `rdp://` link opened through the shell and an `.rdp` file opened through the
ProgID both start a connection in the installed app; the uninstaller removes every
registration and leaves mstsc's `.rdp` default alone
(`tests/servers/windows/installer-*.ps1`).

Regenerate the raster icons after changing the SVG with `scripts/gen-icons.sh`.
