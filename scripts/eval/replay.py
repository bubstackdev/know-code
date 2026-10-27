"""`mine`, `replay` and `report`: ground truth from git history.

`mine` turns recent commits into tasks: each commit that touches 2 to 30 indexed source files (no merges, bulk renames
or whitespace-only changes) becomes one JSONL row with its parent, subject and changed files, each flagged as test
code or not.

`replay` checks out each sampled commit's *parent* into a scratch clone with its own index cache, so no answer can see
the change, and runs the scorers in a scorer file (eval/scorers.toml) against it. A scorer is a knowcode command
template plus a JSON path to the file paths in its answer; the commit's real changes are the truth. `report` prints
recall@k, precision@k and Acc@k with 95% intervals.

Scorer format:

    [[scorer]]
    name = "impact"
    task = "per_file"          # per_file: one task per input file; per_commit: one task per commit
    input = "source"           # input files: "source" (changed non-test files) | "any" (every changed file) |
                               # "none" (the command takes the subject, not files)
    truth = "others"           # others: changed files other than the input | tests: changed test files
    argv = ["impact", "--file", "{file}", "-n", "1000"]
    files = "items[].affected[].file"   # or a list of paths, read in order
    keep = "all"               # which answer files count: "all" | "tests" | "nontest"
    k = [5, 10, 20]

`argv` placeholders: `{file}` (per_file; with per_commit the command runs once per input file and the answers merge,
best rank first), `{files}` (a whole argument: every input file, one argument each, in one run), `{subject}` (the
commit subject without ticket ids), `{terms}` (its keywords, space-separated),
`{terms_regex}` (the keywords as a case-insensitive alternation). Only files that existed at the parent count, as
inputs and as truth: no tool can name a file that doesn't exist yet.
"""

import json
import os
import random
import re
import shutil
import subprocess
import time
from collections import Counter, defaultdict
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from common import EVAL_DIR, GITLAB, ROOT, bootstrap, expand, is_source, is_test_path, json_path, kc, kc_json, load_toml, pct, wilson

SKIP_COMPONENTS = {"node_modules", "Pods", "Carthage", "DerivedData", "vendor", "third_party", ".venv", "venv"}


def git(d, *args, check=True):
    out = subprocess.run(["git", "-C", str(d), *args], capture_output=True, text=True)
    if check and out.returncode != 0:
        raise RuntimeError(f"git {' '.join(args)} in {d}: {out.stderr.strip()}")
    return out.stdout


# --- mine -------------------------------------------------------------------------------------------------------


def indexed_paths(bin_, d, cache):
    """Paths knowcode indexes at the repo's HEAD (the hidden `files` command) and every path tracked there, or None
    if it can't say."""
    code, out, _ = kc(bin_, ["files", "--repo", str(d)], cache)
    if code != 0:
        return None
    rows = [f.split("\t") for f in out.splitlines() if f.count("\t") >= 2]
    indexed = {r[1] for r in rows if r[2].startswith(("indexed", "pending"))}
    return indexed, set(git(d, "ls-files", "-z").split("\0"))


def counts_as_source(path, known):
    if not is_source(path) or any(c in SKIP_COMPONENTS for c in path.split("/")):
        return False
    if known is None:
        return True
    # A path still tracked at HEAD counts only if knowcode indexes it there (ignore rules, generated files); one gone
    # since falls back to the extension rule.
    indexed, tracked = known
    return path in indexed or path not in tracked


