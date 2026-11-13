"""Owned Xcode compiler-to-native integration; requires the separate bridge.

The validation owner supplies KNOW_CODE_TEST_XCODE_BRIDGE (executable or its
runtime directory). This never installs a tool or touches the live coordinator.

Installed mode: --installed-prefix COMPLETED_SIGNED_PACKAGE_TEST --run-directory
NEW_SHORT_OWNED_ARTIFACTS_PATH. Also supply KNOW_CODE_TEST_XCODE_BRIDGE_PACKAGES
for the authenticated separate bridge stage. Product children cannot read any
source checkout; controller-only signing verification reuses sign_packages.py.
"""
from fixture_trust import grant_fixture
from index_paths import physical_database
from xcode_fixture import create
from contextlib import closing
import atexit
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import plistlib
import sqlite3
import subprocess
import sys
import time


ROOT = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--installed-prefix', type=Path, help='Completed signed package_install.py prefix outside checkouts')
parser.add_argument('--run-directory', type=Path, help='New short durable owned directory containing .artifacts')
args = parser.parse_args()
assert bool(args.installed_prefix) == bool(args.run_directory), 'installed mode requires both paths'
INSTALLED = args.installed_prefix.resolve(strict=True) if args.installed_prefix else None
PACKAGE_RESULT = json.loads((INSTALLED / 'result.json').read_text()) if INSTALLED else None
CHECKOUTS = sorted({ROOT.resolve(), *[Path(value).resolve(strict=True) for value in
                                   (PACKAGE_RESULT.get('denied_checkouts', []) if INSTALLED else [])]})
RUN = args.run_directory.resolve() if INSTALLED else ROOT / '.artifacts/xcode-integration' / str(time.time_ns())
if INSTALLED:
    assert PACKAGE_RESULT.get('passed') and (PACKAGE_RESULT.get('signing_verification') or {}).get('verified'), 'a completed signed installed-package pass is required'
    assert INSTALLED == Path(PACKAGE_RESULT['prefix']).resolve()
    assert '.artifacts' in RUN.parts and not RUN.exists(), 'use a new durable owned .artifacts run directory'
    assert all(not directory.is_relative_to(checkout) for directory in [INSTALLED, RUN] for checkout in CHECKOUTS)
RUN.mkdir(parents=True, exist_ok=False)
PROJECT = create(RUN / 'workspace')
WORKSPACE = PROJECT.parent
PROFILE = 'release' if INSTALLED else os.environ.get('KNOW_CODE_BUILD_PROFILE', 'debug')
HOST = INSTALLED / 'bin/know-code' if INSTALLED else ROOT / f'target/{PROFILE}/know-code'
BRIDGE = Path(os.environ['KNOW_CODE_TEST_XCODE_BRIDGE']).expanduser().resolve(strict=True)
if BRIDGE.is_dir():
    BRIDGE = BRIDGE / 'sourcekit-xcode-bsp'
BRIDGE = BRIDGE.resolve(strict=True)
assert BRIDGE.is_file(), BRIDGE
if INSTALLED:
    assert all(not BRIDGE.is_relative_to(checkout) for checkout in CHECKOUTS), 'installed tool must be outside source checkouts'
BIN = RUN / 'bin'
BIN.mkdir()
(BIN / 'sourcekit-xcode-bsp').symlink_to(BRIDGE)
DATA = RUN / 'physical-data'
DATA.mkdir()
DATA_ALIAS = RUN / 'data'
DATA_ALIAS.symlink_to(DATA, target_is_directory=True)
ENV = dict(os.environ, KNOW_CODE_DATA_DIR=str(DATA_ALIAS),
           KNOW_CODE_QUERY_SCOPE=str(RUN / 'query-admission'),
           PATH=str(BIN) + os.pathsep + os.environ['PATH'],
           DOTNET_CLI_TELEMETRY_OPTOUT='1', DOTNET_NOLOGO='1')
ENV.pop('SDKROOT', None)
if INSTALLED:
    for key in ['KNOW_CODE_RUNTIME_DIR', 'KNOW_CODE_COORDINATOR', 'NODE_PATH', 'NODE_OPTIONS']:
        ENV.pop(key, None)
    assert len(os.fsencode(DATA_ALIAS / 'c.sock')) < 104, 'choose a shorter owned run-directory for macOS sockets'
    POLICY = RUN / 'deny-checkout-and-network.sb'
    denied = ''.join('(deny file-read* file-write* (subpath ' + json.dumps(str(checkout)) + '))' for checkout in CHECKOUTS)
    POLICY.write_text('(version 1) (allow default) ' + denied +
        ' (deny network*) (allow network* (local unix-socket (subpath ' + json.dumps(str(RUN)) +
        '))) (allow network* (remote unix-socket (subpath ' + json.dumps(str(RUN)) + ')))\n')
