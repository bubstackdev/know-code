#!/usr/bin/env python3
"""review-v1: commits a later fix repaired, from public repositories, labelled from git alone (eval/review-v1/README.md).

    review_labels.py build --work WORK --out eval/review-v1/reviews.jsonl
    review_labels.py check --work WORK [--reviews eval/review-v1/reviews.jsonl]
    review_labels.py census --work WORK

The repositories and pins are tasks-v1's (scripts/eval/identity/repos.txt), cloned with their full history into
WORK/clones by `task_gate.py fetch`. A fix commit is one tasks-v1 would take (task_labels.FIX, not NOT_A_FIX, one
parent, at most MAX_FILES files, between 1 and MAX_SOURCE existing source files). The source lines it rewrites or
deletes, apart from blank and punctuation-only lines, are blamed (`git blame -w`) at its parent; the commit blamed for
the most of them, among those landed at most WINDOW_DAYS before the fix, is the introducing commit, when it is one
commit with one parent, changes at most MAX_INTRO_FILES files and at least one source file, and isn't a merge, a
revert or a reformat (NOT_INTRO). Its labels are the files the fix modified or deleted that exist at the introducing
commit: the source files, and apart, the tests (task_labels' test paths). Each introducing commit is one review,
labelled by the earliest fix repairing it. QUOTA reviews are drawn per language (`draw`). `check` recomputes every
review from git and exits 1 on a difference; `census` counts the candidates per repository, before any quota.

No repository code runs, and no model is asked anything.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import sys

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import task_labels  # noqa: E402
from task_labels import git, is_test  # noqa: E402

ROOT = HERE.parent.parent
REVIEWS = ROOT/'eval/review-v1/reviews.jsonl'
SEED = 'review-v1'
# The part of the set these repositories make: 1 is tasks-v1's repositories, frozen first. The Go and JVM
# repositories 0.7 adds are part 2, frozen on their own, so adding them can't move part 1's result.
PART = '1'
QUOTA = {'csharp': 25, 'typescript': 25, 'swift': 25}
WINDOW_DAYS = 90
MAX_INTRO_FILES = 30
# Introducing commits whose blame says who last touched a line rather than who wrote the code it holds.
NOT_INTRO = re.compile(r'(?i)(^merge\b|^revert\b|\bformat|whitespace|prettier|\blint|\bstyle\b|\brename|\bmove[ds]?\b|'
                       r'reorganiz|reorder|\bindent|eslint|swiftformat|dotnet[- ]format|line endings|\bcrlf\b)')
# Lines whose blame says nothing about the code: blank, or only brackets and separators.
TRIVIAL = re.compile(r'^[\s{}()\[\];,]*$')
HUNK = re.compile(r'^@@ -(\d+)(?:,(\d+))? \+')


def removed_lines(clone, fix, path):
    """The parent's line numbers the fix rewrites or deletes in `path`, apart from trivial lines."""
    patch = git(clone, 'diff', '-U0', '--no-color', '--no-ext-diff', '--no-textconv', f'{fix}^', fix, '--', path)
    old = git(clone, 'show', f'{fix}^:{path}').split('\n')
    lines = []
    for h in HUNK.finditer('\n'.join(l for l in patch.splitlines() if l.startswith('@@'))):
        start, count = int(h.group(1)), int(h.group(2) if h.group(2) is not None else 1)
        lines += [n for n in range(start, start + count) if n - 1 < len(old) and not TRIVIAL.match(old[n - 1])]
    return lines


def blame(clone, fix, path, lines):
    """Per commit, how many of `lines` of `path` at the fix's parent it last wrote (whitespace changes ignored)."""
    if not lines:
        return {}
    args = ['blame', '-w', '--porcelain']
    for n in lines:
        args += ['-L', f'{n},{n}']
    out = git(clone, *args, f'{fix}^', '--', path)
    counts = {}
    for l in out.splitlines():
        m = re.match(r'^([0-9a-f]{40}) \d+ \d+ \d+$', l)
        if m:
            counts[m.group(1)] = counts.get(m.group(1), 0) + 1
    return counts


def when(clone, sha):
    return int(git(clone, 'show', '-s', '--format=%ct', sha).strip())


def subject(clone, sha):
    return git(clone, 'show', '-s', '--format=%s', sha).strip()


def exists(clone, sha, path):
    return bool(git(clone, 'ls-tree', '--name-only', sha, '--', path).strip())


def introducing(clone, fix, lang):
    """The commit the fix repairs (see the module doc), with how many blamed lines it holds, or (None, reason)."""
    got = task_labels.labels(clone, fix, lang)
    if got is None:
        return None, 'fix shape'
    source, _ = got
    counts = {}
    for path in source:
        for sha, n in blame(clone, fix, path, removed_lines(clone, fix, path)).items():
            counts[sha] = counts.get(sha, 0) + n
    if not counts:
        return None, 'no blamed line'
    fixed_at = when(clone, fix)
    ok = {}
    for sha, n in counts.items():
        if sha == fix or fixed_at - when(clone, sha) > WINDOW_DAYS * 86400:
            continue
        parents = git(clone, 'rev-list', '--parents', '-n', '1', sha).split()[1:]
        if len(parents) != 1 or NOT_INTRO.search(subject(clone, sha)):
            continue
        ok[sha] = n
    if not ok:
        return None, 'none within window'
    sha = sorted(ok, key=lambda s: (-ok[s], task_labels.order(SEED, s)))[0]
    changed = git(clone, 'diff-tree', '--no-commit-id', '--name-only', '-r', sha).splitlines()
    if len(changed) > MAX_INTRO_FILES:
        return None, 'introducing commit too large'
    if not any(p.endswith(task_labels.SOURCE[lang]) for p in changed):
        return None, 'introducing commit changes no source'
    return (sha, ok[sha], sum(counts.values()), sorted(changed)), None


