#!/usr/bin/env python3
"""Identity gate: KnowCode's reference links against identity-v1's compiler labels, with no model calls.

    identity_gate.py fetch --work WORK [--repos a,b]
    identity_gate.py run --knowcode BIN --work WORK [--labels FILE] [--repos a,b] [--out SUMMARY] [--jobs N]
    identity_gate.py report SUMMARY [--wrong N]
    identity_gate.py check SUMMARY

`fetch` clones each repository of scripts/eval/identity/repos.txt at its pin into WORK/clones (a shallow fetch of the
pinned commit). Nothing is built and no repository code runs: labelling happens in a container
(scripts/eval/identity/run.sh), scoring only reads the clones and asks KnowCode.

`run` indexes each clone alone, with the build's own cache, config and data under WORK/kc/<binary hash>/<repo>, never
in a clone. Then, per repository and name in the labels, one `refs NAME --json --cite none` answer, and each label is
judged by the rule in eval/identity-v1/README.md:

- its row: the row of the same file, line and name at the label's column; failing that, the only row of that name on
  the line, when the name appears once on the line;
- a target matches when it is in a file of the label's and its name line (the target's line, else the first of the
  next NAME_WINDOW lines with the name as a word) is one of the label's lines.

The summary has, per language, repository and stratum: precision per confidence tag, the ambiguous rows' hit rate
and size, recall (linked, and among candidates), outside labels linked to an indexed declaration, and every wrong
`exact` row. `check` exits 1 unless, in every language, `exact` precision is at least EXACT_PRECISION and no outside
label is linked.
"""
from __future__ import annotations

import argparse
from collections import Counter, defaultdict
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
REPOS = HERE/'identity/repos.txt'
LABELS = ROOT/'eval/identity-v1/labels.jsonl'
EXACT_PRECISION = 0.99
NAME_WINDOW = 10
LINKS = ('exact', 'unique_name')
GIT_ENV = {'PATH': '/usr/bin:/bin:/opt/homebrew/bin:/usr/local/bin', 'LC_ALL': 'C', 'GIT_CONFIG_NOSYSTEM': '1',
           'GIT_CONFIG_GLOBAL': '/dev/null', 'GIT_TERMINAL_PROMPT': '0'}


def repos():
    out = {}
    for line in REPOS.read_text().splitlines():
        if line.strip() and not line.startswith('#'):
            name, lang, url, sha = line.split()
            out[name] = dict(lang=lang, url=url, sha=sha)
    return out


def jsonl(path):
    return [json.loads(l) for l in Path(path).read_text().splitlines() if l.strip()]


def selected(names):
    known = repos()
    if not names:
        return known
    want = names.split(',')
    unknown = sorted(set(want) - set(known))
    if unknown:
        raise SystemExit('not in repos.txt: ' + ', '.join(unknown))
    return {n: known[n] for n in want}


def fetch(work, names=None):
    clones = Path(work)/'clones'
    clones.mkdir(parents=True, exist_ok=True)
    for name, r in selected(names).items():
        target = clones/name
        git = lambda *a: subprocess.run(['git', '-c', 'core.hooksPath=/dev/null', *a], cwd=target, env=GIT_ENV,
                                        check=True, capture_output=True, text=True).stdout.strip()
        if not (target/'.git').is_dir():
            target.mkdir()
            git('init', '--quiet')
        if subprocess.run(['git', 'rev-parse', 'HEAD'], cwd=target, env=GIT_ENV, capture_output=True,
                          text=True).stdout.strip() != r['sha']:
            git('fetch', '--quiet', '--depth', '1', r['url'], r['sha'])
            git('-c', 'advice.detachedHead=false', 'checkout', '--quiet', 'FETCH_HEAD')
        print(f"{name} {git('rev-parse', 'HEAD')}")


# --- judging --------------------------------------------------------------------------------------------------------

def word(name):
    return re.compile(r'(?<![\w$])' + re.escape(name) + r'(?![\w$])')


