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
for t in brew otool install_name_tool codesign iconutil; do
    command -v "$t" >/dev/null || fail "$t not found"
done
BREW=$(brew --prefix)
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

# ---- resources: icons, schemas, pixbuf loaders ----
mkdir -p "$C/Resources/share/icons" "$C/Resources/share/glib-2.0/schemas"
cp -RL "$BREW/share/icons/Adwaita" "$C/Resources/share/icons/"
cp -RL "$BREW/share/icons/hicolor" "$C/Resources/share/icons/" 2>/dev/null || mkdir -p "$C/Resources/share/icons/hicolor"
cp -R data/icons/hicolor/. "$C/Resources/share/icons/hicolor/"
cp "$BREW"/share/glib-2.0/schemas/*.xml "$C/Resources/share/glib-2.0/schemas/"
glib-compile-schemas "$C/Resources/share/glib-2.0/schemas"
cp -RL "$BREW/lib/gdk-pixbuf-2.0" "$C/Resources/lib/"
rm -f "$C/Resources/lib/gdk-pixbuf-2.0/2.10.0/loaders.cache"
iconutil -c icns data/transom.iconset -o "$C/Resources/transom.icns"
cp COPYING build-aux/packaging/THIRD-PARTY-NOTICES.txt "$C/Resources/"

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
    local bin=$1 ref src base
    while read -r ref; do
        case $ref in /usr/lib/*|/System/*) continue ;; esac
        src=$(resolve "$ref" "$bin") || fail "can't resolve $ref (from $bin)"
        base=$(basename "$src")
        if [[ ! -f $C/Frameworks/$base ]]; then
            cp -L "$src" "$C/Frameworks/$base"; chmod u+w "$C/Frameworks/$base"
            install_name_tool -id "$fw/$base" "$C/Frameworks/$base" 2>/dev/null
            echo "$C/Frameworks/$base"
        fi
        install_name_tool -change "$ref" "$fw/$base" "$bin" 2>/dev/null
    done < <(otool -L "$bin" | tail -n +2 | awk '{print $1}' | grep -v "^$(basename "$bin")$")
}
queue=("$C/MacOS/transom-bin" "$C/MacOS/gdk-pixbuf-query-loaders")
while IFS= read -r -d '' l; do chmod u+w "$l"; queue+=("$l"); done \
    < <(find "$C/Resources/lib/gdk-pixbuf-2.0" -name '*.so' -print0)
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
"$C/MacOS/gdk-pixbuf-query-loaders" > "$GDK_PIXBUF_MODULE_FILE.tmp" 2>/dev/null \
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
if otool -L "$C/MacOS/transom-bin" "$C"/Frameworks/*.dylib | grep -E "^\s+$BREW|^\s+/usr/local|^\s+/opt/homebrew"; then
    fail "the bundle still references Homebrew libraries"
fi
info "wrote $APP (minimum macOS ${minos:-12.0})"

if (( DMG )); then
    command -v create-dmg >/dev/null || fail "create-dmg not found (brew install create-dmg)"
    rm -f dist/Transom.dmg
    create-dmg --volname Transom --app-drop-link 450 120 --icon Transom.app 150 120 \
        --window-size 600 300 dist/Transom.dmg "$APP"
    info "wrote dist/Transom.dmg"
fi
