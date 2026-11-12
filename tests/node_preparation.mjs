// Real package-manager matrix through present-policy framed compiler requests.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import {spawnSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import {createHash} from 'node:crypto';
import protobuf from 'protobufjs';
import {packageTool, runCommand, selection} from '../providers/typescript/package-tools.mjs';

const root = fileURLToPath(new URL('..', import.meta.url));
const run = path.join(root, '.artifacts/node-preparation', Date.now().toString(16));
fs.mkdirSync(run, {recursive: true});
const cache = path.resolve(process.env.KNOW_CODE_TEST_TOOL_CACHE ?? path.join(root, '.artifacts/node-preparation/tools'));
const schema = await protobuf.load(path.join(root, 'proto/analysis.proto'));
const Envelope = schema.lookupType('knowcode.Envelope');
const versions = process.env.KNOW_CODE_TEST_PACKAGE_PINS?.split(',') ?? [
  'npm@10.0.0', 'npm@10.9.10', 'npm@11.0.0', 'npm@11.21.2',
  'pnpm@10.0.0', 'pnpm@10.34.9', 'pnpm@11.0.0', 'pnpm@11.28.11', 'pnpm@12.0.0', 'pnpm@12.18.0'
];
const write = (filename, text) => {fs.mkdirSync(path.dirname(filename), {recursive: true}); fs.writeFileSync(filename, text);};
const environment = {...process.env, npm_config_cache: path.join(run, 'npm-cache'),
  npm_config_store_dir: path.join(run, 'pnpm-store'), pnpm_config_store_dir: path.join(run, 'pnpm-store'),
  XDG_DATA_HOME: path.join(run, 'data'), XDG_CACHE_HOME: path.join(run, 'cache'), XDG_CONFIG_HOME: path.join(run, 'config')};
