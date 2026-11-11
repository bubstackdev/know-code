"""Real compiler -> staged SQLite -> in-process .NET/Node equivalence checks.

All retained test work lives in .artifacts, never a temporary system directory.
"""
import atexit
import concurrent.futures
import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import time

from index_paths import physical_database

ROOT = Path(__file__).resolve().parents[1]
RUN = ROOT / '.artifacts' / 'e2e' / str(time.time_ns())
WORKSPACE = RUN / 'workspace'
WORKSPACE.mkdir(parents=True)
for language in ('typescript', 'csharp'):
    shutil.copytree(ROOT / 'tests' / 'fixtures' / language, WORKSPACE / language,
                    ignore=shutil.ignore_patterns('obj', 'bin', 'node_modules'))
ENV = dict(os.environ, KNOW_CODE_DATA_DIR=str(RUN / 'data'), DOTNET_CLI_TELEMETRY_OPTOUT='1', DOTNET_NOLOGO='1')
PROFILE = os.environ.get('KNOW_CODE_BUILD_PROFILE', 'debug')
HOST = ROOT / f'target/{PROFILE}/know-code'
SMOKE = ROOT / f'bindings/dotnet/Smoke/bin/{PROFILE.capitalize()}/net10.0/Smoke.dll'


def run(arguments, success=True):
    process = subprocess.run([str(a) for a in arguments], cwd=ROOT, env=ENV,
                             capture_output=True, text=True, timeout=120)
    if success and process.returncode:
        raise AssertionError(f'{arguments}\n{process.stdout}\n{process.stderr}')
    if not success:
        assert process.returncode, 'invalid worker input unexpectedly published'
    return process


def cleanup():
    subprocess.run([str(HOST), "coordinator", "stop"], cwd=ROOT, env=ENV, capture_output=True, timeout=15)


atexit.register(cleanup)


def index():
    start = time.perf_counter()
    process = run([HOST, 'index', WORKSPACE])
    output = json.loads(process.stdout)
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        statuses = json.loads(run([HOST, "coordinator", "status"]).stdout)["workspaces"]
        status = next(s for s in statuses if s["database"] == output["database"])
        if status["phase"] == "idle":
            output["generation"] = status["generation"]
            break
        assert status["phase"] != "failed", status
        time.sleep(0.05)
    else:
        raise AssertionError("coordinator did not settle")
    return output, time.perf_counter() - start


initial, elapsed = index()
DATABASE = initial['database']


def equivalent(request):
    # This broad fixture expands full diagnostics and exact target support.
    # Default/minimum envelope limits are exercised in response_budget.py.
    request = dict(request, coverage_detail="full", response_max_bytes=2 * 1024 * 1024)
    encoded = json.dumps(request)
    commands = ([HOST, 'query', DATABASE, encoded],
                ['node', ROOT / f'.artifacts/build/{PROFILE}/node/smoke.mjs', DATABASE, encoded],
                ['dotnet', SMOKE, DATABASE, encoded])
    with concurrent.futures.ThreadPoolExecutor(max_workers=3) as pool:
        responses = list(pool.map(lambda c: json.loads(run(c).stdout), commands))
    assert responses[0] == responses[1] == responses[2], request
    return responses[0]


all_symbols = equivalent({'op': 'symbols', 'limit': 1000})['result']
assert all_symbols['freshness'] == 'current', 'coordinator did not acknowledge observed inputs'
coverage = all_symbols['coverage']
assert {c['target'] for c in coverage if c['language'] == 'csharp'} == {'net10.0', 'netstandard2.1'}, coverage
assert all(c['status'] == 'complete' for c in coverage), coverage
CS_TARGETS = {'net10.0', 'netstandard2.1'}


def assert_csharp_support(symbol):
    supports = symbol['supporting_targets']
    assert len(supports) == 2 and {s['target'] for s in supports} == CS_TARGETS, symbol
    assert len({s['symbol_id'] for s in supports}) == 2, symbol
    assert all(s['locations'] and s['signature'] == symbol['signature'] for s in supports), symbol


