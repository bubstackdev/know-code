#!/usr/bin/env python3
"""Prepare a source-verified draft paired schedule, without starting agents or freezing the study.

Labels never enter this helper or its operator question file. Independent label review,
final configuration, isolated session views and a separate freeze are still required.
"""
from __future__ import annotations

import argparse
from collections import Counter
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import tempfile

ROOT = Path(__file__).resolve().parent.parent
SEED = 'knowcode-public-v1-run-order-2026-10-16'
# The plain arm; KnowCode named in the brief; KnowCode available but not named (unprompted use).
ARMS = ('plain', 'knowcode', 'knowcode-unprompted')
KNOWCODE_ARMS = ('knowcode', 'knowcode-unprompted')
TASKS = {'http-cross-repo': ('http-callers', 'files'),
         'di-implementations': ('implementations', 'file_lines'),
         'two-hop-impact': ('impact', 'file_lines'),
         'definition-lookup': ('locate', 'file_lines')}
PRODUCERS = ('scripts/plan-public-eval.py', 'scripts/census-public-eval.py',
             'scripts/eval/neutral/schema.py')


def digest(data):
    return hashlib.sha256(data).hexdigest()


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def check_questions(questions, manifest, schema):
    pins = {repo['name']: repo['commit'] for family in manifest['families'] for repo in family['repos']}
    if len(pins) != sum(len(f['repos']) for f in manifest['families']):
        raise ValueError('duplicate cohort repository')
    ids = set()
    for q in questions:
        errors = schema.validate_question(q)
        if errors:
            raise ValueError('; '.join(errors))
        # A question file is handed to an operator. Extra fields could accidentally carry labels.
        if set(q) != set(schema.QUESTION_FIELDS):
            raise ValueError('operator question has unexpected fields; keep labels and review metadata separate')
        if not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_-]{0,127}', q['id']) or q['id'] in ids:
            raise ValueError('invalid or duplicate question id')
        ids.add(q['id'])
        if q['source'] != 'structural' or q['stratum'] not in TASKS:
            raise ValueError('question is outside the public source-only task population')
        if (q['type'], q['answer_kind']) != TASKS[q['stratum']]:
            raise ValueError('question type or answer format differs from public task definition')
        if len(set(q['repos'])) != len(q['repos']) or not q['question'].strip():
            raise ValueError('duplicate question repository or empty prompt')
        expected = {repo: pins[repo] for repo in q['repos'] if repo in pins}
        if len(expected) != len(q['repos']) or q['commit'] != expected or q['index_state'] != expected:
            raise ValueError('question source/index pins differ from prepared cohort')
    if not questions:
        raise ValueError('no questions')


def schedule(questions, seed):
    if not seed.strip():
        raise ValueError('empty order seed')
    orders = {}
    for arm in ARMS:
        orders[arm] = sorted((q['id'] for q in questions),
                            key=lambda qid: (digest((seed+'\0'+arm+'\0'+qid).encode()), qid))
    start = int(digest((seed+'\0first-arm').encode()), 16) % len(ARMS)
    arms = ARMS[start:] + ARMS[:start]
    rows = []
    for index in range(len(questions)):
        for arm in arms:
            rows.append({'ordinal': len(rows)+1, 'arm': arm, 'question_id': orders[arm][index],
                         'session': arm+'-'+orders[arm][index]})
    return rows


def check_config(config):
    """Only record an explicitly supplied common agent configuration; never choose or launch a model."""
    required = {'model', 'effort', 'client', 'client_version', 'research_call_budget',
                'knowcode_sha256', 'knowcode_profile'}
    if not isinstance(config, dict) or set(config) != required:
        raise ValueError('run configuration must have exactly: '+', '.join(sorted(required)))
    if any(not isinstance(config[k], str) or not config[k].strip() for k in required-{'research_call_budget'}):
        raise ValueError('run configuration text fields must be nonempty')
    if type(config['research_call_budget']) is not int or config['research_call_budget'] <= 0:
        raise ValueError('research_call_budget must be a positive integer')
    if not re.fullmatch('[0-9a-f]{64}', config['knowcode_sha256']):
        raise ValueError('KnowCode binary hash must be a SHA-256 digest')


