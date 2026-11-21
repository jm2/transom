# 0005 — Clipboard file transfer: lazy temp-dir staging

- Status: accepted
- Date: 2026-11-21

## Context

MS-RDPECLIP transfers files as a `FileGroupDescriptorW` (names, sizes, attributes)
plus on-demand `FileContents` requests for sizes and byte ranges. Locally, Linux apps
expect `text/uri-list` / `x-special/gnome-copied-files` pointing at real files. PLAN §7
leaves the choice between a FUSE filesystem and temp-dir staging to us.

## Decision

**Remote → local: lazy staging in a private cache directory.**
- When the remote offers files, Transom fetches only the descriptor and publishes a
  content provider (`TransomRemoteFiles`) that offers `text/uri-list` and
  `x-special/gnome-copied-files`.
- Nothing is downloaded until something actually pastes. Then the files are fetched in
  4 MiB ranges into `~/.cache/transom/clipboard/<session>/<n>/`, and their `file://` URIs
  are returned. Later pastes reuse the staged copy.
- The session's staging directory is deleted when the session ends.
- Remote names go through `sanitize_relative_path`: no `..`, absolute paths, drive
  letters or control characters. The descriptor is capped at 100 000 entries.

**Local → remote:**
- Offer `FileGroupDescriptorW` when the local clipboard holds a file list.
- Directories are walked recursively, without following symlinked directories, to build
  the descriptor. Ranges are read on a worker thread on request.

Capabilities advertised: `CB_STREAM_FILECLIP_ENABLED | CB_FILECLIP_NO_FILE_PATHS |
CB_HUGE_FILE_SUPPORT_ENABLED` (64-bit offsets).

## Why not FUSE

FUSE would make files appear without copying them first, and would stream huge files.
But it needs `/dev/fuse` and `fusermount3`, which works poorly inside Flatpak, and adds
a privileged-ish moving part for little gain in the common case (a few files, copied
straight into a folder by Nautilus, which copies them anyway). We can revisit for
multi-GB transfers.

## Verification

`scripts/clipboard-check.sh` (files pass) copies a text file and a folder containing a
3 MB random file in each direction through `TransomView` and grd, and compares
SHA-256 hashes: identical both ways.
