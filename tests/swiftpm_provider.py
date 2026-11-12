"""Real SwiftPM planner -> module preparation -> compiler IndexStore contract."""
import base64
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
RUN = ROOT / '.artifacts/swiftpm-provider' / str(time.time_ns())
PACKAGE = RUN / 'package'
DEPENDENCY = RUN / 'dependency'
PROFILE = os.environ.get('KNOW_CODE_BUILD_PROFILE', 'debug')
ENV = dict(os.environ, KNOW_CODE_DATA_DIR=str(RUN / 'data'), KNOW_CODE_BUILD_PROFILE=PROFILE)
ENV['KNOW_CODE_QUERY_SCOPE']=str(RUN/'query-admission')
# Apple's Python launcher can inject its CLT SDK. This suite exercises the
# unpinned default; explicit SDK selection is covered by swift_toolchain.mjs.
ENV.pop('SDKROOT', None)


def write(path, text):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)


write(DEPENDENCY / 'Package.swift', '''// swift-tools-version: 6.0
import PackageDescription
let package = Package(name: "Dependency", products: [.library(name: "Dependency", targets: ["Dependency"])], targets: [.target(name: "Dependency")])
''')
write(DEPENDENCY / 'Sources/Dependency/External.swift', 'public struct ExternalBox { public init() {} }\n')
MANIFEST = '''// swift-tools-version: 6.0
import PackageDescription
let package = Package(name: "KnowCodeFixture", products: [.library(name: "Second", targets: ["Second"])], dependencies: [.package(path: "../dependency")], targets: [
.systemLibrary(name: "CLocal"),
.target(name: "First", dependencies: ["CLocal", .product(name: "Dependency", package: "dependency")]),
.target(name: "Second", dependencies: ["First"]),
.testTarget(name: "SecondTests", dependencies: ["Second"])])
'''
write(PACKAGE / 'Package.swift', MANIFEST)
write(PACKAGE / 'Package@swift-6.0.swift', MANIFEST)
write(PACKAGE / 'Sources/CLocal/module.modulemap', 'module CLocal { header "shim.h" export * }\n')
write(PACKAGE / 'Sources/CLocal/shim.h', 'typedef struct { int value; } LocalBox;\n')
write(PACKAGE / 'Sources/First/First.swift', '''import Dependency
import CLocal
public struct First {
 public init() {}
 public func greet(_ name: String) -> String { let café = name; _ = ExternalBox(); _ = LocalBox(value: 1); return café }
}
''')
SECOND = 'import First\npublic func call() -> String { First().greet("hello") }\n'
write(PACKAGE / 'Sources/Second/Second.swift', SECOND)
write(PACKAGE / 'Tests/SecondTests/Test.swift', '@testable import Second\nfunc fixtureTestInput() -> String { call() }\n')
subprocess.run([sys.executable, ROOT / 'scripts/build_record.py', 'verify', PROFILE], cwd=ROOT, check=True, stdout=subprocess.DEVNULL)
READER = ROOT / f'.artifacts/build/{PROFILE}/swift/indexstore-reader'
reader_sha256 = hashlib.sha256(READER.read_bytes()).hexdigest()


def invoke(name, package=PACKAGE, preparations=None, fingerprint='fixture1'):
    configurations = [str(file) for file in package.glob('Package*.swift')]
    request = {'workspaceRoot': str(package), 'projects': configurations, 'configuredProjects': configurations,
               'looseSources': [], 'configuration': 'Debug', 'preparationFingerprint': fingerprint,
               'preparations': preparations or []}
    (RUN / f'{name}-request.json').write_text(json.dumps(request, indent=2))
    process = subprocess.run(['node', ROOT / 'tests/worker_harness.mjs', ROOT / 'providers/swift/worker.mjs'],
                             cwd=ROOT, env=ENV, input=json.dumps(request), capture_output=True, text=True, timeout=240)
    assert process.returncode == 0, process.stderr
    result = json.loads(process.stdout)
    (RUN / f'{name}.json').write_text(json.dumps(result, indent=2))
    assert result['code'] == 0, result['stderr']
    assert result['messages'][-1]['complete']['success'], result
    return result['messages']


def facts(messages):
    symbols = {symbol['id']: symbol for message in messages if 'facts' in message for symbol in message['facts']['symbols']}
    occurrences = [occurrence for message in messages if 'facts' in message for occurrence in message['facts']['occurrences']]
    artifacts = {message['source']['id']: message['source'] for message in messages if 'source' in message}
    return symbols, occurrences, artifacts


