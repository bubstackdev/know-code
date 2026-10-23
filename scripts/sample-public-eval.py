#!/usr/bin/env python3
"""Rank source-only public evaluation subjects; provisional picks require source review.

No builds, source execution, indexing, KnowCode queries or model calls. public-v1's seed, quotas and server
repositories are the defaults; a later bank passes its own with --plan, a JSON file with `seed`, `quotas`
({task: {family: n}}), `server_repos` and `exclude`: earlier banks' subjects ({task, repo, file, key}). A candidate is
excluded when an earlier subject has its task and either its file or its symbol name, so a new bank asks nothing an
earlier one asked, nor about the same file for the same task.
"""
from __future__ import annotations

import argparse
from collections import Counter, defaultdict
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import sys
import tempfile


ROOT = Path(__file__).resolve().parent.parent


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


if sys.version_info < (3, 11):
    raise SystemExit('Public sampling requires Python 3.11 or newer (the neutral bank uses tomllib).')

PRODUCER_PATHS = ('scripts/sample-public-eval.py', 'scripts/scope-public-eval.py', 'scripts/census-public-eval.py',
                  'scripts/eval/neutral/sample.py', 'scripts/eval/neutral/schema.py', 'scripts/eval/common.py')
PRODUCER_HASHES = {name: hashlib.sha256((ROOT/name).read_bytes()).hexdigest() for name in PRODUCER_PATHS}
SCOPE = load('public_scope', ROOT/'scripts/scope-public-eval.py')
BANK = load('neutral_sample', ROOT/'scripts/eval/neutral/sample.py')
SEED = 'knowcode-public-v1-source-2026-10-16'
TASKS = ('http-cross-repo', 'di-implementations', 'two-hop-impact', 'definition-lookup')
QUOTAS = {'http-cross-repo': {'jellyfin': 3, 'bitwarden-core': 3},
          'di-implementations': {'jellyfin': 1, 'bitwarden-core': 2, 'autofac': 3},
          'two-hop-impact': {'jellyfin': 2, 'bitwarden-core': 2, 'autofac': 2}}
SERVER_REPOS = {'jellyfin-server', 'bitwarden-server'}
CS_TYPE = re.compile(r'^\s*(?:(?:public|internal|private|protected|static|abstract|sealed|partial|readonly|ref|file|new)\s+)*'
                     r'(class|struct|interface|enum|record(?:\s+(?:class|struct))?)\s+(@?\w+)', re.M)
JS_DECL = re.compile(r'^\s*export\s+(?:default\s+)?(?:(?:async|abstract|declare)\s+)*'
                     r'(class|function)\s*\*?\s+([A-Za-z_$][\w$]*)', re.M)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def check_producers():
    if any(digest((ROOT/name).read_bytes()) != expected for name, expected in PRODUCER_HASHES.items()):
        raise ValueError('sampling producer source changed; retain this run as incomplete and rerun unchanged producers')


def declarations(text, language):
    pattern = CS_TYPE if language == 'csharp' else JS_DECL
    for match in pattern.finditer(text):
        yield {'key': match.group(2), 'kind': ' '.join(match.group(1).split()),
               'line': text.count('\n', 0, match.start(2))+1}


def excluded(row, subjects):
    return any(row['task'] == s['task'] and ((row['repo'], row['file']) == (s['repo'], s['file']) or row['key'] == s['key'])
               for s in subjects)


def rank_candidates(rows, views, seed=SEED, server_repos=SERVER_REPOS):
    files = defaultdict(list)
    metadata = {}
    for row in rows:
        metadata[row['repo']] = row['family']
        if row['subject_eligible']:
            files[row['repo']].append(row)
    candidates = {}

    def add(task, repo, item):
        row = dict(item, task=task, repo=repo, family=metadata[repo])
        identity = json.dumps([task, row['family'], repo, row['file'], row['line'], row['key']],
                              ensure_ascii=True, separators=(',', ':'))
        row.update(identity=identity, id=digest(identity.encode()), rank=digest((seed+'\n'+identity).encode()))
        previous = candidates.setdefault(identity, row)
        if previous != row:
            raise ValueError('conflicting source candidate identity')

    for repo, eligible in sorted(files.items()):
        paths = sorted(row['path'] for row in eligible)
        for task, bank_task in [('http-cross-repo', 'http-cross-repo'),
                                ('di-implementations', 'di-implementations'), ('two-hop-impact', 'cs-callers')]:
            if task == 'http-cross-repo' and repo not in server_repos:
                continue
            for item in BANK.structural_candidates(views/repo, paths, bank_task, None, None, eligible_files=set(paths)):
                add(task, repo, item)
        for source in sorted(eligible, key=lambda row: row['path']):
            text = (views/repo/source['path']).read_text(encoding='utf-8', errors='replace')
            for item in declarations(text, source['language']):
                add('definition-lookup', repo, dict(item, file=source['path']))
    return sorted(candidates.values(), key=lambda row: (TASKS.index(row['task']), row['family'],
                  row['repo'] if row['task'] == 'definition-lookup' else '', row['rank'], row['identity']))


