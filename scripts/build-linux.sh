#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Transom — Linux build and packaging helper, with the flags of Tributary's and Balun's
# build-linux.sh. `scripts/build-linux.sh --help` lists them.
set -euo pipefail
cd "$(dirname "$0")/.."
. scripts/lib/build.sh

usage() {
    cat <<'EOF'
Transom — Linux build helper.

Usage:
  scripts/build-linux.sh [options]

With no options, checks the build dependencies, then builds the release binary
(target/release/transom) with Cargo's locked dependency graph.

Launch:
  --run         Build, then run target/release/transom in this terminal. Cannot be
                combined with a quick-exit or packaging mode.

Quick-exit modes (choose at most one):
  --fmt         Run cargo fmt on the workspace. Needs only Cargo.
  --check       Check every workspace target.
  --clippy      Clippy with warnings denied on every workspace target, in the
                debug and then the release profile.
  --coverage    Print a workspace coverage summary. Needs cargo-llvm-cov
                (cargo install --locked cargo-llvm-cov --version 0.8.7).

Packaging (any combination; written to dist/ under the release asset names):
  --deb         dist/transom-ARCH.deb with cargo-deb 3.8.0. Build it on Debian 13
                with FreeRDP 3.33.1 from trixie-backports, as the release workflow
                does (the package requires it): its library dependencies come
                from dpkg-shlibdeps (dpkg-dev).
  --rpm         dist/transom-ARCH.rpm with cargo-generate-rpm 0.21.0, against
                FreeRDP 3.33.1 or later (the package requires it). Needs
                rpm-build, whose find-requires gives the library dependencies.
  --arch-pkg    dist/transom-ARCH.pkg.tar.zst with makepkg (build-aux/arch/PKGBUILD).
                It builds the committed HEAD (scripts/dist.sh), not uncommitted
                changes.
  --flatpak     dist/transom-linux-ARCH.flatpak with flatpak-builder (or the
                org.flatpak.Builder Flatpak) from the user's flathub remote. It
                builds in the GNOME SDK, so needs no host libraries or Rust.
  --appimage    dist/transom-linux-ARCH.AppImage (build-aux/appimage/). Build it on
                Debian 13, as the release workflow does: it bundles the build
                system's libraries, and runs where the C library is as new as
                they need (glibc 2.39, Ubuntu 24.04). FreeRDP is not Debian's: it
                builds the Flatpak's pinned FreeRDP release from source first
                (build-aux/appimage/build-freerdp.sh), and Transom against it, in
                target/appimage/. Downloads the FreeRDP source and the pinned
                AppImage runtime once (each checked against its SHA-256).

Other:
  -h, --help    Show this help and exit.

The helper installs nothing: a missing library or tool stops it before any build
work, with the command that installs it. Cargo may fetch the locked crates.
EOF
}

usage_error() { echo "build-linux: $*" >&2; usage >&2; exit 2; }
fail() { echo "build-linux: $*" >&2; exit 1; }

FMT=0 CHECK=0 CLIPPY=0 COVERAGE=0 RUN=0 DEB=0 RPM=0 ARCH_PKG=0 FLATPAK=0 APPIMAGE=0
for a in "$@"; do
    case $a in
        -h|--help) usage; exit 0 ;;
        --fmt) FMT=1 ;;
        --check) CHECK=1 ;;
        --clippy) CLIPPY=1 ;;
        --coverage) COVERAGE=1 ;;
        --run) RUN=1 ;;
        --deb) DEB=1 ;;
        --rpm) RPM=1 ;;
        --arch-pkg) ARCH_PKG=1 ;;
        --flatpak) FLATPAK=1 ;;
        --appimage) APPIMAGE=1 ;;
        *) usage_error "unknown option: $a" ;;
    esac
done
quick=$((FMT + CHECK + CLIPPY + COVERAGE))
packaging=$((DEB + RPM + ARCH_PKG + FLATPAK + APPIMAGE))
(( quick <= 1 )) || usage_error "choose at most one of --fmt, --check, --clippy and --coverage"
(( quick == 0 || packaging == 0 )) || usage_error "a quick-exit mode can't be combined with packaging"
(( RUN == 0 || quick + packaging == 0 )) || usage_error "--run can't be combined with other modes"

