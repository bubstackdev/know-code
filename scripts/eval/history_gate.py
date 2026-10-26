#!/usr/bin/env python3
"""History gate: KnowCode against plain git on history-v1, with labels from git alone and no model calls.

    history_gate.py fetch --work WORK [--repos a,b]
    history_gate.py run --knowcode BIN --work WORK [--repos a,b] [--out SUMMARY]

`fetch` clones each repository history-v1 asks about from its public URL (public-v2's manifest) with its full
history, into WORK/clones, and checks out its pin. About 250 MB for all four; serilog and serilog-extensions-hosting
alone are about 75 MB.

`run` recomputes every label with history_labels.py and refuses to go on if one differs from
eval/history-v1/labels.jsonl (a different git can move `git log -L`). Then, per question, it asks KnowCode the one
call that answers it and runs the plain-git approaches an agent with grep, find, read and git reaches for, and scores
each against the label:

| Family | KnowCode | Plain git |
| --- | --- | --- |
| symbol-commits, symbol-last | `history FILE:LINE` | `log -L :NAME:FILE`, `log -S NAME -- FILE`, `log --follow -- FILE` |
| symbol-blame | `history FILE:LINE` (`writers`) | `blame -w -L :NAME: FILE`, `blame -w FILE` |
| symbol-cochange | `history FILE:LINE` (`changed_with`) | the files of each approach's commits above |
| callers-history | `impact FILE:LINE` hop 1, in `history`'s commits | `git grep -lw NAME` files, in each approach's commits |
| file-cochange | `coupled FILE --since all` | `log --no-merges -- FILE` and `log --follow --no-merges -- FILE`, counted |

A set is scored by recall and precision; a ranked list (co-change) at the label's size; the last change by whether
the newest commit named is the label's; blame by the lines attributed to the right authors.

The verdict (exit 1 when it fails): over the method-level and caller questions, KnowCode must beat each plain approach:
recall and precision at least equal, one of them higher (a whole file's log has full recall and is beaten on precision).
On the file co-change controls, its recall must be within CONTROL_TOLERANCE of the best plain approach's. KnowCode runs as the CLI (no time budget), with its cache, config and
data in WORK, never in a clone.
"""
from __future__ import annotations

import argparse
import json
import os
from collections import Counter
from pathlib import Path
import re
import subprocess
import sys

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
import history_labels  # noqa: E402

QUESTIONS = ROOT/'eval/history-v1/questions.jsonl'
LABELS = ROOT/'eval/history-v1/labels.jsonl'
MANIFEST = ROOT/'eval/public-v2/manifest.json'
METHOD_FAMILIES = ('symbol-commits', 'symbol-last', 'symbol-blame', 'symbol-cochange', 'callers-history')
CONTROL_TOLERANCE = 0.05
GIT_ENV = {'PATH': '/usr/bin:/bin:/opt/homebrew/bin:/usr/local/bin', 'LC_ALL': 'C', 'GIT_CONFIG_NOSYSTEM': '1',
           'GIT_CONFIG_GLOBAL': '/dev/null', 'GIT_TERMINAL_PROMPT': '0'}


def jsonl(path):
    return [json.loads(l) for l in Path(path).read_text().splitlines() if l.strip()]


def git(repo, *args):
    out = subprocess.run(['git', '-C', str(repo), *args], capture_output=True, env=GIT_ENV)
    return out.stdout.decode('utf-8', 'replace'), out.returncode


def pins():
    """Repository name to (url, pin), from public-v2's manifest."""
    m = json.loads(MANIFEST.read_text())
    return {r['name']: (r['url'], r['commit']) for f in m['families'] for r in f['repos']}


def wanted(questions, only):
    names = sorted({q['repo'] for q in questions})
    return [n for n in names if only is None or n in only]


def fetch(work, only):
    known = pins()
    for name in wanted(jsonl(QUESTIONS), only):
        url, pin = known[name]
        target = Path(work)/'clones'/name
        if not (target/'.git').is_dir():
            target.parent.mkdir(parents=True, exist_ok=True)
            subprocess.run(['git', 'clone', '--quiet', '--no-checkout', url, str(target)], env=GIT_ENV, check=True)
        subprocess.run(['git', '-C', str(target), 'checkout', '--quiet', '--detach', pin], env=GIT_ENV, check=True)
        print(name, pin[:12], git(target, 'rev-list', '--count', 'HEAD')[0].strip(), 'commits')