def mine_repo(d, a, indexed):
    log = git(d, "log", "--no-merges", f"--since={a.days} days ago", "-M", "--name-status", "--format=%x01%H %P%x00%ct%x00%s", check=False)
    rows, why = [], Counter()
    for rec in log.split("\x01")[1:]:
        head, _, body = rec.partition("\n")
        shas, ct, subject = head.split("\x00", 2)
        shas = shas.split()
        if len(shas) != 2:
            why["shallow boundary" if len(shas) == 1 else "merge"] += 1
            continue
        sha, parent = shas
        files, renames = [], 0
        for line in body.strip().splitlines():
            parts = line.split("\t")
            status = parts[0][:1]
            if status == "R":
                renames += 1
                files.append({"path": parts[2], "old": parts[1], "status": "R"})
            elif status in "AMDT":
                files.append({"path": parts[1], "status": status})
        src = [f for f in files if counts_as_source(f["path"], indexed) or ("old" in f and counts_as_source(f["old"], indexed))]
        if not a.min_files <= len(src) <= a.max_files:
            why["too few source files" if len(src) < a.min_files else "too many source files"] += 1
            continue
        if renames and renames >= len(files) / 2:
            why["bulk rename"] += 1
            continue
        paths = sorted({p for f in src for p in (f["path"], f.get("old")) if p})
        if not git(d, "diff", "-w", "--ignore-blank-lines", "--no-renames", "--stat", parent, sha, "--", *paths, check=False).strip():
            why["whitespace only"] += 1
            continue
        for f in src:
            f["test"] = is_test_path(f["path"])
        rows.append({
            "repo": d.name, "sha": sha, "parent": parent, "time": int(ct), "subject": subject,
            "files": src, "other_files": len(files) - len(src),
        })
    return rows, why


def mine(a):
    root = expand(a.root)
    repos = [root / r for r in a.repos] if a.repos else sorted(p for p in root.iterdir() if (p / ".git").exists())
    cache = Path(a.cache or EVAL_DIR / "replay" / "head-cache")
    out = Path(a.out)
    out.parent.mkdir(parents=True, exist_ok=True)

    def one(d):
        indexed = None
        if a.bin:
            kc(a.bin, ["index", str(d), "--threads", "2"], cache, timeout=1800)
            indexed = indexed_paths(a.bin, d, cache)
        return d, *mine_repo(d, a, indexed)

    total, why, per_repo = 0, Counter(), Counter()
    with ThreadPoolExecutor(a.jobs) as pool, out.open("w") as f:
        for d, rows, w in pool.map(one, repos):
            why.update(w)
            for r in rows:
                f.write(json.dumps(r) + "\n")
            per_repo[d.name] += len(rows)
            total += len(rows)
    print(f"{total} commits from {sum(1 for v in per_repo.values() if v)} of {len(repos)} repos -> {out}")
    print("left out: " + ", ".join(f"{k} {v}" for k, v in why.most_common()))
    for name, n in per_repo.most_common(25):
        print(f"  {n:5} {name}")


# --- replay -----------------------------------------------------------------------------------------------------

TICKET = re.compile(r"\[[^\]]*\]|\b[A-Z][A-Z0-9]+-\d+\b|\bsc-\d+\b|#\d+|\bhttps?://\S+|\(\s*\)", re.I)
PREFIX = re.compile(r"^\s*(feat|fix|chore|refactor|docs|test|tests|style|perf|build|ci|revert|wip|hotfix|bugfix)(\([^)]*\))?!?\s*:\s*", re.I)
STOP = set("""a an and are as at be been but by can could did do does done for from get got has have in into is it its
of on or our so than that the their them then there these this those to too up use used uses using via was were when
where which while will with without would add added adds adding fix fixed fixes fixing update updated updates updating
remove removed removes removing change changed changes changing make made makes new now not no more less also only
some all any should must may minor small cleanup clean code issue issues bug bugs work working merge branch
version bump initial misc stuff test tests""".split())


def clean_subject(s):
    s = PREFIX.sub("", re.sub(r"\(\s*\)", " ", TICKET.sub(" ", s)))
    return re.sub(r"\s+", " ", s).strip(" -:;,.")


def terms(s):
    words = re.findall(r"[A-Za-z][A-Za-z0-9]{2,}", s)
    seen, out = set(), []
    for w in words:
        lw = w.lower()
        if lw not in STOP and lw not in seen:
            seen.add(lw)
            out.append(w)
    return out[:8]


def expand_argv(argv, subst, files=()):
    """`{files}`, a whole argument, becomes one argument per input file."""
    out = []
    for a in argv:
        if a == "{files}":
            out.extend(files)
        else:
            out.append(a.format(**subst))
    return out


