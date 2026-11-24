#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Transom — macOS build and packaging: dist/Transom.app, and with --dmg dist/Transom.dmg.
#
# Uses Homebrew's GTK stack, like Tributary and Balun (Apple silicon):
#   brew install gtk4 libadwaita freerdp ffmpeg adwaita-icon-theme pkg-config llvm create-dmg
#
#   scripts/build-macos.sh [--dmg] [--no-build]
#
# The bundle is relocatable: every non-system dylib is copied into
# Contents/Frameworks and referenced as @executable_path/../Frameworks/…; a small
# launcher (Contents/MacOS/Transom) points GTK at Contents/Resources and writes the
# GDK-Pixbuf loader cache for wherever the app is, then execs transom-bin. Code
# signing is ad hoc (no Developer ID or notarisation yet, as in the sibling apps).
set -euo pipefail
cd "$(dirname "$0")/.."
DMG=0; BUILD=1
for a in "$@"; do
    case $a in
        --dmg) DMG=1 ;;
        --no-build) BUILD=0 ;;
        *) echo "usage: $0 [--dmg] [--no-build]" >&2; exit 2 ;;
    esac
done
info() { echo "==> $*"; }
fail() { echo "build-macos: $*" >&2; exit 1; }
[[ $(uname) == Darwin ]] || fail "run this on macOS"
for t in brew otool install_name_tool codesign iconutil realpath; do
    command -v "$t" >/dev/null || fail "$t not found"
done
BREW=$(brew --prefix)
# Scratch space; $work/binaries lists each bundled binary's Homebrew source and its
# place in the bundle (for BUNDLED-PACKAGES.txt).
work=$(mktemp -d "${TMPDIR:-/var/tmp}/transom-macos.XXXXXX")
trap 'rm -rf "$work"' EXIT
: > "$work/binaries"
export PKG_CONFIG_PATH="$BREW/lib/pkgconfig"
export LIBCLANG_PATH="$BREW/opt/llvm/lib"   # bindgen
TARGET=${CARGO_BUILD_TARGET:-aarch64-apple-darwin}
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

if (( BUILD )); then
    info "cargo build --release --target $TARGET -p transom"
    cargo build --release --locked --target "$TARGET" -p transom
fi
BIN_SRC="target/$TARGET/release/transom"
[[ -x $BIN_SRC ]] || fail "missing $BIN_SRC"

APP=dist/Transom.app
C=$APP/Contents
rm -rf "$APP"; mkdir -p "$C/MacOS" "$C/Frameworks" "$C/Resources/share" "$C/Resources/lib"
cp "$BIN_SRC" "$C/MacOS/transom-bin"
cp "$BREW/bin/gdk-pixbuf-query-loaders" "$C/MacOS/"
printf '%s\t%s\n' "$BREW/bin/gdk-pixbuf-query-loaders" MacOS/gdk-pixbuf-query-loaders >> "$work/binaries"