INSTALLED_EVIDENCE = None
SDK_COMMANDS = None

# Extend the common owned fixture instead of maintaining a second PBX generator.
pbx = PROJECT / 'project.pbxproj'
document = plistlib.loads(pbx.read_bytes())
objects = document['objects']


def obj(key, isa, **fields):
    objects[key] = dict(isa=isa, **fields)
    return key


for name in ('Probe', 'ProbeUITests'):
    for configuration in ('Debug', 'Release'):
        settings = objects[name + configuration]['buildSettings']
        settings['PRODUCT_MODULE_NAME'] = name
        # Deliberately hostile project settings must lose to mandatory guards.
        settings['INDEX_DISABLE_SCRIPT_EXECUTION'] = 'NO'
        settings['INDEX_FORCE_SCRIPT_EXECUTION'] = 'YES'

source = obj('DependencySource', 'PBXFileReference', path='Dependency.swift',
             lastKnownFileType='sourcecode.swift', sourceTree='<group>')
buildfile = obj('DependencyBuildFile', 'PBXBuildFile', fileRef=source)
sources = obj('DependencySources', 'PBXSourcesBuildPhase', files=[buildfile],
              buildActionMask='2147483647', runOnlyForDeploymentPostprocessing='0')
product = obj('DependencyProduct', 'PBXFileReference', path='ProbeDependency.framework',
              explicitFileType='wrapper.framework', sourceTree='BUILT_PRODUCTS_DIR')
configurations = []
for name in ('Debug', 'Release'):
    configurations.append(obj('Dependency' + name, 'XCBuildConfiguration', name=name,
        buildSettings={'PRODUCT_NAME': 'ProbeDependency', 'PRODUCT_MODULE_NAME': 'ProbeDependency',
                       'PRODUCT_BUNDLE_IDENTIFIER': 'dev.knowcode.ProbeDependency',
                       'DEFINES_MODULE': 'YES', 'GENERATE_INFOPLIST_FILE': 'YES',
                       'CODE_SIGNING_ALLOWED': 'NO', 'INDEX_DISABLE_SCRIPT_EXECUTION': 'NO',
                       'INDEX_FORCE_SCRIPT_EXECUTION': 'YES'}))
configuration_list = obj('DependencyConfigurations', 'XCConfigurationList',
    buildConfigurations=configurations, defaultConfigurationName='Debug',
    defaultConfigurationIsVisible='0')
dependency = obj('DependencyTarget', 'PBXNativeTarget', name='ProbeDependency',
    productName='ProbeDependency', buildConfigurationList=configuration_list,
    buildPhases=[sources], buildRules=[], dependencies=[], productReference=product,
    productType='com.apple.product-type.framework')
proxy = obj('DependencyProxy', 'PBXContainerItemProxy', containerPortal='Project',
            proxyType='1', remoteGlobalIDString=dependency, remoteInfo='ProbeDependency')
reference = obj('DependencyReference', 'PBXTargetDependency', target=dependency, targetProxy=proxy)
framework = obj('DependencyFrameworkBuildFile', 'PBXBuildFile', fileRef=product)
frameworks = obj('ProbeFrameworks', 'PBXFrameworksBuildPhase', files=[framework],
                 buildActionMask='2147483647', runOnlyForDeploymentPostprocessing='0')
objects['Project']['targets'].append(dependency)
objects['Main']['children'].insert(0, source)
objects['Products']['children'].append(product)
objects['ProbeTarget']['dependencies'].append(reference)
objects['ProbeTarget']['buildPhases'].append(frameworks)

# A tiny catalog requests compiler-generated Swift source without large assets.
catalog = WORKSPACE / 'FixtureAssets.xcassets'
(catalog / 'FixtureTint.colorset').mkdir(parents=True)
(catalog / 'Contents.json').write_text(json.dumps({'info': {'author': 'xcode', 'version': 1}}))
(catalog / 'FixtureTint.colorset/Contents.json').write_text(json.dumps({
    'info': {'author': 'xcode', 'version': 1}, 'colors': [{'idiom': 'universal',
    'color': {'color-space': 'srgb', 'components': {'red': '0.2', 'green': '0.4', 'blue': '0.6', 'alpha': '1.0'}}}]}))
