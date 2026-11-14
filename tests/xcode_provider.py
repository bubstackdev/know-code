"""Xcode policy, workspace selection, prerequisite gaps and saved invalidation."""
from fixture_trust import grant_fixture
import atexit
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time

from xcode_fixture import create

ROOT = Path(__file__).resolve().parents[1]
RUN = ROOT / '.artifacts/xcode-provider' / str(time.time_ns())
PROJECT = create(RUN / 'fixture')
WORKSPACE = PROJECT.parent
ENV = dict(os.environ, KNOW_CODE_DATA_DIR=str(RUN / 'data'))
ENV['KNOW_CODE_QUERY_SCOPE']=str(RUN/'query-admission')
ENV.pop('SDKROOT', None)  # Apple's Python launcher can inject its CLT SDK.
PROFILE = os.environ.get('KNOW_CODE_BUILD_PROFILE', 'debug')
HOST = ROOT / f'target/{PROFILE}/know-code'
files = [WORKSPACE / 'App.swift', WORKSPACE / 'UITests.swift', PROJECT / 'project.pbxproj',
         PROJECT / 'xcshareddata/xcschemes/Probe.xcscheme']


def inventory():
    return {str(file): {'sha256': hashlib.sha256(file.read_bytes()).hexdigest(),
                       'mode': file.stat().st_mode, 'mtimeNs': file.stat().st_mtime_ns,
                       'ctimeNs': file.stat().st_ctime_ns} for file in files}


def invoke(name, policy=None):
    configs = ['Probe.xcodeproj/project.pbxproj',
               'Probe.xcodeproj/project.xcworkspace/contents.xcworkspacedata']
    request = {'workspaceRoot': str(WORKSPACE), 'projects': configs,
               'configuredProjects': configs, 'looseSources': ['App.swift', 'UITests.swift'],
               'configuration': 'Debug', 'preparationFingerprint': name, 'preparations': []}
    if policy is not None:
        request['analysisPolicy'] = policy
    process = subprocess.run(['node', ROOT / 'tests/worker_harness.mjs', ROOT / 'providers/swift/worker.mjs'],
                             cwd=ROOT, env=ENV, input=json.dumps(request), capture_output=True, text=True, timeout=120)
    assert process.returncode == 0, process.stderr
    result = json.loads(process.stdout)
    (RUN / (name + '.json')).write_text(json.dumps(result, indent=2))
    assert result['code'] == 0, result['stderr']
    messages = result['messages']
    assert messages[-1]['complete']['success']
    assert messages[0]['provider']['analysisPolicyVersion'] == 2
    compilations = [message['compilation'] for message in messages if 'compilation' in message]
    reports = [message['report'] for message in messages if 'report' in message]
    assert len(compilations) == len(reports) == 1, messages
    compilation = compilations[0]
    assert compilation['projectPath'] == 'Probe.xcodeproj/project.pbxproj'
    assert compilation['id'] == 'swift:Probe.xcodeproj/project.pbxproj'
    assert not compilation['inferred']
    options = json.loads(compilation['optionsJson'])
    assert not options['supported'] and options['requiredPlatforms'] == ['iphoneos', 'iphonesimulator']
    assert options['preparation'] == 'unavailable' and options['swiftExtractionPolicy'] == 1
    assert options['selectedSchemePath'] == 'Probe.xcodeproj/xcshareddata/xcschemes/Probe.xcscheme'
    assert str(PROJECT / 'xcshareddata/xcschemes/Probe.xcscheme') in compilation['inputPaths']
    assert reports[0]['compilationId'] == compilation['id'] and reports[0]['status'] == 'partial'
    assert reports[0]['capabilities']['calls'] == reports[0]['capabilities']['dependencies'] == 'unavailable'
    assert any(d['code'] == 'xcode_analysis_policy_required' for d in reports[0]['diagnostics'])
    assert not any('facts' in message or 'source' in message or 'preparation' in message for message in messages)
    assert not (WORKSPACE / 'PHASE_EXECUTED').exists()
    assert not (WORKSPACE / 'SCHEME_EXECUTED').exists()
    return compilation['id']


before = inventory()
initial_id = invoke('initial-gap')
assert inventory() == before
scheme = PROJECT / 'xcshareddata/xcschemes/Probe.xcscheme'

