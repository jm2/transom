#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Run the Transom app in a headless mutter (container) and save a screenshot of its
# window after a delay. Uses a throwaway config dir; the grd test server can be
# reached at localhost:13389 (scripts/test-servers.sh up).
#
#   scripts/app-shot.sh NAME SCALE SECONDS [--seed-trust] [--seed-profile]
#                       [--trust HOST:PORT SHA256] [-- transom args...]
#   -> ${TMPDIR:-/var/tmp}/transom-test-UID/app-NAME.png
set -euo pipefail
cd "$(dirname "$0")/.."
NAME=${1:?name}; SCALE=${2:?scale}; SECS=${3:?seconds}; shift 3
source scripts/lib/state.sh
CFG="$STATE/app-config-$NAME"
rm -rf "$CFG"; mkdir -p "$CFG/transom/profiles"
declare -A trust=()   # HOST:PORT -> SHA256; a later option for the same host wins
fp=$(podman logs transom-grd 2>/dev/null | sed -n 's/^TLS fingerprint: //p' | tail -1 || true)
while [[ $# -gt 0 && $1 != "--" ]]; do
    case $1 in
        --seed-trust) trust[localhost:13389]=$fp ;;
        --trust) # HOST:PORT SHA256 (e.g. a Windows test VM)
            trust[$2]=$3; shift 2 ;;
        --seed-multimon)
            printf 'name = "Multi"\nhost = "localhost"\nport = 13389\nusername = "transom"\nmultimon = true\n' > "$CFG/transom/profiles/multi.toml" ;;
        --seed-profile)
            for n in "GNOME Test Server:localhost:13389:transom" "Office Desktop:pc.corp.example:3389:alex" "Lab Windows 11:192.168.50.20:3389:admin"; do
                IFS=: read -r name host port user <<<"$n"
                id=$(echo "$name" | tr 'A-Z ' 'a-z-')
                printf 'name = "%s"\nhost = "%s"\nport = %s\nusername = "%s"\n' "$name" "$host" "$port" "$user" > "$CFG/transom/profiles/$id.toml"
            done ;;
    esac
    shift
done
# Every --seed-trust/--trust host once (TOML forbids duplicate keys), in one [hosts] table.
if (( ${#trust[@]} )); then
    { echo '[hosts]'; for h in "${!trust[@]}"; do printf '"%s" = "%s"\n' "$h" "${trust[$h]}"; done; } \
        > "$CFG/transom/known-hosts.toml"
fi
[[ ${1:-} == "--" ]] && shift
cargo build -q -p transom
podman run --rm --network host --userns=keep-id \
    -e SCALE="$SCALE" -e MONITOR="${MONITOR:-2560x1600}" -e BIN=/demo/transom \
    -e MONITOR2="${MONITOR2:-}" -e SCALE2_STATIC="${SCALE2_STATIC:-}" \
    -e TRANSOM_DEBUG_ACTIONS="${TRANSOM_DEBUG_ACTIONS:-}" -e RUST_LOG="${RUST_LOG:-}" \
    -e INJECT="${INJECT:-}" -e INJECT_AFTER="${INJECT_AFTER:-8}" -e A11Y_VERBOSE="${A11Y_VERBOSE:-}" \
    -v "$PWD/tests/servers/view-check:/checks:ro,Z" \
    -e XDG_CONFIG_HOME=/state/app-config-"$NAME" -e XDG_CACHE_HOME=/tmp/cache \
    -e TRANSOM_DEBUG_SCREENSHOT="/state/app-$NAME.png:$SECS" \
    -v "$PWD/target/debug:/demo:ro,Z" \
    -v "$PWD/tests/servers/view-check/inner.sh:/inner.sh:ro,Z" \
    -v "$STATE:/state:Z" \
    --entrypoint /inner.sh transom-test-grd "$@" 2>&1 | grep -v -E 'dbus-daemon\[|dbus-run-session|portal|fuse|SpiRegistry' || true
ls -la "$STATE/app-$NAME.png"
