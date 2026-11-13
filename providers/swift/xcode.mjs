import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {fileURLToPath, pathToFileURL} from 'node:url';
import {spawnSync} from 'node:child_process';
import {send, MAX_FRAME} from '../shared/protocol.mjs';
import {JsonRpcProcess} from '../shared/jsonrpc.mjs';
import {sourceBoundary} from '../shared/source.mjs';
import {relative, hash, run, sourceFile, compileSources, ingestStore} from './index.mjs';
import {extensionRequest} from './swiftpm.mjs';
import {semanticArguments} from './compiler.mjs';
import {unavailableGraphCapabilities} from './graph.mjs';

const bridgeVersion = '0.1.0-know-code-index-only.6';
const recipe = 'index-only.6-notice1';
const bridges = new Map();
const within = (file, directory) => { const p = path.relative(directory, file); return p === '' || (p !== '..' && !p.startsWith(`..${path.sep}`) && !path.isAbsolute(p)); };
const exists = file => { try { return fs.statSync(file).isFile() || fs.statSync(file).isDirectory(); } catch { return false; } };
const problem = (code, message) => Object.assign(new Error(message), {code});
const diagnostic = (code, message, filename = '') => ({code, severity: 'error', message: String(message).slice(-16000), path: filename});
function json(file, maximum = MAX_FRAME) {
  const stat = fs.statSync(file);
  if (!stat.isFile() || stat.size > maximum) throw problem('xcode_bridge_invalid_proof', `Missing/oversized bridge state: ${file}`);
  return JSON.parse(fs.readFileSync(file, 'utf8'));
}
function put(file, value) { fs.writeFileSync(file, JSON.stringify(value)); }
function container(project) { return path.dirname(project); }

// The internal workspace belongs to the containing project, not another job.
export function canonicalXcodeProject(file) {
  if (path.basename(file) === 'contents.xcworkspacedata' && path.basename(path.dirname(file)) === 'project.xcworkspace'
      && path.dirname(path.dirname(file)).endsWith('.xcodeproj')) return path.join(path.dirname(path.dirname(file)), 'project.pbxproj');
  return file;
}

// A repository has one selection, even when an incremental request contains
// only one of several configured containers. The bridge owns XML/PIF semantics.
export function xcodeSelection({root, configuredProjects, schemePath}) {
  const projects = [...new Set(configuredProjects.map(file => canonicalXcodeProject(path.resolve(root, file)))
    .filter(file => ['project.pbxproj', 'contents.xcworkspacedata'].includes(path.basename(file))))];
  const schemes = new Map();
  try {
    for (const project of projects) {
      const shared = path.join(container(project), 'xcshareddata/xcschemes');
      if (!exists(shared)) continue;
      for (const file of fs.readdirSync(shared, {withFileTypes: true})) if (file.isFile() && file.name.endsWith('.xcscheme')) {
        const filename = fs.realpathSync(path.join(shared, file.name));
        if (!schemes.has(filename)) schemes.set(filename, []);
        schemes.get(filename).push(project);
      }
    }
  } catch (error) {
    return {inputs: [...projects, ...schemes.keys()], error: problem('xcode_scheme_inventory_unavailable', `Cannot read the configured shared-scheme inventory: ${error.message}`)};
  }
  const inputs = [...projects, ...schemes.keys()];
  const fail = message => ({inputs, error: problem('xcode_scheme_selection_required', message)});
  const selected = schemePath && fs.realpathSync(schemePath);
  if (selected && !schemes.has(selected)) return fail(`Selected shared scheme is outside the configured Xcode containers: ${schemePath}`);
  if (!selected && schemes.size !== 1) return fail(`Select one shared Xcode scheme for this workspace with swift.scheme; found ${schemes.size}: ${[...schemes.keys()].map(file => relative(root, file)).join(', ')}`);
  const scheme = selected ?? schemes.keys().next().value;
  const owners = schemes.get(scheme);
  const outer = projects.filter(file => path.basename(file) === 'contents.xcworkspacedata');
  const directOuter = owners.filter(file => outer.includes(file));
  if (directOuter.length > 1 || (!directOuter.length && outer.length > 1)) return fail('The selected scheme belongs to multiple possible Xcode workspaces; this association is unsupported. Use an unambiguous configured workspace.');
  const project = directOuter[0] ?? outer[0] ?? owners[0];
  return {project, schemePath: scheme, inputs};
}

