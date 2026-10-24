#!/usr/bin/env python3
"""The public-v2 study (eval/public-v2/protocol.md): its frozen study, runner and scorer.

    public_v2.py run --freeze freeze.json --source SOURCE --plan-root PLAN_ROOT --out RUNS --ledger LEDGER \
        --client CLIENT --knowcode KNOWCODE --runs b1,b2 --cap USD [--workers 3] [--sessions DIR]
    public_v2.py score --freeze freeze.json --runs-dir RUNS --runs b1,b2 [--baseline summary.json] --out summary.json

`Study` gives session_runner what public-v1's FrozenStudy gives it, for a public-v2 freeze. The runner runs the frozen
schedule's sessions for the named runs in order, through session_runner, with a spend ledger, a cap and two tripwires:
it stops when a session's client refuses a Bash command that doesn't name the KnowCode binary, and when more than 3
sessions fail on contract or transport. The scorer computes the five criteria for one build's runs; with the 0.2.0
baseline's summary it also decides criterion 1. Nothing here starts a model except through session_runner.
"""
from __future__ import annotations

import argparse
from collections import Counter
import concurrent.futures
import hashlib
import json
import math
import os
from pathlib import Path
import random
import shutil
import stat
import statistics
import time

import recheck
import recheck_cost
from frozen_study import ROOT, load
from public_transcript import load_json

FORMAT = 'knowcode-public-v2-freeze-1'
FILES = {'manifest', 'source_receipt', 'plan', 'questions', 'labels', 'order', 'config', 'protocol'}
PRODUCERS = {'scripts/eval/public_v2.py', 'scripts/eval/session_runner.py', 'scripts/eval/bare_client.py',
             'scripts/eval/macos_sandbox.py', 'scripts/eval/owned_process.py', 'scripts/eval/model_relay.py',
             'scripts/eval/public_transcript.py', 'scripts/eval/recheck.py', 'scripts/eval/recheck_cost.py',
             'scripts/eval/neutral/score.py', 'scripts/eval/neutral/schema.py', 'scripts/eval/frozen_study.py',
             'scripts/prepare-public-eval-session.py', 'scripts/prepare-public-eval-index.py',
             'scripts/census-public-eval.py'}
ARMS = ('plain', 'knowcode', 'knowcode-unprompted')
STRATA = ('definition-lookup', 'di-implementations', 'http-cross-repo', 'two-hop-impact')
SEED = 'knowcode-public-v2-bootstrap-2026-10-23'
RESAMPLES = 10000
UNPROMPTED_SHARE = 20 / 24  # the plan's "at least 20 of 24", as a share of every unprompted session
CONTRACT = ('invalid answer contract', 'answer is not one complete JSON object', 'client result has no answer text')


def digest(data):
    return hashlib.sha256(data).hexdigest()


def jsonl(data):
    return [load_json(line) for line in data.decode().splitlines() if line.strip()]


