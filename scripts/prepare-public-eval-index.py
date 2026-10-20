#!/usr/bin/env python3
"""Prepare neutral Git workspaces from verified public session source; never invoke KnowCode.

Fresh metadata only: no clone, fetch, commit, original history, hooks, build or model call.
Agent source views remain unchanged and contain no Git metadata.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parent.parent
GIT_ENV = {'PATH': '/usr/bin:/bin', 'LC_ALL': 'C', 'GIT_CONFIG_NOSYSTEM': '1',
           'GIT_CONFIG_SYSTEM': '/dev/null', 'GIT_CONFIG_GLOBAL': '/dev/null', 'GIT_TERMINAL_PROMPT': '0'}


def digest(data):
    return hashlib.sha256(data).hexdigest()


def load_census():
    spec = importlib.util.spec_from_file_location('public_index_census', ROOT/'scripts/census-public-eval.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def verify_payload(root, repo):
    """The only extra entry allowed is this workspace's real root .git directory."""
    expected = {item['path']: item for item in repo['files']}
    observed = set()
    def walk_error(error):
        raise error
    for directory, children, files in os.walk(root, followlinks=False, onerror=walk_error):
        directory = Path(directory)
        for name in list(children):
            path = directory/name
            if directory == root and name == '.git':
                if path.is_symlink():
                    raise ValueError('symlinked neutral Git metadata')
                children.remove(name)
            elif path.is_symlink() or name == '.git':
                raise ValueError('unexpected source directory or nested Git metadata')
        for name in files:
            path = directory/name
            relative = path.relative_to(root).as_posix()
            if relative not in expected or path.is_symlink():
                raise ValueError('unexpected neutral workspace payload')
            metadata = path.stat()
            data = path.read_bytes()
            after = path.stat()
            if (metadata.st_ino, metadata.st_size, metadata.st_mtime_ns) != (after.st_ino, after.st_size, after.st_mtime_ns):
                raise ValueError('neutral source changed during verification')
            item = expected[relative]
            if metadata.st_mode & 0o222 or len(data) != item['bytes'] or digest(data) != item['sha256']:
                raise ValueError('neutral source differs from pinned receipt or is writable')
            observed.add(relative)
    if observed != set(expected):
        raise ValueError('neutral source file set differs from receipt')


