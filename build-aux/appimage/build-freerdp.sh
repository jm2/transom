#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Build the AppImage's FreeRDP from source, the same release the Flatpak builds, into a
# private directory. `scripts/build-linux.sh --appimage` runs it from the repository
# root on Debian 13, builds Transom against the result, and hands it to
# build-appimage.sh, which bundles it. Debian 13's own FreeRDP (3.15) has many known
# problems a hostile server can exploit and no fixed version in Debian 13 itself, so the
# AppImage doesn't use it.
#
#   build-aux/appimage/build-freerdp.sh DIR
#
# What to build comes from the freerdp module of build-aux/flatpak/io.github.jm2.Transom.yml,
# so the Flatpak and the AppImage always build the same FreeRDP: its release tarball
# (checked against the manifest's SHA-256; downloaded once into ~/.cache/transom, or
# FREERDP_TARBALL names a local copy, checked the same way), its patches and its CMake
# options, then the AppImage's own options below. The compiler and linker flags are
# Debian's hardened defaults (dpkg-buildflags), as Debian's packages are built with.
#
# DIR gets:
#   root/usr/          the installed client libraries, headers and pkg-config files
#   build-info.txt     version, source, SHA-256, patches and options, tab-separated, for
#                      BUNDLED-PACKAGES.txt
#   licenses/          FreeRDP's licence and notice files (paths as in its source)
#   patches/           the patches applied
# DIR is reused when it already holds this exact build (the same source, patches,
# options, flags and script), and rebuilt from scratch otherwise.
set -euo pipefail

DIR=${1:?usage: build-freerdp.sh DIR}
MANIFEST=build-aux/flatpak/io.github.jm2.Transom.yml
SELF=build-aux/appimage/build-freerdp.sh

info() { echo "==> $*"; }
fail() { echo "build-freerdp: $*" >&2; exit 1; }

[[ -f $MANIFEST && -f $SELF ]] || fail "run it from the repository root"