def name_line(lines, line, name):
    """The line of a declaration's name: `line` when the name is on it, else the first of the next NAME_WINDOW lines
    that has it as a word (a declaration that starts at an attribute or a doc comment); `line` when none does."""
    pattern = word(name)
    for n in range(line, min(line + NAME_WINDOW, len(lines)) + 1):
        if 1 <= n <= len(lines) and pattern.search(lines[n - 1]):
            return n
    return line


def find_row(rows, label, line_text):
    """The KnowCode row for a label: same file, line and name at its column, else the only such row on the line when
    the name appears once on it; None when KnowCode has no reference there."""
    here = [r for r in rows if r.get('file') == label['file'] and r.get('line') == label['line']
            and r.get('name') == label['name']]
    for r in here:
        if r.get('col') == label['col']:
            return r
    if len(here) == 1 and len(word(label['name']).findall(line_text or '')) == 1:
        return here[0]
    return None


def target_matches(target, label, lines_of):
    if target.get('file') not in {f for f, _ in label['locs']}:
        return False
    lines = lines_of(target['file'])
    at = name_line(lines, int(target.get('line') or 0), label['name'])
    return [target['file'], at] in [list(l) for l in label['locs']]


def judge(label, row, lines_of):
    """One label's outcome: KnowCode's confidence and targets at that reference, and whether they are right."""
    out = dict(id=label['id'], lang=label['lang'], repo=label['repo'], kind=label['kind'], where=label['where'],
               bucket=label['bucket'])
    if row is None:
        return dict(out, confidence='none', targets=0, right=label['where'] == 'outside', among=False)
    confidence = row.get('confidence') or 'none'
    targets = [t for t in row.get('targets') or [] if t.get('file')]
    hits = [target_matches(t, label, lines_of) for t in targets] if label['where'] == 'inside' else [False] * len(targets)
    if targets:
        right = all(hits)
    else:
        right = label['where'] == 'outside'
    out.update(confidence=confidence, targets=len(targets), right=right, among=any(hits),
               got=[f"{t['file']}:{t.get('line')}" for t in targets[:5]])
    return out


# --- summary --------------------------------------------------------------------------------------------------------

def ratio(a, b):
    return round(a / b, 4) if b else None


def metrics(judged):
    """Precision per confidence tag, ambiguous rows, recall, and outside labels linked, over some judged labels."""
    by_tag = defaultdict(list)
    for j in judged:
        by_tag[j['confidence']].append(j)
    tags = {}
    for tag, rows in sorted(by_tag.items()):
        right = sum(j['right'] for j in rows)
        tags[tag] = dict(rows=len(rows), right=right, precision=ratio(right, len(rows)))
        if tag not in LINKS and any(j['targets'] for j in rows):
            with_targets = [j for j in rows if j['targets']]
            tags[tag].update(contains=ratio(sum(j['among'] for j in with_targets), len(with_targets)),
                             mean_candidates=round(sum(j['targets'] for j in with_targets) / len(with_targets), 2))
    inside = [j for j in judged if j['where'] == 'inside']
    outside = [j for j in judged if j['where'] == 'outside']
    linked = sum(1 for j in inside if j['confidence'] in LINKS and j['right'])
    among = sum(1 for j in inside if j['among'])
    return dict(
        labels=len(judged), inside=len(inside), outside=len(outside), tags=tags,
        exact_precision=tags.get('exact', {}).get('precision'),
        recall_linked=ratio(linked, len(inside)), recall_among=ratio(among, len(inside)),
        outside_linked=sum(1 for j in outside if j['confidence'] in LINKS and j['targets']),
        outside_candidates=sum(1 for j in outside if j['confidence'] not in LINKS and j['targets']))


