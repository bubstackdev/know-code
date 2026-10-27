#!/usr/bin/env python3
"""Record every public evaluation file's subject eligibility, before sampling.

This does not remove source from either arm, execute project code, or query KnowCode.
"""
from __future__ import annotations

import argparse
from collections import Counter
import importlib.util
import json
from pathlib import Path
import tempfile


ROOT = Path(__file__).resolve().parent.parent
SPEC = importlib.util.spec_from_file_location('public_census', ROOT/'scripts/census-public-eval.py')
CENSUS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CENSUS)


def classify(row: dict, policy: dict) -> list[str]:
    reasons = []
    repo = policy['repos'][row['repo']]
    if not any(row['path'].startswith(p) if p.endswith('/') else row['path'] == p for p in repo['include']):
        reasons.append('outside-primary-source-roots')
    if row['language'] not in policy['languages']:
        reasons.append('outside-subject-languages')
    reasons.extend('flag:'+flag for flag in row['review_flags'] if flag in policy['exclude_flags'])
    return sorted(reasons)


def scope(manifest: Path, source: Path, census_root: Path, policy_path: Path, out: Path) -> dict:
    policy_bytes = policy_path.read_bytes()
    policy = json.loads(policy_bytes)
    census_bytes = (census_root/'census.json').read_bytes()
    previous = json.loads(census_bytes)
    files_bytes = (census_root/'files.jsonl').read_bytes()
    if policy.get('format') != 1 or CENSUS.digest(files_bytes) != previous['files_sha256']:
        raise ValueError('invalid scope policy or changed census rows')
    if set(policy['repos']) != {r['name'] for r in previous['repos']}:
        raise ValueError('scope repository set differs from census')
    for repo in policy['repos'].values():
        if not repo['include'] or not repo['evidence']:
            raise ValueError('scope needs source roots and project evidence')
        for name in repo['include']+repo['evidence']:
            if CENSUS.safe_relative(name).as_posix() != name.rstrip('/'):
                raise ValueError('noncanonical scope path')
    out = out.resolve()
    if any(out.is_relative_to(p.resolve()) for p in (ROOT, source, census_root)):
        raise ValueError('scope output must be outside checkout, source and census')
    if out.exists() and any(out.iterdir()):
        raise ValueError('output must be new or empty; existing evidence is retained')
    # Re-enumerate and hash-check pinned source, not just a potentially stale census receipt.
    with tempfile.TemporaryDirectory(prefix='knowcode-scope-verify-') as temp:
        verified = CENSUS.census(manifest, source, Path(temp)/'census')
    for field in ('manifest_sha256', 'source_receipt_sha256', 'files_sha256', 'files', 'repos'):
        if verified[field] != previous[field]:
            raise ValueError('source differs from recorded census: '+field)
    rows = [json.loads(line) for line in files_bytes.decode().splitlines()]
    lookup = {(r['repo'], r['path']): r for r in rows}
    evidence = []
    for repo, entry in sorted(policy['repos'].items()):
        for path in entry['evidence']:
            row = lookup.get((repo, path))
            if row is None or row['symlink']:
                raise ValueError('missing regular project evidence: '+repo+'/'+path)
            evidence.append({'repo': repo, 'path': path, 'sha256': row['sha256']})
    classified = [dict(row, subject_eligible=not (reasons := classify(row, policy)),
                       subject_exclusion_reasons=reasons) for row in rows]
    encoded = ''.join(json.dumps(row, sort_keys=True)+'\n' for row in classified).encode()
    result = {'format': 1, 'stage': 'subject scope recorded; questions pending; no scored runs',
              'policy_sha256': CENSUS.digest(policy_bytes), 'census_sha256': CENSUS.digest(census_bytes),
              'source_receipt_sha256': verified['source_receipt_sha256'],
              'manifest_sha256': verified['manifest_sha256'],
              'tool_sha256': CENSUS.digest(Path(__file__).read_bytes()),
              'census_tool_sha256': verified['tool_sha256'],
              'files_sha256': CENSUS.digest(encoded), 'files': len(classified),
              'eligible_by_repo': dict(sorted(Counter(r['repo'] for r in classified if r['subject_eligible']).items())),
              'exclusion_reasons': dict(sorted(Counter(reason for r in classified for reason in r['subject_exclusion_reasons']).items())),
              'project_evidence': evidence,
              'source_availability': 'All exported files remain available to both arms and labellers; eligibility only controls sampling subjects.'}
    out.mkdir(parents=True, exist_ok=True, mode=0o700)
    (out/'files.jsonl').write_bytes(encoded)
    (out/'scope.json').write_text(json.dumps(result, indent=2)+'\n')
    (out/'policy.json').write_bytes(policy_bytes)
    return result


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--manifest', type=Path, default=ROOT/'eval/public-v1/manifest.json')
    parser.add_argument('--policy', type=Path, default=ROOT/'eval/public-v1/scope.json')
    parser.add_argument('--source', type=Path, required=True)
    parser.add_argument('--census', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    print(json.dumps(scope(args.manifest, args.source, args.census, args.policy, args.out), indent=2))
