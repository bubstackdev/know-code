#!/usr/bin/env python3
"""Check `knowcode search` against `git grep` on the parity set in eval/search.toml.

Usage: scripts/search_eval.py [--only ID] [--show N]

Run `scripts/corpus.py fetch` and `scripts/corpus.py pins` first. Each query runs through `knowcode search --json`
(every result, no paging) and through `git grep -n -I` in each repo's clean checkout. Both answer per line. git grep
hits in files KnowCode deliberately leaves out (ignore rules, binary, large or generated files, as listed by the
hidden `knowcode files` command) are counted separately; every other hit must match exactly in (repo, file, line).
Indexes go to corpus/index under the user cache, apart from the user's own index. Prints Markdown; exits non-zero
on any mismatch.
"""

import argparse
import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import tomllib
from collections import Counter
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))  # kc_env, beside this script
import kc_env  # noqa: E402


ROOT = Path(__file__).resolve().parent.parent
BIN = ROOT / "target" / "release" / "knowcode"
spec = importlib.util.spec_from_file_location("corpus", ROOT / "scripts" / "corpus.py")
corpus = importlib.util.module_from_spec(spec)
spec.loader.exec_module(corpus)
ENV = kc_env.knowcode_env(tempfile.mkdtemp(prefix="knowcode-search-eval-"), cache=corpus.CORPUS / "index")


def kc(args):
    out = subprocess.run([str(BIN), *args], env=ENV, capture_output=True, text=True)
    if out.returncode != 0:
        raise SystemExit(f"knowcode {' '.join(args)}: {out.stderr}")
    return out.stdout


def covered(d):
    """Files knowcode searches in repo dir d, and the skip reason of every other tracked file it knows."""
    searched, skipped = set(), {}
    for line in kc(["files", "--repo", str(d)]).splitlines():
        _, path, state = line.split("\t")
        if state in ("indexed", "pending"):
            searched.add(path)
        else:
            skipped[path] = state.split(":", 1)[1]
    return searched, skipped


def git_grep(d, q):
    args = ["git", "-C", str(d), "grep", "-n", "-I", "-z", "--no-color"]
    if q.get("case") == "insensitive":
        args.append("-i")
    if q.get("word"):
        args.append("-w")
    args.append("-E" if q.get("regex") else "-F")
    args += ["-e", q["pattern"], "--"]
    args += q.get("globs", [])
    out = subprocess.run(args, capture_output=True)
    if out.returncode not in (0, 1):
        raise SystemExit(f"git grep failed in {d}: {out.stderr.decode()}")
    hits = set()
    for rec in out.stdout.split(b"\n"):
        if not rec:
            continue
        path, line, _ = rec.split(b"\0", 2)
        hits.add((path.decode(), int(line)))
    return hits


def run_query(q, cover):
    dirs = [corpus.path(s) for s in q["repos"]]
    for d in dirs:
        if not d.exists():
            raise SystemExit(f"missing {d}: run scripts/corpus.py fetch and pins")
    names = {d.name: d for d in dirs}
    args = ["search", q["pattern"], "--json", "-n", "1000000", "--max-lines", "1000000"]
    for d in dirs:
        args += ["--repo", str(d)]
    if q.get("regex"):
        args.append("-e")
    if q.get("case") == "insensitive":
        args.append("-i")
    if q.get("word"):
        args.append("-w")
    for g in q.get("globs", []):
        args += ["-g", g]
    env = json.loads(kc(args))
    multi = len(dirs) > 1
    ours = {((it["repo"] if multi else dirs[0].name), it["file"], it["line"]) for it in env["items"]}
    theirs, excluded = set(), Counter()
    for name, d in names.items():
        searched, skipped = cover(d)
        for path, line in git_grep(d, q):
            if path in searched:
                theirs.add((name, path, line))
            else:
                excluded[skipped.get(path, "ignored")] += 1
    return ours, theirs, excluded, env


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--only")
    ap.add_argument("--show", type=int, default=5, help="mismatches to print per query")
    a = ap.parse_args()
    queries = tomllib.loads((ROOT / "eval" / "search.toml").read_text())["query"]
    if a.only:
        queries = [q for q in queries if q["id"] == a.only]
    cache = {}

    def cover(d):
        if d not in cache:
            cache[d] = covered(d)
        return cache[d]

    print("| Query | Repos | Kind | git grep lines | Left out by skip rules | knowcode lines | Files | Top hit | Match |")
    print("|---|---|---|---:|---|---:|---:|---|---|")
    failed = 0
    for q in queries:
        ours, theirs, excluded, env = run_query(q, cover)
        ok = ours == theirs
        failed += not ok
        kind = "regex" if q.get("regex") else "literal"
        if q.get("case") == "insensitive":
            kind += ", -i"
        if q.get("word"):
            kind += ", -w"
        top = env["items"][0] if env["items"] else None
        top_s = f"{top['kind']} {top['file'].rsplit('/', 1)[-1]}:{top['line']}" if top else "–"
        ex = ", ".join(f"{k} {v}" for k, v in sorted(excluded.items())) or "–"
        repos = q["repos"][0].split("@")[0] if len(q["repos"]) == 1 else f"{len(q['repos'])} repos"
        print(f"| {q['id']} | {repos} | {kind} | {len(theirs)} | {ex} | {len(ours)} | {env['files']} | {top_s} | {'yes' if ok else '**no**'} |")
        if not ok:
            for h in sorted(theirs - ours)[: a.show]:
                print(f"|   missing {h} |")
            for h in sorted(ours - theirs)[: a.show]:
                print(f"|   extra {h} |")
    print(f"\n{len(queries) - failed} of {len(queries)} queries match git grep exactly.")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
