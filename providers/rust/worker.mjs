import fs from 'node:fs';
import path from 'node:path';
import {pathToFileURL, fileURLToPath} from 'node:url';
import {spawnSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import {readRequest, send} from '../shared/protocol.mjs';
import {Analyzer, compilerEnvironment} from './lsp.mjs';
import {relative,byteOffsets,sourceBoundary} from '../shared/source.mjs';
import {orderedDefinitions,ExternalSources} from './navigation.mjs';
import {syntaxGraph, outgoingCalls} from './graph.mjs';

const rangeKey = range => [range.start.line, range.start.character, range.end.line, range.end.character].join(':');
const kinds = new Set(['namespace', 'type', 'class', 'enum', 'interface', 'struct', 'typeParameter', 'parameter', 'variable', 'property', 'enumMember', 'function', 'method', 'macro', 'constParameter', 'selfKeyword', 'selfTypeKeyword', 'union']);

function command(tool, args, cwd) {
  const result = spawnSync(tool, args, {cwd, encoding: 'utf8', timeout: 120000, maxBuffer: 8 * 1024 * 1024, env: compilerEnvironment()});
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

function inputPaths(metadata, manifest) {
  const result = new Set(metadata.packages.filter(p => metadata.workspace_members.includes(p.id)).map(p => p.manifest_path));
  result.add(manifest);
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

async function analyze(root, manifest, metadata, configuration) {
  const directory = path.dirname(manifest);
  const projectPath = relative(root, manifest);
  const compilationId = `rust:${projectPath}`;
  const compilerVersion = command('rustc', ['-Vv'], directory);
  const sysroot = command('rustc', ['--print', 'sysroot'], directory);
  const librarySources = path.join(sysroot, 'lib/rustlib/src/rust/library');
  if (!fs.existsSync(path.join(librarySources, 'Cargo.toml')) || !fs.existsSync(path.join(librarySources, 'core/src/lib.rs')))
    throw new Error(`missing rust-src prerequisite for repository-selected Rust sysroot ${sysroot}; install rust-src separately for that toolchain and refresh. Know Code does not install toolchains or components`);
  const analyzerVersion = command('rust-analyzer', ['--version'], directory);
  const members = metadata.packages.filter(p => metadata.workspace_members.includes(p.id));
  const files = new Set();
  // Cargo supplies crate roots. Additional module sources are enrolled only
  // from analyzer-resolved module declarations below, never a recursive scan.
  for (const member of members) for (const target of member.targets ?? [])
    if (typeof target.src_path === 'string' && fs.existsSync(target.src_path)) files.add(target.src_path);
  const sources = new Map([...files].sort().map(filename => [pathToFileURL(filename).href, source(filename, root, compilationId)]));
  const options = {provider: 'rust-analyzer-lsp', analyzerVersion, positionEncoding: 'utf-16', configuration,
    cargo: {offline: true, allTargets: true, buildScripts: false, features: 'default'}, procMacros: false,
    sourceOwnership: 'Cargo target roots and unique analyzer-selected module files',
    coverage: 'Semantic navigation in one rust-analyzer workspace view; not an exhaustive per-target/per-feature compiler index.'};
  send('compilation', {id: compilationId, language: 'rust', projectPath, target: 'rust-analyzer-workspace-view', configuration,
    inferred: false, compilerVersion, optionsJson: JSON.stringify(options), inputPaths: inputPaths(metadata, manifest), dependencies: []});
  send('progress', {stage: 'analysis', projectPath, message: 'Loading repository-selected Rust analyzer; build scripts and procedural macro execution are disabled.'});
  const diagnostics = [{code: 'rust_coverage_limits', severity: 'warning', path: projectPath,
    message: 'Experimental compiler-backed Rust navigation: build scripts/procedural macros are disabled; generated inputs, cfg/feature/target variants and exhaustive unresolved-reference diagnostics are not covered. Only Cargo target roots and unique analyzer-selected module files are retained; include!/expanded sources and context-specific module ownership remain incomplete. Only unique analyzer-resolved references are published. This is a partial graph.'}];
  if (configuration !== 'Debug') diagnostics.push({code: 'rust_configuration_unmapped', severity: 'warning', path: projectPath,
    message: `Workspace configuration ${configuration} has no Rust cfg/profile mapping; analysis uses the analyzer default view.`});
  const analyzer = new Analyzer(directory);
  const emitted = new Set();
  const externalSources = new ExternalSources();
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
    if (!target) return undefined;
    const anchor = target.byte(definition.range.start), endAnchor = target.byte(definition.range.end);
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
          callHierarchy: {}, semanticTokens: {requests: {full: true}, tokenTypes: [...kinds, 'keyword'], tokenModifiers: ['declaration', 'definition'], formats: ['relative']}}},
      initializationOptions: {checkOnSave: false, check: {enable: false}, cargo: {offline: true, allTargets: true, buildScripts: {enable: false}},
        procMacro: {enable: false}, cachePriming: {enable: false}, numThreads: 1, diagnostics: {enable: true},
        hover: {documentation: {enable: false}, memoryLayout: {enable: false}}}});
    if (initialized.capabilities.positionEncoding && initialized.capabilities.positionEncoding !== 'utf-16') throw new Error('unsupported rust-analyzer position encoding');
    const legend = initialized.capabilities.semanticTokensProvider?.legend;
    if (!legend) throw new Error('rust-analyzer lacks semantic token support');
    analyzer.notify('initialized', {});
    const status = await analyzer.ready();
    if (status.health !== 'ok') diagnostics.push({code: 'rust_analyzer_load', severity: 'warning', path: projectPath, message: status.message ?? `Analyzer health: ${status.health}`});
    // Discover declared module files before emitting any symbols: source
    // ownership must not depend on whether a caller is analyzed before its
    // callee's module. Outline declarations select request sites; only a unique
    // analyzer definition enrolls another file. No path/name convention does.
    const outlines = new Map();
    for (const current of sources.values()) {
      analyzer.notify('textDocument/didOpen', {textDocument: {uri: current.uri, languageId: 'rust', version: 1, text: current.text}});
      const outline = initialized.capabilities.documentSymbolProvider ?
        await analyzer.request('textDocument/documentSymbol', {textDocument: {uri: current.uri}}) : [];
      outlines.set(current.uri, outline);
      const modules = [];
      const collectModules = items => { for (const item of items ?? []) {
        if (item.kind === 2 && item.selectionRange) modules.push(item.selectionRange.start);
        collectModules(item.children);
      }};
      collectModules(outline);
      for await (const {definitions} of orderedDefinitions(modules,
        position => analyzer.request('textDocument/definition', {textDocument: {uri: current.uri}, position}))) {
        const locations = definitions ? (Array.isArray(definitions) ? definitions : [definitions]) : [];
        const selected = new Map(locations.map(item => {
          const uri = item.targetUri ?? item.uri, range = item.targetSelectionRange ?? item.range;
          return [JSON.stringify([uri, range]), uri];
        }));
        if (selected.size !== 1) continue;
        const uri = [...selected.values()][0];
        if (uri?.startsWith('file:') && !sources.has(uri)) {
          const filename = fileURLToPath(uri);
          if (fs.existsSync(filename)) sources.set(uri, source(filename, root, compilationId));
        }
      }
      analyzer.notify('textDocument/didClose', {textDocument: {uri: current.uri}});
    }
    for (const current of sources.values()) {
      const origin = sourceBoundary(root, current.filename);
      if (origin) diagnostics.push(origin);
      send('source', current.artifact);
      analyzer.notify('textDocument/didOpen', {textDocument: {uri: current.uri, languageId: 'rust', version: 1, text: current.text}});
      const graph = await syntaxGraph(analyzer, current);
      if (!graph) graphUnavailable++;
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
              {name: item.name, uri: item.uri, position: item.selectionRange.start});
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
    diagnostics.push({code: 'rust_graph_limits', severity: 'warning', path: projectPath,
      message: `Analyzer-bound graph slice: ${graphCalls} direct/declared-target calls and ${graphDependencies} syntax-classified value/type/import dependencies. Closures/async blocks, implicit operations, trait/dynamic candidates and independently selected Cargo contexts remain partial; ${graphUnavailable} unavailable syntax/call-hierarchy operations.`});
    send('report', {compilationId, status: 'partial', diagnostics,
      capabilities: {calls: 'partial', dependencies: 'partial', dispatch: 'partial', project_links: 'partial'}});
  } finally { await analyzer.close(); }
}

