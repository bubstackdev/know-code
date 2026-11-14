// Focused actual-Cargo context evidence check. Run by the shared validation owner.
import fs from 'node:fs';
import path from 'node:path';
import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import {fileURLToPath, pathToFileURL} from 'node:url';
import {compilerEnvironment, Analyzer} from '../providers/rust/lsp.mjs';
import {capturedUnit, capturedUnitCurrent, connectCapturedUnits, selectedProject} from '../providers/rust/contexts.mjs';

const repository = fileURLToPath(new URL('..', import.meta.url));
const state = path.join(repository, '.artifacts/rust-contexts', `${Date.now()}-${process.pid}`);
const workspace = path.join(state, 'fixture'), target = path.join(state, 'target');
fs.mkdirSync(path.join(workspace, 'src'), {recursive: true});
fs.mkdirSync(path.join(workspace, 'tests'));
const manifest = path.join(workspace, 'Cargo.toml'), library = path.join(workspace, 'src/lib.rs');
const helper = path.join(workspace, 'src/helper.rs'), data = path.join(workspace, 'src/data.txt');
const binary = path.join(workspace, 'src/main.rs'), integration = path.join(workspace, 'tests/integration.rs');
fs.writeFileSync(manifest, '[package]\nname="owned-contexts"\nversion="0.1.0"\nedition="2024"\n[workspace]\n');
fs.writeFileSync(library, 'mod helper;\npub const DATA: &str = include_str!("data.txt");\n'
  + '#[cfg(debug_assertions)] pub fn profile_leaf() -> usize { 11 }\n'
  + '#[cfg(not(debug_assertions))] pub fn profile_leaf() -> usize { 22 }\n'
  + '#[cfg(target_feature="aes")] pub fn feature_leaf() -> usize { 33 }\n'
  + '#[cfg(not(target_feature="aes"))] pub fn feature_leaf() -> usize { 44 }\n'
  + 'pub fn profile_call() -> usize { profile_leaf() }\n'
  + 'pub fn feature_call() -> usize { feature_leaf() }\n'
  + 'pub fn entry() -> usize { helper::helper() }\n');
fs.writeFileSync(helper, 'pub fn helper() -> usize { 1 }\n');
fs.writeFileSync(data, 'saved 🦀 include data\n');
fs.writeFileSync(binary, 'fn main() { owned_contexts::entry(); }\n');
fs.writeFileSync(integration, '#[test] fn smoke() { assert!(owned_contexts::entry() > 0); }\n');
const inputs = [manifest, library, helper, data, binary, integration];
const capture = path.join(repository, 'providers/rust/capture.mjs');
const env = {...compilerEnvironment(), CARGO_TARGET_DIR: target, RUSTC_WRAPPER: capture,
  RUSTFLAGS: '-Cdebug-assertions=off -Ctarget-feature=+aes'};
delete env.RUSTC_WORKSPACE_WRAPPER;
function run(tool, args, cwd = workspace, environment = env) {
  const result = spawnSync(tool, args, {cwd, env: environment, encoding: 'utf8', timeout: 120000, maxBuffer: 8 * 1024 * 1024});
  if (result.error || result.status !== 0) throw new Error(`${tool} failed: ${result.error?.message ?? result.stderr}`);
  return result.stdout.trim();
}
const metadata = JSON.parse(run('cargo', ['metadata', '--offline', '--no-deps', '--format-version', '1']));
const command = (tool, args, cwd) => run(tool, args, cwd, compilerEnvironment());
function check(label, success = true, previous = []) {
  const directory = path.join(state, label);
  fs.mkdirSync(directory);
  const configuration = path.join(directory, 'configuration.json');
  fs.writeFileSync(configuration, JSON.stringify({directory, inputs, toolRoots: {buildScripts: [], procMacros: [], libraries: []}}));
  const result = spawnSync('cargo', ['check', '--offline', '--all-targets', '--jobs', '1', '--message-format=json'],
    {cwd: workspace, env: {...env, KNOW_CODE_RUST_CAPTURE_CONFIG: configuration}, encoding: 'utf8', timeout: 120000, maxBuffer: 8 * 1024 * 1024});
  fs.writeFileSync(path.join(directory, 'stdout'), result.stdout ?? '');
  fs.writeFileSync(path.join(directory, 'stderr'), result.stderr ?? '');
  assert.equal(result.status === 0, success, result.stderr);
  const messages = (result.stdout ?? '').split('\n').flatMap(line => { try { return [JSON.parse(line)]; } catch { return []; } });
  const artifacts = messages.filter(item => item.reason === 'compiler-artifact');
  const records = fs.readdirSync(directory).filter(name => /^\d+\.json$/.test(name)).map(name => JSON.parse(fs.readFileSync(path.join(directory, name), 'utf8')));
  const units = [];
  for (const record of records.filter(item => item.beforeInputs && item.status === 0 && !item.error)) {
    const paths = record.artifacts.map(item => path.resolve(record.cwd, item.artifact));
    const matched = artifacts.filter(item => item.filenames.some(filename => paths.includes(filename)));
    assert.equal(matched.length, 1);
    units.push(capturedUnit(record, matched[0], command, [manifest]));
  }
  const captured = units.length;
  for (const artifact of artifacts.filter(item => item.fresh)) {
    const candidates = previous.filter(unit => capturedUnitCurrent(unit, artifact));
    if (candidates.length === 1) units.push({...candidates[0], cargo: artifact});
  }
  fs.writeFileSync(path.join(directory, 'units.json'), JSON.stringify(units, null, 2));
  return {units: connectCapturedUnits(units), artifacts, records, captured};
}
const cold = check('cold');
assert.equal(cold.units.length, 5);
const selected = cold.units.findIndex(unit => unit.cfg.root === library && !unit.cfg.test);
const unit = cold.units[selected], graph = selectedProject(cold.units, selected, metadata);
assert(unit.inputs.files.includes(helper) && unit.inputs.files.includes(data));
assert(!unit.cfg.effective.includes('debug_assertions'));
assert(unit.cfg.effective.includes('target_feature="aes"'));
assert(graph.cargoCfgs.includes('!debug_assertions'));
assert.equal(graph.analyzerCfg.setTest, false);
for (let index = 0; index < cold.units.length; index++) selectedProject(cold.units, index, metadata);

