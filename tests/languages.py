"""Four providers share publication, native queries, and saved-input invalidation."""
from fixture_trust import grant_fixture
import atexit
import hashlib
import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import time

from index_paths import physical_database
import re

ROOT = Path(__file__).resolve().parents[1]
RUN = ROOT / '.artifacts/languages' / str(time.time_ns())
WORKSPACE = RUN / 'workspace'
WORKSPACE.mkdir(parents=True)
PROFILE = os.environ.get('KNOW_CODE_BUILD_PROFILE', 'debug')
HOST = ROOT / f'target/{PROFILE}/know-code'
ENV = dict(os.environ, KNOW_CODE_DATA_DIR=str(RUN / 'data'),
           DOTNET_CLI_TELEMETRY_OPTOUT='1', DOTNET_NOLOGO='1')
ENV['KNOW_CODE_QUERY_SCOPE']=str(RUN/'query-admission')
ENV.pop('SDKROOT', None)  # Exercise default selection, not Python's injected SDK.
for language in ('typescript', 'csharp', 'rust', 'swift'):
    shutil.copytree(ROOT / 'tests/fixtures' / language, WORKSPACE / language,
                    ignore=shutil.ignore_patterns('obj', 'bin', 'target', '.build', 'node_modules'))
for directory, filename in (('.build-ci/checkouts/dependency', 'Package.swift'),
                            ('.worktrees/other', 'Cargo.toml'), ('DerivedData/cached', 'Package.swift')):
    excluded = WORKSPACE / directory / filename
    excluded.parent.mkdir(parents=True)
    excluded.write_text('deliberately invalid cached project metadata\n')
package = WORKSPACE / 'swift-package'
(package / 'Sources/PackageModel').mkdir(parents=True)
(package / 'Tests/PackageModelTests').mkdir(parents=True)
manifest = '''// swift-tools-version: 6.0
import PackageDescription
let package = Package(name: "PackageModel", targets: [
 .target(name: "PackageModel"), .testTarget(name: "PackageModelTests", dependencies: ["PackageModel"])])
'''
(package / 'Package.swift').write_text(manifest)
(package / 'Package@swift-6.0.swift').write_text(manifest)
package_source = package / 'Sources/PackageModel/Model.swift'
package_source.write_text('public func packageValue() -> Int { let packageLocal = 7; return packageLocal }\n')
(package / 'Tests/PackageModelTests/ModelTests.swift').write_text('@testable import PackageModel\nfunc checkPackage() -> Int { packageValue() }\n')


def run(args, check=True):
    result = subprocess.run([str(a) for a in args], cwd=ROOT, env=ENV,
                            capture_output=True, text=True, timeout=180)
    assert not check or result.returncode == 0, (args, result.stdout, result.stderr)
    return result


def stop():
    run([HOST, 'coordinator', 'stop'], check=False)


atexit.register(stop)
grant_fixture(HOST, WORKSPACE, ENV, RUN, RUN/'fixture-trust.jsonl')
registered = json.loads(run([HOST, 'watch', WORKSPACE]).stdout)
DATABASE = registered['database']


def wait(after=0, allow_failed=False):
    until = time.monotonic() + 120
    while time.monotonic() < until:
        status = json.loads(run([HOST, 'coordinator', 'status']).stdout)['workspaces'][0]
        assert allow_failed or status['phase'] != 'failed', status
        if (status['phase'] == 'idle' and status['generation'] > after
                and status['published_epoch'] == status['observed_epoch']):
            return status['generation']
        time.sleep(.05)
    raise AssertionError(('language indexing did not settle', status))


def query(request, equivalent=False):
    encoded = json.dumps(request)
    response = json.loads(run([HOST, 'query', DATABASE, encoded]).stdout)
    assert 'result' in response, response
    if equivalent:
        for args in (
            ['node', ROOT / f'.artifacts/build/{PROFILE}/node/smoke.mjs'],
            ['dotnet', ROOT / f'bindings/dotnet/Smoke/bin/{PROFILE.capitalize()}/net10.0/Smoke.dll'],
        ):
            assert json.loads(run([*args, DATABASE, encoded]).stdout) == response
    return response['result']


def owners():
    with sqlite3.connect(physical_database(DATABASE)) as connection:
        return dict(connection.execute('SELECT compilation_id,owner_generation FROM generation_units '
                                       'WHERE generation=(SELECT current_generation FROM state)'))