asset = obj('FixtureAssets', 'PBXFileReference', path=catalog.name,
            lastKnownFileType='folder.assetcatalog', sourceTree='<group>')
asset_build = obj('FixtureAssetsBuild', 'PBXBuildFile', fileRef=asset)
resources = obj('ProbeResources', 'PBXResourcesBuildPhase', files=[asset_build],
                buildActionMask='2147483647', runOnlyForDeploymentPostprocessing='0')
objects['Main']['children'].insert(0, asset)
objects['ProbeTarget']['buildPhases'].append(resources)
for name in ('Debug', 'Release'):
    objects['Probe' + name]['buildSettings']['ASSETCATALOG_COMPILER_GENERATE_SWIFT_ASSET_SYMBOL_EXTENSIONS'] = 'YES'
pbx.write_bytes(plistlib.dumps(document))

APP = WORKSPACE / 'App.swift'
APP.write_text('''import Foundation
import ProbeDependency
#if targetEnvironment(simulator)
public func simulatorLeaf(_ value: Int) -> Int { value + 2 }
#else
public func deviceLeaf(_ value: Int) -> Int { value + 1 }
#endif
public func savedLabel() -> String { "café 🦀 saved-v1" }
public func contextEntry() -> Int {
  _ = savedLabel()
  #if targetEnvironment(simulator)
  return simulatorLeaf(1) + dependencyEntry()
  #else
  return deviceLeaf(1) + dependencyEntry()
  #endif
}
public func contextReference() -> Int { contextEntry() }
''')
(WORKSPACE / 'Dependency.swift').write_text('''public func dependencyLeaf() -> Int { 3 }
public func dependencyEntry() -> Int { dependencyLeaf() }
''')
(WORKSPACE / 'UITests.swift').write_text('''import XCTest
final class ProbeUITests: XCTestCase {
  func testLeaf() -> Int { 4 }
  func testExample() { XCTAssertEqual(testLeaf(), 4) }
}
''')

CHECKS = []
DATABASE = None


def run(argv, check=True, sandbox=True, cwd=None, timeout=45):
    command = (['/usr/bin/sandbox-exec', '-f', str(POLICY)] if INSTALLED and sandbox else []) + list(map(str, argv))
    result = subprocess.run(command, cwd=cwd or (RUN if INSTALLED else ROOT), env=ENV,
                            text=True, capture_output=True, timeout=timeout)
    with (RUN / 'commands.jsonl').open('a') as stream:
        stream.write(json.dumps({'argv': command, 'returncode': result.returncode,
                                 'stdout': result.stdout, 'stderr': result.stderr}) + '\n')
    assert not check or result.returncode == 0, (argv, result.stdout, result.stderr)
    return result