const offlineModule = path.join(run, 'offline-fetch.mjs');
write(offlineModule, "globalThis.fetch = () => { throw new Error('offline worker fixture'); };\n");
function worker(directory, preparations = [], scripts = false, extra = {}) {
  const {configPath, blockFetch, ...overrides} = extra;
  const config = configPath ?? path.join(directory, 'packages/app/tsconfig.json');
  const request = {workspaceRoot: directory, projects: [config], configuredProjects: [config], looseSources: [],
    preparations, preparationFingerprint: 'saved-inputs', analysisPolicy: {version: 1, node: {cacheDirectory: cache, allowInstallScripts: scripts}}, ...overrides};
  const bytes = Envelope.encode(Envelope.create({protocolVersion: 7, jobId: 'package-matrix', analyze: request})).finish();
  const header = Buffer.alloc(4); header.writeUInt32LE(bytes.length);
  const result = spawnSync(process.execPath, ['--expose-gc', path.join(root, 'providers/typescript/worker.mjs')],
    {cwd: directory, input: Buffer.concat([header, bytes]), detached: true, timeout: 120000, maxBuffer: 16 * 1024 * 1024,
      env: blockFetch ? {...environment, NODE_OPTIONS: `${environment.NODE_OPTIONS ?? ''} --import=${offlineModule}`} : environment});
  if (result.error && result.pid) { try { process.kill(-result.pid, 'SIGTERM'); } catch { /* Already exited. */ } }
  assert.equal(result.status, 0, result.stderr?.toString() ?? String(result.error));
  const messages = [];
  for (let offset = 0; offset < result.stdout.length;) {
    const size = result.stdout.readUInt32LE(offset); offset += 4;
    assert(size > 0 && size <= 8 * 1024 * 1024);
    const message = Envelope.toObject(Envelope.decode(result.stdout.subarray(offset, offset + size)), {defaults: true, longs: Number, bytes: String});
    assert.equal(message.jobId, 'package-matrix'); assert.equal(message.protocolVersion, 7);
    messages.push(message); offset += size;
  }
  assert.equal(messages[0].provider.analysisPolicyVersion, 1);
  assert(messages.at(-1).complete.success);
  return messages;
}
const payloads = (messages, key) => messages.filter(m => m[key]).map(m => m[key]);
const dependency = path.join(run, 'dependency/package');
write(path.join(dependency, 'package.json'), JSON.stringify({name: 'know-code-package-dependency', version: '1.0.0', types: 'index.d.ts'}));
write(path.join(dependency, 'index.d.ts'), 'export declare const value: number;\n');
const platformDependency = path.join(run, 'platform-dependency');
write(path.join(platformDependency, 'package.json'), JSON.stringify({name: 'know-code-other-platform', version: '1.0.0', os: ['linux']}));
const cells = [];
for (const pin of versions) {
  const selected = selection(pin), directory = path.join(run, pin.replace('@', '-'));
  const app = path.join(directory, 'packages/app');
  write(path.join(directory, 'package.json'), JSON.stringify({name: 'know-code-package-matrix', private: true, packageManager: pin,
    ...(selected.name === 'npm' ? {workspaces: ['packages/*']} : {}),
    scripts: {postinstall: 'node -e "require(\'fs\').writeFileSync(\'script-ran\',\'yes\')"'}}));
  if (selected.name === 'pnpm') {
    write(path.join(directory, 'pnpm-workspace.yaml'), 'packages:\n  - packages/*\nignoreScripts: false\nignorePnpmfile: false\n');
    write(path.join(directory, '.pnpmfile.cjs'), "require('fs').writeFileSync('hook-ran','yes'); module.exports={hooks:{readPackage:p=>p}};\n");
  }
  write(path.join(directory, '.npmrc'), 'ignore-scripts=false\n');
  write(path.join(app, 'package.json'), JSON.stringify({name: 'know-code-package-app', version: '1.0.0', private: true,
    dependencies: {'know-code-package-dependency': `file:${dependency}`, 'know-code-package-sibling': selected.name === 'pnpm' ? 'workspace:*' : '*'}}));
  write(path.join(directory, 'packages/sibling/package.json'), JSON.stringify({name: 'know-code-package-sibling', version: '1.0.0', types: 'index.d.ts'}));
  write(path.join(directory, 'packages/sibling/index.d.ts'), 'export declare const sibling: number;\n');
  write(path.join(app, 'tsconfig.json'), JSON.stringify({compilerOptions: {target: 'ES2022', module: 'NodeNext'}, include: ['index.ts']}));
  write(path.join(app, 'index.ts'), 'import {value} from "know-code-package-dependency"; import {sibling} from "know-code-package-sibling"; export const total = value + sibling;\n');
  console.log(`Testing ${pin}`);
  const first = worker(directory);
  write(path.join(directory, 'first-result.json'), JSON.stringify(first, null, 2));
  const prepared = payloads(first, 'preparation');
  assert.equal(prepared.length, 1);
  assert(prepared[0].succeeded, prepared[0].error);
  assert(!fs.existsSync(path.join(directory, 'script-ran')), 'unauthorized lifecycle script ran');
  assert(!fs.existsSync(path.join(directory, 'hook-ran')), 'unauthorized pnpm hook ran');
  assert(!fs.existsSync(path.join(directory, selected.name === 'npm' ? 'package-lock.json' : 'pnpm-lock.yaml')), 'unlocked restore wrote source lockfile');
  assert(payloads(first, 'report').every(r => r.status === 'complete'), JSON.stringify(payloads(first, 'report')));
  const comp = payloads(first, 'compilation')[0];
  assert.equal(JSON.parse(comp.optionsJson).knowCodePackageManager.version, selected.version);
  assert(comp.inputPaths.includes(path.join(app, 'package.json')));
  const same = worker(directory, prepared);
  assert.equal(payloads(same, 'preparation').length, 0, 'unchanged save repeated restoration');
  write(path.join(app, 'index.ts'), fs.readFileSync(path.join(app, 'index.ts'), 'utf8') + 'export const saved = 1;\n');
  assert.equal(payloads(worker(directory, prepared), 'preparation').length, 0, 'source save repeated restoration');
  const opted = worker(directory, prepared, true);
  write(path.join(directory, 'opted-result.json'), JSON.stringify(opted, null, 2));
  assert(payloads(opted, 'preparation')[0].succeeded, JSON.stringify(payloads(opted, 'preparation')));
  assert.notEqual(payloads(opted, 'preparation')[0].fingerprint, prepared[0].fingerprint, 'grant did not invalidate preparation');
  // pnpm may legitimately skip lifecycle hooks on an unchanged no-op install.
  // Missing outputs require restoration, which must now permit authorized hooks.
  for (const name of ['node_modules', 'packages/app/node_modules', 'packages/sibling/node_modules'])
    fs.rmSync(path.join(directory, name), {recursive: true, force: true});
  const restored = worker(directory, payloads(opted, 'preparation'), true);
  assert(payloads(restored, 'preparation')[0].succeeded, JSON.stringify(payloads(restored, 'preparation')));
  assert(fs.existsSync(path.join(directory, 'script-ran')), 'personal script grant did not permit root script');
  if (selected.name === 'pnpm') assert(fs.existsSync(path.join(directory, 'hook-ran')), 'personal grant did not permit pnpm hook');
  const tool = await packageTool(selected, cache, () => {});
  // pnpm 10's up-to-date fast path can skip creating a requested author lockfile.
  for (const name of ['node_modules', 'packages/app/node_modules', 'packages/sibling/node_modules', 'script-ran', 'hook-ran'])
    fs.rmSync(path.join(directory, name), {recursive: true, force: true});
  const lockArgs = selected.name === 'npm' ? ['install', '--package-lock-only', '--ignore-scripts', '--no-audit', '--no-fund', '--no-update-notifier', '--logs-max=0'] :
    ['install', '--lockfile-only', '--config.lockfile=true', '--ignore-scripts', '--ignore-pnpmfile', ...(selected.major === 10 ? ['--config.manage-package-manager-versions=false'] : []), '--config.runtime=false', '--config.updateNotifier=false'];
  await runCommand(tool.program, [...tool.prefix, ...lockArgs], directory, undefined, environment);
  const lockPath = path.join(directory, selected.name === 'npm' ? 'package-lock.json' : 'pnpm-lock.yaml');
  const lockBytes = fs.readFileSync(lockPath);
  const locked = worker(directory, payloads(restored, 'preparation'));
  assert(payloads(locked, 'preparation')[0].succeeded, JSON.stringify(payloads(locked, 'preparation')));
  assert.deepEqual(fs.readFileSync(lockPath), lockBytes, 'frozen restoration rewrote source lockfile');
  assert(!fs.existsSync(path.join(directory, 'script-ran')) && !fs.existsSync(path.join(directory, 'hook-ran')));
  const manifestPath = path.join(app, 'package.json'), original = fs.readFileSync(manifestPath);
  const broken = JSON.parse(original); broken.dependencies['absent-package'] = `file:${path.join(run, 'absent.tgz')}`;
  write(manifestPath, JSON.stringify(broken));
  const failed = worker(directory, payloads(locked, 'preparation'));
  assert.equal(payloads(failed, 'preparation')[0].succeeded, false, 'broken frozen inputs claimed preparation success');
  assert(payloads(failed, 'report').some(r => r.diagnostics.some(d => d.code === 'dependency_restore_failed')));
  assert(payloads(failed, 'facts').some(f => f.symbols.some(s => s.name === 'total')), 'setup failure discarded valid compiler graph');
  assert.deepEqual(fs.readFileSync(lockPath), lockBytes, 'failed restore rewrote lockfile');
  assert.equal(payloads(worker(directory, payloads(failed, 'preparation')), 'preparation').length, 0, 'failed checkpoint retried on ordinary save');
  assert.equal(payloads(worker(directory, payloads(failed, 'preparation'), false, {retryPreparation: true}), 'preparation').length, 1, 'explicit retry did not retry');
  write(manifestPath, original);
  const repaired = worker(directory, payloads(failed, 'preparation'));
  assert(payloads(repaired, 'preparation')[0].succeeded);
  const standalone = path.join(run, `${pin.replace('@', '-')}-standalone`);
  write(path.join(standalone, 'package.json'), JSON.stringify({name: 'know-code-standalone', private: true, packageManager: pin,
    dependencies: {'know-code-package-dependency': `file:${dependency}`}}));
  write(path.join(standalone, 'tsconfig.json'), JSON.stringify({compilerOptions: {module: 'NodeNext'}, include: ['index.ts']}));
  write(path.join(standalone, 'index.ts'), 'import {value} from "know-code-package-dependency"; export const standalone = value;\n');
  const single = worker(standalone, [], false, {configPath: path.join(standalone, 'tsconfig.json')});
  assert(payloads(single, 'preparation')[0].succeeded, JSON.stringify(payloads(single, 'preparation')));
  assert(payloads(single, 'report').every(r => r.status === 'complete'));
  const optional = path.join(run, `${pin.replace('@', '-')}-optional`);
  write(path.join(optional, 'package.json'), JSON.stringify({name: 'know-code-optional', private: true, packageManager: pin,
    optionalDependencies: {'know-code-package-dependency': `file:${dependency}`, 'know-code-other-platform': `file:${platformDependency}`}}));
  write(path.join(optional, 'tsconfig.json'), fs.readFileSync(path.join(standalone, 'tsconfig.json')));
  write(path.join(optional, 'index.ts'), fs.readFileSync(path.join(standalone, 'index.ts')));
  const optionalConfig = {configPath: path.join(optional, 'tsconfig.json')};
  const optionalFirst = worker(optional, [], false, optionalConfig);
  assert(payloads(optionalFirst, 'preparation')[0].succeeded, JSON.stringify(payloads(optionalFirst, 'preparation')));
  assert(payloads(optionalFirst, 'report').every(r => r.status === 'complete'), 'optional-only compiler dependency was not restored');
  assert.equal(payloads(worker(optional, payloads(optionalFirst, 'preparation'), false, optionalConfig), 'preparation').length, 0,
    'legitimately absent optional output repeated restoration');
  fs.rmSync(path.join(optional, 'node_modules/know-code-package-dependency'), {recursive: true, force: true});
  const optionalRepaired = worker(optional, payloads(optionalFirst, 'preparation'), false, optionalConfig);
  assert(payloads(optionalRepaired, 'preparation')[0].succeeded, JSON.stringify(payloads(optionalRepaired, 'preparation')));
  cells.push({pin, result: 'passed', checks: ['workspace_and_single_restore', 'compiler_graph', 'scripts_hooks_off_despite_repo_config',
    'unchanged_and_source_save_dedup', 'personal_grant_invalidation', 'missing_outputs_restore', 'frozen_lock_preservation',
    'failed_checkpoint_dedup', 'explicit_retry', 'partial_compiler_graph', 'repair', 'optional_only_setup_and_optional_omission', 'removed_optional_output_restore'], directory});
  fs.writeFileSync(path.join(run, 'cells.json'), JSON.stringify(cells, null, 2) + '\n');
}
for (const pin of ['npm@9.0.0', 'pnpm@9.0.0', 'pnpm@13.0.0', 'pnpm@12.0.0-rc.1', 'yarn@4.0.0'])
  assert.throws(() => selection(pin), /Unsupported package manager/);
