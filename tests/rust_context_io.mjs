// Bounded I/O guards only; no compiler/analyzer/toolchain execution.
import fs from 'node:fs';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import {spawnSync} from 'node:child_process';
import assert from 'node:assert/strict';
import {writeDerivedFile} from '../providers/rust/preparation.mjs';
import {removeCapturedOutput} from '../providers/rust/cargo-contexts.mjs';
import {externArguments, environmentBaseline, capturedEnvironment, capturedEnvironmentCurrent, selectedProject, toolingRoots, preparationEmits} from '../providers/rust/contexts.mjs';
import {syntaxGraph} from '../providers/rust/graph.mjs';

const macroCommand = {cwd: process.cwd(), args: ['--crate-type', 'proc-macro', '-L', 'dependency=/owned/deps', '--extern', 'proc_macro']};
assert.deepEqual(externArguments(macroCommand), {dependencies: [], sysroot: ['proc_macro']});
assert.deepEqual(externArguments({...macroCommand, args: ['--test', '-L', 'dependency=/owned/deps', '--extern', 'proc_macro']}),
  {dependencies: [], sysroot: ['proc_macro']});
for (const args of [
  ['--crate-type', 'lib', '--extern', 'proc_macro'],
  ['--crate-type', 'proc-macro', '--extern', 'unknown'],
  ...['crate=/foreign', 'all=/foreign', '/foreign'].map(search => [...macroCommand.args, '-L', search]),
  [...macroCommand.args, '--extern', 'proc_macro=/foreign/library'],
]) assert.throws(() => externArguments({...macroCommand, args}), /pathless extern/);
assert.deepEqual(externArguments({...macroCommand, args: ['--extern', 'renamed=/owned/dependency']}),
  {dependencies: [{name: 'renamed', filename: '/owned/dependency'}], sysroot: []});

// Package names deliberately collide: only resolve package IDs select roots.
const target = (filename, kind = 'lib', types = [kind]) => ({src_path: filename, kind: [kind], crate_types: types});
const packageItem = (id, targets) => ({id, name: 'same-name', targets});
const edge = (pkg, kind = null) => ({pkg, dep_kinds: [{kind, target: null}]});
const toolingMetadata = {packages: [
  packageItem('build-owner', [target('/owned/build.rs', 'custom-build', ['bin']), target('/owned/app.rs')]),
  packageItem('macro-owner', [target('/owned/macro.rs', 'proc-macro')]),
  packageItem('build-dep', [target('/owned/helper.rs'), target('/owned/helper-test.rs', 'test', ['bin'])]),
  packageItem('macro-dep', [target('/owned/macro-helper.rs')]),
  packageItem('transitive', [target('/owned/transitive.rs', 'rlib')]),
  packageItem('normal-app', [target('/owned/not-tooling.rs')]),
  packageItem('dev-only', [target('/owned/dev-only.rs')]),
  packageItem('mixed', [target('/owned/mixed.rs', 'lib', ['rlib', 'cdylib'])]),
], resolve: {nodes: [
  {id: 'build-owner', deps: [edge('build-dep', 'build'), edge('normal-app'), edge('dev-only', 'dev')]},
  {id: 'macro-owner', deps: [edge('macro-dep'), edge('dev-only', 'dev')]},
  {id: 'build-dep', deps: [edge('transitive'), edge('mixed'), edge('dev-only', 'dev')]},
  {id: 'macro-dep', deps: [edge('transitive')]},
  {id: 'transitive', deps: [edge('build-dep')]}, // cycle is visited once
  ...['normal-app', 'dev-only', 'mixed'].map(id => ({id, deps: []})),
]}};
const roots = toolingRoots(toolingMetadata);
assert.deepEqual(roots.buildScripts, ['/owned/build.rs']);
assert.deepEqual(roots.procMacros, ['/owned/macro.rs']);
assert.deepEqual([...roots.libraries].sort(), ['/owned/helper.rs', '/owned/macro-helper.rs', '/owned/transitive.rs']);
assert.throws(() => toolingRoots({...toolingMetadata, resolve: {nodes: []}}), /missing tooling/);
assert.throws(() => toolingRoots({...toolingMetadata, packages: [...toolingMetadata.packages, toolingMetadata.packages[0]]}), /ambiguous tooling/);
assert.throws(() => toolingRoots({...toolingMetadata, packages: Array(16001).fill(null)}), /bounds/);
const compilerCommand = (root, type, extra = [], emit = 'dep-info,metadata,link') => ({compiler: '/owned/rustc', cwd: '/owned',
  args: ['--crate-name', 'owned', '--edition=2024', root, '--crate-type', type, '--emit=' + emit, ...extra]});
