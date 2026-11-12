import fs from 'node:fs';
import path from 'node:path';

// The coordinator resolves checked-in/personal settings and owns trust. This
// adapter accepts only its acknowledged Swift branch, never private env knobs.
export function swiftAnalysisPolicy(request) {
  const policy = request.analysisPolicy;
  if (!policy) return undefined; // Retain the low-level legacy worker contract.
  if (policy.version !== 1 || !policy.swift || policy.node || policy.csharp || policy.rust)
    throw new Error('Update required: Swift worker requires Swift analysis policy 1');
  const settings = policy.swift;
  if (Object.hasOwn(settings, 'schemePath')) {
    const filename = settings.schemePath;
    if (typeof filename !== 'string' || !path.isAbsolute(filename) || filename.includes('\0')
        || !filename.endsWith('.xcscheme') || path.normalize(filename) !== filename)
      throw new Error('Invalid Swift analysis policy: scheme_path must be an absolute normalized .xcscheme path');
    if (!fs.existsSync(filename) || !fs.statSync(filename).isFile())
      throw new Error(`Selected shared Xcode scheme is unavailable: ${filename}; correct swift.scheme and refresh`);
    return {schemePath: filename};
  }
  return {};
}
