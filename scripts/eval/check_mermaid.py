#!/usr/bin/env python3
"""Checks that every diagram in a golden file parses as Mermaid: each is rendered by mermaid-cli (pinned by digest)
in a throwaway container with no network, and any that fails is printed. No model calls.

    check_mermaid.py [GOLDEN]    (default: crates/knowcode/tests/snapshots/diagrams.txt)

The golden file holds diagrams as `=== title` lines, each followed by the diagram's text.
"""
from __future__ import annotations

from pathlib import Path
import re
import subprocess
import sys
import tempfile

IMAGE = "minlag/mermaid-cli@sha256:99c983b3ab4e14033f2880bc1b9de17e5090b4515dabd63fe9cf8c0ae6130956"
ROOT = Path(__file__).resolve().parents[2]


def split(text):
    """(title, diagram) for each diagram in a golden file."""
    parts = re.split(r"^=== (.*)\n", text, flags=re.M)
    return [(parts[i], parts[i + 1]) for i in range(1, len(parts) - 1, 2)]


def main(argv=None):
    argv = sys.argv[1:] if argv is None else argv
    golden = Path(argv[0]) if argv else ROOT / "crates/knowcode/tests/snapshots/diagrams.txt"
    diagrams = split(golden.read_text())
    with tempfile.TemporaryDirectory() as tmp:
        Path(tmp).chmod(0o777)
        for i, (_, text) in enumerate(diagrams):
            (Path(tmp) / f"d{i:03}.mmd").write_text(text)
        script = ('for f in /data/*.mmd; do /home/mermaidcli/node_modules/.bin/mmdc -p /puppeteer-config.json '
                  '-i "$f" -o "${f%.mmd}.svg" >/dev/null 2>&1 || echo "$f"; done')
        out = subprocess.run(["docker", "run", "--rm", "--network", "none", "-v", f"{tmp}:/data", "--entrypoint", "sh",
                              IMAGE, "-c", script], check=True, capture_output=True, text=True).stdout
    failed = [int(Path(line).stem[1:]) for line in out.split()]
    for i in failed:
        print(f"does not parse: {diagrams[i][0]}")
    print(f"{len(diagrams)} diagrams, {len(failed)} fail to parse")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