if INSTALLED:
    # Reuse the authenticated signing flow, including its complete archive and
    # provenance checks. Test-controller verification can read source; product
    # commands below always inherit checkout/network denial.
    signing = PACKAGE_RESULT['signing_verification']
    PACKAGES = Path(PACKAGE_RESULT['packages']).resolve(strict=True)
    common_work = RUN / 'common-verification'
    verified = json.loads(run([sys.executable, ROOT / 'scripts/sign_packages.py', 'verify', '--packages', PACKAGES,
        '--work-dir', common_work, '--team-id', signing['team_id'], '--signer-sha256', signing['signer_sha256']], sandbox=False, timeout=120).stdout)
    assert verified['verified'] and verified['manifest_sha256'] == PACKAGE_RESULT['candidate_manifest_sha256']
    manifest = json.loads((PACKAGES / 'manifest.json').read_text())
    assert manifest['profile'] == 'release' and {item['file']: item['sha256'] for item in manifest['artifacts']} == PACKAGE_RESULT['archive_sha256']
    receipt = json.loads((PACKAGES / 'signing-transform.json').read_text())
    inventories = {row['kind']: row['signed_inventory'] for row in receipt['archives']}
    bundle = HOST.resolve(strict=True).parent.parent
    node_package = INSTALLED / 'node-consumer/node_modules/@know-code/node'
    managed = INSTALLED / 'net-consumer/bin/Release/net10.0'
    mapped = []
    for kind, installed, archive_root in [('runtime', bundle, f"know-code-{manifest['version']}-darwin-arm64"),
                                           ('node', node_package, 'package')]:
        for name, expected in inventories[kind].items():
            if expected['kind'] == 'file':
                mapped.append((installed / Path(name).relative_to(archive_root), expected))
    for name, target in [('lib/net10.0/KnowCode.dll', managed / 'KnowCode.dll'),
                         ('runtimes/osx-arm64/native/libknow_code_ffi.dylib', managed / 'runtimes/osx-arm64/native/libknow_code_ffi.dylib')]:
        mapped.append((target, inventories['nuget'][name]))
    for file, expected in mapped:
        assert file.resolve(strict=True).is_relative_to(INSTALLED) and not file.is_symlink(), ('foreign installed payload', file)
        assert file.stat().st_size == expected['bytes'] and hashlib.sha256(file.read_bytes()).hexdigest() == expected['sha256'], file

    # The separate tool uses the signing owner's existing bridge verifier and
    # inventory helper. Missing signed-stage evidence is a hard fixture failure.
    bridge_packages_value = os.environ.get('KNOW_CODE_TEST_XCODE_BRIDGE_PACKAGES')
    assert bridge_packages_value, 'installed Xcode mode requires KNOW_CODE_TEST_XCODE_BRIDGE_PACKAGES for authenticated bridge verification'
    bridge_packages = Path(bridge_packages_value).resolve(strict=True)
    bridge_work = RUN / 'bridge-verification'
    bridge_verified = json.loads(run([sys.executable, ROOT / 'scripts/sign_packages.py', 'bridge-verify',
        '--packages', bridge_packages, '--work-dir', bridge_work, '--team-id', signing['team_id'],
        '--signer-sha256', signing['signer_sha256']], sandbox=False, timeout=120).stdout)
    assert bridge_verified['verified'] and bridge_verified['tool'] == 'know-code-xcode-bridge'
    spec = importlib.util.spec_from_file_location('xcode_installed_signing', ROOT / 'scripts/sign_packages.py')
    signing_module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(signing_module)
    assert signing_module.bridge_inventory(BRIDGE.parent) == bridge_verified['runtime_inventory'], 'installed bridge differs from authenticated archive'

    for checkout in CHECKOUTS:
        assert (checkout / 'LICENSE').is_file()
        assert run(['/bin/cat', checkout / 'LICENSE'], check=False).returncode != 0, ('checkout policy ineffective', checkout)
    deny_script = 'import fs from "node:fs";for(const p of process.argv.slice(1)){try{fs.readFileSync(p);throw Error("checkout readable: "+p)}catch(e){if(!["EACCES","EPERM"].includes(e.code))throw e}}'
    run(['node', '--input-type=module', '-e', deny_script, *[checkout / 'LICENSE' for checkout in CHECKOUTS]])
    runtime_info = json.loads(run([HOST, 'runtime-info']).stdout)
    assert runtime_info['layout'] == 'bundle' and runtime_info['profile'] == 'release'
    assert Path(runtime_info['resource_directory']).resolve() == bundle / 'lib/know-code'
    node_smoke = INSTALLED / 'node-consumer' / ('xcode-smoke-' + RUN.name + '.mjs')
    assert not node_smoke.exists()
    node_smoke.write_text('import {Index} from "@know-code/node";console.log(JSON.stringify(await new Index(process.argv[2]).query(JSON.parse(process.argv[3]))));\n')
    resolved = run(['node', '--input-type=module', '-e', 'console.log(import.meta.resolve("@know-code/node"))'], cwd=node_package.parent.parent.parent)
    from urllib.parse import unquote, urlparse
    assert Path(unquote(urlparse(resolved.stdout.strip()).path)).resolve().is_relative_to(node_package.resolve()), resolved.stdout
    SDK_COMMANDS = (['node', node_smoke], ['dotnet', managed / 'Consumer.dll'])
    INSTALLED_EVIDENCE = {'package_prefix': str(INSTALLED), 'candidate_manifest_sha256': verified['manifest_sha256'],
        'archives': PACKAGE_RESULT['archive_sha256'], 'runtime_info': runtime_info, 'bridge_verification': bridge_verified,
        'bridge_root': str(BRIDGE.parent), 'node_entry': resolved.stdout.strip(),
        'installed_payloads': {str(file): expected['sha256'] for file, expected in mapped},
        'policy_sha256': hashlib.sha256(POLICY.read_bytes()).hexdigest(), 'denied_checkouts': list(map(str, CHECKOUTS)),
        'notarization': 'separate distribution/security gate'}
    (RUN / 'installed-artifacts.json').write_text(json.dumps(INSTALLED_EVIDENCE, indent=2) + '\n')


def stop():
    run([HOST, 'coordinator', 'stop'], check=False)