def summarize(judged, labels):
    by_id = {l['id']: l for l in labels}
    groups = defaultdict(list)
    for j in judged:
        groups[('lang', j['lang'])].append(j)
        groups[('repo', j['repo'])].append(j)
        groups[('stratum', f"{j['lang']}/{j['kind']}/{j['where']}/{j['bucket']}")].append(j)
    out = dict(overall=metrics(judged), lang={}, repo={}, stratum={})
    for (level, key), rows in sorted(groups.items()):
        out[level][key] = metrics(rows)

    def listing(j):
        l = by_id[j['id']]
        want = 'outside (' + str(l['origin']) + ')' if l['where'] == 'outside' else \
            ', '.join(f'{f}:{n}' for f, n in l['locs'])
        return dict(at=f"{l['repo']}/{l['file']}:{l['line']}:{l['col']}", name=l['name'], kind=l['kind'],
                    bucket=l['bucket'], confidence=j['confidence'], knowcode=j.get('got', []), toolchain=want,
                    symbol=l['symbol'])
    out['wrong_exact'] = [listing(j) for j in judged if j['confidence'] == 'exact' and not j['right']]
    out['outside_linked'] = [listing(j) for j in judged
                             if j['where'] == 'outside' and j['confidence'] in LINKS and j['targets']]
    out['outside_candidates'] = [listing(j) for j in judged
                                 if j['where'] == 'outside' and j['confidence'] not in LINKS and j['targets']]
    return out


def verdict(summary):
    failures = []
    for lang, m in sorted(summary['lang'].items()):
        if m['exact_precision'] is not None and m['exact_precision'] < EXACT_PRECISION:
            failures.append(f"{lang}: exact precision {m['exact_precision']} < {EXACT_PRECISION}")
        if m['outside_linked']:
            failures.append(f"{lang}: {m['outside_linked']} outside labels linked to an indexed declaration")
    return failures


# --- run ------------------------------------------------------------------------------------------------------------

def binary_hash(binary):
    return hashlib.sha256(Path(binary).read_bytes()).hexdigest()[:16]


def kc_env(base):
    for d in ('home', 'cache', 'config', 'data'):
        (base/d).mkdir(parents=True, exist_ok=True)
    return dict(GIT_ENV, HOME=str(base/'home'), KNOWCODE_CACHE_DIR=str(base/'cache'),
                KNOWCODE_CONFIG_DIR=str(base/'config'), KNOWCODE_DATA_DIR=str(base/'data'), KNOWCODE_UPDATE_CHECK='0')


def refs(binary, clone, env, name):
    """Every row of `refs NAME` in the clone's repository, following a cut answer's cursor."""
    args = [str(binary), 'refs', name, '--json', '--cite', 'none', '--limit', '1000000', '--no-sync']
    rows = []
    while True:
        p = subprocess.run(args, cwd=clone, env=env, capture_output=True, text=True)
        if p.returncode != 0:
            return rows, (p.stderr.strip() or p.stdout.strip())[:300]
        answer = json.loads(p.stdout)
        rows += answer.get('items') or []
        cursor = answer.get('cursor')
        if not cursor or answer.get('complete', True):
            return rows, None
        args = [str(binary), 'more', cursor, '--json']


def run(args):
    labels = jsonl(args.labels)
    chosen = selected(args.repos)
    labels = [l for l in labels if l['repo'] in chosen]
    binary = Path(args.knowcode).resolve()
    work = Path(args.work).resolve()
    base = work/'kc'/binary_hash(binary)
    version = subprocess.run([str(binary), '--version'], capture_output=True, text=True).stdout.strip()
    judged, errors, timing = [], {}, {}
    for repo in chosen:
        mine = [l for l in labels if l['repo'] == repo]
        if not mine:
            continue
        clone = work/'clones'/repo
        if not (clone/'.git').is_dir():
            raise SystemExit(f'{clone}: missing; run `identity_gate.py fetch` first')
        env = kc_env(base/repo)
        started = time.monotonic()
        subprocess.run([str(binary), 'index', str(clone)], env=env, cwd=clone, check=True, capture_output=True)
        indexed = time.monotonic()
        names = sorted({l['name'] for l in mine})
        with ThreadPoolExecutor(max_workers=args.jobs) as pool:
            answers = dict(zip(names, pool.map(lambda n: refs(binary, clone, env, n), names)))
        texts = {}

        def lines_of(rel):
            if rel not in texts:
                try:
                    texts[rel] = (clone/rel).read_text(encoding='utf-8', errors='replace').split('\n')
                except OSError:
                    texts[rel] = []
            return texts[rel]
        for l in mine:
            rows, error = answers[l['name']]
            if error:
                errors[f"{repo}:{l['name']}"] = error
            lines = lines_of(l['file'])
            line_text = lines[l['line'] - 1] if 0 < l['line'] <= len(lines) else ''
            judged.append(judge(l, find_row(rows, l, line_text), lines_of))
        timing[repo] = dict(index_s=round(indexed - started, 1), refs_s=round(time.monotonic() - indexed, 1),
                            names=len(names))
        print(f'{repo}: {len(mine)} labels, {len(names)} names', file=sys.stderr)
    summary = dict(knowcode=version, binary=binary_hash(binary), labels=str(args.labels), timing=timing,
                   errors=errors, **summarize(judged, labels))
    summary['verdict'] = verdict(summary) or 'pass'
    text = json.dumps(summary, indent=1, sort_keys=True) + '\n'
    if args.out:
        Path(args.out).write_text(text)
        (Path(args.out).with_suffix('.judged.jsonl')).write_text(''.join(json.dumps(j, sort_keys=True) + '\n' for j in judged))
    print(report(summary, args.wrong))


