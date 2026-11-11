"""compare-v1's co-change questions (eval/compare-v1/README.md, "0.8's additions"): which files usually change with
file F, labelled by git with history-v1's `file-cochange` rule, on identity-v1's repositories at their pins with
their full history.

    cochange.py draw --clones CLONES --out eval/compare-v1/cochange.jsonl --strata eval/compare-v1/cochange-strata.json

CLONES holds each repository's full clone at its pin (task_gate.py fetch). The draw reads git alone.
"""
from __future__ import annotations

import argparse
from collections import defaultdict
import hashlib
import json
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))  # scripts/eval
import history_labels  # noqa: E402

REPOS = ('serilog', 'autofac', 'jellyfin-server', 'jellyfin-web', 'hono', 'swift-argument-parser',
         'swift-collections', 'swift-nio')
SOURCE = ('.cs', '.ts', '.tsx', '.js', '.jsx', '.swift')
# Strata by the file's non-merge commits (renames followed); each takes up to QUOTA files.
BUCKETS = (('5-19', 5, 19), ('20-99', 20, 99), ('100+', 100, None))
QUOTA = 4
MIN_SHARED = 2  # history-v1's file-cochange threshold: a label shares at least two commits with F
# KnowCode's default co-change window (kc_coupled: "365 days before HEAD"): the labels are also given limited to it, so
# the default-window difference is visible, not folded into one number.
WINDOW_DAYS = 365


def bucket(n):
    for name, low, high in BUCKETS:
        if n >= low and (high is None or n <= high):
            return name
    return None


def order(repo, path):
    return hashlib.sha256(f'compare-v1:cochange:{repo}:{path}'.encode()).hexdigest()


def commits(clone, path, since=None):
    args = ['log', '--follow', '--no-merges', '--format=%H']
    if since:
        args.append(f'--since={since}')
    return history_labels.git(clone, *args, '--', path).split()


def window_start(clone):
    """WINDOW_DAYS before the pin's commit date, as git's --since takes it."""
    import datetime
    head = datetime.datetime.fromisoformat(history_labels.git(clone, 'log', '-1', '--format=%cI').strip())
    return (head - datetime.timedelta(days=WINDOW_DAYS)).isoformat()


def label(clone, path, now, names, since=None):
    """F's co-changed files by history-v1's rule: every file at HEAD sharing at least MIN_SHARED of F's non-merge
    commits of at most MAX_FILES files, under its current name, most shared first. Also the files sharing exactly one,
    which the scorer leaves unjudged. With `since`, only F's commits from then on count."""
    ranked, used = history_labels.cochange(clone, commits(clone, path, since), {path}, now, names)
    return ([[r['file'], r['shared']] for r in ranked if r['shared'] >= MIN_SHARED],
            sorted(r['file'] for r in ranked if r['shared'] < MIN_SHARED), used)


def draw(clones):
    questions, strata = [], {}
    for repo in REPOS:
        clone = Path(clones)/repo
        now = set(history_labels.git(clone, 'ls-files').splitlines())
        names = history_labels.current_names(clone)
        since = window_start(clone)
        counts = defaultdict(int)
        for line in history_labels.git(clone, 'log', '--no-merges', '--format=', '--name-only').splitlines():
            if line:
                counts[names.get(line, line)] += 1
        pool = defaultdict(list)
        for path in sorted(now):
            if path.endswith(SOURCE) and counts.get(path, 0) >= BUCKETS[0][1]:
                pool[bucket(counts[path])].append(path)
        for stratum in sorted(pool):
            chosen = []
            for path in sorted(pool[stratum], key=lambda p: order(repo, p)):
                if len(chosen) == QUOTA:
                    break
                labels, single, used = label(clone, path, now, names)
                if labels:  # a file nothing shares two commits with has no question to ask
                    chosen.append(path)
                    window, window_single, _ = label(clone, path, now, names, since)
                    questions.append({'id': f'{repo}:cochange:{path}', 'repo': repo, 'type': 'cochange',
                                      'stratum': stratum, 'file': path, 'labels': labels, 'single': single,
                                      'commits_counted': used, 'window_since': since, 'labels_window': window,
                                      'single_window': window_single})
            strata[f'{repo}/cochange/{stratum}'] = [len(pool[stratum]), len(chosen)]
    return questions, strata


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('command', choices=['draw'])
    ap.add_argument('--clones', required=True)
    ap.add_argument('--out', type=Path, required=True)
    ap.add_argument('--strata', type=Path, required=True)
    args = ap.parse_args(argv)
    questions, strata = draw(args.clones)
    args.out.write_text(''.join(json.dumps(q, sort_keys=True) + '\n' for q in questions))
    args.strata.write_text(json.dumps(strata, indent=1, sort_keys=True) + '\n')
    print(f'{len(questions)} co-change questions')


if __name__ == '__main__':
    main()