# --- scoring ----------------------------------------------------------------------------------------------------------

def score_set(got, truth):
    got, truth = list(dict.fromkeys(got)), set(truth)
    hit = len(set(got) & truth)
    return {'hits': hit, 'truth': len(truth), 'listed': len(got)}


def score_ranked(got, truth):
    """At the label's size: the first len(truth) of `got`."""
    k = len(truth)
    return score_set(list(dict.fromkeys(got))[:k], truth)


def score_lines(got, truth):
    """Lines by author: a line counts when the answer gives it to the label's author."""
    hit = sum(min(n, got.get(a, 0)) for a, n in truth.items())
    return {'hits': hit, 'truth': sum(truth.values()), 'listed': sum(got.values())}


# --- plain git --------------------------------------------------------------------------------------------------------

def log_shas(repo, *args):
    out, _ = git(repo, 'log', '--format=@@%H', '-s', *args)
    return [l[2:] for l in out.splitlines() if l.startswith('@@')]


def plain_commits(repo, q):
    s, since = q['subject'], since_args(q)
    return {'log -L :name:': log_shas(repo, f"-L:{s['name']}:{s['file']}", *since),
            'log -S name': log_shas(repo, '-S', s['name'], *since, '--', s['file']),
            'log --follow': log_shas(repo, '--follow', *since, '--', s['file'])}


def since_args(q):
    since = q.get('params', {}).get('since')
    return [f'--since={since}'] if since else []


def blame_authors(repo, *args):
    out, _ = git(repo, 'blame', '-w', '--line-porcelain', *args)
    return Counter(l[7:] for l in out.splitlines() if l.startswith('author '))


def files_in(repo, shas, own, now):
    counts = Counter()
    for sha in shas:
        files = history_labels.files_of(repo, sha)
        if files is None:
            continue
        for f in set(files) - {own}:
            if f in now:
                counts[f] += 1
    return counts


def ranked(counts, minimum=1):
    return [f for f, n in sorted(counts.items(), key=lambda kv: (-kv[1], kv[0])) if n >= minimum]


# --- KnowCode ---------------------------------------------------------------------------------------------------------

def knowcode(binary, env, repo, *args):
    out = subprocess.run([str(binary), *args, '--json'], cwd=repo, capture_output=True, env=env)
    try:
        return json.loads(out.stdout)
    except json.JSONDecodeError:
        return {'items': [], 'error': out.stderr.decode('utf-8', 'replace')[-500:]}


def kc_history(binary, env, repo, q):
    s = q['subject']
    args = ['history', f"{s['file']}:{s['line']}", '-n', '1000']
    since = q.get('params', {}).get('since')
    if since:
        args += ['--since', since]
    env_json = knowcode(binary, env, repo, *args)
    return (env_json.get('items') or [{}])[0]


# --- one question -----------------------------------------------------------------------------------------------------

