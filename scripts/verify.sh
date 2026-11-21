#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Run before every commit (PLAN.md §9): fmt, clippy -D warnings, tests, cargo-deny.
#
#   scripts/verify.sh          # whole workspace (needs FreeRDP 3 + GTK 4 dev files)
#   scripts/verify.sh --core   # only transom-view-core (no system deps; used on macOS/Windows CI)
set -euo pipefail
cd "$(dirname "$0")/.."

if [[ "${1:-}" == "--core" ]]; then
    pkgs=(-p transom-view-core)
else
    pkgs=(--workspace)
fi

step() { printf '\n==> %s\n' "$*"; }

step "rustfmt"
cargo fmt --all -- --check

step "clippy"
cargo clippy "${pkgs[@]}" --all-targets --locked -- -D warnings

step "tests"
cargo test "${pkgs[@]}" --locked

if command -v cargo-deny >/dev/null 2>&1; then
    step "cargo-deny"
    cargo deny --locked check
else
    echo "warning: cargo-deny not installed (cargo install --locked cargo-deny); skipping" >&2
fi

if [[ "${1:-}" != "--core" ]]; then
    step "flatpak cargo sources"
    python3 -I build-aux/flatpak/cargo-sources.py Cargo.lock \
        | diff -q build-aux/flatpak/cargo-sources.json - >/dev/null \
        || { echo "build-aux/flatpak/cargo-sources.json is stale: run build-aux/flatpak/cargo-sources.py Cargo.lock > build-aux/flatpak/cargo-sources.json" >&2; exit 1; }
fi

step "ok"
