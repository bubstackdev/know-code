#!/usr/bin/env python3
"""The public-v3 study (eval/public-v3/protocol.md): its frozen study, runner and scorer.

    public_v3.py run   (as public_v2.py run)
    public_v3.py score --freeze freeze.json --runs-dir RUNS --runs b1,b2 [--baseline summary.json] --out summary.json

The study, runner and session scoring are public-v2's (public_v2.py), with public-v3's format, arms and task counts.
The criteria follow the 0.4 plan's numbering:

1. Research tokens at most equal: the median paired difference in session tokens, named-full minus plain-full, ≤ 0.
2. Re-check cost halved: the median over questions of the per-question mean re-check tokens in named-full, at most half
   the 0.3.0 baseline's on the same bank.
3. F1 and recall at least equal, named-full minus plain-full: mean paired F1 difference ≥ 0 over exhaustive labels
   with items, and mean paired recall difference ≥ 0 over every label with items.
4. Unprompted use: unprompted-full sessions with a KnowCode call, at least 20 of every 24. The bare unprompted arm
   is reported alongside, as a descriptive figure.
5. Re-checks that changed the answer, listed per run (descriptive).

A full client's agent may search with Grep and Glob as well as Bash, and its KnowCode tools carry the plugin's names.
0.4's default profile advertises one tool, `kc`, whose `op` names the tool it runs; 0.3.0 advertises separate tools.
Before the re-check detector (recheck.py, unchanged) and recheck_cost.py read a transcript, both are put in the form
the detector reads: every KnowCode call becomes `mcp__knowcode__<tool it runs>` with that tool's arguments
(`kc {op: refs, q: X}` reads as `mcp__knowcode__kc_refs {name: X}`; public_transcript.knowcode_tool), a Grep call
becomes the equivalent `grep -rn`, and a Glob call the equivalent `find -name`. Raw transcripts are never changed.
KnowCode calls are tallied per arm by the tool they run, the same way for both builds.
"""
from __future__ import annotations

import argparse
import copy
import json
from pathlib import Path
import random
import shlex
import sys

import public_v2
import recheck
from public_transcript import knowcode_tool
from frozen_study import ROOT, load
from public_v2 import bootstrap, mean, median, wilson

FORMAT = 'knowcode-public-v3-freeze-1'
NAMED, PLAIN, UNPROMPTED, BARE = 'knowcode-full', 'plain-full', 'knowcode-unprompted-full', 'knowcode-unprompted'
ARMS = (PLAIN, NAMED, UNPROMPTED, BARE)
COUNTS = {'definition-lookup': 6, 'di-implementations': 8, 'http-cross-repo': 12, 'two-hop-impact': 14}
SEED = 'knowcode-public-v3-bootstrap-2026-10-26'
PLUGIN_PREFIX = 'mcp__plugin_knowcode_knowcode__'


class Study(public_v2.Study):
    FORMAT = FORMAT
    ARMS = ARMS
    COUNTS = COUNTS


def detector_form(events):
    """A copy of a transcript's events in the form recheck.py reads (see the module docstring)."""
    out = copy.deepcopy(events)
    for event in out:
        message = event.get('message') if isinstance(event, dict) else None
        if not isinstance(message, dict) or not isinstance(message.get('content'), list):
            continue
        for block in message['content']:
            if not isinstance(block, dict) or block.get('type') != 'tool_use':
                continue
            name, inp = block.get('name', ''), block.get('input') or {}
            tool = knowcode_tool(name, inp)
            if tool is not None:
                block['name'], block['input'] = recheck.KC_PREFIX + tool[0], tool[1]
            elif name == 'Grep' and isinstance(inp.get('pattern'), str):
                block['name'], block['input'] = 'Bash', {'command': 'grep -rn ' + shlex.quote(inp['pattern']) + ' ' + shlex.quote(inp.get('path') or '.')}
            elif name == 'Glob' and isinstance(inp.get('pattern'), str):
                base = inp['pattern'].rsplit('/', 1)[-1]
                block['name'], block['input'] = 'Bash', {'command': 'find ' + shlex.quote(inp.get('path') or '.') + ' -name ' + shlex.quote(base)}
    return out


def knowcode_tools(events):
    """KnowCode calls in a detector-form transcript, counted by the tool they run."""
    counts = {}
    for event in events:
        message = event.get('message') if isinstance(event, dict) else None
        for block in message.get('content') if isinstance(message, dict) and isinstance(message.get('content'), list) else ():
            if isinstance(block, dict) and block.get('type') == 'tool_use' and str(block.get('name', '')).startswith(recheck.KC_PREFIX):
                tool = block['name'][len(recheck.KC_PREFIX):]
                counts[tool] = counts.get(tool, 0) + 1
    return counts


