#!/usr/bin/env python3
"""tasks-v1: tasks from public repositories' fix commits, labelled from git alone (eval/tasks-v1/README.md).

    task_labels.py build --work WORK --out eval/tasks-v1/tasks.jsonl [--titles WORK/issue-titles.json]
    task_labels.py check --work WORK [--tasks eval/tasks-v1/tasks.jsonl]

The repositories and pins are identity-v1's (scripts/eval/identity/repos.txt), cloned with their full history into
WORK/clones by `task_gate.py fetch`. `build` walks each repository's non-merge commits up to its pin, keeps the fix
commits (FIX, not NOT_A_FIX) that change between 1 and MAX_SOURCE existing source files, and draws QUOTA of them per
repository in SHA-256 order of `repo:sha`. Commits from SINCE on, with a subject of at least MIN_WORDS words. A task's text is the title of the issue its commit message closes
(`Fixes #12`), when GitHub has that issue, else the commit subject; titles are fetched once into --titles, so a rebuild
reads them from there; a squash merge's trailing `(#N)` is dropped. Its labels are the files the commit modified that exist at its parent: the source files, and
apart, the tests. `check` recomputes every task's labels from git and exits 1 on a difference.

No repository code runs, and no model is asked anything.
"""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
REPOS = HERE/'identity/repos.txt'
TASKS = ROOT/'eval/tasks-v1/tasks.jsonl'
GIT_ENV = {'PATH': '/usr/bin:/bin:/opt/homebrew/bin:/usr/local/bin', 'LC_ALL': 'C', 'GIT_CONFIG_NOSYSTEM': '1',
           'GIT_CONFIG_GLOBAL': '/dev/null', 'GIT_TERMINAL_PROMPT': '0'}
QUOTA = {'serilog': 6, 'autofac': 6, 'jellyfin-server': 8, 'hono': 9, 'jellyfin-web': 9, 'swift-argument-parser': 5,
         'swift-collections': 5, 'swift-nio': 6}
MAX_SOURCE = 6
MAX_FILES = 12
SOURCE = {'csharp': ('.cs',), 'typescript': ('.ts', '.tsx', '.js', '.jsx', '.mts', '.cts'), 'swift': ('.swift',)}
FIX = re.compile(r'(?i)\b(fix|fixes|fixed|fixing|bug|crash|regression|incorrect|wrong)\b')
# Commits whose subject says they fix the build, the docs or the tooling rather than behaviour.
NOT_A_FIX = re.compile(r'(?i)(\bci\b|\(ci\)|\bchore\b|\bdocs?\b|documen|readme|typo|spelling|\blint|warning|\bdeps\b|'
                       r'bump|dependenc|format|flaky|\bbuild\b|compil|\bsdk\b|workflow|changelog|revert|\btests?\b:|'
                       r'merge|conflict|rename|markup|\bimports?\b|\b[A-Z]{1,3}\d{3,5}\b)')
# The commit window: older history describes code laid out differently from today's.
SINCE = '2020-01-01'
MIN_WORDS = 4
CLOSES = r'(?i)\b(?:fix(?:es|ed)?|close[sd]?|resolve[sd]?)\b:?\s+(?:https://github\.com/{owner}/{name}/issues/|#)(\d+)\b'
TEST_PATH = re.compile(r'(^|/)(tests?|Tests|__tests__|spec|specs)(/|$)|(\.test|\.spec)\.[jt]sx?$|Tests?\.(cs|swift)$')
# A squash-merged subject's pull request number, `(#1584)`: no part of the task.
PR_SUFFIX = re.compile(r'(\s*\(#\d+\))+\s*$')
GENERATED = re.compile(r'(\.g\.cs|\.Designer\.cs|\.min\.js|\.d\.ts)$')


def repos():
    out = {}
    for line in REPOS.read_text().splitlines():
        if line.strip() and not line.startswith('#'):
            name, lang, url, sha = line.split()
            out[name] = dict(lang=lang, url=url, sha=sha)
    return out


def git(clone, *args):
    return subprocess.run(['git', '-c', 'core.hooksPath=/dev/null', *args], cwd=clone, env=GIT_ENV, check=True,
                          capture_output=True, text=True).stdout


def is_test(path):
    return bool(TEST_PATH.search(path))


