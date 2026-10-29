#!/usr/bin/env python3
"""Draws compare-v1's questions and their labels from the toolchain enumerations (eval/compare-v1/README.md,
"The draw").

    questions.py check ENUMERATION.jsonl.gz [...]        # the stop rule: identity-v1's labels drawn again, unchanged
    questions.py draw ENUMERATION.jsonl.gz [...] --out eval/compare-v1/questions.jsonl --strata eval/compare-v1/strata.json

Each enumeration is identity-v1's labeller output for one repository, with compare-v1's `in` field on each reference
row: the key of the innermost enclosing declaration, or null at file level. The draw reads nothing else, so the same
enumerations give the same questions.
"""
from __future__ import annotations

import argparse
from collections import defaultdict
import gzip
import hashlib
import json
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[3]
IDENTITY_LABELS = ROOT/'eval/identity-v1/labels.jsonl'

REFS_MAX = 200
REFS_QUOTA = 3
REFS_BUCKETS = (('1-4', 1, 4), ('5-29', 5, 29), ('30-200', 30, REFS_MAX))
IMPACT_QUOTA = 4
IMPACT_BUCKETS = (('1', 1, 1), ('2-9', 2, 9), ('10+', 10, None))
FIND_MAX = 20
FIND_QUOTA = 4
FIND_BUCKETS = (('1', 1, 1), ('2-4', 2, 4), ('5-20', 5, FIND_MAX))
# Reference kinds that make the enclosing declaration a caller; a `type` reference only names D.
CALLER_KINDS = ('call', 'member', 'dispatch', 'extension')
# Files whose references identity-v1 leaves out (sample.py's EXCLUDED); the same rule holds here.
EXCLUDED = {'autofac': ('test/Autofac.Test.CodeGen/',)}


def bucket(n, buckets):
    for name, low, high in buckets:
        if n >= low and (high is None or n <= high):
            return name
    return None


def order(repo, ident):
    return hashlib.sha256(f'compare-v1:{repo}:{ident}'.encode()).hexdigest()


class Repo:
    """One repository's enumeration, indexed for the draw and the labels."""

    def __init__(self, rows):
        self.decls = {}                       # key -> decl row (the first seen)
        self.refs = defaultdict(list)         # key -> inside reference rows bound to it
        self.names = defaultdict(set)         # name -> keys declared with it
        for r in rows:
            if r['t'] == 'decl':
                self.decls.setdefault(r['key'], r)
                self.names[r['name']].add(r['key'])
        for r in rows:
            if r['t'] == 'ref' and r.get('inside') and not r['file'].startswith(EXCLUDED.get(r['repo'], ())):
                self.refs[r['key']].append(r)
        self.repo = next((r['repo'] for r in rows), None)

    def kind(self, key):
        """The refs stratum kind: `dispatch` when D is reached through a conformance, else its declaration group."""
        if any(r['kind'] == 'dispatch' for r in self.refs[key]):
            return 'dispatch'
        return self.decls[key]['decl']

    def sites(self, key):
        return sorted({(r['file'], r['line']) for r in self.refs[key]})

    def callers(self, key):
        """Keys of the declarations holding a caller reference to `key`, without `key` itself."""
        return sorted({r['in'] for r in self.refs[key]
                       if r['kind'] in CALLER_KINDS and r.get('in') and r['in'] != key and r['in'] in self.decls})

    def impact(self, key):
        hop1 = self.callers(key)
        out = set(hop1)
        for c in hop1:
            out.update(self.callers(c))
        out.discard(key)
        return sorted(out)

    def place(self, key):
        d = self.decls[key]
        return [d['file'], d['line'], d['name']]


def draw(rows):
    """The questions drawn from one repository's rows, and per stratum (available, drawn)."""
    repo = Repo(rows)
    questions, strata = [], {}

    def take(qtype, pool, quota, make):
        by = defaultdict(list)
        for stratum, ident in pool:
            by[stratum].append(ident)
        for stratum in sorted(by):
            chosen = sorted(by[stratum], key=lambda i: order(repo.repo, i))[:quota]
            strata[f'{repo.repo}/{qtype}/{stratum}'] = [len(by[stratum]), len(chosen)]
            for ident in chosen:
                questions.append(make(stratum, ident))

    inside = [k for k in repo.decls if repo.refs[k]]
    refs_pool = [(f'{repo.kind(k)}/{bucket(len(repo.sites(k)), REFS_BUCKETS)}', k)
                 for k in inside if len(repo.sites(k)) <= REFS_MAX]
    strata[f'{repo.repo}/refs/over-{REFS_MAX}'] = [sum(1 for k in inside if len(repo.sites(k)) > REFS_MAX), 0]
    take('refs', refs_pool, REFS_QUOTA, lambda s, k: {
        'type': 'refs', 'stratum': s, 'key': k, 'target': repo.place(k), 'labels': repo.sites(k)})

    callables = [k for k, d in repo.decls.items() if d['decl'] == 'callable' and repo.callers(k)]
    impact_pool = [(bucket(len(repo.callers(k)), IMPACT_BUCKETS), k) for k in callables]

    def impact_question(stratum, key):
        # callers and impact share their targets (README); callers is asked from 0.8.
        return {'type': 'impact', 'stratum': stratum, 'key': key, 'target': repo.place(key),
                'callers': [repo.place(c) for c in repo.callers(key)],
                'labels': [repo.place(c) for c in repo.impact(key)]}
    take('impact', impact_pool, IMPACT_QUOTA, impact_question)

    find_pool = [(bucket(len(keys), FIND_BUCKETS), name) for name, keys in repo.names.items()
                 if len(keys) <= FIND_MAX]
    take('find', find_pool, FIND_QUOTA, lambda s, name: {
        'type': 'find', 'stratum': s, 'name': name,
        'labels': sorted(repo.place(k) for k in repo.names[name])})

    for i, q in enumerate(questions):
        q['repo'] = repo.repo
        q['id'] = f"{repo.repo}:{q['type']}:{q.get('key') or q['name']}"
    return questions, strata