def summarize(rows, questions, runs, baseline=None):
    qids = sorted(q['id'] for q in questions)
    stratum = {q['id']: q['stratum'] for q in questions}

    def get(run, arm, q):
        return rows.get((run, arm, q), {})

    def per_question(sample):
        return [mean([get(r, NAMED, q).get('recheck_tokens') for r in runs]) for q in sample]

    def c2(sample):
        return median(per_question(sample))

    def paired(field, kinds, sample):
        return [get(r, NAMED, q)[field] - get(r, PLAIN, q)[field] for q in sample for r in runs
                if get(r, NAMED, q).get('label') in kinds and get(r, NAMED, q).get(field) is not None
                and get(r, PLAIN, q).get(field) is not None]

    rng = random.Random(SEED)
    criteria = {}
    every = ('exhaustive', 'exhaustive-empty', 'lower_bound', 'uncertain')
    tokens = median(paired('tokens', every, qids))
    criteria['1_research_tokens_at_most_equal'] = dict(
        rule='median paired difference in session tokens (knowcode-full - plain-full) <= 0', difference=tokens,
        ci95=bootstrap(qids, lambda s: median(paired('tokens', every, s)), rng), n=len(paired('tokens', every, qids)),
        met=None if tokens is None else tokens <= 0)
    value = c2(qids)
    entry = dict(rule='median over questions of per-question mean re-check tokens (knowcode-full) <= 0.5 x the 0.3.0 baseline',
                 value=value, ci95=bootstrap(qids, c2, rng), mean_per_session=mean([get(r, NAMED, q).get('recheck_tokens') for q in qids for r in runs]),
                 per_question=dict(zip(qids, per_question(qids))))
    if baseline is not None:
        base = baseline['criteria']['2_recheck_cost_halved']['value']
        entry.update(baseline_value=base, ratio=value / base if base else None, met=None if value is None or not base else value <= 0.5 * base)
    else:
        entry.update(met=None, note='scored without a baseline: this is the reference value')
    criteria['2_recheck_cost_halved'] = entry
    with_items = ('exhaustive', 'lower_bound', 'uncertain')
    f1 = mean(paired('f1', ('exhaustive',), qids))
    recall = mean(paired('recall', with_items, qids))
    criteria['3_f1_and_recall_at_least_equal'] = dict(
        rule='mean paired F1 difference (knowcode-full - plain-full) >= 0 over exhaustive labels with items, and mean '
             'paired recall difference >= 0 over every label with items',
        f1_difference=f1, f1_ci95=bootstrap(qids, lambda s: mean(paired('f1', ('exhaustive',), s)), rng), f1_n=len(paired('f1', ('exhaustive',), qids)),
        recall_difference=recall, recall_ci95=bootstrap(qids, lambda s: mean(paired('recall', with_items, s)), rng),
        recall_n=len(paired('recall', with_items, qids)), recall_questions=len({q for q in qids if get(runs[0], NAMED, q).get('label') in with_items}),
        met=None if f1 is None or recall is None else f1 >= 0 and recall >= 0)

    def use(arm):
        sessions = [get(r, arm, q) for q in qids for r in runs]
        used = sum((s.get('knowcode_calls') or 0) > 0 for s in sessions)
        return dict(sessions=len(sessions), sessions_with_call=used, share=used / len(sessions) if sessions else None,
                    ci95=wilson(used, len(sessions)))
    full, bare = use(UNPROMPTED), use(BARE)
    criteria['4_unprompted_use'] = dict(rule='knowcode-unprompted-full sessions with a KnowCode call >= 20 of every 24', **full,
                                        met=full['share'] is not None and full['share'] >= public_v2.UNPROMPTED_SHARE,
                                        bare_descriptive=bare)
    changed = [dict(run=r, arm=a, question=q, calls=row['changed']) for (r, a, q), row in sorted(rows.items())
               if r in runs and a in (NAMED, UNPROMPTED, BARE) and row.get('changed')]
    criteria['5_rechecks_that_changed_the_answer'] = dict(
        rule='descriptive: each is listed as a KnowCode failure',
        **{arm: {r: sum(len(c['calls']) for c in changed if c['arm'] == arm and c['run'] == r) for r in runs} for arm in (NAMED, UNPROMPTED, BARE)},
        sessions=changed)
    arms = {}
    for arm in ARMS:
        rs = [get(r, arm, q) for q in qids for r in runs]
        arms[arm] = dict(sessions=len(rs), statuses={k: sum(x.get('status', 'missing') == k for x in rs) for k in sorted({x.get('status', 'missing') for x in rs})},
                         tokens_median=median([x.get('tokens') for x in rs]), cache_read_median=median([x.get('cache_read') for x in rs]),
                         uncached_input_median=median([x.get('uncached_input') for x in rs]),
                         fixed_overhead_median=median([x.get('fixed_overhead') for x in rs]),
                         research_calls_mean=mean([x.get('research_calls') for x in rs]),
                         knowcode_calls_mean=mean([x.get('knowcode_calls') for x in rs]),
                         knowcode_tools={t: sum((x.get('knowcode_tools') or {}).get(t, 0) for x in rs)
                                         for t in sorted({t for x in rs for t in x.get('knowcode_tools') or {}})},
                         cost_usd=sum(x.get('cost_usd') or 0 for x in rs),
                         refused_naming_binary=sum(x.get('refused_naming_binary') or 0 for x in rs),
                         refused_other=sum(x.get('refused_other') or 0 for x in rs))
        if arm != PLAIN:
            claims = sum(x.get('answers_with_claims') or 0 for x in rs)
            arms[arm].update(recheck_tokens_mean=mean([x.get('recheck_tokens') for x in rs]),
                             recheck_rate=sum(x.get('rechecked_answers') or 0 for x in rs) / claims if claims else None,
                             use_by_task_type={s: dict(sessions=sum(stratum[q] == s for q in qids for _ in runs),
                                                       with_call=sum(stratum[q] == s and (get(r, arm, q).get('knowcode_calls') or 0) > 0
                                                                     for q in qids for r in runs)) for s in COUNTS})
    return dict(criteria=criteria, arms=arms)


