#!/usr/bin/env python3
"""Review-answers gate: a KnowCode op's answer for a diff, on review-v1's commits, scored against the files and tests
the later fix changed, with no model calls (eval/review-v1/README.md).

    review_gate.py run --knowcode BIN --work WORK [--tool kc_impact] [--args JSON] [--reviews FILE] [--repos a,b]
                       [--budgets 2000,4000,8000] [--out SUMMARY]
    review_gate.py compare BASE_SUMMARY CANDIDATE_SUMMARY

WORK/clones comes from `task_gate.py fetch`. `run` checks out each review's introducing commit as a worktree of its
clone (WORK/trees/<review>, made once), indexes it alone with the build's own cache, config and data
(WORK/kc/<binary hash>/<review>), starts `BIN serve --tools all` there, and sends `--tool` the diff from the commit's
parent (`{"diff": <parent>}`), with `--args` merged in. The answer is the tool's text, as a client reads it.

Labels are judged as task_gate does (a path is named when it appears in the text; within a budget of B bytes when its
first mention ends inside it), over all of a review's labels and, apart, over those its own diff didn't change
(`outside`). `compare` pairs two runs review by review: per budget, each review's recall under each, their mean
difference with wins, ties and losses, and the two median byte counts. The gate (GATE_BUDGET) is the pooled file
recall at that budget: the candidate's must be higher, at a median byte count no larger than the base's. It is
decided for each part of the set (review_labels.PART) on its own and for them combined.
"""
from __future__ import annotations

import argparse
from collections import defaultdict
import hashlib
import json
from pathlib import Path
import statistics
import subprocess
import sys

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import replay_gate  # noqa: E402
import review_labels  # noqa: E402
import task_gate  # noqa: E402
import task_labels  # noqa: E402

BUDGETS = task_gate.BUDGETS
GATE_BUDGET = '4000'
KEYS = ('files', 'tests', 'outside')


def tree(work, review):
    """The review's introducing commit, checked out once as a worktree of its repository's clone."""
    path = Path(work)/'trees'/review['id']
    if not (path/'.git').exists():
        path.parent.mkdir(parents=True, exist_ok=True)
        task_labels.git(Path(work)/'clones'/review['repo'], 'worktree', 'add', '--quiet', '--detach', str(path), review['commit'])
    return path


def judge(review, text, budgets):
    j = dict(task_gate.judge(review, text, budgets), part=review['part'])
    outside = [p for p in review['files'] + review['tests'] if p not in review['in_diff']]
    j['outside'] = task_gate.named(text, outside, budgets)
    return j


def metrics(judged, budgets):
    out = dict(reviews=len(judged), bytes_median=statistics.median([j['bytes'] for j in judged]) if judged else None,
               bytes_mean=round(statistics.mean([j['bytes'] for j in judged]), 1) if judged else None)
    for key in KEYS:
        for at in ['all', *map(str, budgets)]:
            out[f'{key}_recall@{at}'] = task_gate.recall(judged, key, at)[0]
        out[f'{key}_labels'] = sum(len(j[key]) for j in judged)
    out['reviews_with_a_file'] = sum(1 for j in judged if any(v['all'] for v in j['files'].values()))
    return out


def summarize(judged, budgets):
    by, parts = defaultdict(list), defaultdict(list)
    for j in judged:
        by[j['lang']].append(j)
        parts[j['part']].append(j)
    return dict(overall=metrics(judged, budgets), lang={k: metrics(v, budgets) for k, v in sorted(by.items())},
                part={k: metrics(v, budgets) for k, v in sorted(parts.items())})


def run(args):
    reviews = [json.loads(l) for l in Path(args.reviews).read_text().splitlines() if l.strip()]
    if args.repos:
        reviews = [r for r in reviews if r['repo'] in args.repos.split(',')]
    budgets = tuple(int(b) for b in args.budgets.split(','))
    binary = Path(args.knowcode).resolve()
    work = Path(args.work).resolve()
    hashed = hashlib.sha256(binary.read_bytes()).hexdigest()[:16]
    extra = json.loads(args.args) if args.args else {}
    judged, texts, errors = [], {}, {}
    for r in reviews:
        path = tree(work, r)
        env = replay_gate.build_env(work/'kc'/hashed/r['id'])
        subprocess.run([str(binary), 'index', str(path)], env=env, cwd=path, check=True, capture_output=True)
        failed, text = task_gate.ask(binary, env, path, args.tool, {'diff': r['parent'], **extra})
        if failed:
            errors[r['id']] = text[:300]
        texts[r['id']] = text
        judged.append(judge(r, text, budgets))
        print(f"{r['id']}: {judged[-1]['bytes']} bytes", file=sys.stderr)
    version = subprocess.run([str(binary), '--version'], capture_output=True, text=True).stdout.strip()
    summary = dict(knowcode=version, binary=hashed, tool=args.tool, args=extra, budgets=budgets, errors=errors,
                   **summarize(judged, budgets))
    if args.out:
        out = Path(args.out)
        out.write_text(json.dumps(summary, indent=1, sort_keys=True) + '\n')
        out.with_suffix('.judged.jsonl').write_text(''.join(json.dumps(j, sort_keys=True) + '\n' for j in judged))
        out.with_suffix('.answers.jsonl').write_text(''.join(json.dumps({'id': k, 'text': v}) + '\n' for k, v in texts.items()))
    print(report(summary))


