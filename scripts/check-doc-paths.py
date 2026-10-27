#!/usr/bin/env python3
"""Fails when README.md or a doc under docs/ names a code file that no longer exists: a path under `crates/` or
`scripts/` (`crates/kc-store/src/trace.rs`, `scripts/eval/run.py`), as written or as a relative link from docs/. A
path followed by a wildcard or a placeholder (`*`, `{file}`, `<repo>`) is a pattern, not a file, and is skipped.
Planned files a design note names elsewhere (`eval/judge.md`) aren't checked.

Usage: scripts/check-doc-paths.py   (exit 1, listing each missing path with where it is named)
"""

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TOP = ("crates", "scripts")
PATH = re.compile(r"(?<![\w./~-])(?:\.\./)?(?:" + "|".join(TOP) + r")/[\w./-]*[\w/]")


def missing(doc):
    """(line number, path) of each path `doc` names that doesn't exist."""
    out = []
    for n, line in enumerate(doc.read_text().splitlines(), 1):
        for m in PATH.finditer(line):
            p = m.group(0)
            rest = line[m.end():m.end() + 1]
            if rest and rest in "*{<$":
                continue
            # A relative link from docs/ (`../crates/...`) or a path from the repo root.
            target = (doc.parent / p) if p.startswith("../") else ROOT / p
            if not target.exists():
                out.append((n, p))
    return out


def main():
    docs = [ROOT / "README.md", *sorted((ROOT / "docs").glob("*.md"))]
    bad = [(d, n, p) for d in docs for n, p in missing(d)]
    for d, n, p in bad:
        print(f"{d.relative_to(ROOT)}:{n}: {p} does not exist")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
