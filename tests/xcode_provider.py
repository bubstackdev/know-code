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
import {xcodeSelection, findXcodeBridge} from './providers/swift/xcode.mjs';
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
'''
checks = subprocess.run(['node', '--input-type=module', '-e', source_checks, str(WORKSPACE), str(PROJECT / 'project.pbxproj'), str(SECOND / 'project.pbxproj'), str(scheme), str(RUN / 'state'), str(empty_tools), str(invalid_tools)], cwd=ROOT, env=ENV, capture_output=True, text=True, timeout=15)
assert checks.returncode == 0, (checks.stdout, checks.stderr)
(RUN / 'source-boundaries.json').write_text(json.dumps({'passed': True, 'twoProjectAmbiguity': True, 'explicitSelection': True, 'internalAliasDeduplicated': True, 'missingAndIncompatiblePrerequisites': True}))

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
