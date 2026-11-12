import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import {readRequest, send} from '../shared/protocol.mjs';

import {relative, hash, run, sourceFile, compilerDiagnostics, ingestStore} from './index.mjs';
import {canonicalManifest, analyzePackage} from './swiftpm.mjs';
import {createToolchainResolver} from './toolchain.mjs';
import {canonicalXcodeProject, analyzeXcode} from './xcode.mjs';
function within(file, directory) {
  const rel = path.relative(directory, file);
  return rel === '' || (!rel.startsWith(`..${path.sep}`) && rel !== '..' && !path.isAbsolute(rel));
}
function boundary(project) {
  if (/^Package(?:@swift-[0-9.]+)?\.swift$/.test(path.basename(project))) return path.dirname(project);
  return path.dirname(path.dirname(project));
}
function analyzeGroup(request, root, projectPath, files, toolchain, scratch) {
  const {compiler, version, sdk, library, env} = toolchain;
  const id = `swift:${projectPath}`;
  const sources = new Map(files.map(file => [file, sourceFile(file, root)]));
  const module = `KnowCode_${hash(projectPath).slice(0, 16)}`;
  const store = path.join(scratch, hash(projectPath).slice(0, 16));
  const args = ['-frontend', '-typecheck', '-sdk', sdk, '-module-name', module, '-index-store-path', store,
    '-index-include-locals', '-module-cache-path', path.join(path.dirname(scratch), `modules-${hash(toolchain.identity).slice(0, 16)}`)];
  send('compilation', {id, language: 'swift', projectPath, target: '', configuration: request.configuration,
    inferred: true, compilerVersion: version, inputPaths: [...files, ...toolchain.inputPaths], optionsJson: JSON.stringify({
      origin: 'inferred_directory', moduleName: module, compilerPath: compiler, sdkPath: sdk, toolchainSelection: toolchain.selection,
      indexIncludeLocals: true, signatures: 'compiler_index_names_only', sourceScope: 'saved_standalone_files',
      packageSupport: false, xcodeSupport: false
    })});
  send('progress', {stage: 'analysis', projectPath, message: 'Type-checking saved Swift sources and reading compiler IndexStore records'});
  const diagnostics = [];
  let failed = false;
  // The driver emits identical unit names for multi-file typecheck jobs without
  // object output. Explicit primary invocations give every file its own unit.
  // This also bounds compiler concurrency to one process, never one per source.
  for (const file of files) {
    const compile = run(compiler, [...args, '-primary-file', file, ...files.filter(other => other !== file),
      '-index-unit-output-path', path.join(store, `${hash(file)}.o`)], root, env);
    const items = compilerDiagnostics(compile.stderr, root);
    if (/Stack dump:|Please submit a bug report|signal [0-9]+|LLVM ERROR|Assertion failed/i.test(compile.stderr)
        || (compile.status !== 0 && !items.some(d => d.severity === 'error' && d.path)))
      throw new Error(`Swift compiler/driver failed without a usable semantic analysis: ${compile.stderr.slice(0, 16000)}`);
    failed ||= compile.status !== 0;
    for (const item of items) if (diagnostics.length < 100 && !diagnostics.some(existing => JSON.stringify(existing) === JSON.stringify(item))) diagnostics.push(item);
  }
  diagnostics.push({code: 'inferred_configuration', severity: 'info', path: '',
    message: 'Swift module/build options are inferred per directory; SwiftPM/Xcode configuration is not analyzed.'});
  const facts = ingestStore({id, root, sources, store, library, scratch});
  diagnostics.push(...facts.diagnostics);
  send('report', {compilationId: id, status: failed || facts.partial ? 'partial' : 'complete', diagnostics});
}
try {
  const request = readRequest();
  const root = path.resolve(request.workspaceRoot);
  const resolveToolchain = createToolchainResolver({workspaceRoot:root});
  send('provider', {language: 'swift', compilerVersion: 'selected per compilation', runtimeVersion: process.version});
  const canonicalProject = file => canonicalXcodeProject(canonicalManifest(path.resolve(root, file)));
  const projects = [...new Set(request.projects.filter(file => !file.startsWith('$swift-inferred:')).map(canonicalProject))];
  const configured = [...new Set(request.configuredProjects.filter(file => /^Package(?:@swift-[0-9.]+)?\.swift$/.test(path.basename(file)) || ['project.pbxproj', 'contents.xcworkspacedata'].includes(path.basename(file))).map(canonicalProject))];
  const groups = new Map();
  for (const source of request.looseSources.map(file => path.resolve(root, file))) {
    if (configured.some(project => within(source, boundary(project)))) continue;
    const group = `$swift-inferred:${relative(root, path.dirname(source)) || '.'}`;
    const selected = request.projects.filter(project => project.startsWith('$swift-inferred:'));
    if (selected.length && !selected.includes(group)) continue;
    if (!groups.has(group)) groups.set(group, []);
    groups.get(group).push(source);
  }
  const data = process.env.KNOW_CODE_DATA_DIR || path.join(os.homedir(), 'Library', 'Application Support', 'Know Code');
  const work = path.join(data, 'workers', 'swift');
  fs.mkdirSync(work, {recursive: true});
  const scratch = fs.mkdtempSync(path.join(work, 'job-'));
  try {
    for (const project of projects) {
      const toolchain = resolveToolchain(path.dirname(project));
      if (path.basename(project) === 'Package.swift') {
        await analyzePackage({request, root, project, toolchain, work, scratch});
        continue;
      }
      await analyzeXcode({request, root, project, toolchain, work, scratch});
    }
    for (const [group, files] of [...groups].sort(([a], [b]) => a.localeCompare(b)))
      analyzeGroup(request, root, group, files.sort(), resolveToolchain(path.dirname(files[0])), scratch);
  } finally { fs.rmSync(scratch, {recursive: true, force: true}); }
  send('complete', {success: true});
} catch (error) {
  process.stderr.write(`${error.stack ?? error}\n`);
  try { send('complete', {success: false}); } catch { /* Failed before a valid request. */ }
  process.exitCode = 1;
}