def check_ranges(name, provider, target=None):
    symbols = [symbol for symbol in query({'op': 'symbols', 'name': name, 'target': target}, equivalent=True)['symbols']
               if symbol['compilation_id'].startswith(provider + ':')]
    assert symbols, name
    count = 0
    for symbol in symbols:
        references = query({'op': 'references', 'symbol_id': symbol['id']}, equivalent=True)['occurrences']
        assert any(item['role'] == 'reference' for item in references), (name, references)
        for item in references:
            location = item['location']
            source = query({'op': 'source', 'artifact_id': location['artifact_id']}, equivalent=True)['source']
            token = source['text'].encode()[location['start_byte']:location['end_byte']].decode()
            assert token == name, (name, token, item)
            count += 1
    return count


generation = wait()
# Request workspace coverage explicitly; symbol inventory size must not turn
# this cross-provider coverage/parity check into an oversized response.
coverage = query({'op': 'symbols', 'limit': 1, 'coverage_detail': 'full', 'detail_max_bytes': 65536}, equivalent=True)['coverage']
assert {item['language'] for item in coverage} == {'typescript', 'csharp', 'rust', 'swift'}, coverage
assert all(item['status'] == 'partial' for item in coverage if item['language'] == 'rust'), coverage
assert all(item['status'] == 'complete' for item in coverage if item['language'] != 'rust'), coverage
rust_contexts = [item for item in coverage if item['language'] == 'rust']
# This fixture declares a single library with its ordinary and test compiler
# units. Tooling/build-script units supply preparation, not duplicate bodies.
assert len(rust_contexts) == 2, rust_contexts
assert {item['target'] for item in rust_contexts} == {
    'lib:know_code_rust_fixture:normal@host', 'lib:know_code_rust_fixture:test@host'}, rust_contexts
assert all(item['analysis_options']['context']['target']['kind'] == ['lib'] for item in rust_contexts), rust_contexts
assert {item['analysis_options']['context']['profile']['test'] for item in rust_contexts} == {False, True}, rust_contexts
normal_rust = next(item for item in rust_contexts if not item['analysis_options']['context']['profile']['test'])
normal_rust_target = normal_rust['target']
with sqlite3.connect(physical_database(DATABASE)) as connection:
    rust_sources = connection.execute(
        'SELECT a.path,count(DISTINCT u.compilation_id),count(DISTINCT a.rowid),count(DISTINCT a.content_hash) '
        'FROM generation_units u JOIN compilations c ON c.generation=u.owner_generation AND c.id=u.compilation_id '
        'JOIN unit_artifacts ua ON ua.owner_generation=u.owner_generation AND ua.compilation_id=u.compilation_id '
        'JOIN artifacts a ON a.generation=ua.owner_generation AND a.id=ua.artifact_id '
        'WHERE u.generation=(SELECT current_generation FROM state) AND c.language=\'rust\' GROUP BY a.path').fetchall()
assert {row[0] for row in rust_sources} == {'rust/src/lib.rs', 'rust/src/helper.rs'}, rust_sources
assert all(row[1:] == (2, 1, 1) for row in rust_sources), 'Rust contexts must share retained source artifacts/bytes'
checked = check_ranges('café', 'rust', normal_rust_target) + check_ranges('message', 'swift') + check_ranges('packageLocal', 'swift')
assert sum(item['language'] == 'swift' and item['project_path'] == 'swift-package/Package.swift'
           for item in coverage) == 2, coverage
for language, name, expected in [('rust','Greeter','included'),('swift','message','extent_unavailable')]:
    symbol = next(s for s in query({'op':'symbols','name':name, 'target': normal_rust_target if language == 'rust' else None})['symbols']
                  if s['compilation_id'].startswith(language+':'))
    context = query({'op':'symbol_context','symbol_id':symbol['id']}, equivalent=True)['contexts'][0]
    assert context['declarations'] and any(d['source_status']==expected for d in context['declarations']), context
print('languages: four-provider native parity and exact retained UTF-8 ranges passed', flush=True)