# ---- the Flatpak's freerdp module ----
# One "KIND<TAB>VALUE" line per option, the archive's url and sha256, and each patch.
# The manifest is plain block YAML (two-space indents), so awk reads it.
module=$(awk '
    function value(s) { sub(/^[^:]*:[[:space:]]*/, "", s); sub(/[[:space:]]+#.*$/, "", s); return s }
    /^  - name:/ { mod = value($0); key = ""; next }
    mod != "freerdp" || /^[[:space:]]*#/ { next }
    /^    [A-Za-z-]+:/ { key = $1; sub(/:$/, "", key); next }
    key == "config-opts" && /^      - / {
        opt = substr($0, 9); sub(/[[:space:]]+#.*$/, "", opt); print "option\t" opt; next
    }
    key == "sources" && /^      - type:/ { type = value($0); next }
    key == "sources" && type == "archive" && /^        (url|sha256):/ {
        k = $1; sub(/:$/, "", k); print k "\t" value($0); next
    }
    key == "sources" && type == "patch" && /^        path:/ { print "patch\t" value($0); next }
' "$MANIFEST")
mapfile -t OPTIONS < <(awk -F'\t' '$1 == "option" { print $2 }' <<<"$module")
mapfile -t PATCHES < <(awk -F'\t' '$1 == "patch" { print $2 }' <<<"$module")
URL=$(awk -F'\t' '$1 == "url" { print $2 }' <<<"$module")
SHA256=$(awk -F'\t' '$1 == "sha256" { print $2 }' <<<"$module")
[[ $URL =~ ^https://github\.com/FreeRDP/FreeRDP/releases/download/[0-9.]+/freerdp-([0-9.]+)\.tar\.gz$ ]] \
    || fail "$MANIFEST: the freerdp module needs exactly one FreeRDP release tarball (url: $URL)"
VERSION=${BASH_REMATCH[1]}
[[ $SHA256 =~ ^[0-9a-f]{64}$ ]] || fail "$MANIFEST: the freerdp tarball has no SHA-256"
(( ${#OPTIONS[@]} )) || fail "$MANIFEST: the freerdp module has no config-opts"
for o in "${OPTIONS[@]}"; do
    [[ $o == -D* ]] || fail "$MANIFEST: unexpected freerdp option '$o' (only -D options are understood)"
done
for p in "${PATCHES[@]}"; do
    [[ $p != */* && -f build-aux/flatpak/$p ]] || fail "$MANIFEST: no patch build-aux/flatpak/$p"
done

# The AppImage's own options, after the Flatpak's (CMake takes the last of each).
APPIMAGE_OPTIONS=(
    # Installed as a distribution installs it (into DIR/root with DESTDIR), so what
    # FreeRDP and WinPR compile in is the host's own system configuration directory
    # (/etc; WinPR's registry file, for one), never a path of this build that another
    # local user could create on the user's machine.
    -DCMAKE_INSTALL_PREFIX=/usr
    -DCMAKE_INSTALL_LIBDIR=lib
    # Every channel is built in (upstream's default, checked below). A channel plugin
    # would be loaded by file name, through the library's RUNPATH (the image's
    # usr/lib), and never from the host's FreeRDP plugin directory: the image is
    # relocatable, as upstream's own macOS and Windows bundles are.
    -DWITH_ABSOLUTE_PLUGIN_LOAD_PATHS=OFF
    # As the Flatpak's build, whose GNOME SDK has no JSON library: FreeRDP then reads
    # no JSON configuration files (and no build machine's library decides).
    -DWITH_JSON_DISABLED=ON
)
# The channels Transom uses: each must be built in, or the build fails.
CHANNELS=(cliprdr rdpsnd audin rdpgfx disp rdpdr drive printer smartcard serial parallel urbdrc rdpear)

# Debian's hardened flags (stack protector, fortify, RELRO and BINDNOW, control-flow
# protection, ...). CMake reads CFLAGS and LDFLAGS, not CPPFLAGS.
command -v dpkg-buildflags >/dev/null || fail "dpkg-buildflags not found: sudo apt install dpkg-dev"
export DEB_BUILD_MAINT_OPTIONS=hardening=+all
CFLAGS="$(dpkg-buildflags --get CFLAGS) $(dpkg-buildflags --get CPPFLAGS)"
LDFLAGS=$(dpkg-buildflags --get LDFLAGS)
export CFLAGS LDFLAGS

key=$({
    cat "$SELF"
    printf '%s\n' "$URL" "$SHA256" "${OPTIONS[@]}" "${APPIMAGE_OPTIONS[@]}" "$CFLAGS" "$LDFLAGS"
    for p in "${PATCHES[@]}"; do cat "build-aux/flatpak/$p"; done
} | sha256sum | cut -d ' ' -f 1)
if [[ -f $DIR/key && $(<"$DIR/key") == "$key" && -f $DIR/build-info.txt ]]; then
    info "FreeRDP $VERSION: reusing the build in $DIR"
    exit 0
fi

# ---- source ----
tarball=${FREERDP_TARBALL:-}
if [[ -z $tarball ]]; then
    tarball=${XDG_CACHE_HOME:-$HOME/.cache}/transom/freerdp-$VERSION.tar.gz
    if [[ ! -f $tarball ]]; then
        info "downloading FreeRDP $VERSION"
        mkdir -p "${tarball%/*}"
        curl -fsSL --proto '=https' -o "$tarball.part" "$URL"
        mv "$tarball.part" "$tarball"
    fi
fi
echo "$SHA256  $tarball" | sha256sum -c --quiet - >/dev/null 2>&1 \
    || fail "$tarball isn't FreeRDP $VERSION as the Flatpak manifest pins it (SHA-256 $SHA256)"

# Start afresh, but only ever delete a directory this script made.
if [[ -e $DIR && ! -f $DIR/.transom-freerdp && -n $(ls -A "$DIR") ]]; then
    fail "$DIR exists and isn't a FreeRDP build directory of this script: name another"
fi
rm -rf "$DIR"
mkdir -p "$DIR/src" "$DIR/licenses" "$DIR/patches"
touch "$DIR/.transom-freerdp"
tar -xzf "$tarball" -C "$DIR/src" --strip-components=1 --no-same-owner
for p in "${PATCHES[@]}"; do
    info "applying $p"
    patch -d "$DIR/src" -p1 --forward --batch --quiet < "build-aux/flatpak/$p"
    cp "build-aux/flatpak/$p" "$DIR/patches/"
done

# ---- build ----
info "configuring FreeRDP $VERSION"
cmake -S "$DIR/src" -B "$DIR/build" -G Ninja "${OPTIONS[@]}" "${APPIMAGE_OPTIONS[@]}" \
    > "$DIR/configure.log" 2>&1 || { tail -40 "$DIR/configure.log" >&2; fail "cmake failed ($DIR/configure.log)"; }
# FreeRDP quietly drops a feature that was asked for when its library is missing.
if grep -q 'was requested but could not be found' "$DIR/configure.log"; then
    grep -B1 -A1 'was requested but could not be found' "$DIR/configure.log" >&2
    fail "a FreeRDP feature the Flatpak has is missing here: install its development package"
fi
for c in "${CHANNELS[@]}"; do
    grep -Eq "Adding (STATIC|DYNAMIC|DEVICE) channel client.* \"$c\"" "$DIR/configure.log" \
        || fail "FreeRDP's $c channel isn't built ($DIR/configure.log)"
done
info "building FreeRDP $VERSION"
cmake --build "$DIR/build" > "$DIR/build.log" 2>&1 \
    || { tail -40 "$DIR/build.log" >&2; fail "the build failed ($DIR/build.log)"; }
DESTDIR=$DIR/root cmake --install "$DIR/build" --strip > "$DIR/install.log" 2>&1 \
    || { tail -20 "$DIR/install.log" >&2; fail "the install failed ($DIR/install.log)"; }

lib=$DIR/root/usr/lib
for so in libfreerdp3.so.3 libfreerdp-client3.so.3 libwinpr3.so.3; do
    [[ -f $lib/$so ]] || fail "the build installed no $so"
done
# Built-in channels only: nothing is left for the plugin directory.
plugins=$(find "$lib" -mindepth 2 -name '*.so*' -print)
[[ -z $plugins ]] || fail "FreeRDP installed plugins, which the AppImage doesn't carry:"$'\n'"$plugins"

# ---- what was built ----
(cd "$DIR/src" && find . \( -name LICENSE -o -name 'COPYING*' -o -name NOTICE \) -type f) |
    while read -r f; do install -Dm644 "$DIR/src/$f" "$DIR/licenses/${f#./}"; done
{
    printf 'version\t%s\nurl\t%s\nsha256\t%s\n' "$VERSION" "$URL" "$SHA256"
    for p in "${PATCHES[@]}"; do printf 'patch\t%s\n' "$p"; done
    for o in "${OPTIONS[@]}" "${APPIMAGE_OPTIONS[@]}"; do printf 'option\t%s\n' "$o"; done
    printf 'cflags\t%s\nldflags\t%s\n' "$CFLAGS" "$LDFLAGS"
} > "$DIR/build-info.txt"
rm -rf "$DIR/src" "$DIR/build"
echo "$key" > "$DIR/key"
info "built FreeRDP $VERSION into $DIR/root"
