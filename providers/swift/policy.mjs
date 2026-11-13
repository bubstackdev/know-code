import fs from 'node:fs';
import path from 'node:path';

// The coordinator resolves checked-in/personal settings and owns trust. This
// adapter accepts only its acknowledged Swift branch, never private env knobs.
export function swiftAnalysisPolicy(request) {
  const policy = request.analysisPolicy;
  if (!policy) return undefined; // Retain the low-level legacy worker contract.
  if (policy.version !== 2 || !policy.swift || policy.node || policy.csharp || policy.rust)
    throw new Error('Update required: Swift worker requires Swift analysis policy 2');
  const settings = policy.swift;
  if (settings.executionAllowed !== true)
    throw new Error('Swift analysis requires a personal repository trust grant; trust this workspace and refresh.');
  const cacheDirectory = settings.cacheDirectory;
  if (typeof cacheDirectory !== 'string' || !path.isAbsolute(cacheDirectory) || cacheDirectory.includes('\0')
      || path.normalize(cacheDirectory) !== cacheDirectory)
    throw new Error('Invalid Swift analysis policy: cache_directory must be an absolute normalized central directory');
  const result = {cacheDirectory, executionAllowed: true};
  if (Object.hasOwn(settings, 'schemePath')) {
    const filename = settings.schemePath;
    if (typeof filename !== 'string' || !path.isAbsolute(filename) || filename.includes('\0')
        || !filename.endsWith('.xcscheme') || path.normalize(filename) !== filename)
      throw new Error('Invalid Swift analysis policy: scheme_path must be an absolute normalized .xcscheme path');
    if (!fs.existsSync(filename) || !fs.statSync(filename).isFile())
      throw new Error(`Selected shared Xcode scheme is unavailable: ${filename}; correct swift.scheme and refresh`);
    return {...result, schemePath: filename};
  }
  return result;
}
