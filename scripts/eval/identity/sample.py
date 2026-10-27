#!/usr/bin/env python3
"""Draws identity-v1's labels from the labellers' enumerations, by the rule in eval/identity-v1/README.md.

    sample.py ENUMERATION.jsonl.gz [...] --out eval/identity-v1/labels.jsonl [--strata strata.json]

Each enumeration (run.sh's output, one per repository) lists every reference the toolchain binds in the repository's
own source and every declaration it saw there. The draw depends on the enumerations alone: strata of kind × bucket per
repository, each filled in SHA-256 order of `repo:file:line:col`, at most SPREAD references per target. `--strata`
writes, per stratum, how many references there were and how many were drawn.
"""
from __future__ import annotations

import argparse
from collections import Counter, defaultdict
import gzip
import hashlib
import json
from pathlib import Path

HERE = Path(__file__).resolve().parent
QUOTA = {('inside', '1'): 20, ('inside', '2-4'): 40, ('inside', '5+'): 40, ('outside', '0'): 10, ('outside', '1+'): 40}
SPREAD = 3
KINDS = ('extension', 'dispatch', 'call', 'member', 'type')
# Files whose references aren't drawn: a project the labeller loaded without its framework references, so its
# references to the base class library were never bound (eval/identity-v1/README.md).
EXCLUDED = {'autofac': ('test/Autofac.Test.CodeGen/',)}


def languages():
    out = {}
    for line in (HERE/'repos.txt').read_text().splitlines():
        if line.strip() and not line.startswith('#'):
            name, lang = line.split()[:2]
            out[name] = lang
    return out


def bucket(inside, count):
    if inside:
        return '1' if count <= 1 else '2-4' if count <= 4 else '5+'
    return '0' if count == 0 else '1+'


def order(row):
    return hashlib.sha256(f"{row['repo']}:{row['file']}:{row['line']}:{row['col']}".encode()).hexdigest()


def draw(rows, lang=None):
    """The labels drawn from one repository's enumeration rows, and per stratum (available, drawn)."""
    lang = lang or {}
    names = defaultdict(set)
    for r in rows:
        if r['t'] == 'decl':
            names[r['name']].add(r['key'])
    sites = defaultdict(list)
    for r in rows:
        if r['t'] == 'ref' and not r['file'].startswith(EXCLUDED.get(r['repo'], ())):
            sites[(r['repo'], r['file'], r['line'], r['col'])].append(r)
    strata = defaultdict(list)
    for site, found in sites.items():
        # One row per reference; a site the toolchain binds two ways is left out.
        if len({r['key'] for r in found}) != 1:
            continue
        r = found[0]
        count = len(names.get(r['name'], ()))
        b = bucket(r['inside'], count)
        strata[(r['repo'], r['kind'], 'inside' if r['inside'] else 'outside', b)].append(dict(r, multiplicity=count, bucket=b))
    labels, stats = [], {}
    for key in sorted(strata, key=lambda k: (k[0], KINDS.index(k[1]), k[2], k[3])):
        repo, kind, where, b = key
        quota = QUOTA[(where, b)]
        taken, per_target = [], Counter()
        for r in sorted(strata[key], key=order):
            if len(taken) == quota:
                break
            if per_target[r['key']] == SPREAD:
                continue
            per_target[r['key']] += 1
            taken.append(r)
        stats['/'.join(key)] = dict(available=len(strata[key]), drawn=len(taken))
        for r in sorted(taken, key=lambda r: (r['file'], r['line'], r['col'])):
            labels.append(dict(
                id=f"{r['repo']}:{r['file']}:{r['line']}:{r['col']}", repo=r['repo'], lang=lang.get(r['repo'], ''),
                file=r['file'], line=r['line'], col=r['col'], name=r['name'], kind=kind, where=where, bucket=b,
                multiplicity=r['multiplicity'], locs=r['locs'], origin=r['origin'], symbol=r['symbol']))
    return labels, stats


def read(path):
    opener = gzip.open if str(path).endswith('.gz') else open
    with opener(path, 'rt', encoding='utf-8') as f:
        return [json.loads(line) for line in f if line.strip()]


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('enumerations', nargs='+')
    ap.add_argument('--out', required=True)
    ap.add_argument('--strata')
    args = ap.parse_args()
    lang = languages()
    labels, stats = [], {}
    for path in args.enumerations:
        got, s = draw(read(path), lang)
        labels += got
        stats.update(s)
    labels.sort(key=lambda r: (r['lang'], r['repo'], r['file'], r['line'], r['col']))
    Path(args.out).write_text(''.join(json.dumps(r, sort_keys=True) + '\n' for r in labels))
    if args.strata:
        Path(args.strata).write_text(json.dumps(stats, indent=1, sort_keys=True) + '\n')
    print(f'{len(labels)} labels from {len(args.enumerations)} enumerations')


if __name__ == '__main__':
    main()
