#!/usr/bin/env python3
"""Draws impact-dev-v1: impact questions for tuning KnowCode's impact walk, kept apart from compare-v1's gate.

    draw.py ENUMERATION.jsonl.gz [...] --out eval/impact-dev-v1/questions.jsonl --strata eval/impact-dev-v1/strata.json

compare-v1's impact rules (scripts/eval/compare/questions.py) with two changes: targets are taken in SHA-256 order of
`SEED:repo:key` rather than compare-v1's, and no declaration that is a target of any compare-v1 question is drawn.
The draw reads only the enumerations and compare-v1's questions.jsonl, so the same inputs give the same questions.
"""
from __future__ import annotations

import argparse
from collections import defaultdict
import hashlib
import json
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT/'scripts/eval/compare'))
import questions as compare  # noqa: E402

SEED = 'impact-dev-v1'
COMPARE = ROOT/'eval/compare-v1/questions.jsonl'


def draw(rows, excluded):
    repo = compare.Repo(rows)
    pool = defaultdict(list)
    for k, d in repo.decls.items():
        if d['decl'] == 'callable' and k not in excluded.get(repo.repo, ()) and repo.callers(k):
            pool[compare.bucket(len(repo.callers(k)), compare.IMPACT_BUCKETS)].append(k)
    questions, strata = [], {}
    for stratum in sorted(pool):
        order = lambda k: hashlib.sha256(f'{SEED}:{repo.repo}:{k}'.encode()).hexdigest()  # noqa: E731
        chosen = sorted(pool[stratum], key=order)[:compare.IMPACT_QUOTA]
        strata[f'{repo.repo}/impact/{stratum}'] = [len(pool[stratum]), len(chosen)]
        for k in chosen:
            questions.append({'type': 'impact', 'stratum': stratum, 'key': k, 'target': repo.place(k),
                              'callers': [repo.place(c) for c in repo.callers(k)],
                              'labels': [repo.place(c) for c in repo.impact(k)],
                              'repo': repo.repo, 'id': f'{repo.repo}:impact:{k}'})
    return questions, strata


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('enumerations', nargs='+')
    ap.add_argument('--out', required=True)
    ap.add_argument('--strata', required=True)
    a = ap.parse_args()
    excluded = defaultdict(set)
    for line in COMPARE.read_text().splitlines():
        q = json.loads(line)
        if q.get('key'):
            excluded[q['repo']].add(q['key'])
    out, strata = [], {}
    for path in sorted(a.enumerations):
        q, s = draw(compare.read(path), excluded)
        out += q
        strata.update(s)
    Path(a.out).write_text(''.join(json.dumps(q, sort_keys=True) + '\n' for q in out))
    Path(a.strata).write_text(json.dumps(strata, indent=1, sort_keys=True) + '\n')
    print(f'{len(out)} questions; {sum(len(v) for v in excluded.values())} compare-v1 targets excluded', file=sys.stderr)


if __name__ == '__main__':
    main()
