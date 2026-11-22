#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Build the system FreeRDP's version with the MS-RDPEAR channel (Remote Credential
# Guard), which distribution builds such as Fedora's leave out, into
# ${TMPDIR:-/var/tmp}/transom-test/freerdp-rdpear/lib64, in a throwaway Fedora
# container with Fedora's build dependencies. scripts/ad-check.sh uses it when present
# (the Flatpak enables the channel itself). Same version as the system library, so the
# examples run against it with LD_LIBRARY_PATH, unrebuilt.
#   scripts/build-freerdp-rdpear.sh
set -euo pipefail
cd "$(dirname "$0")/.."
OUT="${TMPDIR:-/var/tmp}/transom-test/freerdp-rdpear"
ver=$(pkg-config --modversion freerdp3)
fedora=$(rpm -E %fedora)
work=$(mktemp -d -p "${TMPDIR:-/var/tmp}")
trap 'rm -rf "$work"' EXIT
curl -fsSL -o "$work/freerdp.spec" "https://src.fedoraproject.org/rpms/freerdp/raw/f$fedora/f/freerdp.spec" ||
    curl -fsSL -o "$work/freerdp.spec" https://src.fedoraproject.org/rpms/freerdp/raw/rawhide/f/freerdp.spec
cat > "$work/build.sh" <<SH
set -euo pipefail
dnf -y -q install dnf-plugins-core git cmake ninja-build gcc gcc-c++ >/dev/null
dnf -y -q builddep /b/freerdp.spec >/dev/null
git clone -q --depth 1 -b $ver https://github.com/FreeRDP/FreeRDP.git /b/src
cmake -S /b/src -B /b/build -G Ninja -DCMAKE_BUILD_TYPE=RelWithDebInfo -DCMAKE_INSTALL_PREFIX=/b/prefix \\
  -DCMAKE_INSTALL_LIBDIR=lib64 -DWITH_KRB5=ON -DCHANNEL_RDPEAR=ON -DCHANNEL_RDPEAR_CLIENT=ON \\
  -DWITH_CLIENT_SDL=OFF -DWITH_X11=OFF -DWITH_WAYLAND=OFF -DWITH_SERVER=OFF -DWITH_SAMPLE=OFF \\
  -DWITH_MANPAGES=OFF -DWITH_OPENH264=ON -DWITH_FFMPEG=OFF -DWITH_DSP_FFMPEG=OFF -DWITH_SWSCALE=OFF \\
  -DWITH_PULSE=ON -DWITH_CUPS=OFF -DWITH_FUSE=OFF -DWITH_PCSC=ON -DWITH_PKCS11=OFF -DWITH_AAD=ON \\
  -DWITH_JSONC_REQUIRED=ON -DBUILD_TESTING=OFF -DWITH_WEBVIEW=OFF -DWITH_FDK_AAC=OFF -DWITH_LAME=OFF \\
  -DWITH_GSM=OFF -DWITH_SOXR=OFF -DWITH_GFX_AV1=OFF >/dev/null
ninja -C /b/build install >/dev/null
SH
podman run --rm -v "$work:/b:Z" "registry.fedoraproject.org/fedora:$fedora" bash /b/build.sh
strings "$work/prefix/lib64/libfreerdp-client3.so.3" | grep -q rdpear ||
    { echo "built FreeRDP has no rdpear channel" >&2; exit 1; }
rm -rf "$OUT"; mkdir -p "$OUT"
cp -a "$work/prefix/lib64" "$OUT/"
echo "FreeRDP $ver with MS-RDPEAR: $OUT/lib64"