atexit.register(stop)


def wait_status(label, predicate, timeout=240):
    deadline = time.monotonic() + timeout
    with (RUN / (label + '-status.jsonl')).open('x') as stream:
        while time.monotonic() < deadline:
            status = json.loads(run([HOST, 'coordinator', 'status']).stdout)['workspaces'][0]
            stream.write(json.dumps(status) + '\n')
            stream.flush()
            assert status['phase'] not in ('failed', 'resource_limited'), status
            if predicate(status):
                return status
            time.sleep(.1)
    raise AssertionError(('Xcode indexing did not settle', status))


def ready(label, after=0):
    return wait_status(label, lambda status: status['phase'] == 'idle' and status['generation'] > after
                       and status['observed_epoch'] == status['published_epoch'])['generation']


def query(request, equivalent=False, freshness='current'):
    request = {'response_max_bytes': 2 * 1024 * 1024, **request}
    encoded = json.dumps(request)
    response = json.loads(run([HOST, 'query', DATABASE, encoded]).stdout)
    assert 'result' in response and response['result']['freshness'] == freshness, response
    if equivalent:
        for argv in SDK_COMMANDS or (['node', ROOT / f'.artifacts/build/{PROFILE}/node/smoke.mjs'],
                                    ['dotnet', ROOT / f'bindings/dotnet/Smoke/bin/{PROFILE.capitalize()}/net10.0/Smoke.dll']):
            assert json.loads(run([*argv, DATABASE, encoded]).stdout) == response
    return response['result']


def expanded_coverage():
    anchor = query({'op': 'symbols', 'limit': 1}, equivalent=True)
    expanded = query({'op': 'coverage', 'generation': anchor['generation'],
                      'snapshot': anchor['snapshot'], 'max_bytes': 262144}, equivalent=True)
    assert expanded['generation'] == anchor['generation'] and expanded['snapshot'] == anchor['snapshot']
    return expanded['coverage']


def coverage(expected_total=6):
    rows = expanded_coverage()
    assert len(rows) == expected_total, rows
    rows = [row for row in rows if row['language'] == 'swift']
    assert len(rows) == 6, rows
    assert all(row['language'] == 'swift' and row['project_path'] == 'Probe.xcodeproj/project.pbxproj'
               and row['configuration'] == 'Debug' and row['analysis_options']['swiftExtractionPolicy'] == 1
               and row['analysis_options']['origin'] == 'xcode_build_server' for row in rows), rows
    assert {row['analysis_options']['platform'] for row in rows} == {'iphoneos', 'iphonesimulator'}
    assert all(sum(row['analysis_options']['platform'] == platform for row in rows) == 3
               for platform in ('iphoneos', 'iphonesimulator'))
    assert all(row['capabilities']['calls'] == row['capabilities']['dependencies']
               == row['capabilities']['dispatch'] == 'partial'
               and row['capabilities']['project_links'] == 'unavailable' for row in rows), rows
    return {row['compilation_id']: row for row in rows}


def symbols(name):
    return [symbol for symbol in query({'op': 'symbols', 'name': name, 'limit': 100}, equivalent=True).get('symbols', [])
            if not symbol['external']]


def graph_checks(contexts):
    entries = symbols('contextEntry()')
    assert len(entries) == 2, entries
    for entry in entries:
        platform = contexts[entry['compilation_id']]['analysis_options']['platform']
        expected = 'simulatorLeaf(_:)' if platform == 'iphonesimulator' else 'deviceLeaf(_:)'
        other = 'deviceLeaf(_:)' if platform == 'iphonesimulator' else 'simulatorLeaf(_:)'
        leaves = symbols(expected)
        leaf = next(value for value in leaves if value['compilation_id'] == entry['compilation_id'])
        callees = query({'op': 'callees', 'symbol_id': entry['id'], 'max_depth': 1}, equivalent=True)['graph']
        names = {finding['symbol']['name'] for finding in callees['findings']}
        assert expected in names and other not in names, (platform, callees)
        callers = query({'op': 'callers', 'symbol_id': leaf['id'], 'max_depth': 1}, equivalent=True)['graph']
        finding = next(value for value in callers['findings'] if value['symbol']['id'] == entry['id'])
        assert finding['paths'] and all(edge['kind'] == 'call' and edge['resolution'] == 'compiler_bound'
                                       for path in finding['paths'] for edge in path), finding
        impact = query({'op': 'impact', 'symbol_id': leaf['id']}, equivalent=True)['graph']
        assert entry['id'] in {finding['symbol']['id'] for finding in impact['findings']}
        references = query({'op': 'references', 'symbol_id': leaf['id']}, equivalent=True)
        occurrences = references['occurrences']
        assert any(item['role'] == 'reference' for item in occurrences), occurrences
        for occurrence in occurrences:
            location = occurrence['location']
            source = query({'op': 'source', 'artifact_id': location['artifact_id'],
                            'snapshot': references['snapshot'], 'content_hash': location['content_hash'],
                            'max_bytes': 65536}, equivalent=True)['source']
            assert 'café 🦀' in source['text'], source
            token = source['text'].encode()[location['start_byte']:location['end_byte']].decode()
            assert token == expected.split('(')[0], (token, occurrence)
    for name in ('dependencyEntry()', 'testExample()'):
        assert len(symbols(name)) == 2, name