def ranked_files(answer, path, keep):
    """Files at `path` (or each of a list of paths, in order) in answer order, deduplicated."""
    seen, out = set(), []
    paths = [path] if isinstance(path, str) else path
    for f in (v for p in paths for v in json_path(answer, p)):
        if not isinstance(f, str) or f in seen:
            continue
        if keep == "tests" and not is_test_path(f) or keep == "nontest" and is_test_path(f):
            continue
        seen.add(f)
        out.append(f)
    return out


def score_task(ranked, truth, ks):
    hits = [f in truth for f in ranked]
    row = {"answer": len(ranked), "truth": len(truth), "hits_all": sum(hits)}
    for k in ks:
        h = sum(hits[:k])
        row[f"hits@{k}"] = h
        row[f"recall@{k}"] = h / len(truth)
        row[f"precision@{k}"] = h / min(k, len(ranked)) if ranked else None
        row[f"acc@{k}"] = int(h > 0)
    return row


def at_parent(c):
    """Changed files that existed at the parent: their path there, test flag."""
    out = []
    for f in c["files"]:
        if f["status"] == "A":
            continue
        out.append({"path": f.get("old", f["path"]), "test": is_test_path(f.get("old", f["path"]))})
    return out


def tasks_for(scorer, c):
    """(inputs, truth) pairs for one commit under one scorer."""
    files = at_parent(c)
    kind = scorer.get("input", "source")
    if kind == "source":
        inputs = [f["path"] for f in files if not f["test"]]
    elif kind == "any":
        inputs = [f["path"] for f in files]
    else:
        inputs = []
    if scorer.get("truth", "others") == "tests":
        truth_all = {f["path"] for f in files if f["test"]}
    else:
        truth_all = {f["path"] for f in files}
    if scorer.get("task", "per_file") == "per_file":
        return [([i], truth_all - {i}) for i in inputs]
    return [(inputs, truth_all - set(inputs))]


def run_scorer(scorer, c, wt, bin_, cache):
    ks = scorer.get("k", [5, 10, 20])
    subject = clean_subject(c["subject"])
    ts = terms(subject)
    rows = []
    per_file = any("{file}" in a for a in scorer["argv"])
    uses_terms = any("{terms" in a for a in scorer["argv"])
    for inputs, truth in tasks_for(scorer, c):
        if not truth or (per_file or "{files}" in scorer["argv"]) and not inputs or uses_terms and not ts:
            continue
        subst = {"subject": subject, "terms": " ".join(ts), "terms_regex": "(" + "|".join(re.escape(t) for t in ts) + ")"}
        merged, errors = {}, []
        for f in (inputs if per_file else [None]):
            argv = expand_argv(scorer["argv"], {**subst, "file": f or ""}, inputs) + ["--json", "--no-sync", "--repo", str(wt)]
            ans = kc_json(bin_, argv, cache)
            if "error" in ans:
                errors.append(ans["error"])
                continue
            for rank, path in enumerate(ranked_files(ans, scorer["files"], scorer.get("keep", "all"))):
                merged[path] = min(rank, merged.get(path, rank))
        ranked = [p for p, _ in sorted(merged.items(), key=lambda x: (x[1], x[0])) if p not in inputs]
        row = {
            "scorer": scorer["name"], "repo": c["repo"], "sha": c["sha"], "input": inputs[0] if len(inputs) == 1 else inputs,
            "query": subst["terms"] if uses_terms else None, "top": ranked[:5], "missed": sorted(truth - set(ranked))[:10],
            **score_task(ranked, truth, ks),
        }
        if errors:
            row["errors"] = errors[:3]
        rows.append(row)
    return rows


def checkout(c, repos_root, work):
    """A scratch clone of the repo at the commit's parent, sharing the corpus clone's objects (read only)."""
    wt = work / f"{c['repo']}@{c['parent'][:12]}"
    if (wt / ".git").exists() and git(wt, "rev-parse", "HEAD", check=False).strip() == c["parent"]:
        return wt
    shutil.rmtree(wt, ignore_errors=True)
    subprocess.run(["git", "clone", "-q", "--shared", "--no-checkout", str(repos_root / c["repo"]), str(wt)], check=True, capture_output=True)
    git(wt, "-c", "advice.detachedHead=false", "checkout", "-q", "--detach", c["parent"])
    return wt


