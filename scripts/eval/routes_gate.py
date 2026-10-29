#!/usr/bin/env python3
"""Routes gate: KnowCode's served routes against routes-v1's labels, each framework's own listing, with no model calls.

    routes_gate.py run --knowcode BIN --reader BIN --work WORK --labels LABELS --programs PROGRAMS [--repos a,b]
                       [--out SUMMARY]
    routes_gate.py report SUMMARY

`run` indexes each repository's pinned clone (WORK/clones/REPO, fetched by identity_gate.py's `fetch` or by hand)
with the build's own cache under WORK/kc/<binary hash>/<repo>, never in the clone, and reads the routes it serves
through the reader (crates/kc-index/examples/served_routes.rs; not a product path, see eval/routes-v1/README.md). Then,
by the README's rule:

- only KnowCode's routes in the directories the listed programs compile count;
- a route matches a label when the methods are equal (or either is `*`) and the paths are equal once normalised
  (`norm`): without case and leading or trailing slashes, every parameter alike;
- per repository, precision is KnowCode's routes matching a label over all of them, recall the labels matched over
  all labels; per framework, pooled over its repositories and macro-averaged over them, with Wilson intervals.

It also samples SAMPLE routes per framework from the reader's list (seed SEED) and confirms each through `context` on
the symbol owning it: the agreement rate is reported beside the gate figures.
"""
from __future__ import annotations

import argparse
from collections import defaultdict
import hashlib
import json
import os
from pathlib import Path
import random
import re
import subprocess
import sys

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from common import wilson  # noqa: E402

GATE = 0.95
MIN_ROUTES = 50
SAMPLE = 50
SEED = 7
PARAM = re.compile(r'\{[^/{}]*\}|:[^/]+|\*[^/]*')


def norm(path):
    """A path as the routes ruling compares it: lower-case, no leading or trailing slash, every parameter `{}`, and no
    net/http end-of-path marker (`{$}`)."""
    p = (path or '').strip()
    if ' ' in p:  # a net/http pattern: "GET /x" is split by the caller; a host prefix stays as written
        p = p.split(' ', 1)[1]
    p = p.replace('{$}', '')
    p = PARAM.sub('{}', p.strip('/'))
    return p.lower().strip('/')


def method_and_path(label):
    """A label's method and path; a net/http pattern carries its method in the path (`GET /items/{id}`)."""
    method, path = label['method'], label['path']
    if label['framework'] == 'net/http' and ' ' in path:
        method, path = path.split(' ', 1)
    return (method or '*').upper(), norm(path)


def matches(a, b):
    return (a[0] == b[0] or '*' in (a[0], b[0])) and a[1] == b[1]


def jsonl(path):
    return [json.loads(line) for line in Path(path).read_text().splitlines() if line.strip()]


