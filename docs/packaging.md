# Platforms and packages

| Platform | Package | Built by | Notes |
|---|---|---|---|
| Linux x86_64, aarch64 | Flatpak (GNOME 51) | `build-aux/flatpak/io.github.jm2.Transom.yml` (`scripts/build-linux.sh --flatpak`) | Builds its own FreeRDP (with RDPEAR); H.264 from codecs-extra. Smart-card (`pcsc`), printer (`cups`) and USB access are not granted by default (`flatpak override`; Transom names the command) |
| Linux x86_64, aarch64 with glibc 2.39+ (Ubuntu 24.04+, Debian 13+, Fedora 42+, Arch, openSUSE Tumbleweed) | AppImage | `build-aux/appimage/build-appimage.sh` (`scripts/build-linux.sh --appimage`, on Debian 13) | Bundles Debian 13's GTK, FreeRDP and FFmpeg, leaving the host libraries on `build-aux/appimage/excludelist` (glibc, the graphics stack, …) to the system; the pinned AppImage type2 runtime |
| Fedora 44+ | RPM | `build-aux/rpm/transom.spec` (offline, `scripts/dist.sh` tarballs); Packit → COPR (`.packit.yaml`) | Release workflow also builds an RPM with `cargo generate-rpm` (`scripts/build-linux.sh --rpm`) |
| Debian 13+, Ubuntu 25.10+ | .deb | `cargo deb -p transom` (`scripts/build-linux.sh --deb`) | Needs FreeRDP ≥ 3.15: Debian 12, Ubuntu 24.04 and 25.04 are too old |
| Arch Linux | pkg.tar.zst | `build-aux/arch/PKGBUILD` (committed HEAD, `scripts/build-linux.sh --arch-pkg`); `build-aux/arch/PKGBUILD.aur` (AUR, release tarball) | |
| macOS 13+ (Apple silicon) | .app / .dmg | `scripts/build-macos.sh --dmg` (Homebrew) | Ad-hoc signed; not notarised yet |
| Windows 10/11 x64, arm64 | portable .zip, Inno Setup installer | `scripts/build-windows.ps1 [-InnoSetup]` (MSYS2 CLANG64/CLANGARM64) | The installer registers `.rdp` and `rdp://` (optional task, off by default) |

The three `scripts/build-*` helpers take the same modes as Tributary's and Balun's
(`--help` / `-Help` lists them) and check every build dependency before building, naming
the packages to install for anything missing. `.github/workflows/release.yml` builds all
of these. Run with no tag, it packages the
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
   metainfo. `python3 scripts/release_check.py --tag vX.Y.Z` must pass. Record the
   bundled libraries' versions too (`scripts/bundled-versions.py --write`, which needs
   podman or docker for Debian's): the weekly Bundled libraries workflow compares
   against them. Merge it.
2. Tag the merge commit on `main` with an annotated tag and push it:
   `git tag -a vX.Y.Z -m "Transom vX.Y.Z" && git push origin vX.Y.Z`.
3. Run the workflow: `gh workflow run release.yml -f tag=vX.Y.Z`.
4. Check the draft release (assets, notes), then publish it. Publishing starts Packit's
   COPR builds (`.packit.yaml`); release tags can't be moved or deleted afterwards.

## Licence notices

Every package carries Transom's licence (`COPYING`) and
`RUST-CRATE-LICENSES.txt`: the licence texts and copyright notices of the Rust crates
compiled into the binary, the transom-view libraries' Apache-2.0 among them.
`scripts/gen-licenses.sh` writes it to `build-aux/packaging/` with
[cargo-about](https://github.com/EmbarkStudios/cargo-about) (`about.toml`, template
`build-aux/packaging/about.hbs`), over the union of the shipped targets. The file is
committed, so the offline builds (Flatpak, RPM spec, Arch) need no extra tool: rerun the
script after changing `Cargo.lock`. A crate whose archive has no licence file would get the
licence's standard text with a placeholder copyright line; the script puts its upstream
notice there from `build-aux/packaging/crate-notices.toml`, and fails for a crate that
has none listed. `scripts/verify.sh` fails while the file is stale
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
| AppImage | `usr/share/doc/transom/` in the image (`--appimage-extract` unpacks it): `COPYING`, `THIRD-PARTY-NOTICES.txt`, `RUST-CRATE-LICENSES.txt`, `BUNDLED-PACKAGES.txt`, `licenses/` |

`THIRD-PARTY-NOTICES.txt` covers the native libraries (GTK, FreeRDP, FFmpeg, …) that only
the Windows, macOS and AppImage packages bundle; the other Linux packages use the system's.
It carries the written source offer and points to `BUNDLED-PACKAGES.txt`, which the
bundlers write at build time: every MSYS2 package (from pacman's local database), Homebrew
formula (from the Cellar keg each file resolves into) or Debian package (from dpkg's
database) the bundle took files from, with its exact version, licence (or, for Debian, its
copyright file) and source location. The licence files those packages ship go to
`licenses/`. A bundled DLL, program, dylib or shared library that no package owns fails the
build, since its source could not be named.

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
