#!/usr/bin/env python3
"""Score a public-v1 agent study run against its frozen labels: the pre-registered criteria and their intervals.

    score_public_study.py --freeze freeze.json --runs RUN_DIR [--baseline baseline-summary.json] --out summary.json

RUN_DIR holds one directory per session (named by the frozen schedule's `session`), each with the runner's
`result.json` and `transcript.jsonl`. Every scheduled session is scored; a missing or failed session stays in the
denominators as an error. The rules are in eval/public-v1/protocol.md ("Metrics" and "Criteria"); this script is
frozen by hash with the study, and nothing here reads source, starts a model or edits labels.

The summary holds per-session rows (question, arm, status, scores, tokens, calls, re-checks) and aggregates. It
contains no local paths: transcripts stay with the raw run.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import math
from pathlib import Path
import random
import statistics

import recheck
from frozen_study import ROOT, load

SEED = 'knowcode-public-v1-bootstrap-2026-10-20'
RESAMPLES = 10000
ARMS = ('plain', 'knowcode', 'knowcode-unprompted')
KNOWCODE_ARMS = ('knowcode', 'knowcode-unprompted')
SENSITIVITY_EXCLUDED = ('public-http-cross-repo-001', 'public-http-cross-repo-003')
UNPROMPTED_THRESHOLD = 20 / 24


def f1(precision, recall):
    return 0.0 if not precision or not recall else 2 * precision * recall / (precision + recall)


def label_kind(label):
    if label['completeness'] == 'exhaustive':
        return 'exhaustive-empty' if not label['items'] else 'exhaustive'
    return label['completeness']


def score_session(scorer, question, label, answer):
    """Correctness of one answer. Errors, refusals and missing answers stay in, scored zero."""
    status = answer.get('status', 'error') if answer else 'missing'
    kind = label_kind(label)
    row = {'status': status, 'label': kind, 'f1': None, 'recall': None, 'precision': None, 'correct_empty': None}
    if status == 'answered':
        scored = scorer.score_one(label, answer.get('items', []), question.get('repos', ()))
        row.update(precision=scored['precision'], recall=scored['recall'], tp=scored['tp'], fn=scored['fn'],
                   fp=scored['fp'], negatives_returned=len(scored['negatives_returned']), extra=len(scored['extra']))
        if kind == 'exhaustive':
            row['f1'] = f1(scored['precision'], scored['recall'])
        if kind == 'exhaustive-empty':
            row['correct_empty'] = scored['precision'] == 1.0 and scored['recall'] == 1.0
    else:
        row.update(recall=0.0 if kind in ('exhaustive', 'lower_bound', 'uncertain') else None,
                   f1=0.0 if kind == 'exhaustive' else None,
                   correct_empty=False if kind == 'exhaustive-empty' else None)
    if question['stratum'] == 'two-hop-impact' and status == 'answered':
        row['hop_agreement'] = hop_agreement(label, answer)
    return row


def hop_agreement(label, answer):
    """Of the labelled required items the answer found, the share it put at the labelled shortest distance."""
    hops = answer.get('hops') if isinstance(answer.get('hops'), dict) else {}
    placed = {str(item): key for key, items in hops.items() if isinstance(items, list) for item in items}
    labelled = {i['item']: str(i.get('hop')) for i in label['items'] if i.get('hop') is not None}
    found = [item for item in labelled if any(item == a or a.endswith('/' + item) or item.endswith('/' + a) for a in placed)]
    if not found:
        return None
    right = sum(any((item == a or a.endswith('/' + item) or item.endswith('/' + a)) and placed[a] == labelled[item]
                    for a in placed) for item in found)
    return right / len(found)


def tokens(audit):
    """Research tokens, schemas included: every context token the model processed over the session plus output."""
    processed, output = audit.get('processed_context_tokens'), audit.get('terminal_output_tokens')
    return processed + output if type(processed) is int and type(output) is int else None


def mean(values):
    values = [v for v in values if v is not None]
    return sum(values) / len(values) if values else None


def bootstrap(questions, statistic, rng):
    """Percentile 95% interval of `statistic(question_ids)` over question resamples (cluster bootstrap)."""
    values = []
    for _ in range(RESAMPLES):
        sample = [rng.choice(questions) for _ in questions]
        value = statistic(sample)
        if value is not None:
            values.append(value)
    if len(values) < RESAMPLES // 2:
        return None
    values.sort()
    return [values[int(0.025 * len(values))], values[min(len(values) - 1, int(0.975 * len(values)))]]


def wilson(successes, n, z=1.959964):
    if not n:
        return None
    p = successes / n
    centre = (p + z * z / (2 * n)) / (1 + z * z / n)
    half = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / (1 + z * z / n)
    return [max(0.0, centre - half), min(1.0, centre + half)]


def load_session(run_dir, session):
    base = run_dir / session
    result = json.loads((base / 'result.json').read_text()) if (base / 'result.json').is_file() else None
    transcript = base / 'transcript.jsonl'
    events = recheck.read_events(transcript) if transcript.is_file() else []
    return result, events


def summarize(rows, questions, rng, baseline=None):
    by = {(r['arm'], r['question_id']): r for r in rows}
    qids = sorted({q['id'] for q in questions})
    out = {'arms': {}, 'paired': {}, 'criteria': {}}
    for arm in ARMS:
        arm_rows = [by[(arm, q)] for q in qids if (arm, q) in by]
        recheck_rows = [r['recheck'] for r in arm_rows if r.get('recheck')]
        claims = sum(r['answers_with_claims'] for r in recheck_rows)
        rechecked = sum(r['rechecked_answers'] for r in recheck_rows)
        out['arms'][arm] = {
            'sessions': len(arm_rows),
            'status': {s: sum(r['status'] == s for r in arm_rows) for s in sorted({r['status'] for r in arm_rows})},
            'f1_exhaustive_mean': mean(r['f1'] for r in arm_rows), 'f1_n': sum(r['f1'] is not None for r in arm_rows),
            'recall_lower_bound_mean': mean(r['recall'] for r in arm_rows if r['label'] == 'lower_bound'),
            'recall_lower_bound_n': sum(r['label'] == 'lower_bound' for r in arm_rows),
            'recall_uncertain_mean': mean(r['recall'] for r in arm_rows if r['label'] == 'uncertain'),
            'correct_empty': sum(bool(r['correct_empty']) for r in arm_rows if r['label'] == 'exhaustive-empty'),
            'correct_empty_n': sum(r['label'] == 'exhaustive-empty' for r in arm_rows),
            'hop_agreement_mean': mean(r.get('hop_agreement') for r in arm_rows),
            'tokens_median': statistics.median([r['tokens'] for r in arm_rows if r['tokens'] is not None]) if any(r['tokens'] is not None for r in arm_rows) else None,
            'tokens_n': sum(r['tokens'] is not None for r in arm_rows),
            'fixed_overhead_median': statistics.median([r['fixed_overhead'] for r in arm_rows if r['fixed_overhead'] is not None]) if any(r['fixed_overhead'] is not None for r in arm_rows) else None,
            'output_tokens_median': statistics.median([r['output_tokens'] for r in arm_rows if r['output_tokens'] is not None]) if any(r['output_tokens'] is not None for r in arm_rows) else None,
            'research_calls_mean': mean(r['research_calls'] for r in arm_rows),
            'knowcode_calls_mean': mean(r['knowcode_calls'] for r in arm_rows),
            'sessions_with_knowcode_call': sum((r['knowcode_calls'] or 0) > 0 for r in arm_rows),
            'budget_overruns': sum((r['overrun'] or 0) > 0 for r in arm_rows),
            'cost_usd': round(sum(r['cost_usd'] or 0 for r in arm_rows), 4),
            'knowcode_answers_with_claims': claims, 'knowcode_answers_rechecked': rechecked,
            'recheck_rate': rechecked / claims if claims else None,
            'recheck_rate_ci95': bootstrap(qids, lambda s, a=arm: ratio(by, a, s), rng),
        }
    for arm in KNOWCODE_ARMS:
        pairs = {}
        for name, field, kind in [('f1', 'f1', 'exhaustive'), ('recall_lower_bound', 'recall', 'lower_bound'),
                                  ('tokens', 'tokens', None)]:
            def diffs(sample, field=field, kind=kind, arm=arm):
                values = []
                for q in sample:
                    a, p = by.get((arm, q)), by.get(('plain', q))
                    if a and p and (kind is None or a['label'] == kind) and a[field] is not None and p[field] is not None:
                        values.append(a[field] - p[field])
                return values
            observed = diffs(qids)
            stat = (lambda s, d=diffs: (statistics.median(d(s)) if d(s) else None)) if name == 'tokens' else \
                   (lambda s, d=diffs: mean(d(s)))
            pairs[name] = {'n': len(observed), 'statistic': 'median' if name == 'tokens' else 'mean',
                           'difference': stat(qids), 'ci95': bootstrap(qids, stat, rng),
                           'distribution': sorted(observed)}
        out['paired'][arm + '-minus-plain'] = pairs
    named, unprompted = out['arms']['knowcode'], out['arms']['knowcode-unprompted']
    paired = out['paired']['knowcode-minus-plain']
    criterion1 = {'rule': 'named-arm pooled re-check rate <= 0.5 x the baseline run rate', 'rate': named['recheck_rate'],
                  'ci95': named['recheck_rate_ci95'], 'answers': named['knowcode_answers_with_claims']}
    if baseline is not None:
        base_rate = baseline['arms']['knowcode']['recheck_rate']
        criterion1.update(baseline_rate=base_rate, met=(named['recheck_rate'] is not None and base_rate is not None
                                                         and named['recheck_rate'] <= 0.5 * base_rate))
    else:
        criterion1['met'] = None
        criterion1['note'] = 'baseline run: this rate is the reference the final run must halve'
    f1d, recd = paired['f1']['difference'], paired['recall_lower_bound']['difference']
    out['criteria'] = {
        '1_recheck_rate_halved': criterion1,
        '2_f1_at_least_equal': {'rule': 'mean paired F1 difference (named - plain, non-empty exhaustive labels) >= 0 '
                                        'and mean paired recall difference (lower-bound labels) >= 0',
                                'f1_difference': f1d, 'f1_ci95': paired['f1']['ci95'], 'f1_n': paired['f1']['n'],
                                'recall_difference': recd, 'recall_ci95': paired['recall_lower_bound']['ci95'],
                                'recall_n': paired['recall_lower_bound']['n'],
                                'met': None if f1d is None or recd is None else (f1d >= 0 and recd >= 0)},
        '3_fewer_research_tokens': {'rule': 'median paired difference in session tokens (named - plain) < 0',
                                    'difference': paired['tokens']['difference'], 'ci95': paired['tokens']['ci95'],
                                    'n': paired['tokens']['n'],
                                    'met': None if paired['tokens']['difference'] is None else paired['tokens']['difference'] < 0},
        '4_unprompted_use': {'rule': 'KnowCode called in >= 20 of 24 unprompted sessions',
                             'sessions_with_call': unprompted['sessions_with_knowcode_call'], 'sessions': unprompted['sessions'],
                             'ci95': wilson(unprompted['sessions_with_knowcode_call'], unprompted['sessions']),
                             'calls_per_session_mean': unprompted['knowcode_calls_mean'],
                             'met': (unprompted['sessions_with_knowcode_call'] / unprompted['sessions'] >= UNPROMPTED_THRESHOLD)
                             if unprompted['sessions'] else None},
    }
    return out


def ratio(by, arm, sample):
    claims = rechecked = 0
    for q in sample:
        row = by.get((arm, q))
        if row and row.get('recheck'):
            claims += row['recheck']['answers_with_claims']
            rechecked += row['recheck']['rechecked_answers']
    return rechecked / claims if claims else None


def score(freeze_path, run_dir, baseline=None):
    freeze = json.loads(Path(freeze_path).read_text())
    base = Path(freeze_path).resolve().parent

    def read(name):
        entry = freeze['files'][name]
        path = Path(entry['path']) if Path(entry['path']).is_absolute() else base / entry['path']
        data = path.read_bytes()
        if hashlib.sha256(data).hexdigest() != entry['sha256']:
            raise ValueError('frozen input hash differs: ' + name)
        return data

    questions = [json.loads(l) for l in read('questions').decode().splitlines() if l.strip()]
    labels = {r['id']: r for r in (json.loads(l) for l in read('labels').decode().splitlines() if l.strip())}
    order = [json.loads(l) for l in read('order').decode().splitlines() if l.strip()]
    scorer = load('public_study_neutral_score', ROOT / 'scripts/eval/neutral/score.py')
    qmap = {q['id']: q for q in questions}
    rows = []
    for entry in order:
        result, events = load_session(Path(run_dir), entry['session'])
        audit = (result or {}).get('audit') or {}
        answer = (result or {}).get('answer')
        q = qmap[entry['question_id']]
        row = {'session': entry['session'], 'ordinal': entry['ordinal'], 'arm': entry['arm'], 'question_id': q['id'],
               'stratum': q['stratum'], 'integrity_issues': len((result or {}).get('integrity_issues', [])) if result else None,
               'transport_integrity': ((result or {}).get('transport') or {}).get('transport_integrity'),
               'research_calls': audit.get('research_calls'), 'knowcode_calls': audit.get('knowcode_calls'),
               'overrun': audit.get('research_call_overrun'), 'tokens': tokens(audit),
               'fixed_overhead': audit.get('fixed_overhead_context'), 'context_at_answer': audit.get('context_at_answer'),
               'output_tokens': audit.get('terminal_output_tokens'), 'cost_usd': audit.get('reported_cost_usd'),
               'policy_flags': audit.get('policy_flags', [])}
        row.update(score_session(scorer, q, labels[q['id']], answer if result else None))
        if entry['arm'] in KNOWCODE_ARMS and events:
            found = recheck.detect(events)
            row['recheck'] = {k: found[k] for k in ('knowcode_answers', 'knowcode_errors', 'answers_with_claims',
                                                    'rechecked_answers', 'plain_calls_after_first_answer', 'rechecking_calls')}
            row['recheck_sequences'] = [{'tool': a['tool'], 'rechecked': a['rechecked'],
                                         'by': [{'tool': b['tool'], 'reasons': [reason_kind(x) for x in b['reasons']]} for b in a['by']]}
                                        for a in found['answers']]
        rows.append(row)
    rng = random.Random(SEED)
    summary = summarize(rows, questions, rng, baseline)
    kept = [q for q in questions if q['id'] not in SENSITIVITY_EXCLUDED]
    sensitivity = summarize([r for r in rows if r['question_id'] not in SENSITIVITY_EXCLUDED], kept,
                            random.Random(SEED), baseline.get('sensitivity') if baseline else None)
    return {'format': 1, 'freeze_sha256': hashlib.sha256(Path(freeze_path).read_bytes()).hexdigest(),
            'bootstrap': {'seed': SEED, 'resamples': RESAMPLES, 'unit': 'question'},
            'sessions_scheduled': len(order), 'sessions_with_result': sum(r['status'] != 'missing' for r in rows),
            **summary, 'sensitivity': dict(summary_note='Bitwarden HTTP questions 001 and 003 excluded (labels depend on '
                                                         'the separately licensed tree left out of the export)', **sensitivity),
            'sessions': rows}


def reason_kind(reason):
    """Reason class without file or symbol text, so published sequences carry no source paths beyond the cohort's."""
    return reason.split(' ', 1)[0] if not reason.startswith('repeated') else 'repeated'


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--freeze', type=Path, required=True)
    parser.add_argument('--runs', type=Path, required=True)
    parser.add_argument('--baseline', type=Path)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    baseline = json.loads(args.baseline.read_text()) if args.baseline else None
    summary = score(args.freeze, args.runs, baseline)
    if args.out.exists():
        raise SystemExit('output exists; scored results are never overwritten')
    args.out.write_text(json.dumps(summary, indent=2, sort_keys=True) + '\n')
    print(json.dumps(summary['criteria'], indent=2, sort_keys=True))


if __name__ == '__main__':
    main()