def expand(questions):
    """The questions as asked: each impact question also stands as a callers question on the same target, with its
    hop-1 labels (README, "The draw": callers and impact share their targets)."""
    out = list(questions)
    for q in questions:
        if q['type'] == 'impact' and 'callers' in q:
            out.append({'type': 'callers', 'repo': q['repo'], 'stratum': q['stratum'], 'key': q['key'],
                        'target': q['target'], 'labels': q['callers'], 'id': q['id'].replace(':impact:', ':callers:', 1)})
    return out


def task_questions(tasks):
    """tasks-v1's tasks as compare-v1's locate and task questions: the task's text, its changed files and tests, at its
    parent commit. Each task is its own tree, so `repo` is the task's id."""
    out = []
    for t in tasks:
        for qtype in ('locate', 'task'):
            out.append({'id': f"{t['id']}:{qtype}", 'repo': t['id'], 'source_repo': t['repo'], 'lang': t['lang'],
                        'type': qtype, 'text': t['task'], 'parent': t['parent'], 'files': t['files'], 'tests': t['tests']})
    return out


def bound_lines(rows, name):
    """Per (file, line), the keys a reference row named `name` is bound to: the scorer's view of a refs answer."""
    out = defaultdict(set)
    for r in rows:
        if r['t'] == 'ref' and r['name'] == name:
            out[(r['file'], r['line'])].add(r['key'])
    return dict(out)


def read(path):
    with gzip.open(path, 'rt') as f:
        return [json.loads(line) for line in f if line.strip()]


def check(enumerations, committed=IDENTITY_LABELS):
    """identity-v1's labels drawn again from enumerations that carry `in`: (missing, changed, added) label ids against
    the committed set. compare-v1 uses the enumerations only when all three are empty."""
    sys.path.insert(0, str(ROOT/'scripts/eval/identity'))
    import sample
    lang = sample.languages()
    drawn = {}
    for path in enumerations:
        for label in sample.draw(read(path), lang)[0]:
            drawn[label['id']] = label
    want = {}
    for line in Path(committed).read_text().splitlines():
        if line.strip():
            label = json.loads(line)
            want[label['id']] = label
    repos = {d['repo'] for d in drawn.values()}
    want = {k: v for k, v in want.items() if v['repo'] in repos}
    missing = sorted(want.keys() - drawn.keys())
    changed = sorted(k for k in want.keys() & drawn.keys() if json.dumps(want[k], sort_keys=True) != json.dumps(drawn[k], sort_keys=True))
    added = sorted(drawn.keys() - want.keys())
    return missing, changed, added


def main(argv=None):
    argv = sys.argv[1:] if argv is None else argv
    if argv[:1] == ['tasks']:
        ap = argparse.ArgumentParser(prog='questions.py tasks')
        ap.add_argument('tasks', type=Path)
        ap.add_argument('--out', type=Path, required=True)
        args = ap.parse_args(argv[1:])
        tasks = [json.loads(l) for l in args.tasks.read_text().splitlines() if l.strip()]
        args.out.write_text(''.join(json.dumps(q, sort_keys=True) + '\n' for q in task_questions(tasks)))
        return 0
    if argv[:1] == ['check']:
        ap = argparse.ArgumentParser(prog='questions.py check')
        ap.add_argument('enumerations', nargs='+', type=Path)
        args = ap.parse_args(argv[1:])
        missing, changed, added = check(args.enumerations)
        for name, ids in (('missing', missing), ('changed', changed), ('added', added)):
            for i in ids[:50]:
                print(f'{name}: {i}')
        print(f'{len(missing)} missing, {len(changed)} changed, {len(added)} added')
        return 1 if missing or changed or added else 0
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('command', choices=['draw'])
    ap.add_argument('enumerations', nargs='+', type=Path)
    ap.add_argument('--out', type=Path, required=True)
    ap.add_argument('--strata', type=Path, required=True)
    args = ap.parse_args(argv)
    questions, strata = [], {}
    for path in sorted(args.enumerations):
        q, s = draw(read(path))
        questions += q
        strata.update(s)
    questions.sort(key=lambda q: q['id'])
    args.out.write_text(''.join(json.dumps(q, sort_keys=True) + '\n' for q in questions))
    args.strata.write_text(json.dumps(strata, indent=1, sort_keys=True) + '\n')
    print(f'{len(questions)} questions from {len(args.enumerations)} enumerations')


if __name__ == '__main__':
    sys.exit(main())
