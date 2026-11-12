// Managed Cargo-check collection. Scheduling, grants and publication stay core-owned.
import fs from 'node:fs';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import {createHash} from 'node:crypto';
import {spawnSync} from 'node:child_process';
import {boundedRead, writeDerivedFile} from './preparation.mjs';
import {compilerEnvironment} from './lsp.mjs';
import {cargoSelection} from './policy.mjs';
import {cargoArtifactIdentity, capturedUnit, capturedUnitCurrent, compilerCfg, compilerInputs,
  connectCapturedUnits, selectedProject, fileEvidence} from './contexts.mjs';

const hash = value => createHash('sha256').update(value).digest('hex');
const LIMIT = 8 * 1024 * 1024, CACHE_LIMIT = 32 * 1024 * 1024;
const excluded = new Set(['target', '.git', 'node_modules', '.artifacts', '.research', '.build', '.worktrees', 'DerivedData']);
const within = (directory, filename) => { const value = path.relative(directory, filename); return value !== '..' && !value.startsWith(`..${path.sep}`) && !path.isAbsolute(value); };
const unavailable = message => { throw new Error(`Rust context unavailable: ${message}`); };
const warning = (code, message) => ({code, message: message.slice(-4096), severity: 'warning'});
function jsonFile(filename, value, limit = LIMIT) {
  const encoded = JSON.stringify(value);
  if (Buffer.byteLength(encoded) > limit) unavailable(`derived state exceeds ${limit} bytes`);
  writeDerivedFile(filename, encoded);
  return hash(encoded);
}