assert inventory() == before
# Unsupported policy/path inputs must fail before provider acknowledgment or
# compiler/planner work; opening a workspace must not silently ignore selection.
for label, policy in [
    ('wrong-version', {'version': 1, 'swift': {}}),
    ('wrong-branch', {'version': 2, 'node': {'cacheDirectory': str(RUN)}}),
    ('relative-scheme', {'version': 2, 'swift': {'schemePath': 'Probe.xcscheme', 'cacheDirectory': str(RUN / 'state'), 'executionAllowed': True}}),
    ('missing-scheme', {'version': 2, 'swift': {'schemePath': str(WORKSPACE / 'Missing.xcscheme'), 'cacheDirectory': str(RUN / 'state'), 'executionAllowed': True}}),
    ('no-trust', {'version': 2, 'swift': {'cacheDirectory': str(RUN / 'state'), 'executionAllowed': False}}),
    ('relative-cache', {'version': 2, 'swift': {'cacheDirectory': 'state', 'executionAllowed': True}}),
]:
    request = {'workspaceRoot': str(WORKSPACE), 'analysisPolicy': policy}
    process = subprocess.run(['node', ROOT / 'tests/worker_harness.mjs', ROOT / 'providers/swift/worker.mjs'],
                             cwd=ROOT, env=ENV, input=json.dumps(request), capture_output=True, text=True, timeout=15)
    assert process.returncode == 0, process.stderr
    result = json.loads(process.stdout)
    (RUN / ('policy-' + label + '.json')).write_text(json.dumps(result, indent=2))
    assert result['code'] != 0 and not result['messages'][-1]['complete']['success'], result
    assert not any(any(k in message for k in ('provider', 'compilation', 'progress', 'facts'))
                   for message in result['messages']), result
    assert not (WORKSPACE / 'PHASE_EXECUTED').exists() and not (WORKSPACE / 'SCHEME_EXECUTED').exists()
scheme.write_text(scheme.read_text().replace('parallelizeBuildables="NO"', 'parallelizeBuildables="YES"'))
changed = inventory()
assert invoke('saved-scheme-gap') == initial_id
assert inventory() == changed