def fmt(x):
    return '–' if x is None else f'{x:.3f}' if isinstance(x, float) else str(x)


def report(summary, wrong=50):
    out = [f"KnowCode {summary['knowcode']} ({summary['binary']}): verdict {summary['verdict']}", '',
           '| Language | Labels | Inside | `exact` rows | `exact` precision | `unique_name` precision | Recall, linked | '
           'Recall, among candidates | Outside linked | Outside as candidates |',
           '| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |']
    for lang, m in sorted(summary['lang'].items()):
        t = m['tags']
        out.append(f"| {lang} | {m['labels']} | {m['inside']} | {t.get('exact', {}).get('rows', 0)} | "
                   f"{fmt(m['exact_precision'])} | {fmt(t.get('unique_name', {}).get('precision'))} | "
                   f"{fmt(m['recall_linked'])} | {fmt(m['recall_among'])} | {m['outside_linked']} | "
                   f"{m['outside_candidates']} |")
    out += ['', '| Stratum | Labels | Tags (rows, precision) | Recall, linked | Outside linked |', '| --- | ---: | --- | ---: | ---: |']
    for key, m in sorted(summary['stratum'].items()):
        tags = ', '.join(f"{t} {v['rows']} ({fmt(v['precision'])})" for t, v in sorted(m['tags'].items()))
        out.append(f"| {key} | {m['labels']} | {tags} | {fmt(m['recall_linked'])} | {m['outside_linked']} |")
    if summary['wrong_exact']:
        out += ['', f"Wrong `exact` links ({len(summary['wrong_exact'])}):", '']
        for w in summary['wrong_exact'][:wrong]:
            out.append(f"- `{w['at']}` {w['name']} ({w['kind']}, {w['bucket']}): KnowCode {', '.join(w['knowcode'])}; "
                       f"toolchain {w['toolchain']}")
        if len(summary['wrong_exact']) > wrong:
            out.append(f"- … {len(summary['wrong_exact']) - wrong} more in the summary")
    return '\n'.join(out)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest='cmd', required=True)
    f = sub.add_parser('fetch')
    f.add_argument('--work', required=True)
    f.add_argument('--repos')
    r = sub.add_parser('run')
    r.add_argument('--knowcode', required=True)
    r.add_argument('--work', required=True)
    r.add_argument('--labels', default=str(LABELS))
    r.add_argument('--repos')
    r.add_argument('--out')
    r.add_argument('--jobs', type=int, default=4)
    r.add_argument('--wrong', type=int, default=50)
    p = sub.add_parser('report')
    p.add_argument('summary')
    p.add_argument('--wrong', type=int, default=50)
    c = sub.add_parser('check')
    c.add_argument('summary')
    args = ap.parse_args()
    if args.cmd == 'fetch':
        fetch(args.work, args.repos)
    elif args.cmd == 'run':
        run(args)
    elif args.cmd == 'report':
        print(report(json.loads(Path(args.summary).read_text()), args.wrong))
    else:
        failures = verdict(json.loads(Path(args.summary).read_text()))
        print('\n'.join(failures) or 'pass')
        sys.exit(1 if failures else 0)


if __name__ == '__main__':
    main()