def guards():
    assert not any((WORKSPACE / name).exists() for name in ('PHASE_EXECUTED', 'SCHEME_EXECUTED'))


grant_fixture(HOST, WORKSPACE, ENV, RUN, RUN / 'fixture-trust.jsonl', runner=run if INSTALLED else None)
DATABASE = json.loads(run([HOST, 'watch', WORKSPACE]).stdout)['database']
generation = ready('initial')
contexts = coverage()
assert all(not row['diagnostic_counts'].get('error', 0) for row in contexts.values()), contexts
graph_checks(contexts)
guards()
CHECKS += ['six_app_dependency_UI_contexts', 'saved_Test_configuration', 'native_CLI_Node_dotnet_parity',
           'compiler_selected_device_simulator_calls_and_impact', 'exact_UTF8_reference_source', 'script_guards']

generated = [origin for row in contexts.values() for origin in row['analysis_options']['sourceOrigins'] if origin['generated']]
assert generated, 'The owned asset catalog did not produce compiler-enrolled generated source'
assert {row['analysis_options']['platform'] for row in contexts.values()
        if any(origin['generated'] for origin in row['analysis_options']['sourceOrigins'])} == {'iphoneos', 'iphonesimulator'}
for origin in generated:
    source = query({'op': 'source', 'artifact_id': origin['artifactId'], 'max_bytes': 65536}, equivalent=True)['source']
    assert source['artifact_id'] == origin['artifactId'] and source['text'], source
generated_ids = {origin['artifactId'] for origin in generated}
asset_symbols = query({'op': 'symbols', 'query': 'fixtureTint', 'limit': 100}, equivalent=True).get('symbols', [])
assert any(location['artifact_id'] in generated_ids and location['generated']
           for symbol in asset_symbols for location in symbol.get('locations', [])), asset_symbols
CHECKS.append('compiler_generated_asset_source_retained_and_labeled')
with closing(sqlite3.connect(f'file:{physical_database(DATABASE)}?mode=ro', uri=True)) as connection:
    input_rows = connection.execute('SELECT c.id,c.input_paths_json FROM generation_units u JOIN compilations c '
        'ON c.generation=u.owner_generation AND c.id=u.compilation_id '
        "WHERE u.generation=(SELECT current_generation FROM state) AND c.language='swift'").fetchall()
assert len(input_rows) == 6, input_rows
saved_inputs = {compilation: json.loads(encoded) for compilation, encoded in input_rows}
assert all(not any(Path(file).is_relative_to(boundary) for boundary in (DATA, DATA_ALIAS))
           for inputs in saved_inputs.values() for file in inputs), saved_inputs
(RUN / 'saved-compilation-inputs.json').write_text(json.dumps(saved_inputs, indent=2))
CHECKS.append('data_alias_generated_sources_excluded_from_saved_compilation_inputs')

checkpoint = Path(DATABASE).with_suffix('.preparation.json')
preparation = (checkpoint.stat().st_mtime_ns, hashlib.sha256(checkpoint.read_bytes()).hexdigest())
saved_entries = {symbol['id'] for symbol in symbols('contextEntry()')}
healthy = APP.read_text().replace('saved-v1', 'saved-v2')
APP.write_text(healthy)
generation = ready('saved-body', generation)
assert coverage().keys() == contexts.keys(), 'Unchanged targets received different compilation identities'
assert {symbol['id'] for symbol in symbols('contextEntry()')} == saved_entries
assert (checkpoint.stat().st_mtime_ns, hashlib.sha256(checkpoint.read_bytes()).hexdigest()) == preparation, 'Body save repeated dependency restoration'
assert not any(json.loads(line).get('progress', {}).get('stage') == 'restoring'
               for line in (RUN / 'saved-body-status.jsonl').read_text().splitlines()
               if json.loads(line).get('progress'))
