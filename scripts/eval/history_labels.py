"""Labels for the history question set, from git alone (eval/history-v1/).

Every label is computed from a full-history clone checked out at the question's pin, with git and nothing else: no
KnowCode answer is read. The same clone and git version give byte-identical labels, so the set can join the release
replay gate, which makes no model calls.

A member's span is its declaration line through its closing brace (C#), found by matching braces outside strings,
characters and comments; attributes and doc comments above it are left out. Each span is printed with its first and
last lines, so it can be checked by eye.

Families (see eval/history-v1/README.md):

- symbol-commits: the commits that changed the member's span, `git log -L START,END:FILE`, back to `since`.
- symbol-last: the newest of those, with its author.
- symbol-blame: the member's current lines by author, `git blame -w -L START,END`, most first.
- symbol-cochange: the files changed in those commits (commits of at most MAX_FILES files), by shared commits.
- callers-history: of the files holding a direct caller (hop 1 of a published public-v1/v2 label), those changed in
  one of those commits; a file whose callers the published label all marks optional is `optional` here too.
- file-cochange: the files changed in the same non-merge commits as a file (commits of at most MAX_FILES files), by
  shared commits; `tests` keeps test files only.

Usage: history_labels.py --repos DIR [--only ID] > labels.jsonl   (DIR holds one clone per repo name, at its pin)
"""
from __future__ import annotations

import argparse
import hashlib
import json
from collections import Counter
from pathlib import Path
import re
import subprocess
import sys

HERE = Path(__file__).resolve().parent
QUESTIONS = HERE.parent.parent / 'eval' / 'history-v1' / 'questions.jsonl'
STUDIES = HERE.parent.parent / 'eval'
MAX_FILES = 50
TEST_PATH = re.compile(r'(^|/)(tests?|specs?)/|(Tests?|Spec|Specs|\.test|\.spec)(\.[A-Za-z]+)?\.[A-Za-z]+$|\.Tests?/', re.I)


def git(repo: Path, *args: str) -> str:
    out = subprocess.run(['git', '-C', str(repo), *args], capture_output=True, check=True)
    return out.stdout.decode('utf-8', 'replace')


def span(text: str, line: int, name: str) -> tuple[int, int]:
    """The member declared on 1-based `line`: (line, the line of its closing brace or expression body's `;`)."""
    lines = text.split('\n')
    if name not in lines[line - 1]:
        raise ValueError(f'line {line} does not declare {name}: {lines[line - 1].strip()!r}')
    start = sum(len(l) + 1 for l in lines[:line - 1])
    i, n, depth, opened = start, len(text), 0, False
    while i < n:
        c = text[i]
        two = text[i:i + 2]
        if two == '//':
            i = text.find('\n', i)
            i = n if i < 0 else i
            continue
        if two == '/*':
            j = text.find('*/', i + 2)
            i = n if j < 0 else j + 2
            continue
        if c == '"' or two in ('@"', '$"') or text[i:i + 3] in ('$@"', '@$"'):
            q = text.index('"', i)
            verbatim = '@' in text[i:q]
            i = q + 1
            while i < n:
                if text[i] == '\\' and not verbatim:
                    i += 2
                    continue
                if text[i] == '"':
                    if verbatim and text[i + 1:i + 2] == '"':
                        i += 2
                        continue
                    break
                i += 1
            i += 1
            continue
        if c == "'":
            j = i + 1
            while j < n and text[j] != "'":
                j += 2 if text[j] == '\\' else 1
            i = j + 1
            continue
        if c == '{':
            depth += 1
            opened = True
        elif c == '}':
            depth -= 1
            if opened and depth == 0:
                return line, text.count('\n', 0, i) + 1
        elif c == ';' and not opened:
            # An abstract or interface member, or an expression body (`=> …;`).
            return line, text.count('\n', 0, i) + 1
        i += 1
    raise ValueError(f'no end for {name} at line {line}')


def line_log(repo: Path, file: str, a: int, b: int, since: str | None) -> list[dict]:
    """Commits that changed lines a..b of file at HEAD, newest first (`git log -L`)."""
    args = ['log', f'-L{a},{b}:{file}', '--format=@@%H%x09%an%x09%cs%x09%s', '-s']
    if since:
        args.append(f'--since={since}')
    out = []
    for l in git(repo, *args).splitlines():
        if l.startswith('@@'):
            sha, author, date, subject = l[2:].split('\t', 3)
            out.append({'sha': sha, 'author': author, 'date': date, 'subject': subject})
    return out


def files_of(repo: Path, sha: str) -> list[str] | None:
    """Files a commit changed against its first parent; None past MAX_FILES (a sweep says nothing about coupling)."""
    files = [f for f in git(repo, 'diff-tree', '--no-commit-id', '-r', '--name-only', '--root', '-m', '--first-parent', sha).splitlines() if f]
    return None if len(files) > MAX_FILES else files


def current_names(repo: Path) -> dict[str, str]:
    """Every path a file had, to the path it has at HEAD, from the renames git detects (`log -M`)."""
    later: dict[str, str] = {}
    out = git(repo, 'log', '--reverse', '--no-merges', '-M', '--diff-filter=R', '--name-status', '--format=')
    for l in out.splitlines():
        parts = l.split('\t')
        if len(parts) == 3 and parts[0].startswith('R'):
            later[parts[1]] = parts[2]

    def resolve(p: str) -> str:
        seen = set()
        while p in later and p not in seen:
            seen.add(p)
            p = later[p]
        return p
    return {p: resolve(p) for p in later}


