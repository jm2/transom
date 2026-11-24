#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Write build-aux/packaging/RUST-CRATE-LICENSES.txt: the licence texts of the Rust
# crates compiled into the transom app (MIT, BSD, Apache-2.0, ... require them in
# binary distributions). Every package ships the file (docs/packaging.md). It is
# committed, so offline builds (Flatpak, RPM, Arch) need no cargo-about; rerun this
# after Cargo.lock changes, and scripts/verify.sh fails while it is stale.
#
#   scripts/gen-licenses.sh            # rewrite the file
#   scripts/gen-licenses.sh --check    # exit 1 if it doesn't match Cargo.lock
#
# Configuration: about.toml (accepted licences, targets); template:
# build-aux/packaging/about.hbs. Needs cargo-about (CI installs this version):
#   cargo install --locked cargo-about --version 0.9.3 --features cli
set -euo pipefail
cd "$(dirname "$0")/.."

out=build-aux/packaging/RUST-CRATE-LICENSES.txt
pinned=0.9.3
check=0
case ${1:-} in
    --check) check=1 ;;
    "") ;;
    *) echo "usage: $0 [--check]" >&2; exit 2 ;;
esac

if ! command -v cargo-about >/dev/null 2>&1; then
    echo "gen-licenses: cargo-about not found (cargo install --locked cargo-about --version $pinned --features cli)" >&2
    exit 1
fi
version=$(cargo about --version | awk '{print $2}')
if [[ $version != "$pinned" ]]; then
    echo "gen-licenses: warning: cargo-about $version, not $pinned; the output may differ from CI's" >&2
fi

tmp=$(mktemp -p "${TMPDIR:-/var/tmp}")
trap 'rm -f "$tmp"' EXIT
# Download every target's crates, then generate offline: the result depends only on
# Cargo.lock and the crates' own licence files, never on what a git host serves.
cargo fetch --locked --quiet
# The transom package's graph (not the whole workspace): what the shipped binary
# contains. Crate licence files come with CRLF line ends and trailing blanks; strip
# them, and the blank lines at the end, so the committed file is clean text.
cargo about generate --frozen --fail -c about.toml -m crates/transom/Cargo.toml \
        build-aux/packaging/about.hbs |
    awk '{ sub(/[ \t\r]+$/, "") }
         $0 == "" { blank++; next }
         { while (blank > 0) { print ""; blank-- } print }' > "$tmp"
# Crates without a licence file get the standard text with a "<copyright holders>"
# placeholder: put their upstream notices (build-aux/packaging/crate-notices.toml) in
# its place, and fail for a crate that has none listed.
python3 -I - "$tmp" build-aux/packaging/crate-notices.toml <<'PY'
import re
import sys
import tomllib

path, table = sys.argv[1], sys.argv[2]
notices = tomllib.load(open(table, "rb"))
placeholder = "Copyright (c) <year> <copyright holders>"
sections = open(path, encoding="utf-8").read().split("\n" + "-" * 80 + "\n")
for i, sec in enumerate(sections):
    if placeholder not in sec:
        continue
    crates = re.findall(r"^  (\S+) \S+", sec.split("\n\n")[1], re.M)
    missing = [c for c in crates if c not in notices]
    if missing:
        sys.exit(f"gen-licenses: no licence file and no notice in {table}: {', '.join(missing)}")
    lines = [
        "(These crates ship no licence file, so this is the licence's standard text;",
        "their copyright notices, from their source repositories:)",
    ]
    for c in crates:
        lines.append(f"  {c}: {notices[c]['notice']}")
        lines.append(f"    {notices[c]['source']}")
    sections[i] = sec.replace(placeholder, "\n".join(lines))
open(path, "w", encoding="utf-8").write(("\n" + "-" * 80 + "\n").join(sections))
PY

if (( check )); then
    if ! diff -q "$out" "$tmp" >/dev/null; then
        echo "$out is stale: run scripts/gen-licenses.sh" >&2
        exit 1
    fi
else
    cat "$tmp" > "$out"
    echo "wrote $out"
fi
