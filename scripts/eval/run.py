#!/usr/bin/env python3
"""KnowCode evaluation harness. See docs/evaluation.md.

Usage: scripts/eval/run.py <command> [options]   (`<command> -h` for each)

  snapshot   run a query set and store normalised answers
  diff       compare two snapshots; exit 1 if any answer differs
  mine       extract replay tasks (commits) from repos' git history into a JSONL dataset
  replay     score knowcode against those commits, run at each commit's parent
  report     summarise replay results with confidence intervals
  labels     score answers against a hand-labelled file (precision and recall)
  defects    defect prediction by history replay: hotspot scores against later fix commits

Private data (query sets, datasets, results, labels) lives under $KNOWCODE_EVAL_DIR, by default `<corpus>-eval` beside
the GitLab group corpus (scripts/corpus.py; `<corpus>` is $KNOWCODE_GITLAB_CORPUS, default `gitlab`); the public
examples live in eval/.
"""

import argparse
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import defects  # noqa: E402
import labels  # noqa: E402
import replay  # noqa: E402
import snapshot  # noqa: E402


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="command", required=True)
    snapshot.add_parsers(sub)
    replay.add_parsers(sub)
    labels.add_parsers(sub)
    defects.add_parsers(sub)
    a = ap.parse_args()
    a.func(a)


if __name__ == "__main__":
    main()
