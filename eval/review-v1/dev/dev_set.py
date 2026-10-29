#!/usr/bin/env python3
"""review-v1's development set: what `review` is tuned on, so tuning never reads the gate's own reviews.

    dev_set.py --work WORK --out DEV.jsonl [--pools POOLS.json]

The pool is review-v1's own (review_labels.pool, at the same repositories and pins), less every review that shares
anything with the frozen set (eval/review-v1/reviews.jsonl): its introducing commit, its fix, or, in the same
repository, any file either commit changed or labels. The draw is review_labels.draw with the seed SEED and 25 reviews
per language. `--pools` keeps the pools computed (the slow part) for a later draw.
"""
import argparse
import json
from pathlib import Path
import sys

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent.parent.parent/'scripts/eval'))
import review_labels  # noqa: E402
import task_labels  # noqa: E402

SEED = 'review-dev-v1'
QUOTA = {'csharp': 25, 'typescript': 25, 'swift': 25}


def files_of(r):
    return set(r['changed']) | set(r['files']) | set(r['tests'])


def disjoint(pools, frozen):
    """Each repository's reviews sharing no commit, no fix and no file with a frozen review."""
    commits = {r['commit'] for r in frozen} | {r['fix'] for r in frozen}
    files = {}
    for r in frozen:
        files.setdefault(r['repo'], set()).update(files_of(r))
    return {repo: [g for g in reviews if g['commit'] not in commits and g['fix'] not in commits
                   and not files_of(g) & files.get(repo, set())]
            for repo, reviews in pools.items()}


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--work', required=True)
    ap.add_argument('--out', required=True)
    ap.add_argument('--pools')
    args = ap.parse_args()
    frozen = [json.loads(l) for l in review_labels.REVIEWS.read_text().splitlines() if l.strip()]
    if args.pools and Path(args.pools).is_file():
        pools = json.loads(Path(args.pools).read_text())
    else:
        pools = {repo: review_labels.pool(args.work, repo, r)[0] for repo, r in task_labels.repos().items()}
        if args.pools:
            Path(args.pools).write_text(json.dumps(pools, sort_keys=True))
    left = disjoint(pools, frozen)
    review_labels.SEED = SEED
    rows = [dict(r, part='dev') for r in review_labels.draw(left, QUOTA)]
    Path(args.out).write_text(''.join(json.dumps(r, sort_keys=True) + '\n' for r in rows))
    print(json.dumps({repo: [len(pools[repo]), len(left[repo])] for repo in pools}), file=sys.stderr)
    print(f'{len(rows)} dev reviews', file=sys.stderr)


if __name__ == '__main__':
    main()