# Two independent projects each have a sole shared scheme: workspace selection
# must still be ambiguous, including for an incremental first-project request.
SECOND = create(WORKSPACE / 'Second')
empty_tools = RUN / 'empty-tools'
empty_tools.mkdir()
invalid_tools = RUN / 'invalid-tools'
invalid_tools.mkdir()
invalid_executable = invalid_tools / 'sourcekit-xcode-bsp'
invalid_executable.write_text('unrecognized installed tool\n')
invalid_executable.chmod(0o755)
source_checks = '''
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import {createHash} from 'node:crypto';
import {xcodeSelection, findXcodeBridge, readMetadataRejection, contextProof, retainedAnalysisInputs} from './providers/swift/xcode.mjs';
import {swiftAnalysisPolicy} from './providers/swift/policy.mjs';
const [root, first, second, scheme, cache, empty, invalid] = process.argv.slice(1);
const configuredProjects = [first, second];
const both = xcodeSelection({root, configuredProjects});
assert.equal(both.error.code, 'xcode_scheme_selection_required');
assert.equal(both.inputs.filter(p => p.endsWith('.xcscheme')).length, 2);
const selected = xcodeSelection({root, configuredProjects, schemePath: scheme});
assert.equal(selected.project, first); assert.equal(selected.schemePath, scheme);
const alias = xcodeSelection({root, configuredProjects: [first, first.replace('project.pbxproj', 'project.xcworkspace/contents.xcworkspacedata')], schemePath: scheme});
assert.equal(alias.project, first);
const policy = swiftAnalysisPolicy({analysisPolicy: {version: 2, swift: {cacheDirectory: cache, executionAllowed: true, schemePath: scheme}}});
assert.equal(policy.cacheDirectory, cache); assert.equal(policy.schemePath, scheme);
assert.throws(() => findXcodeBridge({PATH: empty}, empty), error => error.code === 'xcode_bridge_missing');
assert.throws(() => findXcodeBridge({PATH: invalid}, empty), error => error.code === 'xcode_bridge_incompatible');

// Receipt data only: these owned bytes are never executed as a fake runtime.
// Shared JSON-RPC tests separately prove the exit-error causal classification.
const proofRoot = path.join(path.dirname(empty), 'metadata-contract');
const workspacePath = path.join(proofRoot, 'Bound.xcodeproj'), schemePath = path.join(workspacePath, 'xcshareddata/xcschemes/Bound.xcscheme');
fs.mkdirSync(path.dirname(schemePath), {recursive: true});
const projectInput = path.join(workspacePath, 'project.pbxproj'), compiler = path.join(proofRoot, 'compiler-bytes');
const executablePath = path.join(proofRoot, 'metadata-tool-bytes'), missing = path.join(proofRoot, 'MissingPackage.swift');
const attemptFile = path.join(proofRoot, 'know-code-project-export-request.json'), receiptFile = path.join(proofRoot, 'know-code-metadata-outcome.json');
const digest = file => createHash('sha256').update(fs.readFileSync(file)).digest('hex');
for (const [file, bytes] of [[projectInput, 'saved project'], [schemePath, 'saved scheme'], [compiler, 'compiler identity'], [executablePath, 'tool identity']]) fs.writeFileSync(file, bytes);
const binding = {metadataJobNonce: 'a'.repeat(64), metadataToolchainIdentity: 'b'.repeat(64), metadataCompilerPath: compiler,
  metadataInputSha256: {[projectInput]: digest(projectInput), [schemePath]: digest(schemePath)}, metadataMissingInputs: [missing]};
const expected = {...binding, workspacePath, schemePath, platform: 'iphoneos', executablePath, executableSha256: digest(executablePath),
  clones: path.join(proofRoot, 'clones'), cache: path.join(proofRoot, 'cache')};
const directArgs = ['-dumpPIF', path.join(proofRoot, 'know-code-project.pif.json'), '-project', workspacePath, '-scheme', 'Bound', '-configuration', 'Debug',
  '-derivedDataPath', path.join(proofRoot, 'metadata-derived'), '-clonedSourcePackagesDirPath', expected.clones, '-packageCachePath', expected.cache,
  '-disableAutomaticPackageResolution', '-onlyUsePackageVersionsFromResolvedFile', '-skipPackageUpdates'];
const naturalExit = {code: 65, signal: null, closed: true, receivedBytes: 0};
function freshProof() {
  fs.writeFileSync(projectInput, 'saved project'); fs.writeFileSync(executablePath, 'tool identity'); fs.rmSync(missing, {force: true});
  const attempt = {...structuredClone(binding), arguments: ['xcodebuild', ...directArgs], executablePath, executableSha256: expected.executableSha256, platform: expected.platform,
    metadataSchemePath: schemePath, metadataSchemeName: 'Bound', metadataSchemeSha256: binding.metadataInputSha256[schemePath], metadataConfiguration: 'Debug',
    metadataDerivedDataPath: path.join(proofRoot, 'metadata-derived'), clonedSourcePackagesDirPath: expected.clones, packageCachePath: expected.cache};
  const receipt = {...structuredClone(binding), version: 1, phase: 'metadata_command', outcome: 'rejected', workspacePath, schemePath, configuration: 'Debug',
    platform: expected.platform, executablePath, executableSha256: expected.executableSha256, arguments: [...directArgs], terminationReason: 'exit', exitStatus: 74,
    stdoutEOF: true, stderrEOF: true, bridgeCleanupSucceeded: true, expectedBridgeExit: 65, stdout: '', stderr: 'owned command rejection'};
  return {attempt, receipt};
}
function publish(pair) {
  fs.writeFileSync(attemptFile, JSON.stringify(pair.attempt)); pair.receipt.attemptSha256 = digest(attemptFile);
  fs.writeFileSync(receiptFile, JSON.stringify(pair.receipt));
}
const read = (termination = naturalExit) => readMetadataRejection({buildRoot: proofRoot, expected, termination});
const valid = freshProof(); publish(valid); assert.equal(read().code, 'xcode_metadata_unavailable');
let receiptFaults = 0;
function rejects(name, mutate, termination = naturalExit) {
  const pair = freshProof(); publish(pair); mutate(pair); assert.equal(read(termination), null, name); receiptFaults++;
}
rejects('nonce cannot be echoed from an old attempt', pair => { pair.attempt.metadataJobNonce = pair.receipt.metadataJobNonce = 'c'.repeat(64); publish(pair); });
rejects('attempt digest', pair => { pair.receipt.attemptSha256 = '0'.repeat(64); fs.writeFileSync(receiptFile, JSON.stringify(pair.receipt)); });
rejects('matching tampered argv is still outside this command', pair => { pair.attempt.arguments.push('-quiet'); pair.receipt.arguments.push('-quiet'); publish(pair); });
rejects('matching wrong input hash', pair => { pair.attempt.metadataInputSha256[projectInput] = pair.receipt.metadataInputSha256[projectInput] = '0'.repeat(64); publish(pair); });
rejects('saved input changed after command', () => fs.writeFileSync(projectInput, 'later saved bytes'));
rejects('missing input appeared', () => fs.writeFileSync(missing, 'later file'));
rejects('selected executable replaced', () => fs.writeFileSync(executablePath, 'different tool bytes'));
for (const [name, patch] of [['exit disagreement', {code: 1}], ['signal', {signal: 'SIGKILL'}], ['unconfirmed close', {closed: false}], ['protocol stdout', {receivedBytes: 1}]])
  rejects(name, () => {}, {...naturalExit, ...patch});
for (const [name, patch] of [['cleanup incomplete', {bridgeCleanupSucceeded: false}], ['stdout EOF missing', {stdoutEOF: false}], ['stderr EOF missing', {stderrEOF: false}],
  ['command success is not rejection', {exitStatus: 0}], ['status is not an integer', {exitStatus: '74'}], ['signal outcome', {terminationReason: 'signal'}], ['unknown proof version', {version: 2}], ['wrong bridge terminal', {expectedBridgeExit: 1}]])
  rejects(name, pair => { Object.assign(pair.receipt, patch); fs.writeFileSync(receiptFile, JSON.stringify(pair.receipt)); });
rejects('malformed receipt', () => fs.writeFileSync(receiptFile, '{'));
rejects('oversized receipt', () => fs.writeFileSync(receiptFile, ' '.repeat(2 * 1024 * 1024 + 1)));
rejects('missing receipt', () => fs.rmSync(receiptFile));
rejects('missing attempt', () => fs.rmSync(attemptFile));
const finalValid = freshProof(); publish(finalValid); assert.equal(read().code, 'xcode_metadata_unavailable');
// Bridge sidecars canonicalize filesystem paths. A short personal-data alias
// must resolve to the same job root; a PIF symlink escaping that root must fail.
const contextRoot = path.join(path.dirname(empty), 'context-proof'), realRoot = path.join(contextRoot, 'real');
const aliasRoot = path.join(contextRoot, 'alias');
fs.mkdirSync(realRoot, {recursive: true}); fs.symlinkSync(realRoot, aliasRoot, 'dir');
const missingAuthored = path.join(contextRoot, 'linked', 'NotCreated.swift');
const cachedInputs = {analysisInputs: [path.join(realRoot, 'Generated.swift'), path.join(aliasRoot, 'Generated.swift'), schemePath, missingAuthored, 'relative.swift', null]};
assert.deepEqual(retainedAnalysisInputs(cachedInputs, realRoot, aliasRoot), [schemePath, missingAuthored]);
assert.equal(fs.existsSync(missingAuthored), false);
const pifPath = path.join(realRoot, 'know-code-project.pif.json');
fs.writeFileSync(pifPath, JSON.stringify([{type: 'project', contents: {path: workspacePath}}]));
const parameters = {action: 'indexbuild', configurationName: 'Debug', activeRunDestination: {platform: 'iphoneos', sdk: 'iphoneos', targetArchitecture: 'arm64'},
  overrides: {commandLine: {table: {INDEX_DISABLE_SCRIPT_EXECUTION: 'YES', INDEX_FORCE_SCRIPT_EXECUTION: 'NO'}}}};
const selectedContext = {pifPath, pifSha256: digest(pifPath), selection: {schemePath, configuration: 'Debug', inputSha256: {[schemePath]: digest(schemePath)}, targets: [{plannerIdentifier: 'owned-root'}]},
  metadataContext: {schemePath, configuration: 'Debug'}};
const closureContext = {pifSha256: selectedContext.pifSha256, loadedPIFPath: pifPath, configuration: 'Debug', selectedRootGUIDs: ['owned-root']};
const putContext = () => {
  fs.writeFileSync(path.join(realRoot, 'know-code-selected-scheme.json'), JSON.stringify(selectedContext));
  fs.writeFileSync(path.join(realRoot, 'know-code-selected-closure.json'), JSON.stringify(closureContext));
};
putContext();
fs.writeFileSync(path.join(realRoot, 'know-code-index-only-request.json'), JSON.stringify({buildCommand: {command: 'prepareForIndexing'}, parameters,
  configuredTargets: [{guid: 'owned-root', parameters}]}));
assert.deepEqual(contextProof(realRoot, 'iphoneos', {schemePath}).projects, [workspacePath]);
assert.deepEqual(contextProof(aliasRoot, 'iphoneos', {schemePath}).projects, [workspacePath]);
const outsidePif = path.join(contextRoot, 'outside.pif.json'), escapedPif = path.join(realRoot, 'escaped.pif.json');
fs.copyFileSync(pifPath, outsidePif); fs.symlinkSync(outsidePif, escapedPif);
selectedContext.pifPath = closureContext.loadedPIFPath = escapedPif; putContext();
assert.throws(() => contextProof(aliasRoot, 'iphoneos', {schemePath}), error => error.code === 'xcode_context_proof_invalid');
console.log(JSON.stringify({validMetadataRejection: true, rejectedMetadataFaults: receiptFaults, canonicalJobRoot: true, pifSymlinkEscapeRejected: true,
  cachedCentralOutputsRemovedAndMissingLinkedInputsPreserved: true}));
'''
checks = subprocess.run(['node', '--input-type=module', '-e', source_checks, str(WORKSPACE), str(PROJECT / 'project.pbxproj'), str(SECOND / 'project.pbxproj'), str(scheme), str(RUN / 'state'), str(empty_tools), str(invalid_tools)], cwd=ROOT, env=ENV, capture_output=True, text=True, timeout=15)
assert checks.returncode == 0, (checks.stdout, checks.stderr)
(RUN / 'source-boundaries.json').write_text(json.dumps({'passed': True, 'twoProjectAmbiguity': True, 'explicitSelection': True, 'internalAliasDeduplicated': True, 'missingAndIncompatiblePrerequisites': True, 'metadataRejectionContract': json.loads(checks.stdout)}))