def fmt(x):
    return task_gate.fmt(x)


def report(summary):
    budgets = [str(b) for b in summary['budgets']]
    head = ['Set', 'Reviews', 'Median bytes'] + [f'Files @{b}' for b in budgets] + ['Files, all'] + \
           [f'Tests @{b}' for b in budgets] + ['Tests, all'] + [f'Outside @{GATE_BUDGET}']
    out = [f"KnowCode {summary['knowcode']} ({summary['binary']}), `{summary['tool']}` {json.dumps(summary['args'])}", '',
           '| ' + ' | '.join(head) + ' |', '| --- |' + ' ---: |' * (len(head) - 1)]
    rows = [*summary['lang'].items(), *((f'part {k}', v) for k, v in summary['part'].items()), ('all', summary['overall'])]
    for name, m in rows:
        cells = [name, m['reviews'], fmt(m['bytes_median'])] + [fmt(m[f'files_recall@{b}']) for b in budgets] + \
                [fmt(m['files_recall@all'])] + [fmt(m[f'tests_recall@{b}']) for b in budgets] + \
                [fmt(m['tests_recall@all']), fmt(m[f'outside_recall@{GATE_BUDGET}'])]
        out.append('| ' + ' | '.join(str(c) for c in cells) + ' |')
    if summary['errors']:
        out += ['', f"Errors: {len(summary['errors'])}"]
    return '\n'.join(out)


def per_review(j, key, at):
    """One review's recall of its `key` labels at `at`, or None when it has none."""
    v = j[key].values()
    return sum(x[at] for x in v) / len(v) if v else None


def paired(base, cand, key, at):
    """Over the reviews both runs answered that have `key` labels: count, mean difference (candidate − base), wins,
    ties and losses of the candidate."""
    b = {j['id']: j for j in base}
    diffs = []
    for c in cand:
        if c['id'] in b and (x := per_review(c, key, at)) is not None:
            diffs.append(x - per_review(b[c['id']], key, at))
    return dict(n=len(diffs), mean=round(statistics.mean(diffs), 4) if diffs else None,
                wins=sum(d > 0 for d in diffs), ties=sum(d == 0 for d in diffs), losses=sum(d < 0 for d in diffs))


def verdict(base_summary, cand_summary, part=None):
    b, c = ((s['overall'] if part is None else s['part'][part]) for s in (base_summary, cand_summary))
    higher = (c[f'files_recall@{GATE_BUDGET}'] or 0) > (b[f'files_recall@{GATE_BUDGET}'] or 0)
    smaller = c['bytes_median'] <= b['bytes_median']
    return higher and smaller


def compare(base_path, cand_path):
    base, cand = (json.loads(Path(p).read_text()) for p in (base_path, cand_path))
    bj, cj = ([json.loads(l) for l in Path(p).with_suffix('.judged.jsonl').read_text().splitlines() if l.strip()]
              for p in (base_path, cand_path))
    budgets = ['all', *map(str, base['budgets'])]
    out = [f"`{base['tool']}` {json.dumps(base['args'])} → `{cand['tool']}` {json.dumps(cand['args'])}", '',
           f"Median bytes: {fmt(base['overall']['bytes_median'])} → {fmt(cand['overall']['bytes_median'])}", '',
           '| Labels | Budget | Base | Candidate | Reviews | Mean paired difference | Wins | Ties | Losses |',
           '| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |']
    for key in KEYS:
        for at in budgets:
            p = paired(bj, cj, key, at)
            out.append(f"| {key} | {at} | {fmt(base['overall'][f'{key}_recall@{at}'])} | "
                       f"{fmt(cand['overall'][f'{key}_recall@{at}'])} | {p['n']} | {fmt(p['mean'])} | {p['wins']} | "
                       f"{p['ties']} | {p['losses']} |")
    out += ['']
    for part in [*sorted(set(base['part']) & set(cand['part'])), None]:
        name = 'combined' if part is None else f'part {part}'
        out.append(f"Gate, {name} (files @{GATE_BUDGET} higher, median bytes no larger): "
                   f"{'pass' if verdict(base, cand, part) else 'fail'}")
    return '\n'.join(out)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest='cmd', required=True)
    r = sub.add_parser('run')
    r.add_argument('--knowcode', required=True)
    r.add_argument('--work', required=True)
    r.add_argument('--tool', default='kc_impact')
    r.add_argument('--args', help='JSON object merged into every call (a limit, a depth)')
    r.add_argument('--reviews', default=str(review_labels.REVIEWS))
    r.add_argument('--repos')
    r.add_argument('--budgets', default=','.join(map(str, BUDGETS)))
    r.add_argument('--out')
    c = sub.add_parser('compare')
    c.add_argument('base')
    c.add_argument('candidate')
    args = ap.parse_args()
    if args.cmd == 'run':
        run(args)
    else:
        print(compare(args.base, args.candidate))


if __name__ == '__main__':
    main()
