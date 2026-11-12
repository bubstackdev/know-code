"""Compiler-backed Swift worker contract tests; retained work stays in .artifacts."""
import json
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
RUN = ROOT / '.artifacts' / 'swift-provider' / str(time.time_ns())
WORKSPACE = RUN / 'workspace'
WORKSPACE.mkdir(parents=True)
shutil.copytree(ROOT / 'tests/fixtures/swift', WORKSPACE, dirs_exist_ok=True)
PROFILE = os.environ.get('KNOW_CODE_BUILD_PROFILE', 'debug')
ENV = dict(os.environ, KNOW_CODE_DATA_DIR=str(RUN / 'data'), KNOW_CODE_BUILD_PROFILE=PROFILE)
ENV['KNOW_CODE_QUERY_SCOPE']=str(RUN/'query-admission')
HARNESS = ROOT / 'tests/worker_harness.mjs'
# Actual compiler forms include unlocated option warnings. They must not turn
# otherwise healthy source analysis into a compiler-error report.
diagnostic_check = subprocess.run(['node', '--input-type=module', '-e', '''
import assert from 'node:assert/strict';
import {compilerDiagnostics} from './providers/swift/index.mjs';
const root = process.cwd();
const warnings = compilerDiagnostics('<unknown>:0: warning: ignoring -experimental-lazy-typecheck (requires -enable-library-evolution)\\nwarning: driver warning\\n', root);
assert.deepEqual(warnings.map(d => [d.severity, d.path]), [['warning', ''], ['warning', '']]);
const mixed = compilerDiagnostics(`${root}/Sample.swift:3:7: error: missing value\\n<unknown>:0: note: compiler context\\nremark: compiler remark\\n`, root);
assert.deepEqual(mixed.map(d => [d.severity, d.path]), [['error', 'Sample.swift'], ['info', ''], ['info', '']]);
assert.equal(compilerDiagnostics('unrecognized compiler failure', root)[0].severity, 'error');
'''], cwd=ROOT, env=ENV, capture_output=True, text=True, timeout=10)
assert diagnostic_check.returncode == 0, diagnostic_check.stderr
subprocess.run([sys.executable, ROOT / 'scripts/build_record.py', 'verify', PROFILE], cwd=ROOT, check=True, stdout=subprocess.DEVNULL)
READER = ROOT / f'.artifacts/build/{PROFILE}/swift/indexstore-reader'
reader_sha256 = hashlib.sha256(READER.read_bytes()).hexdigest()


def invoke(name, sources, projects=None, configured=None):
    request = {'workspaceRoot': str(WORKSPACE), 'projects': projects or [], 'configuration': 'Debug',
               'looseSources': [str(source) for source in sources], 'configuredProjects': configured or []}
    request_path = RUN / f'{name}-request.json'
    request_path.write_text(json.dumps(request))
    result = subprocess.run(['node', HARNESS, ROOT / 'providers/swift/worker.mjs'], input=json.dumps(request), cwd=ROOT, env=ENV,
                            capture_output=True, text=True, timeout=120)
    assert result.returncode == 0, result.stderr
    output = json.loads(result.stdout)
    (RUN / f'{name}.json').write_text(json.dumps(output, indent=2))
    assert output['code'] == 0, output['stderr']
    assert output['messages'][-1]['complete']['success']
    return output['messages']


