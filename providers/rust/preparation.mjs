import fs from 'node:fs';
import path from 'node:path';
import {createHash} from 'node:crypto';
import {cargoSelection} from './policy.mjs';

const hash = value => createHash('sha256').update(value).digest('hex');
const exists = filename => { try { return fs.statSync(filename).isFile(); } catch { return false; } };
// Derived cache/configuration bounds, not new user settings. Reads use the
// opened file's size and one extra byte so growth cannot allocate without bound.
const SETUP_FILE_BYTES = 8 * 1024 * 1024;
const PLAN_BYTES = 8 * 1024 * 1024;
export function boundedRead(filename, limit) {
  const descriptor = fs.openSync(filename, 'r');
  try {
    const stat = fs.fstatSync(descriptor);
    if (!stat.isFile() || stat.size > limit) throw new Error(`resource-limited: Rust setup input exceeds ${limit} bytes: ${filename}`);
    const buffer = Buffer.alloc(stat.size + 1);
    let length = 0;
    while (length < buffer.length) {
      const count = fs.readSync(descriptor, buffer, length, buffer.length - length, null);
      if (!count) break;
      length += count;
    }
    if (length !== stat.size) throw new Error(`Rust setup input changed while reading: ${filename}`);
    return buffer.subarray(0, length);
  } finally { fs.closeSync(descriptor); }
}
function readPlan(filename, fingerprint) {
  try {
    const plan = JSON.parse(boundedRead(filename, PLAN_BYTES).toString('utf8'));
    if (plan?.fingerprint !== fingerprint || !Array.isArray(plan.outputs) || !plan.outputs.includes(filename) ||
        !plan.outputs.every(item => typeof item === 'string' && path.isAbsolute(item)) ||
        !plan.inputs || typeof plan.inputs !== 'object' || Array.isArray(plan.inputs) ||
        !Object.entries(plan.inputs).every(([item, digest]) => path.isAbsolute(item) && typeof digest === 'string' && /^[a-f0-9]{64}$/.test(digest)) ||
        !plan.metadata || !Array.isArray(plan.metadata.packages) || !Array.isArray(plan.metadata.workspace_members) ||
        !path.isAbsolute(plan.metadata.workspace_root ?? '') ||
        !plan.metadata.packages.every(item => item && typeof item.id === 'string' && path.isAbsolute(item.manifest_path ?? '') &&
          (item.source == null || typeof item.source === 'string') && Array.isArray(item.targets) &&
          item.targets.every(target => target && path.isAbsolute(target.src_path ?? '')))) return;
    return plan;
  } catch { /* Derived cache loss, malformed shape or oversize recreates setup. */ }
}

// Preparation checkpoint ownership, retries and publication stay in the shared
// coordinator. Cargo owns resolution, registry/git credentials and its cache.
export function prepareCargo({request, root, manifest, metadata, policy, identity, command, inputPaths, send}) {
  if (!policy) return metadata; // Legacy direct workers remain offline.
  const directory = path.dirname(manifest), key = `cargo:${manifest}`;
  const fingerprint = hash(JSON.stringify([request.preparationFingerprint, identity, policy, 'cargo-fetch-v1']));
  const previous = request.preparations.find(item => item.key === key);
  const state = path.join(policy.cacheDirectory, hash(manifest).slice(0, 24));
  const planFile = path.join(state, 'dependency-plan.json');
  let plan = readPlan(planFile, fingerprint), valid = false;
  try {
    valid = !!plan && plan.outputs.every(exists) &&
      Object.entries(plan.inputs).every(([filename, expected]) => exists(filename) && hash(boundedRead(filename, SETUP_FILE_BYTES)) === expected);
  } catch { /* Changed/missing/oversize setup inputs are evaluated afresh below. */ }
  if (previous?.fingerprint === fingerprint && !request.retryPreparation) {
    if (!previous.succeeded) throw new Error(`Cargo dependency preparation previously failed; refresh explicitly to retry: ${previous.error}`);
    if (valid) return plan.metadata;
  }
  let error = '';
  try {
    // Probe the full resolved graph, not only --no-deps: local dependency
    // manifests and restored external packages are setup outputs too.
    const args = ['metadata', '--offline', '--format-version', '1', '--manifest-path', manifest, ...cargoSelection(policy)];
    let resolved;
    try { resolved = JSON.parse(command('cargo', args, directory)); }
    catch (cause) {
      if (!policy.executionAllowed) throw new Error(`Cargo dependencies are unavailable; a personal repository trust grant is required before restoration: ${cause.message}`);
      send('progress', {stage: 'restoring', projectPath: path.relative(root, manifest),
        message: 'Fetching repository-selected Cargo dependencies; no build scripts, procedural macros or products are run by fetch.'});
      const fetch = ['fetch', '--manifest-path', manifest];
      if (exists(path.join(metadata.workspace_root, 'Cargo.lock'))) fetch.push('--locked');
      for (const target of policy.targets) fetch.push('--target', target);
      // Fetch has no feature flags. The subsequent metadata query evaluates
      // the requested features. Preserve any user-selected offline environment.
      command('cargo', fetch, directory, {restore: true});
      resolved = JSON.parse(command('cargo', args, directory));
    }
    if (fs.realpathSync(resolved.workspace_root) !== fs.realpathSync(metadata.workspace_root))
      throw new Error('Cargo workspace ownership changed during dependency preparation; refresh saved inputs');
    const inputs = {};
    for (const filename of new Set([...inputPaths(resolved, manifest, policy), ...resolved.packages.map(item => item.manifest_path)]))
      // Source saves schedule analyzer preparation through compilation inputs,
      // without turning every source edit into dependency restoration setup.
      if (!filename.endsWith('.rs') && exists(filename)) inputs[filename] = hash(boundedRead(filename, SETUP_FILE_BYTES));
    const outputs = [planFile, ...resolved.packages.map(item => item.manifest_path)];
    plan = {fingerprint, inputs, outputs, metadata: resolved};
    fs.mkdirSync(state, {recursive: true});
    const encoded = JSON.stringify(plan);
    if (Buffer.byteLength(encoded) > PLAN_BYTES) throw new Error(`resource-limited: Cargo dependency plan exceeds ${PLAN_BYTES} bytes`);
    fs.writeFileSync(planFile, encoded);
  } catch (cause) { error = cause.message.slice(-4096); }
  send('preparation', {key, fingerprint, succeeded: !error, error, outputs: error ? [] : plan.outputs});
  if (error) throw new Error(`Cargo dependency preparation failed: ${error}`);
  return plan.metadata;
}
