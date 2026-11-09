import path from 'node:path';
import {createHash} from 'node:crypto';
import {declarationSymbols, symbolIdentity, syntheticIdentity, dispatchTypeKeys} from './symbols.mjs';
import {projectLinks} from './project-links.mjs';
import {graphFacts} from './graph.mjs';
const relative = (root, name) => path.relative(root, name).split(path.sep).join('/');

// One linear conversion per source; never slice from byte zero for every token.
function byteOffsets(text) {
  const offsets = new Uint32Array(text.length + 1);
  let bytes = 0;
  for (let i = 0; i < text.length;) {
    const point = text.codePointAt(i);
    offsets[i] = bytes;
    if (point > 0xffff) offsets[i + 1] = bytes;
    bytes += Buffer.byteLength(String.fromCodePoint(point));
    i += point > 0xffff ? 2 : 1;
  }
  offsets[text.length] = bytes;
  return offsets;
}

export function analyze(ts, root, config, inferredFiles, setupDiagnostics = [], inferredDirectory = root, send, epoch = '') {
  const inferred = !config;
  let parsed;
  const projectPath = config ? relative(root, config) : '$inferred';
  const id = `ts:${projectPath}${inferred && inferredDirectory !== root ? ":" + relative(root, inferredDirectory) : ""}`;
  const inputPaths = new Set();
  const configRead = filename => { inputPaths.add(path.resolve(filename)); return ts.sys.readFile(filename); };
  if (config) {
    const read = ts.readConfigFile(config, configRead);
    if (read.error) throw new Error(ts.flattenDiagnosticMessageText(read.error.messageText, '\n'));
    parsed = ts.parseJsonConfigFileContent(read.config, {...ts.sys, readFile: configRead}, path.dirname(config), undefined, config);
  } else {
    parsed = {fileNames: inferredFiles, errors: [], options: {
      target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.NodeNext,
      moduleResolution: ts.ModuleResolutionKind.NodeNext, strict: true, noEmit: true, skipLibCheck: true
    }};
  }
  const host=ts.createCompilerHost(parsed.options);
  host.useSourceOfProjectReferenceRedirect=()=>true;
  const program = ts.createProgram({rootNames: parsed.fileNames, options: parsed.options, projectReferences: parsed.projectReferences,host});
  const links=projectLinks(ts,root,config,parsed,program,epoch);
  send('compilation', {id, language: 'typescript', projectPath, target: '', configuration: '', inferred,
    references:links.references,inputFingerprint:links.fingerprint,
    inputPaths: [...inputPaths], compilerVersion: ts.version, optionsJson: JSON.stringify({...parsed.options, knowCodeSourceReferenceRedirect: !parsed.options.disableSourceOfProjectReferenceRedirect, ...(inferred ? {inferredRoot: relative(root, inferredDirectory)} : {})}),
    dependencies: (parsed.projectReferences ?? []).map(r => relative(root, ts.resolveProjectReferencePath(r)))});
  const checker = program.getTypeChecker();
  const declarationSymbol = declarationSymbols(ts, checker);
  const files = program.getSourceFiles().filter(f => !program.isSourceFileDefaultLibrary(f) && !program.isSourceFileFromExternalLibrary(f) && links.owns(f));
  const indexed = new Set(files.map(f => f.fileName));
  const emitted = new Set();
  let batch = {compilationId: id, symbols: [], occurrences: [], relationships: []};
  const flush = () => {
    if (batch.symbols.length || batch.occurrences.length || batch.relationships.length) send('facts', batch);
    batch = {compilationId: id, symbols: [], occurrences: [], relationships: []};
  };
  function symbolId(symbol) {
    return symbolIdentity(root,id,symbol);
  }
  function emit(symbol) {
    symbol = declarationSymbol(symbol);
    const key = symbolId(symbol);
    if (emitted.has(key)) return key;
    emitted.add(key);
    const declaration = symbol.valueDeclaration ?? symbol.declarations?.[0];
    let signature = symbol.getName();
    if (declaration) signature = checker.typeToString(checker.getTypeOfSymbolAtLocation(symbol, declaration));
    batch.symbols.push({id: key, name: symbol.getName(), kind: ts.SymbolFlags[symbol.flags] ?? String(symbol.flags),
      signature, ...links.definition(symbol), external: !symbol.declarations?.some(d => indexed.has(d.getSourceFile().fileName))});
    return key;
  }
  let graphFile, graphArtifact, graphOffsets;
  const graph=graphFacts(ts,checker,{emit,dispatchKey:dispatchTypeKeys(ts,checker,root),
    synthetic(node,kind,name) {
      const key=syntheticIdentity(root,id,node,kind);
      if(!emitted.has(key)){emitted.add(key);batch.symbols.push({id:key,name,kind,signature:name,...links.synthetic(node,kind),external:!indexed.has(node.getSourceFile().fileName)});}
      return key;
    },
    occurrence(symbolId,node,role) {
      batch.occurrences.push({symbolId,artifactId:graphArtifact,startByte:graphOffsets[node.getStart(graphFile)],endByte:graphOffsets[node.getEnd()],role,enclosingSymbolId:''});
      if(role==='declaration') batch.occurrences.push({symbolId,artifactId:graphArtifact,startByte:graphOffsets[node.getStart(graphFile)],endByte:graphOffsets[node.getEnd()],role:'definition',enclosingSymbolId:''});
    },
    relationship(edge) {batch.relationships.push(edge);if(batch.relationships.length>=128)flush();}
  });
  for (const file of files) {
    const utf8 = Buffer.from(file.text, 'utf8');
    const sha256 = createHash('sha256').update(utf8).digest('hex');
    const artifactId = `${relative(root, file.fileName)}@${sha256}`;
    flush();
    send('source', {id: artifactId, path: relative(root, file.fileName), utf8, sha256, generated: false, compilationId: id});
    const offsets = byteOffsets(file.text);
    graphFile=file;graphArtifact=artifactId;graphOffsets=offsets;
    graph.start(file,artifactId,offsets);
    const visit = (node, parent) => {
      const owner=graph.scope(node,parent);
      graph.operation(node,owner);
      if (ts.isIdentifier(node) || ts.isPrivateIdentifier(node)) {
        const found = checker.getSymbolAtLocation(node);
        if (found) {
          const declaration = found.declarations?.some(d => d.name === node);
          const symbols = [found];
          if (!declaration && (found.flags & ts.SymbolFlags.Alias)) {
            symbols[0] = checker.getAliasedSymbol(found);
          }
          if (ts.isShorthandPropertyAssignment(node.parent)) {
            const value = checker.getShorthandAssignmentValueSymbol(node.parent);
            if (value && value !== found) symbols.push(value);
          }
          for (const symbol of symbols) {
            batch.occurrences.push({symbolId: emit(symbol), artifactId,
              startByte: offsets[node.getStart(file)], endByte: offsets[node.getEnd()],
              role: declaration ? 'declaration' : 'reference', enclosingSymbolId: ''});
            if(declaration) for(const extent of symbol.declarations ?? []) {
              if(extent.name!==node || extent.getSourceFile()!==file) continue;
              batch.occurrences.push({symbolId:emit(symbol),artifactId,startByte:offsets[extent.getStart(file)],endByte:offsets[extent.getEnd()],role:'definition',enclosingSymbolId:''});
            }
            graph.reference(node,symbol,owner,declaration);
          }
          if (batch.occurrences.length >= 128) flush();
        }
      }
      ts.forEachChild(node, child=>visit(child,owner));
    };
    visit(file);
  }
  flush();
  const diagnostics = [...parsed.errors, ...ts.getPreEmitDiagnostics(program)];
  const items = diagnostics.slice(0, 100).map(d => ({code: `TS${d.code}`,
    severity: d.category === ts.DiagnosticCategory.Error ? 'error' : 'warning',
    message: ts.flattenDiagnosticMessageText(d.messageText, '\n'), path: d.file ? relative(root, d.file.fileName) : ''}));
  items.push(...setupDiagnostics);
  if (inferred) items.push({code: 'inferred_configuration', severity: 'info', message: 'Compiler options were inferred; intended build configuration is unknown.', path: ''});
  if (diagnostics.length > 100) items.push({code: 'diagnostics_truncated', severity: 'info', message: `${diagnostics.length} diagnostics; first 100 included.`, path: ''});
  send('report', {compilationId: id, status: diagnostics.some(d => d.category === ts.DiagnosticCategory.Error) || setupDiagnostics.length ? 'partial' : 'complete', diagnostics: items,
    capabilities:{calls:'partial',dependencies:'partial',dispatch:'partial',project_links:links.complete?'complete':'partial'}});
  return {references: (parsed.projectReferences ?? []).map(reference => ts.resolveProjectReferencePath(reference)), files: files.map(f => path.resolve(f.fileName))};
}
