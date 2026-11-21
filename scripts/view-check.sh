#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Pixel-exactness check for TransomView without touching the host desktop:
# runs transom-view-demo --check in a headless mutter (container) at a given
# monitor scale against the grd test server (scripts/test-servers.sh up).
#
#   scripts/view-check.sh SCALE [MONITOR_WxH] [WINDOW_WxH] [extra demo args...]
#   scripts/view-check.sh 2 2560x1600 1100x700
set -euo pipefail
cd "$(dirname "$0")/.."
SCALE=${1:?scale}; MONITOR=${2:-2560x1600}; WINDOW=${3:-1100x700}; shift $(( $# < 3 ? $# : 3 ))
STATE="${TMPDIR:-/var/tmp}/transom-test"
[[ -f $STATE/grd.pw ]] || { echo "run scripts/test-servers.sh up first" >&2; exit 2; }
cargo build -q -p transom-view --example transom-view-demo
fp=$(podman logs transom-grd 2>&1 | sed -n 's/^TLS fingerprint: //p' | tail -1)
user=$(sed -n 1p "$STATE/grd.creds")
tag=$(echo "$SCALE" | tr . _)
podman run --rm --network host --userns=keep-id \
    -e SCALE="$SCALE" -e MONITOR="$MONITOR" -e TRANSOM_CHECK_INVALIDATE -e SCALE2 -e SCALE2_AFTER \
    -v "$PWD/target/debug/examples:/demo:ro,Z" \
    -v "$PWD/tests/servers/view-check/inner.sh:/inner.sh:ro,Z" \
    -v "$STATE:/state:Z" \
    --entrypoint /inner.sh transom-test-grd \
    localhost:13389 "$user" --password-file /state/grd.pw --trust "$fp" \
    --size "$WINDOW" --check --screenshot "/state/view-$tag.png" "$@"
