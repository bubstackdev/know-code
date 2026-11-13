import fs from 'node:fs';
import path from 'node:path';
import {pathToFileURL, fileURLToPath} from 'node:url';
import {JsonRpcProcess} from '../shared/jsonrpc.mjs';
import {send} from '../shared/protocol.mjs';
import {checked, hash, relative, sourceFile, compileSources, ingestStore} from './index.mjs';
import {unavailableGraphCapabilities} from './graph.mjs';
import {semanticArguments} from './compiler.mjs';

const packageManifest = file => /^Package(?:@swift-[0-9.]+)?\.swift$/.test(path.basename(file));
export const canonicalManifest = file => packageManifest(file) ? path.join(path.dirname(file), 'Package.swift') : file;
const exists = file => { try { return fs.statSync(file).isFile() || fs.statSync(file).isDirectory(); } catch { return false; } };
const within = (file, directory) => { const rel = path.relative(directory, file); return rel === '' || (!rel.startsWith('../') && rel !== '..' && !path.isAbsolute(rel)); };
function manifests(directory) {
  return fs.readdirSync(directory).filter(packageManifest).map(name => path.join(directory, name));
}
function diagnostic(code, message, filename = '') { return {code, severity: 'error', message: message.slice(-16000), path: filename}; }
function gap(root, project, toolchain, configuration, diagnostics) {
  const {version, compiler, sdk} = toolchain;
  const projectPath = relative(root, project), id = `swift:${projectPath}`;
  send('compilation', {id, language: 'swift', projectPath, configuration, compilerVersion: version,
    inputPaths: [...manifests(path.dirname(project)), ...toolchain.inputPaths], optionsJson: JSON.stringify({swiftExtractionPolicy: 1, origin: 'swiftpm', supported: false, compilerPath: compiler, sdkPath: sdk, toolchainSelection: toolchain.selection})});
  send('report', {compilationId: id, status: 'partial', diagnostics, capabilities: unavailableGraphCapabilities});
}
function packageArguments(directory, state, configuration, sdk) {
  return ['package', '--sdk', sdk, '--package-path', directory, '--scratch-path', path.join(state, 'build'),
    '--cache-path', path.join(state, 'cache'), '--manifest-cache', 'local', '--jobs', '1',
    ...(configuration ? ['--configuration', configuration.toLowerCase()] : [])];
}
function loadManifest(swift, directory, state, configuration, env, sdk) {
  return JSON.parse(checked(swift, [...packageArguments(directory, state, configuration, sdk), 'dump-package'], directory, env));
}
function unsupported(manifest, filename) {
  const plugins = manifest.targets.filter(target => target.type === 'plugin' || target.pluginUsages?.length);
  if (plugins.length) return diagnostic('swiftpm_external_codegen_required',
    `SwiftPM build-tool plugins require external code generation: ${plugins.map(target => target.name).join(', ')}. No plugin was built or executed; restore/provide its required outputs separately.`, filename);
  const macros = manifest.targets.filter(target => target.type === 'macro');
  if (macros.length) return diagnostic('swiftpm_macro_tool_required',
    `These compiler-integrated macro targets need prepared tool executables: ${macros.map(target => target.name).join(', ')}. Automatic macro tool builds are not implemented; no full product/tool build was run.`, filename);
  return null;
}
function dependencyNodes(graph) {
  const nodes = new Map();
  const visit = item => { if (!nodes.has(item.path)) { nodes.set(item.path, item); for (const dep of item.dependencies ?? []) visit(dep); } };
  visit(graph); return [...nodes.values()];
}
function cachedPlanValid(plan) {
  return plan && plan.outputs.every(exists) && Object.entries(plan.inputs).every(([filename, expected]) => {
    try { return hash(fs.readFileSync(filename)) === expected; } catch { return false; }
  });
}
function prepareDependencies({request, root, project, swift, toolchain, state, configuration}) {
  const {env, identity, sdk} = toolchain;
  const directory = path.dirname(project), key = `swiftpm:${project}`;
  const fingerprint = hash(`${request.preparationFingerprint}:${identity}:${configuration}:swiftpm-planner-v2`);
  const previous = request.preparations.find(item => item.key === key);
  const planFile = path.join(state, 'dependency-plan.json');
  let plan;
  try { plan = JSON.parse(fs.readFileSync(planFile, 'utf8')); } catch { /* Rebuild absent/incomplete derived setup state. */ }
  if (previous?.fingerprint === fingerprint && !request.retryPreparation) {
    if (!previous.succeeded) return {error: diagnostic('dependency_restore_failed', previous.error, relative(root, project))};
    if (cachedPlanValid(plan)) return {plan};
  }
  send('progress', {stage: 'restoring', projectPath: relative(root, project), message: 'Resolving SwiftPM dependencies; build-tool plugins and products are not run'});
  let error = '';
  try {
    const args = packageArguments(directory, state, configuration, sdk);
    checked(swift, [...args, 'resolve'], directory, env);
    const graph = JSON.parse(checked(swift, [...args, 'show-dependencies', '--format', 'json'], directory, env));
    const nodes = dependencyNodes(graph);
    const inputs = {};
    for (const node of nodes) for (const file of manifests(node.path)) inputs[file] = hash(fs.readFileSync(file));
    const resolved = path.join(directory, 'Package.resolved');
    if (exists(resolved)) inputs[resolved] = hash(fs.readFileSync(resolved));
    const outputs = [planFile, path.join(state, 'build', 'workspace-state.json'), ...nodes.map(node => node.path)];
    plan = {graph, inputs, outputs};
    fs.writeFileSync(planFile, JSON.stringify(plan));
    if (!cachedPlanValid(plan)) throw new Error('SwiftPM restoration completed but its dependency plan/checkout state is unavailable');
  } catch (cause) { error = cause.message.slice(-16000); }
  send('preparation', {key, fingerprint, succeeded: !error, error, outputs: error ? [] : plan.outputs});
  return error ? {error: diagnostic('dependency_restore_failed', error, relative(root, project))} : {plan};
}
export async function extensionRequest(client, method, params) {
  try { return await client.request(`sourcekit/${method}`, params); }
  catch (error) { if (error.rpcCode !== -32601) throw error; return client.request(method, params); }
}
function verifyToolchain(target, compiler) {
  const uri = target.data?.toolchain;
  if (!uri) throw new Error(`SwiftPM target ${target.displayName} has no compiler toolchain identity; update the toolchain/Know Code`);
  const selected = path.join(fileURLToPath(uri), 'usr', 'bin', 'swiftc');
  if (!exists(selected) || fs.realpathSync(selected) !== fs.realpathSync(compiler))
    throw new Error(`SwiftPM target ${target.displayName} selects ${selected}, but the provider selected ${compiler}; use the matching Swift toolchain and refresh. No compiler substitution was made.`);
}
export async function analyzePackage({request, root, project, toolchain, work, scratch}) {
  const {compiler, version, sdk, env} = toolchain;
  const directory = path.dirname(project);
  const state = path.join(work, 'packages', hash(`${project}:${toolchain.identity}:${request.configuration}`).slice(0, 24));
  fs.mkdirSync(state, {recursive: true});
  const swift = path.join(path.dirname(compiler), 'swift');
  const configuration = request.configuration || 'Debug';
  if (!['debug', 'release'].includes(configuration.toLowerCase())) {
    gap(root, project, toolchain, configuration, [diagnostic('unsupported_configuration', 'SwiftPM configuration must be Debug or Release.', relative(root, project))]); return;
  }
  let rootManifest;
  try { rootManifest = loadManifest(swift, directory, state, configuration, env, sdk); }
  catch (error) { gap(root, project, toolchain, configuration, [diagnostic('swiftpm_manifest_failed', error.message, relative(root, project))]); return; }
  let problem = unsupported(rootManifest, relative(root, project));
  if (problem) { gap(root, project, toolchain, configuration, [problem]); return; }
  const prepared = prepareDependencies({request, root, project, swift, toolchain, state, configuration});
  if (prepared.error) { gap(root, project, toolchain, configuration, [prepared.error]); return; }
  const nodes = dependencyNodes(prepared.plan.graph);
  // Complete manifest preflight precedes creating the BSP server: SwiftPM can
  // execute build-tool plugins while constructing its in-memory build graph.
  for (const node of nodes) {
    try { problem = unsupported(loadManifest(swift, node.path, state, configuration, env, sdk), relative(root, path.join(node.path, 'Package.swift'))); }
    catch (error) { problem = diagnostic('swiftpm_manifest_failed', error.message, relative(root, path.join(node.path, 'Package.swift'))); }
    if (problem) { gap(root, project, toolchain, configuration, [problem]); return; }
  }
  const buildDiagnostics = [];
  let lastProgress = 0;
  let firstReloadSucceeded = false;
  const initialMissingDescription = [];
  let droppedDiagnostics = 0;
  const addDiagnostic = item => { if (buildDiagnostics.length < 100) buildDiagnostics.push(item); else droppedDiagnostics++; };
  const client = new JsonRpcProcess(swift, [...packageArguments(directory, state, configuration, sdk), 'experimental-build-server'], {
    cwd: directory, env, timeout: 120000,
    onNotification(method, params) {
      if (method === 'build/taskFinish') {
        if (params?.taskId?.id === 'package-reloading' && params.status === 1) {
          firstReloadSucceeded = true;
          // The underlying build server can log a missing description before
          // SwiftPM has supplied its first PIF. Typed reload success recovers it.
          for (const item of initialMissingDescription) {
            item.code = 'swiftpm_recovered_initialization'; item.severity = 'info';
            item.message = `SwiftPM successfully reloaded its build graph after an initial missing build description: ${item.message}`;
          }
          initialMissingDescription.length = 0;
        } else if (params?.status === 2 || params?.status === 3)
          addDiagnostic(diagnostic('swiftpm_preparation', `SwiftPM task ${params.taskId?.id ?? ''} ${params.status === 3 ? 'was cancelled' : 'failed'}.`, relative(root, project)));
      }
      if (method !== 'build/logMessage') return;
      const message = String(params?.message ?? '');
      const initialMissing = !firstReloadSucceeded && message === 'Error generating build description: No such file or directory (2)';
      if (params?.type === 1 || /^error[: ]|: error:/i.test(message)) {
        const item = diagnostic('swiftpm_preparation', message, relative(root, project));
        if (initialMissing) initialMissingDescription.push(item);
        addDiagnostic(item);
      } else if (params?.type === 2) addDiagnostic({...diagnostic('swiftpm_preparation', message, relative(root, project)), severity: 'warning'});
      if (Date.now() - lastProgress >= 500) {
        lastProgress = Date.now(); send('progress', {stage: 'preparing', projectPath: relative(root, project), message: message.slice(-512)});
      }
    }
  });
  try {
    const initialized = await client.request('build/initialize', {displayName: 'Know Code', version: '0.0.2',
      bspVersion: '2.2.0', rootUri: pathToFileURL(directory).href, capabilities: {languageIds: ['swift', 'c', 'cpp', 'objective-c', 'objective-cpp']}});
    if (!initialized.data?.sourceKitOptionsProvider || !initialized.data?.prepareProvider)
      throw new Error('incompatible SwiftPM build server: sourceKitOptions and indexing-only prepare are required; update the Swift toolchain/Know Code');
    client.notify('build/initialized', {});
    await extensionRequest(client, 'workspace/waitForBuildSystemUpdates', {});
    const response = await client.request('workspace/buildTargets', {});
    const targets = response.targets.filter(target => !target.tags?.includes('not-buildable'));
    const sourcesByTarget = new Map();
    for (const target of targets) {
      const response = await client.request('buildTarget/sources', {targets: [target.id]});
      sourcesByTarget.set(target.id.uri, response.items.flatMap(item => item.sources));
    }
    const loadDiagnostics = [...buildDiagnostics];
    const ownedTargets = targets.filter(target => (sourcesByTarget.get(target.id.uri) ?? []).some(source => {
      if (!source.uri.startsWith('file:')) return false;
      const file = fileURLToPath(source.uri);
      const owner = nodes.filter(node => within(file, node.path)).sort((a, b) => b.path.length - a.path.length)[0];
      return file.endsWith('.swift') && within(file, root) && !within(file, work) && owner?.path === directory;
    }));
    if (!ownedTargets.length) { gap(root, project, toolchain, configuration,
      [...buildDiagnostics, diagnostic('swiftpm_no_swift_targets', `SwiftPM reported no analyzable Swift targets. ${client.stderr}`, relative(root, project))]); return; }
    const targetProject = target => {
      const file = (sourcesByTarget.get(target.id.uri) ?? []).find(source => source.uri.startsWith('file:') && !within(fileURLToPath(source.uri), state));
      const owner = file && nodes.filter(node => within(fileURLToPath(file.uri), node.path)).sort((a, b) => b.path.length - a.path.length)[0];
      return relative(root, path.join(owner?.path ?? directory, 'Package.swift'));
    };
    for (const target of ownedTargets) {
      verifyToolchain(target, compiler);
      const projectPath = relative(root, project), id = `swift:${projectPath}:${hash(target.id.uri)}`;
      const items = sourcesByTarget.get(target.id.uri);
      const preparationStart = buildDiagnostics.length;
      send('progress', {stage: 'preparing', projectPath, message: `Preparing Swift modules for ${target.displayName}; no product/object build`});
      await extensionRequest(client, 'buildTarget/prepare', {targets: [target.id]});
      const sourceResponse = await client.request('buildTarget/sources', {targets: [target.id]});
      const currentItems = sourceResponse.items.flatMap(item => item.sources);
      const swiftItems = currentItems.filter(source => source.uri.startsWith('file:') && source.uri.endsWith('.swift'));
      const sources = new Map();
      const options = [];
      const configurations = new Map();
      const diagnostics = [...loadDiagnostics, ...buildDiagnostics.slice(preparationStart)];
      for (const item of swiftItems) {
        const file = fileURLToPath(item.uri);
        if (!exists(file)) { diagnostics.push(diagnostic('swiftpm_generated_input_missing', `SwiftPM source is unavailable: ${file}`, relative(root, project))); continue; }
        const settings = await extensionRequest(client, 'textDocument/sourceKitOptions', {textDocument: {uri: item.uri}, target: target.id, language: 'swift', purpose: 'index'});
        if (!settings?.compilerArguments?.length) { diagnostics.push(diagnostic('swiftpm_compiler_options_missing', `SwiftPM provided no compiler options for ${file}`, relative(root, project))); continue; }
        const source = sourceFile(file, root);
        source.generated = Boolean(item.generated) || within(file, state);
        sources.set(file, source);
        const args = semanticArguments(settings.compilerArguments);
        const sdkIndex=args.indexOf('-sdk');
        if (sdkIndex<0 || !args[sdkIndex+1] || fs.realpathSync(args[sdkIndex+1])!==fs.realpathSync(sdk))
          throw new Error(`SwiftPM target ${target.displayName} did not provide the selected SDK ${sdk}; select a matching SDK/toolchain and refresh. No SDK was substituted.`);
        const workingDirectory = settings.workingDirectory ?? directory;
        const key = hash(JSON.stringify([args, workingDirectory]));
        if (!configurations.has(key)) configurations.set(key, {id: key, args, workingDirectory});
        const settingsGroup = configurations.get(key);
        options.push({file, args: settingsGroup.args, workingDirectory, configurationId: key});
      }
      const dependencies = [...new Set((target.dependencies ?? []).map(dependency => targets.find(candidate => candidate.id.uri === dependency.uri)).filter(Boolean).map(targetProject))];
      const inputs = [...new Set([...Object.keys(prepared.plan.inputs).filter(file => !within(file, state)),
        ...[...sources.values()].filter(source => !source.generated).map(source => source.filename),
        ...items.filter(item => item.uri.startsWith('file:')).map(item => fileURLToPath(item.uri)).filter(file => !within(file, state))])];
      const closure = new Set();
      const includeDependencies = current => {
        if (closure.has(current.id.uri)) return;
        closure.add(current.id.uri);
        for (const dependency of current.dependencies ?? []) {
          const next = targets.find(candidate => candidate.id.uri === dependency.uri);
          if (next) includeDependencies(next);
        }
      };
      includeDependencies(target);
      for (const uri of closure) for (const item of sourcesByTarget.get(uri) ?? []) {
        if (!item.uri.startsWith('file:')) continue;
        const file = fileURLToPath(item.uri);
        if (!within(file, work) && exists(file)) inputs.push(file);
      }
      for (const option of options) for (const argument of option.args) {
        if (!argument.startsWith('-fmodule-map-file=')) continue;
        const file = path.resolve(option.workingDirectory, argument.slice('-fmodule-map-file='.length));
        if (!within(file, work) && exists(file)) inputs.push(file);
      }
      const effectiveTarget = options[0]?.args[options[0].args.indexOf('-target') + 1] ?? target.displayName;
      const store = path.join(scratch, `index-${hash(id)}`);
      let partial = diagnostics.some(item => item.severity === 'error');
      const compilerInputs = new Set([...inputs, ...toolchain.inputPaths]);
      const toolchainRoot = path.resolve(path.dirname(compiler), '..', '..');
      const compiled = compileSources({compiler, options, store, root, env});
      partial ||= compiled.partial;
      for (const item of compiled.diagnostics) if (diagnostics.length < 100 && !diagnostics.some(existing => JSON.stringify(existing) === JSON.stringify(item))) diagnostics.push(item);
      const library = path.resolve(path.dirname(compiler), '..', 'lib', 'libIndexStore.dylib');
      const facts = ingestStore({id, root, sources, store, library, scratch, onInputs: indexInputs => {
        for (const file of indexInputs)
          if (!within(file, work) && !within(file, toolchainRoot) && !within(file, fs.realpathSync(sdk)) && exists(file)) compilerInputs.add(file);
        send('compilation', {id, language: 'swift', projectPath, target: target.displayName, configuration, compilerVersion: version,
        dependencies, inputPaths: [...compilerInputs], optionsJson: JSON.stringify({swiftExtractionPolicy: 1, origin: 'swiftpm_build_server',
          bspVersion: initialized.bspVersion, buildServerVersion: initialized.version, bspTarget: target.id.uri,
          compilerPath: compiler, sdkPath: sdk, toolchainSelection: toolchain.selection, effectiveTarget, signatures: 'compiler_index_names_only',
          indexIncludeLocals: true, jobs: 1, preparation: 'modules_only', compilerConfigurations: [...configurations.values()],
          sourceConfigurations: options.map(option => ({file: option.file, configurationId: option.configurationId}))})});
      }});
      diagnostics.push(...facts.diagnostics);
      if (droppedDiagnostics) diagnostics.push({code: 'diagnostics_truncated', severity: 'info', path: '', message: `${droppedDiagnostics} additional SwiftPM preparation diagnostics were omitted.`});
      if (!sources.size) diagnostics.push(diagnostic('swiftpm_no_source_inputs', 'No Swift source could be analyzed for this target.', projectPath));
      send('report', {compilationId: id, status: partial || facts.partial || !sources.size ? 'partial' : 'complete', diagnostics, capabilities: facts.capabilities});
    }
  } finally {
    try { if (!client.failure) { await client.request('build/shutdown', {}); client.notify('build/exit', {}); } }
    finally { client.terminate(); }
  }
}

// Xcode inspects the compiler-selected package manifests with the same tools
// and conservative plugin/macro policy before module preparation.
export {loadManifest, unsupported as unsupportedManifest};
