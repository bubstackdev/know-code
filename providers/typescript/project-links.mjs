import path from 'node:path';
import {createHash} from 'node:crypto';
import {symbolIdentity,syntheticIdentity} from './symbols.mjs';
const relative=(root,file)=>path.relative(root,file).split(path.sep).join('/');

// The compiler supplies the resolved reference tree and source ownership.
// Fingerprints witness saved project inputs/options plus the restoration epoch;
// no declaration-name or output-path guessing is used to choose a definition.
export function projectLinks(ts,root,config,parsed,program,epoch) {
  const contexts=new Map(), owners=new Map(), snapshots=new Map();
  let complete=true;
  const ownId=config?`ts:${relative(root,config)}`:null;
  const ownFiles=new Set(parsed.fileNames.map(f=>path.resolve(f)));
  function register(ref,ancestors=new Set()) {
    if(!ref){complete=false;return;}
    const filename=path.resolve(ref.sourceFile.fileName);
    if(ancestors.has(filename)){complete=false;return;}
    if(contexts.has(filename))return;
    const id=`ts:${relative(root,filename)}`;
    const entry={id,projectPath:relative(root,filename),parsed:ref.commandLine,refs:ref.references??[],filename};
    contexts.set(filename,entry);
    for(const file of entry.parsed.fileNames) {
      const key=path.resolve(file);
      if(owners.has(key)&&owners.get(key)!==entry) {owners.set(key,null);complete=false;}
      else owners.set(key,entry);
    }
    for(const child of entry.refs)register(child,new Set([...ancestors,filename]));
  }
  for(const ref of program.getResolvedProjectReferences()??[])register(ref);
  if(parsed.projectReferences?.length && (parsed.options.disableSourceOfProjectReferenceRedirect || typeof program.isSourceOfProjectReferenceRedirect!=='function'))complete=false;
  // Refuse a snapshot witness that omits compiler-consumed first-party inputs
  // (for example an imported file omitted from a composite project's files).
  for(const file of program.getSourceFiles()) {
    if(program.isSourceFileDefaultLibrary(file)||program.isSourceFileFromExternalLibrary(file))continue;
    const filename=path.resolve(file.fileName);
    if(!ownFiles.has(filename)&&!owners.has(filename))complete=false;
    if(program.isSourceOfProjectReferenceRedirect?.(file.fileName)&&!owners.get(filename))complete=false;
  }
  function snapshot(entry,visiting=new Set()) {
    if(snapshots.has(entry.id))return snapshots.get(entry.id);
    if(visiting.has(entry.id)){complete=false;return '';}
    const hash=createHash('sha256');
    const add=value=>hash.update(JSON.stringify(value)).update('\0');
    add([ts.version,epoch,entry.id,entry.parsed.options]);
    for(const file of [...entry.parsed.fileNames].sort()) {
      // Prefer compiler-captured text, so a concurrent save cannot be mistaken
      // for the version that supplied the binding. Publication rechecks inputs.
      const text=program.getSourceFile(file)?.text??ts.sys.readFile(file);
      if(text===undefined){complete=false;return '';}
      add([path.resolve(file),text]);
    }
    for(const ref of entry.refs) {
      const child=ref&&contexts.get(path.resolve(ref.sourceFile.fileName));
      if(!child){complete=false;return '';}
      const digest=snapshot(child,new Set([...visiting,entry.id]));
      if(!digest)return '';
      add([child.id,digest]);
    }
    const digest=hash.digest('hex');snapshots.set(entry.id,digest);return digest;
  }
  const self={id:ownId??'$inferred',parsed,refs:program.getResolvedProjectReferences()??[]};
  const fingerprint=snapshot(self);
  const references=[...contexts.values()].map(e=>({compilationId:e.id,projectPath:e.projectPath,inputFingerprint:snapshot(e)})).filter(e=>e.compilationId!==ownId);
  function definition(declarations,identity) {
    if(!complete || !declarations.length)return {};
    const entries=declarations.map(d=>owners.get(path.resolve(d.getSourceFile().fileName)));
    const entry=entries[0];
    if(!entry || entries.some(e=>e!==entry) || declarations.some(d=>ownFiles.has(path.resolve(d.getSourceFile().fileName))||!program.isSourceOfProjectReferenceRedirect(d.getSourceFile().fileName)))return {};
    const digest=snapshot(entry);
    if(!digest)return {};
    return {definitionCompilationId:entry.id,definitionSymbolId:identity(entry.id),definitionFingerprint:digest};
  }
  return {
    get complete(){return complete;},fingerprint,references,
    owns(file) {
      const filename=path.resolve(file.fileName);
      return ownFiles.has(filename)||(!owners.has(filename)&&!program.isSourceOfProjectReferenceRedirect?.(file.fileName));
    },
    definition(symbol) {return definition(symbol.declarations??[],context=>symbolIdentity(root,context,symbol));},
    synthetic(node,kind) {return definition([node],context=>syntheticIdentity(root,context,node,kind));}
  };
}