# Exercise the shared coordinator/watch path, including creation of a new
# settings input. A worker-only replay cannot detect a missing watcher suffix.
def command(*args, check=True):
    result = subprocess.run([str(HOST), *map(str, args)], cwd=ROOT, env=ENV,
                            capture_output=True, text=True, timeout=30)
    assert not check or result.returncode == 0, (result.stdout, result.stderr)
    return json.loads(result.stdout) if result.returncode == 0 and result.stdout.strip() else None


atexit.register(lambda: command('coordinator', 'stop', check=False))
grant_fixture(HOST,WORKSPACE,ENV,RUN,RUN/'fixture-trust.jsonl')
registration = command('watch', WORKSPACE)


def ready(after=0):
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        status = command('coordinator', 'status')['workspaces'][0]
        assert status['phase'] != 'failed', status
        if status['phase'] == 'idle' and status['generation'] > after:
            result = command('query', registration['database'], json.dumps({'op': 'symbols','coverage_detail':'full'}))['result']
            assert result['freshness'] == 'current', result
            assert len(result['coverage']) == 2 and all(row['status'] == 'partial' for row in result['coverage']), result
            assert all(any(d['code'] == 'xcode_scheme_selection_required' for d in row['diagnostics']) for row in result['coverage']), result
            assert not result.get('symbols'), result
            assert not (WORKSPACE / 'PHASE_EXECUTED').exists()
            assert not (WORKSPACE / 'SCHEME_EXECUTED').exists()
            return status['generation']
        time.sleep(.05)
    raise AssertionError(status)


