#!/usr/bin/env python3
"""Owned actual-Cargo managed preparation checks; run by the shared build owner."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
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
                            timeout=190, env=dict(os.environ, CARGO_HOME=str(ARTIFACTS / 'cargo-home'),
                                                 RUSTC_WORKSPACE_WRAPPER=str(environment_wrapper), CARGO_BUILD_JOBS='37'))
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

local_macro = ARTIFACTS / 'local-macro'
(local_macro / 'src').mkdir(parents=True)
(local_macro / 'Cargo.toml').write_text('[package]\nname="local-macro"\nversion="0.1.0"\nedition="2024"\n'
                                      '[lib]\nproc-macro=true\n[workspace]\n')
(local_macro / 'src/lib.rs').write_text('extern crate proc_macro;\nmod implementation;\n'
    '#[proc_macro] pub fn literal_value(_: proc_macro::TokenStream) -> proc_macro::TokenStream { implementation::literal() }\n')
(local_macro / 'src/implementation.rs').write_text('pub fn literal() -> proc_macro::TokenStream { "41".parse().unwrap() }\n')
(local_macro / 'build.rs').write_text('fn main() { println!("cargo:rerun-if-changed=src/lib.rs"); }\n')

# Cargo's selected repository wrapper and environment workspace wrapper must
# both reach actual compilation, rather than being replaced by RA's wrapper.
def wrapper(filename, log):
    filename.write_text(f'#!{sys.executable}\nimport json,os,sys\n'
        f'with open({str(log)!r}, "a") as file: file.write(json.dumps({{"args": sys.argv[1:], "jobs": os.environ.get("CARGO_BUILD_JOBS")}})+"\\n")\n'
        'os.execv(sys.argv[1], sys.argv[1:])\n')
    filename.chmod(0o755)
    return filename

repository_log, environment_log = ARTIFACTS / 'repository-wrapper.jsonl', ARTIFACTS / 'environment-wrapper.jsonl'
repository_wrapper = wrapper(ARTIFACTS / 'repository-wrapper.py', repository_log)
environment_wrapper = wrapper(ARTIFACTS / 'environment-wrapper.py', environment_log)

workspace = ARTIFACTS / 'workspace'
(workspace / 'src').mkdir(parents=True)
(workspace / '.cargo').mkdir()
(workspace / '.cargo/config.toml').write_text('[build]\nrustc-wrapper=' + json.dumps(str(repository_wrapper)) + '\n')
(workspace / 'Cargo.toml').write_text('[package]\nname="managed-fixture"\nversion="0.1.0"\nedition="2024"\n'
                                      '[workspace]\n[dependencies]\nmanaged-dependency={git=' + json.dumps(dependency.as_uri()) + '}\n'
                                      'local-macro={path=' + json.dumps(str(local_macro)) + '}\n')
(workspace / 'build.rs').write_text('fn main() {\n'
    'std::fs::write(std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("build-script-ran"), "trusted").unwrap();\n'
    'println!("cargo:rustc-cfg=managed_generated");\n}\n')
(workspace / 'src/lib.rs').write_text('pub fn entry() -> usize { managed_dependency::restored_value() }\n'
                                       '#[cfg(managed_generated)] pub fn compiler_prepared() -> usize { 42 }\n'
                                       'pub fn macro_user() -> usize { local_macro::literal_value!() }\n')

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
assert all(item['cargo']['buildScripts']['useRustcWrapper'] is False and item['cargo']['buildJobs'] == 1 for item in options)
input_paths = {Path(filename) for frame in first['messages'] if 'compilation' in frame for filename in frame['compilation']['inputPaths']}
assert {local_macro / 'Cargo.toml', local_macro / 'src/lib.rs', local_macro / 'src/implementation.rs', local_macro / 'build.rs'} <= input_paths
assert not any(frame.get('source', {}).get('path', '').startswith('../local-macro/') for frame in first['messages']), 'dependency source became owned facts'

def compilation_count(log, crate):
    records = [json.loads(line) for line in log.read_text().splitlines()]
    assert records and all(record['jobs'] == '1' for record in records), records
    return sum(any(value == '--crate-name' and record['args'][index + 1] == crate
                   for index, value in enumerate(record['args'][:-1])) for record in records)

assert compilation_count(repository_log, 'build_script_build') > 0, 'repository wrapper was replaced'
assert compilation_count(environment_log, 'build_script_build') > 0, 'workspace wrapper was replaced'
macro_compilations = compilation_count(repository_log, 'local_macro')
assert macro_compilations > 0, 'compiler-integrated procedural macro preparation was absent'

# A normal source save reuses dependency setup, while the analyzer's own
# compiler preparation/cache still decides whether build inputs need updating.
with (workspace / 'src/lib.rs').open('a') as file:
    file.write('\npub fn saved_after_restore() -> usize { entry() }\n')
reused = analyze('saved-source-checkpoint', workspace, previous=preparations)
successful(reused)
assert not any('preparation' in frame or frame.get('progress', {}).get('stage') == 'restoring' for frame in reused['messages']), reused
assert any(symbol['name'] == 'saved_after_restore' for frame in reused['messages'] if 'facts' in frame for symbol in frame['facts']['symbols'])

with (local_macro / 'src/implementation.rs').open('a') as file:
    file.write('// external local proc-macro source saved\n')
macro_saved = analyze('external-macro-source-save', workspace, previous=preparations)
successful(macro_saved)
assert compilation_count(repository_log, 'local_macro') > macro_compilations, 'saved proc-macro source reused stale compiler preparation'
assert not any('preparation' in frame for frame in macro_saved['messages']), 'source save reran dependency setup'

plan = Path(preparations[0]['outputs'][0])
plan.unlink()
recreated = analyze('missing-derived-plan', workspace, previous=preparations)
successful(recreated)
assert plan.is_file() and any(frame.get('preparation', {}).get('succeeded') for frame in recreated['messages']), recreated

# Matching fingerprints do not make malformed derived data authoritative.
for label, mutation in [
    ('matching-plan-wrong-outputs', lambda value: {**value, 'outputs': 'not-an-array'}),
    ('matching-plan-wrong-inputs', lambda value: {**value, 'inputs': None}),
    ('matching-plan-wrong-metadata', lambda value: {**value, 'metadata': {'packages': [None]}}),
]:
    valid = json.loads(plan.read_text())
    plan.write_text(json.dumps(mutation(valid)))
    repaired = analyze(label, workspace, previous=preparations)
    successful(repaired)
    assert isinstance(json.loads(plan.read_text())['outputs'], list)
    assert any(frame.get('preparation', {}).get('succeeded') for frame in repaired['messages']), repaired

with plan.open('wb') as file:
    file.write(b' ' * (8 * 1024 * 1024 + 1))
oversized = analyze('oversized-derived-plan', workspace, previous=preparations)
successful(oversized)
assert plan.stat().st_size < 8 * 1024 * 1024

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
    'repository/environment wrappers preserved with one Cargo build job', 'local proc-macro inputs and saved-source recompilation',
    'missing/malformed/oversized derived plan recreated', 'failed preparation explicit retry']}, indent=2))
print(ARTIFACTS)