# Exercise byte-column conversion across a UTF-8 BOM and CRLF boundaries.
sample = WORKSPACE / 'Sample.swift'
sample.write_bytes(b'\xef\xbb\xbf' + sample.read_bytes().replace(b'\n', b'\r\n'))
sources = sorted(WORKSPACE.glob('*.swift'))
original = {source: source.read_bytes() for source in sources}
messages = invoke('initial', sources)
compilations = [message['compilation'] for message in messages if 'compilation' in message]
reports = [message['report'] for message in messages if 'report' in message]
assert len(compilations) == 1 and compilations[0]['projectPath'] == '$swift-inferred:.'
assert reports[0]['status'] == 'complete', reports
assert compilations[0]['inferred'] and 'Swift version 6.' in compilations[0]['compilerVersion']
assert json.loads(compilations[0]['optionsJson'])['signatures'] == 'compiler_index_names_only'
symbols = {symbol['id']: symbol for message in messages if 'facts' in message for symbol in message['facts']['symbols']}
occurrences = [occ for message in messages if 'facts' in message for occ in message['facts']['occurrences']]
relationships = [edge for message in messages if 'facts' in message for edge in message['facts']['relationships']]
artifacts = {message['source']['id']: message['source'] for message in messages if 'source' in message}
assert len(artifacts) == len(sources)
for name in ('name', 'message', 'café'):
    matched = [symbol for symbol in symbols.values() if symbol['name'] == name]
    assert matched and all(not symbol['external'] for symbol in matched), (name, matched)
    assert any(occ['role'] == 'reference' and occ['symbolId'] in {s['id'] for s in matched} for occ in occurrences)
# Distinct compiler USRs resolve overloads; both source calls point to their own declaration.
choices = [symbol for symbol in symbols.values() if symbol['name'] == 'choose(_:)']
assert len(choices) == 2, choices
assert all(len([occ for occ in occurrences if occ['symbolId'] == symbol['id'] and occ['role'] == 'reference']) == 1 for symbol in choices)
assert any(symbol['name'] == 'String' and symbol['external'] for symbol in symbols.values())
# A function value reference is not an invocation. Calls use compiler CALLEDBY
# evidence; implicit accessors retain that distinction and source-bound sites.
leaf = next(s['id'] for s in symbols.values() if s['name'] == 'swiftGraphLeaf(_:)')
direct = next(s['id'] for s in symbols.values() if s['name'] == 'swiftGraphDirect()')
reference = next(s['id'] for s in symbols.values() if s['name'] == 'swiftGraphReference()')
assert any(e['sourceId'] == direct and e['targetId'] == leaf and e['kind'] == 'call' for e in relationships)
assert not any(e['sourceId'] == reference and e['targetId'] == leaf and e['kind'] == 'call' for e in relationships)
assert any(e['sourceId'] == reference and e['targetId'] == leaf and e['kind'] == 'reference' for e in relationships)
for name in ('swiftGraphLoaded', 'swiftGraphClosure', 'stored'):
    owner = next(s['id'] for s in symbols.values() if s['name'] == name)
    assert not any(e['sourceId'] == owner and e['targetId'] == leaf and e['kind'] == 'call' for e in relationships)
    assert any(e['sourceId'] == owner and e['targetId'] == leaf and e['kind'] == 'reference' for e in relationships)
assert any(d['code'] == 'swift_graph_evidence_unavailable' for d in reports[0]['diagnostics'])
assert any(e['kind'] == 'call' and e['implicit'] and symbols[e['targetId']]['name'] == 'getter:computed' for e in relationships)
assert any(e['kind'] == 'type_reference' for e in relationships)
assert any(e['kind'] == 'inherits' and symbols[e['sourceId']]['name'] == 'SwiftGraphDerived'
           and symbols[e['targetId']]['name'] == 'SwiftGraphBase' for e in relationships)
assert reports[0]['capabilities'] == {'calls': 'partial', 'dependencies': 'partial', 'dispatch': 'partial', 'project_links': 'unavailable'}
for edge in relationships:
    assert edge['resolution'] == 'compiler_bound'
    assert edge['sourceId'] in symbols and edge['targetId'] in symbols
    payload = __import__('base64').b64decode(artifacts[edge['artifactId']]['utf8'])
    assert payload[edge['startByte']:edge['endByte']].decode('utf8'), edge
import base64
checked = 0
for occurrence in occurrences:
    source = artifacts[occurrence['artifactId']]
    token = base64.b64decode(source['utf8'])[occurrence['startByte']:occurrence['endByte']].decode('utf8')
    name = symbols[occurrence['symbolId']]['name']
    assert token and '\n' not in token, occurrence
    if name in ('Café', 'café', 'name', 'message'):
        assert token == name, (name, token)
        checked += 1