try {
  const request = readRequest();
  const root = path.resolve(request.workspaceRoot);
  send('provider', {language: 'rust', compilerVersion: 'repository-selected rustc and rust-analyzer per workspace', runtimeVersion: process.version});
  const workspaces = new Map();
  for (const project of request.projects) {
    const manifest = path.resolve(root, project);
    const metadata = JSON.parse(command('cargo', ['metadata', '--offline', '--no-deps', '--format-version', '1', '--manifest-path', manifest], path.dirname(manifest)));
    const workspaceRelative = path.relative(fs.realpathSync(root), fs.realpathSync(metadata.workspace_root));
    if (workspaceRelative === '..' || workspaceRelative.startsWith(`..${path.sep}`) || path.isAbsolute(workspaceRelative))
      throw new Error(`Cargo workspace ${metadata.workspace_root} lies outside registered root ${root}; register the enclosing Cargo workspace/repository root so compilation ownership and saved updates remain coherent`);
    const canonical = path.join(metadata.workspace_root, 'Cargo.toml');
    workspaces.set(canonical, metadata);
  }
  for (const [manifest, metadata] of workspaces) await analyze(root, manifest, metadata, request.configuration || 'Debug');
  if (!workspaces.size) throw new Error('Rust analysis requires a Cargo.toml project; standalone Rust files are not supported by this provider yet');
  send('complete', {success: true});
} catch (e) {
  process.stderr.write(`${e.stack ?? e}\n`);
  process.exitCode = 1;
}