initial = invoke('initial')
compilations = [message['compilation'] for message in initial if 'compilation' in message]
reports = [message['report'] for message in initial if 'report' in message]
assert len(compilations) == 3 and all(c['projectPath'] == 'Package.swift' for c in compilations), compilations
assert {c['target'] for c in compilations} == {'First', 'Second', 'SecondTests-product'}
assert all(report['status'] == 'complete' for report in reports), reports
assert all(not c['inferred'] and json.loads(c['optionsJson'])['origin'] == 'swiftpm_build_server' for c in compilations)
assert all(json.loads(c['optionsJson'])['jobs'] == 1 for c in compilations)
assert any('Package@swift-6.0.swift' in input_path for c in compilations for input_path in c['inputPaths'])
preparations = [message['preparation'] for message in initial if 'preparation' in message]
assert len(preparations) == 1 and preparations[0]['succeeded']
symbols, occurrences, artifacts = facts(initial)
assert any(s['name'] == 'greet(_:)' and not s['external'] for s in symbols.values())
assert any(s['name'] == 'ExternalBox' and s['external'] for s in symbols.values())
assert any(s['name'] == 'LocalBox' and s['external'] for s in symbols.values())
assert not any('../dependency/' in source['path'] for source in artifacts.values()), 'dependency sources must not be indexed'
assert any(source['generated'] and 'test_entry_point.swift' in source['path'] for source in artifacts.values())
assert any(occurrence['role'] == 'reference' and symbols[occurrence['symbolId']]['name'] == 'greet(_:)' for occurrence in occurrences)
for occurrence in occurrences:
    source = artifacts[occurrence['artifactId']]
    token = base64.b64decode(source['utf8'])[occurrence['startByte']:occurrence['endByte']].decode('utf8')
    assert token and '\n' not in token
    if symbols[occurrence['symbolId']]['name'] == 'café':
        assert token == 'café'
assert any(path.endswith('/Sources/CLocal/shim.h') for c in compilations for path in c['inputPaths'])
assert any(path.endswith('/Sources/CLocal/module.modulemap') for c in compilations for path in c['inputPaths'])
assert not list((RUN / 'data').rglob('*.o')), 'index preparation must not generate object files'
assert list((RUN / 'data').rglob('*.swiftmodule')), 'SwiftPM should prepare dependency modules'
# Saved-source changes reuse valid dependency setup checkpoints; normal compiler
# diagnostics publish a fresh partial graph rather than inventing unknown symbols.
write(PACKAGE / 'Sources/Second/Second.swift', SECOND + 'public func broken() -> Int { unknownValue }\n')
partial = invoke('partial', preparations=preparations)
assert not any('preparation' in message for message in partial), 'source save retried dependency resolution'
partial_reports = [message['report'] for message in partial if 'report' in message]
assert any(report['status'] == 'partial' and any('unknownValue' in d['message'] for d in report['diagnostics']) for report in partial_reports), partial_reports
assert not any(s['name'] == 'unknownValue' for s in facts(partial)[0].values())
write(PACKAGE / 'Sources/Second/Second.swift', SECOND)
repaired = invoke('repaired', preparations=preparations)
assert all(message['report']['status'] == 'complete' for message in repaired if 'report' in message)
assert not any('preparation' in message for message in repaired)
# Compiler-reported missing modules must not leak stale prepared module bindings.
assert any(s['name'] == 'call()' for s in facts(repaired)[0].values())
# The preferred Xcode context must keep real toolchain macros available on a
# saved update too. These functions are indexed, never executed as tests.
selection = json.loads(compilations[0]['optionsJson'])['toolchainSelection']
assert selection['origin'] in ('preferred_xcode', 'explicit_override', 'repository_pin', 'command_line_tools')
if selection['xcodeVersion']:
    testing = '@testable import Second\nimport Testing\n@Test func macroFixture() { #expect(call() == "hello") }\n'
    write(PACKAGE / 'Tests/SecondTests/Test.swift', testing)
    macros = invoke('testing-macros', preparations=preparations)
    assert all(m['report']['status'] == 'complete' for m in macros if 'report' in m)
    write(PACKAGE / 'Tests/SecondTests/Test.swift', testing.replace('macroFixture', 'savedMacroFixture'))
    saved_macros = invoke('testing-macros-save', preparations=preparations)
    assert all(m['report']['status'] == 'complete' for m in saved_macros if 'report' in m)
    assert not any('preparation' in m for m in saved_macros)
    write(PACKAGE / 'Tests/SecondTests/Test.swift', '@testable import Second\nfunc fixtureTestInput() -> String { call() }\n')
