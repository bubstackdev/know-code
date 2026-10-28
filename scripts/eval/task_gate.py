#!/usr/bin/env python3
"""Task-answers gate: a KnowCode op's answer to tasks-v1's tasks, scored against labels from git, with no model calls.

    task_gate.py fetch --work WORK [--repos a,b]
    task_gate.py run --knowcode BIN --work WORK [--tool kc_locate] [--query-arg query] [--args JSON]
                     [--tasks FILE] [--repos a,b] [--budgets 2000,4000,8000] [--out SUMMARY]
    task_gate.py compare BASE_SUMMARY CANDIDATE_SUMMARY

`fetch` clones each repository of scripts/eval/identity/repos.txt with its full history into WORK/clones and checks
out its pin. `run` checks out each task's parent commit as a worktree of that clone (WORK/trees/<task>, made once),
indexes it alone with the build's own cache, config and data (WORK/kc/<binary hash>/<task>), starts `BIN serve
--tools all` there, and sends the tool `--tool` the task's text as `--query-arg`, with `--args` merged in. The answer
is the tool's text, as a client reads it.

Per task: the answer's bytes, and the labels it names: a label is named when its repository-relative path appears in
the text. Recall of the changed source files and, apart, of the changed tests (over the tasks that changed tests), on
the whole answer and within each byte budget (a label counts when its path ends within the first B bytes). The
baseline is `locate` alone (the defaults). `compare` puts two summaries side by side.
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
import task_labels  # noqa: E402

TASKS = task_labels.TASKS
BUDGETS = (2000, 4000, 8000)


def fetch(work, names=None):
    clones = Path(work)/'clones'
    clones.mkdir(parents=True, exist_ok=True)
    for name, r in task_labels.repos().items():
        if names and name not in names.split(','):
            continue
        target = clones/name
        if not (target/'.git').is_dir():
            subprocess.run(['git', '-c', 'core.hooksPath=/dev/null', 'clone', '--quiet', '--no-checkout', r['url'], str(target)],
                           env=task_labels.GIT_ENV, check=True)
        task_labels.git(target, '-c', 'advice.detachedHead=false', 'checkout', '--quiet', r['sha'])
        print(f"{name} {task_labels.git(target, 'rev-parse', 'HEAD').strip()}")


def tree(work, task):
    """The task's parent commit, checked out once as a worktree of its repository's clone."""
    path = Path(work)/'trees'/task['id']
    if not (path/'.git').exists():
        path.parent.mkdir(parents=True, exist_ok=True)
        task_labels.git(Path(work)/'clones'/task['repo'], 'worktree', 'add', '--quiet', '--detach', str(path), task['parent'])
    return path


# --- judging --------------------------------------------------------------------------------------------------------

def named(text, paths, budgets):
    """Per path, whether the answer names it anywhere, and within each budget (its first mention ends inside it)."""
    data = text.encode()
    out = {}
    for p in paths:
        at = data.find(p.encode())
        end = at + len(p.encode()) if at >= 0 else None
        out[p] = {'all': at >= 0, **{str(b): end is not None and end <= b for b in budgets}}
    return out


def judge(task, text, budgets):
    return dict(id=task['id'], repo=task['repo'], lang=task['lang'], bytes=len(text.encode()),
                files=named(text, task['files'], budgets), tests=named(text, task['tests'], budgets))


def recall(judged, key, at):
    hits = total = 0
    for j in judged:
        for v in j[key].values():
            total += 1
            hits += v[at]
    return (round(hits / total, 4) if total else None), total


def metrics(judged, budgets):
    out = dict(tasks=len(judged), bytes_median=statistics.median([j['bytes'] for j in judged]) if judged else None,
               bytes_mean=round(statistics.mean([j['bytes'] for j in judged]), 1) if judged else None)
    for key in ('files', 'tests'):
        for at in ['all', *map(str, budgets)]:
            r, n = recall(judged, key, at)
            out[f'{key}_recall@{at}'] = r
        out[f'{key}_labels'] = n
    # Tasks with at least one changed file named: what a reader of the answer starts from.
    out['tasks_with_a_file'] = sum(1 for j in judged if any(v['all'] for v in j['files'].values()))
    return out


def summarize(judged, budgets):
    by = defaultdict(list)
    for j in judged:
        by[j['lang']].append(j)
    return dict(overall=metrics(judged, budgets), lang={k: metrics(v, budgets) for k, v in sorted(by.items())})


# --- run ------------------------------------------------------------------------------------------------------------

def ask(binary, env, path, tool, arguments, tries=2):
    """One call to a fresh server, asked again once when it doesn't answer in time (a loaded machine)."""
    for attempt in range(tries):
        server = replay_gate.Server(binary, ['--tools', 'all'], env, path)
        try:
            return server.call(tool, arguments, timeout=300)
        except TimeoutError:
            if attempt + 1 == tries:
                return True, 'timed out'
        finally:
            server.close()

