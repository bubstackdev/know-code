#!/usr/bin/env python3
"""Validates SARIF logs against the SARIF 2.1.0 JSON schema, in a throwaway container with no network. No model calls.

    sarif_check.py fetch --work WORK
    sarif_check.py check --work WORK LOG...

`fetch` downloads the inputs once: the schema from OASIS (errata 01, checked against SCHEMA_SHA256) and the
`jsonschema` package's wheels. `check` validates each log with them in IMAGE, with `--network none`, and prints every
log that fails with its first errors; it exits 1 if any fails. A log must also hold no source text (`snippet`,
`contextRegion`): KnowCode's SARIF carries locations and messages only.
"""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys
import urllib.request

SCHEMA_URL = "https://docs.oasis-open.org/sarif/sarif/v2.1.0/errata01/os/schemas/sarif-schema-2.1.0.json"
SCHEMA_SHA256 = "c3b4bb2d6093897483348925aaa73af03b3e3f4bd4ca38cef26dcb4212a2682e"
JSONSCHEMA = "jsonschema==4.23.0"
IMAGE = "python:3.12-trixie@sha256:17b9724493d916f56565c131f389faff2597f46dec6ce09516f95125bc9f519f"

VALIDATE = r"""
import json, sys
from jsonschema import Draft4Validator
schema = json.load(open("/in/sarif-schema-2.1.0.json"))
v = Draft4Validator(schema)
bad = 0
for path in sys.argv[1:]:
    errors = sorted(v.iter_errors(json.load(open(path))), key=lambda e: list(e.path))
    if errors:
        bad += 1
        print(f"invalid: {path}")
        for e in errors[:5]:
            print(f"  {'/'.join(map(str, e.path))}: {e.message[:200]}")
print(f"{len(sys.argv) - 1} logs, {bad} invalid")
sys.exit(1 if bad else 0)
"""


def fetch(work):
    work = Path(work)
    (work / "wheels").mkdir(parents=True, exist_ok=True)
    data = urllib.request.urlopen(SCHEMA_URL, timeout=60).read()
    if hashlib.sha256(data).hexdigest() != SCHEMA_SHA256:
        sys.exit("the SARIF schema's SHA-256 differs from the pinned one")
    (work / "sarif-schema-2.1.0.json").write_bytes(data)
    subprocess.run(["docker", "run", "--rm", "-v", f"{(work / 'wheels').resolve()}:/w", IMAGE,
                    "pip", "download", "-q", "-d", "/w", JSONSCHEMA], check=True)


def source_text(log):
    """Whether a log quotes source: a `snippet` or a `contextRegion` anywhere in it."""
    text = json.dumps(log)
    return '"snippet"' in text or '"contextRegion"' in text


def check(work, logs):
    work = Path(work).resolve()
    inp = work / "in"
    if inp.exists():
        shutil.rmtree(inp)
    (inp / "logs").mkdir(parents=True)
    shutil.copy(work / "sarif-schema-2.1.0.json", inp)
    quoted = []
    for i, log in enumerate(logs):
        shutil.copy(log, inp / "logs" / f"{i:04}.sarif")
        if source_text(json.loads(Path(log).read_text())):
            quoted.append(log)
    (inp / "validate.py").write_text(VALIDATE)
    script = ("pip install -q --no-index --find-links /wheels jsonschema && "
              "python3 /in/validate.py /in/logs/*.sarif")
    p = subprocess.run(["docker", "run", "--rm", "--network", "none", "-v", f"{inp}:/in:ro",
                        "-v", f"{(work / 'wheels').resolve()}:/wheels:ro", IMAGE, "sh", "-c", script],
                       capture_output=True, text=True)
    names = {f"/in/logs/{i:04}.sarif": str(log) for i, log in enumerate(logs)}
    out = p.stdout
    for k, v in names.items():
        out = out.replace(k, v)
    print(out.strip())
    for log in quoted:
        print(f"quotes source: {log}")
    if p.returncode not in (0, 1):
        print(p.stderr.strip())
    return 1 if p.returncode != 0 or quoted else 0


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    f = sub.add_parser("fetch")
    f.add_argument("--work", required=True)
    c = sub.add_parser("check")
    c.add_argument("--work", required=True)
    c.add_argument("logs", nargs="+")
    args = ap.parse_args(argv)
    if args.cmd == "fetch":
        fetch(args.work)
        return 0
    return check(args.work, args.logs)


if __name__ == "__main__":
    sys.exit(main())