assert checked >= 8, checked
# Swift IndexStore anchors syntax sugar on punctuation: keep the exact source
# token and compiler binding. Adjacent postfix '+' and '..<' stay distinct.
anchor_tokens = {}
for occurrence in occurrences:
    source = artifacts[occurrence['artifactId']]
    if source['path'] != 'Anchors.swift':
        continue
    symbol = symbols[occurrence['symbolId']]
    token = base64.b64decode(source['utf8'])[occurrence['startByte']:occurrence['endByte']].decode('utf8')
    anchor_tokens.setdefault((symbol['name'], occurrence['role']), set()).add(token)
assert '[' in anchor_tokens[('subscript(_:)', 'reference')], anchor_tokens
assert '{' in anchor_tokens[('getter:size', 'declaration')], anchor_tokens
assert '..<' in anchor_tokens[('..<(_:_:)', 'reference')], anchor_tokens
assert '...' in anchor_tokens[('...(_:_:)', 'reference')], anchor_tokens
assert '+' in anchor_tokens[('+(_:)', 'reference')], anchor_tokens
assert all('+..<' not in tokens for tokens in anchor_tokens.values()), anchor_tokens
# Selected inferred group does not pull unrelated directory facts into publication.
other = WORKSPACE / 'other'
other.mkdir()
(other / 'Other.swift').write_text('func other() -> Int { 42 }\n')
scoped = invoke('scoped', sources + [other / 'Other.swift'], ['$swift-inferred:.'])
assert len([m for m in scoped if 'compilation' in m]) == 1
assert not any(s['name'] == 'other()' for m in scoped if 'facts' in m for s in m['facts']['symbols'])
deleted = invoke('last-file-deleted', [], ['$swift-inferred:.'])
assert not any('compilation' in m or 'facts' in m for m in deleted)
# Compiler errors retain exact references from healthy regions and flag partial coverage.
broken = WORKSPACE / 'Broken.swift'
broken.write_text('func broken() -> Int { missingSymbol }\n')
partial = invoke('partial', sources + [broken])
assert any(m['report']['status'] == 'partial' and any('missingSymbol' in d['message'] for d in m['report']['diagnostics']) for m in partial if 'report' in m)
assert any(s['name'] == 'choose(_:)' for m in partial if 'facts' in m for s in m['facts']['symbols'])
assert not any(s['name'] == 'missingSymbol' for m in partial if 'facts' in m for s in m['facts']['symbols'])
# Explicit project ownership never falls back to standalone inference, including scoped runs.
package = WORKSPACE / 'Package.swift'
package.write_text('// swift-tools-version: 6.0\nimport PackageDescription\nlet package = Package(name: "Fixture")\n')
packaged = invoke('packaged', sources, [str(package)], [str(package)])
assert len([m for m in packaged if 'compilation' in m]) == 1
assert any(d['code'] in ('swiftpm_no_swift_targets', 'swiftpm_manifest_failed') for m in packaged if 'report' in m for d in m['report']['diagnostics'])
assert not any('facts' in m or 'source' in m for m in packaged)
assert all(source.read_bytes() == content for source, content in original.items())
assert not any((RUN / 'data/workers/swift').glob('job-*')), 'compiler scratch must be reclaimed'
assert hashlib.sha256(READER.read_bytes()).hexdigest() == reader_sha256, 'test changed the built reader'
summary = {'passed': True, 'run': str(RUN), 'profile': PROFILE, 'symbols': len(symbols), 'reader_sha256': reader_sha256,
           'occurrences': len(occurrences), 'unicode_ranges_checked': checked,
           'relationships': len(relationships), 'graph_capabilities': reports[0]['capabilities'],
           'compiler': compilations[0]['compilerVersion'], 'scope': 'standalone_directory_groups',
           'explicit_packages': 'empty_package_partial_without_inference'}
(RUN / 'summary.json').write_text(json.dumps(summary, indent=2))
(ROOT / '.artifacts/swift-provider/latest.json').write_text(json.dumps(summary, indent=2))
print(json.dumps(summary))
