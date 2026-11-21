#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Accessibility audit: run the app headlessly and walk its AT-SPI tree (what Orca
# reads), failing on interactive controls without an accessible name. Checks the
# connection list and a live session. Needs scripts/test-servers.sh up.
set -euo pipefail
cd "$(dirname "$0")/.."
fail=0
run() {
    local name=$1; shift
    out=$(INJECT=/checks/a11y-audit.py "$@" 2>&1 | grep -E '\[inject\] (a11y|A11Y)' | sed 's/\[inject\] //')
    echo "== $name"; echo "$out"
    grep -q "A11Y OK" <<<"$out" || fail=1
}
run "connection list" env INJECT_AFTER=5 scripts/app-shot.sh a11y-list 1 9 --seed-profile
run "session" env INJECT_AFTER=9 scripts/app-shot.sh a11y-session 1 13 --seed-trust -- \
    --password-file /state/grd.pw rdp://transom@localhost:13389
exit $fail
