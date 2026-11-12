// The exact native compiler remains an analysis frontend. Storage and graph
// traversal stay in Rust; graph.mjs owns the shared TS call/dependency forms.
import fs from 'node:fs';
import path from 'node:path';
import {createRequire} from 'node:module';
import {pathToFileURL} from 'node:url';
import {spawnSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import {canSend,MAX_FRAME} from '../shared/protocol.mjs';
import {relative,byteOffsets,sourceBoundary} from '../shared/source.mjs';
import {ProjectSetupError,boundedText} from './project-errors.mjs';
import {classificationPolicy,compilationClassification,sourceClassification} from './test-classification.mjs';
import {graphFacts} from './graph.mjs';
import {symbolIdentity,syntheticIdentity} from './symbols.mjs';

export const NATIVE_TYPESCRIPT_VERSIONS = new Set(['7.0.2']);

export async function nativeCompiler(packageRoot,version) {
  if (!NATIVE_TYPESCRIPT_VERSIONS.has(version))
    throw new ProjectSetupError('unsupported_compiler',`Native TypeScript ${version} has not been validated; this frontend selects only 7.0.2 and never substitutes a legacy compiler.`,version);
  const require=createRequire(path.join(packageRoot,'package.json'));
  const platformPackage=`@typescript/typescript-${process.platform}-${process.arch}`;
  let api,ast,native,nativeManifest;
  try {
    nativeManifest=require.resolve(`${platformPackage}/package.json`);
    const pkg=JSON.parse(fs.readFileSync(nativeManifest,'utf8'));
    if(pkg.name!==platformPackage||pkg.version!==version)throw new Error(`Expected ${platformPackage}@${version}; found ${pkg.name}@${pkg.version}`);
    native=path.join(path.dirname(nativeManifest),'lib',process.platform==='win32'?'tsc.exe':'tsc');
    if(!fs.statSync(native).isFile())throw new Error(`Missing selected native executable ${native}`);
    // This isolated analysis worker must never enable upstream timing collection
    // inherited from a developer shell. It does not change the embedding host.
    delete process.env.TS_API_TIMING_DIR;
    const checked=spawnSync(native,['--version'],{encoding:'utf8',timeout:5000,maxBuffer:65536});
    if(checked.error||checked.status!==0||checked.stdout.trim()!==`Version ${version}`)
      throw new Error(`Selected native executable did not report ${version}: ${checked.error?.message??checked.stderr??checked.stdout}`);
    api=await import(pathToFileURL(require.resolve('typescript/unstable/sync')).href);
    ast=await import(pathToFileURL(require.resolve('typescript/unstable/ast')).href);
  } catch(error) {
    throw new ProjectSetupError('compiler_unavailable',`Cannot load selected native TypeScript ${version}: ${error.message}; restore the repository's exact TypeScript package and matching optional platform package. No compiler substitution was made.`,version);
  }
  // These are the two renamed syntax predicates used by the existing graph
  // extractor, not an emulation of the legacy checker/program object graph.
  const syntax={...ast,SymbolFlags:api.SymbolFlags,
    isFunctionLike:ast.isSignatureDeclaration,isMethodSignature:ast.isMethodSignatureDeclaration};
  const open=directory=>new api.API({tsserverPath:native,cwd:directory,collectTiming:false});
  return {
    native:true,version,
    plan(config) {
      const client=open(path.dirname(config));
      try {return {...client.parseConfigFile(config),projectReferences:[]};}
      finally {client.close();}
    },
    analyze(root,config,looseFiles,setupDiagnostics,inferredDirectory,emit,epoch,testPolicy) {
      return analyzeNative({root,config,looseFiles,setupDiagnostics,inferredDirectory,send:emit,epoch,testPolicy,
        version,syntax,api,compilerManifest:path.join(packageRoot,'package.json'),nativeManifest,native,open});
    }
  };
}

function analyzeNative({root,config,looseFiles,setupDiagnostics,inferredDirectory,send:emitFrame,epoch,testPolicy,
  version,syntax:ts,api,compilerManifest,nativeManifest,native,open}) {
  classificationPolicy(testPolicy);
  const inferred=!config, projectPath=config?relative(root,config):'$inferred';
  const id=`ts:${projectPath}${inferred&&inferredDirectory!==root?':'+relative(root,inferredDirectory):''}`;
  const directory=config?path.dirname(config):inferredDirectory;
  const client=open(directory);
  let snapshot;
  try {
    snapshot=client.updateSnapshot(config?{openProjects:[config]}:{openFiles:looseFiles});
    const project=config?snapshot.getProject(config):snapshot.getDefaultProjectForFile(looseFiles[0]);
    if(!project)throw new ProjectSetupError('compiler_context_unavailable',`Native TypeScript ${version} did not produce the requested ${config??'inferred saved-file'} project.`,version);
    if(inferred&&project.configFileName&&fs.existsSync(project.configFileName))
      throw new ProjectSetupError('compiler_context_unavailable',`Native TypeScript selected configured project ${project.configFileName} for files requested as inferred; refresh configured ownership rather than treating it as standalone.`,version);
    const program=project.program,checker=project.checker;
    const diagnostics=[{code:'native_typescript_scope',severity:'warning',path:projectPath,
      message:'Native TypeScript 7.0.2 frontend: symbols, declarations and call/dependency facts use this exact compiler Program. Compiler-consumed first-party sources remain owned by the consumer compilation. Independent project-body links, receiver-specialized dispatch and complete compiler-input provenance are not available; no reference-context proof is inferred from names or filenames.'}];
    if(inferred)diagnostics.push({code:'inferred_configuration',severity:'info',path:'',
      message:'The native compiler inferred this saved-file project; its effective compiler options are reported. No synthetic configuration was written.'});
    const files=[];
    let omitted=0;
    for(const filename of program.getSourceFileNames()) {
      // Program metadata can exclude dependency/default-library bodies before
      // an AST round trip materializes them in the Node client. An individual
      // external declaration may still be resolved below when a fact needs it.
      const metadata=program.getSourceFileMetadata(filename);
      if(!metadata) {
        if(diagnostics.length<100)diagnostics.push({code:'native_source_metadata_unavailable',severity:'warning',path:boundedText(relative(root,filename)),
          message:'The selected compiler supplied no source ownership metadata; this artifact and its facts are omitted.'});
        omitted++;continue;
      }
      if(metadata.isDefaultLibrary||metadata.isFromExternalLibrary)continue;
      const origin=sourceBoundary(root,filename);
      if(origin) {if(diagnostics.length<100)diagnostics.push({...origin,message:'Native analysis omits this source body and its facts because its workspace origin is outside or unverified; compiler-backed external identities and signatures may remain.'});omitted++;continue;}
      const file=program.getSourceFile(filename);
      if(!file){omitted++;continue;}
      const utf8=Buffer.from(file.text,'utf8'),sourcePath=relative(root,file.fileName);
      if(!canSend('source',{id:`${sourcePath}@${'0'.repeat(64)}`,path:sourcePath,utf8,sha256:'0'.repeat(64),generated:false,compilationId:id,
        ...(testPolicy?{testClassification:sourceClassification(sourcePath,testPolicy)}:{})})) {
        if(diagnostics.length<100)diagnostics.push({code:'artifact_too_large',severity:'warning',path:boundedText(sourcePath),
          message:`Native compiler source exceeds the ${MAX_FRAME}-byte worker frame; its facts and body are omitted.`});
        omitted++;continue;
      }
      files.push(file);
    }
    const indexed=new Set(files.map(file=>path.resolve(file.fileName)));
    const fingerprint=createHash('sha256');
    const add=value=>fingerprint.update(JSON.stringify(value)).update('\0');
    add([version,epoch,id,project.compilerOptions]);
    for(const file of [...files].sort((a,b)=>a.fileName.localeCompare(b.fileName)))add([path.resolve(file.fileName),file.text]);
    const inputs=new Set([...(config?[config]:[]),...files.map(file=>file.fileName),compilerManifest,nativeManifest,native]);
    if(config)add([path.resolve(config),fs.readFileSync(config,'utf8')]);
    const options={...project.compilerOptions,knowCodeNativeCompiler:version,knowCodeNativeApi:'unstable/sync',
      knowCodeSourceOwnership:'compiler-consumed first-party source in the consumer Program',
      knowCodeIndependentProjectContextProof:false,...(inferred?{inferredRoot:relative(root,inferredDirectory)}:{})};
    emitFrame('compilation',{id,language:'typescript',projectPath,inferred,compilerVersion:version,
      target:'',configuration:'',optionsJson:JSON.stringify(options),inputPaths:[...inputs],dependencies:[],references:[],
      inputFingerprint:fingerprint.digest('hex'),...(testPolicy?{testClassification:compilationClassification(testPolicy)}:{})});
    emitFrame('progress',{stage:'analysis',projectPath,message:`Extracting exact native TypeScript ${version} compiler facts; independent project links remain unavailable.`});
    let batch={compilationId:id,symbols:[],occurrences:[],relationships:[]};
    const flush=()=>{if(batch.symbols.length||batch.occurrences.length||batch.relationships.length)emitFrame('facts',batch);batch={compilationId:id,symbols:[],occurrences:[],relationships:[]};};
    const declarationCache=new WeakMap(),canonicalCache=new WeakMap(),emitted=new Set(),anchored=new Set(),unsupportedViews=new WeakSet();
    let current,artifactId,offsets,missing=0,transientViews=0;
    const declarations=symbol=>{
      if(!symbol)return [];
      if(!declarationCache.has(symbol))declarationCache.set(symbol,(symbol.declarations??[]).map(handle=>handle.resolve()).filter(Boolean));
      return declarationCache.get(symbol);
    };
    const same=(a,b)=>a===b||!!a&&!!b&&a.pos===b.pos&&a.end===b.end&&a.kind===b.kind&&a.getSourceFile().fileName===b.getSourceFile().fileName;
    function canonical(symbol) {
      if(canonicalCache.has(symbol))return canonicalCache.get(symbol);
      let selected=symbol;
      const roots=declarations(symbol);
      // Only a compiler lookup at the one exact declaration can establish a
      // declaration root here. Multiple-root synthetic views are never picked.
      if(symbol.flags&api.SymbolFlags.Transient&&roots.length===1&&roots[0].name) {
        const rootSymbol=checker.getSymbolAtLocation(roots[0].name);
        if(rootSymbol&&!(rootSymbol.flags&api.SymbolFlags.Transient)&&declarations(rootSymbol).some(d=>same(d,roots[0])))selected=rootSymbol;
      }
      canonicalCache.set(symbol,selected);return selected;
    }
    const synthetic=(node,kind,name)=>{
      const key=syntheticIdentity(root,id,node,kind);
      if(!emitted.has(key)){emitted.add(key);batch.symbols.push({id:key,name,kind,signature:name,external:!indexed.has(path.resolve(node.getSourceFile().fileName))});}
      if(kind==='module'||kind==='initializer'){occurrence(key,node,'declaration');occurrence(key,node,'definition');}
      return key;
    };
    function occurrence(symbolId,node,role) {
      if(!symbolId||node.getSourceFile()!==current)return;
      const startByte=offsets[node.getStart(current)],endByte=offsets[node.getEnd()];
      if(endByte<=startByte)return;
      const key=JSON.stringify([symbolId,artifactId,startByte,endByte,role]);
      if(anchored.has(key))return;anchored.add(key);
      batch.occurrences.push({symbolId,artifactId,startByte,endByte,role,enclosingSymbolId:''});
    }
    function emit(raw) {
      if(!raw||checker.isUnknownSymbol(raw))return '';
      const symbol=canonical(raw);
      // Shared declaration anchors alone cannot distinguish compiler-specialized
      // or multiple-root views. Keep only proven canonical declaration symbols;
      // a runtime symbol id or a selected branch must not become a public id.
      if(symbol.flags&api.SymbolFlags.Transient) {
        if(!unsupportedViews.has(symbol)){unsupportedViews.add(symbol);transientViews++;}
        return '';
      }
      const nodes=declarations(symbol);
      if(!nodes.length){missing++;return '';}
      const facade={declarations:nodes,flags:symbol.flags,getName:()=>symbol.name};
      const key=symbolIdentity(root,id,facade);
      if(!emitted.has(key)) {
        const declaration=symbol.valueDeclaration?.resolve()??nodes[0];
        const type=checker.getTypeOfSymbolAtLocation(symbol,declaration);
        emitted.add(key);batch.symbols.push({id:key,name:symbol.name,kind:api.SymbolFlags[symbol.flags]??String(symbol.flags),
          signature:type?checker.typeToString(type):symbol.name,external:!nodes.some(d=>indexed.has(path.resolve(d.getSourceFile().fileName)))});
      }
      if(symbol.flags&api.SymbolFlags.Module)for(const node of nodes)if(ts.isSourceFile(node)){occurrence(key,node,'declaration');occurrence(key,node,'definition');}
      return key;
    }
    const graph=graphFacts(ts,checker,{emit,synthetic,occurrence:(symbol,node,role)=>{occurrence(symbol,node,role);if(role==='declaration')occurrence(symbol,node,'definition');},dispatchKey:()=>'',implementationProofs:false,
      symbolName:symbol=>symbol.name,declarations,signatureDeclaration:signature=>signature?.declaration?.resolve(),
      relationship:edge=>{if(!edge.sourceId)return;batch.relationships.push(edge);if(batch.relationships.length>=128)flush();}});
    for(const file of files) {
      current=file;offsets=byteOffsets(file.text);
      const utf8=Buffer.from(file.text,'utf8'),sha256=createHash('sha256').update(utf8).digest('hex');
      artifactId=`${relative(root,file.fileName)}@${sha256}`;
      flush();
      emitFrame('source',{id:artifactId,path:relative(root,file.fileName),utf8,sha256,generated:false,compilationId:id,
        ...(testPolicy?{testClassification:sourceClassification(relative(root,file.fileName),testPolicy)}:{})});
      graph.start(file,artifactId,offsets);
      const visit=(node,parent)=>{
        const owner=graph.scope(node,parent);
        graph.operation(node,owner);
        if(ts.isIdentifier(node)||ts.isPrivateIdentifier(node)) {
          const found=checker.getSymbolAtLocation(node);
          if(found&&!checker.isUnknownSymbol(found)) {
            const declared=declarations(found).some(d=>same(d.name,node));
            const symbols=[!declared&&(found.flags&api.SymbolFlags.Alias)?checker.getAliasedSymbol(found):found];
            if(ts.isShorthandPropertyAssignment(node.parent)) {
              const value=checker.getShorthandAssignmentValueSymbol(node.parent);if(value&&value!==found)symbols.push(value);
            }
            for(const symbol of symbols) {
              const sid=emit(symbol);if(!sid)continue;
              occurrence(sid,node,declared?'declaration':'reference');
              if(declared)for(const extent of declarations(symbol))if(same(extent.name,node))occurrence(sid,extent,'definition');
              graph.reference(node,symbol,owner,declared);
            }
          } else if(!(ts.isLabeledStatement(node.parent)||ts.isBreakStatement(node.parent)||ts.isContinueStatement(node.parent)))missing++;
        }
        if(batch.occurrences.length>=128)flush();
        node.forEachChild(child=>{visit(child,owner);});
      };
      visit(file);flush();
    }
    const compilerDiagnostics=[...program.getConfigFileParsingDiagnostics(),...program.getGlobalDiagnostics(),
      ...program.getSyntacticDiagnostics(),...program.getSemanticDiagnostics()];
    diagnostics.push(...compilerDiagnostics.slice(0,100).map(d=>({code:`TS${d.code}`,severity:d.category===api.DiagnosticCategory.Error?'error':'warning',
      path:d.fileName?relative(root,d.fileName):'',message:boundedText(d.text)})),...setupDiagnostics.slice(0,100));
    if(transientViews)diagnostics.push({code:'native_transient_views_omitted',severity:'warning',path:projectPath,
      message:`${transientViews} compiler transient symbol views lack a proven single declaration root. Exact references for these views are omitted rather than collapsing specializations or choosing one branch.`});
    if(missing||omitted)diagnostics.push({code:'native_typescript_omissions',severity:'warning',path:projectPath,
      message:`${missing} unresolved or unsupported symbol lookups and ${omitted} omitted source artifacts; no exact reference or source ownership was invented.`});
    if(compilerDiagnostics.length>100)diagnostics.push({code:'diagnostics_truncated',severity:'info',path:'',message:`${compilerDiagnostics.length} compiler diagnostics; first100 retained.`});
    emitFrame('report',{compilationId:id,status:'partial',diagnostics,
      capabilities:{calls:'partial',dependencies:'partial',dispatch:'partial',project_links:'unavailable',...(testPolicy?{test_classification:'partial'}:{})}});
    return {references:[],files:files.map(file=>path.resolve(file.fileName))};
  } finally {
    try {snapshot?.dispose();}
    finally {client.close();}
  }
}
