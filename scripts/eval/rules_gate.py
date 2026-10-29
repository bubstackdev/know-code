#!/usr/bin/env python3
"""Rules gate: KnowCode's rule matches against the ast-grep command line's, on the same files, with no model calls.

    rules_gate.py reference --sg-dir DIR --tree TREE --rules RULES... [--out FILE]
    rules_gate.py compare EXPECTED ACTUAL
    rules_gate.py verify --sg-dir DIR

`verify` runs the reference on eval/rules-v1 again and compares it with the committed eval/rules-v1/expected.jsonl,
which `crates/kc-index/tests/rules_fixture.rs` holds KnowCode's matches to, and checks that ast-grep refuses every
rule file in eval/rules-v1/refused, which the same test checks KnowCode refuses: the two together are the gate.

`reference` runs ast-grep (the release matching the embedded ast-grep-core, see SG_VERSION; its Linux binary
unpacked in DIR and checked against SG_SHA256) in a throwaway Linux container with no network, as
`ast-grep scan --rule R --json=stream .` from TREE, once per rule file, and writes each match as one JSON line:
rule id, file (relative to TREE), start and end byte, severity and message, sorted. ast-grep's own matches are the
labels; KnowCode labels nothing.

`compare` reads two such files and prints the matches each has that the other lacks, per rule, then exits 1 if
there is any. A match is (rule, file, start, end); severity and message are compared too, and reported apart.
"""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import sys

SG_VERSION = "0.50.0"
# app-aarch64-unknown-linux-gnu.zip's `ast-grep` binary from the GitHub release 0.50.0 of ast-grep/ast-grep.
SG_SHA256 = {
    "aarch64": "e453bdbe419cb21e2c2ae1e9586e09111fcbe70f98213276f5ad728849503d6b",
}
IMAGE = "ubuntu:26.04"


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def normalize(record):
    """One ast-grep JSON match as the gate compares it."""
    return {
        "rule": record["ruleId"],
        "file": record["file"].removeprefix("./"),
        "start": record["range"]["byteOffset"]["start"],
        "end": record["range"]["byteOffset"]["end"],
        "severity": record["severity"],
        "message": record["message"],
    }


def key(m):
    return (m["rule"], m["file"], m["start"], m["end"])


def parse_stream(text):
    return [normalize(json.loads(line)) for line in text.splitlines() if line.strip()]


def reference(sg_dir, tree, rules):
    sg_dir, tree = Path(sg_dir).resolve(), Path(tree).resolve()
    binary = sg_dir / "ast-grep"
    want = SG_SHA256.get("aarch64")
    if want and sha256(binary) != want:
        sys.exit(f"{binary} is not ast-grep {SG_VERSION} (sha256 differs)")
    out = []
    for r in rules:
        r = Path(r).resolve()
        cmd = [
            "docker", "run", "--rm", "--platform", "linux/arm64", "--network", "none",
            "-v", f"{sg_dir}:/sg:ro", "-v", f"{tree}:/w:ro", "-v", f"{r.parent}:/rules:ro", "-w", "/w", IMAGE,
            "/sg/ast-grep", "scan", "--rule", f"/rules/{r.name}", "--json=stream", ".",
        ]
        p = subprocess.run(cmd, capture_output=True, text=True)
        # ast-grep exits 1 when it found error-severity matches; anything else past 1 is a failure.
        if p.returncode not in (0, 1) or (p.returncode == 1 and "error(s) found in code" not in p.stderr):
            sys.exit(f"ast-grep failed on {r.name}: {p.stderr.strip()}")
        out.extend(parse_stream(p.stdout))
    return sorted(out, key=key)


def refused(sg_dir, rules_dir):
    """The rule files in `rules_dir` the ast-grep command line does NOT refuse (it should refuse every one)."""
    sg_dir, rules_dir = Path(sg_dir).resolve(), Path(rules_dir).resolve()
    loaded = []
    for r in sorted(rules_dir.glob("*.yml")):
        cmd = ["docker", "run", "--rm", "--platform", "linux/arm64", "--network", "none", "-v", f"{sg_dir}:/sg:ro",
               "-v", f"{rules_dir}:/rules:ro", "-w", "/tmp", IMAGE, "/sg/ast-grep", "scan", "--rule", f"/rules/{r.name}", "."]
        p = subprocess.run(cmd, capture_output=True, text=True)
        if "Cannot parse rule" not in p.stderr:
            loaded.append(r.name)
    return loaded


def compare(expected, actual):
    """Matches only in `expected`, only in `actual`, and those in both whose severity or message differ."""
    e = {key(m): m for m in expected}
    a = {key(m): m for m in actual}
    missing = sorted(e.keys() - a.keys())
    extra = sorted(a.keys() - e.keys())
    differ = sorted(k for k in e.keys() & a.keys() if (e[k]["severity"], e[k]["message"]) != (a[k]["severity"], a[k]["message"]))
    return missing, extra, differ


def read_lines(path):
    return [json.loads(line) for line in Path(path).read_text().splitlines() if line.strip()]


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("reference")
    r.add_argument("--sg-dir", required=True)
    r.add_argument("--tree", required=True)
    r.add_argument("--rules", nargs="+", required=True)
    r.add_argument("--out")
    v = sub.add_parser("verify")
    v.add_argument("--sg-dir", required=True)
    c = sub.add_parser("compare")
    c.add_argument("expected")
    c.add_argument("actual")
    args = ap.parse_args(argv)
    if args.cmd == "reference":
        text = "".join(json.dumps(m, sort_keys=True) + "\n" for m in reference(args.sg_dir, args.tree, args.rules))
        if args.out:
            Path(args.out).write_text(text)
        else:
            sys.stdout.write(text)
        return 0
    if args.cmd == "verify":
        here = Path(__file__).resolve().parents[2] / "eval" / "rules-v1"
        expected = read_lines(here / "expected.jsonl")
        actual = reference(args.sg_dir, here / "src", sorted((here / "rules").glob("*.yml")))
        for name in refused(args.sg_dir, here / "refused"):
            print(f"ast-grep loads {name}, which the set says it refuses")
            return 1
    else:
        expected, actual = read_lines(args.expected), read_lines(args.actual)
    missing, extra, differ = compare(expected, actual)
    for name, keys in (("only in expected", missing), ("only in actual", extra), ("severity or message differs", differ)):
        for k in keys:
            print(f"{name}: {k[0]} {k[1]}:{k[2]}-{k[3]}")
    print(f"expected {len(expected)}, actual {len(actual)}, only in expected {len(missing)}, only in actual {len(extra)}, "
          f"severity or message differs {len(differ)}")
    return 1 if missing or extra or differ else 0


if __name__ == "__main__":
    sys.exit(main())