# The same stored traversal and native SDKs must preserve compiler evidence
# from both new adapters, including the difference between calls and values.
for language, leaf_name, direct_name, reference_name in [
        ('swift', 'swiftGraphLeaf(_:)', 'swiftGraphDirect()', 'swiftGraphReference()'),
        ('rust', 'graph_leaf', 'graph_entry', 'graph_value_only')]:
    selected = {}
    for name in (leaf_name, direct_name, reference_name):
        matches = [symbol for symbol in query({'op': 'symbols', 'name': name}, equivalent=True).get('symbols', [])
                   if symbol['name'] == name and symbol['compilation_id'].startswith(language + ':')
                   and (language != 'rust' or symbol['compilation_id'] == normal_rust['compilation_id'])]
        assert len(matches) == 1, (language, name, matches)
        selected[name] = matches[0]['id']
    leaf_id, direct_id, reference_id = (selected[name] for name in (leaf_name, direct_name, reference_name))
    callers = query({'op': 'callers', 'symbol_id': leaf_id}, equivalent=True)['graph']
    assert reference_id not in {finding['symbol']['id'] for finding in callers['findings']}, callers
    direct = next(finding for finding in callers['findings'] if finding['symbol']['id'] == direct_id)
    assert direct['paths'] and all(edge['kind'] == 'call' and edge['resolution'] == 'compiler_bound'
                                   for path in direct['paths'] for edge in path), direct
    callees = query({'op': 'callees', 'symbol_id': direct_id, 'max_depth': 1}, equivalent=True)['graph']
    called = next(finding for finding in callees['findings'] if finding['symbol']['id'] == leaf_id)
    assert called['paths'] and all(edge['source_id'] == direct_id and edge['target_id'] == leaf_id
                                  and edge['kind'] == 'call' and edge['resolution'] == 'compiler_bound'
                                  for path in called['paths'] for edge in path), called
    assert not query({'op': 'callees', 'symbol_id': reference_id}, equivalent=True)['graph']['findings']
    impact = query({'op': 'impact', 'symbol_id': leaf_id}, equivalent=True)['graph']
    assert {direct_id, reference_id} <= {finding['symbol']['id'] for finding in impact['findings']}, impact
    value_use = next(finding for finding in impact['findings'] if finding['symbol']['id'] == reference_id)
    assert value_use['paths'] and all(edge['kind'] == 'reference' and edge['resolution'] == 'compiler_bound'
                                    for path in value_use['paths'] for edge in path), value_use
    reports = [report for report in query({'op': 'symbols', 'name': leaf_name, 'coverage_detail': 'full'}, equivalent=True)['coverage']
               if report['language'] == language]
    assert reports and all(report['capabilities']['calls'] == 'partial'
                           and report['capabilities']['dependencies'] == 'partial'
                           and report['capabilities']['dispatch'] == 'partial' for report in reports), reports
    if language == 'rust':
        assert all(report['status'] == 'partial' for report in reports), reports
        constants = [symbol for symbol in query({'op': 'symbols', 'name': 'GRAPH_VALUE'}, equivalent=True).get('symbols', [])
                     if symbol['name'] == 'GRAPH_VALUE' and symbol['compilation_id'] == normal_rust['compilation_id']]
        assert len(constants) == 1, constants
        constant_impact = query({'op': 'impact', 'symbol_id': constants[0]['id'], 'kinds': ['reference']}, equivalent=True)['graph']
        for identifier in (leaf_id, direct_id):
            finding = next(item for item in constant_impact['findings'] if item['symbol']['id'] == identifier)
            assert finding['paths'] and all(edge['kind'] == 'reference' and edge['resolution'] == 'compiler_bound'
                                           for path in finding['paths'] for edge in path), finding
    else:
        assert all(report['capabilities']['project_links'] == 'unavailable' for report in reports), reports
        for name in ('swiftGraphLoaded', 'swiftGraphClosure'):
            values = [symbol for symbol in query({'op': 'symbols', 'name': name}, equivalent=True).get('symbols', [])
                      if symbol['name'] == name and symbol['compilation_id'].startswith('swift:')]
            assert len(values) == 1, (name, values)
            assert not query({'op': 'callees', 'symbol_id': values[0]['id']}, equivalent=True)['graph']['findings']
print('languages: stored Swift/Rust calls, value impact and honest graph capability parity passed', flush=True)

before = owners()
rust = WORKSPACE / 'rust/src/lib.rs'
rust.write_text(rust.read_text() + '\n// saved Rust change\n')
generation = wait(generation)
after = owners()
rust_units = {key for key in before if key.startswith('rust:')}
assert len(rust_units) == 2 and {key for key in after if key.startswith('rust:')} == rust_units
assert all(after[key] != before[key] for key in rust_units)
assert all(after[key] == value for key, value in before.items() if not key.startswith('rust:'))