def labels(clone, sha, lang):
    """(source files, test files) the commit modified that exist at its parent; None when it isn't one task (a root
    commit, too many files, no source file)."""
    parents = git(clone, 'rev-list', '--parents', '-n', '1', sha).split()[1:]
    if len(parents) != 1:
        return None
    changed = [l.split('\t') for l in git(clone, 'diff-tree', '--no-commit-id', '--name-status', '-r', sha).splitlines()]
    existing = [p for status, *paths in changed if status[0] in 'MD' for p in paths[:1]]
    code = sorted(p for p in existing if p.endswith(SOURCE[lang]) and not GENERATED.search(p))
    source = [p for p in code if not is_test(p)]
    tests = [p for p in code if is_test(p)]
    if not source or len(source) > MAX_SOURCE or len(changed) > MAX_FILES:
        return None
    return source, tests


def issue_title(owner, name, number, cache):
    key = f'{owner}/{name}#{number}'
    if key not in cache:
        p = subprocess.run(['gh', 'api', f'repos/{owner}/{name}/issues/{number}'], capture_output=True, text=True)
        issue = json.loads(p.stdout) if p.returncode == 0 else {}
        cache[key] = None if not issue or 'pull_request' in issue else issue.get('title')
    return cache[key]


def order(repo, sha):
    return hashlib.sha256(f'{repo}:{sha}'.encode()).hexdigest()


def build(work, titles_path):
    titles = json.loads(Path(titles_path).read_text()) if Path(titles_path).is_file() else {}
    tasks = []
    for repo, r in repos().items():
        clone = Path(work)/'clones'/repo
        owner, name = r['url'].rstrip('/').split('/')[-2:]
        log = git(clone, 'log', '--no-merges', f'--since={SINCE}', '--format=%H%x1f%s%x1f%b%x1e', r['sha'])
        commits = [c.strip('\n').split('\x1f') for c in log.split('\x1e') if c.strip()]
        candidates = [(sha, subject, body) for sha, subject, body in commits
                      if FIX.search(subject) and not NOT_A_FIX.search(subject) and len(subject.split()) >= MIN_WORDS]
        taken = 0
        for sha, subject, body in sorted(candidates, key=lambda c: order(repo, c[0])):
            if taken == QUOTA[repo]:
                break
            got = labels(clone, sha, r['lang'])
            if got is None:
                continue
            source, tests = got
            closes = re.findall(CLOSES.format(owner=re.escape(owner), name=re.escape(name)), f'{subject}\n{body}')
            title = next((t for t in (issue_title(owner, name, n, titles) for n in closes) if t), None)
            parent = git(clone, 'rev-parse', f'{sha}^').strip()
            tasks.append(dict(id=f'{repo}-{sha[:10]}', repo=repo, lang=r['lang'], commit=sha, parent=parent,
                              task=PR_SUFFIX.sub('', title or subject), source='issue' if title else 'subject',
                              files=source, tests=tests))
            taken += 1
        print(f'{repo}: {taken} of {len(candidates)} fix commits', file=sys.stderr)
    Path(titles_path).write_text(json.dumps(titles, indent=1, sort_keys=True) + '\n')
    return tasks


def check(work, tasks_path):
    known = repos()
    diffs = []
    for t in (json.loads(l) for l in Path(tasks_path).read_text().splitlines() if l.strip()):
        got = labels(Path(work)/'clones'/t['repo'], t['commit'], known[t['repo']]['lang'])
        if got is None or list(got) != [t['files'], t['tests']]:
            diffs.append(t['id'])
    return diffs


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest='cmd', required=True)
    b = sub.add_parser('build')
    b.add_argument('--work', required=True)
    b.add_argument('--out', default=str(TASKS))
    b.add_argument('--titles')
    c = sub.add_parser('check')
    c.add_argument('--work', required=True)
    c.add_argument('--tasks', default=str(TASKS))
    args = ap.parse_args()
    if args.cmd == 'build':
        tasks = build(args.work, args.titles or str(Path(args.work)/'issue-titles.json'))
        Path(args.out).parent.mkdir(parents=True, exist_ok=True)
        Path(args.out).write_text(''.join(json.dumps(t, sort_keys=True) + '\n' for t in tasks))
        print(f'{len(tasks)} tasks')
    else:
        diffs = check(args.work, args.tasks)
        print('\n'.join(diffs) or 'labels match')
        sys.exit(1 if diffs else 0)


if __name__ == '__main__':
    main()