graph_checks(contexts)
CHECKS.append('saved_body_preserves_context_and_symbol_ids_without_dependency_restore')

APP.write_text(healthy + '\npublic func fixtureBroken() -> Int { missingFixtureValue }\n')
generation = ready('compiler-error', generation)
partial = coverage()
assert partial.keys() == contexts.keys()
assert any(row['status'] == 'partial' and row['diagnostic_counts'].get('error', 0) for row in partial.values()), partial
assert len(symbols('dependencyEntry()')) == 2 and len(symbols('contextEntry()')) == 2
CHECKS.append('compiler_error_is_fresh_partial_with_healthy_facts')
APP.write_text(healthy)
generation = ready('compiler-repair', generation)
repaired = coverage()
assert repaired.keys() == contexts.keys()
assert all(not row['diagnostic_counts'].get('error', 0) for row in repaired.values()), repaired
graph_checks(contexts)
guards()
CHECKS.append('saved_compiler_error_repair')

# A missing compiler-declared input is a typed preparation failure, not an
# invitation to keep old bodies in a newly current generation. Repair through
# the existing saved-file watcher, without an explicit refresh or new client.
dependency_source = WORKSPACE / 'Dependency.swift'
saved_dependency = dependency_source.read_bytes()
try:
    dependency_source.unlink()
    generation = ready('missing-compiler-source', generation)
    unavailable = expanded_coverage()
    assert len(unavailable) == 2, unavailable
    assert {row['analysis_options']['platform'] for row in unavailable} == {'iphoneos', 'iphonesimulator'}
    assert all(row['status'] == 'partial' and row['diagnostic_counts'].get('error', 0)
               and row['analysis_options']['origin'] == 'xcode'
               and row['analysis_options']['supported'] is False
               and row['analysis_options']['preparation'] == 'unavailable'
               and any(item['code'] == 'xcode_module_preparation_failed'
                       for item in row.get('diagnostics', [])) for row in unavailable), unavailable
    assert not symbols('dependencyEntry()') and not symbols('contextEntry()'), 'Prior bodies appeared in the current unavailable graph'
    guards()
    CHECKS.append('missing_compiler_source_publishes_typed_unavailable_SDK_contexts_without_prior_bodies')
finally:
    dependency_source.write_bytes(saved_dependency)
generation = ready('missing-compiler-source-repair', generation)
restored = coverage()
assert restored.keys() == contexts.keys()
assert all(not row['diagnostic_counts'].get('error', 0) for row in restored.values()), restored
assert dependency_source.read_bytes() == saved_dependency
graph_checks(contexts)
guards()
CHECKS.append('recreated_compiler_source_automatically_repairs_both_SDK_graphs')

# A rejected Xcode configuration must not discard an unrelated provider's new
# facts. The .7 bridge supplies the typed metadata-command rejection receipt;
# unexpected crashes/transport failures remain a different failure path.
sibling = WORKSPACE / 'sibling.ts'
sibling.write_text('export function healthySibling() { return 7; }\n')
generation = ready('healthy-sibling', generation)
assert coverage(expected_total=7).keys() == contexts.keys()
assert len(symbols('healthySibling')) == 1
saved_project = pbx.read_bytes()
try:
    pbx.write_text('this is deliberately invalid saved Xcode project data\n')
    generation = ready('rejected-xcode-project', generation)
    mixed = expanded_coverage()
    healthy = [row for row in mixed if row['language'] == 'typescript']
    unavailable = [row for row in mixed if row['language'] == 'swift']
    assert len(mixed) == 3 and len(healthy) == 1 and len(unavailable) == 2, mixed
    assert healthy[0]['status'] == 'complete' and not healthy[0]['diagnostic_counts'].get('error', 0), healthy
    assert {row['analysis_options']['platform'] for row in unavailable} == {'iphoneos', 'iphonesimulator'}
    assert all(row['status'] == 'partial' and row['analysis_options']['supported'] is False
               and row['analysis_options']['preparation'] == 'unavailable'
               and any(item['code'] in ('xcode_metadata_unavailable', 'dependency_restore_failed')
                       for item in row.get('diagnostics', [])) for row in unavailable), unavailable
    assert len(symbols('healthySibling')) == 1
    assert not symbols('contextEntry()') and not symbols('dependencyEntry()'), 'Rejected Xcode configuration retained old current bodies'
    guards()
    CHECKS.append('typed_Xcode_metadata_rejection_publishes_healthy_TypeScript_sibling')