def run(args):
    tasks = [json.loads(l) for l in Path(args.tasks).read_text().splitlines() if l.strip()]
    if args.repos:
        tasks = [t for t in tasks if t['repo'] in args.repos.split(',')]
    budgets = tuple(int(b) for b in args.budgets.split(','))
    binary = Path(args.knowcode).resolve()
    work = Path(args.work).resolve()
    hashed = hashlib.sha256(binary.read_bytes()).hexdigest()[:16]
    extra = json.loads(args.args) if args.args else {}
    judged, texts, errors = [], {}, {}
    for t in tasks:
        path = tree(work, t)
        env = replay_gate.build_env(work/'kc'/hashed/t['id'])
        subprocess.run([str(binary), 'index', str(path)], env=env, cwd=path, check=True, capture_output=True)
        failed, text = ask(binary, env, path, args.tool, {args.query_arg: t['task'], **extra})
        if failed:
            errors[t['id']] = text[:300]
        texts[t['id']] = text
        judged.append(judge(t, text, budgets))
        print(f"{t['id']}: {judged[-1]['bytes']} bytes", file=sys.stderr)
    version = subprocess.run([str(binary), '--version'], capture_output=True, text=True).stdout.strip()
    summary = dict(knowcode=version, binary=hashed, tool=args.tool, query_arg=args.query_arg, args=extra, budgets=budgets,
                   errors=errors, **summarize(judged, budgets))
    if args.out:
        Path(args.out).write_text(json.dumps(summary, indent=1, sort_keys=True) + '\n')
        Path(args.out).with_suffix('.judged.jsonl').write_text(''.join(json.dumps(j, sort_keys=True) + '\n' for j in judged))
        Path(args.out).with_suffix('.answers.jsonl').write_text(
            ''.join(json.dumps({'id': k, 'text': v}) + '\n' for k, v in texts.items()))
    print(report(summary))


def fmt(x):
    return '–' if x is None else f'{x:.3f}' if isinstance(x, float) else str(x)


def report(summary):
    budgets = [str(b) for b in summary['budgets']]
    head = ['Language', 'Tasks', 'Median bytes'] + [f'Files @{b}' for b in budgets] + ['Files, all'] + \
           [f'Tests @{b}' for b in budgets] + ['Tests, all']
    out = [f"KnowCode {summary['knowcode']} ({summary['binary']}), `{summary['tool']}` {json.dumps(summary['args'])}", '',
           '| ' + ' | '.join(head) + ' |', '| --- |' + ' ---: |' * (len(head) - 1)]
    for name, m in [*summary['lang'].items(), ('all', summary['overall'])]:
        cells = [name, m['tasks'], fmt(m['bytes_median'])] + [fmt(m[f'files_recall@{b}']) for b in budgets] + \
                [fmt(m['files_recall@all'])] + [fmt(m[f'tests_recall@{b}']) for b in budgets] + [fmt(m['tests_recall@all'])]
        out.append('| ' + ' | '.join(str(c) for c in cells) + ' |')
    if summary['errors']:
        out += ['', f"Errors: {len(summary['errors'])}"]
    return '\n'.join(out)


def compare(base, cand):
    budgets = [str(b) for b in base['budgets']]
    keys = ['bytes_median'] + [f'files_recall@{b}' for b in budgets] + ['files_recall@all'] + \
           [f'tests_recall@{b}' for b in budgets] + ['tests_recall@all']
    out = [f"`{base['tool']}` {json.dumps(base['args'])} → `{cand['tool']}` {json.dumps(cand['args'])}", '',
           '| Language | Figure | Base | Candidate |', '| --- | --- | ---: | ---: |']
    for lang in [*sorted(set(base['lang']) | set(cand['lang'])), 'all']:
        b = base['overall'] if lang == 'all' else base['lang'].get(lang, {})
        c = cand['overall'] if lang == 'all' else cand['lang'].get(lang, {})
        for k in keys:
            out.append(f'| {lang} | {k} | {fmt(b.get(k))} | {fmt(c.get(k))} |')
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
    r.add_argument('--tool', default='kc_locate')
    r.add_argument('--query-arg', default='query')
    r.add_argument('--args', help='JSON object merged into every call (a budget, a limit)')
    r.add_argument('--tasks', default=str(TASKS))
    r.add_argument('--repos')
    r.add_argument('--budgets', default=','.join(map(str, BUDGETS)))
    r.add_argument('--out')
    c = sub.add_parser('compare')
    c.add_argument('base')
    c.add_argument('candidate')
    args = ap.parse_args()
    if args.cmd == 'fetch':
        fetch(args.work, args.repos)
    elif args.cmd == 'run':
        run(args)
    else:
        print(compare(json.loads(Path(args.base).read_text()), json.loads(Path(args.candidate).read_text())))


if __name__ == '__main__':
    main()
