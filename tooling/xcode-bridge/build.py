#!/usr/bin/env python3
"""Build/stage a separate pinned tool from local source; never install or fetch."""
import argparse
import fnmatch
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import tarfile
import time

HERE = Path(__file__).resolve().parent


def digest(filename):
    with Path(filename).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def git(checkout, *args):
    return subprocess.check_output(['git', '-C', str(checkout), *args], text=True, timeout=30).strip()


def check_checkout(checkout, revision):
    if git(checkout, 'rev-parse', 'HEAD') != revision:
        raise RuntimeError(f'Wrong pinned revision: {checkout}; expected {revision}')
    if git(checkout, 'status', '--porcelain', '--untracked-files=no'):
        raise RuntimeError(f'Tracked source has local changes: {checkout}')


def dependency_heads(scratch, recipe):
    result = {}
    for name, pin in recipe['dependencies'].items():
        checkout = scratch / 'checkouts' / name
        if not checkout.is_dir():
            raise RuntimeError(f'Missing prepared dependency checkout {checkout}; resolve the exact committed Package.resolved separately before building')
        check_checkout(checkout, pin['revision'])
        result[name] = pin['revision']
    return result


def licenses(checkout, destination):
    copied = []
    for name in git(checkout, 'ls-files').splitlines():
        if not any(fnmatch.fnmatchcase(Path(name).name.upper(), pattern) for pattern in ['LICENSE*', 'NOTICE*', 'COPYING*', 'CONTRIBUTORS*']):
            continue
        source = checkout / name
        if source.is_file():
            target = destination / name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, target)
            copied.append(name)
    if not copied:
        raise RuntimeError(f'No retained license/notice files found in {checkout}')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source-checkout', type=Path, required=True)
    parser.add_argument('--scratch-path', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True, help='New durable evidence directory; runtime/ is the installable tool bundle')
    parser.add_argument('--developer-dir', type=Path, required=True, help='Selected installed Xcode Contents/Developer; never changes xcode-select')
    parser.add_argument('--prepare-only', action='store_true', help='Apply/check local source and pins without launching Swift or compiling')
    args = parser.parse_args()
    recipe = json.loads((HERE / 'recipe.json').read_text())
    for filename, expected in [('know-code.patch', recipe['patch_sha256']), ('Package.resolved', recipe['package_resolved_sha256'])]:
        if digest(HERE / filename) != expected:
            raise RuntimeError(f'Tooling input differs from recipe: {filename}')
    checkout, scratch, out = args.source_checkout.resolve(), args.scratch_path.resolve(), args.output.resolve()
    check_checkout(checkout, recipe['upstream']['revision'])
    heads = dependency_heads(scratch, recipe)
    out.mkdir(parents=True, exist_ok=False)
    source = out / 'source'
    source.mkdir()
    archive = out / 'upstream.tar'
    with archive.open('wb') as stream:
        subprocess.run(['git', '-C', str(checkout), 'archive', '--format=tar', recipe['upstream']['revision']], stdout=stream, check=True, timeout=30)
    with tarfile.open(archive) as stream:
        stream.extractall(source, filter='data')
    # No repository/index mutation: prevent discovery of the surrounding Know
    # Code checkout while applying this trusted, versioned patch to owned bytes.
    patch_env = dict(os.environ, GIT_CEILING_DIRECTORIES=str(source.parent))
    for key in ['GIT_DIR', 'GIT_WORK_TREE', 'GIT_INDEX_FILE', 'GIT_COMMON_DIR']:
        patch_env.pop(key, None)
    subprocess.run(['git', 'apply', '--check', str(HERE / 'know-code.patch')], cwd=source, env=patch_env, check=True, timeout=30)
    subprocess.run(['git', 'apply', str(HERE / 'know-code.patch')], cwd=source, env=patch_env, check=True, timeout=30)
    shutil.copy2(HERE / 'Package.resolved', source / 'Package.resolved')
    expected_sources = {name: item['distributed_sha256'] for name, item in recipe['changed_files'].items()}
    expected_sources.update({'Package.swift': recipe['package_manifest_sha256'], 'Package.resolved': recipe['package_resolved_sha256']})
    for filename, expected in expected_sources.items():
        if digest(source / filename) != expected:
            raise RuntimeError(f'Prepared source differs from recipe: {filename}')
    record = {'status': 'prepared_only', 'builder_sha256': digest(Path(__file__).resolve()),
              'recipe_id': recipe['recipe_id'], 'recipe_sha256': digest(HERE / 'recipe.json'),
              'upstream': recipe['upstream'], 'patch_sha256': recipe['patch_sha256'],
              'source_hashes': expected_sources, 'dependencies_before': heads}
    (out / 'build-record.json').write_text(json.dumps(record, indent=2) + '\n')
    if args.prepare_only:
        print(json.dumps({'status': record['status'], 'output': str(out)}))
        return
    env = dict(os.environ, DEVELOPER_DIR=str(args.developer_dir.resolve()), MACOSX_DEPLOYMENT_TARGET='15.0')
    env.pop('SDKROOT', None)
    argv = ['/usr/bin/xcrun', 'swift', 'build', '--package-path', str(source), '--scratch-path', str(scratch),
            '--configuration', 'release', '--force-resolved-versions', '--skip-update', '--product', recipe['executable_name'], '--jobs', '2']
    record.update(status='building', command=argv, started_ns=time.time_ns())
    (out / 'build-record.json').write_text(json.dumps(record, indent=2) + '\n')
    with (out / 'stdout.log').open('wb') as stdout, (out / 'stderr.log').open('wb') as stderr:
        child = subprocess.Popen(argv, env=env, stdout=stdout, stderr=stderr, start_new_session=True)
        def interrupted(signum, _frame):
            raise InterruptedError(f'Bridge build interrupted by signal {signum}')
        previous = {signum: signal.signal(signum, interrupted) for signum in (signal.SIGTERM, signal.SIGINT)}
        try:
            try:
                code = child.wait(timeout=900)
            except subprocess.TimeoutExpired:
                code = 124
        finally:
            try:
                if child.poll() is None:
                    os.killpg(child.pid, signal.SIGTERM)
                    try:
                        child.wait(timeout=2)
                    except subprocess.TimeoutExpired:
                        os.killpg(child.pid, signal.SIGKILL)
                        child.wait()
            finally:
                for signum, handler in previous.items():
                    signal.signal(signum, handler)
    record.update(status='built_only' if code == 0 else 'failed', exit_code=code, finished_ns=time.time_ns(),
                  dependencies_after=dependency_heads(scratch, recipe))
    (out / 'build-record.json').write_text(json.dumps(record, indent=2) + '\n')
    for filename, expected in expected_sources.items():
        if digest(source / filename) != expected:
            raise RuntimeError(f'Source changed during build: {filename}')
    check_checkout(checkout, recipe['upstream']['revision'])
    if digest(Path(__file__).resolve()) != record['builder_sha256']:
        raise RuntimeError('Builder source changed during build; do not package these outputs')
    if code:
        raise RuntimeError(f'Build failed ({code}); retain {out}/stderr.log')
    runtime = out / 'runtime'
    runtime.mkdir()
    release = scratch / 'release'
    for filename in [recipe['executable_name'], 'SWBBuildServiceBundle', *recipe['runtime_resources']]:
        original, target = release / filename, runtime / filename
        if original.is_dir():
            shutil.copytree(original, target, symlinks=True)
        else:
            shutil.copy2(original, target)
    for name, local in [('sourcekit-xcode-bsp', checkout), *[(name, scratch / 'checkouts' / name) for name in recipe['dependencies']]]:
        licenses(local, runtime / 'licenses' / name)
    shutil.copy2(HERE / 'MODIFICATIONS.md', runtime / 'MODIFICATIONS.md')
    shutil.copy2(HERE / 'recipe.json', runtime / 'source-recipe.json')
    shutil.copy2(HERE / 'Package.resolved', runtime / 'Package.resolved')
    shutil.copy2(HERE / 'know-code.patch', runtime / 'know-code.patch')
    identity = subprocess.check_output([str(runtime / recipe['executable_name']), '--version'], env=env, text=True, timeout=10).strip()
    if identity != recipe['cli_identity']:
        raise RuntimeError(f'Unexpected bridge identity: {identity}')
    inventory = {str(p.relative_to(runtime)): {'bytes': p.stat().st_size, 'sha256': digest(p)}
                 for p in sorted(runtime.rglob('*')) if p.is_file()}
    manifest = {'schema_version': 1, 'tool': 'know-code-xcode-bridge', 'recipe_id': recipe['recipe_id'],
                'cli_identity': identity, 'upstream_revision': recipe['upstream']['revision'],
                'swift_build_revision': heads['swift-build'], 'recipe_sha256': record['recipe_sha256'],
                'patch_sha256': recipe['patch_sha256'], 'package_resolved_sha256': recipe['package_resolved_sha256'],
                'dependency_revisions': heads, 'source_hashes': expected_sources,
                'build': {'configuration': 'release', 'jobs': 2, 'deployment_target': '15.0',
                          'xcode_identity': subprocess.check_output(['/usr/bin/xcodebuild', '-version'], env=env, text=True, timeout=10).strip()},
                'files': inventory}
    (runtime / 'know-code-xcode-bridge.json').write_text(json.dumps(manifest, indent=2) + '\n')
    record.update(status='staged_build_only', manifest_sha256=digest(runtime / 'know-code-xcode-bridge.json'))
    (out / 'build-record.json').write_text(json.dumps(record, indent=2) + '\n')
    print(json.dumps({'status': record['status'], 'runtime': str(runtime), 'files': len(inventory),
                      'logical_bytes': sum(item['bytes'] for item in inventory.values())}))


if __name__ == '__main__':
    main()
