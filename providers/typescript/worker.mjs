import fs from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';
import {readRequest, send} from '../shared/protocol.mjs';
import {preparePackages, context, nodePreparationPolicy} from './preparation.mjs';
import {analyze} from './analysis.mjs';
import {classificationPolicy} from './test-classification.mjs';

const ownRequire = createRequire(import.meta.url);

function compilerFor(directory) {
  const require = createRequire(path.join(directory, '__know_code__.cjs'));
  let filename;
  try { filename = require.resolve('typescript'); }
  catch {
    // An unconfigured standalone project can use the provider's pinned compiler.
    for (let current = directory;; current = path.dirname(current)) {
      const manifest = path.join(current, 'package.json');
      if (fs.existsSync(manifest)) {
        const pkg = JSON.parse(fs.readFileSync(manifest, 'utf8'));
        if (pkg.dependencies?.typescript || pkg.devDependencies?.typescript)
          throw new Error(`missing repository TypeScript dependency in ${directory}; restore its packages`);
      }
      if (path.dirname(current) === current) break;
    }
    filename = ownRequire.resolve('typescript');
  }
  const ts = ownRequire(filename);
  if (!ts.version.startsWith('5.9.')) throw new Error(`unsupported TypeScript ${ts.version}: this first slice supports 5.9.x; use a compatible Know Code provider`);
  return ts;
}

try {
  const request = readRequest();
  const testPolicy = classificationPolicy(request.testClassificationPolicy);
  if (typeof globalThis.gc !== 'function')
    throw new Error('Update required: the TypeScript worker needs node --expose-gc from its launcher; rebuild and restart the Know Code coordinator');
  const root = request.workspaceRoot;
  nodePreparationPolicy(request);
  send('provider', {language: 'typescript', compilerVersion: 'selected per compilation', runtimeVersion: process.version,
    analysisPolicyVersion: 1, testClassificationPolicy: testPolicy});
  const directories = [...request.configuredProjects.map(c => path.dirname(c)), ...request.looseSources.map(f => path.dirname(f))];
  const setupDiagnostics = await preparePackages(root, directories, request, send);
  const compilationSend = directory => (kind, value) => {
    const setup = kind === 'compilation' && setupDiagnostics.metadata(directory);
    if (setup) value = {...value, inputPaths: [...new Set([...value.inputPaths, ...setup.inputPaths])],
      optionsJson: JSON.stringify({...JSON.parse(value.optionsJson), knowCodePackageManager: setup.packageManager})};
    send(kind, value);
  };
  const owned = new Set();
  const configurations = [...request.configuredProjects];
  const planned = new Set();
  while (configurations.length) {
    const config = path.resolve(configurations.shift());
    if (planned.has(config)) continue;
    planned.add(config);
    const ts = compilerFor(path.dirname(config));
    const read = ts.readConfigFile(config, ts.sys.readFile);
    if (read.error) throw new Error(ts.flattenDiagnosticMessageText(read.error.messageText, '\n'));
    const parsed = ts.parseJsonConfigFileContent(read.config, ts.sys, path.dirname(config), undefined, config);
    for (const filename of parsed.fileNames) owned.add(path.resolve(filename));
    configurations.push(...(parsed.projectReferences ?? []).map(r => ts.resolveProjectReferencePath(r)));
  }
  const queue = [...request.projects];
  const visited = new Set();
  while (queue.length) {
    const config = path.resolve(queue.shift());
    if (visited.has(config)) continue;
    visited.add(config);
    send('progress', {stage: 'analyzing', projectPath: config, message: 'Analyzing configured TypeScript project'});
    const result = analyze(compilerFor(path.dirname(config)), root, config, [], setupDiagnostics(path.dirname(config)), root, compilationSend(path.dirname(config)), request.preparationFingerprint, testPolicy);
    // analyze() has returned: no compiler/checker/source graph is retained here.
    globalThis.gc();
    queue.push(...result.references);
    for (const file of result.files) owned.add(file);
  }
  const groups = new Map();
  for (const filename of request.looseSources) {
    if (owned.has(path.resolve(filename))) continue;
    const directory = context(root, path.dirname(filename))?.directory ?? root;
    if (!groups.has(directory)) groups.set(directory, []);
    groups.get(directory).push(filename);
  }
  for (const [directory, files] of groups) {
    send('progress', {stage: 'analyzing', projectPath: directory, message: 'Analyzing saved TypeScript files outside configured projects with inferred options'});
    analyze(compilerFor(directory), root, null, files, setupDiagnostics(directory), directory, compilationSend(directory), request.preparationFingerprint, testPolicy);
    globalThis.gc();
  }
  send('complete', {success: true});
} catch (error) {
  console.error(`TypeScript analysis failed: ${error.stack ?? error}`);
  process.exitCode = 1;
}