def binary_hash(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()[:16]


def env_for(base):
    return dict(os.environ, HOME=str(base/'home'), KNOWCODE_CACHE_DIR=str(base/'cache'),
                KNOWCODE_CONFIG_DIR=str(base/'config'), KNOWCODE_DATA_DIR=str(base/'data'), KNOWCODE_UPDATE_CHECK='0')


def covered(programs):
    """The repository directories the listed programs compile, and those whose test files they compile too (a harness
    test's own package): a program never compiles its packages' `_test.go` files."""
    dirs = {d for p in programs if p['status'] == 'listed' for d in p['dirs']}
    tests = {p['package'][len('test:'):] for p in programs if p['status'] == 'listed' and p['package'].startswith('test:')}
    return dirs, tests


def in_dirs(file, dirs):
    dirs, tests = dirs
    d = file.rsplit('/', 1)[0] if '/' in file else '.'
    if file.endswith('_test.go') and d not in tests:
        return False
    return d in dirs or (d == '.' and ('.' in dirs or '' in dirs))


def attribute(row, labels):
    """The framework a KnowCode route is counted under: a matching label's, else the framework of the labels
    registered in its directory, else the repository's only one; `None` when that can't be told."""
    key = (row['verb'] or '*').upper(), norm(row['path'])
    found = {l['framework'] for l in labels if matches(key, method_and_path(l))}
    if len(found) == 1:
        return found.pop()
    d = row['file'].rsplit('/', 1)[0] if '/' in row['file'] else ''
    near = {l['framework'] for l in labels for loc in l.get('locs', []) if loc.rsplit('/', 1)[0].endswith(d)}
    if len(near) == 1:
        return near.pop()
    frameworks = {l['framework'] for l in labels}
    return frameworks.pop() if len(frameworks) == 1 else None


def score_repo(labels, rows):
    """Per framework: KnowCode's routes and how many match, labels and how many are matched, and the misses."""
    out = defaultdict(lambda: dict(routes=0, routes_matched=0, labels=0, labels_matched=0, false=[], missed=[]))
    keyed = [(method_and_path(l), l) for l in labels]
    seen = set()
    for r in rows:
        key = ((r['verb'] or '*').upper(), norm(r['path']))
        if key in seen:
            continue
        seen.add(key)
        framework = attribute(r, labels)
        hit = any(matches(key, k) for k, _ in keyed)
        for f in ([framework] if framework else sorted({l['framework'] for l in labels})):
            out[f]['routes'] += 1
            out[f]['routes_matched'] += hit
            if not hit:
                out[f]['false'].append(f"{r['verb']} /{r['path']} at {r['file']}:{r['line']}")
    for k, l in keyed:
        f = out[l['framework']]
        f['labels'] += 1
        if any(matches(k, (((r['verb'] or '*').upper()), norm(r['path']))) for r in rows):
            f['labels_matched'] += 1
        else:
            f['missed'].append(f"{l['method']} {l['path']} ({', '.join(l['handlers'][:2])})")
    return dict(out)


def ratio(k, n):
    lo, hi = wilson(k, n)
    return dict(k=k, n=n, value=round(k / n, 4) if n else None, low=round(lo, 4), high=round(hi, 4))


def summarize(per_repo):
    """Per framework: pooled precision and recall, the macro average over repositories, and each repository's."""
    frameworks = defaultdict(dict)
    for repo, fs in per_repo.items():
        for f, s in fs.items():
            frameworks[f][repo] = s
    out = {}
    for f, repos in sorted(frameworks.items()):
        pooled = {k: sum(s[k] for s in repos.values()) for k in ('routes', 'routes_matched', 'labels', 'labels_matched')}
        per = {repo: dict(precision=ratio(s['routes_matched'], s['routes']), recall=ratio(s['labels_matched'], s['labels']))
               for repo, s in sorted(repos.items())}
        macro = {m: round(sum(p[m]['value'] for p in per.values() if p[m]['value'] is not None)
                          / max(1, sum(1 for p in per.values() if p[m]['value'] is not None)), 4)
                 for m in ('precision', 'recall')}
        precision, recall = ratio(pooled['routes_matched'], pooled['routes']), ratio(pooled['labels_matched'], pooled['labels'])
        gated = pooled['labels'] >= MIN_ROUTES
        out[f] = dict(precision=precision, recall=recall, macro=macro, repos=per, gated=gated,
                      passed=(not gated) or ((precision['value'] or 0) >= GATE and (recall['value'] or 0) >= GATE))
    return out


def confirm(binary, clone, env, row):
    """Whether `context` on the symbol owning a served route lists it as served."""
    target = f"{row['file']}:{row['start_line']}"
    r = subprocess.run([str(binary), 'context', target, '--json', '--no-sync'], env=env, cwd=clone, capture_output=True,
                       text=True)
    try:
        answer = json.loads(r.stdout)
    except json.JSONDecodeError:
        return False
    want = ((row['verb'] or '*').upper(), norm(row['path']))
    for item in answer.get('items', []):
        for h in item.get('http', []) or []:
            if h.get('role') == 'serves' and matches(want, ((h.get('verb') or '*').upper(), norm(h.get('path')))):
                return True
    return False


def run(args):
    labels = jsonl(args.labels)
    programs = json.loads(Path(args.programs).read_text())
    repos = sorted(set(args.repos.split(',')) if args.repos else {l['repo'] for l in labels})
    work = Path(args.work).resolve()
    binary, reader = Path(args.knowcode).resolve(), Path(args.reader).resolve()
    per_repo, served, sample_pool = {}, {}, defaultdict(list)
    for repo in repos:
        clone = work/'clones'/repo
        env = env_for(work/'kc'/binary_hash(binary)/repo)
        subprocess.run([str(binary), 'index', str(clone)], env=env, cwd=clone, check=True, capture_output=True)
        out = subprocess.run([str(reader), str(clone)], env=env, cwd=clone, check=True, capture_output=True, text=True)
        dirs = covered(programs.get(repo, []))
        rows = [r for r in map(json.loads, out.stdout.splitlines()) if in_dirs(r['file'], dirs)]
        mine = [l for l in labels if l['repo'] == repo]
        per_repo[repo] = score_repo(mine, rows)
        served[repo] = len(rows)
        for r in rows:
            f = attribute(r, mine)
            if f:
                sample_pool[f].append((repo, r))
    checks = {}
    for f, pool in sorted(sample_pool.items()):
        picked = random.Random(SEED).sample(pool, min(SAMPLE, len(pool)))
        ok = 0
        for repo, r in picked:
            clone = work/'clones'/repo
            ok += confirm(binary, clone, env_for(work/'kc'/binary_hash(binary)/repo), r)
        checks[f] = dict(sampled=len(picked), confirmed=ok)
    summary = dict(binary=binary_hash(binary), labels=str(args.labels), served=served, frameworks=summarize(per_repo),
                   context_check=checks, per_repo=per_repo)
    text = json.dumps(summary, indent=1, sort_keys=True) + '\n'
    if args.out:
        Path(args.out).write_text(text)
    print(report(summary))


def fmt(r):
    return '–' if r['value'] is None else f"{r['value']:.3f} ({r['k']}/{r['n']}, {r['low']:.3f}–{r['high']:.3f})"


def report(summary):
    out = ['| Framework | Labels | Precision, pooled | Recall, pooled | Macro precision | Macro recall | Gated | Pass | '
           '`context` check |', '| --- | ---: | --- | --- | ---: | ---: | --- | --- | ---: |']
    for f, s in summary['frameworks'].items():
        c = summary['context_check'].get(f, dict(sampled=0, confirmed=0))
        out.append(f"| {f} | {s['recall']['n']} | {fmt(s['precision'])} | {fmt(s['recall'])} | {s['macro']['precision']} | "
                   f"{s['macro']['recall']} | {'yes' if s['gated'] else 'no'} | {'yes' if s['passed'] else 'no'} | "
                   f"{c['confirmed']}/{c['sampled']} |")
    out += ['', '| Framework | Repository | Precision | Recall |', '| --- | --- | --- | --- |']
    for f, s in summary['frameworks'].items():
        for repo, r in s['repos'].items():
            out.append(f"| {f} | {repo} | {fmt(r['precision'])} | {fmt(r['recall'])} |")
    return '\n'.join(out)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest='cmd', required=True)
    r = sub.add_parser('run')
    for a in ('--knowcode', '--reader', '--work', '--labels', '--programs'):
        r.add_argument(a, required=True)
    r.add_argument('--repos')
    r.add_argument('--out')
    p = sub.add_parser('report')
    p.add_argument('summary')
    args = ap.parse_args()
    if args.cmd == 'run':
        run(args)
    else:
        print(report(json.loads(Path(args.summary).read_text())))


if __name__ == '__main__':
    main()