# Legacy Cargo configuration is extensionless; discovery must notice its creation.
config = WORKSPACE / 'rust/.cargo/config'
config.parent.mkdir()
config.write_text('# saved Cargo configuration input\n')
generation = wait(generation)
changed = owners()
assert {key for key in changed if key.startswith('rust:')} == rust_units
assert all(changed[key] != after[key] for key in rust_units)
after = owners()

before = after
swift = WORKSPACE / 'swift/Sample.swift'
swift.write_text(swift.read_text().replace('message', 'reply'))
generation = wait(generation)
after = owners()
assert after['swift:$swift-inferred:swift'] != before['swift:$swift-inferred:swift']
assert all(after[key] == value for key, value in before.items() if not key.startswith('swift:'))
assert not any(symbol['compilation_id'].startswith('swift:')
               for symbol in query({'op': 'symbols', 'name': 'message'})['symbols'])
checked += check_ranges('reply', 'swift')

before = owners()
package_source.write_text(package_source.read_text().replace('packageLocal', 'packageSaved'))
generation = wait(generation)
after = owners()
assert all(after[key] != value for key, value in before.items() if key.startswith('swift:swift-package/Package.swift:'))
assert all(after[key] == value for key, value in before.items() if not key.startswith('swift:swift-package/Package.swift:'))
assert not query({'op': 'symbols', 'name': 'packageLocal'}).get('symbols')
checked += check_ranges('packageSaved', 'swift')

# Creating, repairing and deleting a saved compiler pin uses ordinary shared
# invalidation; an unavailable pin preserves the previous coherent generation.
pin = package / '.swift-version'
pin.write_text('99.99.99\n')
until = time.monotonic() + 120
while time.monotonic() < until:
    state = json.loads(run([HOST, 'coordinator', 'status']).stdout)['workspaces'][0]
    if state['phase'] == 'failed':
        break
    time.sleep(.05)
else:
    raise AssertionError(('unavailable Swift pin did not fail', state))
assert state['generation'] == generation and '.swift-version' in state['error'], state
assert query({'op': 'symbols', 'name': 'packageSaved'})['freshness'] == 'stale'
compiler = next(item['compiler_version'] for item in coverage if item['language'] == 'swift')
pin.write_text(re.search(r'Swift version (\d+\.\d+\.\d+)', compiler).group(1) + '\n')
generation = wait(generation, allow_failed=True)
scoped = query({'op': 'symbols', 'name': 'packageSaved','coverage_detail':'full', 'detail_max_bytes':65536}, equivalent=True)['coverage']
assert all(item['analysis_options']['toolchainSelection']['origin'] == 'repository_pin'
           for item in scoped if item['project_path'] == 'swift-package/Package.swift')
pin.unlink()
generation = wait(generation)
assert all(item['analysis_options']['toolchainSelection']['swiftPin'] is None
           for item in query({'op': 'symbols','limit':1,'coverage_detail':'full', 'detail_max_bytes':65536})['coverage'] if item['project_path'] == 'swift-package/Package.swift')

added = WORKSPACE / 'swift/new/New.swift'
added.parent.mkdir()
added.write_text('struct AddedSwift {}\n')
generation = wait(generation)
assert query({'op': 'symbols', 'name': 'AddedSwift'})['symbols']
before = owners()
added.unlink()
generation = wait(generation)
assert not query({'op': 'symbols', 'name': 'AddedSwift'}).get('symbols')
after = owners()
assert 'swift:$swift-inferred:swift/new' not in after
assert all(after[key] == value for key, value in before.items() if key != 'swift:$swift-inferred:swift/new')

before = owners()
healthy = swift.read_text()
swift.write_text(healthy + '\nlet broken: Int = "wrong type"\n')
generation = wait(generation)
coverage = query({'op': 'symbols', 'name': 'reply'})['coverage']
assert any(item['language'] == 'swift' and item['status'] == 'partial' for item in coverage)
assert all(owners()[key] == value for key, value in before.items() if not key.startswith('swift:'))
swift.write_text(healthy)
generation = wait(generation)
assert all(item['status'] == 'complete' for item in query({'op': 'symbols'})['coverage'] if item['language'] == 'swift')