if (( FMT )); then
    command -v cargo >/dev/null || fail "cargo not found: install Rust with rustup, https://rustup.rs"
    cargo fmt --version >/dev/null 2>&1 || fail "cargo fmt not found: rustup component add rustfmt"
    info "cargo fmt --all"
    exec cargo fmt --all
fi

[[ $(uname) == Linux ]] || fail "run this on Linux (scripts/build-macos.sh, scripts/build-windows.ps1)"
# The host build, against the system's libraries: everything but a packaging run of
# only --arch-pkg/--flatpak/--appimage, which build in their own environments (the
# AppImage's against the FreeRDP it builds, with the system's other libraries).
HOST=$(( packaging == 0 || DEB || RPM ))

# ---- dependency checks (everything first, then one report) ----
distro=$(. /etc/os-release 2>/dev/null; echo " ${ID:-} ${ID_LIKE:-} ")
dnf_pkgs="gcc clang-devel pkgconf-pkg-config freerdp-devel libwinpr-devel gtk4-devel libadwaita-devel libsecret-devel libavcodec-free-devel libavutil-free-devel"
apt_pkgs="build-essential pkg-config clang libclang-dev libgtk-4-dev libadwaita-1-dev freerdp3-dev libwinpr3-dev libsecret-1-dev libavcodec-dev libavutil-dev"
pacman_pkgs="rust clang freerdp gtk4 libadwaita libsecret ffmpeg"
# The AppImage alone builds its own FreeRDP: not the system's development files.
if (( APPIMAGE && !HOST )); then
    apt_pkgs=${apt_pkgs/ freerdp3-dev libwinpr3-dev/}
fi
# What build-aux/appimage/build-freerdp.sh needs for the Flatpak's FreeRDP features.
freerdp_apt_pkgs="cmake ninja-build libssl-dev libkrb5-dev libicu-dev libopus-dev libpulse-dev libcups2-dev libpcsclite-dev libusb-1.0-0-dev libswscale-dev libsystemd-dev"
hints=()
case $distro in
    *" fedora "*|*" rhel "*) hints+=("Install the libraries:  sudo dnf install $dnf_pkgs") ;;
    *" debian "*|*" ubuntu "*) hints+=("Install the libraries:  sudo apt install $apt_pkgs") ;;
    *" arch "*) hints+=("Install the libraries:  sudo pacman -S --needed $pacman_pkgs") ;;
    *) hints+=("Install the libraries:" "  Fedora: sudo dnf install $dnf_pkgs"
               "  Debian 13 / Ubuntu 25.10: sudo apt install $apt_pkgs"
               "  Arch: sudo pacman -S --needed $pacman_pkgs") ;;
esac

# bindgen (the FreeRDP and FFmpeg bindings) loads libclang: LIBCLANG_PATH, else the
# places clang-sys searches on Fedora, Debian/Ubuntu and Arch.
have_libclang() {
    local f
    [[ -f ${LIBCLANG_PATH:-} ]] && return 0   # clang-sys also takes the library file itself
    for f in ${LIBCLANG_PATH:+"$LIBCLANG_PATH"/libclang*.so*} /usr/lib64/libclang*.so* \
             /usr/lib/libclang*.so* /usr/lib/*-linux-gnu/libclang*.so* \
             /usr/lib/llvm*/lib/libclang*.so* /usr/lib64/llvm*/lib*/libclang*.so*; do
        [[ -e $f ]] && return 0
    done
    return 1
}