def answers(q, lab, repo, binary, env):
    """Per approach (KnowCode first), the score of its answer to `q`."""
    fam, s = q['family'], q['subject']
    out = {}
    now = set(git(repo, 'ls-files')[0].splitlines())
    minimum = q.get('params', {}).get('min_shared', 2)
    if fam == 'file-cochange':
        truth = [i['file'] for i in lab['items']]
        tests = q['params'].get('tests')
        keep = (lambda f: history_labels.TEST_PATH.search(f)) if tests else (lambda f: True)
        env_json = knowcode(binary, env, repo, 'coupled', s['file'], '--since', 'all', '-n', '1000')
        partners = [p['file'] for p in (env_json.get('items') or [{}])[0].get('partners', []) if not tests or p.get('test')]
        out['knowcode'] = score_ranked([f for f in partners if keep(f)], truth)
        for name, args in (('log', ['--no-merges']), ('log --follow', ['--follow', '--no-merges'])):
            shas = log_shas(repo, *args, '--', s['file'])
            out[name] = score_ranked([f for f in ranked(files_in(repo, shas, s['file'], now)) if keep(f)], truth)
        return out
    item = kc_history(binary, env, repo, q)
    kc_shas = [c['sha'] for c in item.get('commits', [])]
    plain = plain_commits(repo, q)
    # The KnowCode answer's commits, by full id.
    full = [git(repo, 'rev-parse', sha)[0].strip() for sha in kc_shas] if fam == 'callers-history' else []
    if fam == 'symbol-commits':
        truth = [i['sha'][:12] for i in lab['items']]
        out['knowcode'] = score_set(kc_shas, truth)
        for name, shas in plain.items():
            out[name] = score_set([x[:12] for x in shas], truth)
    elif fam == 'symbol-last':
        truth = lab['items'][0]['sha'][:12]
        newest = lambda shas: shas[:1]
        out['knowcode'] = score_set(newest(kc_shas), [truth])
        for name, shas in plain.items():
            out[name] = score_set([x[:12] for x in newest(shas)], [truth])
    elif fam == 'symbol-blame':
        truth = {i['author']: i['lines'] for i in lab['items']}
        out['knowcode'] = score_lines({w['author']: w['lines'] for w in item.get('writers', [])}, truth)
        out['blame -L :name:'] = score_lines(blame_authors(repo, f"-L:{s['name']}", '--', s['file']), truth)
        out['blame file'] = score_lines(blame_authors(repo, '--', s['file']), truth)
    elif fam == 'symbol-cochange':
        truth = [i['file'] for i in lab['items']]
        cw = [c['file'] for c in item.get('changed_with', []) if c['shared'] >= minimum]
        out['knowcode'] = score_set(cw, truth)
        for name, shas in plain.items():
            out[name] = score_set(ranked(files_in(repo, shas, s['file'], now), minimum), truth)
    elif fam == 'callers-history':
        truth = [i['file'] for i in lab['items']]
        # The label's callers are production code (public-v2's), as the question says: tests are left out on each side.
        if 'callers_changed_with' in item:
            # One call: `history` lists the direct callers changed in the symbol's commits.
            found = {c['file'] for c in item['callers_changed_with'] if not c.get('test')}
        else:
            # A build without it: `impact`'s first hop, in `history`'s commits (two calls).
            impact = knowcode(binary, env, repo, 'impact', f"{s['file']}:{s['line']}", '-n', '1000')
            callers = {a['file'] for a in (impact.get('items') or [{}])[0].get('affected', [])
                       if a.get('hop') == 1 and a.get('class') != 'test'}
            changed = files_in(repo, full, s['file'], now)
            found = {f for f in callers if f in changed}
        out['knowcode'] = score_set(sorted(found), truth)
        grep, _ = git(repo, 'grep', '-lw', s['name'])
        candidates = {f for f in grep.splitlines() if f != s['file'] and not history_labels.TEST_PATH.search(f)}
        for name, shas in plain.items():
            changed = files_in(repo, shas, s['file'], now)
            out[name] = score_set(sorted(f for f in candidates if f in changed), truth)
    return out


# --- run --------------------------------------------------------------------------------------------------------------

def totals(rows, approach):
    hits = sum(r['scores'][approach]['hits'] for r in rows if approach in r['scores'])
    truth = sum(r['scores'][approach]['truth'] for r in rows if approach in r['scores'])
    listed = sum(r['scores'][approach]['listed'] for r in rows if approach in r['scores'])
    return {'recall': round(hits / truth, 3) if truth else None, 'precision': round(hits / listed, 3) if listed else None,
            'hits': hits, 'truth': truth, 'listed': listed}


def verdict(rows):
    """Failures of the gate, as readable lines."""
    fails = []
    method = [r for r in rows if r['family'] in METHOD_FAMILIES]
    if method:
        for approach in sorted({a for r in method for a in r['scores'] if a != 'knowcode'}):
            p = totals([r for r in method if approach in r['scores']], approach)
            k = totals([r for r in method if approach in r['scores']], 'knowcode')
            kr, kp, pr, pp = k['recall'] or 0, k['precision'] or 0, p['recall'] or 0, p['precision'] or 0
            # Better: at least as good on both, and strictly on one. An approach listing everything (a whole
            # file's log or blame) has full recall, which no answer can exceed; it is beaten on precision.
            if not (kr >= pr and kp >= pp and (kr > pr or kp > pp)):
                fails.append(f"method-level: knowcode recall {k['recall']} precision {k['precision']} vs {approach} "
                             f"recall {p['recall']} precision {p['precision']}")
    control = [r for r in rows if r['family'] == 'file-cochange']
    for r in control:
        kc = r['scores']['knowcode']
        best = max(s['hits'] / s['truth'] for a, s in r['scores'].items() if a != 'knowcode' and s['truth'])
        mine = kc['hits'] / kc['truth'] if kc['truth'] else 0
        if mine < best - CONTROL_TOLERANCE:
            fails.append(f"{r['id']}: knowcode recall {mine:.3f} vs best plain {best:.3f}")
    return fails