export function findXcodeBridge(env = process.env, home = os.homedir()) {
  const candidates = [...new Set([path.join(home, '.local/share/know-code/tools/xcode-bridge', recipe, 'sourcekit-xcode-bsp'),
    ...(env.PATH ?? '').split(path.delimiter).filter(Boolean).map(dir => path.join(dir, 'sourcekit-xcode-bsp'))])];
  let incompatible;
  for (const candidate of candidates) {
    if (!exists(candidate)) continue;
    try {
      fs.accessSync(candidate, fs.constants.X_OK);
      const executable = fs.realpathSync(candidate), root = path.dirname(executable), manifestFile = path.join(root, 'know-code-xcode-bridge.json');
      const manifest = json(manifestFile);
      if (manifest.schema_version !== 1 || manifest.tool !== 'know-code-xcode-bridge' || manifest.recipe_id !== recipe
          || manifest.cli_identity !== bridgeVersion || manifest.upstream_revision !== '726ce706cf18ffb7ed402bb44b2e47c0960076eb'
          || manifest.swift_build_revision !== '7f96ee0e8aa8a35fe84f3e80932eb9e456356d10') throw Error('unrecognized bridge recipe/identity');
      const service = path.join(root, 'SWBBuildServiceBundle');
      const key = JSON.stringify([executable, fs.statSync(manifestFile).mtimeMs, fs.statSync(executable).mtimeMs, fs.statSync(service).mtimeMs]);
      if (bridges.has(key)) return bridges.get(key);
      for (const file of [executable, service]) {
        const stat = fs.statSync(file), expected = manifest.files?.[path.basename(file)];
        if (!stat.isFile() || !expected || stat.size !== expected.bytes || hash(fs.readFileSync(file)) !== expected.sha256) throw Error(`bridge artifact does not match its installed inventory: ${file}`);
      }
      const version = spawnSync(executable, ['--version'], {env, encoding: 'utf8', timeout: 5000, maxBuffer: 65536});
      if (version.error || version.signal || version.status !== 0 || version.stdout.trim() !== bridgeVersion) throw Error(`expected ${bridgeVersion}; got ${version.stdout?.trim() || version.stderr?.trim() || version.error?.message}`);
      const result = {executable, service, version: bridgeVersion, recipe, manifestSha256: hash(fs.readFileSync(manifestFile))};
      bridges.set(key, result); return result;
    } catch (error) { incompatible = `${candidate}: ${error.message}`; }
  }
  throw problem(incompatible ? 'xcode_bridge_incompatible' : 'xcode_bridge_missing',
    `Install the separate Know Code Xcode bridge recipe ${recipe} (${bridgeVersion}), put sourcekit-xcode-bsp on PATH or in ~/.local/share/know-code/tools/xcode-bridge/${recipe}, and refresh. ${incompatible ?? 'No compatible installed bridge was found.'}`);
}

