#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""List every `unsafe` site in the crates allowed to have them, by file and function,
with the line numbers and whether a `// SAFETY:` comment justifies each block, as
Markdown: the line-level index for docs/unsafe-review.md (regenerate it as the code
changes).

  scripts/unsafe-inventory.py                      # crates/transom-view-freerdp
  scripts/unsafe-inventory.py crates/transom-view-capi crates/transom-macos
"""
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FN = re.compile(r'^\s*(?:pub(?:\([^)]*\))?\s+)?(?:unsafe\s+)?(?:extern\s+"C"\s+)?fn\s+(\w+)')
IMPL = re.compile(r"^\s*(?:unsafe\s+)?impl\b\s*(?:<[^>]*>\s*)?([^{]*)")
UNSAFE = re.compile(r"\bunsafe\b")


def sites(path):
    """[(function, line, kind, justified)] for one file."""
    lines = path.read_text().splitlines()
    func, out = "(module)", []
    for i, line in enumerate(lines):
        code = line.split("//", 1)[0]
        if m := FN.match(code):
            func = m.group(1)
        elif m := IMPL.match(code):
            func = "impl " + " ".join(m.group(1).split())
        if not UNSAFE.search(code):
            continue
        if re.search(r'\bunsafe\s+extern\s+"C"\s+fn\b', code):
            # Called by C (FreeRDP, FFmpeg, GLib): its contract is that API's; the
            # blocks inside carry the SAFETY notes.
            kind, justified = "callback", True
        elif re.search(r"\bunsafe\s+fn\b", code):
            kind = "fn"
            # An unsafe fn states its contract in a "# Safety" doc section.
            above = "\n".join(lines[max(0, i - 12):i])
            justified = "# Safety" in above or "SAFETY" in above
        elif re.search(r"\bunsafe\s+impl\b", code):
            kind, justified = "impl", "SAFETY" in "\n".join(lines[max(0, i - 4):i])
        else:
            kind = "block"
            justified = any("SAFETY" in lines[j] for j in range(max(0, i - 6), i + 1))
        out.append((func, i + 1, kind, justified))
    return out


def main():
    crates = sys.argv[1:] or ["crates/transom-view-freerdp"]
    total = unjustified = 0
    for crate in crates:
        print(f"## {crate}\n")
        print("| File | Function | Lines (b = block, c = C callback, f = unsafe fn, i = impl; ! = no SAFETY note) |")
        print("|---|---|---|")
        for path in sorted((ROOT / crate / "src").glob("*.rs")):
            by_fn = {}
            for func, line, kind, ok in sites(path):
                by_fn.setdefault(func, []).append(f"{line}{kind[0]}{'' if ok else '!'}")
                total += 1
                unjustified += not ok
            for func, refs in by_fn.items():
                print(f"| `{path.name}` | `{func}` | {', '.join(refs)} |")
        print()
    print(f"{total} sites; {unjustified} without a SAFETY note nearby.")


if __name__ == "__main__":
    main()
