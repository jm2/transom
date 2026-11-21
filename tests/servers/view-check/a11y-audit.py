#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Walk Transom's AT-SPI tree (what screen readers see) and report interactive
controls without an accessible name."""
import sys

import pyatspi

NEEDS_NAME = {
    pyatspi.ROLE_PUSH_BUTTON, pyatspi.ROLE_TOGGLE_BUTTON, pyatspi.ROLE_ENTRY,
    pyatspi.ROLE_TEXT, pyatspi.ROLE_CHECK_BOX, pyatspi.ROLE_SPIN_BUTTON,
    pyatspi.ROLE_COMBO_BOX, pyatspi.ROLE_PAGE_TAB, pyatspi.ROLE_SLIDER,
}
desktop = pyatspi.Registry.getDesktop(0)
apps = [a for a in desktop if a and a.name and "transom" in a.name.lower()]
if not apps:
    print("a11y: transom not found on the AT-SPI bus:", [a.name for a in desktop if a])
    sys.exit(1)

problems, checked = [], 0


def walk(node, depth=0):
    global checked
    if node is None or depth > 60:
        return
    try:
        role, name, states = node.getRole(), node.name, node.getState()
    except Exception:  # noqa: BLE001 (objects can vanish while walking)
        return
    if role in NEEDS_NAME and states.contains(pyatspi.STATE_SHOWING):
        checked += 1
        if "--verbose" in sys.argv or __import__("os").environ.get("A11Y_VERBOSE"):
            print(f"a11y:   {node.getRoleName()}: {name!r}")
        if not (name or "").strip():
            problems.append(f"{node.getRoleName()} without a name (parent: {node.parent.getRoleName() if node.parent else '-'} '{node.parent.name if node.parent else ''}')")
    for child in node:
        walk(child, depth + 1)


for app in apps:
    walk(app)
print(f"a11y: checked {checked} interactive controls")
for p in problems:
    print("a11y: PROBLEM:", p)
print("A11Y OK" if not problems else f"A11Y FAILED ({len(problems)})")