function gap({root, project, toolchain, selection, platform = '', target, error, inputs = []}) {
  const projectPath = relative(root, project), id = `swift:${projectPath}${platform ? `:${platform}${target ? `:${hash(target.id.uri)}` : ''}:unavailable` : ''}`;
  send('compilation', {id, language: 'swift', projectPath, target: target ? `${target.displayName}:${platform}` : platform, compilerVersion: toolchain.version,
    inputPaths: [...new Set([project, ...toolchain.inputPaths, ...(selection?.inputs ?? []), ...inputs])], optionsJson: JSON.stringify({
      swiftExtractionPolicy: 1, origin: 'xcode', supported: false, compilerPath: toolchain.compiler,
      selectedSchemePath: selection?.schemePath ? relative(root, selection.schemePath) : null,
      bspTarget: target?.id?.uri, toolchainSelection: toolchain.selection, requiredPlatforms: ['iphoneos', 'iphonesimulator'], platform,
      preparation: 'unavailable'
    })});
  send('report', {compilationId: id, status: 'partial', capabilities: unavailableGraphCapabilities,
    diagnostics: [diagnostic(error.code ?? 'xcode_analysis_unavailable', error.message, projectPath)]});
}
function validPlan(plan) {
  return plan && plan.outputs.every(exists) && Object.entries(plan.inputs).every(([file, expected]) => {
    try { return hash(fs.readFileSync(file)) === expected; } catch { return false; }
  });
}
async function close(client) {
  client.timeout = Math.min(client.timeout, 5000);
  try { if (!client.failure) { await client.request('build/shutdown', {}); client.notify('build/exit', {}); } }
  catch { /* Preserve the primary planner/preparation diagnostic. */ }
  finally { client.terminate(); }
}
async function openBridge({root, bridge, project, selection, state, scratch, platform, env, onDiagnostic}) {
  const buildRoot = path.join(state, platform), config = path.join(scratch, `${hash(project)}-${platform}`);
  fs.mkdirSync(buildRoot, {recursive: true}); fs.mkdirSync(config, {recursive: true});
  for (const name of ['know-code-selected-scheme.json', 'know-code-selected-closure.json', 'know-code-last-preparation.json', 'know-code-project-export-request.json', 'know-code-project.pif.json', 'know-code-index-only-request.json']) fs.rmSync(path.join(buildRoot, name), {force: true});
  put(path.join(config, 'buildServer.json'), {workspace: container(project), schemePath: selection.schemePath, buildRoot, platform,
    clonedSourcePackagesDirPath: path.join(state, 'clones'), packageCachePath: path.join(state, 'cache'), serviceBundlePath: bridge.service});
  const client = new JsonRpcProcess(bridge.executable, [], {cwd: config, env, timeout: 120000,
    onNotification(method, params) { if (method === 'build/logMessage' && [1, 2].includes(params?.type)) onDiagnostic({...diagnostic('xcode_preparation', String(params.message ?? '')), severity: params.type === 1 ? 'error' : 'warning'}); }});
  try {
    await client.request('build/initialize', {displayName: 'Know Code', version: '0.0.3', bspVersion: '2.2.0',
      rootUri: pathToFileURL(root).href, capabilities: {languageIds: ['swift']}});
    client.notify('build/initialized', {}); await extensionRequest(client, 'workspace/waitForBuildSystemUpdates', {});
    const inventory = await client.request('workspace/buildTargets', {});
    if (!Array.isArray(inventory.targets)) throw Error('Bridge target inventory is unavailable');
    return {client, buildRoot, targets: inventory.targets};
  } catch (error) {
    const metadataFailed = exists(path.join(buildRoot, 'know-code-project-export-request.json')) && !exists(path.join(buildRoot, 'know-code-selected-scheme.json'));
    error.fatal ||= Boolean(client.failure && !metadataFailed);
    await close(client); error.buildRoot = buildRoot; throw error;
  }
}
function contextProof(buildRoot, platform, selection) {
  const proof = json(path.join(buildRoot, 'know-code-selected-scheme.json'));
  const closure = json(path.join(buildRoot, 'know-code-selected-closure.json'));
  if (!within(proof.pifPath, buildRoot) || hash(fs.readFileSync(proof.pifPath)) !== proof.pifSha256
      || closure.pifSha256 !== proof.pifSha256 || closure.loadedPIFPath !== proof.pifPath
      || proof.selection.schemePath !== selection.schemePath || proof.metadataContext.schemePath !== selection.schemePath
      || proof.metadataContext.configuration !== proof.selection.configuration || closure.configuration !== proof.selection.configuration)
    throw problem('xcode_context_proof_invalid', 'Bridge scheme/configuration/PIF evidence does not match this context');
  for (const [file, expected] of Object.entries(proof.selection.inputSha256)) if (hash(fs.readFileSync(file)) !== expected) throw problem('xcode_saved_inputs_changed', `Saved scheme/project changed during planning: ${file}`);
  const request = json(path.join(buildRoot, 'know-code-index-only-request.json'));
  const selected = proof.selection.targets.map(target => target.plannerIdentifier).sort();
  if (request.buildCommand.command !== 'prepareForIndexing' || JSON.stringify(request.configuredTargets.map(target => target.guid).sort()) !== JSON.stringify(selected)
      || JSON.stringify([...closure.selectedRootGUIDs].sort()) !== JSON.stringify(selected)) throw problem('xcode_context_proof_invalid', 'Bridge selected-root request does not match its compiler PIF');
  for (const params of [request.parameters, ...request.configuredTargets.map(target => target.parameters)]) {
    if (params?.action !== 'indexbuild' || params.configurationName !== proof.selection.configuration
        || params.activeRunDestination?.platform !== platform || params.activeRunDestination.sdk !== platform || params.activeRunDestination.targetArchitecture !== 'arm64'
        || params.overrides?.commandLine?.table?.INDEX_DISABLE_SCRIPT_EXECUTION !== 'YES' || params.overrides.commandLine.table.INDEX_FORCE_SCRIPT_EXECUTION !== 'NO')
      throw problem('xcode_script_guard_unavailable', 'Bridge omitted a mandatory per-context/per-target script-disable or platform parameter');
  }
  const pif = json(proof.pifPath, 64 * 1024 * 1024);
  const projects = pif.filter(item => item.type === 'project').map(item => item.contents.path);
  return {proof, closure, projects};
}
function restoration({root, project, selection, state, bridge, toolchain, request, context, previous, key, fingerprint, onDiagnostic}) {
  const planFile = path.join(state, 'dependency-plan.json');
  let plan; try { plan = json(planFile); } catch { /* Explicit preparation will replace absent derived state. */ }
  if (previous?.fingerprint === fingerprint && !request.retryPreparation) {
    if (!previous.succeeded) throw problem('dependency_restore_failed', previous.error);
    if (validPlan(plan)) return {plan, prepared: false};
  }
  // The bridge's existing saved-scheme parser writes this request before
  // invoking metadata, including when missing dependencies prevent planning.
  if (!context) throw problem('xcode_preparation_context_unavailable', 'The bridge did not supply a saved Test configuration for dependency restoration');
  if (context.metadataSchemePath !== selection.schemePath || context.metadataSchemeSha256 !== hash(fs.readFileSync(selection.schemePath))) throw problem('xcode_saved_inputs_changed', 'Saved scheme changed before dependency restoration');
  const fields = context.arguments;
  if (fields[0] !== 'xcodebuild' || fields[1] !== '-dumpPIF') throw problem('xcode_preparation_context_unavailable', 'Unsupported bridge metadata request');
  const configuration = context.metadataConfiguration;
  const env = toolchain.env;
  send('progress', {stage: 'restoring', projectPath: relative(root, project), message: 'Resolving Xcode package dependencies with the saved Test configuration; no product build'});
  let error = '';
  try {
    const args = ['xcodebuild', '-resolvePackageDependencies', path.basename(project) === 'project.pbxproj' ? '-project' : '-workspace', container(project),
      '-scheme', context.metadataSchemeName, '-configuration', configuration, '-derivedDataPath', path.join(state, 'metadata-preparation'),
      '-clonedSourcePackagesDirPath', path.join(state, 'clones'), '-packageCachePath', path.join(state, 'cache'), '-skipPackageUpdates'];
    if ([path.join(container(project), 'project.xcworkspace/xcshareddata/swiftpm/Package.resolved'), path.join(container(project), 'xcshareddata/swiftpm/Package.resolved')].some(exists)) args.push('-onlyUsePackageVersionsFromResolvedFile');
    const result = run('/usr/bin/xcrun', args, path.dirname(container(project)), env);
    if (result.status !== 0) throw Error(`${result.stdout}\n${result.stderr}`);
    const inputs = Object.fromEntries([...new Set([project, ...selection.inputs, ...toolchain.inputPaths])].filter(exists).map(file => [file, hash(fs.readFileSync(file))]));
    plan = {inputs, outputs: [planFile, path.join(state, 'clones'), path.join(state, 'cache')], bridgeManifest: bridge.manifestSha256, configuration};
    put(planFile, plan);
  } catch (cause) { error = String(cause.message).slice(-16000); onDiagnostic(diagnostic('dependency_restore_failed', error, relative(root, project))); }
  send('preparation', {key, fingerprint, succeeded: !error, error, outputs: error ? [] : plan.outputs});
  if (error) throw problem('dependency_restore_failed', error);
  return {plan, prepared: true};
}

