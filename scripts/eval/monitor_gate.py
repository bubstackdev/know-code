#!/usr/bin/env python3
"""Monitors gate: a monitor (`knowcode rules --new-since <parent>`) replayed over a public repo's history must fire on
exactly the commits the labels name, with no model calls.

    monitor_gate.py label --clone DIR --rules FILE --sg-dir DIR --work WORK [--commits N] [--rev REV] --out LABELS
    monitor_gate.py run --knowcode BIN --clone DIR --rules FILE --work WORK --labels LABELS [--out RESULTS]
    monitor_gate.py label-text --clone DIR --regex RE --glob GLOB [--commits N] [--rev REV] --out LABELS
    monitor_gate.py run --knowcode BIN --clone DIR --regex RE --glob GLOB --work WORK --labels LABELS [--out RESULTS]

`label-text` labels a text monitor from git alone: the commits `git log -E -G<RE> -- ':(glob)GLOB'` lists among the
same commits are the candidates, and a candidate fires when, over the files it changes (renames followed, the files
KnowCode reads), the added lines RE matches outnumber the identical removed ones for some line text. `run` with
--regex replays `BIN rules --regex RE -g GLOB --new-since <parent>`.

`label` takes the last N non-merge commits on REV's first-parent line (each with one parent) and, for each, the files
the commit changes (`git diff -M --name-status` against its parent). It writes both versions of those files into
WORK/label/<sha>/{new,old}/<path> (never into the clone), runs ast-grep (see rules_gate.py) on every directory in one
throwaway container with no network, and labels the commit as firing when some (rule, matched text) occurs more
often in the new versions than in the old. Files the commit doesn't change hold the same matches at both commits, so
they cancel and aren't read. ast-grep labels; KnowCode labels nothing.

`run` checks out each labelled commit in a worktree of the clone (WORK/tree, moved commit to commit), runs
`BIN rules --rules FILE --new-since <parent> --json` there with its own cache, config and data dirs under WORK/kc,
and counts the commit as fired when the answer has a match. It prints the commits where the two differ and exits 1
if there is any.
"""
from __future__ import annotations

import argparse
from collections import Counter
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import rules_gate  # noqa: E402


def git(clone, *args, binary=False):
    out = subprocess.run(["git", "-C", str(clone), *args], check=True, capture_output=True)
    return out.stdout if binary else out.stdout.decode()


def commits(clone, rev, n):
    """The last `n` commits on `rev`'s first-parent line that have exactly one parent, oldest first, with it."""
    out = git(clone, "rev-list", "--first-parent", "--parents", f"--max-count={n * 3}", rev)
    pairs = []
    for line in out.splitlines():
        parts = line.split()
        if len(parts) == 2:
            pairs.append((parts[0], parts[1]))
        if len(pairs) == n:
            break
    return list(reversed(pairs))


# ast-grep's ignore files, all turned off: every file written out is read.
NO_IGNORE = ("hidden", "dot", "vcs", "parent", "exclude", "global")
# Directories KnowCode never reads (dependency and build output: `kc_index`'s discovery), and so neither side of the
# gate reads.
LEFT_OUT = {"node_modules", "Pods", "Carthage", "DerivedData", "vendor", "third_party", ".venv", "venv", ".git"}


def read(path, mode):
    """Whether KnowCode reads a file at this path with this git mode: not a symbolic link, nor under LEFT_OUT."""
    return mode != "120000" and not LEFT_OUT.intersection(path.split("/")[:-1])


def changed(clone, parent, sha):
    """(path at parent or None, path at sha or None) for each file the commit changes, renames followed, each side
    kept only where KnowCode reads it."""
    out = git(clone, "-c", "core.quotePath=false", "diff", "--raw", "-M", "-z", "--no-abbrev", parent, sha)
    fields = out.split("\0")
    pairs, i = [], 0
    while i < len(fields) and fields[i].startswith(":"):
        old_mode, new_mode, _, _, status = fields[i][1:].split(" ")
        if status[0] in "RC":
            old, new = fields[i + 1], fields[i + 2]
            old = old if status[0] == "R" else None
            i += 3
        else:
            path = fields[i + 1]
            old, new = (None if status[0] == "A" else path), (None if status[0] == "D" else path)
            i += 2
        old = old if old and read(old, old_mode) else None
        new = new if new and read(new, new_mode) else None
        if old or new:
            pairs.append((old, new))
    return pairs