class Study:
    """A public-v2 freeze, checked from content: inputs and runner implementation by hash, plan bindings, bank."""

    def __init__(self, path):
        self.path = Path(path).resolve(strict=True)
        self.raw = self.path.read_bytes()
        self.freeze_sha256 = digest(self.raw)
        spec = load_json(self.raw)
        if spec.get('format') != FORMAT or set(spec.get('files', {})) != FILES:
            raise ValueError('complete public-v2 freeze file inventory required')
        self.captured, self.inputs, self.hashes = {self.path: self.raw}, {}, {}
        for name, entry in spec['files'].items():
            candidate = Path(entry['path'])
            candidate = candidate if candidate.is_absolute() else self.path.parent / candidate
            if candidate.is_symlink() or not candidate.is_file():
                raise ValueError('regular nonsymlink frozen input required')
            candidate = candidate.resolve(strict=True)
            data = candidate.read_bytes()
            if digest(data) != entry['sha256']:
                raise ValueError('frozen input hash differs: ' + name)
            self.inputs[name], self.hashes[name], self.captured[candidate] = candidate, entry['sha256'], data
        producers = spec.get('producers')
        if not isinstance(producers, dict) or not PRODUCERS.issubset(producers):
            raise ValueError('complete runner implementation hash inventory required')
        for name, expected in producers.items():
            candidate = ROOT / name
            if candidate.is_symlink() or not candidate.is_file() or digest(candidate.read_bytes()) != expected:
                raise ValueError('runner implementation differs: ' + name)
            self.captured[candidate.resolve()] = candidate.read_bytes()
        self.client_sha256 = spec.get('client_sha256')
        self.process_timeout_seconds = spec.get('process_timeout_seconds')
        self.runs = spec.get('runs')
        if not isinstance(self.runs, dict) or not self.runs:
            raise ValueError('the freeze names its runs')
        self.manifest = self.read('manifest')
        self.plan = self.read('plan')
        self.config = self.read('config')
        self.receipt = self.read('source_receipt')
        self.questions = jsonl(self.captured[self.inputs['questions']])
        self.labels = jsonl(self.captured[self.inputs['labels']])
        self.order = jsonl(self.captured[self.inputs['order']])
        schema = load('public_v2_schema', ROOT / 'scripts/eval/neutral/schema.py')
        errors = schema.validate_bank(self.questions, self.labels)
        if errors:
            raise ValueError('complete valid label bank required: ' + '; '.join(errors[:3]))
        if Counter(q['stratum'] for q in self.questions) != {s: 10 for s in STRATA}:
            raise ValueError('forty questions, ten per task type, required')
        bindings = {'manifest_sha256': 'manifest', 'source_receipt_sha256': 'source_receipt', 'order_sha256': 'order',
                    'operator_questions_sha256': 'questions', 'config_sha256': 'config'}
        if any(self.plan.get(k) != self.hashes[v] for k, v in bindings.items()):
            raise ValueError('plan differs from frozen inputs')
        qids = {q['id'] for q in self.questions}
        for row in self.order:
            if row.get('arm') not in ARMS or row.get('question_id') not in qids or row.get('run') not in self.runs:
                raise ValueError('order row outside the frozen questions, arms and runs')
        expected = {(run, arm, q) for run in self.runs for arm in ARMS for q in qids}
        if {(r['run'], r['arm'], r['question_id']) for r in self.order} != expected or len(self.order) != len(expected):
            raise ValueError('every run must hold every question in every arm exactly once')
        self.unchanged()

    def read(self, name):
        return load_json(self.captured[self.inputs[name]])

    def unchanged(self):
        if any(path.read_bytes() != data for path, data in self.captured.items()):
            raise ValueError('frozen study input or implementation changed')

    def binaries(self, client, knowcode):
        for path, expected in [(Path(client), self.client_sha256), (Path(knowcode), self.config['knowcode_sha256'])]:
            if path.is_symlink() or not path.is_file() or digest(path.read_bytes()) != expected:
                raise ValueError('executable differs from frozen configuration')


# --- runner -------------------------------------------------------------------------------------------------------

def remove_tree(path):
    for root, dirs, files in os.walk(path):
        for name in dirs + files:
            p = Path(root) / name
            if not p.is_symlink():
                p.chmod(p.stat().st_mode | stat.S_IWUSR | stat.S_IXUSR)
    Path(path).chmod(0o700)
    shutil.rmtree(path)


