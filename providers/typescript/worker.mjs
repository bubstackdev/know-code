import fs from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';
import {readRequest, send} from '../shared/protocol.mjs';
import {preparePackages, nodePreparationPolicy} from './preparation.mjs';
import {typescriptInstallation} from './package-tools.mjs';
import {analyze} from './analysis.mjs';
import {nativeCompiler,NATIVE_TYPESCRIPT_VERSIONS} from './native.mjs';
import {classificationPolicy,compilationClassification} from './test-classification.mjs';
import {ProjectSetupError,boundedText} from './project-errors.mjs';

const ownRequire = createRequire(import.meta.url);

const nativeCompilers = new Map();
async function compilerFor(directory, root, declared) {
  directory=path.resolve(directory);root=path.resolve(root);
  const inside=directory===root||directory.startsWith(root+path.sep);
  const installation=typescriptInstallation(directory,root);
  if(installation?.error)throw installation.error;
  let filename=installation?.filename;
  if(!filename) {
    if(!inside)
      throw new ProjectSetupError('compiler_context_unavailable',`Referenced TypeScript project ${directory} has no local compiler installation; ambient compilers outside the selected workspace were not substituted`);
    if(declared)
      throw new ProjectSetupError('compiler_unavailable',`Missing repository TypeScript dependency in ${directory}; restore its packages; no compiler substitution was made`);
    filename=ownRequire.resolve('typescript');
  }
  let ts;
  try {ts=ownRequire(filename);}
  catch(error) {throw new ProjectSetupError('typescript_load_failed',`Cannot load selected TypeScript: ${error.message}`);}
  if(NATIVE_TYPESCRIPT_VERSIONS.has(ts.version)) {
    const packageRoot=installation?fs.realpathSync(installation.entry):path.dirname(ownRequire.resolve('typescript/package.json'));
    if(!nativeCompilers.has(packageRoot))nativeCompilers.set(packageRoot,nativeCompiler(packageRoot,ts.version));
    return nativeCompilers.get(packageRoot);
  }
  if (typeof ts.version!=='string'||!(ts.version.startsWith('5.9.')||ts.version.startsWith('6.0.')))
    throw new ProjectSetupError('unsupported_compiler',`unsupported TypeScript ${ts.version??'unknown'}: this provider supports 5.9.x, 6.0.x and the explicitly selected native 7.0.2 frontend; no compiler substitution was made`,ts.version??'unavailable');
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
  const compiler = directory => compilerFor(directory,root,setupDiagnostics.typescriptDeclared(directory));
  const compilationSend = directory => (kind, value) => {
    const setup = kind === 'compilation' && setupDiagnostics.metadata(directory);
    if (setup) value = {...value, inputPaths: [...new Set([...value.inputPaths, ...setup.inputPaths])],
      optionsJson: JSON.stringify({...JSON.parse(value.optionsJson), knowCodePackageManager: setup.packageManager})};
    send(kind, value);
  };
  const owned = new Set(),failed=new Set(),blocked=new Set();
  function unavailable(config,error) {
    if(!(error instanceof ProjectSetupError))throw error;
    config=path.resolve(config);if(failed.has(config))return;
    failed.add(config);blocked.add(path.dirname(config));
    const projectPath=path.relative(root,config).split(path.sep).join('/'),id=`ts:${projectPath}`;
    compilationSend(path.dirname(config))('compilation',{id,language:'typescript',projectPath,compilerVersion:error.compilerVersion,
      inputPaths:[config],optionsJson:JSON.stringify({knowCodeSetupFailure:error.code}),
      ...(testPolicy?{testClassification:compilationClassification(testPolicy)}:{})});
    send('report',{compilationId:id,status:'unavailable',diagnostics:[{code:error.code,severity:'error',path:boundedText(projectPath),
      message:boundedText(`${error.message}; this explicit project's facts are unavailable; standalone inference below its directory is suppressed while ownership is unknown.`)}],
      capabilities:{calls:'unavailable',dependencies:'unavailable',dispatch:'unavailable',project_links:'unavailable'}});
  }
  const configurations = [...request.configuredProjects];
  const planned = new Set();
  while (configurations.length) {
    const config = path.resolve(configurations.shift());
    if (planned.has(config)) continue;
    planned.add(config);
    try {
      const setupFailure=setupDiagnostics.failure(path.dirname(config));
      if(setupFailure)throw setupFailure;
      const ts = await compiler(path.dirname(config));
      if(ts.native) {for(const filename of ts.plan(config).fileNames)owned.add(path.resolve(filename));continue;}
      const read = ts.readConfigFile(config, ts.sys.readFile);
      if (read.error) throw new ProjectSetupError('invalid_project_configuration',ts.flattenDiagnosticMessageText(read.error.messageText,'\n'),ts.version);
      const parsed = ts.parseJsonConfigFileContent(read.config, ts.sys, path.dirname(config), undefined, config);
      for (const filename of parsed.fileNames) owned.add(path.resolve(filename));
      configurations.push(...(parsed.projectReferences ?? []).map(r => ts.resolveProjectReferencePath(r)));
    } catch(error) {unavailable(config,error);}
  }
  const queue = [...request.projects];
  const visited = new Set();
  while (queue.length) {
    const config = path.resolve(queue.shift());
    if (visited.has(config)||failed.has(config)) continue;
    visited.add(config);
    send('progress', {stage: 'analyzing', projectPath: config, message: 'Analyzing configured TypeScript project'});
    try {
      const setupFailure=setupDiagnostics.failure(path.dirname(config));
      if(setupFailure)throw setupFailure;
      const ts=await compiler(path.dirname(config));
      const args=[root,config,[],setupDiagnostics(path.dirname(config)),root,compilationSend(path.dirname(config)),request.preparationFingerprint,testPolicy];
      const result=ts.native?ts.analyze(...args):analyze(ts,...args);
      queue.push(...result.references);
      for (const file of result.files) owned.add(file);
    } catch(error) {unavailable(config,error);}
    // No compiler/checker/source graph is retained after the compilation.
    globalThis.gc();
  }
  const groups = new Map();
  for (const filename of request.looseSources) {
    const saved=path.resolve(filename);
    if (owned.has(saved)||[...blocked].some(directory=>saved===directory||saved.startsWith(directory+path.sep))) continue;
    const savedDirectory=path.dirname(saved);
    const directory=setupDiagnostics.failure(savedDirectory)?savedDirectory:setupDiagnostics.directory(savedDirectory);
    if (!groups.has(directory)) groups.set(directory, []);
    groups.get(directory).push(filename);
  }
  for (const [directory, files] of groups) {
    send('progress', {stage: 'analyzing', projectPath: directory, message: 'Analyzing saved TypeScript files outside configured projects with inferred options'});
    try {
      const setupFailure=setupDiagnostics.failure(directory);
      if(setupFailure)throw setupFailure;
      const ts=await compiler(directory);
      const args=[root,null,files,setupDiagnostics(directory),directory,compilationSend(directory),request.preparationFingerprint,testPolicy];
      if(ts.native)ts.analyze(...args);else analyze(ts,...args);
    }
    catch(error) {
      if(!(error instanceof ProjectSetupError))throw error;
      // Preserve the inferred identity rather than creating an explicit config.
      const projectPath='$inferred',id=`ts:${projectPath}${directory!==root?':'+path.relative(root,directory).split(path.sep).join('/'):''}`;
      compilationSend(directory)('compilation',{id,language:'typescript',projectPath,inferred:true,compilerVersion:error.compilerVersion,
        optionsJson:JSON.stringify({inferredRoot:path.relative(root,directory),knowCodeSetupFailure:error.code}),
        ...(testPolicy?{testClassification:compilationClassification(testPolicy)}:{})});
      send('report',{compilationId:id,status:'unavailable',diagnostics:[{code:error.code,severity:'error',message:boundedText(error.message),path:''}],
        capabilities:{calls:'unavailable',dependencies:'unavailable',dispatch:'unavailable',project_links:'unavailable'}});
    }
    globalThis.gc();
  }
  send('complete', {success: true});
} catch (error) {
  console.error(`TypeScript analysis failed: ${error.stack ?? error}`);
  process.exitCode = 1;
}
