"""Defect prediction by history replay: does `hotspots`' concern score rank first the files that later fixes touch?

For each sampled repo, KnowCode runs at the parent commit `--before` a cutoff (a scratch clone, as in `replay`), with
`hotspots --as-of head`. The truth is the set of files that fix commits (the same subject rule as `vitals::is_fix`,
mirrored below) touched in the `--horizon` days after that parent. Each scoring formula ranks the parent's files;
per repo we score AUC (the chance a truth file outranks a non-truth file) and precision@10, then average over repos
with a bootstrap interval, and report paired differences against the first formula.
"""

import json
import math
import random
import re
import shutil
import subprocess
from pathlib import Path

from common import EVAL_DIR, GITLAB, bootstrap, kc, kc_json, pct

DAY = 86_400

# --- the fix rule, mirroring kc_index::vitals::is_fix -------------------------------------------------------------

FIX_ALWAYS = {"bug", "bugs", "bugfix", "bugfixes", "hotfix", "hotfixes", "revert", "reverts", "reverted", "defect", "defects"}
FIX_WORDS = {"fix", "fixes", "fixed", "fixing", "regression", "crash", "crashes", "crashing", "broken", "nre"}
CHORE_WORDS = {
    "test", "tests", "spec", "specs", "lint", "linting", "build", "builds", "ci", "pipeline", "conflict", "conflicts",
    "merge", "typo", "typos", "format", "formatting", "whitespace", "warning", "warnings", "spelling", "docs", "doc",
    "readme", "changelog", "comment", "comments", "style", "gps", "location", "position",
}
NONFIX_TYPES = {"test", "tests", "docs", "doc", "chore", "style", "ci", "build", "refactor", "perf", "feat"}


def conventional_type(low):
    m = re.match(r"([a-z]+)(\([^)]*\))?!?:", low)
    return m.group(1) if m else None


def is_fix(subject):
    low = subject.strip().lower()
    t = conventional_type(low)
    if t in {"fix", "fixes", "bugfix", "hotfix", "bug"}:
        return True
    if t in NONFIX_TYPES:
        return False
    words = [w for w in re.split(r"[^a-z0-9]", low) if w]
    if any(w in FIX_ALWAYS for w in words):
        return True
    for i, w in enumerate(words):
        if w in FIX_WORDS and not any(c in CHORE_WORDS for c in words[max(0, i - 2): i + 3]):
            return True
    return bool(words) and words[0] == "correct"


# --- scoring formulas ----------------------------------------------------------------------------------------------


def z(vals):
    n = max(len(vals), 1)
    mean = sum(vals) / n
    sd = math.sqrt(sum((v - mean) ** 2 for v in vals) / n)
    return [(v - mean) / sd if sd > 0 else 0.0 for v in vals]


BASE_TERMS = [
    (1.0, lambda f: f["weighted_churn"]),
    (1.0, lambda f: f["complexity"]),
    (0.5, lambda f: f["max_nesting"]),
    (0.5, lambda f: f["longest_function"]),
    (0.5, lambda f: f["fan_in"]),
    (0.5, lambda f: f["coupled_files"]),
    (0.25, lambda f: f["authors_365d"]),
]
FIX_RATE = lambda f: f["fix_commits_365d"] / (f["commits_365d"] + 2)  # noqa: E731
FIX_SCORE = lambda f: f["fix_score"]  # noqa: E731

FORMULAS = {
    "old (fix_rate)": BASE_TERMS + [(1.0, FIX_RATE)],
    "fix_score": BASE_TERMS + [(1.0, FIX_SCORE)],
    "both (0.5 each)": BASE_TERMS + [(0.5, FIX_RATE), (0.5, FIX_SCORE)],
    "both (1 each)": BASE_TERMS + [(1.0, FIX_RATE), (1.0, FIX_SCORE)],
    "fix_score only": [(1.0, FIX_SCORE)],
    "fix_rate only": [(1.0, FIX_RATE)],
    "churn only": [(1.0, lambda f: f["weighted_churn"])],
}