def run(args):
    import session_runner
    prepare_session = load('public_v2_session', ROOT / 'scripts/prepare-public-eval-session.py')
    study = Study(args.freeze)
    runs = args.runs.split(',')
    if not set(runs) <= set(study.runs):
        raise SystemExit('unknown run')
    ledger, out_root = Path(args.ledger), Path(args.out)
    out_root.mkdir(parents=True, exist_ok=True)
    sessions = Path(args.sessions)

    def rows():
        return [json.loads(l) for l in ledger.read_text().splitlines() if l.strip()] if ledger.exists() else []

    def log(row):
        with ledger.open('a') as stream:
            stream.write(json.dumps(dict(row, time=time.strftime('%Y-%m-%dT%H:%M:%S'))) + '\n')

    def spent():
        return sum(r['reported_cost_usd'] for r in rows() if r.get('kind') == 'session' and isinstance(r.get('reported_cost_usd'), (int, float)))

    def one(row):
        out = out_root / row['run'] / row['session']
        if (out / 'result.json').exists() or (out / 'failure.json').exists():
            return None
        out.parent.mkdir(parents=True, exist_ok=True)
        session = sessions / row['session']
        if session.exists():
            remove_tree(session)
        session.parent.mkdir(parents=True, exist_ok=True)
        prepare_session.prepare(study.inputs['manifest'], Path(args.source), Path(args.plan_root), row['session'], session)
        cost = None
        try:
            result = session_runner.execute(args.freeze, session, args.client, args.knowcode, out,
                                            api_key=os.environ['ANTHROPIC_API_KEY'], git_toolchain=Path(args.git_toolchain))
            audit = result['audit']
            named, other = refused_bash(recheck.read_events(out / 'transcript.jsonl'), study.config['knowcode_binary_name'])
            line = dict(kind='session', session=row['session'], run=row['run'], status=result['answer']['status'],
                        reported_cost_usd=audit.get('reported_cost_usd'), knowcode_calls=audit['knowcode_calls'],
                        transport_integrity=result['transport']['transport_integrity'],
                        answer_issues=audit.get('answer_issues'), refused_naming_binary=named, refused_other=other)
        except Exception as exc:  # the session's own evidence is kept by session_runner
            line = dict(kind='session', session=row['session'], run=row['run'], status='error', reported_cost_usd=cost,
                        error=type(exc).__name__)
        log(line)
        for name in ['source', 'control/index', 'control/kc-cache']:
            if (session / name).exists():
                remove_tree(session / name)
        return line

    order = [r for run_name in runs for r in study.order if r['run'] == run_name]
    pending = [r for r in order if not any((out_root / r['run'] / r['session'] / n).exists() for n in ('result.json', 'failure.json'))]
    while pending:
        done = [r for r in rows() if r.get('kind') == 'session']
        if any(r.get('refused_other') for r in done):
            log(dict(kind='stop', reason='tripwire: a Bash command not naming the KnowCode binary was refused'))
            print(json.dumps(dict(stopped='tripwire'))); return
        failed = [r['session'] for r in done if r['status'] == 'error' or r.get('transport_integrity') is False
                  or any(i in CONTRACT for i in r.get('answer_issues') or [])]
        if len(failed) > 3:
            log(dict(kind='stop', reason='more than 3 failed sessions', sessions=failed))
            print(json.dumps(dict(stopped='failures', sessions=failed))); return
        costs = [r['reported_cost_usd'] for r in done if isinstance(r.get('reported_cost_usd'), (int, float))]
        estimate = max(0.3, max(costs) if costs else 0.3) * min(args.workers, len(pending))
        if spent() + estimate > args.cap:
            log(dict(kind='stop', reason='next batch would cross the cap', spent=spent(), estimate=estimate, cap=args.cap))
            print(json.dumps(dict(stopped='cap', spent=spent()))); return
        current, pending = pending[:args.workers], pending[args.workers:]
        with concurrent.futures.ThreadPoolExecutor(args.workers) as pool:
            for line in pool.map(one, current):
                if line:
                    print(json.dumps({k: line.get(k) for k in ('session', 'status', 'reported_cost_usd', 'knowcode_calls', 'refused_other')}), flush=True)
    print(json.dumps(dict(done=True, spent=round(spent(), 4))))


# --- scorer -------------------------------------------------------------------------------------------------------