# Changed exported API + a source error cannot leave dependent targets bound
# to a previously prepared module's old public API.
first_file = PACKAGE / 'Sources/First/First.swift'
first_original = first_file.read_text()
first_file.write_text(first_original.replace('func greet(', 'func salute(') + 'public func brokenExport() -> Int { missingExportValue }\n')
changed_api = invoke('changed-api-partial', preparations=preparations)
changed_compilations = {m['compilation']['id']: m['compilation'] for m in changed_api if 'compilation' in m}
changed_reports = {m['report']['compilationId']: m['report'] for m in changed_api if 'report' in m}
consumer = next(c for c in changed_compilations.values() if c['target'] == 'Second')
assert changed_reports[consumer['id']]['status'] == 'partial', changed_reports
assert not any(s['name'] == 'greet(_:)' and s['id'].startswith(consumer['id'] + ':') for s in facts(changed_api)[0].values()), 'dependent resolved an old cached public API'
assert not any('preparation' in message for message in changed_api), 'source API change retried dependency resolution'
first_file.write_text(first_original)
api_repaired = invoke('changed-api-repaired', preparations=preparations)
assert all(m['report']['status'] == 'complete' for m in api_repaired if 'report' in m)
# A broken explicit manifest never produces a standalone fallback.
write(PACKAGE / 'Package@swift-6.0.swift', 'this is not a valid Swift package manifest\n')
broken = invoke('broken-manifest', preparations=preparations, fingerprint='broken')
assert any(d['code'] == 'swiftpm_manifest_failed' for message in broken if 'report' in message for d in message['report']['diagnostics'])
assert not any('facts' in message or 'source' in message for message in broken)
write(PACKAGE / 'Package@swift-6.0.swift', MANIFEST)
# Preflight prevents executing external code generation even while constructing
# the BSP build graph. The plugin writes a negative marker if it ever runs.
PLUGIN = RUN / 'plugin-package'
write(PLUGIN / 'Package.swift', '''// swift-tools-version: 6.0
import PackageDescription
let package = Package(name: "PluginFixture", targets: [.target(name: "Main", plugins: [.plugin(name: "Generator")]), .plugin(name: "Generator", capability: .buildTool())])
''')
write(PLUGIN / 'Sources/Main/Main.swift', 'public func value() -> Int { 1 }\n')
write(PLUGIN / 'Plugins/Generator/Plugin.swift', '''import Foundation
import PackagePlugin
@main struct Generator: BuildToolPlugin {
 func createBuildCommands(context: PluginContext, target: Target) throws -> [Command] {
  try "EXECUTED".write(toFile: context.package.directory.appending("PLUGIN_EXECUTED").string, atomically: true, encoding: .utf8)
  return []
 }
}
''')
plugin = invoke('plugin-negative', package=PLUGIN)
assert any(d['code'] == 'swiftpm_external_codegen_required' for message in plugin if 'report' in message for d in message['report']['diagnostics'])
assert not (PLUGIN / 'PLUGIN_EXECUTED').exists()
assert not any('facts' in message or 'source' in message for message in plugin)
# Protocol method aliases are negotiated only for the precise method-not-found
# error, retaining actual preparation failures rather than masking them.
protocol_test = '''
import {extensionRequest} from './providers/swift/swiftpm.mjs';
let methods=[];
const legacy={request:async(method)=>{methods.push(method);if(method.startsWith('sourcekit/'))throw Object.assign(new Error('missing'),{rpcCode:-32601});return 42;}};
if(await extensionRequest(legacy,'buildTarget/prepare',{})!==42 || methods.join(',')!=='sourcekit/buildTarget/prepare,buildTarget/prepare')throw new Error('legacy negotiation');
methods=[];
const failed={request:async(method)=>{methods.push(method);throw Object.assign(new Error('actual failure'),{rpcCode:-32001});}};
try{await extensionRequest(failed,'buildTarget/prepare',{});throw new Error('masked failure');}catch(error){if(error.rpcCode!==-32001 || methods.length!==1)throw error;}
methods=[];
const modern={request:async(method)=>{methods.push(method);return 7;}};
if(await extensionRequest(modern,'buildTarget/prepare',{})!==7 || methods.length!==1)throw new Error('modern negotiation');
'''
subprocess.run(['node', '--input-type=module', '-e', protocol_test], cwd=ROOT, env=ENV, check=True, timeout=15)
assert not any((RUN / 'data/workers/swift').glob('job-*')), 'per-job indexes/spools must be reclaimed'
assert hashlib.sha256(READER.read_bytes()).hexdigest() == reader_sha256, 'test changed the built reader'
summary = {'passed': True, 'run': str(RUN), 'profile': PROFILE, 'reader_sha256': reader_sha256,
           'targets': [c['target'] for c in compilations], 'initial_symbols': len(symbols),
           'initial_occurrences': len(occurrences), 'source_artifacts': len(artifacts),
           'module_only_preparation': True, 'dependency_sources_indexed': False,
           'source_save_dependency_checkpoint_reused': True, 'external_plugin_executed': False}
summary['toolchain_selection'] = selection
summary['xcode_testing_macros_saved_update'] = bool(selection['xcodeVersion'])
(RUN / 'summary.json').write_text(json.dumps(summary, indent=2))
(ROOT / '.artifacts/swiftpm-provider/latest.json').write_text(json.dumps(summary, indent=2))
print(json.dumps(summary))