for (const [root, type] of [['/owned/build.rs', 'bin'], ['/owned/macro.rs', 'proc-macro'],
  ['/owned/helper.rs', 'lib'], ['/owned/transitive.rs', 'rlib']])
  assert.equal(preparationEmits(compilerCommand(root, type), roots).root, root);
for (const [root, type, extra] of [
  ['/owned/not-tooling.rs', 'lib', []], ['/owned/dev-only.rs', 'lib', []],
  ['/owned/helper-test.rs', 'bin', []], ['/owned/helper.rs', 'bin', []],
  ['/owned/helper.rs', 'lib', ['--test']], ['/owned/helper.rs', 'lib', ['--target', 'aarch64-apple-darwin']],
  ['/owned/helper.rs', 'cdylib', []], ['/owned/helper.rs', 'staticlib', []], ['/owned/helper.rs', 'lib,cdylib', []],
  ['/owned/build.rs', 'proc-macro', []], ['/owned/macro.rs', 'bin', []],
  ['/owned/build.rs', 'bin', ['--test']], ['/owned/macro.rs', 'proc-macro', ['--test']],
]) assert.throws(() => preparationEmits(compilerCommand(root, type, extra), roots), /final-product\/codegen/);
assert.throws(() => preparationEmits(compilerCommand('/owned/helper.rs', 'lib', [], 'llvm-ir'), roots), /final-product\/codegen/);
assert.throws(() => preparationEmits(compilerCommand('/owned/helper.rs', 'lib'), {...roots, libraries: ['/relative'.slice(1)]}), /tooling root/);
assert.equal(preparationEmits(compilerCommand('/owned/not-tooling.rs', 'bin', ['--test'], 'dep-info,metadata'), roots).test, true);

const proofSource = {uri: 'file:///owned/saved.rs', text: 'fn saved() {}'};
const presentationError = Object.assign(new Error('owned synthetic syntax presentation failure'), {rpcCode: -32603});
const presentationAnalyzer = {request: async method => {
  if (method === 'rust-analyzer/viewFileText') return proofSource.text;
  throw presentationError;
}};
let omittedProofs = 0;
assert.equal(await syntaxGraph(presentationAnalyzer, proofSource, {onPresentationUnavailable: () => omittedProofs++}), undefined);
assert.equal(omittedProofs, 1);
await assert.rejects(syntaxGraph(presentationAnalyzer, proofSource), error => error === presentationError);
await assert.rejects(syntaxGraph({request: async () => { throw presentationError; }}, proofSource,
  {onPresentationUnavailable: () => assert.fail('source-read errors are not presentation gaps')}), error => error === presentationError);
await assert.rejects(syntaxGraph({request: async method => {
  if (method === 'rust-analyzer/viewFileText') return proofSource.text;
  throw new Error('owned synthetic transport failure');
}}, proofSource, {onPresentationUnavailable: () => assert.fail('transport errors must remain fatal')}), /transport failure/);

