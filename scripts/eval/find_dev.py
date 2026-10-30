#!/usr/bin/env python3
"""find-dev-v1: find's precision on a development set, and what each rule for non-exact rows would do
(eval/find-dev-v1/README.md). No model calls.

    find_dev.py draw ENUMERATION.jsonl.gz [...] --out eval/find-dev-v1/questions.jsonl
    find_dev.py run --knowcode BIN --clones DIR --work WORK --questions FILE --out ANSWERS [--page 10]
    find_dev.py score --questions FILE --answers ANSWERS --enumerations DIR --clones DIR [--out REPORT]

`draw` takes compare-v1's find draw (scripts/eval/compare/questions.py) with this set's seed and quota, and adds each
name's misspelled and half-remembered forms. `run` asks `BIN find QUERY --json -n 200` per question in each
repository's pinned clone, with HOME, config, data and cache all the run's own; with `--page N`, it asks with `-n N`
and keeps the build's own page and `similar` lines, scored as option `built`. `score` judges each option's rows
with compare-v1's judge (score.judge_decls) and prints the counts.
"""
from __future__ import annotations

import argparse
from collections import defaultdict
import gzip
import hashlib
import json
import math
import os
from pathlib import Path
import subprocess
import sys

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE / 'compare'))
import questions as compare_questions  # noqa: E402
import score as compare_score  # noqa: E402

SEED = 'find-dev-v1'
QUOTA = 8
PAGE = 10
SIMILAR = 5
OPTIONS = ('current', 'exact-only', 'similar-apart', 'cursor')


def forms(name):
    """The forms a name is asked in: itself, and those a caller who doesn't remember it exactly might type."""
    out = [('exact', name)]
    if name.lower() != name:
        out.append(('lower', name.lower()))
    cut = max(3, math.ceil(len(name) * 0.6))
    if cut < len(name):
        out.append(('prefix', name[:cut]))
    if len(name) >= 4 and name[1] != name[2]:
        out.append(('typo', name[0] + name[2] + name[1] + name[3:]))
    return out


def order(repo, name):
    return hashlib.sha256(f'{SEED}:{repo}:{name}'.encode()).hexdigest()


def draw(rows):
    """One repository's find questions: per stratum of how many declarations share a name, the first QUOTA names in
    this set's order, each in every form."""
    repo = compare_questions.Repo(rows)
    pool = defaultdict(list)
    for name, keys in repo.names.items():
        if len(keys) <= compare_questions.FIND_MAX:
            pool[compare_questions.bucket(len(keys), compare_questions.FIND_BUCKETS)].append(name)
    out = []
    for stratum in sorted(pool):
        for name in sorted(pool[stratum], key=lambda n: order(repo.repo, n))[:QUOTA]:
            labels = sorted(repo.place(k) for k in repo.names[name])
            for form, query in forms(name):
                out.append({'id': f'{repo.repo}:{name}:{form}', 'repo': repo.repo, 'stratum': stratum, 'name': name,
                            'form': form, 'query': query, 'labels': labels})
    return out


def read_rows(path):
    with gzip.open(path, 'rt') as f:
        return [json.loads(line) for line in f if line.strip()]


def options(rows, query):
    """Each option's first page and `similar` list, from find's rows (each with `name` and `matched`)."""
    exact = [r for r in rows if r.get('name') == query]
    others = [r for r in rows if r.get('name') != query]
    current = rows[:PAGE]
    if not exact:
        return {o: (current, []) for o in OPTIONS}
    first = exact[:PAGE]
    return {'current': (current, []), 'exact-only': (first, []), 'similar-apart': (first, others[:SIMILAR]),
            'cursor': (first, [])}


def similar_row(line):
    """A `similar` line (`kind name  file:line`) as a row."""
    file, _, at = line.rsplit('  ', 1)[-1].rpartition(':')
    return {'file': file, 'line': int(at)}


def run(knowcode, clones, work, questions, out, page=None):
    work = Path(work).resolve()
    dirs = {k: work / k for k in ('home', 'config', 'data', 'cache')}
    for d in dirs.values():
        d.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, KNOWCODE_UPDATE_CHECK='0', HOME=str(dirs['home']), KNOWCODE_CONFIG_DIR=str(dirs['config']),
               KNOWCODE_DATA_DIR=str(dirs['data']), KNOWCODE_CACHE_DIR=str(dirs['cache']))
    with open(out, 'w') as f:
        for q in questions:
            p = subprocess.run([knowcode, 'find', q['query'], '--json', '-n', str(page or 200)], cwd=Path(clones) / q['repo'],
                               env=env, capture_output=True, text=True)
            rows, similar = [], []
            if p.returncode == 0:
                data = json.loads(p.stdout)
                rows = [{'file': i.get('file'), 'line': i.get('line'), 'name': i.get('name'), 'matched': i.get('matched')}
                        for i in data.get('items', [])]
                similar = [similar_row(line) for line in data.get('similar', [])]
            a = {'id': q['id'], 'ok': p.returncode == 0, 'rows': rows, 'error': p.stderr.strip()[-300:] if p.returncode else None}
            if page:
                a.update(page=page, similar=similar)
            f.write(json.dumps(a) + '\n')