def score(args):
    study = Study(args.freeze)
    runs = args.runs.split(',')
    scorer = load('public_v3_neutral_score', ROOT / 'scripts/eval/neutral/score.py')
    questions = {q['id']: q for q in study.questions}
    labels = {l['id']: l for l in study.labels}
    rows = {}
    for row in study.order:
        if row['run'] not in runs:
            continue
        base = Path(args.runs_dir) / row['run'] / row['session']
        result = json.loads((base / 'result.json').read_text()) if (base / 'result.json').is_file() else None
        events = detector_form(recheck.read_events(base / 'transcript.jsonl')) if (base / 'transcript.jsonl').is_file() else []
        rows[(row['run'], row['arm'], row['question_id'])] = public_v2.score_session(
            scorer, questions[row['question_id']], labels[row['question_id']], result, events,
            study.config['knowcode_binary_name'], not row['arm'].startswith('plain'))
        rows[(row['run'], row['arm'], row['question_id'])]['knowcode_tools'] = knowcode_tools(events)
    baseline = json.loads(Path(args.baseline).read_text()) if args.baseline else None
    summary = dict(format='public-v3-summary-1', freeze_sha256=study.freeze_sha256, runs=runs,
                   bootstrap=dict(seed=SEED, resamples=public_v2.RESAMPLES, unit='question'),
                   **summarize(rows, study.questions, runs, baseline),
                   sessions=[dict(run=r, arm=a, question_id=q, **v) for (r, a, q), v in sorted(rows.items())])
    out = Path(args.out)
    if out.exists():
        raise SystemExit('output exists; scored results are never overwritten')
    out.write_text(json.dumps(summary, indent=2, sort_keys=True) + '\n')
    print(json.dumps(summary['criteria'], indent=2, sort_keys=True)[:4000])


def main():
    if sys.argv[1:2] == ['run']:
        sys.argv.pop(1)
        parser = argparse.ArgumentParser(description=__doc__)
        for name in ['freeze', 'source', 'plan-root', 'out', 'ledger', 'client', 'knowcode', 'runs', 'sessions']:
            parser.add_argument('--' + name, required=True)
        parser.add_argument('--cap', type=float, required=True)
        parser.add_argument('--workers', type=int, default=3)
        parser.add_argument('--git-toolchain', default='/Library/Developer/CommandLineTools')
        public_v2.run(parser.parse_args(), Study)
        return
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['score'])
    for name in ['freeze', 'runs-dir', 'runs', 'out']:
        parser.add_argument('--' + name, required=True)
    parser.add_argument('--baseline')
    score(parser.parse_args())


if __name__ == '__main__':
    main()
