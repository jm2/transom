#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Compare the probe log with the inputtest script (keep both in sync)."""
import json
import sys

widget = "--widget" in sys.argv
path = [a for a in sys.argv[1:] if not a.startswith("--")][0]
events = [json.loads(line) for line in open(path) if line.strip()]
presses = [e["name"] for e in events if e["t"] == "key" and e["down"]]
buttons = [(e["button"], e["x"], e["y"]) for e in events if e["t"] == "button" and e["down"]]
scrolls = [(round(e["dx"]), round(e["dy"])) for e in events if e["t"] == "scroll"]

expected_keys = ["h", "e", "l", "l", "o", "Shift_L", "exclam", "Left", "Right", "Home",
                 "Delete", "KP_Enter", "Control_R", "F5", "z"]
# gnome-remote-desktop injects Unicode input as a keysym via mutter, which can only
# produce keysyms present in the session keymap (US here), so "eacute" is optional
# there. Windows takes any Unicode character, so it is required.
if "--windows" in sys.argv:
    expected_keys = expected_keys + ["eacute"]
elif presses and presses[-1] == "eacute":
    presses = presses[:-1]
expected_buttons = [(1, 50, 50), (1, 100, 100), (3, 200, 150), (2, 300, 200)]
expected_scroll = [(0, -1), (0, 1), (1, 0)]

if widget:
    # Widget pass: only the focus click and the keys (no Unicode, no wheel).
    expected_keys = [k for k in expected_keys if k not in ("z", "eacute")]
    expected_buttons = expected_buttons[:1]
    expected_scroll = []
    buttons = buttons[:1]
    # The warm-up Shift tap may or may not arrive (see inject-keys.py).
    if presses[:1] == ["Shift_L"]:
        presses = presses[1:]
    buttons = [(b, None, None) for b, _, _ in buttons]
    expected_buttons = [(1, None, None)]

ok = True
def check(name, got, want):
    global ok
    status = "ok" if got == want else "MISMATCH"
    ok &= got == want
    print(f"{name:8} {status}\n  want {want}\n  got  {got}")

check("keys", presses, expected_keys)
check("buttons", buttons, expected_buttons)
check("scroll", scrolls, expected_scroll)
print("INPUT OK" if ok else "INPUT FAILED")
sys.exit(0 if ok else 1)