def label_kind(label):
    if label['completeness'] == 'exhaustive':
        return 'exhaustive' if label['items'] else 'exhaustive-empty'
    return label['completeness']


def score_session(scorer, question, label, result, events, binary_name, knowcode_arm=True):
    answer = (result or {}).get('answer') or {}
    audit = (result or {}).get('audit') or {}
    status = answer.get('status', 'error') if result else 'missing'
    kind = label_kind(label)
    row = dict(status=status, label=kind, f1=None, recall=None, correct_empty=None)
    if status == 'answered':
        s = scorer.score_one(label, answer.get('items', []), question.get('repos', ()))
        p, r = s['precision'], s['recall']
        row.update(recall=r, precision=p, undecided_returned=len(s.get('undecided_returned', [])))
        if kind == 'exhaustive':
            row['f1'] = 2 * p * r / (p + r) if p is not None and p + r else 0.0
        if kind == 'exhaustive-empty':
            row['correct_empty'] = p == 1.0 and r == 1.0
    else:
        row.update(recall=0.0 if kind in ('exhaustive', 'lower_bound', 'uncertain') else None,
                   f1=0.0 if kind == 'exhaustive' else None, correct_empty=False if kind == 'exhaustive-empty' else None)
    usage = audit.get('terminal_usage') or {}
    processed, output = audit.get('processed_context_tokens'), audit.get('terminal_output_tokens')
    row.update(tokens=processed + output if processed is not None and output is not None else None,
               cache_read=usage.get('cache_read_input_tokens'),
               uncached_input=(usage.get('input_tokens') or 0) + (usage.get('cache_creation_input_tokens') or 0) if usage else None,
               fixed_overhead=audit.get('fixed_overhead_context'), research_calls=audit.get('research_calls'),
               knowcode_calls=audit.get('knowcode_calls'), cost_usd=audit.get('reported_cost_usd'),
               transport_integrity=((result or {}).get('transport') or {}).get('transport_integrity'))
    row['refused_naming_binary'], row['refused_other'] = refused_bash(events, binary_name)
    if events and knowcode_arm:
        cost = recheck_cost.cost(events, answer.get('items', []))
        found = recheck.detect(events)
        row.update(recheck_tokens=cost['recheck_tokens'], recheck_calls=cost['recheck_calls'],
                   changed=[dict(call=c['call'], tool=c['tool'], added=c['added']) for c in cost['calls'] if c['changed']],
                   answers_with_claims=found['answers_with_claims'], rechecked_answers=found['rechecked_answers'])
    return row


def refused_bash(events, binary_name):
    """Bash calls the client refused: (naming the KnowCode binary, anything else)."""
    calls, named, other = {}, 0, 0
    for event in events:
        message = event.get('message') if isinstance(event, dict) else None
        if not isinstance(message, dict) or not isinstance(message.get('content'), list):
            continue
        for block in message['content']:
            if isinstance(block, dict) and block.get('type') == 'tool_use' and block.get('name') == 'Bash':
                calls[block.get('id')] = (block.get('input') or {}).get('command', '')
            elif isinstance(block, dict) and block.get('type') == 'tool_result' and block.get('tool_use_id') in calls:
                if 'Permission to use Bash' in recheck.result_text(block):
                    named, other = (named + 1, other) if binary_name in calls[block['tool_use_id']] else (named, other + 1)
    return named, other


def mean(values):
    values = [v for v in values if v is not None]
    return sum(values) / len(values) if values else None


def median(values):
    values = [v for v in values if v is not None]
    return statistics.median(values) if values else None


def bootstrap(qids, statistic, rng):
    values = []
    for _ in range(RESAMPLES):
        value = statistic([rng.choice(qids) for _ in qids])
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


