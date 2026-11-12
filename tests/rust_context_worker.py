#!/usr/bin/env python3
"""Actual managed Rust worker context/source/call assertions; build owner only."""
import base64
import json
import os
from pathlib import Path
import shutil
import subprocess
import time

ROOT = Path(__file__).resolve().parents[1]
ARTIFACTS = ROOT / '.artifacts/rust-context-worker' / str(time.time_ns())
WORKSPACE = ARTIFACTS / 'fixture'
(WORKSPACE / 'src').mkdir(parents=True)
(WORKSPACE / 'tests').mkdir()
(WORKSPACE / 'Cargo.toml').write_text('[package]\nname="owned-worker-contexts"\nversion="0.1.0"\nedition="2024"\n[workspace]\n')
(WORKSPACE / 'src/lib.rs').write_text(
    '#[cfg(test)] pub fn unit_leaf() -> usize { 22 }\n'
    '#[cfg(not(test))] pub fn unit_leaf() -> usize { 11 }\n'
    'pub fn entry() -> usize { unit_leaf() }\n'
    '#[derive(Clone, Default)] pub struct Derived;\n'
    'pub fn builtin_derive() { let value = Derived::default(); let _ = value.clone(); }\n'
    'macro_rules! generated { () => { pub fn generated_a() -> usize { 3 } pub fn generated_b() -> usize { 4 } }; }\n'
    'generated!();\npub fn uses_generated() -> usize { generated_a() + generated_b() }\n')
(WORKSPACE / 'src/main.rs').write_text('fn main() { let _ = owned_worker_contexts::entry(); }\n')
(WORKSPACE / 'tests/integration.rs').write_text('#[test] fn integration_call() { let _ = owned_worker_contexts::entry(); }\n')
NODE = shutil.which('node')
assert NODE


def analyze(label, preparations=()):
    request = {'workspaceRoot': str(WORKSPACE), 'projects': [str(WORKSPACE / 'Cargo.toml')],
               'configuration': 'Debug', 'preparationFingerprint': 'owned-context-worker-v1',
               'preparations': list(preparations),
               'analysisPolicy': {'version': 1, 'rust': {'executionAllowed': True,
                                                       'cacheDirectory': str(ARTIFACTS / 'cache')}}}
    result = subprocess.run([NODE, str(ROOT / 'tests/worker_harness.mjs'), str(ROOT / 'providers/rust/worker.mjs')],
                            input=json.dumps(request), cwd=ROOT, capture_output=True, text=True, timeout=190,
                            env=dict(os.environ), check=True)
    payload = json.loads(result.stdout)
    (ARTIFACTS / f'{label}.json').write_text(json.dumps(payload, indent=2))
    assert payload['code'] == 0, payload
    assert any(frame.get('complete', {}).get('success') for frame in payload['messages'])
    return payload


def validate(payload):
    compilations = {frame['compilation']['id']: frame['compilation'] for frame in payload['messages'] if 'compilation' in frame}
    assert len(compilations) == 5, [(item['target'], item['optionsJson']) for item in compilations.values()]
    sources, symbols, edges, declarations = {}, {}, {}, {}
    for frame in payload['messages']:
        if 'source' in frame:
            item = frame['source']
            sources.setdefault(item['compilationId'], {})[item['id']] = (item['path'], base64.b64decode(item['utf8']))
        if 'facts' in frame:
            item = frame['facts']
            symbols.setdefault(item['compilationId'], {}).update((symbol['id'], symbol) for symbol in item['symbols'])
            edges.setdefault(item['compilationId'], []).extend(item['relationships'])
            declarations.setdefault(item['compilationId'], []).extend(occurrence for occurrence in item['occurrences'] if occurrence['role'] == 'declaration')
    for identity, compilation in compilations.items():
        options = json.loads(compilation['optionsJson'])
        context = options['context']
        assert len(compilation['inputFingerprint']) == 64
        assert options['sourceOwnership'].startswith('Selected compiler unit')
        assert options['analyzerReplay']['buildScripts'] is False
        paths = {filename for filename, _ in sources[identity].values()}
        kind, tested = context['target']['kind'], context['profile']['test']
        expected_path = 'src/lib.rs' if kind == ['lib'] else 'src/main.rs' if kind == ['bin'] else 'tests/integration.rs'
        assert paths == {expected_path}, (compilation['target'], paths)
        if kind == ['lib']:
            names = symbols[identity]
            assert len([value for value in names.values() if value['name'] == 'unit_leaf' and not value.get('external')]) == 1, 'inactive declaration leaked into the selected context'
            assert not any(value['name'] == 'Derived' and value['kind'] in ('function', 'method') for value in names.values()), 'derive target reused a type declaration anchor'
            assert not {'generated_a', 'generated_b'} & {value['name'] for value in names.values()}, 'unproven expansion declaration identity was published'
            entry = next(key for key, value in names.items() if value['name'] == 'entry' and not value.get('external'))
            calls = [edge for edge in edges[identity] if edge['sourceId'] == entry and edge['kind'] == 'call']
            assert len(calls) == 1, calls
            target = calls[0]['targetId']
            assert names[target]['name'] == 'unit_leaf' and not names[target].get('external')
            declaration = next(item for item in declarations[identity] if item['symbolId'] == target)
            text = sources[identity][declaration['artifactId']][1]
            assert text[declaration.get('startByte', 0):].startswith(
                b'unit_leaf() -> usize { 22 }' if tested else b'unit_leaf() -> usize { 11 }'), (compilation['target'], declaration)
        else:
            dependency_entry = [value for value in symbols[identity].values() if value['name'] == 'entry']
            assert dependency_entry and all(value.get('external') for value in dependency_entry), dependency_entry
            assert all(not value.get('definitionCompilationId') for value in dependency_entry), 'unproved body link was emitted'
    return compilations


first = analyze('cold')
before = validate(first)
preparations = [frame['preparation'] for frame in first['messages'] if 'preparation' in frame]
with (WORKSPACE / 'src/lib.rs').open('a') as file:
    file.write('pub fn saved_after_capture() -> usize { entry() }\n')
saved = analyze('saved', preparations)
after = validate(saved)
assert set(before) == set(after), 'logical unit identities changed on an ordinary save'
assert any(before[key]['inputFingerprint'] != after[key]['inputFingerprint'] for key in before)
assert not any('preparation' in frame for frame in saved['messages']), 'source save reran dependency setup'
(ARTIFACTS / 'summary.json').write_text(json.dumps({'passed': True, 'assertions': [
    'five actual compiler contexts', 'cfg-selected direct calls at exact saved declaration bytes',
    'root-unit source ownership without dependency body multiplication', 'lookup-only dependency identities',
    'builtin derive and multi-declaration macro origins cannot reuse authored anchors',
    'logical identities survive save while fingerprints change', 'dependency setup reuse'],
    'limits': ['no cross-unit body-link promotion', 'no macro expansion identity certification']}, indent=2))
print(ARTIFACTS)
