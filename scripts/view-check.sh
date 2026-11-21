#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Pixel-exactness check for TransomView without touching the host desktop:
# runs transom-view-demo --check in a headless mutter (container) at a given
# monitor scale against a test server: TARGET=grd (default; scripts/test-servers.sh up),
# win11 or server (scripts/winvm.sh).
#
#   scripts/view-check.sh SCALE [MONITOR_WxH] [WINDOW_WxH] [extra demo args...]
#   scripts/view-check.sh 2 2560x1600 1100x700
set -euo pipefail
cd "$(dirname "$0")/.."
SCALE=${1:?scale}; MONITOR=${2:-2560x1600}; WINDOW=${3:-1100x700}; shift $(( $# < 3 ? $# : 3 ))
source scripts/lib/target.sh
cargo build -q -p transom-view --example transom-view-demo
tag=$TARGET-$(echo "$SCALE" | tr . _)
podman run --rm --network host --userns=keep-id \
    -e SCALE="$SCALE" -e MONITOR="$MONITOR" -e TRANSOM_CHECK_INVALIDATE -e SCALE2 -e SCALE2_AFTER \
    -v "$PWD/target/debug:/demo:ro,Z" \
    -v "$PWD/tests/servers/view-check/inner.sh:/inner.sh:ro,Z" \
    -v "$STATE:/state:Z" \
    --entrypoint /inner.sh transom-test-grd \
    "$T_ADDR" "$T_USER" --password-file /state/target.pw --trust "$T_FP" \
    --size "$WINDOW" --check --screenshot "/state/view-$tag.png" "$@"