def surplus(new, old):
    """How many matches the new side has beyond the old, by (rule, matched text)."""
    n = Counter((m["rule"], m["text"]) for m in new)
    n.subtract(Counter((m["rule"], m["text"]) for m in old))
    return sum(v for v in n.values() if v > 0)


def export(clone, sha, path, dest):
    dest.parent.mkdir(parents=True, exist_ok=True)
    dest.write_bytes(git(clone, "show", f"{sha}:{path}", binary=True))


def label(clone, rules, sg_dir, work, n, rev):
    root = Path(work) / "label"
    if root.exists():
        shutil.rmtree(root)
    pairs = commits(clone, rev, n)
    for sha, parent in pairs:
        for old, new in changed(clone, parent, sha):
            if new:
                export(clone, sha, new, root / sha / "new" / new)
            if old:
                export(clone, parent, old, root / sha / "old" / old)
        for side in ("new", "old"):
            (root / sha / side).mkdir(parents=True, exist_ok=True)
    rules = Path(rules).resolve()
    # Every directory scanned on its own (so each side's matches are told apart); a scan that fails, other than
    # ast-grep's exit status 1 for matches of severity error, stops the labelling.
    script = ('for d in */*; do (cd "$d" && /sg/ast-grep scan ' + " ".join(f"--no-ignore {k}" for k in NO_IGNORE)
              + ' --rule /rules/r.yml --json=stream . 2>/tmp/err | sed "s|^|$d\t|"; s=$?; '
              + 'if [ -s /tmp/err ] && ! grep -q "error(s) found in code" /tmp/err; then cat /tmp/err >&2; exit 2; fi) || exit 2; done')
    cmd = [
        "docker", "run", "--rm", "--platform", "linux/arm64", "--network", "none",
        "-v", f"{Path(sg_dir).resolve()}:/sg:ro", "-v", f"{root.resolve()}:/w:ro", "-v", f"{rules}:/rules/r.yml:ro",
        "-w", "/w", rules_gate.IMAGE, "sh", "-c", script,
    ]
    p = subprocess.run(cmd, capture_output=True, text=True)
    if p.returncode != 0:
        sys.exit(f"ast-grep failed: {p.stderr.strip()}")
    by = {}
    for line in p.stdout.splitlines():
        where, _, rec = line.partition("\t")
        r = json.loads(rec)
        by.setdefault(where, []).append({"rule": r["ruleId"], "text": r["text"]})
    labels = []
    for sha, parent in pairs:
        s = surplus(by.get(f"{sha}/new", []), by.get(f"{sha}/old", []))
        labels.append({"commit": sha, "parent": parent, "fires": s > 0, "new": s})
    return labels


def run(knowcode, clone, rules, work, labels, regex=None, glob=None):
    work = Path(work).resolve()
    tree = work / "tree"
    if not tree.exists():
        git(clone, "worktree", "add", "--detach", str(tree), labels[0]["commit"])
    env = dict(os.environ, KNOWCODE_UPDATE_CHECK="0", KNOWCODE_CACHE_DIR=str(work / "kc/cache"),
               KNOWCODE_CONFIG_DIR=str(work / "kc/config"), KNOWCODE_DATA_DIR=str(work / "kc/data"))
    results = []
    for lab in labels:
        subprocess.run(["git", "-C", str(tree), "checkout", "-q", "--detach", lab["commit"]], check=True)
        what = ["--regex", regex, "-g", glob] if regex else ["--rules", str(Path(rules).resolve())]
        out = subprocess.run([knowcode, "rules", *what, "--new-since", lab["parent"], "--json", "-n", "100000"],
                             cwd=tree, env=env, capture_output=True, text=True)
        if out.returncode != 0:
            sys.exit(f"knowcode failed at {lab['commit']}: {out.stderr.strip()}")
        answer = json.loads(out.stdout)
        results.append({"commit": lab["commit"], "fires": answer["total"] > 0, "new": answer["total"],
                        "complete": answer.get("complete", True)})
    return results


