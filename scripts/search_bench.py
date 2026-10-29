#!/usr/bin/env python3
"""Warm search latency: `knowcode search` across every repo in the workspace against a baseline grep CLI.

Usage: scripts/search_bench.py --baseline BIN [--baseline-args ARGS] [--workspace DIR] [--runs N]

The baseline is any grep-style CLI that takes `-n -i -w -F -e PATTERN PATH...` and recurses into directories on its
own, run two ways:
- `workspace`: over the repo directories, as an agent's grep tool runs it (its own ignore handling, plus
  `--baseline-args`, for example the same size cap and excluded paths KnowCode uses),
- `same files`: over exactly the files KnowCode searches, given as arguments.

KnowCode runs end to end (process start, freshness check of every repo, search, ranking, JSON out) with its index in
a temporary cache directory, built once before timing. Each query is run once to warm the page cache, then `runs`
times per tool. Prints Markdown with p50 and p95 per tool, and per query.
"""

import argparse
import json
import os
import shlex
import statistics
import subprocess
import tempfile
import time
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parent))  # kc_env, beside this script
import kc_env  # noqa: E402


ROOT = Path(__file__).resolve().parent.parent
BIN = ROOT / "target" / "release" / "knowcode"

# (pattern, regex, ignore case, whole word)
QUERIES = [
    ("PairingArchive", False, False, False),
    ("loadSoundBank", False, False, False),
    ("YouTubeDurationProviding", False, False, False),
    ("BubstackTheme", False, False, False),
    ("queue.json", False, False, False),
    ("func ", False, False, False),
    ("self.", False, False, False),
    ("import SwiftUI", False, False, False),
    ("return nil", False, False, False),
    ("TODO", False, False, False),
    (r"func [a-z]+Store", True, False, False),
    (r"struct \w+View: View", True, False, False),
    (r"@(MainActor|Observable)", True, False, False),
    (r'fatalError\(".*"\)', True, False, False),
    (r"[0-9]+\.[0-9]+\.[0-9]+", True, False, False),
    ("error", False, True, False),
    ("settings", False, True, False),
    ("http", False, True, False),
    ("id", False, False, True),
    ("async", False, False, True),
]


def pct(v, q):
    v = sorted(v)
    return v[min(len(v) - 1, round((len(v) - 1) * q))]


def timed(cmd, env=None):
    t = time.perf_counter()
    out = subprocess.run(cmd, env=env, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    return (time.perf_counter() - t) * 1000, out.stdout


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--baseline", required=True, help="grep-style CLI to compare against")
    ap.add_argument("--baseline-args", default="", help="extra arguments for the baseline (shell-quoted)")
    ap.add_argument("--workspace", default=str(Path.home() / "Code" / "bubstackdev"))
    ap.add_argument("--runs", type=int, default=7)
    a = ap.parse_args()

    ws = Path(a.workspace)
    repos = sorted(str(p) for p in ws.iterdir() if (p / ".git").exists())
    env = kc_env.knowcode_env(tempfile.mkdtemp(prefix="knowcode-search-bench-"))
    subprocess.run([str(BIN), "index", *repos], env=env, check=True, capture_output=True)
    files = []
    for line in subprocess.run([str(BIN), "files", *sum((["--repo", r] for r in repos), [])], env=env, check=True, capture_output=True, text=True).stdout.splitlines():
        root, path, state = line.split("\t")
        if state in ("indexed", "pending"):
            files.append(str(Path(root) / path))
    extra = shlex.split(a.baseline_args)

    def kc_cmd(q):
        pattern, regex, ci, word = q
        cmd = [str(BIN), "search", pattern, "--json", "-n", "20", *sum((["--repo", r] for r in repos), [])]
        cmd += ["-e"] * regex + ["-i"] * ci + ["-w"] * word
        return cmd

    def base_cmd(q, paths):
        pattern, regex, ci, word = q
        return [a.baseline, *extra, "-n", *(["-i"] * ci), *(["-w"] * word), *([] if regex else ["-F"]), "-e", pattern, *paths]

    tools = {"knowcode": [], "baseline (workspace)": [], "baseline (same files)": []}
    rows = []
    for q in QUERIES:
        cmds = {
            "knowcode": kc_cmd(q),
            "baseline (workspace)": base_cmd(q, repos),
            "baseline (same files)": base_cmd(q, files),
        }
        for c in cmds.values():
            timed(c, env)  # warm
        per = {}
        for name, c in cmds.items():
            per[name] = [timed(c, env)[0] for _ in range(a.runs)]
            tools[name].extend(per[name])
        _, out = timed(cmds["knowcode"], env)
        d = json.loads(out)
        _, same = timed(cmds["baseline (same files)"], env)
        rows.append((q, d["matches"], same.count(b"\n"), d["stats"]["candidates"], per))

    print(f"Workspace: {ws} ({len(repos)} repos, {len(files)} searchable files). {a.runs} warm runs per query and tool.\n")
    print("| Tool | p50 | p95 |")
    print("|---|---:|---:|")
    for name, v in tools.items():
        print(f"| {name} | {pct(v, 0.5):.0f} ms | {pct(v, 0.95):.0f} ms |")
    print("\n| Query | Matches (knowcode / baseline, same files) | Candidate files | knowcode p50 | baseline workspace p50 | baseline same files p50 |")
    print("|---|---:|---:|---:|---:|---:|")
    for q, ours, theirs, cands, per in rows:
        flags = " ".join(f for f, on in (("regex", q[1]), ("-i", q[2]), ("-w", q[3])) if on)
        label = f"`{q[0]}`" + (f" ({flags})" if flags else "")
        p = {k: statistics.median(v) for k, v in per.items()}
        print(f"| {label} | {ours} / {theirs} | {cands} | {p['knowcode']:.0f} ms | {p['baseline (workspace)']:.0f} ms | {p['baseline (same files)']:.0f} ms |")


if __name__ == "__main__":
    main()
