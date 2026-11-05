#!/usr/bin/env python3
"""Contract-changes gate: `review`'s `broken` consumers on contract-v1's commits, scored against the labelled client
requests, with no model calls (eval/contract-v1/README.md#scoring).

    contract_changes_gate.py run --knowcode BIN --work WORK --rows ROWS [--pairs a,b] [--out SUMMARY]

ROWS is the set's rows (`contract_labels.py rows`); a file whose sha256 isn't the one the README records is refused, so
no other rows are ever scored as the set. A run whose SUMMARY's answers file exists asks only what that file doesn't
hold. For each pair and each of its commits, the server at the commit and the client at the row's client commit are
checked out side by side as worktrees of WORK/clones (WORK/gate/<pair>/<commit, 12 digits>/ws), indexed into one fresh
cache with a config whose only workspace root is that directory, and `BIN review --diff <parent> --json` is asked in the
server. The checkouts and the cache are removed after each answer; the answers are kept beside SUMMARY.

A reported `broken` consumer is right when a row of that commit and pair has its verb and path (compared as the labeller
compares them) and a labelled consumer in the same client file, starting at most LINE_SLACK lines before the reported
line. Precision is right over reported `broken`, per pair and pooled; the gate is pooled precision at least GATE, with
every commit answered (a commit without an answer fails the run rather than leaving its consumers uncounted). Recall is
matched labelled consumers (by a `broken` or `possible` consumer) over all of them, and over those whose request writes
its path in the client's own source (`send`, `url`).
"""
from __future__ import annotations

import argparse
from collections import defaultdict
import hashlib
import json
import re
from pathlib import Path
import shutil
import subprocess
import sys

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import contract_labels as cl  # noqa: E402
import replay_gate  # noqa: E402

GATE = 0.95
LINE_SLACK = 4
OWN_SOURCE = ('send', 'url')
README = HERE.parent.parent/'eval'/'contract-v1'/'README.md'


def frozen_sha256(readme=README):
    """The rows' sha256 the README records (`shasum -a 256 rows.jsonl   # <hex>`)."""
    m = re.search(r'shasum -a 256 rows\.jsonl\s+#\s*([0-9a-f]{64})', Path(readme).read_text())
    if not m:
        raise SystemExit(f'{readme}: no rows checksum')
    return m.group(1)


def read_rows(path, readme=README):
    """The frozen rows, refused when the file isn't the one the README records."""
    data = Path(path).read_bytes()
    got, want = hashlib.sha256(data).hexdigest(), frozen_sha256(readme)
    if got != want:
        raise SystemExit(f'{path}: sha256 {got}, but the README records {want}; these are not the frozen rows')
    return [json.loads(l) for l in data.decode().splitlines() if l.strip()]


def contract_key(contract):
    """`VERB path` as review writes a route, in the labeller's terms."""
    verb, _, path = contract.partition(' ')
    return verb.upper(), cl.norm(path)


def judge(rows, answer, client):
    """One (pair, commit)'s answer against its rows: each reported consumer in the client, right or wrong, and each
    labelled consumer, matched or not."""
    labels = defaultdict(list)  # (verb, path) -> [(file, line, by)]
    for r in rows:
        for c in r['consumers']:
            labels[(r['verb'], cl.norm(r['path']))].append((c['file'], c['line'], c['by']))
    reported, matched = [], set()
    for ch in answer.get('contracts') or []:
        key = contract_key(ch.get('contract', ''))
        for u in ch.get('consumers') or []:
            if u.get('repo') != client:
                continue
            hit = [l for l in labels.get(key, []) if l[0] == u.get('file') and 0 <= u.get('line', 0) - l[1] <= LINE_SLACK]
            if u.get('verdict') in ('broken', 'possible'):
                matched.update((key, h) for h in hit)
            reported.append(dict(contract=ch.get('contract'), file=u.get('file'), line=u.get('line'),
                                 verdict=u.get('verdict'), right=bool(hit)))
    every = [(k, l) for k, ls in labels.items() for l in ls]
    return dict(reported=reported, labels=len(every), matched=sum(1 for e in every if e in matched),
                labels_own=sum(1 for _, l in every if l[2] in OWN_SOURCE),
                matched_own=sum(1 for k, l in every if l[2] in OWN_SOURCE and (k, l) in matched))


def tally(judged):
    broken = [x for j in judged for x in j['reported'] if x['verdict'] == 'broken']
    possible = [x for j in judged for x in j['reported'] if x['verdict'] == 'possible']
    right = sum(1 for x in broken if x['right'])
    labels, matched = sum(j['labels'] for j in judged), sum(j['matched'] for j in judged)
    own, matched_own = sum(j['labels_own'] for j in judged), sum(j['matched_own'] for j in judged)
    return dict(commits=len(judged), broken=len(broken), broken_right=right,
                precision=right / len(broken) if broken else None,
                possible=len(possible), possible_right=sum(1 for x in possible if x['right']),
                labels=labels, matched=matched, recall=matched / labels if labels else None,
                labels_own=own, matched_own=matched_own, recall_own=matched_own / own if own else None)


def passed(pooled, errors):
    """The gate: pooled precision of `broken` at least GATE, over a run where every commit was answered."""
    return not errors and pooled['precision'] is not None and pooled['precision'] >= GATE


def checkout(clone, path, commit):
    cl.git(clone, 'worktree', 'add', '--quiet', '--detach', str(path), commit)