const inheritedEnvironment = {SAVED: 'owned ambient must not be dumped', CHANGED: 'before', REMOVED: 'before', MAKEFLAGS: 'private descriptor'};
const baselineEnvironment = environmentBaseline(inheritedEnvironment);
const environmentRecord = capturedEnvironment({SAVED: inheritedEnvironment.SAVED, CHANGED: 'after', ADDED: 'Cargo injection', MAKEFLAGS: 'different descriptor'}, baselineEnvironment);
assert(!JSON.stringify(baselineEnvironment).includes(inheritedEnvironment.SAVED));
assert.deepEqual(environmentRecord.environment, {CHANGED: 'after', ADDED: 'Cargo injection'});
assert.deepEqual(environmentRecord.removedEnvironment, ['REMOVED']);
assert(!Object.hasOwn(environmentRecord.environmentOrigins, 'MAKEFLAGS'));
assert(capturedEnvironmentCurrent(environmentRecord, baselineEnvironment));
assert(capturedEnvironmentCurrent(environmentRecord, Object.fromEntries(Object.entries(baselineEnvironment).reverse())));
assert(!capturedEnvironmentCurrent(environmentRecord, environmentBaseline({...inheritedEnvironment, SAVED: 'changed untracked ambient value'})));
assert(!capturedEnvironmentCurrent(environmentRecord, environmentBaseline({...inheritedEnvironment, CHANGED: 'different parent'})));
assert(!capturedEnvironmentCurrent({...environmentRecord, environment: {...environmentRecord.environment, CHANGED: 'tampered'}}, baselineEnvironment));
assert.throws(() => capturedEnvironment({}, {bad: 'raw inherited value'}), /hashed compiler environment/);
const removedMacroUnit = {cfg: {name: 'owned_macro', root: '/owned/macro.rs', edition: '2024', effective: [], removals: [],
  sysroot: '/owned/sysroot', compilerVersion: 'owned', compilerSha256: 'owned'}, inputs: {files: []}, dependencies: [],
  cargo: {package_id: 'owned', target: {kind: ['proc-macro']}, filenames: ['/owned/macro.dylib']},
  record: {cwd: '/owned', artifacts: [{emit: 'link', artifact: '/owned/macro.dylib'}], environment: {}, removedEnvironment: ['REMOVED']},
  outputHashes: {'/owned/macro.dylib': 'owned'}};
assert.throws(() => selectedProject([removedMacroUnit], 0, {workspace_members: []}), /cannot represent deletion/);


const repository = fileURLToPath(new URL('..', import.meta.url));
const directory = path.join(repository, '.artifacts/rust-context-io', `${Date.now()}-${process.pid}`);
fs.mkdirSync(directory, {recursive: true});
const fifo = path.join(directory, 'input.fifo');
assert.equal(spawnSync('mkfifo', [fifo]).status, 0);
for (const source of [
  `import {boundedRead} from './providers/rust/preparation.mjs'; boundedRead(${JSON.stringify(fifo)}, 64);`,
  `import {fileEvidence} from './providers/rust/contexts.mjs'; fileEvidence([${JSON.stringify(fifo)}]);`]) {
  const result = spawnSync(process.execPath, ['--input-type=module', '-e', source],
    {cwd: repository, encoding: 'utf8', timeout: 2000});
  assert(!result.error, `regular-file guard blocked: ${result.error?.message}`);
  assert.notEqual(result.status, 0);
}
writeDerivedFile(fifo, 'recovered derived data');
assert(fs.statSync(fifo).isFile());
assert.equal(fs.readFileSync(fifo, 'utf8'), 'recovered derived data');
assert(!fs.readdirSync(directory).some(name => name.endsWith('.next')));

const originalWrite = fs.writeFileSync, failedTarget = path.join(directory, 'failed-derived.json');
let ownedBeforeFailure = 0;
try {
  fs.writeFileSync = (descriptor, bytes) => {
    assert.equal(typeof descriptor, 'number');
    originalWrite(descriptor, String(bytes).slice(0, 3));
    ownedBeforeFailure = fs.readdirSync(directory).filter(name => name.endsWith('.next')).length;
    throw Object.assign(new Error('simulated partial-write ENOSPC'), {code: 'ENOSPC'});
  };
  assert.throws(() => writeDerivedFile(failedTarget, 'partial data'), /simulated partial-write ENOSPC/);
} finally { fs.writeFileSync = originalWrite; }
assert.equal(ownedBeforeFailure, 1);
assert(!fs.existsSync(failedTarget));
assert(!fs.readdirSync(directory).some(name => name.endsWith('.next')), 'partial-write scratch file was left behind');

