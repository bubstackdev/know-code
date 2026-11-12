#!/usr/bin/env python3
"""Owned actual-Cargo managed preparation checks; run by the shared build owner."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import time

ROOT = Path(__file__).resolve().parents[1]
ARTIFACTS = ROOT / '.artifacts/rust-managed' / str(time.time_ns())
ARTIFACTS.mkdir(parents=True)
NODE = shutil.which('node')
assert NODE, 'Node.js is required'


def command(args, cwd):
    return subprocess.run(args, cwd=cwd, text=True, capture_output=True, check=True)


def analyze(label, workspace, execution=True, previous=(), retry=False):
    request = {'workspaceRoot': str(workspace), 'projects': [str(workspace / 'Cargo.toml')],
               'configuration': 'Debug', 'preparationFingerprint': 'owned-managed-setup-v1',
               'preparations': list(previous), 'retryPreparation': retry,
               'analysisPolicy': {'version': 1, 'rust': {'executionAllowed': execution,
                                                       'cacheDirectory': str(ARTIFACTS / 'cache')}}}
    result = subprocess.run([NODE, str(ROOT / 'tests/worker_harness.mjs'), str(ROOT / 'providers/rust/worker.mjs')],
                            input=json.dumps(request), text=True, cwd=ROOT, capture_output=True, check=True,
                            timeout=190, env=dict(os.environ, CARGO_HOME=str(ARTIFACTS / 'cargo-home')))
    payload = json.loads(result.stdout)
    (ARTIFACTS / f'{label}.json').write_text(json.dumps(payload, indent=2))
    return payload


def successful(payload):
    assert payload['code'] == 0, payload
    assert any(frame.get('complete', {}).get('success') for frame in payload['messages']), payload
    assert next(frame['provider'] for frame in payload['messages'] if 'provider' in frame)['analysisPolicyVersion'] == 1
    reports = [frame['report'] for frame in payload['messages'] if 'report' in frame]
    assert reports and all(report['status'] == 'partial' for report in reports), reports


# A local Git dependency exercises real missing-cache restoration without a
# hosted registry, network download, or mutation of a user repository.
dependency = ARTIFACTS / 'dependency'
(dependency / 'src').mkdir(parents=True)
(dependency / 'Cargo.toml').write_text('[package]\nname="managed-dependency"\nversion="0.1.0"\nedition="2024"\n')
(dependency / 'src/lib.rs').write_text('pub fn restored_value() -> usize { 7 }\n')
command(['git', 'init', '--quiet'], dependency)
command(['git', 'add', 'Cargo.toml', 'src/lib.rs'], dependency)
command(['git', '-c', 'user.name=Know Code fixture', '-c', 'user.email=fixture@example.invalid',
         '-c', 'commit.gpgsign=false', 'commit', '--quiet', '-m', 'Owned preparation dependency'], dependency)

workspace = ARTIFACTS / 'workspace'
(workspace / 'src').mkdir(parents=True)
(workspace / 'Cargo.toml').write_text('[package]\nname="managed-fixture"\nversion="0.1.0"\nedition="2024"\n'
                                      '[workspace]\n[dependencies]\nmanaged-dependency={git=' + json.dumps(dependency.as_uri()) + '}\n')
(workspace / 'build.rs').write_text('fn main() {\n'
    'std::fs::write(std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("build-script-ran"), "trusted").unwrap();\n'
    'println!("cargo:rustc-cfg=managed_generated");\n}\n')
(workspace / 'src/lib.rs').write_text('pub fn entry() -> usize { managed_dependency::restored_value() }\n'
                                       '#[cfg(managed_generated)] pub fn compiler_prepared() -> usize { 42 }\n')

denied = analyze('denied-missing-dependencies', workspace, execution=False)
assert denied['code'] != 0 and 'personal repository trust grant' in denied['stderr'], denied
assert not (workspace / 'build-script-ran').exists()
assert not any(frame.get('progress', {}).get('stage') == 'restoring' for frame in denied['messages'])

first = analyze('restored-and-prepared', workspace)
successful(first)
assert (workspace / 'build-script-ran').read_text() == 'trusted', first
assert (workspace / 'Cargo.lock').is_file()
preparations = [frame['preparation'] for frame in first['messages'] if 'preparation' in frame]
assert len(preparations) == 1 and preparations[0]['succeeded'], preparations
assert all(Path(filename).exists() for filename in preparations[0]['outputs'])
assert any(frame.get('progress', {}).get('stage') == 'restoring' for frame in first['messages']), first
names = {symbol['name'] for frame in first['messages'] if 'facts' in frame for symbol in frame['facts']['symbols']}
assert 'compiler_prepared' in names and 'restored_value' in names, names
options = [json.loads(frame['compilation']['optionsJson']) for frame in first['messages'] if 'compilation' in frame]
assert options and all(item['cargo']['buildScripts']['enable'] and item['procMacros'] for item in options), options
assert all(Path(item['cargo']['targetDir']).is_relative_to(ARTIFACTS / 'cache') for item in options)

# A normal source save reuses dependency setup, while the analyzer's own
# compiler preparation/cache still decides whether build inputs need updating.
with (workspace / 'src/lib.rs').open('a') as file:
    file.write('\npub fn saved_after_restore() -> usize { entry() }\n')
reused = analyze('saved-source-checkpoint', workspace, previous=preparations)
successful(reused)
assert not any('preparation' in frame or frame.get('progress', {}).get('stage') == 'restoring' for frame in reused['messages']), reused
assert any(symbol['name'] == 'saved_after_restore' for frame in reused['messages'] if 'facts' in frame for symbol in frame['facts']['symbols'])

plan = Path(preparations[0]['outputs'][0])
plan.unlink()
recreated = analyze('missing-derived-plan', workspace, previous=preparations)
successful(recreated)
assert plan.is_file() and any(frame.get('preparation', {}).get('succeeded') for frame in recreated['messages']), recreated

failed_checkpoint = {**preparations[0], 'succeeded': False, 'error': 'owned failed setup checkpoint'}
failed = analyze('failed-checkpoint-preserved', workspace, previous=[failed_checkpoint])
assert failed['code'] != 0 and 'refresh explicitly' in failed['stderr'], failed
assert not any(frame.get('progress', {}).get('stage') == 'restoring' for frame in failed['messages'])
retried = analyze('explicit-retry', workspace, previous=[failed_checkpoint], retry=True)
successful(retried)
assert any(frame.get('preparation', {}).get('succeeded') for frame in retried['messages']), retried

(ARTIFACTS / 'summary.json').write_text(json.dumps({'status': 'passed', 'cases': [
    'missing dependencies denied without execution grant', 'local Git restoration without hosted downloads',
    'trusted build-script cfg and marker', 'central analyzer outputs', 'saved-source dependency checkpoint reuse',
    'missing derived plan recreated', 'failed preparation explicit retry']}, indent=2))
print(ARTIFACTS)