lib_problems=0
if (( HOST || APPIMAGE )); then
    check_rust
    before=${#PROBLEMS[@]}
    # The pkg-config the build scripts use: PKG_CONFIG, else pkg-config.
    pc=${PKG_CONFIG:-pkg-config}
    if command -v "$pc" >/dev/null; then
        check_modules "$pc" "gtk4 4.16" "libadwaita-1 1.6" "libsecret-1"
        if (( DEB || RPM )); then
            # The packages require FreeRDP 3.33.1: older releases have known security
            # problems a server can exploit.
            check_modules "$pc" "freerdp3 3.33.1" "freerdp-client3 3.33.1" "winpr3 3.33.1"
        elif (( HOST )); then
            check_modules "$pc" "freerdp3 3.15" "freerdp-client3 3.15" "winpr3 3.15"
        fi
        if (( HOST )); then
            # Optional: without FFmpeg, hardware H.264 decoding is left out of the build.
            for m in libavcodec libavutil; do
                "$pc" --atleast-version=59 "$m" 2>/dev/null \
                    || warn "$m >= 59 not found: building without hardware video decoding"
            done
        fi
        if (( APPIMAGE )); then
            # FFmpeg (Transom's hardware video decoding, FreeRDP's scaling), and the
            # libraries behind the other FreeRDP features the Flatpak has (PulseAudio,
            # printers, smart cards, USB, Kerberos, ...).
            check_modules "$pc" "libavcodec 59" "libavutil 59" libswscale openssl libpulse \
                cups libpcsclite libusb-1.0 mit-krb5 icu-uc icu-i18n opus libsystemd
        fi
    else
        problem "$pc (pkg-config)"
    fi
    have_libclang || problem "libclang, for bindgen (set LIBCLANG_PATH if it is somewhere unusual)"
    lib_problems=$(( ${#PROBLEMS[@]} - before ))
    if (( DEB && lib_problems )); then
        hints+=("The .deb needs FreeRDP 3.33.1; on Debian 13 it is in trixie-backports (README.md, Installation):  sudo apt install -t trixie-backports freerdp3-dev libwinpr3-dev")
    fi
fi
(( CLIPPY )) && check_component clippy clippy
(( COVERAGE )) && check_llvm_cov
if (( DEB )); then
    check_tool_version cargo-deb 3.8.0 "cargo install --locked cargo-deb --version 3.8.0"
    check_cmd dpkg-shlibdeps "dpkg-dev, which gives the package its library dependencies (build the .deb on Debian 13 / Ubuntu 25.10 or later)"
fi
if (( RPM )); then
    check_tool_version cargo-generate-rpm 0.21.0 "cargo install --locked cargo-generate-rpm --version 0.21.0"
    # Without it cargo-generate-rpm falls back to its own scan, which emits glibc
    # "[WEAK]" requirements nothing provides.
    [[ -x /usr/lib/rpm/find-requires ]] \
        || problem "/usr/lib/rpm/find-requires: rpm-build (sudo dnf install rpm-build), for the package's library requirements"
fi
if (( ARCH_PKG )); then
    check_cmd makepkg "from pacman (Arch Linux)"
    check_cmd git "scripts/dist.sh archives HEAD with it"
    check_cmd cargo "scripts/dist.sh vendors the crates with it: https://rustup.rs"
    # The PKGBUILD's dependencies, which makepkg would only check after the vendoring.
    if command -v pacman >/dev/null; then
        missing=$(bash -c '. build-aux/arch/PKGBUILD; pacman -T "${depends[@]}" "${makedepends[@]}"' 2>/dev/null || true)
        [[ -z $missing ]] || problem "the PKGBUILD's dependencies: sudo pacman -S --needed" $missing
    fi
    (( EUID )) || problem "a non-root user: makepkg refuses to run as root"
fi
if (( FLATPAK )); then
    check_cmd python3 "for build-aux/flatpak/cargo-sources.py"
    if command -v flatpak >/dev/null; then
        if command -v flatpak-builder >/dev/null; then
            builder=(flatpak-builder)
        elif flatpak info org.flatpak.Builder >/dev/null 2>&1; then
            builder=(flatpak run org.flatpak.Builder)
        else
            problem "flatpak-builder: install it, or flatpak install --user flathub org.flatpak.Builder"
        fi
        flatpak remote-list --user --columns=name | grep -qx flathub \
            || problem "the user's flathub remote: flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo"
    else
        problem "flatpak"
    fi
fi
if (( APPIMAGE )); then
    if command -v dpkg-query >/dev/null; then
        before=${#PROBLEMS[@]}
        check_cmd patchelf "patchelf"
        check_cmd mksquashfs "squashfs-tools"
        check_cmd readelf "binutils"
        check_cmd glib-compile-schemas "libglib2.0-bin"
        # FreeRDP's build (build-aux/appimage/build-freerdp.sh).
        check_cmd cmake "cmake"
        check_cmd ninja "ninja-build"
        check_cmd patch "patch"
        check_cmd dpkg-buildflags "dpkg-dev"
        [[ -n ${APPIMAGE_RUNTIME:-} && -n ${FREERDP_TARBALL:-} ]] \
            || check_cmd curl "curl (or set APPIMAGE_RUNTIME and FREERDP_TARBALL to downloaded copies)"
        multiarch=/usr/lib/$(uname -m)-linux-gnu
        for f in "$multiarch/gdk-pixbuf-2.0/gdk-pixbuf-query-loaders:libgdk-pixbuf-2.0-0" \
                 "$multiarch/gdk-pixbuf-2.0/2.10.0/loaders/libpixbufloader_svg.so:librsvg2-common" \
                 "$multiarch/gio/modules/libdconfsettings.so:dconf-gsettings-backend" \
                 "$multiarch/ossl-modules/legacy.so:openssl-provider-legacy" \
                 "/usr/share/icons/Adwaita/index.theme:adwaita-icon-theme" \
                 "/usr/share/icons/hicolor/index.theme:hicolor-icon-theme"; do
            [[ -f ${f%%:*} ]] || problem "${f%%:*}: ${f##*:}"
        done
        if (( ${#PROBLEMS[@]} > before || lib_problems )); then
            hints+=("For the AppImage:  sudo apt install patchelf squashfs-tools binutils libglib2.0-bin curl librsvg2-common dconf-gsettings-backend openssl-provider-legacy adwaita-icon-theme $freerdp_apt_pkgs")
            lib_problems=1
        fi
    else
        problem "dpkg-query: build the AppImage on Debian 13, as the release workflow does (for example in a debian:trixie container); it records the Debian package behind every bundled file"
    fi
fi
if (( lib_problems )); then report_problems "${hints[@]}"; else report_problems; fi

# ---- quick-exit modes ----
if (( CHECK )); then
    info "cargo check --workspace --all-targets"
    exec cargo check --workspace --all-targets --locked
fi
if (( CLIPPY )); then
    info "cargo clippy --workspace --all-targets (debug)"
    cargo clippy --workspace --all-targets --locked -- -D warnings
    info "cargo clippy --workspace --all-targets --release"
    exec cargo clippy --workspace --all-targets --release --locked -- -D warnings
fi
if (( COVERAGE )); then
    info "cargo llvm-cov --workspace --all-targets"
    CARGO_LLVM_COV_SETUP=no exec cargo llvm-cov --workspace --all-targets --locked --summary-only
fi

mkdir -p dist
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

# ---- Flatpak (in the GNOME SDK, not on the host) ----
if (( FLATPAK )); then
    python3 -I build-aux/flatpak/cargo-sources.py Cargo.lock \
        | diff -q build-aux/flatpak/cargo-sources.json - >/dev/null \
        || fail "build-aux/flatpak/cargo-sources.json is stale: run build-aux/flatpak/cargo-sources.py Cargo.lock > build-aux/flatpak/cargo-sources.json"
    out=dist/transom-linux-$(flatpak --default-arch).flatpak
    rm -f "$out"
    info "flatpak-builder (GNOME SDK) -> $out"
    # _flatpak/ is in the manifest's skip list, so neither directory is copied into
    # the build.
    "${builder[@]}" --user --install-deps-from=flathub --force-clean --repo=_flatpak/repo \
        _flatpak/build build-aux/flatpak/io.github.jm2.Transom.yml
    flatpak build-bundle _flatpak/repo "$out" io.github.jm2.Transom \
        --runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo
    info "wrote $out"
fi

# Cargo's target directory (CARGO_TARGET_DIR, a config file, or target/).
cargo_target_dir() {
    cargo metadata --format-version 1 --no-deps --locked |
        sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p'
}

# ---- host release build ----
if (( HOST )); then
    info "cargo build --release -p transom"
    cargo build --release --locked -p transom
    target_dir=$(cargo_target_dir)
    BIN=$target_dir/release/transom
    [[ -x $BIN ]] || fail "missing $BIN"
    info "built $BIN"
    if (( RUN )); then
        info "running $BIN"
        exec "$BIN"
    fi
fi

if (( DEB )); then
    out=dist/transom-$(dpkg --print-architecture).deb
    rm -f "$out"
    info "cargo deb -> $out"
    cargo deb -p transom --no-build --output "$out"
    [[ -f $out ]] || fail "cargo-deb wrote no $out"
    info "wrote $out"
fi

if (( RPM )); then
    out=dist/transom-$(uname -m).rpm
    rm -f "$out"
    info "cargo generate-rpm -> $out"
    cargo generate-rpm -p crates/transom -o "$out"
    [[ -f $out ]] || fail "cargo-generate-rpm wrote no $out"
    info "wrote $out"
fi

if (( ARCH_PKG )); then
    pkgver=$(sed -n 's/^pkgver=//p' build-aux/arch/PKGBUILD)
    [[ $pkgver == "$VERSION" ]] || fail "build-aux/arch/PKGBUILD has pkgver=$pkgver, Cargo.toml $VERSION"
    [[ -z $(git status --porcelain --untracked-files=no) ]] \
        || warn "the Arch package builds the committed HEAD: uncommitted changes are left out"
    info "scripts/dist.sh (source and vendored-crate tarballs of HEAD)"
    scripts/dist.sh
    work=$(mktemp -d -p "${TMPDIR:-/var/tmp}" transom-arch.XXXXXX)
    trap 'rm -rf "$work"' EXIT
    cp build-aux/arch/PKGBUILD "dist/transom-$VERSION.tar.gz" "dist/transom-$VERSION-vendor.tar.xz" "$work/"
    info "makepkg"
    # makepkg checks the dependencies itself; it never installs them here (no -s).
    (cd "$work" && PKGDEST="$work/out" SRCDEST="$work" BUILDDIR="$work/build" makepkg -f --noconfirm)
    # The package, and its detached signature when makepkg.conf has "sign".
    pkgs=()
    for f in "$work"/out/*.pkg.tar*; do [[ -f $f && $f != *.sig ]] && pkgs+=("$f"); done
    (( ${#pkgs[@]} == 1 )) || fail "makepkg didn't write exactly one package"
    out=dist/transom-$(uname -m).pkg.tar${pkgs[0]##*.pkg.tar}
    rm -f "$out.sig"   # an earlier signed build's, which wouldn't match this package
    mv "${pkgs[0]}" "$out"
    [[ ! -f ${pkgs[0]}.sig ]] || mv "${pkgs[0]}.sig" "$out.sig"
    info "wrote $out"
fi

if (( APPIMAGE )); then
    # FreeRDP from the Flatpak's pinned source, then Transom against it, in a target
    # directory of their own: a --deb or --rpm of the same run keeps the system's.
    appimage_dir=$(cargo_target_dir)/appimage
    build-aux/appimage/build-freerdp.sh "$appimage_dir/freerdp"
    info "cargo build --release -p transom (against $appimage_dir/freerdp)"
    PKG_CONFIG_PATH="$appimage_dir/freerdp/root/usr/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}" \
        CARGO_TARGET_DIR=$appimage_dir cargo build --release --locked -p transom
    build-aux/appimage/build-appimage.sh "$appimage_dir/release/transom" "$appimage_dir/freerdp" \
        "dist/transom-linux-$(uname -m).AppImage"
fi

if (( packaging == 0 )); then
    info "scripts/install-local.sh installs it under ~/.local; --run starts it from here"
fi