def summarize(rows, questions, runs, baseline=None):
    """The five criteria for one build's runs; rows are keyed by (run, arm, question id)."""
    qids = sorted(q['id'] for q in questions)
    stratum = {q['id']: q['stratum'] for q in questions}

    def get(run, arm, q):
        return rows.get((run, arm, q), {})

    def c1(sample):
        return mean([get(r, 'knowcode', q).get('recheck_tokens') for q in sample for r in runs])

    def paired(field, kinds, sample):
        return [get(r, 'knowcode', q)[field] - get(r, 'plain', q)[field] for q in sample for r in runs
                if get(r, 'knowcode', q).get('label') in kinds and get(r, 'knowcode', q).get(field) is not None
                and get(r, 'plain', q).get(field) is not None]

    rng = random.Random(SEED)
    c1_value = c1(qids)
    criteria = {}
    entry = dict(rule='mean re-check tokens per named-arm session <= 0.5 x the 0.2.0 baseline on the same questions',
                 value=c1_value, ci95=bootstrap(qids, c1, rng), sessions=len(qids) * len(runs))
    if baseline is not None:
        base = baseline['criteria']['1_recheck_cost_halved']['value']
        entry.update(baseline_value=base, ratio=c1_value / base if base else None,
                     met=None if c1_value is None or not base else c1_value <= 0.5 * base)
    else:
        entry['met'] = None
        entry['note'] = 'scored without a baseline: this is the reference value'
    criteria['1_recheck_cost_halved'] = entry
    changed = [dict(run=r, arm=a, question=q, calls=row['changed']) for (r, a, q), row in sorted(rows.items())
               if r in runs and a != 'plain' and row.get('changed')]
    criteria['2_rechecks_that_changed_the_answer'] = dict(rule='descriptive: each is listed as a KnowCode failure',
                                                          named=sum(len(c['calls']) for c in changed if c['arm'] == 'knowcode'),
                                                          unprompted=sum(len(c['calls']) for c in changed if c['arm'] != 'knowcode'),
                                                          sessions=changed)
    f1 = mean(paired('f1', ('exhaustive',), qids))
    recall = mean(paired('recall', ('lower_bound',), qids))
    criteria['3_f1_and_recall_at_least_equal'] = dict(
        rule='mean paired F1 difference (named - plain, exhaustive labels with items) >= 0 and mean paired recall '
             'difference (lower-bound labels) >= 0', f1_difference=f1,
        f1_ci95=bootstrap(qids, lambda s: mean(paired('f1', ('exhaustive',), s)), rng),
        f1_n=len(paired('f1', ('exhaustive',), qids)), recall_difference=recall,
        recall_ci95=bootstrap(qids, lambda s: mean(paired('recall', ('lower_bound',), s)), rng),
        recall_n=len(paired('recall', ('lower_bound',), qids)),
        met=None if f1 is None else f1 >= 0 and (recall is None or recall >= 0))
    kinds = ('exhaustive', 'exhaustive-empty', 'lower_bound', 'uncertain')
    tokens = median(paired('tokens', kinds, qids))
    criteria['4_research_tokens_at_most_equal'] = dict(
        rule='median paired difference in session tokens (named - plain) <= 0', difference=tokens,
        ci95=bootstrap(qids, lambda s: median(paired('tokens', kinds, s)), rng), n=len(paired('tokens', kinds, qids)),
        met=None if tokens is None else tokens <= 0)
    unprompted = [get(r, 'knowcode-unprompted', q) for q in qids for r in runs]
    used = sum((u.get('knowcode_calls') or 0) > 0 for u in unprompted)
    criteria['5_unprompted_use'] = dict(
        rule='KnowCode called in at least 20 of every 24 unprompted sessions (share >= 20/24)', sessions=len(unprompted),
        sessions_with_call=used, share=used / len(unprompted) if unprompted else None, ci95=wilson(used, len(unprompted)),
        met=bool(unprompted) and used >= UNPROMPTED_SHARE * len(unprompted))
    arms = {}
    for arm in ARMS:
        rs = [get(r, arm, q) for q in qids for r in runs]
        arms[arm] = dict(sessions=len(rs), statuses=dict(Counter(x.get('status', 'missing') for x in rs)),
                         tokens_median=median([x.get('tokens') for x in rs]), cache_read_median=median([x.get('cache_read') for x in rs]),
                         uncached_input_median=median([x.get('uncached_input') for x in rs]),
                         fixed_overhead_median=median([x.get('fixed_overhead') for x in rs]),
                         research_calls_mean=mean([x.get('research_calls') for x in rs]),
                         knowcode_calls_mean=mean([x.get('knowcode_calls') for x in rs]), cost_usd=sum(x.get('cost_usd') or 0 for x in rs),
                         refused_naming_binary=sum(x.get('refused_naming_binary') or 0 for x in rs),
                         refused_other=sum(x.get('refused_other') or 0 for x in rs))
        if arm != 'plain':
            claims = sum(x.get('answers_with_claims') or 0 for x in rs)
            arms[arm].update(recheck_tokens_mean=mean([x.get('recheck_tokens') for x in rs]),
                             recheck_rate=sum(x.get('rechecked_answers') or 0 for x in rs) / claims if claims else None,

                             use_by_task_type={s: dict(sessions=sum(stratum[q] == s for q in qids for _ in runs),
                                                       with_call=sum(stratum[q] == s and (get(r, arm, q).get('knowcode_calls') or 0) > 0
                                                                     for q in qids for r in runs)) for s in STRATA})
    return dict(criteria=criteria, arms=arms)


