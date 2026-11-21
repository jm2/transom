#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Source tarballs for distro packages: dist/transom-VERSION.tar.gz (git HEAD) and
# dist/transom-VERSION-vendor.tar.xz (all crates, for offline builds).
set -euo pipefail
cd "$(dirname "$0")/.."
ver=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
mkdir -p dist
git archive --format=tar.gz --prefix="transom-$ver/" -o "dist/transom-$ver.tar.gz" HEAD
tmp=$(mktemp -d -p "${TMPDIR:-/var/tmp}")
trap 'rm -rf "$tmp"' EXIT
tar -xzf "dist/transom-$ver.tar.gz" -C "$tmp"
(cd "$tmp/transom-$ver" && cargo vendor --locked --versioned-dirs -q vendor >/dev/null)
tar -cJf "dist/transom-$ver-vendor.tar.xz" -C "$tmp/transom-$ver" vendor
ls -la dist/ | sed 1d
