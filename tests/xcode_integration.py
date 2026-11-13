"""Owned Xcode compiler-to-native integration; requires the separate bridge.

The validation owner supplies KNOW_CODE_TEST_XCODE_BRIDGE (executable or its
runtime directory). This never installs a tool or touches the live coordinator.
"""
from fixture_trust import grant_fixture
from xcode_fixture import create
import atexit
import hashlib
import json
import os
from pathlib import Path
import plistlib
import subprocess
import time


ROOT = Path(__file__).resolve().parents[1]
RUN = ROOT / '.artifacts/xcode-integration' / str(time.time_ns())
PROJECT = create(RUN / 'workspace')
WORKSPACE = PROJECT.parent
PROFILE = os.environ.get('KNOW_CODE_BUILD_PROFILE', 'debug')
HOST = ROOT / f'target/{PROFILE}/know-code'
BRIDGE = Path(os.environ['KNOW_CODE_TEST_XCODE_BRIDGE']).expanduser().resolve(strict=True)
if BRIDGE.is_dir():
    BRIDGE = BRIDGE / 'sourcekit-xcode-bsp'
assert BRIDGE.is_file(), BRIDGE
BIN = RUN / 'bin'
BIN.mkdir()
(BIN / 'sourcekit-xcode-bsp').symlink_to(BRIDGE)
ENV = dict(os.environ, KNOW_CODE_DATA_DIR=str(RUN / 'data'),
           KNOW_CODE_QUERY_SCOPE=str(RUN / 'query-admission'),
           PATH=str(BIN) + os.pathsep + os.environ['PATH'],
           DOTNET_CLI_TELEMETRY_OPTOUT='1', DOTNET_NOLOGO='1')
ENV.pop('SDKROOT', None)

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


def run(argv, check=True):
    result = subprocess.run([str(value) for value in argv], cwd=ROOT, env=ENV,
                            text=True, capture_output=True, timeout=45)
    with (RUN / 'commands.jsonl').open('a') as stream:
        stream.write(json.dumps({'argv': list(map(str, argv)), 'returncode': result.returncode,
                                 'stdout': result.stdout, 'stderr': result.stderr}) + '\n')
    assert not check or result.returncode == 0, (argv, result.stdout, result.stderr)
    return result


def stop():
    run([HOST, 'coordinator', 'stop'], check=False)


atexit.register(stop)


def ready(label, after=0):
    deadline = time.monotonic() + 240
    with (RUN / (label + '-status.jsonl')).open('x') as stream:
        while time.monotonic() < deadline:
            status = json.loads(run([HOST, 'coordinator', 'status']).stdout)['workspaces'][0]
            stream.write(json.dumps(status) + '\n')
            stream.flush()
            assert status['phase'] not in ('failed', 'resource_limited'), status
            if (status['phase'] == 'idle' and status['generation'] > after
                    and status['observed_epoch'] == status['published_epoch']):
                return status['generation']
            time.sleep(.1)
    raise AssertionError(('Xcode indexing did not settle', status))


def query(request, equivalent=False):
    request = {'response_max_bytes': 2 * 1024 * 1024, **request}
    encoded = json.dumps(request)
    response = json.loads(run([HOST, 'query', DATABASE, encoded]).stdout)
    assert 'result' in response and response['result']['freshness'] == 'current', response
    if equivalent:
        for argv in (['node', ROOT / f'.artifacts/build/{PROFILE}/node/smoke.mjs'],
                     ['dotnet', ROOT / f'bindings/dotnet/Smoke/bin/{PROFILE.capitalize()}/net10.0/Smoke.dll']):
            assert json.loads(run([*argv, DATABASE, encoded]).stdout) == response
    return response['result']


def coverage():
    rows = query({'op': 'coverage', 'max_bytes': 262144}, equivalent=True)['coverage']
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
        occurrences = query({'op': 'references', 'symbol_id': leaf['id']}, equivalent=True)['occurrences']
        assert any(item['role'] == 'reference' for item in occurrences), occurrences
        for occurrence in occurrences:
            location = occurrence['location']
            source = query({'op': 'source', 'artifact_id': location['artifact_id'], 'max_bytes': 65536}, equivalent=True)['source']
            assert 'café 🦀' in source['text'], source
            token = source['text'].encode()[location['start_byte']:location['end_byte']].decode()
            assert token == expected.split('(')[0], (token, occurrence)
    for name in ('dependencyEntry()', 'testExample()'):
        assert len(symbols(name)) == 2, name


def guards():
    assert not any((WORKSPACE / name).exists() for name in ('PHASE_EXECUTED', 'SCHEME_EXECUTED'))


grant_fixture(HOST, WORKSPACE, ENV, RUN, RUN / 'fixture-trust.jsonl')
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
for origin in generated:
    source = query({'op': 'source', 'artifact_id': origin['artifactId'], 'max_bytes': 65536}, equivalent=True)['source']
    assert source['artifact_id'] == origin['artifactId'] and source['text'], source
generated_ids = {origin['artifactId'] for origin in generated}
asset_symbols = query({'op': 'symbols', 'query': 'fixtureTint', 'limit': 100}, equivalent=True).get('symbols', [])
assert any(location['artifact_id'] in generated_ids and location['generated']
           for symbol in asset_symbols for location in symbol.get('locations', [])), asset_symbols
CHECKS.append('compiler_generated_asset_source_retained_and_labeled')

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

result = {'passed': True, 'profile': PROFILE, 'run': str(RUN), 'database': DATABASE,
          'bridge': str(BRIDGE), 'generation': generation, 'checks': CHECKS,
          'limits': ['finite owned current-host integration, not whole-language or release certification',
                     'unsupported graph forms remain explicitly partial', 'no encoding or installed-distribution performance claim']}
(RUN / 'summary.json').write_text(json.dumps(result, indent=2) + '\n')
(RUN.parent / 'latest.json').write_text(json.dumps(result, indent=2) + '\n')
print(json.dumps(result, indent=2))