def replay_group(key, commits, scorers, a, repos_root, work, caches):
    repo, parent = key
    t0 = time.time()
    try:
        wt = checkout(commits[0], repos_root, work)
    except Exception as e:  # noqa: BLE001 - a broken checkout skips its commits, reported below
        return [], f"{repo}@{parent[:12]}: checkout failed: {e}"
    cache = caches / repo / parent[:12]
    code, _, err = kc(a.bin, ["index", str(wt), "--threads", str(a.threads)], cache, timeout=3600)
    if code != 0:
        return [], f"{repo}@{parent[:12]}: index failed: {err.strip()[-200:]}"
    rows = []
    for c in commits:
        for s in scorers:
            rows += run_scorer(s, c, wt, a.bin, cache)
    if not a.keep_checkouts:
        shutil.rmtree(wt, ignore_errors=True)
    return rows, f"{repo}@{parent[:12]}: {len(rows)} tasks, {time.time() - t0:.0f} s"


def sample(commits, a):
    rng = random.Random(a.seed)
    by_repo = defaultdict(list)
    for c in commits:
        by_repo[c["repo"]].append(c)
    names = sorted(r for r, cs in by_repo.items() if len(cs) >= a.min_commits)
    if a.repos:
        names = [r for r in names if r in set(a.repos)]
    rng.shuffle(names)
    names = sorted(names[: a.max_repos])
    per = max(1, a.sample // max(1, len(names)))
    picked = []
    for r in names:
        cs = sorted(by_repo[r], key=lambda c: c["sha"])
        rng.shuffle(cs)
        picked += cs[:per]
    return picked


def read_jsonl(p):
    return [json.loads(line) for line in Path(p).read_text().splitlines() if line.strip()]


def replay(a):
    # Each parent checkout's history ends at the parent: hotspot windows end there too, not today.
    os.environ["KNOWCODE_NOW"] = "head"
    # A saved sample replays exactly the same commits, so two builds compare task by task.
    picked = read_jsonl(a.sample_file) if a.sample_file else sample(read_jsonl(a.dataset), a)
    scorer_file = load_toml(a.scorers)["scorer"]
    scorers = [s for s in scorer_file if s["name"] in a.only] if a.only else scorer_file
    if not scorers:
        raise SystemExit(f"no scorer named {a.only} in {a.scorers}")
    out = Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    work = Path(a.work)
    work.mkdir(parents=True, exist_ok=True)
    # Indexes are kept per build, so two builds never share one.
    build = re.sub(r"[^A-Za-z0-9.+-]+", "_", kc(a.bin, ["--version"], ".")[1].strip()) or "unknown"
    caches = Path(a.caches) if a.caches else EVAL_DIR / "replay" / "index" / build
    groups = defaultdict(list)
    for c in picked:
        groups[(c["repo"], c["parent"])].append(c)
    print(f"{len(picked)} commits from {len({c['repo'] for c in picked})} repos, {len(groups)} parent checkouts, "
          f"scorers: {', '.join(s['name'] for s in scorers)}", flush=True)
    t0 = time.time()
    rows = []
    with ThreadPoolExecutor(a.jobs) as pool:
        futs = [pool.submit(replay_group, k, v, scorers, a, expand(a.root), work, caches) for k, v in sorted(groups.items())]
        for i, fut in enumerate(futs, 1):
            r, msg = fut.result()
            rows += r
            print(f"[{i}/{len(futs)}] {msg}", flush=True)
    rows.sort(key=lambda r: (r["repo"], r["sha"], str(r["input"])))
    for s in scorers:
        with (out / f"{s['name']}.jsonl").open("w") as f:
            for r in rows:
                if r["scorer"] == s["name"]:
                    f.write(json.dumps(r) + "\n")
    with (out / "sample.jsonl").open("w") as f:
        for c in picked:
            f.write(json.dumps(c) + "\n")
    meta = {
        "dataset": str(a.dataset), "commits": len(picked), "repos": sorted({c["repo"] for c in picked}),
        "seed": a.seed, "binary": str(a.bin), "version": kc(a.bin, ["--version"], ".")[1].strip(),
        "seconds": round(time.time() - t0), "taken": time.strftime("%Y-%m-%d %H:%M:%S"),
    }
    (out / "_meta.json").write_text(json.dumps(meta, indent=1) + "\n")
    print(f"done in {meta['seconds']} s -> {out}\n")
    report_dir(out, [s["name"] for s in scorers])


# --- report -----------------------------------------------------------------------------------------------------


def mean_ci(vals, groups):
    if not vals:
        return "–"
    lo, hi = bootstrap(vals, groups=groups)
    return f"{pct(sum(vals) / len(vals))} [{pct(lo)}, {pct(hi)}]"


def summarize(rows, ks):
    groups = [r["repo"] for r in rows]
    lines = []
    for k in ks:
        rec = [r[f"recall@{k}"] for r in rows]
        prec = [(r[f"precision@{k}"], r["repo"]) for r in rows if r[f"precision@{k}"] is not None]
        acc = [r[f"acc@{k}"] for r in rows]
        lo, hi = wilson(sum(acc), len(rows))
        hits = sum(r[f"hits@{k}"] for r in rows)
        truth = sum(r["truth"] for r in rows)
        plo, phi = wilson(hits, truth)
        lines.append(
            f"| {k} | {mean_ci(rec, groups)} | {pct(hits / truth) if truth else '–'} [{pct(plo)}, {pct(phi)}] "
            f"| {mean_ci([p for p, _ in prec], [g for _, g in prec])} | {mean_ci(acc, groups)} | [{pct(lo)}, {pct(hi)}] |"
        )
    return lines


def task_key(r):
    return (r["repo"], r["sha"], json.dumps(r["input"]))


def compare(rows, base, ks):
    """Paired differences against a baseline run over the tasks both scored, with a repo-clustered bootstrap."""
    old = {task_key(r): r for r in base}
    pairs = [(r, old[task_key(r)]) for r in rows if task_key(r) in old]
    if not pairs:
        return ["No tasks in common with the baseline."]
    lines = [f"Against the baseline, over {len(pairs)} tasks both runs scored "
             f"({len(rows) - len(pairs)} only here, {len(base) - len(pairs)} only there):\n",
             "| k | Δ Recall@k (95% CI) | Δ Acc@k (95% CI) | Tasks better / worse |", "|---:|---|---|---|"]
    groups = [r["repo"] for r, _ in pairs]
    for k in ks:
        dr = [r[f"recall@{k}"] - b[f"recall@{k}"] for r, b in pairs]
        da = [r[f"acc@{k}"] - b[f"acc@{k}"] for r, b in pairs]
        (rlo, rhi), (alo, ahi) = bootstrap(dr, groups=groups), bootstrap(da, groups=groups)
        better, worse = sum(d > 0 for d in dr), sum(d < 0 for d in dr)
        lines.append(f"| {k} | {100 * sum(dr) / len(dr):+.1f} pts [{100 * rlo:+.1f}, {100 * rhi:+.1f}] "
                     f"| {100 * sum(da) / len(da):+.1f} pts [{100 * alo:+.1f}, {100 * ahi:+.1f}] | {better} / {worse} |")
    return lines


def report_dir(out, names=None, against=None, by_repo=False):
    out = Path(out)
    names = names or sorted(p.stem for p in out.glob("*.jsonl") if p.stem != "sample")
    for name in names:
        p = out / f"{name}.jsonl"
        rows = read_jsonl(p) if p.exists() else []
        print(f"### {name}\n")
        if not rows:
            print("No scorable tasks.\n")
            continue
        ks = sorted(int(k.split("@")[1]) for k in rows[0] if k.startswith("recall@"))
        empty = sum(1 for r in rows if r["answer"] == 0)
        errs = sum(1 for r in rows if r.get("errors"))
        print(f"{len(rows)} tasks over {len({r['sha'] for r in rows})} commits in {len({r['repo'] for r in rows})} repos; "
              f"{empty} empty answers, {errs} with errors; mean truth size {sum(r['truth'] for r in rows) / len(rows):.1f}, "
              f"mean answer size {sum(r['answer'] for r in rows) / len(rows):.1f}; recall over the whole answer "
              f"{pct(sum(r['hits_all'] for r in rows) / sum(r['truth'] for r in rows))}.\n")
        print("| k | Recall@k (mean [95% CI, repo bootstrap]) | Recall@k (pooled [Wilson]) | Precision@k (mean over non-empty answers) "
              "| Acc@k (any hit; mean [repo bootstrap]) | Acc@k [Wilson] |")
        print("|---:|---|---|---|---|---|")
        print("\n".join(summarize(rows, ks)))
        print()
        if by_repo:
            k = ks[len(ks) // 2]
            print(f"| Repo | Tasks | Recall@{k} | Acc@{k} |\n|---|---:|---:|---:|")
            per = defaultdict(list)
            for r in rows:
                per[r["repo"]].append(r)
            for repo, rs in sorted(per.items()):
                print(f"| {repo} | {len(rs)} | {pct(sum(r[f'recall@{k}'] for r in rs) / len(rs))} "
                      f"| {pct(sum(r[f'acc@{k}'] for r in rs) / len(rs))} |")
            print()
        if against:
            b = Path(against) / f"{name}.jsonl"
            print("\n".join(compare(rows, read_jsonl(b), ks)) if b.exists() else f"No {b}.")
            print()


def report(a):
    report_dir(a.results, a.only, a.against, a.by_repo)


def add_parsers(sub):
    p = sub.add_parser("mine", help="extract replay tasks from git history into JSONL")
    p.add_argument("--root", default=str(GITLAB), help="directory of repos")
    p.add_argument("--repos", nargs="*", help="repo directory names (default: every repo under root)")
    p.add_argument("--out", default=str(EVAL_DIR / "replay" / "commits.jsonl"))
    p.add_argument("--bin", type=Path, help="knowcode binary; when given, paths it doesn't index at HEAD are left out")
    p.add_argument("--cache", help="index cache for --bin (default: <eval dir>/replay/head-cache)")
    p.add_argument("--days", type=int, default=400)
    p.add_argument("--min-files", type=int, default=2)
    p.add_argument("--max-files", type=int, default=30)
    p.add_argument("--jobs", type=int, default=4)
    p.set_defaults(func=mine)

    p = sub.add_parser("replay", help="score knowcode on sampled commits, run at each commit's parent")
    p.add_argument("--dataset", default=str(EVAL_DIR / "replay" / "commits.jsonl"))
    p.add_argument("--scorers", default=str(ROOT / "eval" / "scorers.toml"))
    p.add_argument("--only", nargs="*", help="scorer names to run")
    p.add_argument("--bin", required=True, type=Path)
    p.add_argument("--root", default=str(GITLAB), help="directory holding the dataset's repos")
    p.add_argument("--out", required=True, help="results directory (one JSONL per scorer)")
    p.add_argument("--work", default=str(EVAL_DIR / "replay" / "checkouts"), help="scratch clones at each parent")
    p.add_argument("--caches", help="index caches, one per repo and parent (default: <eval dir>/replay/index/<build>)")
    p.add_argument("--sample-file", help="replay exactly these commits (a run's sample.jsonl) instead of sampling")
    p.add_argument("--keep-checkouts", action="store_true")
    p.add_argument("--sample", type=int, default=200, help="commits to sample")
    p.add_argument("--max-repos", type=int, default=20)
    p.add_argument("--min-commits", type=int, default=10, help="repos with fewer eligible commits are not sampled")
    p.add_argument("--repos", nargs="*", help="sample only from these repos")
    p.add_argument("--seed", type=int, default=1)
    p.add_argument("--jobs", type=int, default=4)
    p.add_argument("--threads", type=int, default=2, help="parser threads per index")
    p.set_defaults(func=replay)

    p = sub.add_parser("report", help="summarise replay results")
    p.add_argument("results")
    p.add_argument("--only", nargs="*")
    p.add_argument("--against", help="a baseline results directory: paired differences per task")
    p.add_argument("--by-repo", action="store_true", help="recall and Acc per repo")
    p.set_defaults(func=report)
