#!/usr/bin/env python3
"""Measure KnowCode against its footprint targets.

Usage: scripts/measure.py [--workspace DIR] [--runs N] [--threads N] [repo ...]

Reads the repos only. Indexes go to a temporary cache directory (KNOWCODE_CACHE_DIR), and the single-file update
test edits a throwaway local clone, so nothing in the workspace or the user cache is touched. Prints Markdown.
"""

import argparse
import json
import os
import shutil
import statistics
import subprocess
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BIN = ROOT / "target" / "release" / "knowcode"


def run(args, env, cwd=None):
    out = subprocess.run([str(BIN), *args], env=env, cwd=cwd, capture_output=True, text=True, check=True)
    return out.stdout


def timed(args, env, cwd=None):
    t = time.perf_counter()
    run(args, env, cwd)
    return (time.perf_counter() - t) * 1000


def swift_bytes(repo):
    """Tracked source files (Swift, Python, Rust) and their total size."""
    files = subprocess.run(["git", "-C", repo, "ls-files", "-z", "*.swift", "*.py", "*.rs"], capture_output=True, check=True).stdout
    paths = [p for p in files.decode().split("\0") if p]
    return len(paths), sum((Path(repo) / p).stat().st_size for p in paths if (Path(repo) / p).exists())


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--workspace", default=str(Path.home() / "Code" / "bubstackdev"))
    ap.add_argument("--runs", type=int, default=3)
    ap.add_argument("--threads", type=int, default=0, help="0 = knowcode default (half the cores)")
    ap.add_argument("repos", nargs="*")
    a = ap.parse_args()

    ws = Path(a.workspace)
    repos = [ws / r for r in a.repos] if a.repos else sorted(p for p in ws.iterdir() if (p / ".git").exists())
    repos = [r for r in repos if swift_bytes(r)[0] > 0]
    cache = tempfile.mkdtemp(prefix="knowcode-measure-")
    env = dict(os.environ, KNOWCODE_CACHE_DIR=cache)
    threads = ["--threads", str(a.threads)] if a.threads else []

    print(f"Binary: {BIN.stat().st_size / 1e6:.1f} MB. Runs per cold index: {a.runs}.\n")
    print("| Repo | Source files | Source MB | Cold index (median) | Index MB | Symbols | Refs | Parse-error files | No-op freshness check |")
    print("|---|---:|---:|---:|---:|---:|---:|---:|---:|")
    totals = {"files": 0, "bytes": 0, "ms": 0.0, "index": 0}
    for repo in repos:
        files, nbytes = swift_bytes(repo)
        cold = []
        for _ in range(a.runs):
            report = json.loads(run(["index", str(repo), "--rebuild", "--json", *threads], env))
            cold.append(report["total_ms"])
        status = json.loads(run(["status", str(repo), "--json"], env))
        noop = statistics.median(json.loads(run(["index", str(repo), "--json"], env))["total_ms"] for _ in range(5))
        syms = sum(status["symbols"].values())
        refs = sum(status["refs"].values())
        med = statistics.median(cold)
        idx = status["index"]["bytes"]
        totals["files"] += files
        totals["bytes"] += nbytes
        totals["ms"] += med
        totals["index"] += idx
        print(
            f"| {repo.name} | {files} | {nbytes / 1e6:.2f} | {med:.0f} ms | {idx / 1e6:.1f} | {syms} | {refs} | "
            f"{status['coverage']['with_parse_errors']} | {noop:.0f} ms |"
        )
    print(
        f"| **all** | {totals['files']} | {totals['bytes'] / 1e6:.2f} | {totals['ms'] / 1000:.1f} s | "
        f"{totals['index'] / 1e6:.1f} | | | | |\n"
    )

    # Whole-workspace cold index in one process, wall clock and peak RSS.
    t = time.perf_counter()
    out = subprocess.run(
        ["/usr/bin/time", "-l", str(BIN), "index", "--workspace", str(ws), "--rebuild", "--json", *threads],
        env=env, capture_output=True, text=True, check=True,
    )
    wall = time.perf_counter() - t
    rss = next((int(l.split()[0]) for l in out.stderr.splitlines() if "maximum resident set size" in l), 0)
    print(f"Whole workspace, one process: {wall:.1f} s wall, peak RSS {rss / 1e6:.0f} MB.\n")

    # Single-file update: edit one file in a throwaway clone and time the incremental index.
    probe = next((r for r in repos if r.name == "bubstack"), repos[0])
    with tempfile.TemporaryDirectory(prefix="knowcode-clone-") as tmp:
        clone = Path(tmp) / probe.name
        subprocess.run(["git", "clone", "--quiet", "--local", str(probe), str(clone)], check=True)
        run(["index", str(clone)], env)
        target = next(Path(clone).rglob("*.swift"))
        samples = []
        for i in range(5):
            with open(target, "a") as f:
                f.write(f"\n// knowcode measure {i}\n")
            report = json.loads(run(["index", str(clone), "--json"], env))
            samples.append(report["repos"][0]["report"]["total_ms"])
        print(f"Single-file update ({probe.name} clone, 5 edits): median {statistics.median(samples):.0f} ms, "
              f"max {max(samples):.0f} ms.\n")

    # Query latency, in process and end to end (process start + freshness check + query + JSON).
    bench = json.loads(run(["bench", "--json", "--samples", "60", *sum((["-r", str(r)] for r in repos), [])], env))
    o = bench["overall"]
    print("| Query (in process, no freshness check) | n | p50 | p95 | max |")
    print("|---|---:|---:|---:|---:|")
    for k in ["find", "refs", "context", "freshness_check"]:
        d = o[k]
        print(f"| {k} | {d['n']} | {d['p50_ms']} ms | {d['p95_ms']} ms | {d['max_ms']} ms |")
    print()

    biggest = max(repos, key=lambda r: swift_bytes(r)[1])
    e2e = {}
    for name, args in {
        "find (prefix)": ["find", "Engine", "--json"],
        "refs": ["refs", "init", "--json"],
        "context": ["context", "body", "--json"],
    }.items():
        e2e[name] = sorted(timed(args + ["-r", str(biggest)], env) for _ in range(20))
    print(f"End-to-end CLI on the largest repo ({biggest.name}), 20 runs each:\n")
    print("| Command | p50 | p95 |")
    print("|---|---:|---:|")
    for name, s in e2e.items():
        print(f"| {name} | {statistics.median(s):.0f} ms | {s[int(len(s) * 0.95) - 1]:.0f} ms |")

    shutil.rmtree(cache, ignore_errors=True)


if __name__ == "__main__":
    main()