def score(questions, answers, enumerations, clones):
    decls = {}
    for path in Path(enumerations).glob('*/out/enumeration-*.jsonl.gz'):
        rows = read_rows(path)
        repo = compare_questions.Repo(rows)
        decls[repo.repo] = [tuple(repo.place(k)) for k in repo.decls]
    cache = {}

    def source(repo):
        def read(f):
            key = (repo, f)
            if key not in cache:
                try:
                    cache[key] = (Path(clones) / repo / f).read_text(errors='replace').splitlines()
                except OSError:
                    cache[key] = None
            return cache[key]
        return read

    by_id = {a['id']: a for a in answers}
    totals = defaultdict(lambda: defaultdict(int))
    for q in questions:
        a = by_id.get(q['id'])
        if not a or not a['ok']:
            totals[('error', q['form'])]['questions'] += 1
            continue
        labels = {tuple(lab) for lab in q['labels']}
        built = {'built': (a['rows'], a['similar'])} if a.get('page') else None
        for option, (page, similar) in (built or options(a['rows'], q['query'])).items():
            t = totals[(option, q['form'])]
            t['questions'] += 1
            j = compare_score.judge_decls([('decl', r['file'], r['line']) for r in page], labels, decls[q['repo']], source(q['repo']))
            t['right'] += len(j.right)
            t['wrong'] += len(j.wrong)
            t['unjudged'] += len(j.unjudged)
            t['labels'] += len(labels)
            t['hits'] += len(j.hits)
            t['found'] += bool(j.hits)
            t['rows'] += len(page) + len(similar)
            t['bytes'] += len(json.dumps(page)) + len(json.dumps(similar))
            if similar:
                js = compare_score.judge_decls([('decl', r['file'], r['line']) for r in page + similar], labels,
                                               decls[q['repo']], source(q['repo']))
                t['hits_with_similar'] += len(js.hits)
                t['found_with_similar'] += bool(js.hits)
            else:
                t['hits_with_similar'] += len(j.hits)
                t['found_with_similar'] += bool(j.hits)
    return totals


def report(totals):
    lines = ['| Option | Form | Questions | Precision | Recall | Found | Recall with similar | Found with similar | Rows | Bytes |',
             '| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |']
    for (option, form), t in sorted(totals.items()):
        if option == 'error':
            lines.append(f'| error | {form} | {t["questions"]} | | | | | | | |')
            continue
        judged = t['right'] + t['wrong']
        p = f'{t["right"] / judged:.3f} ({t["right"]}/{judged})' if judged else 'n/a'
        r = f'{t["hits"] / t["labels"]:.3f} ({t["hits"]}/{t["labels"]})' if t['labels'] else 'n/a'
        rs = f'{t["hits_with_similar"] / t["labels"]:.3f}' if t['labels'] else 'n/a'
        lines.append(f'| {option} | {form} | {t["questions"]} | {p} | {r} | {t["found"]} | {rs} | {t["found_with_similar"]} '
                     f'| {t["rows"]} | {t["bytes"]} |')
    return '\n'.join(lines) + '\n'


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest='cmd', required=True)
    d = sub.add_parser('draw')
    d.add_argument('enumerations', nargs='+')
    d.add_argument('--out', required=True)
    r = sub.add_parser('run')
    r.add_argument('--knowcode', required=True)
    r.add_argument('--clones', required=True)
    r.add_argument('--work', required=True)
    r.add_argument('--questions', required=True)
    r.add_argument('--out', required=True)
    r.add_argument('--page', type=int, help="ask with -n PAGE and keep the build's own page and similar lines")
    s = sub.add_parser('score')
    s.add_argument('--questions', required=True)
    s.add_argument('--answers', required=True)
    s.add_argument('--enumerations', required=True)
    s.add_argument('--clones', required=True)
    s.add_argument('--out')
    args = ap.parse_args(argv)
    if args.cmd == 'draw':
        qs = [q for e in sorted(args.enumerations) for q in draw(read_rows(e))]
        Path(args.out).write_text(''.join(json.dumps(q, sort_keys=True) + '\n' for q in qs))
        print(f'{len(qs)} questions, {len({(q["repo"], q["name"]) for q in qs})} names')
        return 0
    qs = [json.loads(line) for line in Path(args.questions).read_text().splitlines() if line.strip()]
    if args.cmd == 'run':
        run(args.knowcode, args.clones, args.work, qs, args.out, args.page)
        return 0
    answers = [json.loads(line) for line in Path(args.answers).read_text().splitlines() if line.strip()]
    text = report(score(qs, answers, args.enumerations, args.clones))
    if args.out:
        Path(args.out).write_text(text)
    print(text)
    return 0


if __name__ == '__main__':
    sys.exit(main())
