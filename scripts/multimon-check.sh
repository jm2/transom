#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Multi-monitor check: a headless mutter with two monitors at different scales
# (default 200% + 100%), transom-view-demo --multimon opens one fullscreen view per
# monitor, and each is compared with its region of the framebuffer.
#   scripts/multimon-check.sh [SCALE1 MONITOR1 SCALE2 MONITOR2]
set -euo pipefail
cd "$(dirname "$0")/.."
S1=${1:-2}; M1=${2:-2560x1600}; S2=${3:-1}; M2=${4:-1920x1080}
STATE="${TMPDIR:-/var/tmp}/transom-test"
cargo build -q -p transom-view --example transom-view-demo
fp=$(podman logs transom-grd 2>&1 | sed -n 's/^TLS fingerprint: //p' | tail -1)
out=$(podman run --rm --network host --userns=keep-id \
    -e SCALE="$S1" -e MONITOR="$M1" -e MONITOR2="$M2" -e SCALE2_STATIC="$S2" \
    -e BIN=/demo/examples/transom-view-demo \
    -v "$PWD/target/debug:/demo:ro,Z" \
    -v "$PWD/tests/servers/view-check/inner.sh:/inner.sh:ro,Z" \
    -v "$STATE:/state:Z" \
    --entrypoint /inner.sh transom-test-grd \
    localhost:13389 transom --password-file /state/grd.pw --trust "$fp" \
    --multimon --check --screenshot /state/multimon.png 2>&1 || true)
echo "$out" | grep -E "^(layout|remote desktop|monitor [0-9]|check)" | sed 's/, surface offset[^)]*)//'
n=$(echo "$out" | grep -c "PIXEL-EXACT" || true)
[[ $n -ge 2 ]] && echo "MULTIMON OK" || { echo "MULTIMON FAILED"; exit 1; }
