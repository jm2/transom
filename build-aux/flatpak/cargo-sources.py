#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Generate flatpak-builder sources that vendor every crates.io dependency in
Cargo.lock, so the Flatpak builds offline (Flathub requirement).

Usage: build-aux/flatpak/cargo-sources.py Cargo.lock > build-aux/flatpak/cargo-sources.json
"""
import json
import sys
import tomllib

lock = tomllib.load(open(sys.argv[1] if len(sys.argv) > 1 else "Cargo.lock", "rb"))
sources = []
for pkg in lock["package"]:
    src = pkg.get("source", "")
    if not src.startswith("registry+https://github.com/rust-lang/crates.io-index"):
        if src:
            sys.exit(f"unsupported source for {pkg['name']}: {src}")
        continue  # workspace member
    name, ver, sha = pkg["name"], pkg["version"], pkg["checksum"]
    dest = f"cargo/vendor/{name}-{ver}"
    sources.append({
        "type": "archive",
        "archive-type": "tar-gzip",
        "url": f"https://static.crates.io/crates/{name}/{name}-{ver}.crate",
        "sha256": sha,
        "dest": dest,
    })
    sources.append({
        "type": "inline",
        "contents": json.dumps({"package": sha, "files": {}}),
        "dest": dest,
        "dest-filename": ".cargo-checksum.json",
    })
sources.append({
    "type": "inline",
    "contents": '[source.vendored-sources]\ndirectory = "cargo/vendor"\n\n'
                '[source.crates-io]\nreplace-with = "vendored-sources"\n',
    "dest": "cargo",
    "dest-filename": "config.toml",
})
json.dump(sources, sys.stdout, indent=2)
print()
