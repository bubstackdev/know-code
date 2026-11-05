#!/usr/bin/env python3
"""Builds routes-v1's labels from run.sh's listings, by the rule in eval/routes-v1/README.md.

    labels.py IO/out/routes-REPO.jsonl [...] --out eval/routes-v1/go/labels.jsonl --programs eval/routes-v1/go/programs.json

A label is a route one of a repository's programs lists: its repository, framework, method and path, once however
many programs list it, with the handlers and registration sites they give. `--programs` records, per repository, each
program run, whether it listed its routes, and the repository directories it compiles: scoring counts KnowCode's
routes only in the directories of programs that listed theirs.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path


def read(path):
    with open(path, encoding='utf-8') as f:
        return [json.loads(line) for line in f if line.strip()]


def build(rows):
    """The labels and program records of one or more listings' rows."""
    labels, programs = {}, {}
    for r in rows:
        if r['t'] == 'program':
            p = programs.setdefault(r['repo'], [])
            p.append(dict(module=r['module'], package=r['package'], status=r['status'],
                          dirs=sorted(d for d in r['dirs'].split() if d)))
        elif r['t'] == 'route' and r.get('framework') != 'unknown':
            key = (r['repo'], r['framework'], r['method'] or '*', r['path'])
            label = labels.setdefault(key, dict(repo=key[0], framework=key[1], method=key[2], path=key[3],
                                                handlers=set(), locs=set(), programs=set()))
            label['handlers'].add(r['handler'])
            if r.get('loc'):
                label['locs'].add(r['loc'])
            label['programs'].add(f"{r['module']}:{r['package']}")
    out = []
    for key in sorted(labels):
        label = labels[key]
        out.append(dict(label, handlers=sorted(label['handlers']), locs=sorted(label['locs']),
                        programs=sorted(label['programs'])))
    return out, {repo: sorted(ps, key=lambda p: (p['module'], p['package'])) for repo, ps in sorted(programs.items())}


def counts(labels):
    """Labels per framework and per repository within it."""
    by = {}
    for label in labels:
        f = by.setdefault(label['framework'], {})
        f[label['repo']] = f.get(label['repo'], 0) + 1
    return by


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('listings', nargs='+')
    ap.add_argument('--out', required=True)
    ap.add_argument('--programs', required=True)
    args = ap.parse_args()
    rows = [r for path in args.listings for r in read(path)]
    labels, programs = build(rows)
    Path(args.out).write_text(''.join(json.dumps(label, sort_keys=True) + '\n' for label in labels))
    Path(args.programs).write_text(json.dumps(programs, indent=1, sort_keys=True) + '\n')
    for framework, repos in sorted(counts(labels).items()):
        print(f"{framework}: {sum(repos.values())} ({', '.join(f'{r} {n}' for r, n in sorted(repos.items()))})")


if __name__ == '__main__':
    main()