# ---- resources: icons, schemas, pixbuf loaders ----
mkdir -p "$C/Resources/share/icons" "$C/Resources/share/glib-2.0/schemas"
cp -RL "$BREW/share/icons/Adwaita" "$C/Resources/share/icons/"
cp -RL "$BREW/share/icons/hicolor" "$C/Resources/share/icons/" 2>/dev/null || mkdir -p "$C/Resources/share/icons/hicolor"
cp -R data/icons/hicolor/. "$C/Resources/share/icons/hicolor/"
cp "$BREW"/share/glib-2.0/schemas/*.xml "$C/Resources/share/glib-2.0/schemas/"
glib-compile-schemas "$C/Resources/share/glib-2.0/schemas"
cp -RL "$BREW/lib/gdk-pixbuf-2.0" "$C/Resources/lib/"
rm -f "$C/Resources/lib/gdk-pixbuf-2.0/2.10.0/loaders.cache"
# librsvg installs its loader as both .so and .dylib; only .so is registered.
for d in "$C"/Resources/lib/gdk-pixbuf-2.0/2.10.0/loaders/*.dylib; do
    [[ -f ${d%.dylib}.so ]] && rm -f "$d"
done
iconutil -c icns data/transom.iconset -o "$C/Resources/transom.icns"
cp COPYING build-aux/packaging/THIRD-PARTY-NOTICES.txt build-aux/packaging/RUST-CRATE-LICENSES.txt \
    "$C/Resources/"

# ---- dylib closure ----
info "bundling dylibs"
fw="@executable_path/../Frameworks"
resolve() { # LIBREF BIN → source path
    local ref=$1 bin=$2 base cand
    base=$(basename "$ref")
    case $ref in
        @rpath/*|@loader_path/*|@executable_path/*)
            for cand in "$C/Frameworks/$base" "$BREW/lib/$base" "$BREW"/opt/*/lib/"$base"; do
                [[ -f $cand ]] && { echo "$cand"; return; }
            done
            return 1 ;;
        *) [[ -f $ref ]] && echo "$ref" ;;
    esac
}
process() { # BIN: copy its non-system dependencies, rewrite references; echo new libs
    local bin=$1 ref src base self
    self=$(otool -D "$bin" | tail -n +2)   # a dylib's own install name isn't a dependency
    while read -r ref; do
        [[ -n $self && $ref == "$self" ]] && continue
        case $ref in /usr/lib/*|/System/*) continue ;; esac
        src=$(resolve "$ref" "$bin") || fail "can't resolve $ref (from $bin)"
        base=$(basename "$src")
        if [[ ! -f $C/Frameworks/$base ]]; then
            cp -L "$src" "$C/Frameworks/$base"; chmod u+w "$C/Frameworks/$base"
            printf '%s\t%s\n' "$src" "Frameworks/$base" >> "$work/binaries"
            install_name_tool -id "$fw/$base" "$C/Frameworks/$base" 2>/dev/null
            echo "$C/Frameworks/$base"
        fi
        install_name_tool -change "$ref" "$fw/$base" "$bin" 2>/dev/null
    done < <(otool -L "$bin" | tail -n +2 | awk '{print $1}')
}
queue=("$C/MacOS/transom-bin" "$C/MacOS/gdk-pixbuf-query-loaders")
while IFS= read -r -d '' l; do
    chmod u+w "$l"; queue+=("$l")
    # A binary like the dylibs: its source must resolve into a Cellar keg (checked in
    # write_bundled_packages), not just be skipped by the data-tree scan.
    rel=${l#"$C/Resources/lib/"}
    printf '%s\t%s\n' "$BREW/lib/$rel" "Resources/lib/$rel" >> "$work/binaries"
    # Loaders are dlopen()ed by path; give any that carry a Homebrew install name a local one.
    [[ -n $(otool -D "$l" | tail -n +2) ]] && install_name_tool -id "@loader_path/$(basename "$l")" "$l" 2>/dev/null
done < <(find "$C/Resources/lib/gdk-pixbuf-2.0" \( -name '*.so' -o -name '*.dylib' \) -print0)
passes=0
while (( ${#queue[@]} )); do
    next=()
    for b in "${queue[@]}"; do
        while IFS= read -r n; do [[ -n $n ]] && next+=("$n"); done < <(process "$b")
    done
    queue=("${next[@]+"${next[@]}"}")
    (( ++passes < 40 )) || fail "dylib closure didn't converge"
done
info "bundled $(ls "$C/Frameworks" | wc -l | tr -d ' ') dylibs"

# ---- bundled Homebrew packages ----
# Record the formula and exact version (the Cellar keg) behind every bundled file, so
# each release names the sources THIRD-PARTY-NOTICES.txt points to
# (Resources/BUNDLED-PACKAGES.txt), and copy the licence files each keg ships
# (Resources/licenses/). A bundled binary from outside the Cellar fails the build: its
# source can't be identified.
keg_field() { # KEY FILE → the first "KEY": "string" value in a keg's JSON file
    [[ -f $2 ]] || return 0
    awk -v key="\"$1\"" '{
        i = index($0, key); if (!i) next
        s = substr($0, i + length(key))
        if (sub(/^[ \t]*:[ \t]*"/, "", s) && match(s, /^[^"]*"/)) { print substr(s, 1, RLENGTH - 1); exit }
    }' "$2"
}
write_bundled_packages() { # CELLAR RESOURCES TITLE DATA_TREE...; reads $work/binaries
    local cellar=$1 res=$2 title=$3 src dest phys rest outside="" tree
    local formula ver keg license src_url core_rev lic lics files
    shift 3
    while IFS=$'\t' read -r src dest; do
        phys=$(realpath "$src")
        if [[ $phys == "$cellar"/*/*/* ]]; then
            rest=${phys#"$cellar"/}
            formula=${rest%%/*}; rest=${rest#*/}
            printf '%s/%s\t%s\n' "$formula" "${rest%%/*}" "$dest"
        else
            outside+=" $src"
        fi
    done < "$work/binaries" > "$work/binary-kegs"
    [[ -z $outside ]] || fail "bundled binaries from outside Homebrew's Cellar:$outside"
    # Data trees (icons, schemas, pixbuf loaders): every file or link below them that
    # resolves into a keg. Broken links and generated caches match none.
    for tree in "$@"; do
        [[ -e $tree ]] || continue
        find "$tree" \( -type f -o -type l \) -exec realpath {} + 2>/dev/null || true
    done | sed -n "s|^$cellar/\([^/]*\)/\([^/]*\)/.*|\1/\2|p" | sort -u > "$work/data-kegs"

    {
        printf '%s\n%s\n\n' "$title" "$(printf '%*s' "${#title}" '' | tr ' ' =)"
        cat <<'TXT'
Every library, program, loader, icon and schema in Transom.app that Transom does
not build itself comes from one of the Homebrew formulae below; the version is
that of the Cellar keg it was copied from. Each entry gives the licence and
source the keg's SBOM records, the formula (the build recipe, kept in
https://github.com/Homebrew/homebrew-core), the licence files copied into
licenses/, and what it provided. See THIRD-PARTY-NOTICES.txt.
TXT
        while IFS=/ read -r formula ver; do
            keg="$cellar/$formula/$ver"
            # Homebrew records the formula's licence as licenseConcluded.
            license=$(keg_field licenseConcluded "$keg/sbom.spdx.json")
            [[ -n $license && $license != NOASSERTION ]] \
                || license=$(keg_field licenseDeclared "$keg/sbom.spdx.json")
            [[ -n $license && $license != NOASSERTION ]] || license="not recorded in the keg"
            src_url=$(keg_field downloadLocation "$keg/sbom.spdx.json")
            [[ -n $src_url && $src_url != NOASSERTION ]] || src_url="see the formula"
            core_rev=$(keg_field tap_git_head "$keg/INSTALL_RECEIPT.json")
            lics=""
            while IFS= read -r lic; do
                [[ -n $lic ]] || continue
                mkdir -p "$res/licenses/$formula"
                cp "$lic" "$res/licenses/$formula/"
                lics+="${lics:+, }licenses/$formula/${lic##*/}"
            done < <(find "$keg" -maxdepth 1 -type f \( -iname 'COPYING*' -o -iname 'LICEN[CS]E*' \
                         -o -iname 'NOTICE*' -o -iname 'COPYRIGHT*' \) | LC_ALL=C sort)
            files=$(awk -F'\t' -v k="$formula/$ver" \
                '$1 == k { printf "%s%s", (n++ ? ", " : ""), $2 }' "$work/binary-kegs")
            if grep -qxF "$formula/$ver" "$work/data-kegs"; then files+="${files:+, }data files"; fi
            printf '\n%s %s\n  License: %s\n  Source: %s\n' "$formula" "$ver" "$license" "$src_url"
            printf '  Formula: https://formulae.brew.sh/formula/%s' "$formula"
            if [[ $core_rev =~ ^[0-9a-f]{7,40}$ ]]; then printf ' (homebrew-core %s)' "$core_rev"; fi
            printf '\n  License files: %s\n  Bundled: %s\n' \
                "${lics:-none shipped by the keg; see its source}" "$files"
        done < <(sort -u <(cut -f1 "$work/binary-kegs") "$work/data-kegs")
    } > "$res/BUNDLED-PACKAGES.txt"
}
info "recording bundled Homebrew packages"
CELLAR=$(realpath "$(brew --cellar)")
write_bundled_packages "$CELLAR" "$C/Resources" "Transom $VERSION for macOS: bundled Homebrew packages" \
    "$BREW/share/icons/Adwaita" "$BREW/share/icons/hicolor" "$BREW/lib/gdk-pixbuf-2.0" \
    "$BREW"/share/glib-2.0/schemas/*.xml
info "recorded $(grep -c '^  Bundled:' "$C/Resources/BUNDLED-PACKAGES.txt") Homebrew formulae in BUNDLED-PACKAGES.txt"

# ---- launcher, Info.plist ----
cat > "$C/MacOS/Transom" <<'SH'
#!/bin/bash
# Point GTK at the bundle's resources, write the GDK-Pixbuf loader cache for this
# location, then run Transom.
C="$(cd "$(dirname "$0")/.." && pwd)"
R="$C/Resources"
export XDG_DATA_DIRS="$R/share"
export GSETTINGS_SCHEMA_DIR="$R/share/glib-2.0/schemas"
export GTK_DATA_PREFIX="$R"
export GTK_PATH="$R/lib/gtk-4.0"
cache_dir="${HOME}/Library/Caches/io.github.jm2.Transom"
mkdir -p "$cache_dir"
export GDK_PIXBUF_MODULE_FILE="$cache_dir/loaders.cache"
export GDK_PIXBUF_MODULEDIR="$R/lib/gdk-pixbuf-2.0/2.10.0/loaders"
# GLib prints debug messages on stdout: keep them out of the cache.
G_MESSAGES_DEBUG= "$C/MacOS/gdk-pixbuf-query-loaders" > "$GDK_PIXBUF_MODULE_FILE.tmp" 2>/dev/null \
    && mv "$GDK_PIXBUF_MODULE_FILE.tmp" "$GDK_PIXBUF_MODULE_FILE"
exec "$C/MacOS/transom-bin" "$@"
SH
chmod +x "$C/MacOS/Transom"

# Homebrew bottles target the macOS they were built for: declare the newest minimum.
minos=$(for f in "$C/MacOS/transom-bin" "$C"/Frameworks/*.dylib; do
            otool -l "$f" | awk '/LC_BUILD_VERSION/{b=1} b&&/minos/{print $2; b=0}'
        done | sort -V | tail -1)
cat > "$C/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Transom</string>
  <key>CFBundleDisplayName</key><string>Transom</string>
  <key>CFBundleIdentifier</key><string>io.github.jm2.Transom</string>
  <key>CFBundleExecutable</key><string>Transom</string>
  <key>CFBundleIconFile</key><string>transom</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>CFBundleShortVersionString</key><string>${VERSION%%-*}</string>
  <key>LSMinimumSystemVersion</key><string>${minos:-12.0}</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSHumanReadableCopyright</key><string>Copyright © 2026 Transom contributors</string>
  <key>CFBundleURLTypes</key>
  <array><dict>
    <key>CFBundleURLName</key><string>Remote Desktop Protocol</string>
    <key>CFBundleURLSchemes</key><array><string>rdp</string></array>
  </dict></array>
  <key>CFBundleDocumentTypes</key>
  <array><dict>
    <key>CFBundleTypeName</key><string>Remote Desktop Connection</string>
    <key>CFBundleTypeRole</key><string>Viewer</string>
    <key>CFBundleTypeExtensions</key><array><string>rdp</string></array>
    <key>LSHandlerRank</key><string>Alternate</string>
  </dict></array>
</dict>
</plist>
PLIST

# ---- ad-hoc signing (inside out) ----
info "signing (ad hoc)"
find "$C/Frameworks" "$C/Resources/lib" \( -name '*.dylib' -o -name '*.so' \) -print0 |
    xargs -0 codesign --force --sign -
codesign --force --sign - "$C/MacOS/gdk-pixbuf-query-loaders" "$C/MacOS/transom-bin"
codesign --force --deep --sign - "$APP"
codesign --verify --deep --strict "$APP"
# No reference may point outside the bundle (or at Homebrew).
machos=("$C/MacOS/transom-bin" "$C/MacOS/gdk-pixbuf-query-loaders")
while IFS= read -r -d '' f; do machos+=("$f"); done \
    < <(find "$C/Frameworks" "$C/Resources/lib" -type f \( -name '*.dylib' -o -name '*.so' \) -print0)
if otool -L "${machos[@]}" | grep -E "^\s+($BREW|/usr/local|/opt/homebrew|@rpath/)"; then
    fail "the bundle still references Homebrew libraries"
fi
info "wrote $APP (minimum macOS ${minos:-12.0})"

if (( DMG )); then
    command -v create-dmg >/dev/null || fail "create-dmg not found (brew install create-dmg)"
    rm -f dist/Transom.dmg dist/rw.*.Transom.dmg
    dmg_args=(--volname Transom --app-drop-link 450 120 --icon Transom.app 150 120 --window-size 600 300)
    # The window layout is applied by scripting Finder, which needs a GUI login and
    # Automation permission; without them, build a plain (unstyled) DMG.
    if ! create-dmg "${dmg_args[@]}" dist/Transom.dmg "$APP"; then
        echo "build-macos: Finder layout failed; building the DMG without it" >&2
        rm -f dist/Transom.dmg dist/rw.*.Transom.dmg
        create-dmg "${dmg_args[@]}" --skip-jenkins dist/Transom.dmg "$APP"
    fi
    info "wrote dist/Transom.dmg"
fi