generation = ready()
scheme.write_text(scheme.read_text() + '\n<!-- saved scheme input -->\n')
generation = ready(generation)
config = WORKSPACE / 'NewSettings.xcconfig'
config.write_text('SWIFT_ACTIVE_COMPILATION_CONDITIONS = KNOW_CODE_PROBE\n')
generation = ready(generation)
config.write_text('SWIFT_ACTIVE_COMPILATION_CONDITIONS = KNOW_CODE_SAVED\n')
generation = ready(generation)
config.unlink()
generation = ready(generation)
summary = {'run': str(RUN), 'explicitPartialGap': True, 'workspaceSchemeAmbiguity': True, 'swiftPolicyVersion': 2, 'internalWorkspaceDeduplicated': True,
           'configuredSourcesNotInferred': True, 'stableCompilationIdentity': True,
           'negativePhaseMarker': True, 'negativeSchemeMarker': True,
           'noProjectBuildOrPreparation': True, 'inputBytesAndMetadataPreserved': True,
           'savedSchemeObserved': True, 'newChangedRemovedXcconfigObserved': True,
           'coordinatorGeneration': generation, 'profile': PROFILE}
(RUN / 'summary.json').write_text(json.dumps(summary, indent=2))
(RUN.parent / 'latest.json').write_text(json.dumps(summary, indent=2))
print(json.dumps(summary))