finally:
    pbx.write_bytes(saved_project)
generation = ready('rejected-xcode-project-repair', generation)
restored = coverage(expected_total=7)
assert restored.keys() == contexts.keys()
assert all(not row['diagnostic_counts'].get('error', 0) for row in restored.values()), restored
assert pbx.read_bytes() == saved_project and len(symbols('healthySibling')) == 1
graph_checks(contexts)
guards()
CHECKS.append('saved_Xcode_project_repair_restores_both_SDK_graphs_beside_healthy_sibling')

# Observe real preparation before cancelling through the ordinary CLI. A
# cancellation that loses the commit race is not evidence that an active job
# was aborted; keep that outcome visible instead of silently retrying it.
retained = query({'op': 'symbols', 'name': 'contextEntry()'}, equivalent=True)
before_cancel = APP.read_bytes()
baseline = retained['generation']
APP.write_bytes(before_cancel + b'\npublic func cancelledCandidate() -> Int { contextEntry() }\n')
active = wait_status('cancel-active', lambda status:
    (status['generation'] == baseline and status['observed_epoch'] > status['published_epoch']
     and (status.get('progress') or {}).get('stage') == 'preparing')
    or status['generation'] > baseline, timeout=60)
assert active['generation'] == baseline, ('Cancellation window was not observed before publication', active)
acknowledged = json.loads(run([HOST, 'cancel', WORKSPACE]).stdout)
(RUN / 'cancel-acknowledgement.json').write_text(json.dumps(acknowledged, indent=2) + '\n')
assert acknowledged['phase'] in ('cancelling', 'cancelled'), ('Cancellation lost the commit race; no aborted-job gate is claimed', acknowledged)
assert acknowledged['generation'] == baseline, acknowledged
stopped = wait_status('cancel-complete', lambda status: status['phase'] == 'cancelled', timeout=30)
assert stopped['outcome_code'] == 'cancelled' and stopped['generation'] == baseline, stopped
assert all(json.loads(line)['generation'] == baseline
           for line in (RUN / 'cancel-complete-status.jsonl').read_text().splitlines())
stale = query({'op': 'symbols', 'name': 'contextEntry()', 'freshness_wait_ms': 0}, equivalent=True, freshness='stale')
assert stale['generation'] == baseline and stale['symbols'] == retained['symbols'], stale
assert not query({'op': 'symbols', 'name': 'cancelledCandidate()', 'freshness_wait_ms': 0}, equivalent=True, freshness='stale').get('symbols')
location = retained['symbols'][0]['locations'][0]
old_source = query({'op': 'source', 'artifact_id': location['artifact_id'], 'snapshot': retained['snapshot'],
                    'content_hash': location['content_hash'], 'max_bytes': 65536, 'freshness_wait_ms': 0},
                   equivalent=True, freshness='stale')['source']
assert old_source['text'].encode() == before_cancel and old_source['content_hash'] == hashlib.sha256(before_cancel).hexdigest(), old_source
guards()
CHECKS.append('CLI_active_Xcode_cancellation_preserves_generation_and_stale_graph_without_partial_publication')
# Save only after the durable cancelled acknowledgement; edits racing its final
# baseline check are deliberately outside this assertion's snapshot boundary.
APP.write_bytes(before_cancel + b'\npublic func resumedAfterCancellation() -> Int { contextEntry() }\n')
generation = ready('cancel-new-save-recovery', baseline)
assert coverage(expected_total=7).keys() == contexts.keys()
assert len(symbols('resumedAfterCancellation()')) == 2 and not symbols('cancelledCandidate()')
graph_checks(contexts)
guards()
CHECKS.append('new_save_after_acknowledged_cancellation_resumes_both_SDK_graphs')

result = {'passed': True, 'profile': PROFILE, 'run': str(RUN), 'database': DATABASE,
          'bridge': str(BRIDGE), 'generation': generation, 'checks': CHECKS,
          'installed_artifacts': INSTALLED_EVIDENCE,
          'limits': ['finite owned current-host integration, not whole-language or release certification',
                     'unsupported graph forms remain explicitly partial', 'no encoding or installed-distribution performance claim']}
(RUN / 'summary.json').write_text(json.dumps(result, indent=2) + '\n')
if not INSTALLED:
    (RUN.parent / 'latest.json').write_text(json.dumps(result, indent=2) + '\n')
print(json.dumps(result, indent=2))
