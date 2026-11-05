#!/usr/bin/env python3
"""Where indexing goes: each phase's instructions, CPU time and share, per repo, cold and warm.

Usage: scripts/index-phases.py [--bin PATH] [--runs N] [--threads N] [--work DIR] [--json FILE] repo ...

For each repo and run, indexes it cold into a fresh cache directory with `knowcode index --phases --json` (which also
computes the derived data a first query would), then once more with nothing changed (the warm check every query
pays). Prints a Markdown table per repo: the median of each phase over the runs, and its share of the repo's
instructions. Reads the repos only: caches, config and data go under --work (a temporary directory by default).

Compare by instructions retired, which a loaded machine barely moves; wall time is printed as indicative only.
Instructions are counted on macOS; elsewhere shares are of CPU time. Child processes (git) are counted in CPU time
only, in their own column. `write.rows` (the writer thread) runs beside `parse` (the workers): its wall time is in
`parse`'s.
"""

import argparse
import json
import os
import shutil
import statistics
import subprocess
import tempfile
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parent))  # kc_env, beside this script
import kc_env  # noqa: E402


ROOT = Path(__file__).resolve().parent.parent
FIELDS = ("instructions", "cpu_ms", "child_cpu_ms", "wall_ms")


def index(bin_path, repo, work, threads, clean):
    cache = work / "cache"
    if clean and cache.exists():
        shutil.rmtree(cache)
    env = kc_env.knowcode_env(work, cache=cache)
    args = [str(bin_path), "index", "--phases", "--json"]
    if threads:
        args += ["--threads", str(threads)]
    out = subprocess.run([*args, str(repo)], env=env, capture_output=True, text=True, check=True)
    row = json.loads(out.stdout)["repos"][0]
    return row["phases"], row["report"]


def median_phases(runs):
    """Phase name -> field -> median over runs (phases in first-seen order; a phase recorded twice is summed)."""
    names, per = [], {}
    for phases in runs:
        seen = {}
        for p in phases:
            if p["name"] not in names:
                names.append(p["name"])
            acc = seen.setdefault(p["name"], {f: 0 for f in FIELDS})
            for f in FIELDS:
                acc[f] += p.get(f) or 0
        for name, acc in seen.items():
            for f in FIELDS:
                per.setdefault(name, {}).setdefault(f, []).append(acc[f])
    return [(n, {f: statistics.median(per[n][f]) for f in FIELDS}) for n in names]


def table(title, rows):
    total_i = sum(r["instructions"] for _, r in rows)
    total_c = sum(r["cpu_ms"] for _, r in rows)
    lines = [f"**{title}**", "", "| phase | G instr | share | CPU ms | child CPU ms | wall ms (indicative) |", "| --- | ---: | ---: | ---: | ---: | ---: |"]
    for name, r in rows:
        share = r["instructions"] / total_i if total_i else (r["cpu_ms"] / total_c if total_c else 0)
        lines.append(
            f"| {name} | {r['instructions'] / 1e9:.3f} | {100 * share:.1f}% | {r['cpu_ms']:.0f} | {r['child_cpu_ms']:.0f} | {r['wall_ms']:.0f} |"
        )
    tot = {f: sum(r[f] for _, r in rows) for f in FIELDS}
    lines.append(f"| total | {tot['instructions'] / 1e9:.3f} | | {tot['cpu_ms']:.0f} | {tot['child_cpu_ms']:.0f} | {tot['wall_ms']:.0f} |")
    return "\n".join(lines)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("repos", nargs="+", type=Path)
    ap.add_argument("--bin", type=Path, default=ROOT / "target" / "release" / "knowcode")
    ap.add_argument("--runs", type=int, default=3)
    ap.add_argument("--threads", type=int)
    ap.add_argument("--work", type=Path, help="where caches go (default: a temporary directory, removed after)")
    ap.add_argument("--json", type=Path, help="also write every run's phases here")
    a = ap.parse_args()

    tmp = None
    work = a.work
    if work is None:
        tmp = tempfile.mkdtemp(prefix="kc-phases-")
        work = Path(tmp)
    raw = {}
    try:
        for repo in a.repos:
            repo = repo.resolve()
            cold, warm, reports = [], [], []
            for _ in range(a.runs):
                phases, report = index(a.bin, repo, work, a.threads, clean=True)
                cold.append(phases)
                reports.append(report)
                warm.append(index(a.bin, repo, work, a.threads, clean=False)[0])
            raw[str(repo)] = {"cold": cold, "warm": warm, "reports": reports}
            r = reports[0]
            print(f"### {repo.name}: {r['files']} files indexed, {r['bytes_parsed'] / 1e6:.1f} MB parsed, {a.runs} runs (medians)\n")
            print(table("cold", median_phases(cold)) + "\n")
            print(table("warm (nothing changed)", median_phases(warm)) + "\n")
    finally:
        if tmp:
            shutil.rmtree(tmp, ignore_errors=True)
    if a.json:
        a.json.write_text(json.dumps(raw, indent=1))


if __name__ == "__main__":
    main()