const lastDirectory = cells.at(-1).directory;
let hookResult = worker(lastDirectory);
assert(payloads(hookResult, 'compilation')[0].inputPaths.includes(path.join(lastDirectory, '.pnpmfile.mjs')),
  'absent default hook candidate is untracked');
const newHook = path.join(lastDirectory, '.pnpmfile.mjs');
write(newHook, "throw new Error('untrusted new hook must not execute');\n");
const createdHook = worker(lastDirectory, payloads(hookResult, 'preparation'));
assert.equal(payloads(createdHook, 'preparation').length, 1, 'new default hook did not invalidate preparation');
assert(payloads(createdHook, 'preparation')[0].succeeded);
// Track selected INI and user YAML paths without executing them for discovery.
const customHook = path.join(run, 'custom-external-hook.cjs');
write(path.join(lastDirectory, '.npmrc'), `ignore-scripts=false\npnpmfile="${customHook}"\n`);
const selectedHook = worker(lastDirectory);
assert(payloads(selectedHook, 'compilation')[0].inputPaths.includes(customHook), 'missing INI-selected external hook untracked');
write(customHook, "throw new Error('custom external hook must remain suppressed');\n");
const changedHook = worker(lastDirectory, payloads(selectedHook, 'preparation'));
assert.equal(payloads(changedHook, 'preparation').length, 1, 'selected external hook creation did not invalidate setup');
assert(payloads(changedHook, 'preparation')[0].succeeded);
const userHook = path.join(run, 'user-external-hook.cjs');
write(path.join(environment.XDG_CONFIG_HOME, 'pnpm/config.yaml'), `globalPnpmfile: ${JSON.stringify(userHook)}\n`);
const userSelected = worker(lastDirectory);
assert(payloads(userSelected, 'compilation')[0].inputPaths.includes(userHook), 'missing user-selected YAML hook untracked');
write(userHook, "throw new Error('user hook still requires a personal workspace grant');\n");
const userChanged = worker(lastDirectory, payloads(userSelected, 'preparation'));
assert.equal(payloads(userChanged, 'preparation').length, 1, 'selected user hook contents did not invalidate setup');
assert(payloads(userChanged, 'preparation')[0].succeeded);
const arbitraryConfig = path.join(run, 'company-npm-config');
const arbitraryHook = path.join(run, 'company-external-hook.cjs');
write(arbitraryConfig, `global-pnpmfile=${arbitraryHook}\n`);
environment.npm_config_userconfig = arbitraryConfig;
const arbitrarySelected = worker(lastDirectory);
assert(payloads(arbitrarySelected, 'compilation')[0].inputPaths.includes(arbitraryHook), 'arbitrary-basename user INI selector was not parsed');
write(arbitraryHook, "throw new Error('company hook must remain suppressed without personal grant');\n");
const arbitraryChanged = worker(lastDirectory, payloads(arbitrarySelected, 'preparation'));
assert.equal(payloads(arbitraryChanged, 'preparation').length, 1);
assert(payloads(arbitraryChanged, 'preparation')[0].succeeded);
const selectedManifest = path.join(lastDirectory, 'package.json'), selectedBytes = fs.readFileSync(selectedManifest);
const unsupported = JSON.parse(selectedBytes); unsupported.packageManager = 'pnpm@13.0.0';
write(selectedManifest, JSON.stringify(unsupported));
const unsupportedResult = worker(lastDirectory);
assert(payloads(unsupportedResult, 'preparation')[0].error.includes('No substitution was made'));
assert(payloads(unsupportedResult, 'report').some(r => r.status === 'partial'));
write(selectedManifest, selectedBytes);
const coldResult = worker(lastDirectory, [], false, {retryPreparation: true, blockFetch: true,
  analysisPolicy: {version: 1, node: {cacheDirectory: path.join(run, 'worker-cold-cache'), allowInstallScripts: false}}});