def review(clone, repo, lang, fix):
    """One review from a fix commit, or (None, reason)."""
    found, why = introducing(clone, fix, lang)
    if found is None:
        return None, why
    sha, blamed, removed, changed = found
    modified = [l.split('\t') for l in git(clone, 'diff-tree', '--no-commit-id', '--name-status', '-r', fix).splitlines()]
    code = sorted(p for status, *paths in modified if status[0] in 'MD' for p in paths[:1]
                  if p.endswith(task_labels.SOURCE[lang]) and not task_labels.GENERATED.search(p) and exists(clone, sha, p))
    files = [p for p in code if not is_test(p)]
    tests = [p for p in code if is_test(p)]
    if not files:
        return None, 'no label at the introducing commit'
    parent = git(clone, 'rev-parse', f'{sha}^').strip()
    days = round((when(clone, fix) - when(clone, sha)) / 86400, 1)
    return dict(id=f'{repo}-{sha[:10]}', part=PART, repo=repo, lang=lang, commit=sha, parent=parent, fix=fix, days=days,
                blamed=blamed, removed=removed, changed=changed, files=files, tests=tests,
                in_diff=sorted(p for p in files + tests if p in changed)), None


def candidates(clone, pin):
    log = git(clone, 'log', '--no-merges', f'--since={task_labels.SINCE}', '--format=%H%x1f%s%x1e', pin)
    commits = [c.strip('\n').split('\x1f') for c in log.split('\x1e') if c.strip()]
    return [sha for sha, s in commits
            if task_labels.FIX.search(s) and not task_labels.NOT_A_FIX.search(s) and len(s.split()) >= task_labels.MIN_WORDS]


def pool(work, repo, r):
    """Every review the repository's fix commits give, one per introducing commit: the earliest fix repairing it."""
    clone = Path(work)/'clones'/repo
    reasons, by = {}, {}
    fixes = candidates(clone, r['sha'])
    for fix in fixes:
        got, why = review(clone, repo, r['lang'], fix)
        if got is None:
            reasons[why] = reasons.get(why, 0) + 1
            continue
        key = (when(clone, fix), fix)
        if got['commit'] not in by or key < by[got['commit']][0]:
            by[got['commit']] = (key, got)
    return [g for _, g in by.values()], dict(fixes=len(fixes), reviews=len(by), dropped=reasons)


def draw(pools, quota):
    """Per language, up to quota[lang] reviews: its repositories in turn (in name order), one review at a time, each
    repository's reviews in SHA-256 order of `SEED:repo:commit`."""
    out = []
    for lang, n in sorted(quota.items()):
        queues = {repo: sorted(reviews, key=lambda g: task_labels.order(f'{SEED}:{repo}', g['commit']))
                  for repo, reviews in sorted(pools.items()) if reviews and reviews[0]['lang'] == lang}
        taken = []
        while len(taken) < n and any(queues.values()):
            for repo in sorted(queues):
                if queues[repo] and len(taken) < n:
                    taken.append(queues[repo].pop(0))
        out += sorted(taken, key=lambda g: g['id'])
    return out


def build(work, quota=None):
    pools = {}
    for repo, r in task_labels.repos().items():
        pools[repo], counts = pool(work, repo, r)
        print(f'{repo}: {json.dumps(counts, sort_keys=True)}', file=sys.stderr)
    return draw(pools, quota or QUOTA)


def census(work):
    return {repo: pool(work, repo, r)[1] for repo, r in task_labels.repos().items()}


def check(work, path):
    known = task_labels.repos()
    diffs = []
    for t in (json.loads(l) for l in Path(path).read_text().splitlines() if l.strip()):
        got, _ = review(Path(work)/'clones'/t['repo'], t['repo'], known[t['repo']]['lang'], t['fix'])
        if got != t:
            diffs.append(t['id'])
    return diffs


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest='cmd', required=True)
    b = sub.add_parser('build')
    b.add_argument('--work', required=True)
    b.add_argument('--out', default=str(REVIEWS))
    c = sub.add_parser('check')
    c.add_argument('--work', required=True)
    c.add_argument('--reviews', default=str(REVIEWS))
    n = sub.add_parser('census')
    n.add_argument('--work', required=True)
    args = ap.parse_args()
    if args.cmd == 'build':
        reviews = build(args.work)
        Path(args.out).parent.mkdir(parents=True, exist_ok=True)
        Path(args.out).write_text(''.join(json.dumps(t, sort_keys=True) + '\n' for t in reviews))
        print(f'{len(reviews)} reviews')
    elif args.cmd == 'census':
        print(json.dumps(census(args.work), indent=1, sort_keys=True))
    else:
        diffs = check(args.work, args.reviews)
        print('\n'.join(diffs) or 'labels match')
        sys.exit(1 if diffs else 0)


if __name__ == '__main__':
    main()
