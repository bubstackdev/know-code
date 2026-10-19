#!/usr/bin/env python3
"""Copy pinned public source for one draft evaluation session, without starting an agent.

This prepares source and empty control/profile directories. It is not an OS sandbox,
client configuration, KnowCode index, study freeze or permission to run scored tasks.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import tempfile

ROOT = Path(__file__).resolve().parent.parent


def digest(data):
    return hashlib.sha256(data).hexdigest()


def census_module():
    path = ROOT/'scripts/census-public-eval.py'
    spec = importlib.util.spec_from_file_location('public_session_census', path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def prepare(manifest, source, plan_root, session_id, out):
    source = source.resolve(strict=True)
    plan_root = plan_root.resolve(strict=True)
    out = out.absolute()
    if not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_-]{0,255}', session_id):
        raise ValueError('unsafe session identifier')
    if out.exists() or out.is_symlink():
        raise ValueError('output must be new; existing session evidence is retained')
    if any(out.resolve().is_relative_to(p) for p in (ROOT, source, plan_root)):
        raise ValueError('session output must be outside checkout, source and plan')
    inputs = [manifest, source/'source-receipt.json', plan_root/'plan.json',
              plan_root/'order.jsonl', plan_root/'questions.jsonl']
    captured = {p: p.read_bytes() for p in inputs}
    producer_paths = [Path(__file__), ROOT/'scripts/census-public-eval.py']
    producers = {p: digest(p.read_bytes()) for p in producer_paths}
    plan = json.loads(captured[plan_root/'plan.json'])
    receipt = json.loads(captured[source/'source-receipt.json'])
    if plan.get('format') != 1 or plan.get('manifest_sha256') != digest(captured[manifest]):
        raise ValueError('session manifest differs from plan')
    if plan.get('source_receipt_sha256') != digest(captured[source/'source-receipt.json']):
        raise ValueError('source receipt differs from plan')
    for name, key in [('order.jsonl', 'order_sha256'), ('questions.jsonl', 'operator_questions_sha256')]:
        if plan.get(key) != digest(captured[plan_root/name]):
            raise ValueError('plan '+name+' hash differs')
    orders = [json.loads(line) for line in captured[plan_root/'order.jsonl'].decode().splitlines() if line.strip()]
    matches = [row for row in orders if row.get('session') == session_id]
    if len(matches) != 1 or matches[0].get('arm') not in ('plain', 'knowcode'):
        raise ValueError('session is absent, duplicated or has an unknown arm')
    selected = matches[0]
    questions = [json.loads(line) for line in captured[plan_root/'questions.jsonl'].decode().splitlines() if line.strip()]
    question = [q for q in questions if q.get('id') == selected.get('question_id')]
    if len(question) != 1:
        raise ValueError('session question missing or duplicated')
    census = census_module()
    with tempfile.TemporaryDirectory(prefix='knowcode-session-verify-') as temp:
        verified = census.census(manifest, source, Path(temp)/'census')
    if verified['files_sha256'] != plan.get('source_files_sha256'):
        raise ValueError('source file population differs from plan')

    def unchanged():
        if any(p.read_bytes() != value for p, value in captured.items()):
            raise ValueError('session inputs changed; no prepared session published')
        if any(digest(p.read_bytes()) != value for p, value in producers.items()):
            raise ValueError('session producer changed; no prepared session published')

    unchanged()
    out.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.knowcode-session-', dir=out.parent) as temp:
        staging = Path(temp)/'session'
        view = staging/'source'
        view.mkdir(parents=True, mode=0o700)
        upstream = []
        copied = 0
        byte_count = 0
        for repo in receipt['repos']:
            target = view/repo['name']
            target.mkdir()
            original = source/'view'/repo['name']
            for item in repo['files']:
                name = census.safe_relative(item['path']).as_posix()
                destination = target/name
                destination.parent.mkdir(parents=True, exist_ok=True)
                path = original/name
                # census verified all original paths. Recheck type/bytes for this copy before promoting it.
                if item['symlink']:
                    if not path.is_symlink():
                        raise ValueError('source entry type changed')
                    link = os.readlink(path)
                    if digest(os.fsencode(link)) != item['sha256']:
                        raise ValueError('source symlink changed')
                    destination.symlink_to(link)
                else:
                    if path.is_symlink() or not path.is_file():
                        raise ValueError('source entry type changed')
                    with path.open('rb') as src, destination.open('xb') as dst:
                        shutil.copyfileobj(src, dst)
                    copied_stat, original_stat = destination.stat(), path.stat()
                    if copied_stat.st_nlink != 1 or (copied_stat.st_dev, copied_stat.st_ino) == (original_stat.st_dev, original_stat.st_ino):
                        raise ValueError('session source must be copied by value, never hard-linked')
                    destination.chmod(0o444)
                copied += 1
                byte_count += item['bytes']
                parts = Path(name).parts
                if Path(name).name in ('AGENTS.md', 'CLAUDE.md', '.mcp.json') or '.claude' in parts or '.codex' in parts:
                    upstream.append(repo['name']+'/'+name)
            # This verifies the copied bytes, entry set, internal symlinks and absence of Git/privacy surprises.
            census.inspect_view(target, repo)
        for directory, _, _ in os.walk(view, topdown=False):
            Path(directory).chmod(0o555)
        (staging/'control').mkdir(mode=0o700)
        (staging/'profile').mkdir(mode=0o700)
        result = {'format': 1, 'stage': 'source copy prepared; runtime isolation, index and freeze pending',
                  'session': selected, 'question': question[0], 'ready_to_run': False, 'scored_runs': 0,
                  'plan_sha256': digest(captured[plan_root/'plan.json']),
                  'manifest_sha256': digest(captured[manifest]),
                  'source_receipt_sha256': digest(captured[source/'source-receipt.json']),
                  'source_files_sha256': verified['files_sha256'], 'copied_files': copied, 'copied_bytes': byte_count,
                  'producer_hashes': {p.name: value for p, value in producers.items()},
                  'source_root': 'source', 'required_client_cwd': 'control', 'empty_profile_directory': 'profile',
                  'upstream_instruction_paths': sorted(upstream),
                  'remaining_gates': ['freeze and independent label adjudication',
                                      'actual client profile and tool schema verification',
                                      'disable upstream instruction/config auto-loading; source files remain data',
                                      'enforce runtime filesystem/tool/network access; copies and permissions are not a sandbox',
                                      'separate source-identical synthetic Git/index workspace for the KnowCode arm',
                                      'no source edits, notes, unrelated integrations or other-session interference']}
        (staging/'session.json').write_text(json.dumps(result, indent=2)+'\n')
        unchanged()
        # Verify originals again after the copy, so the artifact never silently mixes source versions.
        for repo in receipt['repos']:
            census.inspect_view(source/'view'/repo['name'], repo)
        unchanged()
        if out.exists() or out.is_symlink():
            raise ValueError('output appeared; existing session evidence is retained')
        os.rename(staging, out)
    return result


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--manifest', type=Path, default=ROOT/'eval/public-v1/manifest.json')
    parser.add_argument('--source', type=Path, required=True)
    parser.add_argument('--plan', type=Path, required=True)
    parser.add_argument('--session', required=True)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    result = prepare(args.manifest, args.source, args.plan, args.session, args.out)
    print(json.dumps({k: result[k] for k in ('copied_files', 'copied_bytes', 'ready_to_run', 'scored_runs')}))