def prepare(manifest, source_receipt, session):
    session = Path(session)
    if session.is_symlink() or not session.is_dir():
        raise ValueError('real prepared session required')
    session = session.resolve(strict=True)
    session_path = session/'session.json'
    inputs = [Path(manifest), Path(source_receipt), session_path]
    if any(p.is_symlink() for p in inputs):
        raise ValueError('symlinked preparation input')
    captured = {p: p.read_bytes() for p in inputs}
    specification = json.loads(captured[Path(manifest)])
    receipt = json.loads(captured[Path(source_receipt)])
    state = json.loads(captured[session_path])
    if specification.get('format') != 1 or not receipt.get('stage', '').startswith('source-ready;'):
        raise ValueError('source preparation is incomplete or has an unknown format')
    if state.get('session', {}).get('arm') not in ('knowcode', 'knowcode-unprompted'):
        raise ValueError('neutral Git workspaces belong only to the KnowCode arms')
    if state.get('manifest_sha256') != digest(captured[Path(manifest)]) or receipt.get('manifest_sha256') != digest(captured[Path(manifest)]):
        raise ValueError('manifest differs from prepared source/session')
    if state.get('source_receipt_sha256') != digest(captured[Path(source_receipt)]):
        raise ValueError('source receipt differs from prepared session')
    if (state.get('source_root'), state.get('required_client_cwd')) != ('source', 'control'):
        raise ValueError('unexpected prepared session contract')
    view, control = session/'source', session/'control'
    if any(p.is_symlink() or not p.is_dir() for p in [view, control]):
        raise ValueError('missing or symlinked session roots')
    out = control/'index'
    record = out/'neutral-index.json'
    if any(p.exists() or p.is_symlink() for p in [out, record]):
        raise ValueError('existing neutral workspace/evidence retained; prepare a new session')
    expected = {}
    for family in specification['families']:
        for repo in family['repos']:
            if repo['name'] in expected:
                raise ValueError('duplicate cohort repository')
            expected[repo['name']] = dict(repo, family=family['name'])
    if len(receipt['repos']) != len(expected) or {r['name'] for r in receipt['repos']} != set(expected):
        raise ValueError('source receipt repository set differs')
    census = load_census()
    for name in expected:
        # Reuse the strict source path validation; repo names are also single path components.
        if census.safe_relative(name).parts != (name,):
            raise ValueError('unsafe cohort repository name')
    if {p.name for p in view.iterdir()} != set(expected):
        raise ValueError('session source repository set differs')
    rows = []
    for repo in sorted(receipt['repos'], key=lambda r: r['name']):
        if any(repo.get(key) != value for key, value in expected[repo['name']].items()):
            raise ValueError('source metadata differs from public manifest')
        if any(item['symlink'] for item in repo['files']):
            raise ValueError('neutral workspace currently requires a regular-file-only cohort; no files dropped')
        rows.extend(census.inspect_view(view/repo['name'], repo))
    encoded = ''.join(json.dumps(row, sort_keys=True)+'\n' for row in rows).encode()
    if digest(encoded) != state.get('source_files_sha256'):
        raise ValueError('source file population differs from prepared session')
    producers = {p: digest(p.read_bytes()) for p in [Path(__file__), ROOT/'scripts/census-public-eval.py']}
    git = shutil.which('git')
    if git is None:
        raise ValueError('Git required for neutral metadata preparation')

    def run_git(root, *args, expected_code=0):
        process = subprocess.run([git, '-c', 'core.hooksPath=/dev/null', '-c', 'core.fsmonitor=false', *args],
                                 cwd=root, env=GIT_ENV, stdin=subprocess.DEVNULL, capture_output=True,
                                 text=True, timeout=15)
        if process.returncode != expected_code:
            raise ValueError('neutral Git command failed: '+process.stderr.strip())
        return process.stdout

    def unchanged():
        if any(p.read_bytes() != value for p, value in captured.items()):
            raise ValueError('preparation inputs changed; workspace not promoted')
        if any(digest(p.read_bytes()) != value for p, value in producers.items()):
            raise ValueError('preparation producer changed; workspace not promoted')

    summaries = []
    with tempfile.TemporaryDirectory(prefix='.knowcode-neutral-', dir=control) as temp:
        stage = Path(temp)/'index'
        stage.mkdir(mode=0o700)
        template = Path(temp)/'empty-template'
        template.mkdir(mode=0o700)
        for repo in sorted(receipt['repos'], key=lambda r: r['name']):
            target = stage/repo['name']
            target.mkdir()
            for item in repo['files']:
                name = census.safe_relative(item['path']).as_posix()
                source = view/repo['name']/name
                destination = target/name
                destination.parent.mkdir(parents=True, exist_ok=True)
                with source.open('rb') as src, destination.open('xb') as dst:
                    shutil.copyfileobj(src, dst)
                a, b = source.stat(), destination.stat()
                if b.st_nlink != 1 or (a.st_dev, a.st_ino) == (b.st_dev, b.st_ino):
                    raise ValueError('neutral payload must be copied by value')
                destination.chmod(0o444)
            census.inspect_view(target, repo)
            run_git(target, 'init', '--quiet', '--initial-branch=public-eval', '--template='+str(template))
            run_git(target, 'config', '--local', 'core.hooksPath', '/dev/null')
            run_git(target, 'config', '--local', 'remote.origin.url', repo['url'])
            if run_git(target, 'symbolic-ref', 'HEAD').strip() != 'refs/heads/public-eval':
                raise ValueError('unexpected synthetic branch')
            run_git(target, 'rev-parse', '--verify', 'HEAD', expected_code=128)
            if run_git(target, 'for-each-ref').strip():
                raise ValueError('neutral repository unexpectedly has refs')
            counts = dict(line.split(': ', 1) for line in run_git(target, 'count-objects', '-v').splitlines())
            if any(int(counts[key]) != 0 for key in ['count', 'in-pack', 'packs']):
                raise ValueError('neutral repository unexpectedly contains Git objects')
            verify_payload(target, repo)
            for directory, children, _ in os.walk(target, topdown=False):
                path = Path(directory)
                if '.git' not in path.relative_to(target).parts:
                    path.chmod(0o555)
            summaries.append({'name': repo['name'], 'source_pin': repo['commit'], 'public_origin': repo['url'],
                              'files': len(repo['files']), 'head_unborn': True, 'refs': 0, 'objects': 0})
        for repo in receipt['repos']:
            census.inspect_view(view/repo['name'], repo)
        unchanged()
        if out.exists() or record.exists():
            raise ValueError('workspace output appeared; retained existing evidence')
        result = {'format': 1, 'stage': 'neutral Git metadata/source prepared; KnowCode indexing and freeze pending',
                  'manifest_sha256': digest(captured[Path(manifest)]),
                  'source_receipt_sha256': digest(captured[Path(source_receipt)]),
                  'source_files_sha256': state['source_files_sha256'], 'repos': summaries,
                  'repo_paths': {r['name']: str(out/r['name']) for r in summaries},
                  'producer_hashes': {p.name: value for p, value in producers.items()},
                  'git_version': run_git(control, '--version').strip(), 'git_env': GIT_ENV,
                  'original_history_imported': False, 'knowcode_executed': False, 'model_calls': 0,
                  'scored_runs': 0, 'ready_to_run': False,
                  'remaining': ['verify source/labels/config/order and independent review, then freeze',
                                'build audited binary and index only after freeze; record indexing separately',
                                'actual MCP schemas, client permissions, API/credential isolation and transcript audit']}
        staged_record = stage/'neutral-index.json'
        staged_record.write_text(json.dumps(result, indent=2)+'\n')
        unchanged()
        os.rename(stage, out)
    return result


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--manifest', type=Path, default=ROOT/'eval/public-v1/manifest.json')
    parser.add_argument('--source-receipt', type=Path, required=True)
    parser.add_argument('--session', type=Path, required=True)
    args = parser.parse_args()
    result = prepare(args.manifest, args.source_receipt, args.session)
    print(json.dumps({'repos': len(result['repos']), 'files': sum(r['files'] for r in result['repos']),
                      'knowcode_executed': False, 'model_calls': 0, 'ready_to_run': False}))
