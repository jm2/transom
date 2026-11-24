#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Assemble the Transom AppImage from a release binary. `scripts/build-linux.sh
# --appimage` runs it from the repository root, after checking the tools it needs.
# Build it on Debian 13, as the release workflow does: the oldest system with
# Transom's minimum GTK, libadwaita and FreeRDP, and so the widest reach (it
# reports the oldest glibc the image runs with: 2.39, Ubuntu 24.04), and the
# bundled packages are recorded with dpkg.
#
#   build-aux/appimage/build-appimage.sh BINARY OUTPUT
#
# An AppImage is the AppImage type2 runtime followed by a squashfs image of an
# AppDir. The runtime is the release pinned below, downloaded once into
# ~/.cache/transom and checked against its SHA-256 (APPIMAGE_RUNTIME names a local
# copy instead, checked the same way). The AppDir:
#   AppRun, io.github.jm2.Transom.desktop and .png, .DirIcon   (the entry points)
#   usr/bin/                transom, gdk-pixbuf-query-loaders (AppRun runs it)
#   usr/lib/*.so*           every library those and the modules need, except the
#                           host's (build-aux/appimage/excludelist: glibc, the
#                           graphics stack, ...), each with RUNPATH $ORIGIN
#   usr/lib/{gdk-pixbuf-2.0,gio/modules,ossl-modules}   loadable modules
#   usr/share/              icons (Adwaita, the app's), GTK's GSettings schemas,
#                           the desktop entry, metainfo, MIME type
#   usr/share/doc/transom/  COPYING, THIRD-PARTY-NOTICES.txt,
#                           RUST-CRATE-LICENSES.txt, BUNDLED-PACKAGES.txt, licenses/
set -euo pipefail

BIN=${1:?usage: build-appimage.sh BINARY OUTPUT}
OUT=${2:?usage: build-appimage.sh BINARY OUTPUT}
ID=io.github.jm2.Transom
ARCH=$(uname -m)
# https://github.com/AppImage/type2-runtime/releases
RUNTIME_RELEASE=20251108
case $ARCH in
    x86_64) RUNTIME_SHA256=2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d ;;
    aarch64) RUNTIME_SHA256=00cbdfcf917cc6c0ff6d3347d59e0ca1f7f45a6df1a428a0d6d8a78664d87444 ;;
    *) echo "build-appimage: no AppImage runtime is pinned for $ARCH" >&2; exit 1 ;;
esac
LIBDIR=/usr/lib/$ARCH-linux-gnu
PIXBUF=$LIBDIR/gdk-pixbuf-2.0
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

info() { echo "==> $*"; }
fail() { echo "build-appimage: $*" >&2; exit 1; }

work=$(mktemp -d -p "${TMPDIR:-/var/tmp}" transom-appimage.XXXXXX)
trap 'rm -rf "$work"' EXIT
APPDIR=$work/AppDir
# SOURCE<TAB>DEST of every file taken from the system (a directory tree: one of its
# files, and DEST ending in /), for BUNDLED-PACKAGES.txt.
: > "$work/files"

# copy SOURCE DEST: a system file into the AppDir (DEST relative to it), recorded.
copy() {
    install -D -m "$(stat -Lc %a "$1")" "$1" "$APPDIR/$2"
    printf '%s\t%s\n' "$1" "$2" >> "$work/files"
}

declare -A EXCLUDED=()
while read -r so; do
    EXCLUDED[$so]=1
done < <(sed -e 's/#.*//' -e 's/[[:space:]]//g' build-aux/appimage/excludelist | grep -v '^$')
# The list names the x86 dynamic loaders; aarch64's is the host's just the same.
EXCLUDED[ld-linux-aarch64.so.1]=1
# Bundled although the list leaves them to the host:
# - libgpg-error: the image bundles Debian 13's libgcrypt, which needs a newer one
#   than older hosts have (Ubuntu 24.04's); bundled together, the pair matches.
#   (The list excludes it for the opposite case: a host libgcrypt meeting an older
#   bundled libgpg-error.)
unset 'EXCLUDED[libgpg-error.so.0]'