def answer(binary, work, pair, commit, rows):
    p = cl.PAIRS[pair]
    base = work/'gate'/pair/commit[:12]
    ws = base/'ws'
    if base.exists():
        shutil.rmtree(base)
    ws.mkdir(parents=True)
    server, client = ws/p['server'], ws/p['client']
    clones = work/'clones'
    try:
        checkout(clones/p['server'], server, commit)
        checkout(clones/p['client'], client, rows[0]['client_commit'])
        env = replay_gate.build_env(base/'kc')
        cfg = Path(env['KNOWCODE_CONFIG_DIR'])
        cfg.mkdir(parents=True, exist_ok=True)
        (cfg/'config.toml').write_text(f'[workspace]\nroots = [{json.dumps(str(ws))}]\nmax_depth = 1\n')
        for tree in (server, client):
            ix = subprocess.run([str(binary), 'index', str(tree)], env=env, cwd=tree, capture_output=True, text=True)
            if ix.returncode:
                return {}, f'index {tree.name}: ' + (ix.stderr or ix.stdout)[:500]
        out = subprocess.run([str(binary), 'review', '--diff', rows[0]['parent'], '--json'], env=env, cwd=server,
                             capture_output=True, text=True)
        try:
            return json.loads(out.stdout), None
        except json.JSONDecodeError:
            return {}, (out.stderr or out.stdout)[:500]
    finally:
        for clone, tree in ((clones/p['server'], server), (clones/p['client'], client)):
            if tree.exists():
                cl.git(clone, 'worktree', 'remove', '--force', str(tree))
        shutil.rmtree(base, ignore_errors=True)


def run(args):
    rows = read_rows(args.rows)
    pairs = args.pairs.split(',') if args.pairs else sorted({r['pair'] for r in rows})
    binary, work = Path(args.knowcode).resolve(), Path(args.work).resolve()
    groups = defaultdict(list)
    for r in rows:
        if r['pair'] in pairs:
            groups[(r['pair'], r['commit'])].append(r)
    out = Path(args.out) if args.out else None
    # A run cut short goes on where it stopped: the answers it kept (without an error) are judged again, not asked.
    kept = {}
    if out and out.with_suffix('.answers.jsonl').exists():
        for l in out.with_suffix('.answers.jsonl').read_text().splitlines():
            a = json.loads(l)
            if not a.get('error'):
                kept[(a['pair'], a['commit'])] = a
    answers = out.with_suffix('.answers.jsonl').open('w') if out else None
    for a in kept.values():
        answers.write(json.dumps(a, sort_keys=True) + '\n')
    judged, errors = defaultdict(list), {}
    for (pair, commit), rs in sorted(groups.items()):
        if (pair, commit) in kept:
            ans, err = kept[(pair, commit)], None
        else:
            ans, err = answer(binary, work, pair, commit, rs)
        if err:
            errors[f'{pair}/{commit}'] = err
        j = judge(rs, ans, cl.PAIRS[pair]['client'])
        j.update(pair=pair, commit=commit, incomplete=ans.get('incomplete'))
        judged[pair].append(j)
        if answers and (pair, commit) not in kept:
            answers.write(json.dumps(dict(pair=pair, commit=commit, contracts=ans.get('contracts'),
                                          incomplete=ans.get('incomplete'), error=err), sort_keys=True) + '\n')
            answers.flush()
        print(f"{pair} {commit[:10]}: {len(j['reported'])} reported, {j['matched']}/{j['labels']} labels"
              + (f' ERROR {err[:80]}' if err else ''), file=sys.stderr, flush=True)
    version = subprocess.run([str(binary), '--version'], capture_output=True, text=True).stdout.strip()
    pooled = tally([j for js in judged.values() for j in js])
    summary = dict(knowcode=version, gate=GATE, line_slack=LINE_SLACK, errors=errors, pooled=pooled,
                   passed=passed(pooled, errors),
                   per_pair={p: tally(js) for p, js in sorted(judged.items())})
    if out:
        out.write_text(json.dumps(summary, indent=1, sort_keys=True) + '\n')
        out.with_suffix('.judged.jsonl').write_text(''.join(json.dumps(j, sort_keys=True) + '\n'
                                                            for js in judged.values() for j in js))
    print(report(summary))


def fmt(x):
    return '-' if x is None else f'{x:.3f}'


def report(s):
    lines = ['| Pair | Commits | Broken | Right | Precision | Possible | Recall (all) | Recall (own source) |',
             '| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |']
    for name, t in [*s['per_pair'].items(), ('pooled', s['pooled'])]:
        lines.append(f"| {name} | {t['commits']} | {t['broken']} | {t['broken_right']} | {fmt(t['precision'])} | "
                     f"{t['possible']} | {fmt(t['recall'])} ({t['matched']}/{t['labels']}) | "
                     f"{fmt(t['recall_own'])} ({t['matched_own']}/{t['labels_own']}) |")
    lines.append(f"\ngate: pooled precision of `broken` >= {s['gate']}: {'PASS' if s['passed'] else 'FAIL'}")
    if s['errors']:
        lines.append(f"errors: {len(s['errors'])} commits without an answer, so the run doesn't pass")
    return '\n'.join(lines)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest='cmd', required=True)
    r = sub.add_parser('run')
    r.add_argument('--knowcode', required=True)
    r.add_argument('--work', required=True)
    r.add_argument('--rows', required=True)
    r.add_argument('--pairs')
    r.add_argument('--out')
    args = ap.parse_args()
    run(args)


if __name__ == '__main__':
    main()
