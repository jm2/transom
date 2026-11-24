#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Tests for scripts/unsafe-inventory.py: each unsafe site needs its own note."""

import importlib.util
import os
import tempfile
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "unsafe_inventory", Path(__file__).resolve().parent / "unsafe-inventory.py")
inventory = importlib.util.module_from_spec(spec)
spec.loader.exec_module(inventory)

SOURCE = """\
/// Documented.
///
/// # Safety
/// `p` must be valid.
unsafe fn documented(p: *const u8) {}

/// Right after a documented function, but without a contract of its own.
unsafe fn undocumented(p: *const u8) {}

fn blocks(p: *const u8) {
    // SAFETY: `p` is valid here.
    let a = unsafe { *p };
    let b = 1;
    let c = unsafe { *p }; // SAFETY: the same pointer.
    let d = 2;
    let e = unsafe { *p };
}

unsafe extern "C" fn callback(p: *const u8) {}

struct S;
// SAFETY: S holds nothing.
unsafe impl Send for S {}
unsafe impl Sync for S {}
"""


class SitesTest(unittest.TestCase):
    def setUp(self):
        fd, name = tempfile.mkstemp(suffix=".rs", dir=os.environ.get("TMPDIR") or "/var/tmp")
        os.close(fd)
        self.path = Path(name)
        self.path.write_text(SOURCE)

    def tearDown(self):
        self.path.unlink()

    def test_each_site_needs_its_own_note(self):
        got = {(f, kind, ok) for f, _, kind, ok in inventory.sites(self.path)}
        self.assertIn(("documented", "fn", True), got)
        # A neighbour's "# Safety" section doesn't cover it.
        self.assertIn(("undocumented", "fn", False), got)
        # A C callback's contract is the C API's.
        self.assertIn(("callback", "callback", True), got)
        self.assertIn(("impl Send for S", "impl", True), got)
        self.assertIn(("impl Sync for S", "impl", False), got)

    def test_block_notes_attach_to_their_statement(self):
        blocks = [ok for f, _, kind, ok in inventory.sites(self.path) if kind == "block"]
        # Above the statement, on its line, and none (a note two statements up
        # belongs to other code).
        self.assertEqual(blocks, [True, True, False])


if __name__ == "__main__":
    unittest.main()