export async function analyzeXcode({request, root, project, toolchain, work, scratch, policy, selection}) {
  if (!policy) return gap({root, project, toolchain, selection, error: problem('xcode_analysis_policy_required', 'Xcode analysis requires the current coordinator policy and a separate personal trust grant; update Know Code and refresh. No bridge was started.')});
  if (selection?.error) return gap({root, project, toolchain, selection, error: selection.error});
  if (!selection || selection.project !== project) return gap({root, project, toolchain, selection, error: problem('xcode_scheme_not_selected', 'This configured Xcode container is outside the workspace’s selected shared scheme; no additional scheme was expanded.')});
  let bridge;
  try { bridge = findXcodeBridge(toolchain.env); }
  catch (error) { return gap({root, project, toolchain, selection, error}); }
  if (!toolchain.xcodeVersion) return gap({root, project, toolchain, selection, error: problem('xcode_required', 'Xcode project indexing requires a compatible installed Xcode; Command Line Tools alone cannot provide both iOS SDKs.')});
  if (toolchain.env.SDKROOT) return gap({root, project, toolchain, selection, error: problem('xcode_sdk_override_conflict', 'Unset SDKROOT for Xcode indexing so the selected Xcode can supply separate device and simulator SDKs; no SDK override was silently substituted.')});
  const state = path.join(work, 'xcode', hash(`${project}:${toolchain.identity}`).slice(0, 24));
  for (const dir of [state, path.join(state, 'clones'), path.join(state, 'cache')]) fs.mkdirSync(dir, {recursive: true});
  const key = `xcode:${project}`, fingerprint = hash(`${request.preparationFingerprint}:${toolchain.identity}:${bridge.manifestSha256}:${selection.schemePath}:xcode-preparation-1`);
  let previous = request.preparations.find(item => item.key === key);
  if (previous?.fingerprint === fingerprint && !previous.succeeded && !request.retryPreparation) return gap({root, project, toolchain, selection, error: problem('dependency_restore_failed', previous.error)});
  let plan; try { plan = json(path.join(state, 'dependency-plan.json')); } catch { /* First preparation is explicit below. */ }
  // The fixed bridge's factory sets these same service keys. Supply one
  // coherent child environment to normal preparation and metadata as well.
  const env = {...toolchain.env, TMPDIR: path.join(state, 'tmp'), SWBBUILDSERVICE_PATH: bridge.service,
    UseSynchronousBuildDescriptionSerialization: 'YES'};
  fs.mkdirSync(env.TMPDIR, {recursive: true});
  const gitCount = Number(env.GIT_CONFIG_COUNT ?? '0');
  if (!Number.isSafeInteger(gitCount) || gitCount < 0 || gitCount > 1024) throw problem('xcode_preparation_environment', 'Invalid inherited Git configuration count');
  env.GIT_CONFIG_COUNT = String(gitCount + 2); env[`GIT_CONFIG_KEY_${gitCount}`] = 'core.hooksPath'; env[`GIT_CONFIG_VALUE_${gitCount}`] = '/dev/null';
  env[`GIT_CONFIG_KEY_${gitCount + 1}`] = 'protocol.ext.allow'; env[`GIT_CONFIG_VALUE_${gitCount + 1}`] = 'never';
  let preparationFailed, preparationAttempted = false;
  for (const platform of ['iphoneos', 'iphonesimulator']) {
    if (preparationFailed) { gap({root, project, toolchain, selection, platform, error: preparationFailed}); continue; }
    let client, analyzing = false;
    const diagnostics = []; let dropped = 0;
    const addDiagnostic = item => { if (diagnostics.length < 100) diagnostics.push(item); else dropped++; };
    try {
      let session, initialError;
      try { session = await openBridge({root, bridge, project, selection, state, scratch, platform, env, onDiagnostic: addDiagnostic}); }
      catch (error) { initialError = error; }
      if (!previous || previous.fingerprint !== fingerprint || (request.retryPreparation && !preparationAttempted) || !validPlan(plan)) {
        let attempt; try { attempt = json(path.join(state, platform, 'know-code-project-export-request.json')); } catch { /* Preserve original planner/selection failure. */ }
        if (!attempt) throw initialError ?? problem('xcode_preparation_context_unavailable', 'No exact saved Test configuration was supplied by the bridge');
        if (session) await close(session.client);
        const preparationRequest = {...request, retryPreparation: request.retryPreparation && !preparationAttempted};
        preparationAttempted = true;
        const prepared = restoration({root, project, selection, state, bridge, toolchain: {...toolchain, env}, request: preparationRequest, context: attempt, previous, key, fingerprint, onDiagnostic: addDiagnostic});
        plan = prepared.plan; previous = {key, fingerprint, succeeded: true, error: '', outputs: plan.outputs};
        session = await openBridge({root, bridge, project, selection, state, scratch, platform, env, onDiagnostic: addDiagnostic});
        if (initialError) for (const item of diagnostics) { item.code = 'xcode_recovered_initialization'; item.severity = 'info'; item.message = `Recovered after successful dependency preparation: ${item.message}`; }
      } else if (initialError) throw initialError;
      client = session.client;
      const {buildRoot, targets} = session;
      let evidence;
      try { evidence = contextProof(buildRoot, platform, selection); } catch (error) { error.fatal = true; throw error; }
      const sourcesByTarget = new Map();
      for (const target of targets) {
        const response = await client.request('buildTarget/sources', {targets: [target.id]});
        sourcesByTarget.set(target.id.uri, response.items.flatMap(item => item.sources));
      }
      send('progress', {stage: 'preparing', projectPath: relative(root, project), message: `Preparing selected ${platform} modules; scripts and product builds stay disabled`});
      await extensionRequest(client, 'buildTarget/prepare', {targets: targets.map(target => target.id)});
      const prepared = json(path.join(buildRoot, 'know-code-last-preparation.json'));
      if (!Array.isArray(prepared.requestedTargetURIs) || !Array.isArray(prepared.configuredRootGUIDs) || prepared.pifSha256 !== evidence.proof.pifSha256 || prepared.configurationName !== evidence.proof.selection.configuration || prepared.succeeded !== true
          || JSON.stringify([...prepared.requestedTargetURIs].sort()) !== JSON.stringify(targets.map(target => target.id.uri).sort())
          || JSON.stringify([...prepared.configuredRootGUIDs].sort()) !== JSON.stringify([...evidence.closure.selectedRootGUIDs].sort())
          || prepared.operationState !== 'succeeded' || JSON.stringify(prepared.typedTerminalResults) !== '["ok"]' || !(prepared.typedPreparationCompleteCount > 0) || prepared.droppedDiagnostics)
        throw problem('xcode_module_preparation_failed', 'Xcode module preparation did not supply a complete typed successful terminal result');
      const observedInputs = new Set([project, ...selection.inputs, ...toolchain.inputPaths, ...Object.keys(evidence.proof.selection.inputSha256)]);
      for (const filename of evidence.projects) {
        if (path.basename(filename) === 'Package.swift' && !within(filename, state)) observedInputs.add(filename);
        for (const lock of [path.join(path.dirname(filename), 'Package.resolved'), path.join(filename, 'project.xcworkspace/xcshareddata/swiftpm/Package.resolved')]) if (exists(lock) && !within(lock, state)) observedInputs.add(lock);
      }
      for (const items of sourcesByTarget.values()) for (const item of items) if (item.uri.startsWith('file:')) {
        const file = fileURLToPath(item.uri); if (!within(file, work) && exists(file)) observedInputs.add(file);
      }
      const sdk = run('/usr/bin/xcrun', ['--sdk', platform, '--show-sdk-path'], root, env);
      if (sdk.status !== 0 || !sdk.stdout.trim()) throw problem('xcode_sdk_unavailable', sdk.stderr || `Missing ${platform} SDK`);
      const sdkPath = fs.realpathSync(sdk.stdout.trim());
      let owned = 0;
      for (const target of targets) {
        const refresh = await client.request('buildTarget/sources', {targets: [target.id]});
        const items = refresh.items.flatMap(item => item.sources).filter(item => item.uri.startsWith('file:') && item.uri.endsWith('.swift'));
        const authored = items.filter(item => !item.generated && !within(fileURLToPath(item.uri), work));
        if (!authored.length) continue; // dependency/cache-only targets remain external signatures
        owned++;
        try {
          const targetCompiler = target.data?.toolchain && path.join(fileURLToPath(target.data.toolchain), 'usr/bin/swiftc');
          if (!targetCompiler || fs.realpathSync(targetCompiler) !== fs.realpathSync(toolchain.compiler)) throw problem('xcode_toolchain_mismatch', 'The configured target selected a different compiler; select its matching installed Xcode and refresh.');
          const sources = new Map(), options = [], configurations = new Map(), sourceOrigins = [], targetDiagnostics = [...diagnostics];
          for (const item of items) {
            const file = fileURLToPath(item.uri);
            if (within(file, path.join(state, 'clones'))) continue;
            if (!exists(file)) { targetDiagnostics.push(diagnostic('xcode_generated_input_missing', `Compiler source is unavailable: ${file}`, relative(root, project))); continue; }
            const settings = await extensionRequest(client, 'textDocument/sourceKitOptions', {textDocument: {uri: item.uri}, target: target.id, language: 'swift', purpose: 'index'});
            if (!settings?.compilerArguments?.length) { targetDiagnostics.push(diagnostic('xcode_compiler_options_missing', `No compiler options for ${file}`, relative(root, project))); continue; }
            const args = semanticArguments(settings.compilerArguments), sdkIndex = args.indexOf('-sdk'), targetIndex = args.indexOf('-target'), triple = args[targetIndex + 1];
            if (sdkIndex < 0 || fs.realpathSync(args[sdkIndex + 1]) !== sdkPath || targetIndex < 0 || !triple?.startsWith('arm64-apple-ios') || triple.endsWith('-simulator') !== (platform === 'iphonesimulator')) throw problem('xcode_platform_mismatch', 'Compiler options did not preserve the selected arm64 device/simulator SDK context; no arguments were relabeled.');
            let source;
            try { source = sourceFile(file, root); } catch (error) { throw problem('xcode_source_encoding_unavailable', error.message); }
            source.generated = Boolean(item.generated) || within(file, work);
            if (!source.generated) { const boundary = sourceBoundary(root, file); if (boundary) targetDiagnostics.push(boundary); }
            sources.set(file, source); sourceOrigins.push({artifactId: source.id, generated: source.generated, compilerSource: item.uri});
            const workingDirectory = settings.workingDirectory ?? path.dirname(container(project)), configurationId = hash(JSON.stringify([args, workingDirectory]));
            if (!configurations.has(configurationId)) configurations.set(configurationId, {id: configurationId, args, workingDirectory});
            const selected = configurations.get(configurationId); options.push({file, args: selected.args, workingDirectory, configurationId});
          }
          const projectPath = relative(root, project), id = `swift:xcode:${projectPath}:${hash(JSON.stringify([selection.schemePath, evidence.proof.selection.configuration, platform, sdkPath, toolchain.identity, target.id.uri]))}`;
          const store = path.join(scratch, `index-${hash(id)}`); analyzing = true;
          const compiled = compileSources({compiler: toolchain.compiler, options, store, root, env});
          targetDiagnostics.push(...compiled.diagnostics);
          const facts = ingestStore({id, root, sources, store, library: toolchain.library, scratch, onInputs: indexInputs => {
            for (const file of indexInputs) if (!within(file, work) && !within(file, toolchain.bundle) && !within(file, sdkPath) && exists(file)) observedInputs.add(file);
            send('compilation', {id, language: 'swift', projectPath, target: `${target.displayName}:${platform}`, configuration: evidence.proof.selection.configuration, compilerVersion: toolchain.version,
              inputPaths: [...observedInputs], optionsJson: JSON.stringify({swiftExtractionPolicy: 1, origin: 'xcode_build_server', platform, bspTarget: target.id.uri,
                bridgeRecipe: bridge.recipe, bridgeVersion: bridge.version, selectedSchemePath: relative(root, selection.schemePath), pifSha256: evidence.proof.pifSha256,
                compilerPath: toolchain.compiler, sdkPath, toolchainSelection: toolchain.selection, preparation: 'modules_only', sourceOrigins,
                compilerConfigurations: [...configurations.values()], sourceConfigurations: options.map(option => ({file: option.file, configurationId: option.configurationId}))})});
          }});
          targetDiagnostics.push(...facts.diagnostics);
          if (dropped) targetDiagnostics.push({code: 'diagnostics_truncated', severity: 'info', path: '', message: `${dropped} additional bridge diagnostics omitted.`});
          send('report', {compilationId: id, status: compiled.partial || facts.partial || !sources.size || targetDiagnostics.some(item => item.severity === 'error') ? 'partial' : 'complete', diagnostics: targetDiagnostics.slice(0, 100), capabilities: facts.capabilities});
          analyzing = false;
        } catch (error) {
          if (analyzing || !error.code || error.code === 'xcode_saved_inputs_changed') throw error;
          gap({root, project, toolchain, selection, platform, target, error, inputs: [...observedInputs]});
        }
      }
      if (!owned) throw problem('xcode_no_owned_swift_targets', 'The selected scheme supplied no owned Swift source targets; dependency sources were not indexed as project bodies.');
      for (const [file, expected] of Object.entries(evidence.proof.selection.inputSha256)) if (hash(fs.readFileSync(file)) !== expected) throw problem('xcode_saved_inputs_changed', `Saved project/scheme changed: ${file}`);
      // Only compiler-selected saved inputs enter the shared watcher manifest.
      // Job/module/generated output paths never trigger a save/restore loop.
      const setupInputs = [...new Set([...observedInputs, ...evidence.projects.filter(file => path.basename(file) === 'Package.swift')])].filter(file => /(?:^|\/)(?:Package(?:@swift-[0-9.]+)?\.swift|Package\.resolved|project\.pbxproj|contents\.xcworkspacedata|\.swift-version|\.tool-versions|\.xcode-version)$|\.(?:xcscheme|xcconfig)$/.test(file));
      plan = {inputs: Object.fromEntries(setupInputs.filter(exists).map(file => [file, hash(fs.readFileSync(file))])),
        outputs: [path.join(state, 'dependency-plan.json'), path.join(state, 'clones'), path.join(state, 'cache'), ...evidence.projects.filter(file => within(file, state))], bridgeManifest: bridge.manifestSha256, configuration: evidence.proof.selection.configuration};
      put(path.join(state, 'dependency-plan.json'), plan);
    } catch (error) {
      if (error.code === 'dependency_restore_failed') preparationFailed = error;
      if (analyzing || error.fatal || client?.failure || error.code === 'xcode_saved_inputs_changed') throw error;
      if (!error.code && /interrupted|resource-limited|timed out|oversized|truncated|frame|compiler\/driver failed|Swift tool failed|IndexStore reader failed/i.test(error.message)) throw error;
      gap({root, project, toolchain, selection, platform, error, inputs: plan ? Object.keys(plan.inputs) : []});
    } finally { if (client) await close(client); }
  }
}