checked_ranges = 0
for name in ('greet', 'Greet'):
    grouped = equivalent({'op': 'symbols', 'name': name})['result']['symbols']
    assert len(grouped) == 1, grouped
    if name == 'Greet':
        assert_csharp_support(grouped[0])
    for target in (sorted(CS_TARGETS) if name == 'Greet' else [None]):
        scope = {'target': target} if target else {}
        symbols = equivalent({'op': 'symbols', 'name': name, **scope})['result']['symbols']
        assert len(symbols) == 1 and not symbols[0].get('supporting_targets'), symbols
        for symbol in symbols:
            assert symbol['locations'], symbol
            references = equivalent({'op': 'references', 'symbol_id': symbol['id'], **scope})['result']['occurrences']
            assert references, symbol
            for item in references:
                location = item['location']
                source = equivalent({'op': 'source', 'artifact_id': location['artifact_id']})['result']['source']
                token = source['text'].encode('utf8')[location['start_byte']:location['end_byte']].decode('utf8')
                assert token == name, (token, item)
                assert source['content_hash'] == location['content_hash']
                checked_ranges += 1

# File-local identities stay distinct while equivalent target bindings group.
for name in ('cachedArg', 'cachedLocal'):
    grouped = equivalent({'op': 'symbols', 'name': name})['result']['symbols']
    assert len(grouped) == 2 and len({s['id'] for s in grouped}) == 2, grouped
    for symbol in grouped:
        assert_csharp_support(symbol)
    for target in sorted(CS_TARGETS):
        symbols = equivalent({'op': 'symbols', 'name': name, 'target': target})['result']['symbols']
        assert len(symbols) == 2 and len({s['id'] for s in symbols}) == 2, symbols
        for symbol in symbols:
            assert symbol['target'] == target and not symbol.get('supporting_targets'), symbol
            occurrences = equivalent({'op': 'references', 'symbol_id': symbol['id'], 'target': target})['result']['occurrences']
            assert len([o for o in occurrences if o['role'] == 'reference']) == 2, (symbol, occurrences)
            assert len({o['location']['path'] for o in occurrences}) == 1, occurrences

# Invocations preserve exact method/callable identities in each target and
# expose equivalent cross-target findings with both supporting identities.
for name, kind, reference_count in (
        ('InvocationGeneric', 'method', 1), ('InvocationExtension', 'method', 2),
        ('InvocationLocal', 'method', 1), ('InvocationMember', 'method', 1),
        ('invocationDelegate', 'local', 1), ('InvocationCallback', 'property', 1),
        ('InvocationField', 'field', 1)):
    grouped = equivalent({'op': 'symbols', 'name': name})['result']['symbols']
    assert len(grouped) == 1 and grouped[0]['kind'] == kind, (name, grouped)
    assert_csharp_support(grouped[0])
    for target in sorted(CS_TARGETS):
        symbols = equivalent({'op': 'symbols', 'name': name, 'target': target})['result']['symbols']
        assert len(symbols) == 1 and symbols[0]['kind'] == kind and not symbols[0].get('supporting_targets'), (name, symbols)
        occurrences = equivalent({'op': 'references', 'symbol_id': symbols[0]['id'], 'target': target})['result']['occurrences']
        assert len([o for o in occurrences if o['role'] == 'reference']) == reference_count, (name, target, occurrences)
grouped_overloads = equivalent({'op': 'symbols', 'name': 'InvocationTarget'})['result']['symbols']
assert len(grouped_overloads) == 2, grouped_overloads
for symbol in grouped_overloads:
    assert_csharp_support(symbol)
for target in sorted(CS_TARGETS):
    overloads = equivalent({'op': 'symbols', 'name': 'InvocationTarget', 'target': target})['result']['symbols']
    assert len(overloads) == 2, (target, overloads)
    for symbol in overloads:
        occurrences = equivalent({'op': 'references', 'symbol_id': symbol['id'], 'target': target})['result']['occurrences']
        expected = 2 if symbol['signature'].endswith('(int)') else 1
        assert len([o for o in occurrences if o['role'] == 'reference']) == expected, (symbol, occurrences)

platform = equivalent({'op': 'symbols', 'name': 'PlatformSpecific'})['result']['symbols']
assert len(platform) == 1 and platform[0]['target'] == 'net10.0', platform
assert not equivalent({'op': 'symbols', 'name': 'PlatformSpecific', 'target': 'netstandard2.1'})['result'].get('symbols')
generated = equivalent({'op': 'symbols', 'name': 'PayloadContext'})['result']['symbols']
generated_locations = [loc for symbol in generated for loc in symbol['locations'] if loc['generated']]
assert generated_locations, 'compiler-only source-generator documents were not indexed'
generated_source = equivalent({'op': 'source', 'artifact_id': generated_locations[0]['artifact_id']})['result']['source']
assert generated_source['path'].startswith('$generated/') and 'PayloadContext' in generated_source['text']
assert equivalent({'op': 'symbols', 'query': 'Greeter'})['result'].get('symbols'), 'FTS5 search returned no symbols'
assert equivalent({'op': 'references'})['error']['code'] == 'invalid_request'
assert equivalent({'op': 'symbols', 'limit': 0})['error']['code'] == 'invalid_request'