def text_surplus(patch, regex):
    """How many lines a `git diff` patch adds, beyond the identical lines it removes, that `regex` matches, over all
    its files."""
    import re
    rx = re.compile(regex)
    n = Counter()
    header = False
    for line in patch.split("\n"):
        # A file's header runs from `diff --git` to its first hunk; its `---`/`+++` lines aren't content.
        if line.startswith("diff --git "):
            header = True
        elif line.startswith("@@"):
            header = False
        elif not header and line[:1] in ("+", "-") and rx.search(line[1:]):
            n[line[1:]] += 1 if line[0] == "+" else -1
    return sum(v for v in n.values() if v > 0)


def label_text(clone, regex, glob, n, rev):
    pairs = commits(clone, rev, n)
    shas = [sha for sha, _ in pairs]
    listed = set(git(clone, "log", "-E", f"-G{regex}", "--format=%H", f"{shas[0]}^..{shas[-1]}", "--",
                     f":(glob){glob}").split())
    labels = []
    for sha, parent in pairs:
        s = 0
        if sha in listed:
            paths = [p for pair in changed(clone, parent, sha) for p in pair if p]
            keep = sorted({p for p in paths if fnmatch_glob(glob, p)})
            if keep:
                patch = git(clone, "-c", "core.quotePath=false", "diff", "-M", "--no-color", "--no-ext-diff",
                            "--unified=0", parent, sha, "--", *keep)
                s = text_surplus(patch, regex)
        labels.append({"commit": sha, "parent": parent, "fires": s > 0, "new": s})
    return labels


def fnmatch_glob(glob, path):
    """git's `:(glob)` match for the globs the set uses (`**/*.ext`): the file's name has the extension."""
    assert glob.startswith("**/*."), glob
    return path.endswith(glob[4:])


def compare(labels, results):
    got = {r["commit"]: r for r in results}
    return [(lab["commit"], lab["fires"], got.get(lab["commit"], {}).get("fires")) for lab in labels
            if got.get(lab["commit"], {}).get("fires") != lab["fires"]]


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    lab = sub.add_parser("label")
    lab.add_argument("--clone", required=True)
    lab.add_argument("--rules", required=True)
    lab.add_argument("--sg-dir", required=True)
    lab.add_argument("--work", required=True)
    lab.add_argument("--commits", type=int, default=200)
    lab.add_argument("--rev", default="HEAD")
    lab.add_argument("--out", required=True)
    t = sub.add_parser("label-text")
    t.add_argument("--clone", required=True)
    t.add_argument("--regex", required=True)
    t.add_argument("--glob", required=True)
    t.add_argument("--commits", type=int, default=200)
    t.add_argument("--rev", default="HEAD")
    t.add_argument("--out", required=True)
    r = sub.add_parser("run")
    r.add_argument("--knowcode", required=True)
    r.add_argument("--clone", required=True)
    r.add_argument("--rules")
    r.add_argument("--regex")
    r.add_argument("--glob")
    r.add_argument("--work", required=True)
    r.add_argument("--labels", required=True)
    r.add_argument("--out")
    args = ap.parse_args(argv)
    if args.cmd == "label":
        labels = label(args.clone, args.rules, args.sg_dir, args.work, args.commits, args.rev)
        Path(args.out).write_text("".join(json.dumps(x, sort_keys=True) + "\n" for x in labels))
        print(f"{len(labels)} commits, {sum(x['fires'] for x in labels)} labelled as firing")
        return 0
    if args.cmd == "label-text":
        labels = label_text(args.clone, args.regex, args.glob, args.commits, args.rev)
        Path(args.out).write_text("".join(json.dumps(x, sort_keys=True) + "\n" for x in labels))
        print(f"{len(labels)} commits, {sum(x['fires'] for x in labels)} labelled as firing")
        return 0
    labels = [json.loads(x) for x in Path(args.labels).read_text().splitlines() if x.strip()]
    results = run(args.knowcode, args.clone, args.rules, args.work, labels, args.regex, args.glob)
    if args.out:
        Path(args.out).write_text("".join(json.dumps(x, sort_keys=True) + "\n" for x in results))
    diff = compare(labels, results)
    for sha, want, got in diff:
        print(f"differs: {sha} labelled {'fires' if want else 'quiet'}, monitor {'fired' if got else 'quiet'}")
    fired = sum(x["fires"] for x in labels)
    print(f"{len(labels)} commits, {fired} labelled as firing, {len(diff)} differ")
    return 1 if diff else 0


if __name__ == "__main__":
    sys.exit(main())