needed() { readelf -d "$1" | sed -n 's/.*(NEEDED).*\[\(.*\)\]$/\1/p'; }

# bundle_libs ELF...: copy every library they need into usr/lib, and those
# libraries' own, except the host's. A library reached only through a host library
# is the host's business, so the walk stops at excluded ones.
bundle_libs() {
    local queue=("$@") elf so path
    local -A where
    while (( ${#queue[@]} )); do
        elf=${queue[0]}
        queue=("${queue[@]:1}")
        where=()
        while read -r so path; do
            where[$so]=$path
        done < <(ldd "$elf" | awk '$2 == "=>" { print $1, $3 }')
        while read -r so; do
            [[ -z $so || -n ${EXCLUDED[$so]:-} || -e $APPDIR/usr/lib/$so ]] && continue
            path=${where[$so]:-}
            [[ $path == /* ]] || fail "$elf needs $so, which isn't installed"
            copy "$path" "usr/lib/$so"
            queue+=("$path")
        done < <(needed "$elf")
    done
}

# ---- programs, modules, data ----
info "assembling the AppDir"
install -Dm755 "$BIN" "$APPDIR/usr/bin/transom"
copy "$PIXBUF/gdk-pixbuf-query-loaders" usr/bin/gdk-pixbuf-query-loaders
install -m755 build-aux/appimage/AppRun "$APPDIR/AppRun"

modules=()
for f in "$PIXBUF"/2.10.0/loaders/*.so; do
    copy "$f" "usr/lib/gdk-pixbuf-2.0/2.10.0/loaders/${f##*/}"
    modules+=("$f")
done
# GSettings through the host's dconf, so GTK's settings (the file chooser's) persist.
copy "$LIBDIR/gio/modules/libdconfsettings.so" usr/lib/gio/modules/libdconfsettings.so
# OpenSSL's legacy provider: MD4 and RC4, which NTLM needs.
copy "$LIBDIR/ossl-modules/legacy.so" usr/lib/ossl-modules/legacy.so
modules+=("$LIBDIR/gio/modules/libdconfsettings.so" "$LIBDIR/ossl-modules/legacy.so")

# The Adwaita icons GTK and libadwaita use (not its cursors: the compositor draws the
# standard ones, Transom the remote's), and the hicolor theme for the app icon.
mkdir -p "$APPDIR/usr/share/icons/Adwaita"
for e in index.theme icon-theme.cache scalable symbolic 16x16; do
    if [[ -e /usr/share/icons/Adwaita/$e ]]; then
        cp -RL "/usr/share/icons/Adwaita/$e" "$APPDIR/usr/share/icons/Adwaita/"
    fi
done
printf '%s\t%s\n' /usr/share/icons/Adwaita/index.theme usr/share/icons/Adwaita/ >> "$work/files"
copy /usr/share/icons/hicolor/index.theme usr/share/icons/hicolor/index.theme
cp -R data/icons/hicolor/. "$APPDIR/usr/share/icons/hicolor/"

for f in /usr/share/glib-2.0/schemas/org.gtk.gtk4.*.gschema.xml; do
    copy "$f" "usr/share/glib-2.0/schemas/${f##*/}"
done
glib-compile-schemas "$APPDIR/usr/share/glib-2.0/schemas"

install -Dm644 "data/$ID.desktop" "$APPDIR/usr/share/applications/$ID.desktop"
install -Dm644 "data/$ID.metainfo.xml" "$APPDIR/usr/share/metainfo/$ID.metainfo.xml"
install -Dm644 "data/$ID.mime.xml" "$APPDIR/usr/share/mime/packages/$ID.xml"
# The AppImage's own entry points: the desktop entry and its icon at the top, and
# .DirIcon (file managers' thumbnail).
cp "data/$ID.desktop" "$APPDIR/"
cp "data/icons/hicolor/256x256/apps/$ID.png" "$APPDIR/"
ln -s "$ID.png" "$APPDIR/.DirIcon"

# ---- libraries ----
info "bundling libraries"
bundle_libs "$BIN" "$PIXBUF/gdk-pixbuf-query-loaders" "${modules[@]}"
info "bundled $(find "$APPDIR/usr/lib" -maxdepth 1 -name '*.so*' | wc -l) libraries"

# Each finds its libraries next to it, or in usr/lib from the programs and modules.
set_runpath() {
    local rp=$1 f
    shift
    for f in "$@"; do patchelf --set-rpath "$rp" "$f"; done
}
# shellcheck disable=SC2016 # $ORIGIN is for the dynamic linker
{
    set_runpath '$ORIGIN/../lib' "$APPDIR"/usr/bin/*
    set_runpath '$ORIGIN' "$APPDIR"/usr/lib/*.so*
    set_runpath '$ORIGIN/../../..' "$APPDIR"/usr/lib/gdk-pixbuf-2.0/2.10.0/loaders/*.so
    set_runpath '$ORIGIN/../..' "$APPDIR"/usr/lib/gio/modules/*.so
    set_runpath '$ORIGIN/..' "$APPDIR"/usr/lib/ossl-modules/*.so
}

# Every library each ELF file needs is bundled or the host's, and loads. (Which copy
# of a library both the image and a host library need loads first is up to the
# dynamic linker; it shares one copy by soname either way.)
elves=("$APPDIR"/usr/bin/* "$APPDIR"/usr/lib/*.so* "$APPDIR"/usr/lib/*/*.so
       "$APPDIR"/usr/lib/*/*/*.so "$APPDIR"/usr/lib/gdk-pixbuf-2.0/2.10.0/loaders/*.so)
for f in "${elves[@]}"; do
    [[ -f $f ]] || continue
    while read -r so; do
        [[ -n ${EXCLUDED[$so]:-} || -e $APPDIR/usr/lib/$so ]] \
            || fail "${f#"$APPDIR"/} needs $so, which is neither bundled nor the host's"
    done < <(needed "$f")
    if ldd "$f" | grep -q 'not found'; then
        fail "${f#"$APPDIR"/} needs libraries that don't load: $(ldd "$f" | grep 'not found' | tr -s ' \t\n' ' ')"
    fi
done
# The oldest C library (and C++ library: libstdc++ is the host's too) it runs with.
versions=$(for f in "${elves[@]}"; do if [[ -f $f ]]; then readelf -V "$f"; fi; done 2>/dev/null)
glibc=$(grep -o 'Name: GLIBC_[0-9.]*' <<<"$versions" | sed 's/.*GLIBC_//' | sort -Vu | tail -1)
glibcxx=$(grep -o 'Name: GLIBCXX_[0-9.]*' <<<"$versions" | sed 's/.*GLIBCXX_//' | sort -Vu | tail -1)

# ---- licences: the notices, and the Debian package behind every bundled file ----
info "recording bundled Debian packages"
DOC=$APPDIR/usr/share/doc/transom
install -Dm644 -t "$DOC" COPYING build-aux/packaging/THIRD-PARTY-NOTICES.txt \
    build-aux/packaging/RUST-CRATE-LICENSES.txt
owner() { # FILE → the package (name:arch) that installed it
    local f p
    for f in "$1" "$(realpath "$1")"; do
        for p in "$f" "${f#/usr}"; do
            dpkg-query -S "$p" 2>/dev/null | sed -n '1{s/: \/.*//; s/,.*//; p}' | grep . && return 0
        done
    done
    return 1
}
: > "$work/owned"
while IFS=$'\t' read -r src dest; do
    pkg=$(owner "$src") || fail "no Debian package installed $src: its source can't be named"
    printf '%s\t%s\n' "$pkg" "$dest" >> "$work/owned"
done < "$work/files"
# shellcheck source=/dev/null
distro=$(. /etc/os-release && echo "${PRETTY_NAME:-$ID}")
title="Transom $VERSION AppImage ($ARCH): bundled Debian packages"
{
    printf '%s\n%s\n\n' "$title" "$(printf '%*s' "${#title}" '' | tr ' ' =)"
    fmt -w 80 <<TXT
Every library, program, module, icon and schema in this AppImage that Transom does
not build itself comes from one of the $distro packages below, at the version
given. Each entry names the Debian source package (its source, with Debian's
patches and build recipe, is at the address given), the upstream homepage, the
package's copyright file (its licence terms, copied into licenses/), and what it
provided. See THIRD-PARTY-NOTICES.txt.
TXT
    while read -r pkg; do
        name=${pkg%%:*}
        IFS=$'\t' read -r ver src srcver home < <(dpkg-query -W \
            -f '${Version}\t${source:Package}\t${source:Version}\t${Homepage}\n' "$pkg")
        copyright="none shipped; see the source"
        if [[ -f /usr/share/doc/$name/copyright ]]; then
            install -Dm644 "/usr/share/doc/$name/copyright" "$DOC/licenses/$name/copyright"
            copyright="licenses/$name/copyright"
        fi
        files=$(awk -F'\t' -v p="$pkg" '$1 == p { sub(/\/$/, " (data files)", $2); printf "%s%s", (n++ ? ", " : ""), $2 }' "$work/owned")
        printf '\n%s %s\n' "$name" "$ver"
        printf '  Source: %s %s, https://sources.debian.org/src/%s/%s/\n' "$src" "$srcver" "$src" "$srcver"
        printf '  Homepage: %s\n  Copyright: %s\n  Bundled: %s\n' "${home:-none recorded}" "$copyright" "$files"
    done < <(cut -f1 "$work/owned" | LC_ALL=C sort -u)
} > "$DOC/BUNDLED-PACKAGES.txt"
info "recorded $(grep -c '^  Bundled:' "$DOC/BUNDLED-PACKAGES.txt") Debian packages in BUNDLED-PACKAGES.txt"

# ---- smoke test, image ----
# Binding every symbol at startup catches a library that is too old for another.
LD_BIND_NOW=1 "$APPDIR/AppRun" --help >/dev/null \
    || fail "the AppDir's transom doesn't start (AppRun --help)"

runtime=${APPIMAGE_RUNTIME:-}
if [[ -z $runtime ]]; then
    runtime=${XDG_CACHE_HOME:-$HOME/.cache}/transom/appimage-runtime-$RUNTIME_RELEASE-$ARCH
    if [[ ! -f $runtime ]]; then
        info "downloading the AppImage runtime $RUNTIME_RELEASE ($ARCH)"
        mkdir -p "${runtime%/*}"
        curl -fsSL --proto '=https' -o "$runtime.part" \
            "https://github.com/AppImage/type2-runtime/releases/download/$RUNTIME_RELEASE/runtime-$ARCH"
        mv "$runtime.part" "$runtime"
    fi
fi
echo "$RUNTIME_SHA256  $runtime" | sha256sum -c --quiet - >/dev/null 2>&1 \
    || fail "$runtime isn't the pinned AppImage runtime $RUNTIME_RELEASE (SHA-256 $RUNTIME_SHA256)"

info "squashfs image (zstd)"
SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct 2>/dev/null || date +%s)} \
    mksquashfs "$APPDIR" "$work/image.squashfs" -root-owned -noappend -no-xattrs \
    -comp zstd -Xcompression-level 19 -b 1M -quiet -no-progress
cat "$runtime" "$work/image.squashfs" > "$OUT.part"
chmod 755 "$OUT.part"
mv "$OUT.part" "$OUT"
# The image itself starts (unpacked, as FUSE may be missing here).
TMPDIR=$work APPIMAGE_EXTRACT_AND_RUN=1 LD_BIND_NOW=1 "$OUT" --help >/dev/null \
    || fail "$OUT doesn't start (--help)"
info "wrote $OUT ($(du -h "$OUT" | cut -f1); needs glibc >= $glibc${glibcxx:+, libstdc++ with GLIBCXX_$glibcxx})"