assert.equal(payloads(coldResult, 'preparation')[0].succeeded, false);
assert(payloads(coldResult, 'report').some(r => r.status === 'partial'));
write(path.join(run, 'cold-worker-result.json'), JSON.stringify(coldResult, null, 2));
const originalFetch = globalThis.fetch;
try {
  let cancelled = false;
  globalThis.fetch = async () => new Response(new ReadableStream({
    start(controller) { controller.enqueue(Buffer.alloc(600000)); controller.enqueue(Buffer.alloc(600000)); },
    cancel() { cancelled = true; }
  }));
  await assert.rejects(packageTool(selection('npm@11.21.2'), path.join(run, 'oversized-cache'), () => {}), /Oversized package-tool metadata/);
  assert(cancelled, 'oversized metadata response was not cancelled');
} finally { globalThis.fetch = originalFetch; }
try {
  globalThis.fetch = () => { throw new Error('offline fixture: no networking'); };
  for (const pin of versions) assert.equal((await packageTool(selection(pin), cache, () => {})).version, selection(pin).version);
  const coldCache = path.join(run, 'offline-cold-cache');
  await assert.rejects(packageTool(selection('npm@11.21.2'), coldCache, () => {}), /offline fixture/);
  assert.equal(fs.readdirSync(coldCache).length, 0, 'failed acquisition left a lock/staged cache entry');
} finally { globalThis.fetch = originalFetch; }