// Actual analyzer replay must select the compiler's profile/feature branches.
const analyzer = new Analyzer(workspace), replies = [];
const position = (text, offset) => { const lines = text.slice(0, offset).split('\n'); return {line: lines.length - 1, character: lines.at(-1).length}; };
try {
  await analyzer.request('initialize', {processId: process.pid, rootUri: pathToFileURL(workspace).href,
    capabilities: {general: {positionEncodings: ['utf-16']}, experimental: {serverStatusNotification: true}, textDocument: {definition: {linkSupport: true}}},
    initializationOptions: {linkedProjects: [graph.project], cfg: graph.analyzerCfg,
      cargo: {offline: true, cfgs: graph.cargoCfgs, buildScripts: {enable: false, useRustcWrapper: false}},
      check: {enable: false}, checkOnSave: false, procMacro: {enable: false}, cachePriming: {enable: false}, numThreads: 1}});
  analyzer.notify('initialized', {});
  assert.equal((await analyzer.ready()).health, 'ok');
  const text = fs.readFileSync(library, 'utf8'), uri = pathToFileURL(library).href;
  analyzer.notify('textDocument/didOpen', {textDocument: {uri, languageId: 'rust', version: 1, text}});
  for (const [call, declaration, token] of [
    ['pub fn profile_call() -> usize { profile_leaf() }', 'pub fn profile_leaf() -> usize { 22 }', 'profile_leaf'],
    ['pub fn feature_call() -> usize { feature_leaf() }', 'pub fn feature_leaf() -> usize { 33 }', 'feature_leaf']]) {
    const at = position(text, text.indexOf(call) + call.indexOf(token));
    const value = await analyzer.request('textDocument/definition', {textDocument: {uri}, position: at});
    const definitions = value ? Array.isArray(value) ? value : [value] : [];
    assert.equal(definitions.length, 1);
    const definition = definitions[0];
    assert.equal(definition.targetUri ?? definition.uri, uri);
    assert.deepEqual((definition.targetSelectionRange ?? definition.range).start, position(text, text.indexOf(declaration) + declaration.indexOf(token)));
    replies.push({call, definition});
  }
} finally { await analyzer.close(); }
fs.writeFileSync(path.join(state, 'cfg-replay.json'), JSON.stringify(replies, null, 2));

const warm = check('warm', true, cold.units);
assert.equal(warm.captured, 0, 'fresh Cargo outputs unexpectedly produced new captures');
assert(cold.units.every(saved => warm.artifacts.some(artifact => artifact.fresh && capturedUnitCurrent(saved, artifact))));
const bytes = fs.readFileSync(data);
fs.appendFileSync(data, 'changed include input\n');
assert(!capturedUnitCurrent(unit, unit.cargo), 'include data edit retained old proof');
fs.writeFileSync(data, bytes);
const output = unit.outputs.find(filename => filename.endsWith('.rmeta')), original = fs.readFileSync(output);
fs.appendFileSync(output, 'replaced output');
assert(!capturedUnitCurrent(unit, unit.cargo), 'artifact replacement retained old proof');
fs.writeFileSync(output, original);

// Different roots can consume the same module. File-only LSP cannot select
// that source's crate unambiguously, even though root_module differs.
fs.writeFileSync(binary, '#[path="helper.rs"] mod shared;\nfn main() { owned_contexts::entry(); shared::helper(); }\n');
const overlap = check('shared-module', true, warm.units);
const binaryContext = overlap.units.findIndex(item => item.cfg.root === binary && !item.cfg.test);
assert.throws(() => selectedProject(overlap.units, binaryContext, metadata), /shared Rust source/);
const healthy = overlap.units.findIndex(item => item.cfg.root === library && !item.cfg.test);
assert(selectedProject(overlap.units, healthy, metadata));

fs.appendFileSync(library, '#[cfg(test)] compile_error!("owned failed unit");\n');
const failed = check('failed-unit', false, overlap.units);
const failedRecord = failed.records.find(item => item.args.includes('--test') && item.status !== 0 && item.beforeInputs);
assert(failedRecord, 'no failed actual compiler invocation retained');
assert.throws(() => capturedUnit(failedRecord, unit.cargo, command), /failed compilation/);
assert(!capturedUnitCurrent(unit, unit.cargo), 'failed changed source retained old proof');
fs.writeFileSync(path.join(state, 'summary.json'), JSON.stringify({passed: true,
  assertions: ['five actual unit contexts', 'compiler effective profile/target-feature cfg and actual analyzer replay',
    'compiler dep-info module/include membership', 'matching fresh cache evidence', 'artifact content replacement rejected',
    'shared-module ambiguity rejected without blocking healthy context', 'failed compiler unit rejected'],
  limits: ['planning/capture helpers only; managed worker routing is not integrated',
    'fixture supplies bounded pre-compilation input inventory', 'general wrapper/config and generated-input boundaries remain unfinished']}, null, 2));
process.stdout.write(`${state}\n`);
