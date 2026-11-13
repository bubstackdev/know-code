// Actual owned-Cargo collection/cache checks; shared validation owner executes.
import fs from 'node:fs';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import {spawnSync} from 'node:child_process';
import assert from 'node:assert/strict';
import {collectCargoContexts} from '../providers/rust/cargo-contexts.mjs';
import {compilerEnvironment} from '../providers/rust/lsp.mjs';

const repository = fileURLToPath(new URL('..', import.meta.url));
const state = path.join(repository, '.artifacts/rust-context-collection', `${Date.now()}-${process.pid}`);
const workspace = path.join(state, 'fixture');
fs.mkdirSync(state, {recursive: true});
fs.cpSync(path.join(repository, 'tests/fixtures/rust'), workspace, {recursive: true});
const buildScript = path.join(workspace, 'build.rs');
// The marker is an output. Without this declaration Cargo's default package
// scan sees that output change and correctly reruns the script on every check.
fs.writeFileSync(buildScript, fs.readFileSync(buildScript, 'utf8').replace(/}\s*$/, '    println!("cargo:rerun-if-changed=build.rs");\n}\n'));
const manifest = path.join(workspace, 'Cargo.toml'), library = path.join(workspace, 'src/lib.rs');
const data = path.join(workspace, 'src/input.txt');
fs.writeFileSync(data, 'saved 🦀 data\n');
fs.appendFileSync(library, '\npub const INCLUDED: &str = include_str!("input.txt");\n');
const configInput = path.join(workspace, '.cargo/config.toml');
fs.mkdirSync(path.dirname(configInput), {recursive: true});
fs.writeFileSync(configInput, '[env]\nKNOW_CODE_OWNED_CONTEXT_ENV={value="cargo-selected",force=true}\n');
process.env.KNOW_CODE_OWNED_CONTEXT_ENV = 'owned inherited value';
process.env.KNOW_CODE_OWNED_AMBIENT_SENTINEL = 'owned ambient value must not be retained';
fs.appendFileSync(library, 'pub const FROM_CARGO_ENV: &str = env!("KNOW_CODE_OWNED_CONTEXT_ENV");\n');

function command(tool, args, cwd) {
  const result = spawnSync(tool, args, {cwd, env: compilerEnvironment(), encoding: 'utf8', timeout: 120000, maxBuffer: 8 * 1024 * 1024});
  assert.equal(result.status, 0, result.stderr);
  return result.stdout.trim();
}
const metadata = JSON.parse(command('cargo', ['metadata', '--offline', '--format-version', '1', '--manifest-path', manifest], workspace));
const setupInputs = [manifest, path.join(workspace, 'Cargo.lock')].filter(filename => fs.existsSync(filename));
const policy = {targets: [], features: [], defaultFeatures: true, executionAllowed: true, cacheDirectory: path.join(state, 'cache')};
function collect(label) {
  const progress = [];
  const result = collectCargoContexts({manifest, metadata, policy, setupInputs, command, send: (kind, item) => progress.push({kind, item})});
  fs.writeFileSync(path.join(state, `${label}.json`), JSON.stringify({result, progress}, null, 2));
  return result;
}

const cold = collect('cold-include-discovery');
assert.equal(cold.captureAttempts, 2, 'unknown include input was not recaptured once');
assert.equal(cold.plans.length, 2, JSON.stringify(cold.diagnostics));
assert(cold.cargoSucceeded && fs.existsSync(path.join(workspace, 'know-code-build-ran')));
assert(cold.plans.every(plan => plan.unit.inputs.files.includes(data)));
assert(cold.plans.every(plan => plan.unit.cfg.effective.includes('know_code_generated')));
assert(cold.plans.every(plan => plan.unit.record.environment.KNOW_CODE_OWNED_CONTEXT_ENV === 'cargo-selected'));
assert(cold.plans.every(plan => plan.unit.inputs.environment.KNOW_CODE_OWNED_CONTEXT_ENV === 'cargo-selected'));
assert(!JSON.stringify(cold.units).includes('owned ambient value must not be retained'), 'unchanged ambient value was dumped');

assert(cold.plans.every(plan => plan.graph.firstPartyFiles.includes(library)));
assert(cold.plans.every(plan => !plan.graph.firstPartyFiles.includes(path.join(workspace, 'build.rs'))), 'tooling source was duplicated inside ordinary library units');

const warm = collect('warm');
assert.equal(warm.captureAttempts, 1);
assert.equal(warm.capturedCount, 0);
assert.equal(warm.plans.length, 2, JSON.stringify(warm.diagnostics));
fs.appendFileSync(data, 'include data changed\n');
const changed = collect('changed-include');
assert.equal(changed.captureAttempts, 1, 'known consumed input needed an avoidable discovery pass');
assert.equal(changed.plans.length, 2, JSON.stringify(changed.diagnostics));
assert(changed.capturedCount >= 2);

const metadataOutput = changed.plans[0].unit.record.artifacts.find(item => item.emit === 'metadata');
assert(metadataOutput);
fs.appendFileSync(path.resolve(changed.plans[0].unit.record.cwd, metadataOutput.artifact), 'owned artifact replaced');
const repaired = collect('replaced-artifact');
assert.equal(repaired.plans.length, 2, JSON.stringify(repaired.diagnostics));
assert(repaired.capturedCount > 0, 'replaced artifact accepted from stale cache proof');

fs.appendFileSync(library, '#[cfg(test)] compile_error!("owned failing test context");\n');
const partial = collect('compiler-partial');
assert(!partial.cargoSucceeded);
assert(partial.plans.some(plan => !plan.unit.cfg.test), JSON.stringify(partial.diagnostics));
assert(!partial.plans.some(plan => plan.unit.cfg.test), 'old successful test body proof was reused');
assert(partial.failed.some(record => record.args.includes('--test')));
assert(partial.diagnostics.some(item => item.severity === 'error' && item.message.includes('owned failing test context')));

const configDirectory = path.join(workspace, '.cargo');
fs.mkdirSync(configDirectory, {recursive: true});
const wrapper = path.join(state, 'never-execute-wrapper.sh'), marker = path.join(state, 'wrapper-executed');
fs.writeFileSync(wrapper, `#!/bin/sh\ntouch '${marker}'\nexit 99\n`); fs.chmodSync(wrapper, 0o755);
fs.writeFileSync(path.join(configDirectory, 'config.toml'), `[build]\nrustc-wrapper=${JSON.stringify(wrapper)}\n`);
assert.throws(() => collect('configured-wrapper'), /wrapper\/include\/quoted-key configuration/);
assert(!fs.existsSync(marker), 'selected wrapper was silently replaced or executed to capture');

const captureDirectories = fs.readdirSync(path.join(policy.cacheDirectory, 'contexts')).flatMap(name =>
  fs.readdirSync(path.join(policy.cacheDirectory, 'contexts', name)).filter(item => item.startsWith('capture-')));
assert.equal(captureDirectories.length, 0, 'scratch captures retained after successful cleanup');
fs.writeFileSync(path.join(state, 'summary.json'), JSON.stringify({passed: true, assertions: [
  'bounded package snapshots and one include-discovery recapture', 'unchanged cache avoids compilation including Cargo-injected env! input',
  'actual Cargo env overrides replay while unrelated ambient values are not retained',
  'known include save refreshes directly', 'artifact replacement recaptures', 'compiler failure preserves healthy proven units',
  'configured wrapper stops before capture', 'scratch capture cleanup'],
  limits: ['worker routing not integrated', 'proc-macro graph replay and general escaped dep-info forms remain gaps']}, null, 2));
process.stdout.write(`${state}\n`);