def scores(files, terms):
    cols = [(w, z([float(g(f)) for f in files])) for w, g in terms]
    return [sum(w * c[i] for w, c in cols) for i in range(len(files))]


def auc(score, truth):
    """Mann-Whitney AUC with ties counted half."""
    pos = [s for s, t in zip(score, truth) if t]
    neg = [s for s, t in zip(score, truth) if not t]
    if not pos or not neg:
        return None
    ranked = sorted((s, i) for i, s in enumerate(score))
    ranks = [0.0] * len(score)
    i = 0
    while i < len(ranked):
        j = i
        while j + 1 < len(ranked) and ranked[j + 1][0] == ranked[i][0]:
            j += 1
        for k in range(i, j + 1):
            ranks[ranked[k][1]] = (i + j) / 2 + 1
        i = j + 1
    rpos = sum(r for r, t in zip(ranks, truth) if t)
    return (rpos - len(pos) * (len(pos) + 1) / 2) / (len(pos) * len(neg))


def precision_at(score, truth, files, k):
    order = sorted(range(len(files)), key=lambda i: (-score[i], files[i]["file"]))[:k]
    return sum(truth[i] for i in order) / max(1, min(k, len(order)))


# --- history ------------------------------------------------------------------------------------------------------


def git(d, *args):
    return subprocess.run(["git", "-C", str(d), *args], capture_output=True, text=True).stdout


def fix_files_after(repo, start, horizon_days):
    """Files touched by fix commits in (start, start + horizon]."""
    log = git(repo, "log", "--no-merges", "--no-renames", "--name-only", f"--since={start + 1}", f"--until={start + horizon_days * DAY}",
              "--format=@@%ct\t%s", "HEAD")
    out, fix = set(), False
    for line in log.splitlines():
        if line.startswith("@@"):
            t, _, s = line[2:].partition("\t")
            fix = is_fix(s) and start < int(t) <= start + horizon_days * DAY
        elif line.strip() and fix:
            out.add(line.strip())
    return out


def checkout(repo, sha, work):
    wt = work / f"{repo.name}@{sha[:12]}"
    shutil.rmtree(wt, ignore_errors=True)
    subprocess.run(["git", "clone", "-q", "--shared", "--no-checkout", str(repo), str(wt)], check=True, capture_output=True)
    subprocess.run(["git", "-C", str(wt), "-c", "advice.detachedHead=false", "checkout", "-q", "--detach", sha], check=True, capture_output=True)
    # A shallow source makes a shallow clone; keep the source's boundary so the walk can leave it out.
    src_shallow = repo / ".git" / "shallow"
    if src_shallow.exists() and not (wt / ".git" / "shallow").exists():
        shutil.copy(src_shallow, wt / ".git" / "shallow")
    return wt


def run_repo(repo, a, work, caches):
    cutoff = a.before
    parent = git(repo, "rev-list", "-1", f"--before={cutoff}", "HEAD").strip()
    if not parent:
        return None, f"{repo.name}: no commit before the cutoff"
    t0 = int(git(repo, "log", "-1", "--format=%ct", parent).strip() or 0)
    truth_all = fix_files_after(repo, t0, a.horizon)
    if len(truth_all) < a.min_truth:
        return None, f"{repo.name}: {len(truth_all)} fix-touched files"
    wt = checkout(repo, parent, work)
    cache = caches / repo.name / parent[:12]
    code, _, err = kc(a.bin, ["index", str(wt), "--threads", "2"], cache, timeout=3600)
    if code != 0:
        return None, f"{repo.name}: index failed {err[-200:]}"
    ans = kc_json(a.bin, ["hotspots", "-n", "1000000", "--as-of", "head", "--json", "--no-sync", "--repo", str(wt)], cache)
    shutil.rmtree(wt, ignore_errors=True)
    if "error" in ans:
        return None, f"{repo.name}: {ans['error']}"
    files = [f for f in ans["items"] if not (a.nontest and is_test(f["file"]))]
    truth = [f["file"] in truth_all for f in files]
    if sum(truth) < a.min_truth or len(files) < 30:
        return None, f"{repo.name}: {sum(truth)} truth files among {len(files)}"
    row = {"repo": repo.name, "parent": parent, "time": t0, "files": len(files), "truth": sum(truth)}
    for name, terms in FORMULAS.items():
        s = scores(files, terms)
        row[name] = {"auc": auc(s, truth), "p@10": precision_at(s, truth, files, 10), "p@20": precision_at(s, truth, files, 20)}
    # The binary's own concern, to check the formula mirror.
    s = [f["concern"] for f in files]
    row["binary concern"] = {"auc": auc(s, truth), "p@10": precision_at(s, truth, files, 10), "p@20": precision_at(s, truth, files, 20)}
    return row, None