def plan(manifest_path, source, questions_path, out, seed=SEED, config_path=None):
    source = source.resolve(strict=True)
    out = out.absolute()
    inputs = [manifest_path, source/'source-receipt.json', questions_path]
    if config_path is not None:
        inputs.append(config_path)
    resolved = out.resolve()
    if out.is_symlink() or resolved.is_relative_to(ROOT) or resolved.is_relative_to(source):
        raise ValueError('plan output must be outside checkout and prepared source')
    if any(resolved.is_relative_to(p.resolve().parent) for p in inputs if p != manifest_path):
        raise ValueError('plan output must be outside input evidence directories')
    if out.exists():
        raise ValueError('output must be new; existing evidence is retained')
    captured = {p: p.read_bytes() for p in inputs}
    producers = {name: digest((ROOT/name).read_bytes()) for name in PRODUCERS}
    manifest = json.loads(captured[manifest_path])
    questions = [json.loads(line) for line in captured[questions_path].decode().splitlines() if line.strip()]
    schema = load('public_plan_schema', ROOT/'scripts/eval/neutral/schema.py')
    check_questions(questions, manifest, schema)
    config = json.loads(captured[config_path]) if config_path is not None else None
    if config is not None:
        check_config(config)
    census = load('public_plan_census', ROOT/'scripts/census-public-eval.py')
    with tempfile.TemporaryDirectory(prefix='knowcode-plan-verify-') as temp:
        verified = census.census(manifest_path, source, Path(temp)/'census')
    rows = schedule(questions, seed)
    encoded = ''.join(json.dumps(row, sort_keys=True)+'\n' for row in rows).encode()
    operator = ''.join(json.dumps(q, sort_keys=True)+'\n' for q in sorted(questions, key=lambda q: q['id'])).encode()
    result = {'format': 1, 'stage': 'draft paired schedule; not frozen; no agents or scored runs',
              'scored_runs': 0, 'ready_to_run': False, 'seed': seed,
              'manifest_sha256': digest(captured[manifest_path]),
              'source_receipt_sha256': digest(captured[source/'source-receipt.json']),
              'verified_source_files': verified['files'], 'source_files_sha256': verified['files_sha256'],
              'questions_input_sha256': digest(captured[questions_path]),
              'operator_questions_sha256': digest(operator), 'order_sha256': digest(encoded),
              'config_sha256': digest(captured[config_path]) if config_path is not None else None,
              'config': config, 'producer_hashes': producers,
              'questions': len(questions), 'sessions': len(rows),
              'strata': dict(sorted(Counter(q['stratum'] for q in questions).items())),
              'arms': {'plain': ['grep', 'find', 'read'], 'knowcode': ['grep', 'find', 'read', 'knowcode-mcp'],
                       'knowcode-unprompted': ['grep', 'find', 'read', 'knowcode-mcp (not named in the brief)']},
              'order_rule': 'Each arm sorts task IDs by SHA256(seed+NUL+arm+NUL+ID), ID as tie-breaker. Rotate arms in ARMS order; starting arm uses SHA256(seed+NUL+first-arm) modulo the number of arms.',
              'remaining_gates': ['accepted subject/sampling linkage and all rejections',
                                  'complete source labels and independent review with adjudications',
                                  'final common model/client/budget and actual binary/schema verification',
                                  'separate source-identical isolated sessions with labels, memory and unrelated plugins inaccessible',
                                  'hash-bound freeze of sampling, questions, labels, run order, configuration and runner'],
              'timing_policy': 'Record load/concurrency. Wall-clock comparisons require established quiet conditions; this plan asserts none.',
              'isolation_policy': 'Fresh sessions and separate source copies for both arms. Never expose labels, personal memory, unrelated MCP servers or upstream agent instructions. No source edits, notes or network. Controls only evaluation-owned processes.'}

    def unchanged():
        if any(p.read_bytes() != data for p, data in captured.items()):
            raise ValueError('input evidence changed during planning; no plan published')
        if any(digest((ROOT/name).read_bytes()) != value for name, value in producers.items()):
            raise ValueError('plan producer changed during verification; no plan published')
    unchanged()
    out.parent.mkdir(parents=True, exist_ok=True)
    # Keep incomplete output private and temporary; never overwrite an earlier plan.
    with tempfile.TemporaryDirectory(prefix='.knowcode-plan-', dir=out.parent) as temp:
        staging = Path(temp)/'plan'
        staging.mkdir(mode=0o700)
        (staging/'questions.jsonl').write_bytes(operator)
        (staging/'order.jsonl').write_bytes(encoded)
        (staging/'plan.json').write_text(json.dumps(result, indent=2)+'\n')
        unchanged()
        if out.exists() or out.is_symlink():
            raise ValueError('output appeared during verification; existing evidence is retained')
        os.rename(staging, out)
    return result


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--manifest', type=Path, default=ROOT/'eval/public-v1/manifest.json')
    parser.add_argument('--source', type=Path, required=True)
    parser.add_argument('--questions', type=Path, required=True)
    parser.add_argument('--config', type=Path)
    parser.add_argument('--seed', default=SEED)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    result = plan(args.manifest, args.source, args.questions, args.out, args.seed, args.config)
    print(json.dumps({k: result[k] for k in ('questions', 'sessions', 'verified_source_files', 'ready_to_run', 'scored_runs')}))