const configuration = path.join(directory, 'configuration.json'), marker = path.join(directory, 'compiler-must-not-run');
fs.writeFileSync(configuration, JSON.stringify({directory, inputs: [], toolRoots: {buildScripts: [], procMacros: [], libraries: []}}));
const rejected = spawnSync(process.execPath, [path.join(repository, 'providers/rust/capture.mjs'), '/usr/bin/touch', marker], {
  cwd: repository, encoding: 'utf8', timeout: 2000,
  env: {...process.env, KNOW_CODE_RUST_CAPTURE_CONFIG: configuration, CARGO_MAKEFLAGS: '--jobserver-auth=fifo:/unsupported'}});
assert(!rejected.error);
assert.notEqual(rejected.status, 0);
assert.match(rejected.stderr, /unsupported Cargo jobserver coordination format/);
assert(!fs.existsSync(marker));

const cache = path.join(directory, 'owned-cache'), foreign = path.join(directory, 'foreign');
fs.mkdirSync(path.join(cache, 'target'), {recursive: true});
fs.mkdirSync(path.join(cache, 'build')); fs.mkdirSync(foreign);
const sentinel = path.join(foreign, 'sentinel'), stateData = path.join(cache, 'units.json');
fs.writeFileSync(sentinel, 'outside compiler outputs'); fs.writeFileSync(stateData, 'cache state is not a compiler output');
const escaped = path.join(cache, 'target/escaped'); fs.symlinkSync(foreign, escaped, 'dir');
const linked = path.join(cache, 'build/linked'); fs.symlinkSync(sentinel, linked);
const fakeArtifacts = {filenames: [sentinel, stateData, path.join(escaped, 'sentinel'), linked]};
for (const filename of fakeArtifacts.filenames) assert.throws(() => removeCapturedOutput(cache, filename), /refusing/);
assert.equal(fs.readFileSync(sentinel, 'utf8'), 'outside compiler outputs');
assert.equal(fs.readFileSync(stateData, 'utf8'), 'cache state is not a compiler output');
assert(fs.lstatSync(escaped).isSymbolicLink() && fs.lstatSync(linked).isSymbolicLink());
const linkedCache = path.join(directory, 'linked-root-cache');
fs.mkdirSync(linkedCache); fs.symlinkSync(foreign, path.join(linkedCache, 'target'), 'dir');
assert.throws(() => removeCapturedOutput(linkedCache, path.join(linkedCache, 'target/sentinel')), /refusing/);
const ownedOutput = path.join(cache, 'build/owned-metadata'); fs.writeFileSync(ownedOutput, 'owned');
removeCapturedOutput(cache, ownedOutput); assert(!fs.existsSync(ownedOutput));

fs.writeFileSync(path.join(directory, 'summary.json'), JSON.stringify({passed: true, assertions: [
  'FIFO setup/source/artifact reads fail without blocking', 'derived FIFO target replaced atomically',
  'partial-write ENOSPC simulation cleans the owned temporary file', 'unsupported jobserver format refused before tool execution',
  'bare Cargo proc_macro permits macro/test units only with sysroot-only direct lookup; unknown/conflicting search forms rejected',
  'typed host-tool dependency closure permits only intermediate host lib/rlib link outputs and excludes dev/normal-app/test/final products',
  'only an explicitly allowed syntax-presentation RPC failure omits proof; owned source-read and transport failures remain fatal',
  'Cargo environment sets/overrides and removals bind hashed parent origins without dumping unchanged ambient values',
  'fake artifact cleanup preserves foreign/state/symlink sentinels and removes only owned target/build files'],
  simulatedWriteFailure: {ownedFilesBeforeFailure: ownedBeforeFailure, retainedFilesAfterCleanup: 0}}, null, 2));
process.stdout.write(`${directory}\n`);
