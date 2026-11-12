import path from 'node:path';
import {createHash} from 'node:crypto';

// The coordinator resolves inheritance and derives execution permission from
// the existing personal trust grant. The adapter validates only this boundary.
export function rustPolicy(request) {
  const policy = request.analysisPolicy;
  if (!policy) return undefined;
  const rust = policy.rust;
  if (policy.version !== 1 || !rust || policy.node || policy.csharp || policy.swift)
    throw new Error('Update required: Rust worker requires Rust analysis policy 1');
  for (const field of ['targets', 'features']) {
    const values = rust[field] ?? [];
    if (!Array.isArray(values) || values.some(value => typeof value !== 'string' || !value.trim() || value.includes('\0') || value.startsWith('-')))
      throw new Error(`Update required: Rust policy ${field} must be nonempty selectors, not command options`);
  }
  if (rust.defaultFeatures != null && typeof rust.defaultFeatures !== 'boolean' ||
      rust.executionAllowed != null && typeof rust.executionAllowed !== 'boolean' ||
      typeof rust.cacheDirectory !== 'string' || !path.isAbsolute(rust.cacheDirectory) || rust.cacheDirectory.includes('\0'))
    throw new Error('Update required: Rust policy needs boolean selections and an absolute compiler cache directory');
  return {targets: [...new Set(rust.targets ?? [])], features: [...new Set(rust.features ?? [])],
    defaultFeatures: rust.defaultFeatures ?? true, executionAllowed: rust.executionAllowed ?? false,
    cacheDirectory: rust.cacheDirectory};
}

export function cargoSelection(policy, target) {
  const args = [];
  if (target) args.push('--filter-platform', target);
  if (policy?.defaultFeatures === false) args.push('--no-default-features');
  if (policy?.features.length) args.push('--features', policy.features.join(','));
  return args;
}

export function analyzerCargo(policy, manifest, target) {
  const cargo = {offline: true, allTargets: true, buildScripts: {enable: false}};
  if (!policy) return cargo; // Preserve the legacy direct-worker defaults.
  cargo.buildScripts.enable = policy.executionAllowed;
  cargo.features = policy.features;
  cargo.noDefaultFeatures = !policy.defaultFeatures;
  if (target) cargo.target = target; // Otherwise Cargo keeps repository defaults.
  const key = createHash('sha256').update(JSON.stringify([manifest, target ?? null, policy.features, policy.defaultFeatures])).digest('hex');
  cargo.targetDir = path.join(policy.cacheDirectory, key.slice(0, 24), 'target');
  return cargo;
}