def cochange(repo: Path, shas: list[str], own: set[str], now: set[str], names: dict[str, str] | None = None) -> list[dict]:
    """Files changed in `shas` other than `own`, existing at HEAD, by shared commits, most first then by path. With
    `names`, each file counts under the name it has now."""
    counts: Counter = Counter()
    used = 0
    for sha in shas:
        files = files_of(repo, sha)
        if files is None:
            continue
        used += 1
        current = {names.get(f, f) for f in files} if names is not None else set(files)
        for f in current - own:
            if f in now:
                counts[f] += 1
    return [{'file': f, 'shared': c} for f, c in sorted(counts.items(), key=lambda kv: (-kv[1], kv[0]))], used


def published_callers(ref: str, repo: str) -> list[dict]:
    study, qid = ref.split('#')
    for l in (STUDIES / study / 'study' / 'labels.jsonl').read_text().splitlines():
        lab = json.loads(l)
        if lab['id'] == qid:
            return [i for i in lab['items'] if i.get('hop') == 1 and i['item'].startswith(repo + '/')]
    raise KeyError(ref)


def label(q: dict, repos: Path) -> dict:
    repo = repos / q['repo']
    head = git(repo, 'rev-parse', 'HEAD').strip()
    if head != q['pin']:
        raise SystemExit(f"{q['id']}: {repo} is at {head}, not the pin {q['pin']}")
    s = q['subject']
    text = (repo / s['file']).read_bytes()
    out = {'id': q['id'], 'family': q['family'], 'pin': head, 'file': s['file'],
           'source_sha256': hashlib.sha256(text).hexdigest(), 'completeness': 'exhaustive'}
    p = q.get('params', {})
    now = set(git(repo, 'ls-files').splitlines())
    if q['family'] == 'file-cochange':
        since = [f"--since={p['since']}"] if p.get('since') else []
        if p.get('follow'):
            # Renames followed on both sides: the file's commits under any name it had, and each partner counted
            # under the name it has now.
            shas = git(repo, 'log', '--follow', '--no-merges', '--format=%H', *since, '--', s['file']).split()
            ranked, used = cochange(repo, shas, {s['file']}, now, current_names(repo))
        else:
            shas = git(repo, 'log', '--no-merges', '--format=%H', *since, '--', s['file']).split()
            ranked, used = cochange(repo, shas, {s['file']}, now)
        if p.get('tests'):
            ranked = [r for r in ranked if TEST_PATH.search(r['file'])]
        out.update(commits=len(shas), commits_counted=used, items=[r for r in ranked if r['shared'] >= p.get('min_shared', 2)])
        return out
    a, b = span(text.decode('utf-8-sig', 'replace'), s['line'], s['name'])
    src = text.decode('utf-8-sig', 'replace').split('\n')
    out['span'] = {'start': a, 'end': b, 'first': src[a - 1].strip(), 'last': src[b - 1].strip()}
    if q['family'] == 'symbol-blame':
        authors: Counter = Counter()
        for l in git(repo, 'blame', '-w', '--line-porcelain', f'-L{a},{b}', '--', s['file']).splitlines():
            if l.startswith('author '):
                authors[l[7:]] += 1
        total = sum(authors.values())
        out['items'] = [{'author': n, 'lines': c, 'share': round(c / total, 3)} for n, c in sorted(authors.items(), key=lambda kv: (-kv[1], kv[0]))]
        return out
    commits = line_log(repo, s['file'], a, b, p.get('since'))
    if q['family'] == 'symbol-commits':
        out['items'] = commits
    elif q['family'] == 'symbol-last':
        out['items'] = commits[:1]
    elif q['family'] == 'symbol-cochange':
        ranked, used = cochange(repo, [c['sha'] for c in commits], {s['file']}, now)
        out.update(commits=len(commits), commits_counted=used, items=[r for r in ranked if r['shared'] >= p.get('min_shared', 2)])
    elif q['family'] == 'callers-history':
        callers = published_callers(q['callers'], q['repo'])
        file_of = lambda c: c['item'].split('/', 1)[1].rsplit(':', 1)[0]
        caller_files = sorted({file_of(c) for c in callers})
        touched: dict[str, list[str]] = {}
        for c in commits:
            files = files_of(repo, c['sha'])
            for f in caller_files:
                if files is not None and f in files:
                    touched.setdefault(f, []).append(c['sha'][:12])
        # A file whose every caller the published label marks optional (one it accepts listed or not) is optional too.
        optional = {f for f in caller_files if all(c.get('optional') for c in callers if file_of(c) == f)}
        out.update(callers=len(callers), caller_files=caller_files, commits=len(commits),
                   items=[{'file': f, 'commits': touched[f], **({'optional': True} if f in optional else {})}
                          for f in caller_files if f in touched])
    else:
        raise SystemExit(f"{q['id']}: unknown family {q['family']}")
    return out


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.split('\n')[0])
    ap.add_argument('--repos', type=Path, required=True)
    ap.add_argument('--questions', type=Path, default=QUESTIONS)
    ap.add_argument('--only')
    o = ap.parse_args()
    for l in o.questions.read_text().splitlines():
        q = json.loads(l)
        if o.only and q['id'] != o.only:
            continue
        print(json.dumps(label(q, o.repos), sort_keys=True))
        sys.stdout.flush()


if __name__ == '__main__':
    main()