def provisional_picks(candidates, repos, quotas=QUOTAS, exclude=()):
    groups = defaultdict(list)
    for row in candidates:
        group = row['repo'] if row['task'] == 'definition-lookup' else row['family']
        groups[(row['task'], group)].append(row)
    result = []
    for task in TASKS:
        wanted = {repo: 1 for repo in sorted(repos)} if task == 'definition-lookup' else quotas[task]
        for group, quota in sorted(wanted.items()):
            seen = set()
            picks = []
            for row in sorted(groups[(task, group)], key=lambda row: (row['rank'], row['identity'])):
                if excluded(row, exclude):
                    continue
                file = (row['repo'], row['file'])
                if file in seen:
                    continue
                seen.add(file)
                picks.append(row['id'])
                if len(picks) == quota:
                    break
            result.append({'task': task, 'group': group, 'quota': quota, 'provisional_ids': picks,
                           'shortfall': quota-len(picks)})
    return result


def sample(manifest, source, census, scope_root, policy, out, seed=SEED, plan=None):
    check_producers()
    settings = json.loads(plan.read_bytes()) if plan else {}
    seed = settings.get('seed', seed)
    quotas = settings.get('quotas', QUOTAS)
    server_repos = set(settings.get('server_repos', SERVER_REPOS))
    exclude = settings.get('exclude', [])
    scope_bytes = (scope_root/'scope.json').read_bytes()
    previous = json.loads(scope_bytes)
    rows_bytes = (scope_root/'files.jsonl').read_bytes()
    if digest(rows_bytes) != previous['files_sha256']:
        raise ValueError('scope rows differ from scope receipt')
    out = out.resolve()
    if any(out.is_relative_to(path.resolve()) for path in (ROOT, source, census, scope_root)):
        raise ValueError('sampling output must be outside checkout and input evidence')
    if out.exists() and any(out.iterdir()):
        raise ValueError('output must be new or empty; existing evidence is retained')
    with tempfile.TemporaryDirectory(prefix='knowcode-sample-verify-') as temp:
        checked = SCOPE.scope(manifest, source, census, policy, Path(temp)/'scope')
    for field in ('policy_sha256', 'census_sha256', 'source_receipt_sha256', 'manifest_sha256', 'files_sha256'):
        if checked[field] != previous[field]:
            raise ValueError('sampling inputs differ from recorded scope: '+field)
    rows = [json.loads(line) for line in rows_bytes.decode().splitlines()]
    candidates = rank_candidates(rows, source/'view', seed, server_repos)
    # Recheck source after extraction as well, so candidate enumeration cannot conceal source edits.
    with tempfile.TemporaryDirectory(prefix='knowcode-sample-recheck-') as temp:
        after = SCOPE.scope(manifest, source, census, policy, Path(temp)/'scope')
    if after['files_sha256'] != checked['files_sha256']:
        raise ValueError('source changed during candidate extraction')
    check_producers()
    encoded = ''.join(json.dumps(row, sort_keys=True)+'\n' for row in candidates).encode()
    picks = provisional_picks(candidates, {row['repo'] for row in rows}, quotas, exclude)
    picks_bytes = (json.dumps(picks, indent=2)+'\n').encode()
    result = {'format': 1, 'stage': 'source candidates ranked; provisional picks unreviewed; no questions or labels frozen; no scored runs',
              'seed': seed, 'plan_sha256': digest(plan.read_bytes()) if plan else None,
              'excluded_candidates': sum(excluded(row, exclude) for row in candidates), 'scope_sha256': digest(scope_bytes), 'candidates_sha256': digest(encoded),
              'provisional_sha256': digest(picks_bytes), 'tool_sha256': digest(Path(__file__).read_bytes()),
              'neutral_sampler_sha256': digest((ROOT/'scripts/eval/neutral/sample.py').read_bytes()),
              'producer_hashes': PRODUCER_HASHES, 'python_version': sys.version,
              'candidates': len(candidates),
              'population_by_task': dict(sorted(Counter(row['task'] for row in candidates).items())),
              'provisional_subjects': sum(len(group['provisional_ids']) for group in picks),
              'shortfall': sum(group['shortfall'] for group in picks),
              'review_required': 'Review in rank order from pinned source; record all rejections and duplicate-file skips. Empty answers and unsupported patterns are not exclusion reasons.',
              'extraction_limits': 'Regex declaration seeds, not semantic resolution; comments, strings, attributes, overloads and unusual syntax require source review. No reference search or tool output influences ranking.'}
    out.mkdir(parents=True, exist_ok=True, mode=0o700)
    (out/'candidates.jsonl').write_bytes(encoded)
    (out/'provisional.json').write_bytes(picks_bytes)
    (out/'sampling.json').write_text(json.dumps(result, indent=2)+'\n')
    return result


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--manifest', type=Path, default=ROOT/'eval/public-v1/manifest.json')
    parser.add_argument('--policy', type=Path, default=ROOT/'eval/public-v1/scope.json')
    parser.add_argument('--source', type=Path, required=True)
    parser.add_argument('--census', type=Path, required=True)
    parser.add_argument('--scope', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--seed', default=SEED)
    parser.add_argument('--plan', type=Path, help='a later bank\'s seed, quotas, server repositories and exclusions')
    args = parser.parse_args()
    print(json.dumps(sample(args.manifest, args.source, args.census, args.scope, args.policy, args.out, args.seed,
                            args.plan), indent=2))