def score(args):
    study = Study(args.freeze)
    runs = args.runs.split(',')
    scorer = load('public_v2_neutral_score', ROOT / 'scripts/eval/neutral/score.py')
    questions = {q['id']: q for q in study.questions}
    labels = {l['id']: l for l in study.labels}
    rows = {}
    for row in study.order:
        if row['run'] not in runs:
            continue
        base = Path(args.runs_dir) / row['run'] / row['session']
        result = json.loads((base / 'result.json').read_text()) if (base / 'result.json').is_file() else None
        events = recheck.read_events(base / 'transcript.jsonl') if (base / 'transcript.jsonl').is_file() else []
        rows[(row['run'], row['arm'], row['question_id'])] = score_session(
            scorer, questions[row['question_id']], labels[row['question_id']], result, events,
            study.config['knowcode_binary_name'], row['arm'] != 'plain')
    baseline = json.loads(Path(args.baseline).read_text()) if args.baseline else None
    summary = dict(format='public-v2-summary-1', freeze_sha256=study.freeze_sha256, runs=runs,
                   bootstrap=dict(seed=SEED, resamples=RESAMPLES, unit='question'), **summarize(rows, study.questions, runs, baseline),
                   sessions=[dict(run=r, arm=a, question_id=q, **v) for (r, a, q), v in sorted(rows.items())])
    out = Path(args.out)
    if out.exists():
        raise SystemExit('output exists; scored results are never overwritten')
    out.write_text(json.dumps(summary, indent=2, sort_keys=True) + '\n')
    print(json.dumps(summary['criteria'], indent=2, sort_keys=True)[:4000])


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest='command', required=True)
    r = sub.add_parser('run')
    for name in ['freeze', 'source', 'plan-root', 'out', 'ledger', 'client', 'knowcode', 'runs', 'sessions']:
        r.add_argument('--' + name, required=True)
    r.add_argument('--cap', type=float, required=True)
    r.add_argument('--workers', type=int, default=3)
    r.add_argument('--git-toolchain', default='/Library/Developer/CommandLineTools')
    s = sub.add_parser('score')
    for name in ['freeze', 'runs-dir', 'runs', 'out']:
        s.add_argument('--' + name, required=True)
    s.add_argument('--baseline')
    args = parser.parse_args()
    run(args) if args.command == 'run' else score(args)


if __name__ == '__main__':
    main()
