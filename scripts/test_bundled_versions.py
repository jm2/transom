#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Offline tests for scripts/bundled-versions.py: the tracked lists, the recorded
baseline and the Flatpak manifest agree, and --check reports what moved."""

import contextlib
import importlib.util
import io
import sys
import unittest
from pathlib import Path
from unittest import mock

sys.dont_write_bytecode = True  # no scripts/__pycache__ left behind
spec = importlib.util.spec_from_file_location(
    "bundled_versions", Path(__file__).resolve().parent / "bundled-versions.py")
bv = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bv)


def tracked():
    return ({("debian", p) for p in bv.DEBIAN} | {("msys2", p) for p in bv.MSYS2}
            | {("homebrew", p) for p in bv.HOMEBREW} | {("flatpak", p) for p in bv.FLATPAK})


def run_check(now, upstream):
    """main(--check) with the sources replaced: (exit status, stdout, stderr)."""
    out, err = io.StringIO(), io.StringIO()
    with mock.patch.object(bv, "current", return_value=now), \
            mock.patch.object(bv, "flatpak_upstream", return_value=upstream), \
            mock.patch.object(sys, "argv", ["bundled-versions.py", "--check"]), \
            contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        status = bv.main()
    return status, out.getvalue(), err.getvalue()


class BaselineTest(unittest.TestCase):
    def test_baseline_records_exactly_the_tracked_packages(self):
        self.assertEqual(set(bv.read_baseline()), tracked())

    def test_baseline_flatpak_rows_are_the_manifest_pins(self):
        base = bv.read_baseline()
        for name, pin in bv.flatpak_pins().items():
            self.assertEqual(base[("flatpak", name)], pin, name)

    def test_appimage_freerdp_is_the_flatpak_pin(self):
        # The AppImage builds the Flatpak's FreeRDP: Debian's isn't tracked any more.
        self.assertIn("freerdp", bv.FLATPAK)
        self.assertFalse([p for p in bv.DEBIAN if "freerdp" in p or "winpr" in p])


class CheckTest(unittest.TestCase):
    def setUp(self):
        self.now = bv.read_baseline()
        self.upstream = bv.flatpak_pins()

    def test_no_changes(self):
        status, out, _ = run_check(self.now, self.upstream)
        self.assertEqual(status, 0)
        self.assertIn("No changes", out)

    def test_a_moved_package_is_reported(self):
        self.now[("debian", "libva2")] = "2.99.0-1"
        status, out, _ = run_check(self.now, self.upstream)
        self.assertEqual(status, 3)
        self.assertIn("| `libva2` | Debian 13 (AppImage) |", out)
        self.assertIn("2.99.0-1", out)

    def test_a_newer_upstream_freerdp_names_both_pinned_packages(self):
        self.upstream["freerdp"] = "99.0.0"
        status, out, _ = run_check(self.now, self.upstream)
        self.assertEqual(status, 3)
        self.assertIn("| `freerdp` | Flatpak and AppImage (pinned in the Flatpak manifest) |", out)

    def test_a_package_a_source_no_longer_lists_stops_the_check(self):
        del self.now[("homebrew", "openssl@4")]
        status, out, err = run_check(self.now, self.upstream)
        self.assertEqual(status, 2)
        self.assertEqual(out, "")
        self.assertIn("openssl@4 (Homebrew (macOS))", err)


if __name__ == "__main__":
    unittest.main()