def is_test(path):
    from common import is_test_path
    return is_test_path(path)


def defects(a):
    root = Path(a.root).expanduser()
    out = Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    work = out / "work"
    work.mkdir(exist_ok=True)
    caches = Path(a.caches) if a.caches else EVAL_DIR / "defects" / "index"
    repos = sorted(d for d in root.iterdir() if (d / ".git").exists())
    rng = random.Random(a.seed)
    rng.shuffle(repos)
    rows, notes = [], []
    for repo in repos:
        if len(rows) >= a.repos:
            break
        # Cheap pre-filter: enough fix commits in the horizon at all.
        row, note = run_repo(repo, a, work, caches)
        if row:
            rows.append(row)
            print(f"{repo.name}: {row['files']} files, {row['truth']} truth; "
                  + ", ".join(f"{k} auc {row[k]['auc']:.3f}" for k in list(FORMULAS)[:2]), flush=True)
        else:
            notes.append(note)
    (out / "results.jsonl").write_text("".join(json.dumps(r) + "\n" for r in rows))
    (out / "skipped.txt").write_text("\n".join(notes) + "\n")
    report(rows)


def report(rows):
    names = list(FORMULAS) + ["binary concern"]
    base = names[0]
    print(f"\n{len(rows)} repos, {sum(r['files'] for r in rows)} files, {sum(r['truth'] for r in rows)} truth files\n")
    print("| formula | AUC | Δ AUC vs old | P@10 | Δ P@10 vs old | P@20 |")
    print("|---|---|---|---|---|---|")
    for n in names:
        cell = []
        for m in ["auc", "p@10", "p@20"]:
            vals = [r[n][m] for r in rows if r[n][m] is not None]
            lo, hi = bootstrap(vals)
            cell.append(f"{sum(vals) / len(vals):.3f} [{lo:.3f}, {hi:.3f}]" if vals else "–")
            if m in ("auc", "p@10"):
                d = [r[n][m] - r[base][m] for r in rows if r[n][m] is not None and r[base][m] is not None]
                lo, hi = bootstrap(d)
                cell.append(f"{sum(d) / len(d):+.3f} [{lo:+.3f}, {hi:+.3f}]" if d else "–")
        print(f"| {n} | " + " | ".join(cell) + " |")


def add_parsers(sub):
    p = sub.add_parser("defects", help="defect prediction by history replay: concern scores vs later fix commits")
    p.add_argument("--bin", default=str(Path(__file__).resolve().parents[2] / "target" / "release" / "knowcode"))
    p.add_argument("--root", default=str(GITLAB), help="directory of git clones")
    p.add_argument("--before", required=True, help="run at the last commit before this date (YYYY-MM-DD)")
    p.add_argument("--horizon", type=int, default=90, help="days after the parent in which fixes count")
    p.add_argument("--repos", type=int, default=30, help="repos to score")
    p.add_argument("--min-truth", type=int, default=3, help="fewest fix-touched files a repo needs")
    p.add_argument("--nontest", action="store_true", help="rank non-test files only")
    p.add_argument("--seed", type=int, default=1)
    p.add_argument("--caches", help="index caches (default $KNOWCODE_EVAL_DIR/defects/index)")
    p.add_argument("--out", required=True)
    p.set_defaults(func=defects)