with sqlite3.connect(physical_database(DATABASE)) as connection:
    source_copies = connection.execute('SELECT count(*) FROM artifacts').fetchone()[0]
    source_payloads = connection.execute('SELECT count(*) FROM contents').fetchone()[0]
    assert source_payloads < source_copies, (source_payloads, source_copies)
    assert connection.execute('PRAGMA journal_mode').fetchone()[0] == 'wal'
    assert connection.execute('PRAGMA foreign_key_check').fetchall() == []

# A malformed worker project must not replace the previously published graph.
config = WORKSPACE / 'typescript/tsconfig.json'
saved_config = config.read_text()
config.write_text('{ invalid JSON')
run([HOST, 'index', WORKSPACE], success=False)
assert equivalent({'op': 'symbols', 'name': 'Greet'})['result']['generation'] == initial['generation']
config.write_text(saved_config)

# A compiler diagnostic is a completed partial analysis, not a crashed worker.
broken = WORKSPACE / 'typescript/src/broken.ts'
broken.write_text('export const problem: number = missingValue;\n')
broken_calls = WORKSPACE / 'csharp/BrokenInvocations.cs'
broken_calls.write_text('''using System;
class BrokenInvocations {
    static void AmbiguousInvocationTarget(string value) { }
    static void AmbiguousInvocationTarget(Uri value) { }
    static void Run() { AmbiguousInvocationTarget(null); }
}
''')
partial, partial_seconds = index()
assert partial['generation'] > initial['generation']
partial_result = equivalent({'op': 'symbols', 'name': 'greet'})['result']
assert any(c['language'] == 'typescript' and c['status'] == 'partial' and c['diagnostics'] for c in partial_result['coverage'])
assert partial_result['symbols'], partial_result
assert all(c['status'] == 'partial' for c in partial_result['coverage'] if c['language'] == 'csharp'), partial_result
ambiguous = equivalent({'op': 'symbols', 'name': 'AmbiguousInvocationTarget'})['result']['symbols']
assert len(ambiguous) == 2, ambiguous
for symbol in ambiguous:
    assert_csharp_support(symbol)
for target in sorted(CS_TARGETS):
    ambiguous = equivalent({'op': 'symbols', 'name': 'AmbiguousInvocationTarget', 'target': target})['result']['symbols']
    assert len(ambiguous) == 2, (target, ambiguous)
    for symbol in ambiguous:
        assert symbol['locations'], symbol
        references = equivalent({'op': 'references', 'symbol_id': symbol['id'], 'target': target})['result']
        assert not references.get('occurrences'), references

# Inference is compiler-backed and never writes a synthetic tsconfig.
broken.unlink()
broken_calls.unlink()
config.unlink()
inferred, inferred_seconds = index()
assert not config.exists()
inferred_result = equivalent({'op': 'symbols', 'name': 'greet'})['result']
assert any(c['language'] == 'typescript' and c['inferred'] for c in inferred_result['coverage'])
assert inferred_result['symbols']
assert all(c['analysis_options']['strict'] for c in inferred_result['coverage'] if c['inferred'])

summary = {'workspace': str(WORKSPACE), 'database': DATABASE, 'checked_utf8_reference_ranges': checked_ranges,
           'initial_index_seconds_including_restore': round(elapsed, 3), 'partial_refresh_seconds': round(partial_seconds, 3),
           'inferred_refresh_seconds': round(inferred_seconds, 3), 'source_artifacts': source_copies,
           'unique_source_payloads': source_payloads, 'result': 'passed'}
summary['compiler_generated_source_checked'] = True
(RUN / 'result.json').write_text(json.dumps(summary, indent=2) + '\n')
(ROOT / '.artifacts/e2e/latest.json').write_text(json.dumps(summary, indent=2) + '\n')
print(json.dumps(summary, indent=2))
