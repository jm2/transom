#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Track the security-relevant libraries inside Transom's self-contained packages.

The AppImage (Debian 13 packages), macOS (Homebrew) and Windows (MSYS2) packages
bundle FreeRDP, FFmpeg, OpenSSL and the libraries that parse what a server sends
(images, XML, Kerberos, ...); the Flatpak builds FreeRDP and pcsc-lite from pinned
sources. None of them gets a distribution's security updates: a fix reaches users
only when Transom is rebuilt and released (SECURITY.md). This script compares what
those sources offer now with build-aux/packaging/bundled-versions.txt, the versions
recorded at the last release:

  scripts/bundled-versions.py --check   # print the changes (Markdown); exit 3 if any
  scripts/bundled-versions.py --write   # record the current versions (at a release)

Exit status 2: a source couldn't be queried, or didn't list every package (no report
is better than a wrong one, and the baseline is left as it was). Debian's versions
come from apt-cache with trixie-security, after a fresh apt-get update: natively on
Debian 13 when run as root, else in a debian:trixie container (podman or docker); MSYS2's from
packages.msys2.org; Homebrew's from formulae.brew.sh; the Flatpak's pins are
checked against the latest upstream releases (GitHub; GITHUB_TOKEN is used if set).
"""
import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import urllib.parse
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BASELINE = ROOT / "build-aux/packaging/bundled-versions.txt"
MANIFEST = ROOT / "build-aux/flatpak/io.github.jm2.Transom.yml"

# Code a hostile server's data reaches, or that secures the connection.
DEBIAN = [
    "libfreerdp3-3", "libfreerdp-client3-3", "libwinpr3-3",
    "libavcodec61", "libavutil59", "libswscale8", "libswresample5", "libdav1d7",
    "libssl3t64", "libgnutls30t64", "libkrb5-3",
    "libxml2", "libpng16-16t64", "libjpeg62-turbo", "libtiff6", "libwebp7",
    "libgdk-pixbuf-2.0-0", "librsvg2-2", "libglib2.0-0t64", "libgtk-4-1",
    "libcups2t64", "libpcsclite1", "libzstd1",
]
MSYS2 = [  # base names; the CLANG64 and CLANGARM64 builds share versions (no krb5:
    # FreeRDP uses Windows' own Kerberos, SSPI)
    "freerdp", "ffmpeg", "dav1d", "openssl", "gnutls",
    "libxml2", "libpng", "libjpeg-turbo", "libtiff", "libwebp",
    "gdk-pixbuf2", "librsvg", "glib2", "gtk4", "zstd",
]
HOMEBREW = [
    "freerdp", "ffmpeg", "dav1d", "openssl@3", "gnutls", "krb5",
    "libxml2", "libpng", "jpeg-turbo", "libtiff", "webp",
    "gdk-pixbuf", "librsvg", "glib", "gtk4", "zstd",
]
# The Flatpak's pinned sources: manifest URL pattern, and where upstream releases.
FLATPAK = {
    "freerdp": (r"freerdp-(\d[\d.]*)\.tar", "FreeRDP/FreeRDP", "releases"),
    "pcsc-lite": (r"pcsc-lite-(\d[\d.]*)\.tar", "LudovicRousseau/PCSC", "tags"),
}


class QueryError(Exception):
    pass


def fetch_json(url):
    headers = {"User-Agent": "transom-bundled-versions", "Accept": "application/json"}
    if url.startswith("https://api.github.com/") and os.environ.get("GITHUB_TOKEN"):
        headers["Authorization"] = f"Bearer {os.environ['GITHUB_TOKEN']}"
    try:
        with urllib.request.urlopen(urllib.request.Request(url, headers=headers), timeout=60) as r:
            return json.load(r)
    except (OSError, ValueError) as e:
        raise QueryError(f"{url}: {e}") from e


def debian_versions():
    script = (
        "apt-get update -qq >/dev/null 2>&1 || exit 3; "
        "apt-cache policy " + " ".join(DEBIAN) + " 2>/dev/null"
    )
    os_release = Path("/etc/os-release").read_text() if Path("/etc/os-release").exists() else ""
    # Natively only as root: the package lists must be refreshed first (a stale
    # list would hide a security update), and apt-get update needs root.
    if shutil.which("apt-cache") and "VERSION_CODENAME=trixie" in os_release and os.geteuid() == 0:
        cmd = ["sh", "-c", script]
    elif engine := shutil.which("podman") or shutil.which("docker"):
        cmd = [engine, "run", "--rm", "debian:trixie", "sh", "-c", script]
    else:
        raise QueryError("Debian: needs Debian 13 as root (apt-get update), or podman or docker for a debian:trixie container")
    out = subprocess.run(cmd, capture_output=True, text=True, check=False)
    if out.returncode:
        raise QueryError(f"Debian: {' '.join(cmd[:3])}… failed ({out.returncode}): {out.stderr.strip()[:300]}")
    versions, pkg = {}, None
    for line in out.stdout.splitlines():
        if re.match(r"^\S+:$", line):
            pkg = line[:-1]
        elif pkg and (m := re.match(r"^\s+Candidate:\s+(\S+)", line)) and m.group(1) != "(none)":
            versions[pkg] = m.group(1)
    return versions


def msys2_versions():
    versions = {}
    for name in MSYS2:
        # By the mingw-w64- base name: a plain name can match an MSYS package first.
        q = urllib.parse.urlencode({"query": f"mingw-w64-{name}", "qtype": "pkg"})
        exact = (fetch_json(f"https://packages.msys2.org/api/search?{q}").get("results") or {}).get("exact")
        if exact and exact.get("name") == f"mingw-w64-{name}" and "clang64" in exact.get("repos", []):
            versions[name] = exact["version"]
    return versions


def homebrew_versions():
    versions = {}
    for name in HOMEBREW:
        d = fetch_json(f"https://formulae.brew.sh/api/formula/{urllib.parse.quote(name)}.json")
        v = d["versions"]["stable"]
        versions[name] = f"{v}_{d['revision']}" if d.get("revision") else v
    return versions


def version_key(v):
    return [int(x) for x in re.findall(r"\d+", v)]


def flatpak_pins():
    text = MANIFEST.read_text()
    pins = {}
    for name, (pattern, _, _) in FLATPAK.items():
        m = re.search(pattern, text)
        if not m:
            raise QueryError(f"Flatpak: no {name} source in {MANIFEST.name}")
        pins[name] = m.group(1)
    return pins


def flatpak_upstream():
    latest = {}
    for name, (_, repo, kind) in FLATPAK.items():
        if kind == "releases":
            tag = fetch_json(f"https://api.github.com/repos/{repo}/releases/latest")["tag_name"]
        else:
            tags = [t["name"] for t in fetch_json(f"https://api.github.com/repos/{repo}/tags?per_page=100")]
            tags = [t for t in tags if re.fullmatch(r"\d+(\.\d+)+", t)]
            if not tags:
                raise QueryError(f"Flatpak: no release tags for {repo}")
            tag = max(tags, key=version_key)
        latest[name] = tag.lstrip("v")
    return latest


def current():
    """{(source, package): version} for every source."""
    found = {}
    for source, query in (("debian", debian_versions), ("msys2", msys2_versions),
                          ("homebrew", homebrew_versions), ("flatpak", flatpak_pins)):
        for pkg, ver in query().items():
            found[(source, pkg)] = ver
    return found


def read_baseline():
    rows = {}
    for line in BASELINE.read_text().splitlines():
        if line.strip() and not line.startswith("#"):
            source, pkg, ver = line.split("\t")
            rows[(source, pkg)] = ver
    return rows


SOURCE_NAMES = {
    "debian": "Debian 13 (AppImage)",
    "msys2": "MSYS2 (Windows)",
    "homebrew": "Homebrew (macOS)",
    "flatpak": "Flatpak (pinned in the manifest)",
}


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    mode = ap.add_mutually_exclusive_group(required=True)
    mode.add_argument("--check", action="store_true")
    mode.add_argument("--write", action="store_true")
    args = ap.parse_args()
    try:
        now = current()
        upstream = flatpak_upstream() if args.check else {}
    except QueryError as e:
        print(f"bundled-versions: {e}", file=sys.stderr)
        return 2
    expected = {("debian", p) for p in DEBIAN} | {("msys2", p) for p in MSYS2} \
        | {("homebrew", p) for p in HOMEBREW} | {("flatpak", p) for p in FLATPAK}
    missing = sorted(expected - now.keys())
    if missing:
        # A package a source didn't list can't be compared, and writing the baseline
        # without it would stop tracking it: neither mode goes on.
        print("bundled-versions: not found (renamed or removed? update the lists in "
              "scripts/bundled-versions.py): "
              + ", ".join(f"{p} ({SOURCE_NAMES[s]})" for s, p in missing), file=sys.stderr)
        return 2

    if args.write:
        lines = [
            "# The security-relevant libraries in Transom's self-contained packages, as",
            "# their sources offered them at the last release: scripts/bundled-versions.py",
            "# --write records them, and the weekly Bundled libraries workflow (--check)",
            "# opens an issue when they move. Source<TAB>package<TAB>version.",
        ]
        lines += [f"{s}\t{p}\t{v}" for (s, p), v in sorted(now.items())]
        BASELINE.write_text("\n".join(lines) + "\n")
        print(f"wrote {BASELINE.relative_to(ROOT)} ({len(now)} packages)")
        return 0

    base = read_baseline()
    changes = [(s, p, base.get((s, p), "(not recorded)"), v)
               for (s, p), v in sorted(now.items())
               if s != "flatpak" and base.get((s, p)) != v]
    for name, pin in flatpak_pins().items():
        if version_key(upstream[name]) > version_key(pin):
            changes.append(("flatpak", name, pin, f"{upstream[name]} (upstream release)"))
    if not changes:
        print("No changes since the last release.")
        return 0
    print("| Package | Source | At the last release | Now |")
    print("|---|---|---|---|")
    for s, p, was, ver in changes:
        print(f"| `{p}` | {SOURCE_NAMES[s]} | {was} | {ver} |")
    # Not 1: that is what Python exits with when the script itself fails.
    return 3


if __name__ == "__main__":
    sys.exit(main())
