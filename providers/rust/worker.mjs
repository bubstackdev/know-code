import fs from 'node:fs';
import path from 'node:path';
import {pathToFileURL, fileURLToPath} from 'node:url';
import {spawnSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import {readRequest, send} from '../shared/protocol.mjs';
import {Analyzer, compilerEnvironment} from './lsp.mjs';
import {relative,byteOffsets,sourceBoundary} from '../shared/source.mjs';
import {orderedDefinitions,ExternalSources} from './navigation.mjs';
import {syntaxGraph, outgoingCalls, authoredDeclaration} from './graph.mjs';
import {rustPolicy, cargoSelection, analyzerCargo} from './policy.mjs';
import {prepareCargo} from './preparation.mjs';
import {collectCargoContexts} from './cargo-contexts.mjs';
import {fileEvidence} from './contexts.mjs';

const rangeKey = range => [range.start.line, range.start.character, range.end.line, range.end.character].join(':');
const kinds = new Set(['namespace', 'type', 'class', 'enum', 'interface', 'struct', 'typeParameter', 'parameter', 'variable', 'property', 'enumMember', 'function', 'method', 'macro', 'procMacro', 'derive', 'const', 'static', 'constParameter', 'selfKeyword', 'selfTypeKeyword', 'union']);

function command(tool, args, cwd, options) {
  const result = spawnSync(tool, args, {cwd, encoding: 'utf8', timeout: 120000, maxBuffer: 8 * 1024 * 1024, env: compilerEnvironment(options)});
  if (result.error || result.status !== 0) throw new Error(`${tool} prerequisite/project evaluation failed: ${result.error?.message ?? result.stderr}; install the repository-selected Rust toolchain, rust-analyzer and rust-src components, and restore Cargo dependencies separately`);
  return result.stdout.trim();
}

function source(filename, root, compilationId) {
  const utf8 = fs.readFileSync(filename);
  const text = utf8.toString('utf8');
  if (!Buffer.from(text, 'utf8').equals(utf8)) throw new Error(`unsupported non-UTF-8 Rust source: ${filename}`);
  const offsets = byteOffsets(text);
  const lines = [0];
  for (let i=0;i<text.length;i++) if (text.charCodeAt(i)===10) lines.push(i+1);
  const sha256 = createHash('sha256').update(utf8).digest('hex');
  return {filename, text, offsets, lines, uri: pathToFileURL(filename).href,
    artifact: {id: `${relative(root, filename)}@${sha256}`, path: relative(root, filename), utf8, sha256, compilationId, generated: false},
    byte(position) {
      const start = lines[position.line];
      if (start === undefined || position.character < 0) throw new Error('invalid compiler source position');
      const offset = start + position.character;
      const end = position.line + 1 < lines.length ? lines[position.line + 1] : text.length;
      if (offset > end) throw new Error('compiler source position exceeds indexed source');
      return offsets[offset];
    },
    position(byte) {
      let low = 0, high = offsets.length - 1;
      while (low < high) { const middle = (low + high) >>> 1; if (offsets[middle] < byte) low = middle + 1; else high = middle; }
      if (offsets[low] !== byte) throw new Error('compiler byte range splits a UTF-8 character');
      const characterOffset = low;
      low = 0; high = lines.length;
      while (low < high) { const middle = (low + high) >>> 1; if (lines[middle] <= characterOffset) low = middle + 1; else high = middle; }
      return {line: low - 1, character: characterOffset - lines[low - 1]};
    }};
}

function localDependencyInputs(metadata, result) {
  const members = new Set(metadata.workspace_members);
  const pending = metadata.packages.filter(item => item.source == null && !members.has(item.id))
    .map(item => path.dirname(item.manifest_path));
  const visited = new Set();
  // Reuse the adapter/core artifact exclusions; bin is intentionally retained
  // because Rust's src/bin contains source. These paths are watch-only inputs.
  const excluded = new Set(['target', '.git', 'node_modules', '.artifacts', '.research', '.build', '.worktrees', 'DerivedData']);
  let entries = 0, retainedBytes = pending.reduce((bytes, filename) => bytes + Buffer.byteLength(filename), 0);
  if (retainedBytes > 4 * 1024 * 1024) throw new Error('resource-limited: Rust local dependency input paths exceed 4 MiB');
  while (pending.length) {
    const directory = pending.pop();
    if (visited.has(directory)) continue;
    visited.add(directory);
    const handle = fs.opendirSync(directory);
    try {
      for (let entry; (entry = handle.readSync());) {
        if (++entries > 50000) throw new Error('resource-limited: Rust local dependency input discovery exceeds 50000 entries');
        const filename = path.join(directory, entry.name);
        if (entry.isDirectory() && !excluded.has(entry.name) && !entry.name.startsWith('.build-')) {
          retainedBytes += Buffer.byteLength(filename);
          if (retainedBytes > 4 * 1024 * 1024) throw new Error('resource-limited: Rust local dependency input paths exceed 4 MiB');
          pending.push(filename);
        }
        else if (entry.isFile() && entry.name.endsWith('.rs')) {
          result.add(filename);
          retainedBytes += Buffer.byteLength(filename);
          if (retainedBytes > 4 * 1024 * 1024) throw new Error('resource-limited: Rust local dependency input paths exceed 4 MiB');
        }
      }
    } finally { handle.closeSync(); }
  }
}

function inputPaths(metadata, manifest, policy) {
  const result = new Set(metadata.packages.map(p => p.manifest_path));
  // Cargo-selected local crate roots include build scripts and proc macros.
  // Track saved changes without publishing third-party dependency source facts.
  // This is not exhaustive ownership of their module/rerun/include inputs.
  for (const item of metadata.packages) if (item.source == null)
    for (const target of item.targets ?? []) if (typeof target.src_path === 'string') result.add(target.src_path);
  localDependencyInputs(metadata, result);
  result.add(manifest);
  for (const target of policy?.targets ?? []) if (target.endsWith('.json')) result.add(path.resolve(path.dirname(manifest), target));
  result.add(path.join(metadata.workspace_root, 'Cargo.lock'));
  for (let directory = metadata.workspace_root;; directory = path.dirname(directory)) {
    for (const item of ['rust-toolchain', 'rust-toolchain.toml', '.cargo/config', '.cargo/config.toml']) {
      const candidate = path.join(directory, item);
      if (fs.existsSync(candidate)) result.add(candidate);
    }
    if (path.dirname(directory) === directory) break;
  }
  return [...result].filter(p => fs.existsSync(p));
}

function toolsFor(manifest, managed = false) {
  const directory = path.dirname(manifest);
  // Managed Cargo may select build.rustc independently of PATH. Its successful
  // captured compiler supplies the actual sysroot/rust-src prerequisite below.
  if (managed) return {compilerVersion: 'Cargo-selected compiler captured per unit', analyzerVersion: command('rust-analyzer', ['--version'], directory)};
  const compilerVersion = command('rustc', ['-Vv'], directory);
  const sysroot = command('rustc', ['--print', 'sysroot'], directory);
  const librarySources = path.join(sysroot, 'lib/rustlib/src/rust/library');
  if (!fs.existsSync(path.join(librarySources, 'Cargo.toml')) || !fs.existsSync(path.join(librarySources, 'core/src/lib.rs')))
    throw new Error(`missing rust-src prerequisite for repository-selected Rust sysroot ${sysroot}; install rust-src separately for that toolchain and refresh. Know Code does not install toolchains or components`);
  return {compilerVersion, sysroot, analyzerVersion: command('rust-analyzer', ['--version'], directory)};
}

function contextIdentity(manifest, root, plan) {
  const unit = plan.unit;
  const key = JSON.stringify([unit.cargo.package_id, unit.cargo.target, unit.cargo.profile, unit.cargo.features, unit.cfg.target ?? null]);
  return `rust:${relative(root, manifest)}:unit:${createHash('sha256').update(key).digest('hex')}`;
}

function unavailableContext(root, manifest, configuration, target, diagnostics, context = 'unavailable') {
  const projectPath = relative(root, manifest), id = `rust:${projectPath}:unavailable:${encodeURIComponent(target ?? 'repository-default')}`;
  send('compilation', {id, language: 'rust', projectPath, target: target ?? 'repository-default', configuration,
    inferred: false, optionsJson: JSON.stringify({provider: 'cargo-rust-analyzer', rustContextPolicy: 1, context}), inputPaths: [manifest], dependencies: []});
  send('report', {compilationId: id, status: 'partial', diagnostics,
    capabilities: {calls: 'partial', dependencies: 'partial', dispatch: 'partial', project_links: 'partial'}});
}

async function analyze(root, manifest, metadata, configuration, policy, selectedTarget, request, prepared, selected, collection, tools) {
  const directory = path.dirname(manifest);
  const projectPath = relative(root, manifest);
  const compilationId = selected ? contextIdentity(manifest, root, selected) : `rust:${projectPath}${selectedTarget ? `:target:${encodeURIComponent(selectedTarget)}` : ''}`;
  const cargo = analyzerCargo(policy, manifest, selectedTarget);
  tools ??= toolsFor(manifest);
  const compilerVersion = selected?.unit.cfg.compilerVersion ?? tools.compilerVersion;
  const {analyzerVersion} = tools;
  if (policy && !selected) {
    if (!prepared.has(manifest)) prepared.set(manifest, prepareCargo({request, root, manifest, metadata, policy,
      identity: [compilerVersion, analyzerVersion, command('cargo', ['--version'], directory)], command, inputPaths, send}));
    metadata = prepared.get(manifest);
  }
  const trustedPreparation = policy?.executionAllowed === true;
  const members = metadata.packages.filter(p => metadata.workspace_members.includes(p.id));
  const files = new Set();
  // Cargo supplies crate roots. Additional module sources are enrolled only
  // from analyzer-resolved module declarations below, never a recursive scan.
  if (selected) for (const filename of selected.graph.firstPartyFiles) files.add(filename);
  else for (const member of members) for (const target of member.targets ?? [])
      if (typeof target.src_path === 'string' && fs.existsSync(target.src_path)) files.add(target.src_path);
  const sources = new Map([...files].sort().map(filename => [pathToFileURL(filename).href, source(filename, root, compilationId)]));
  if (selected) for (const current of sources.values())
    if (selected.unit.inputHashes[current.filename] !== current.artifact.sha256)
      throw new Error('Rust saved source changed after its compiler-context snapshot; refresh saved inputs');
  const semanticInputs = new Map(selected ? selected.graph.reachable.flatMap(index => Object.entries(collection.units[index].inputHashes)) : []);
  const macroSources = new Set(selected?.graph.macroDylibs.flatMap(item => item.sourceFiles) ?? []);
  const replayMacros = !!selected?.graph.macroDylibs.length && trustedPreparation;
  if (replayMacros) {
    const current = fileEvidence(selected.graph.macroDylibs.map(item => item.path), {artifact: true});
    for (const item of selected.graph.macroDylibs) if (current[item.path] !== item.sha256)
      throw new Error('Captured procedural-macro dylib changed before analyzer replay');
  }
  const generatedInputs = [];
  if (selected) for (const current of sources.values()) for (const output of collection.buildScripts) {
    if (typeof output.outDir !== 'string' || !path.isAbsolute(output.outDir)) continue;
    const relativeOutput = path.relative(output.outDir, current.filename);
    if (!path.isAbsolute(relativeOutput) && relativeOutput !== '..' && !relativeOutput.startsWith(`..${path.sep}`)) {
      current.artifact.generated = true;
      generatedInputs.push({path: current.artifact.path, packageId: output.packageId, outDir: output.outDir});
    }
  }
  const inputFingerprint = selected ? createHash('sha256').update(JSON.stringify([selected.unit.cfg, selected.unit.inputHashes,
    selected.unit.dependencyHashes, selected.unit.outputHashes])).digest('hex') : '';
  const options = {provider: 'rust-analyzer-lsp', analyzerVersion, positionEncoding: 'utf-16', configuration,
    cargo: {...cargo, features: policy?.features ?? 'default', buildJobs: 1}, procMacros: trustedPreparation,
    requestedPolicy: policy ? {targets: policy.targets, defaultFeatures: policy.defaultFeatures, executionAllowed: policy.executionAllowed} : null,
    sourceOwnership: selected ? 'Selected compiler unit dep-info membership; dependency crates are lookup-only' : 'Cargo target roots and unique analyzer-selected module files',
    ...(selected ? {rustContextPolicy: 1, context: {cargoPackageId: selected.unit.cargo.package_id, target: selected.unit.cargo.target,
      profile: selected.unit.cargo.profile, features: selected.unit.cargo.features, cfg: selected.unit.cfg.effective,
      compilerSha256: selected.unit.cfg.compilerSha256, captureAttempts: collection.captureAttempts}, generatedInputs,
      analyzerReplay: {cfg: selected.graph.analyzerCfg, cargoCfgs: selected.graph.cargoCfgs, buildScripts: false,
        procMacros: replayMacros, macroDylibs: selected.graph.macroDylibs.map(item => ({path: item.path, sha256: item.sha256}))}} : {}),
    dependencyInputTracking: 'Cargo-selected local dependency package roots: bounded watch-only .rs superset, excluding artifact directories and symlink traversal',
    coverage: selected ? 'Captured selected-root compiler context; compiler/analyzer graph capabilities remain partial.' :
      'Semantic navigation in one rust-analyzer workspace view; not an exhaustive per-target/per-feature compiler index.'};
  const targetLabel = selected ? `${selected.unit.cargo.target.kind.join('+')}:${selected.unit.cargo.target.name}:${selected.unit.cfg.test ? 'test' : 'normal'}@${selected.unit.cfg.target ?? 'host'}` :
    selectedTarget ? `${selectedTarget}/rust-analyzer-workspace-view` : 'rust-analyzer-workspace-view';
  send('compilation', {id: compilationId, language: 'rust', projectPath, target: targetLabel, configuration, inputFingerprint,
    inferred: false, compilerVersion, optionsJson: JSON.stringify(options), inputPaths: [...new Set([...inputPaths(metadata, manifest, policy), ...(collection?.inputPaths ?? [])])], dependencies: []});
  send('progress', {stage: 'analysis', projectPath, message: trustedPreparation ?
    'Loading repository-selected Rust analyzer with trusted compiler-integrated build-script and procedural-macro preparation; product checks on save are disabled.' :
    'Loading repository-selected Rust analyzer; build scripts and procedural macro execution are disabled.'});
  const diagnostics = [{code: 'rust_coverage_limits', severity: 'warning', path: projectPath,
    message: `Experimental compiler-backed Rust navigation: build scripts/procedural macros are ${trustedPreparation ? 'personally authorized for compiler preparation' : 'disabled'}; generated inputs, cfg/feature/target variants and exhaustive unresolved-reference diagnostics are not fully covered. Only Cargo target roots and unique analyzer-selected module files are retained; include!/expanded sources and context-specific module ownership remain incomplete. Watch-only inputs include evaluated dependency manifests and a bounded Rust-source superset below local dependency package roots, without following symlink directories. Arbitrary build-script rerun files, generated inputs and environment changes are not exhaustively tracked. Only unique analyzer-resolved references are published. This is a partial graph.`}];
  if (selected) {
    diagnostics[0].message = 'Captured compiler unit and saved dep-info inputs select this analyzer graph. Only this unit owns source facts; dependency crates remain lookup-only and cross-unit body links are not yet published. Implicit calls, deferred bodies, trait/dynamic candidates, expansion locations and arbitrary build-script rerun inputs remain partial. Proc-macro replay executes compiler tools again: runtime file/environment reads and nondeterministic expansion are not exhaustively tracked or certified equivalent to the captured compilation.';
  }
  if (configuration !== 'Debug') diagnostics.push({code: 'rust_configuration_unmapped', severity: 'warning', path: projectPath,
    message: `Workspace configuration ${configuration} has no Rust cfg/profile mapping; analysis uses the analyzer default view.`});
  const analyzer = new Analyzer(directory, selected ? {RUSTC: selected.unit.record.compiler} : {});
  const emitted = new Set();
  const externalSources = new ExternalSources();
  const declarationOrigins = new Map();
  const canonicalCallables = new Map();
  let originAnchors = 0, omittedOrigins = 0;
  function rememberOrigins(target, syntax) {
    const key = `${target.uri}@${target.artifact.sha256}`, anchors = syntax?.declarationAnchors() ?? new Map();
    if (anchors.size > 65536) return undefined;
    if (declarationOrigins.has(key)) { originAnchors -= declarationOrigins.get(key).size; declarationOrigins.delete(key); }
    while (declarationOrigins.size && (declarationOrigins.size >= 64 || originAnchors + anchors.size > 65536)) {
      const oldest = declarationOrigins.keys().next().value;
      originAnchors -= declarationOrigins.get(oldest).size; declarationOrigins.delete(oldest);
    }
    declarationOrigins.set(key, anchors); originAnchors += anchors.size;
    return anchors;
  }
  async function authoredTarget(target, start, end, kind) {
    if (!selected) return true; // Historical experimental direct-worker boundary.
    const key = `${target.uri}@${target.artifact.sha256}`;
    let anchors = declarationOrigins.get(key);
    if (!anchors) {
      // Full syntax responses amplify source size. Large lookup-only files
      // remain an explicit identity gap instead of threatening the bounded RPC
      // connection needed to finish this unit's own source facts.
      if (!sources.has(target.uri) && target.artifact.utf8.length > 64 * 1024) return false;
      const syntax = await syntaxGraph(analyzer, target, !sources.has(target.uri) ? {
        onPresentationUnavailable: error => {
          if (diagnostics.length < 100) diagnostics.push({code: 'rust_lookup_syntax_unavailable', severity: 'warning',
            path: relative(root, target.filename), message: `Lookup-only authored-origin proof is unavailable because the analyzer syntax presentation failed (${error.rpcCode}); affected targets were omitted without reusing invocation anchors.`});
        }
      } : {});
      if (!syntax) { rememberOrigins(target, undefined); return false; }
      anchors = rememberOrigins(target, syntax);
      if (!anchors) return false;
    } else { declarationOrigins.delete(key); declarationOrigins.set(key, anchors); }
    return authoredDeclaration(anchors, start, end, kind);
  }
  const callableIdentity = item => JSON.stringify([item.uri, item.kind, item.name, item.selectionRange,
    item.range, item.detail ?? null, item.data ?? null]);
  async function canonicalCallable(definition, target, start, end, fallback) {
    if (!replayMacros) return true;
    const name = target.artifact.utf8.subarray(start, end).toString('utf8').replace(/^r#/, '').normalize('NFC');
    const matches = item => [6, 12].includes(item.kind) && item.uri === definition.uri &&
      rangeKey(item.selectionRange) === rangeKey(definition.range) && item.name?.normalize('NFC') === name;
    const use = fallback.compilerItem ? [fallback.compilerItem] : await analyzer.request('textDocument/prepareCallHierarchy',
      {textDocument: {uri: fallback.uri}, position: fallback.position});
    const candidates = (use ?? []).filter(matches);
    if (candidates.length !== 1) return false;
    const key = `${definition.uri}:${rangeKey(definition.range)}`;
    if (!canonicalCallables.has(key)) {
      const declaration = await analyzer.request('textDocument/prepareCallHierarchy',
        {textDocument: {uri: definition.uri}, position: definition.range.start});
      if (canonicalCallables.size >= 256) canonicalCallables.delete(canonicalCallables.keys().next().value);
      canonicalCallables.set(key, (declaration ?? []).filter(matches));
    }
    const declared = canonicalCallables.get(key);
    return declared.length === 1 && callableIdentity(declared[0]) === callableIdentity(candidates[0]);
  }
  let batch = {compilationId, symbols: [], occurrences: [], relationships: []};
  const flush = () => {
    if (batch.symbols.length || batch.occurrences.length || batch.relationships.length) send('facts', batch);
    batch = {compilationId, symbols: [], occurrences: [], relationships: []};
  };
  let unresolved = 0, graphCalls = 0, graphDependencies = 0, graphUnavailable = 0;
  const relationship = edge => batch.relationships.push(edge);
  const recordLimit = () => { if (batch.symbols.length + batch.occurrences.length + batch.relationships.length >= 128) flush(); };
  async function definitionSymbol(definition, kind, fallback) {
    let target = sources.get(definition.uri) ?? externalSources.get(definition.uri);
    if (!target && definition.uri.startsWith('file:')) {
      const filename = fileURLToPath(definition.uri);
      if (fs.existsSync(filename)) { target = source(filename, root, compilationId); externalSources.set(definition.uri, target); }
    }
    if (!target) { if (selected) omittedOrigins++; return undefined; }
    if (selected && semanticInputs.has(target.filename) && semanticInputs.get(target.filename) !== target.artifact.sha256)
      throw new Error('Rust dependency source changed after its compiler-context snapshot; refresh saved inputs');
    const anchor = target.byte(definition.range.start), endAnchor = target.byte(definition.range.end);
    if (selected && ['function', 'method'].includes(kind) && macroSources.has(target.filename) &&
        !selected.unit.cargo.target.kind.includes('proc-macro')) { omittedOrigins++; return undefined; }
    if (!await authoredTarget(target, anchor, endAnchor, kind)) { omittedOrigins++; return undefined; }
    if (selected && ['function', 'method'].includes(kind) && !await canonicalCallable(definition, target, anchor, endAnchor, fallback))
      { omittedOrigins++; return undefined; }
    const id = `${compilationId}:${relative(root, target.filename)}:${anchor}`;
    if (!emitted.has(id)) {
      const selectedName = target.artifact.utf8.subarray(anchor, endAnchor).toString('utf8');
      const identifierSelection = /^(?:r#)?[_\p{ID_Start}][_\p{ID_Continue}]*$/u.test(selectedName);
      const name = identifierSelection ? selectedName : fallback.name;
      const hover = await analyzer.request('textDocument/hover', {textDocument: {uri: identifierSelection ? definition.uri : fallback.uri},
        position: identifierSelection ? definition.range.start : fallback.position});
      const content = hover?.contents;
      const signature = typeof content === 'string' ? content : Array.isArray(content) ? content.map(c => typeof c === 'string' ? c : c.value).join('\n') : content?.value;
      emitted.add(id);
      batch.symbols.push({id, name, kind, signature: signature ?? name, external: !sources.has(definition.uri)});
      recordLimit();
    }
    return {id, target, anchor, endAnchor};
  }
  try {
    const initialized = await analyzer.request('initialize', {processId: process.pid, rootUri: pathToFileURL(directory).href,
      workspaceFolders: [{uri: pathToFileURL(directory).href, name: path.basename(directory)}],
      capabilities: {general: {positionEncodings: ['utf-16']}, experimental: {serverStatusNotification: true},
        textDocument: {documentSymbol: {hierarchicalDocumentSymbolSupport: true}, definition: {linkSupport: true}, hover: {contentFormat: ['plaintext']},
          ...(replayMacros ? {diagnostic: {dynamicRegistration: false, relatedDocumentSupport: false}} : {}),
          callHierarchy: {}, semanticTokens: {requests: {full: true}, tokenTypes: [...kinds, 'keyword'], tokenModifiers: ['declaration', 'definition'], formats: ['relative']}}},
      initializationOptions: {checkOnSave: false, check: {enable: false},
        cargo: selected ? {offline: true, cfgs: selected.graph.cargoCfgs, buildScripts: {enable: false, useRustcWrapper: false}} : cargo,
        ...(selected ? {linkedProjects: [selected.graph.project], cfg: selected.graph.analyzerCfg} : {}),
        procMacro: {enable: selected ? replayMacros : trustedPreparation, processes: 1,
          ...(replayMacros ? {attributes: {enable: true}, ignored: {}} : {})},
        cachePriming: {enable: false}, numThreads: 1,
        diagnostics: {enable: true, ...(replayMacros ? {disabled: []} : {})},
        hover: {documentation: {enable: false}, memoryLayout: {enable: false}}}});
    if (initialized.capabilities.positionEncoding && initialized.capabilities.positionEncoding !== 'utf-16') throw new Error('unsupported rust-analyzer position encoding');
    const legend = initialized.capabilities.semanticTokensProvider?.legend;
    if (!legend) throw new Error('rust-analyzer lacks semantic token support');
    analyzer.notify('initialized', {});
    const status = await analyzer.ready();
    const pullDiagnostics = initialized.capabilities.diagnosticProvider;
    if (replayMacros && (status.health !== 'ok' || !pullDiagnostics || typeof pullDiagnostics !== 'object' || pullDiagnostics.interFileDependencies !== true)) {
      send('report', {compilationId, status: 'partial', diagnostics: [...diagnostics,
        {code: 'rust_macro_replay_unavailable', severity: 'error', path: projectPath,
          message: status.message ?? 'Macro replay requires a ready analyzer with synchronous pull diagnostics; no macro-disabled binding fallback was published.'}],
        capabilities: {calls: 'partial', dependencies: 'partial', dispatch: 'partial', project_links: 'partial'}});
      return;
    }
    if (status.health !== 'ok') diagnostics.push({code: 'rust_analyzer_load', severity: 'warning', path: projectPath, message: status.message ?? `Analyzer health: ${status.health}`});
    // Discover declared module files before emitting any symbols: source
    // ownership must not depend on whether a caller is analyzed before its
    // callee's module. Outline declarations select request sites; only a unique
    // analyzer definition enrolls another file. No path/name convention does.
    const outlines = new Map();
    const macroDiagnostics = [];
    for (const current of sources.values()) {
      analyzer.notify('textDocument/didOpen', {textDocument: {uri: current.uri, languageId: 'rust', version: 1, text: current.text}});
      const outline = initialized.capabilities.documentSymbolProvider ?
        await analyzer.request('textDocument/documentSymbol', {textDocument: {uri: current.uri}}) : [];
      outlines.set(current.uri, outline);
      if (replayMacros) {
        const actual = await analyzer.request('rust-analyzer/viewFileText', {uri: current.uri});
        if (actual !== current.text && actual !== current.text.replace(/\r\n/g, '\n'))
          throw new Error('Macro diagnostic source differs from its saved compiler-context snapshot');
        const pulled = await analyzer.request('textDocument/diagnostic', {textDocument: {uri: current.uri},
          ...(typeof pullDiagnostics.identifier === 'string' ? {identifier: pullDiagnostics.identifier} : {})});
        if (pulled?.kind !== 'full' || !Array.isArray(pulled.items))
          throw new Error('Macro replay requires a complete synchronous diagnostic report');
        for (const item of pulled.items) if (item.severity === 1 || String(item.code ?? '').includes('macro')) {
          if (macroDiagnostics.length < 100) macroDiagnostics.push({code: String(item.code ?? 'rust_analyzer'),
            severity: 'error', path: relative(root, current.filename), message: String(item.message).slice(-4096)});
        }
      }
      const modules = [];
      const collectModules = items => { for (const item of items ?? []) {
        if (item.kind === 2 && item.selectionRange) modules.push(item.selectionRange.start);
        collectModules(item.children);
      }};
      collectModules(outline);
      for await (const {definitions} of orderedDefinitions(modules,
        position => analyzer.request('textDocument/definition', {textDocument: {uri: current.uri}, position}))) {
        const locations = definitions ? (Array.isArray(definitions) ? definitions : [definitions]) : [];
        const definitionsByLocation = new Map(locations.map(item => {
          const uri = item.targetUri ?? item.uri, range = item.targetSelectionRange ?? item.range;
          return [JSON.stringify([uri, range]), uri];
        }));
        if (definitionsByLocation.size !== 1) continue;
        const uri = [...definitionsByLocation.values()][0];
        if (!selected && uri?.startsWith('file:') && !sources.has(uri)) {
          const filename = fileURLToPath(uri);
          if (fs.existsSync(filename)) sources.set(uri, source(filename, root, compilationId));
        }
      }
      analyzer.notify('textDocument/didClose', {textDocument: {uri: current.uri}});
    }
    if (replayMacros) {
      if (macroDiagnostics.length) {
        send('report', {compilationId, status: 'partial', diagnostics: [...diagnostics,
          {code: 'rust_macro_replay_unavailable', severity: 'error', path: projectPath,
            message: 'Synchronous macro-enabled diagnostics reported errors before publication; no macro-disabled binding fallback was published.'}, ...macroDiagnostics],
          capabilities: {calls: 'partial', dependencies: 'partial', dispatch: 'partial', project_links: 'partial'}});
        return;
      }
    }
    for (const current of sources.values()) {
      const origin = sourceBoundary(root, current.filename);
      if (origin) diagnostics.push(origin);
      send('source', current.artifact);
      analyzer.notify('textDocument/didOpen', {textDocument: {uri: current.uri, languageId: 'rust', version: 1, text: current.text}});
      const graph = await syntaxGraph(analyzer, current);
      if (!graph) graphUnavailable++;
      if (selected && graph) rememberOrigins(current, graph);
      const moduleId = `${compilationId}:module:${relative(root, current.filename)}`;
      if (graph) {
        graph.root.id = moduleId;
        emitted.add(moduleId);
        batch.symbols.push({id: moduleId, name: relative(root, current.filename), kind: 'module', signature: '<Rust source module>', external: false});
        if (current.artifact.utf8.length) batch.occurrences.push({symbolId: moduleId, artifactId: current.artifact.id,
          startByte: 0, endByte: current.artifact.utf8.length, role: 'definition', enclosingSymbolId: ''});
      }
      const initializerIds = new Map();
      const initializer = owner => {
        if (!initializerIds.has(owner.id)) {
          const id = `${compilationId}:initializer:${relative(root, current.filename)}:${owner.start}`;
          initializerIds.set(owner.id, id);
          batch.symbols.push({id, name: `<${owner.kind.toLowerCase()} initialization>`, kind: 'initializer', signature: '<Rust constant/static initialization>', external: false});
          batch.occurrences.push({symbolId: id, artifactId: current.artifact.id, startByte: owner.start, endByte: owner.end, role: 'definition', enclosingSymbolId: owner.id});
          relationship({sourceId: owner.id, targetId: id, kind: 'initializes', resolution: 'compiler_bound', artifactId: current.artifact.id,
            startByte: owner.selection.start, endByte: owner.selection.end});
        }
        return initializerIds.get(owner.id);
      };
      const extents = new Map();
      if (initialized.capabilities.documentSymbolProvider) {
        const outline = outlines.get(current.uri);
        const collect = items => { for (const item of items ?? []) {
          if (item.selectionRange && item.range) {
            const key = rangeKey(item.selectionRange);
            const ranges = extents.get(key) ?? [];
            ranges.push(item.range); extents.set(key, ranges);
          }
          collect(item.children);
        }};
        collect(outline);
      }
      const tokens = await analyzer.request('textDocument/semanticTokens/full', {textDocument: {uri: current.uri}});
      function* positions() {
       let line = 0, character = 0;
       for (let i = 0; i < (tokens?.data.length ?? 0); i += 5) {
        const [deltaLine, deltaCharacter, length, tokenType, modifiers] = tokens.data.slice(i, i + 5);
        line += deltaLine;
        character = deltaLine ? deltaCharacter : character + deltaCharacter;
        const kind = legend.tokenTypes[tokenType];
        if (!kinds.has(kind)) continue;
        const position = {line, character};
        const endPosition = {line, character: character + length};
        // rust-analyzer may highlight a macro's bang as a separate semantic
        // token. The identifier carries the reference; punctuation adds none.
        if (kind === 'macro') {
          const spelling = current.artifact.utf8.subarray(current.byte(position), current.byte(endPosition)).toString('utf8');
          if (spelling === '!') continue;
          if (spelling.endsWith('!')) endPosition.character--;
        }
        yield {kind,position,endPosition};
       }
      }
      for await (const {item:{kind,position,endPosition},definitions} of orderedDefinitions(positions(),
        ({position})=>analyzer.request('textDocument/definition',{textDocument:{uri:current.uri},position}))) {
        const locations = definitions ? (Array.isArray(definitions) ? definitions : [definitions]) : [];
        const unique = new Map(locations.map(d => {
          const uri = d.targetUri ?? d.uri;
          const range = d.targetSelectionRange ?? d.range;
          return [JSON.stringify([uri, range]), {uri, range}];
        }));
        if (unique.size !== 1) { unresolved++; continue; }
        const definition = [...unique.values()][0];
        const startByte = current.byte(position), endByte = current.byte(endPosition);
        const selected = await definitionSymbol(definition, kind, {uri: current.uri, position,
          name: current.artifact.utf8.subarray(startByte, endByte).toString('utf8')});
        if (!selected) { unresolved++; continue; }
        const {id: symbolId, anchor, endAnchor} = selected;
        const declaration = current.uri === definition.uri && startByte === anchor && endByte === endAnchor;
        if (declaration) graph?.bind(startByte, endByte, symbolId);
        const owner = graph?.enclosing(startByte, endByte, declaration ? symbolId : '');
        batch.occurrences.push({symbolId, artifactId: current.artifact.id, startByte, endByte,
          role: declaration ? 'declaration' : 'reference', enclosingSymbolId: owner?.id ?? ''});
        if (declaration && owner && owner.id !== symbolId)
          relationship({sourceId: owner.id, targetId: symbolId, kind: 'contains', resolution: 'compiler_bound',
            artifactId: current.artifact.id, startByte, endByte});
        const dependency = !declaration && owner && graph.dependency(startByte, endByte);
        if (dependency && owner.id !== symbolId) {
          relationship({sourceId: owner.kind === 'CONST' || owner.kind === 'STATIC' ? initializer(owner) : owner.id,
            targetId: symbolId, kind: dependency, resolution: 'compiler_bound', artifactId: current.artifact.id, startByte, endByte});
          graphDependencies++;
        }
        if (declaration) {
          const ranges = extents.get(rangeKey(definition.range));
          if (ranges?.length === 1) {
            const start = current.byte(ranges[0].start), end = current.byte(ranges[0].end);
            if (start <= startByte && end >= endByte)
              batch.occurrences.push({symbolId,artifactId:current.artifact.id,startByte:start,endByte:end,role:'definition',enclosingSymbolId:owner?.id ?? ''});
          }
        }
        recordLimit();
      }
      if (graph && initialized.capabilities.callHierarchyProvider) {
        graphCalls += await outgoingCalls(analyzer, graph, {
          position: byte => current.position(byte), relationship, initializer, recordLimit,
          symbol: async item => {
            if (![6, 12].includes(item.kind)) return undefined;
            const selected = await definitionSymbol({uri: item.uri, range: item.selectionRange}, item.kind === 6 ? 'method' : 'function',
              {name: item.name, uri: item.uri, position: item.selectionRange.start, compilerItem: item});
            return selected && {id: selected.id, method: item.kind === 6};
          }
        });
      } else graphUnavailable++;
      analyzer.notify('textDocument/didClose', {textDocument: {uri: current.uri}});
      flush();
    }
    for (const [uri, items] of analyzer.diagnostics) for (const item of items) {
      if (diagnostics.length >= 100) break;
      diagnostics.push({code: String(item.code ?? 'rust_analyzer'), severity: item.severity === 1 ? 'error' : item.severity === 2 ? 'warning' : 'info',
        path: uri.startsWith('file:') ? relative(root, fileURLToPath(uri)) : '', message: item.message});
    }
    if (unresolved) diagnostics.push({code: 'rust_unresolved_navigation', severity: 'warning', path: projectPath,
      message: `${unresolved} semantic tokens had no unique supported definition; no exact reference was invented.`});
    if (omittedOrigins) diagnostics.push({code: 'rust_unproven_definition_origin', severity: 'warning', path: projectPath,
      message: `${omittedOrigins} targets lacked matching authored declaration ranges/kinds, including possible expansion origins or lookup-only source beyond the 64 KiB syntax-proof allowance; no invocation anchor was reused as a symbol identity.`});
    diagnostics.push({code: 'rust_graph_limits', severity: 'warning', path: projectPath,
      message: `Analyzer-bound graph slice: ${graphCalls} direct/declared-target calls and ${graphDependencies} syntax-classified value/type/import dependencies. Closures/async blocks, implicit operations, trait/dynamic candidates and ${selected ? 'cross-unit body links' : 'independently selected Cargo contexts'} remain partial; ${graphUnavailable} unavailable syntax/call-hierarchy operations.`});
    send('report', {compilationId, status: 'partial', diagnostics,
      capabilities: {calls: 'partial', dependencies: 'partial', dispatch: 'partial', project_links: 'partial'}});
  } finally { await analyzer.close(); }
}

try {
  const request = readRequest();
  const policy = rustPolicy(request);
  const root = path.resolve(request.workspaceRoot);
  send('provider', {language: 'rust', compilerVersion: 'repository-selected rustc and rust-analyzer per workspace', runtimeVersion: process.version, analysisPolicyVersion: policy ? 1 : 0});
  const workspaces = new Map();
  for (const project of request.projects) {
    const manifest = path.resolve(root, project);
    const metadata = JSON.parse(command('cargo', ['metadata', '--offline', '--no-deps', '--format-version', '1', '--manifest-path', manifest, ...cargoSelection(policy)], path.dirname(manifest)));
    const workspaceRelative = path.relative(fs.realpathSync(root), fs.realpathSync(metadata.workspace_root));
    if (workspaceRelative === '..' || workspaceRelative.startsWith(`..${path.sep}`) || path.isAbsolute(workspaceRelative))
      throw new Error(`Cargo workspace ${metadata.workspace_root} lies outside registered root ${root}; register the enclosing Cargo workspace/repository root so compilation ownership and saved updates remain coherent`);
    const canonical = path.join(metadata.workspace_root, 'Cargo.toml');
    workspaces.set(canonical, metadata);
  }
  const prepared = new Map();
  for (const [manifest, discovered] of workspaces) {
    if (!policy) {
      await analyze(root, manifest, discovered, request.configuration || 'Debug', policy, undefined, request, prepared);
      continue;
    }
    const tools = toolsFor(manifest, true);
    const metadata = prepareCargo({request, root, manifest, metadata: discovered, policy,
      identity: [tools.compilerVersion, tools.analyzerVersion, command('cargo', ['--version'], path.dirname(manifest))], command, inputPaths, send});
    for (const target of policy.targets.length ? policy.targets : [undefined]) {
      let collection;
      try { collection = collectCargoContexts({manifest, metadata, policy, target, setupInputs: inputPaths(metadata, manifest, policy), command, send}); }
      catch (error) {
        unavailableContext(root, manifest, request.configuration || 'Debug', target,
          [{code: 'rust_context_unavailable', severity: 'error', path: relative(root, manifest), message: error.message}]);
        continue;
      }
      if (!collection.plans.length || collection.failed.length || collection.diagnostics.length || !collection.cargoSucceeded)
        unavailableContext(root, manifest, request.configuration || 'Debug', target, collection.diagnostics.length ? collection.diagnostics :
          [{code: 'rust_context_unavailable', severity: 'error', path: relative(root, manifest), message: 'No proven first-party compiler context is available.'}],
          collection.plans.length && collection.cargoSucceeded && !collection.failed.length && !collection.diagnostics.some(item => item.code === 'rust_context_unavailable') ?
            'workspace-summary' : 'unavailable');
      for (const selected of collection.plans)
        await analyze(root, manifest, metadata, request.configuration || 'Debug', policy, target, request, prepared, selected, collection, tools);
    }
  }
  if (!workspaces.size) throw new Error('Rust analysis requires a Cargo.toml project; standalone Rust files are not supported by this provider yet');
  send('complete', {success: true});
} catch (e) {
  process.stderr.write(`${e.stack ?? e}\n`);
  process.exitCode = 1;
}
