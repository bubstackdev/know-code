"""Four providers share publication, native queries, and saved-input invalidation."""
import atexit
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
registered = json.loads(run([HOST, 'watch', WORKSPACE]).stdout)
DATABASE = registered['database']


def wait(after=0, allow_failed=False):
    until = time.monotonic() + 120
    while time.monotonic() < until:
        status = json.loads(run([HOST, 'coordinator', 'status']).stdout)['workspaces'][0]
        assert allow_failed or status['phase'] != 'failed', status
        if status['phase'] == 'idle' and status['generation'] > after:
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


def check_ranges(name, provider):
    symbols = [symbol for symbol in query({'op': 'symbols', 'name': name}, equivalent=True)['symbols']
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
coverage = query({'op': 'symbols', 'limit': 1000}, equivalent=True)['coverage']
assert {item['language'] for item in coverage} == {'typescript', 'csharp', 'rust', 'swift'}, coverage
assert all(item['status'] == 'partial' for item in coverage if item['language'] == 'rust'), coverage
assert all(item['status'] == 'complete' for item in coverage if item['language'] != 'rust'), coverage
checked = check_ranges('café', 'rust') + check_ranges('message', 'swift') + check_ranges('packageLocal', 'swift')
assert sum(item['language'] == 'swift' and item['project_path'] == 'swift-package/Package.swift'
           for item in coverage) == 2, coverage
for language, name, expected in [('rust','Greeter','included'),('swift','message','extent_unavailable')]:
    symbol = next(s for s in query({'op':'symbols','name':name})['symbols'] if s['compilation_id'].startswith(language+':'))
    context = query({'op':'symbol_context','symbol_id':symbol['id']}, equivalent=True)['contexts'][0]
    assert context['declarations'] and any(d['source_status']==expected for d in context['declarations']), context
print('languages: four-provider native parity and exact retained UTF-8 ranges passed', flush=True)

before = owners()
rust = WORKSPACE / 'rust/src/lib.rs'
rust.write_text(rust.read_text() + '\n// saved Rust change\n')
generation = wait(generation)
after = owners()
assert after['rust:rust/Cargo.toml'] != before['rust:rust/Cargo.toml']
assert all(after[key] == value for key, value in before.items() if not key.startswith('rust:'))

# Legacy Cargo configuration is extensionless; discovery must notice its creation.
config = WORKSPACE / 'rust/.cargo/config'
config.parent.mkdir()
config.write_text('# saved Cargo configuration input\n')
generation = wait(generation)
assert owners()['rust:rust/Cargo.toml'] != after['rust:rust/Cargo.toml']
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
scoped = query({'op': 'symbols', 'name': 'packageSaved','coverage_detail':'full'}, equivalent=True)['coverage']
assert all(item['analysis_options']['toolchainSelection']['origin'] == 'repository_pin'
           for item in scoped if item['project_path'] == 'swift-package/Package.swift')
pin.unlink()
generation = wait(generation)
assert all(item['analysis_options']['toolchainSelection']['swiftPin'] is None
           for item in query({'op': 'symbols','coverage_detail':'full'})['coverage'] if item['project_path'] == 'swift-package/Package.swift')

added = WORKSPACE / 'swift/new/New.swift'
added.parent.mkdir()
added.write_text('struct AddedSwift {}\n')
generation = wait(generation)
assert query({'op': 'symbols', 'name': 'AddedSwift'})['symbols']
added.unlink()
generation = wait(generation)
assert not query({'op': 'symbols', 'name': 'AddedSwift'}).get('symbols')
assert 'swift:$swift-inferred:swift/new' not in owners()

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

summary = {'result': 'passed', 'profile': PROFILE, 'database': DATABASE,
           'checked_utf8_ranges': checked, 'rust_coverage': 'explicitly partial',
           'checks': ['four_provider_publication', 'native_parity', 'UTF8_source_ranges', 'build_cache_and_worktree_exclusions',
                      'Rust_scoped_save', 'extensionless_Cargo_config', 'Swift_scoped_save', 'SwiftPM_target_save',
                      'Swift_pin_failure_repair_and_removal', 'new_Swift_group',
                      'last_Swift_file_deletion', 'Swift_partial_and_repair']}
(RUN / 'result.json').write_text(json.dumps(summary, indent=2) + '\n')
(ROOT / '.artifacts/languages/latest.json').write_text(json.dumps(summary, indent=2) + '\n')
print(json.dumps(summary, indent=2))