# Keep one real cursor across removal of all owned language inputs. The saved
# sources move outside discovery, but remain durable evidence for pinned reads.
# Creating separate queries creates separate opaque cursors. Establish one
# cursor, then compare native clients while replaying that same token.
retained = query({'op': 'symbols', 'name': 'reply', 'limit': 1, 'page_size': 1})
assert retained['generation'] == generation and len(retained['symbols']) == 1, retained
cursor = retained['page']['cursor']
location = retained['symbols'][0]['locations'][0]
saved_swift = swift.read_bytes()
assert saved_swift[location['start_byte']:location['end_byte']].decode() == 'reply', location
retained_page = {key: value for key, value in retained.items() if key not in ('freshness', 'coordination')}
initial_replay = query({'op': 'next_page', 'cursor': cursor}, equivalent=True)
assert initial_replay['freshness'] == 'current', initial_replay
assert {key: value for key, value in initial_replay.items() if key not in ('freshness', 'coordination')} == retained_page
removed = RUN / 'removed-inputs'
removed.mkdir()
for directory in ('typescript', 'csharp', 'rust', 'swift', 'swift-package'):
    shutil.move(str(WORKSPACE / directory), removed / directory)
generation = wait(generation, allow_failed=True)


def check_empty():
    result = query({'op': 'symbols', 'coverage_detail': 'full'}, equivalent=True)
    assert result['freshness'] == 'current' and result['generation'] == generation, result
    assert not result.get('symbols', []) and not result['coverage'], result
    assert result['workspace_coverage']['compilations'] == 0 and not owners(), result


check_empty()
replayed = query({'op': 'next_page', 'cursor': cursor}, equivalent=True)
assert replayed['freshness'] == 'stale', replayed
assert {key: value for key, value in replayed.items() if key not in ('freshness', 'coordination')} == retained_page
pinned = query({'op': 'source', 'cursor': cursor, 'artifact_id': location['artifact_id'],
                'content_hash': location['content_hash'], 'start_byte': 0,
                'max_bytes': len(saved_swift) + 1}, equivalent=True)
assert pinned['generation'] == retained['generation'] and pinned['freshness'] == 'stale', pinned
body = pinned['source']
assert body['text'].encode() == saved_swift and body['start_byte'] == 0 and body['end_byte'] == len(saved_swift), body
assert body['content_hash'] == location['content_hash'] == hashlib.sha256(saved_swift).hexdigest(), body
run([HOST, 'index', WORKSPACE])
generation = wait(generation)
check_empty()
stop()
until = time.monotonic() + 15
while (RUN / 'data/c.sock').exists() and time.monotonic() < until:
    time.sleep(.05)
assert not (RUN / 'data/c.sock').exists(), 'Isolated coordinator did not stop'
assert json.loads(run([HOST, 'watch', WORKSPACE]).stdout)['database'] == DATABASE
generation = wait(generation - 1)
check_empty()
# Cursors are deliberately not reused after the coordinator restart.
restored_source = WORKSPACE / 'restored.ts'
restored_source.write_text('export function restoredAfterEmpty() { return 7; }\n')
generation = wait(generation)
restored = query({'op': 'symbols', 'name': 'restoredAfterEmpty'}, equivalent=True)
assert restored['freshness'] == 'current' and restored['symbols'], restored
assert {item['language'] for item in restored['coverage']} == {'typescript'}, restored
assert owners(), 'Saved standalone TypeScript source did not restore a compilation'

summary = {'result': 'passed', 'profile': PROFILE, 'database': DATABASE,
           'checked_utf8_ranges': checked, 'rust_coverage': 'explicitly partial',
           'checks': ['four_provider_publication', 'native_parity', 'UTF8_source_ranges', 'build_cache_and_worktree_exclusions',
                      'Swift_Rust_stored_call_and_impact_native_parity', 'compiler_bound_paths_and_value_reference_separation',
                      'Rust_constant_dependency_impact', 'Rust_normal_and_test_compiler_contexts_share_source_storage',
                      'Swift_value_initializers_do_not_invent_calls',
                      'Rust_scoped_save', 'extensionless_Cargo_config', 'Swift_scoped_save', 'SwiftPM_target_save',
                      'Swift_pin_failure_repair_and_removal', 'new_Swift_group',
                      'last_Swift_file_deletion_without_sibling_reanalysis', 'Swift_partial_and_repair',
                      'all_inputs_removed_empty_current_manifest_and_native_parity',
                      'retained_page_and_exact_UTF8_source_across_empty_publication',
                      'explicit_empty_refresh_and_restart', 'saved_source_rediscovery_after_empty']}
(RUN / 'result.json').write_text(json.dumps(summary, indent=2) + '\n')
(ROOT / '.artifacts/languages/latest.json').write_text(json.dumps(summary, indent=2) + '\n')
print(json.dumps(summary, indent=2))