function guardConfiguration(manifest) {
  for (const name of ['RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER', 'CARGO_BUILD_RUSTC_WRAPPER', 'CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER'])
    if (process.env[name]) unavailable(`selected ${name} cannot yet be preserved by compiler capture`);
  const paths = new Set();
  for (let directory = path.dirname(manifest);; directory = path.dirname(directory)) {
    paths.add(path.join(directory, '.cargo/config')); paths.add(path.join(directory, '.cargo/config.toml'));
    if (path.dirname(directory) === directory) break;
  }
  const cargoHome = process.env.CARGO_HOME ?? path.join(process.env.HOME, '.cargo');
  paths.add(path.join(cargoHome, 'config')); paths.add(path.join(cargoHome, 'config.toml'));
  for (const filename of paths) if (fs.existsSync(filename)) {
    const contents = boundedRead(filename, LIMIT).toString('utf8');
    // Conservative boundary, not a replacement Cargo/TOML evaluator. Escaped
    // or quoted keys/includes might hide wrapper selection; refuse those forms.
    if (/rustc[-_](?:workspace[-_])?wrapper|^\s*include\s*=/m.test(contents) ||
        contents.split(/\r?\n/).some(line => {
          const key = line.split('=', 1)[0].trim();
          return !key.startsWith('#') && (key.includes('\\') || /^['"]/.test(key) || /^\[.*['"]/.test(key));
        })) unavailable(`wrapper/include/quoted-key configuration requires supported capture: ${filename}`);
  }
  return [...paths].filter(filename => fs.existsSync(filename));
}

function packageSources(metadata) {
  const packages = new Map(), visited = new Set();
  let entries = 0, bytes = 0;
  for (const item of metadata.packages) {
    const files = new Set(item.targets.map(target => target.src_path)), pending = [path.dirname(item.manifest_path)];
    while (pending.length) {
      const directory = pending.pop(), key = `${item.manifest_path}\0${directory}`;
      if (visited.has(key)) continue;
      visited.add(key);
      const handle = fs.opendirSync(directory);
      try {
        for (let entry; (entry = handle.readSync());) {
          if (++entries > 50000) unavailable('package source inventory exceeds 50000 entries');
          const filename = path.join(directory, entry.name);
          if (entry.isDirectory() && !excluded.has(entry.name) && !entry.name.startsWith('.build-')) pending.push(filename);
          else if (entry.isFile() && entry.name.endsWith('.rs')) files.add(filename);
        }
      } finally { handle.closeSync(); }
    }
    const selected = [...files].filter(filename => fs.existsSync(filename));
    bytes += selected.reduce((total, filename) => total + Buffer.byteLength(filename), 0);
    if (bytes > 4 * 1024 * 1024) unavailable('package source inventory paths exceed 4 MiB');
    packages.set(item.manifest_path, new Set(selected));
  }
  return packages;
}

function snapshotPackages(directory, packages) {
  const snapshots = {}, paths = [...new Set([...packages.values()].flatMap(files => [...files]))];
  if (paths.length > 16000 || paths.reduce((bytes, filename) => bytes + Buffer.byteLength(filename), 0) > 4 * 1024 * 1024)
    unavailable('package input snapshot path bounds exceeded');
  const hashes = fileEvidence(paths);
  for (const [manifest, files] of packages) {
    const filename = path.join(directory, `${hash(manifest)}.json`);
    const sha256 = jsonFile(filename, Object.fromEntries([...files].map(item => [item, hashes[item]])));
    snapshots[manifest] = {filename, sha256};
  }
  return snapshots;
}

function capturedRecords(directory) {
  const records = [], snapshots = new Map();
  let total = 0;
  for (const entry of fs.readdirSync(directory)) if (/^\d+\.json$/.test(entry)) {
    const bytes = boundedRead(path.join(directory, entry), LIMIT);
    total += bytes.length;
    if (records.length >= 16000 || total > CACHE_LIMIT) unavailable('compiler capture collection exceeds its bound');
    const record = JSON.parse(bytes.toString('utf8'));
    if (record.beforeSnapshot) {
      const key = JSON.stringify(record.beforeSnapshot);
      if (!snapshots.has(key)) {
        const snapshot = boundedRead(record.beforeSnapshot.filename, LIMIT);
        if (hash(snapshot) !== record.beforeSnapshot.sha256) unavailable('pre-compilation package snapshot changed');
        snapshots.set(key, JSON.parse(snapshot.toString('utf8')));
      }
      const before = snapshots.get(key);
      let consumed = [];
      try { consumed = compilerInputs(record).files; } catch { /* Unsupported dep-info remains unproven. */ }
      record.beforeInputs = Object.fromEntries(consumed.filter(filename => Object.hasOwn(before, filename)).map(filename => [filename, before[filename]]));
      delete record.beforeSnapshot;
    }
    records.push(record);
  }
  return records;
}

export function collectCargoContexts({manifest, metadata, policy, target, setupInputs, command, send = () => {}}) {
  if (!policy?.executionAllowed) unavailable('personal compiler-execution grant is required');
  const configurationInputs = guardConfiguration(manifest);
  const wrapper = fileURLToPath(new URL('./capture.mjs', import.meta.url));
  const implementation = [wrapper, fileURLToPath(import.meta.url), fileURLToPath(new URL('./contexts.mjs', import.meta.url))];
  const setupEvidence = fileEvidence([...implementation, ...setupInputs.filter(item => !item.endsWith('.rs')), ...configurationInputs]);
  const fingerprint = hash(JSON.stringify([setupEvidence,
    policy.features, policy.defaultFeatures, target ?? null, process.env.PATH,
    Object.fromEntries(Object.entries(process.env).filter(([name]) => /^(?:CARGO|RUST)/.test(name)))]));
  const state = path.join(policy.cacheDirectory, 'contexts', hash(JSON.stringify([manifest, target ?? null, policy.features, policy.defaultFeatures])));
  fs.mkdirSync(state, {recursive: true});
  const cacheFile = path.join(state, 'units.json'), targetDirectory = path.join(state, 'target');
  let previous = [];
  try {
    const cache = JSON.parse(boundedRead(cacheFile, CACHE_LIMIT).toString('utf8'));
    if (cache.version === 1 && cache.fingerprint === fingerprint && Array.isArray(cache.units)) previous = cache.units;
  } catch { /* Missing/malformed evidence requires actual recapture, never invented proof. */ }
  const diagnostics = [], packages = packageSources(metadata), toolRoots = metadata.packages.flatMap(item => item.targets
    .filter(target => target.kind.some(kind => ['custom-build', 'proc-macro'].includes(kind))).map(target => target.src_path));
  // Prior actual dep-info supplies include/generated inputs for a new saved
  // snapshot, even when their changed bytes invalidate the prior unit proof.
  for (const unit of previous) {
    const files = packages.get(unit?.cargo?.manifest_path);
    if (files && Array.isArray(unit.inputs?.files)) for (const filename of unit.inputs.files)
      if (typeof filename === 'string' && path.isAbsolute(filename) && fs.existsSync(filename)) files.add(filename);
  }
  const scratch = fs.mkdtempSync(path.join(state, 'capture-'));
  let units = [], cargoSucceeded = false, artifacts = [], failed = [], buildScripts = [], droppedDiagnostics = 0, captureAttempts = 0, capturedCount = 0;
  const warn = (code, message) => { if (diagnostics.length < 100) diagnostics.push(warning(code, message)); else droppedDiagnostics++; };
  const queryCache = new Map();
  const query = (program, args, cwd) => {
    const key = JSON.stringify([program, args, cwd]);
    if (!queryCache.has(key)) queryCache.set(key, command(program, args, cwd));
    return queryCache.get(key);
  };
  try {
    for (let attempt = 0; attempt < 2; attempt++) {
      captureAttempts++;
      const directory = path.join(scratch, String(attempt)); fs.mkdirSync(directory);
      const snapshots = snapshotPackages(directory, packages), configuration = path.join(directory, 'configuration.json');
      jsonFile(configuration, {directory, snapshots, toolRoots});
      send('progress', {stage: 'analysis', projectPath: manifest,
        message: attempt ? 'Recapturing unproven Cargo metadata once with discovered saved inputs.' : 'Evaluating actual Cargo library, binary and test compiler contexts.'});
      const args = ['check', '--offline', '--workspace', '--all-targets', '--keep-going', '--jobs', '1', '--message-format=json', '--manifest-path', manifest,
        '--target-dir', targetDirectory, ...cargoSelection(policy)];
      if (target) args.push('--target', target);
      const result = spawnSync('cargo', args, {cwd: path.dirname(manifest), env: {...compilerEnvironment(),
        // Public Cargo control: ask the currently selected compiler for its
        // identity even when compilation artifacts are fresh. This does not
        // disable metadata/incremental reuse or force source compilation.
        CARGO_CACHE_RUSTC_INFO: '0', CARGO_BUILD_BUILD_DIR: path.join(state, 'build'),
        RUSTC_WRAPPER: wrapper, KNOW_CODE_RUST_CAPTURE_CONFIG: configuration},
        encoding: 'utf8', timeout: 120000, maxBuffer: LIMIT});
      if (result.error || result.signal) throw new Error(`Cargo context preparation failed: ${result.error?.message ?? result.signal}`);
      cargoSucceeded = result.status === 0;
      const messages = (result.stdout ?? '').split(/\r?\n/).flatMap(line => { try { return [JSON.parse(line)]; } catch { return []; } });
      artifacts = messages.filter(item => item.reason === 'compiler-artifact');
      buildScripts = messages.filter(item => item.reason === 'build-script-executed').map(item => ({packageId: item.package_id, outDir: item.out_dir}));
      const records = capturedRecords(directory), current = [], recapture = new Set(), unproven = [];
      const selectedCompilers = new Set(records.filter(record => record.status === 0 && !record.error && !record.beforeInputs)
        .map(record => fs.realpathSync(record.compiler)));
      capturedCount += records.filter(record => record.beforeInputs && record.status === 0 && !record.error).length;
      for (const item of messages.filter(item => item.reason === 'compiler-message')) {
        if (diagnostics.length < 100) diagnostics.push({code: item.message?.code?.code ?? 'rustc',
          severity: item.message?.level === 'error' ? 'error' : 'warning', message: String(item.message?.message ?? '').slice(-4096),
          path: item.message?.spans?.find(span => span.is_primary)?.file_name ?? ''});
        else droppedDiagnostics++;
      }
      failed = records.filter(record => record.status !== 0 || record.error);
      for (const artifact of artifacts) {
        // Cargo may copy/alias a build-script executable after rustc emits it.
        // Tooling execution/provenance is retained through Cargo messages; no
        // application semantic context is invented from that alias pathname.
        if (artifact.target.kind.includes('custom-build')) continue;
        const candidates = records.filter(record => record.status === 0 && !record.error && record.artifacts?.some(item =>
          artifact.filenames.includes(path.resolve(record.cwd, item.artifact))));
        let unit;
        try {
          if (candidates.length === 1) unit = capturedUnit(candidates[0], artifact, query, [...setupInputs.filter(item => !item.endsWith('.rs')), ...configurationInputs]);
          else if (candidates.length > 1) unavailable('multiple compiler captures match one Cargo artifact');
          else if (artifact.fresh) {
            const saved = previous.filter(item => capturedUnitCurrent(item, artifact));
            if (saved.length === 1 && selectedCompilers.size === 1 && selectedCompilers.has(fs.realpathSync(saved[0].record.compiler)) &&
                JSON.stringify(compilerCfg(saved[0].record, query)) === JSON.stringify(saved[0].cfg)) unit = saved[0];
          }
          if (!unit) unavailable('no matching successful compiler/input/artifact proof');
          // Store only the consumed subset of the pre-Cargo package snapshot.
          unit.record.beforeInputs = Object.fromEntries(unit.inputs.files.map(filename => [filename, unit.record.beforeInputs[filename]]));
          current.push(unit);
        } catch (error) {
          unproven.push(error.message);
          if (candidates.length === 1) {
            try {
              const consumed = compilerInputs(candidates[0]);
              const files = packages.get(artifact.manifest_path);
              if (files) for (const filename of consumed.files) files.add(filename);
              for (const output of candidates[0].artifacts.filter(item => item.emit === 'metadata')) recapture.add(path.resolve(candidates[0].cwd, output.artifact));
            } catch { /* Unknown dep-info forms remain explicit gaps, not guessed inputs. */ }
          } else if (!artifact.target.kind.some(kind => ['custom-build', 'proc-macro'].includes(kind))) {
            // Cargo check identifies owned non-tool artifacts directly. Their
            // removal is recovery of our cache, never a semantic producer join.
            for (const filename of artifact.filenames) recapture.add(path.resolve(filename));
          }
        }
      }
      units = current;
      if (attempt || !recapture.size) {
        for (const message of unproven) warn('rust_context_unavailable', message);
        break;
      }
      for (const filename of recapture) {
        if (!within(state, filename) || !within(fs.realpathSync(state), fs.realpathSync(filename)) || !fs.lstatSync(filename).isFile()) unavailable('refusing cleanup outside owned compiler cache');
        fs.unlinkSync(filename);
      }
      previous = current;
    }
    if (!cargoSucceeded) warn('rust_compiler_partial', 'Cargo check reported compiler errors; only individually proven successful contexts are available.');
    for (const record of failed) warn('rust_failed_context', record.error ?? 'A compiler unit failed; no old body proof was substituted.');
    const connected = connectCapturedUnits(units), plans = [];
    const currentSetup = fileEvidence(Object.keys(setupEvidence));
    if (JSON.stringify(currentSetup) !== JSON.stringify(setupEvidence)) unavailable('compiler/configuration inputs changed during Cargo preparation');
    for (let index = 0; index < connected.length; index++) if (metadata.workspace_members.includes(connected[index].cargo.package_id) &&
        !connected[index].cargo.target.kind.includes('custom-build')) {
      try { plans.push({unit: connected[index], graph: selectedProject(connected, index, metadata)}); }
      catch (error) { warn('rust_context_unavailable', error.message); }
    }
    try { jsonFile(cacheFile, {version: 1, fingerprint, units}, CACHE_LIMIT); }
    catch (error) { warn('rust_context_cache_limited', error.message); }
    if (droppedDiagnostics) diagnostics.push(warning('rust_context_diagnostics_limited', `${droppedDiagnostics} additional context diagnostics omitted.`));
    return {plans, units: connected, diagnostics, failed, buildScripts, cargoSucceeded, captureAttempts, capturedCount,
      inputPaths: [...new Set(connected.flatMap(unit => Object.keys(unit.inputHashes)))]};
  } finally {
    // Failed cleanup is an error; never report retained capture bytes reclaimed.
    fs.rmSync(scratch, {recursive: true});
  }
}