def run(a):
    only = set(a.repos.split(',')) if a.repos else None
    work = Path(a.work)
    clones = work/'clones'
    questions = [q for q in jsonl(QUESTIONS) if only is None or q['repo'] in only]
    published = {l['id']: l for l in jsonl(LABELS)}
    labels = {}
    for q in questions:
        lab = history_labels.label(q, clones)
        if json.dumps(lab, sort_keys=True) != json.dumps(published[q['id']], sort_keys=True):
            raise SystemExit(f"{q['id']}: the label recomputed here differs from {LABELS.relative_to(ROOT)}; "
                             'check the clone is at its pin and the git version')
        labels[q['id']] = lab
    base = work/'knowcode'
    env = dict(os.environ, HOME=str(base/'home'), KNOWCODE_CACHE_DIR=str(base/'cache'),
               KNOWCODE_CONFIG_DIR=str(base/'config'), KNOWCODE_DATA_DIR=str(base/'data'), KNOWCODE_UPDATE_CHECK='0')
    (base/'home').mkdir(parents=True, exist_ok=True)
    binary = Path(a.knowcode).resolve()
    subprocess.run([str(binary), 'index', *sorted(str(clones/n) for n in {q['repo'] for q in questions})], env=env,
                   check=True, capture_output=True)
    rows = []
    for q in questions:
        scores = answers(q, labels[q['id']], clones/q['repo'], binary, env)
        rows.append({'id': q['id'], 'family': q['family'], 'scores': scores})
    version = subprocess.run([str(binary), '--version'], capture_output=True, text=True).stdout.strip()
    summary = {'knowcode_version': version, 'questions': rows}
    summary['method_level'] = {ap: totals([r for r in rows if r['family'] in METHOD_FAMILIES], ap)
                               for ap in sorted({x for r in rows if r['family'] in METHOD_FAMILIES for x in r['scores']})}
    summary['failures'] = verdict(rows)
    return summary


def table(summary):
    lines = ['| Question | Approach | Recall | Precision |', '| --- | --- | ---: | ---: |']
    for r in summary['questions']:
        for approach, s in r['scores'].items():
            rec = f"{s['hits']}/{s['truth']}"
            pre = f"{s['hits']}/{s['listed']}" if s['listed'] else '—'
            lines.append(f"| {r['id']} | {approach} | {rec} | {pre} |")
    lines += ['', '| Method-level and caller questions | Recall | Precision |', '| --- | ---: | ---: |']
    for approach, t in summary['method_level'].items():
        lines.append(f"| {approach} | {t['recall']} ({t['hits']}/{t['truth']}) | {t['precision']} ({t['hits']}/{t['listed']}) |")
    lines.append('')
    lines += [f'FAIL: {f}' for f in summary['failures']] or ['history gate: pass']
    return '\n'.join(lines) + '\n'


def main():
    parser = argparse.ArgumentParser(description=__doc__.split('\n\n')[0])
    sub = parser.add_subparsers(dest='command', required=True)
    f = sub.add_parser('fetch')
    f.add_argument('--work', required=True)
    f.add_argument('--repos')
    r = sub.add_parser('run')
    r.add_argument('--knowcode', required=True)
    r.add_argument('--work', required=True)
    r.add_argument('--repos')
    r.add_argument('--out')
    a = parser.parse_args()
    if a.command == 'fetch':
        fetch(a.work, set(a.repos.split(',')) if a.repos else None)
        return
    summary = run(a)
    if a.out:
        Path(a.out).write_text(json.dumps(summary, indent=1, sort_keys=True) + '\n')
    print(table(summary), end='')
    sys.exit(1 if summary['failures'] else 0)


if __name__ == '__main__':
    main()