// Two real processes race for one cache entry. The registry is a deterministic
// local response fixture; actual upstream acquisition is tested separately.
const reference = await packageTool(selection('npm@11.21.2'), cache, () => {});
const simulated = path.join(run, 'cache-race');
fs.mkdirSync(simulated);
fs.cpSync(reference.cacheDirectory, path.join(simulated, 'package'), {recursive: true, verbatimSymlinks: true});
await runCommand('/usr/bin/tar', ['-czf', path.join(simulated, 'tool.tgz'), '-C', simulated, 'package'], run);
const compressed = fs.readFileSync(path.join(simulated, 'tool.tgz'));
write(path.join(simulated, 'metadata.json'), JSON.stringify({name: 'npm', version: '11.21.2', dist: {
  tarball: 'https://registry.npmjs.org/npm/-/tool-fixture.tgz', integrity: `sha512-${createHash('sha512').update(compressed).digest('base64')}`}}));
const counter = path.join(simulated, 'requests.log');
const driver = path.join(simulated, 'race.mjs');
write(driver, `import fs from 'node:fs';
import {packageTool,selection} from ${JSON.stringify(new URL('../providers/typescript/package-tools.mjs', import.meta.url).href)};
globalThis.fetch = async url => {
 fs.appendFileSync(${JSON.stringify(counter)}, String(url)+'\\n');
 await new Promise(resolve=>setTimeout(resolve,25));
 return new Response(fs.readFileSync(String(url).endsWith('.tgz') ? ${JSON.stringify(path.join(simulated, 'tool.tgz'))} : ${JSON.stringify(path.join(simulated, 'metadata.json'))}),{status:200});
};
console.log(JSON.stringify(await packageTool(selection('npm@11.21.2'), process.argv[2], ()=>{})));
`);
const raceCache = path.join(simulated, 'cache');
const race = await Promise.all([0, 1].map(() => runCommand(process.execPath, [driver, raceCache], root)));
const tools = race.map(JSON.parse);
assert.equal(tools[0].cacheDirectory, tools[1].cacheDirectory);
assert.equal(fs.readFileSync(counter, 'utf8').trim().split('\n').length, 2, 'concurrent cache acquisition downloaded more than one metadata/archive pair');
assert(fs.readdirSync(raceCache).every(p => !p.includes('.lock') && !p.includes('.staging-')));
const manifest = path.join(tools[0].cacheDirectory, 'tool.json');
const installed=tools[0].cacheDirectory;
const savedManifest=fs.readFileSync(manifest);
const requiredModule=path.join(installed,'lib/cli.js'), savedModule=fs.readFileSync(requiredModule);
const savedMode=fs.statSync(requiredModule).mode&0o777;
const restoreModule=()=>{fs.writeFileSync(requiredModule,savedModule);fs.chmodSync(requiredModule,savedMode);};
const extra=path.join(installed,'unexpected-module.js');
const outsideModule=path.join(simulated,'outside.js');fs.writeFileSync(outsideModule,'export {};\n');
const corruptionChecks=[];
try {
  globalThis.fetch = () => { throw new Error('offline corrupted cache'); };
  for (const [name,damage,repair] of [
    ['changed_required_module',()=>fs.appendFileSync(requiredModule,'\n// changed module\n'),restoreModule],
    ['missing_required_module',()=>fs.unlinkSync(requiredModule),restoreModule],
    ['changed_executable_mode',()=>fs.chmodSync(requiredModule,savedMode^0o100),()=>fs.chmodSync(requiredModule,savedMode)],
    ['added_file',()=>fs.writeFileSync(extra,'export {};\n'),()=>fs.unlinkSync(extra)],
    ['outside_symlink',()=>fs.symlinkSync(path.relative(installed,outsideModule),extra),()=>fs.unlinkSync(extra)],
    ['legacy_entry_only_manifest',()=>write(manifest,JSON.stringify({...JSON.parse(savedManifest),format:1})),()=>fs.writeFileSync(manifest,savedManifest)]
  ]) {
    damage();
    try { await assert.rejects(packageTool(selection('npm@11.21.2'),raceCache,()=>{}),/offline corrupted cache/); }
    finally { repair(); }
    assert.equal((await packageTool(selection('npm@11.21.2'),raceCache,()=>{})).source,'user-cache');
    assert(fs.readdirSync(raceCache).every(p=>!p.includes('.lock')&&!p.includes('.staging-')));
    corruptionChecks.push(name);
  }
} finally { globalThis.fetch=originalFetch; }
const corrupted = JSON.parse(fs.readFileSync(manifest)); corrupted.version = '0.0.0'; write(manifest, JSON.stringify(corrupted));
try {
  globalThis.fetch = () => { throw new Error('offline corrupted cache'); };
  await assert.rejects(packageTool(selection('npm@11.21.2'), raceCache, () => {}), /offline corrupted cache/);
} finally { globalThis.fetch = originalFetch; }
const summary = {result: 'passed', node: process.version, cache, cells,
  wholeCacheCorruptionChecks: corruptionChecks,
  cacheChecks: ['unsupported_pins', 'bounded_metadata_stream', 'warm_offline_reuse', 'cold_offline_failure_and_cleanup', 'two_process_acquisition_deduplication', 'corrupted_cache_not_used'],
  inputChecks: ['absent_default_hook_creation', 'ini_selected_external_hook_creation', 'user_yaml_selected_external_hook_change', 'arbitrary_basename_user_ini_hook']};
fs.writeFileSync(path.join(run, 'result.json'), JSON.stringify(summary, null, 2) + '\n');
write(path.join(root, '.artifacts/node-preparation/latest.json'), JSON.stringify({...summary, evidence: run}, null, 2) + '\n');
console.log(JSON.stringify({result: summary.result, cells: cells.length, evidence: run}));
