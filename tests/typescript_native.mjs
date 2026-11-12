// Exact selected native compiler -> existing worker frames; no native/store build.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import {spawnSync} from 'node:child_process';
import protobuf from 'protobufjs';

const project=fileURLToPath(new URL('..',import.meta.url));
const compilerRoot=process.env.KNOW_CODE_TEST_TYPESCRIPT_ROOT;
assert(compilerRoot&&path.isAbsolute(compilerRoot),'Supply the restored exact TypeScript 7.0.2 package with KNOW_CODE_TEST_TYPESCRIPT_ROOT');
assert.equal(JSON.parse(fs.readFileSync(path.join(compilerRoot,'package.json'),'utf8')).version,'7.0.2');
const run=path.join(project,'.artifacts/typescript-native',process.hrtime.bigint().toString());
const workspace=path.join(run,'workspace');fs.mkdirSync(path.join(workspace,'node_modules'),{recursive:true});
fs.symlinkSync(compilerRoot,path.join(workspace,'node_modules/typescript'),'dir');
const write=(name,text)=>{const file=path.join(workspace,name);fs.mkdirSync(path.dirname(file),{recursive:true});fs.writeFileSync(file,text);return file;};
const options={strict:true,target:'es2022',module:'nodenext',moduleResolution:'nodenext',composite:true};
const dep=write('dep/tsconfig.json',JSON.stringify({compilerOptions:options,files:['code.ts']}));
write('dep/code.ts','export interface Shape { value: number; }\nexport const count = 41;\nexport function leaf(value: number) { return value + 1; }\nexport class Box<T> { constructor(public item: T) { const constructorLocal = item; } }\n');
const source=write('app/code.ts','import {leaf as invoke, count, Box, type Shape} from "../dep/code.js";\n// π😀 before UTF-8 anchors\nexport function start(input: Shape) { const café = count; return invoke(café) + input.value; }\nexport function other(input: number) { const café = input; return café; }\nconst text = new Box("text"); const number = new Box(1); text.item; number.item;\n');
const config=write('app/tsconfig.json',JSON.stringify({compilerOptions:options,files:['code.ts'],references:[{path:'../dep'}]}));
const disabled=write('app/tsconfig.disabled.json',JSON.stringify({compilerOptions:{...options,disableSourceOfProjectReferenceRedirect:true},files:['code.ts'],references:[{path:'../dep'}]}));
const loose=write('loose/standalone.ts','export function inferred(value: number) { const retained = value; return retained; }\n');
const schema=await protobuf.load(path.join(project,'proto/analysis.proto'));
const Envelope=schema.lookupType('knowcode.Envelope');
const request={workspaceRoot:workspace,projects:[config,dep,disabled],configuredProjects:[config,dep,disabled],looseSources:[loose],preparationFingerprint:'native-fixture'};
const env={...process.env};delete env.NODE_OPTIONS;delete env.NODE_PATH;
function worker(label) {
  const jobId='native-typescript-'+label;
  const bytes=Envelope.encode(Envelope.create({protocolVersion:7,jobId,analyze:request})).finish();
  const header=Buffer.alloc(4);header.writeUInt32LE(bytes.length);
  const child=spawnSync(process.execPath,['--expose-gc',path.join(project,'providers/typescript/worker.mjs')],
    {input:Buffer.concat([header,bytes]),cwd:project,env,timeout:120000,maxBuffer:32*1024*1024,detached:true});
  fs.writeFileSync(path.join(run,label+'.stdout'),child.stdout??Buffer.alloc(0));
  fs.writeFileSync(path.join(run,label+'.stderr'),child.stderr??Buffer.alloc(0));
  if(child.error&&child.pid){try{process.kill(-child.pid,'SIGTERM');}catch{}}
  assert.equal(child.status,0,child.stderr?.toString()??String(child.error));
  const frames=[];
  for(let offset=0;offset<child.stdout.length;) {
    assert(offset+4<=child.stdout.length,'truncated worker frame prefix');
    const size=child.stdout.readUInt32LE(offset);offset+=4;
    assert(size>0&&size<=8*1024*1024&&offset+size<=child.stdout.length);
    const frame=Envelope.toObject(Envelope.decode(child.stdout.subarray(offset,offset+size)),{defaults:true,bytes:String,longs:Number});offset+=size;
    assert.equal(frame.protocolVersion,7);assert.equal(frame.jobId,jobId);frames.push(frame);
  }
  assert(frames.at(-1)?.complete.success);
  assert(!frames.some(f=>f.preparation),'fixture must not perform restoration');
  fs.writeFileSync(path.join(run,label+'.json'),JSON.stringify(frames,null,2));return frames;
}
function inspect(frames) {
  const compilations=frames.filter(f=>f.compilation).map(f=>f.compilation);
  assert.equal(compilations.length,4);
  assert(compilations.every(c=>c.compilerVersion==='7.0.2'&&c.references.length===0));
  assert(compilations.some(c=>c.inferred));
  const reports=frames.filter(f=>f.report).map(f=>f.report);
  assert(reports.every(r=>r.status==='partial'&&r.capabilities.project_links==='unavailable'));
  assert(reports.every(r=>r.diagnostics.some(d=>d.code==='native_typescript_scope')));
  const facts=frames.filter(f=>f.facts).map(f=>f.facts);
  const symbols=new Map(facts.flatMap(f=>f.symbols).map(s=>[s.id,s]));
  const occurrences=facts.flatMap(f=>f.occurrences);
  const relationships=facts.flatMap(f=>f.relationships);
  const sources=new Map(frames.filter(f=>f.source).map(f=>[f.source.id,Buffer.from(f.source.utf8,'base64')]));
  for(const occurrence of occurrences) {
    assert(symbols.has(occurrence.symbolId));const bytes=sources.get(occurrence.artifactId);assert(bytes);
    assert(0<=occurrence.startByte&&occurrence.startByte<occurrence.endByte&&occurrence.endByte<=bytes.length);
    new TextDecoder('utf-8',{fatal:true}).decode(bytes.subarray(occurrence.startByte,occurrence.endByte));
  }
  const appId='ts:app/tsconfig.json';
  const local=[...symbols.values()].filter(s=>s.id.startsWith(appId+':')&&s.name==='café');
  assert.equal(local.length,2);assert.notEqual(local[0].id,local[1].id);
  for(const symbol of local) {
    const uses=occurrences.filter(o=>o.symbolId===symbol.id&&o.role!=='definition');
    assert(uses.some(o=>o.role==='declaration')&&uses.some(o=>o.role==='reference'));
    for(const use of uses)assert.equal(sources.get(use.artifactId).subarray(use.startByte,use.endByte).toString('utf8'),'café');
  }
  const appEdges=relationships.filter(e=>e.sourceId.startsWith(appId+':'));
  assert(appEdges.some(e=>e.kind==='call'&&symbols.get(e.sourceId)?.name==='start'&&symbols.get(e.targetId)?.name==='leaf'));
  assert(appEdges.some(e=>e.kind==='reference'&&symbols.get(e.targetId)?.name==='count'));
  assert(appEdges.some(e=>e.kind==='type_reference'&&symbols.get(e.targetId)?.name==='Shape'));
  assert([...symbols.values()].every(s=>!s.definitionCompilationId&&!s.definitionFingerprint),'independent body links must not be invented');
  assert(frames.some(f=>f.source?.compilationId===appId&&f.source.path==='dep/code.ts'),'consumer source must come from actual caller Program');
  const disabledId='ts:app/tsconfig.disabled.json';
  assert(!frames.some(f=>f.source?.compilationId===disabledId&&f.source.path==='dep/code.ts'),'disabled redirect must not borrow another Program source');
  assert(!relationships.some(e=>e.sourceId.startsWith(disabledId+':')&&symbols.get(e.targetId)?.name==='leaf'&&e.resolution==='compiler_bound'));
  const items=[...symbols.values()].filter(s=>s.id.startsWith(appId+':')&&s.name==='item'&&s.kind==='Property');
  assert.equal(items.length,1,'generic property views need the exact compiler declaration root');
  assert.equal(items[0].signature,'T');
  const parameters=[...symbols.values()].filter(s=>s.id.startsWith(appId+':')&&s.name==='item'&&s.kind==='FunctionScopedVariable');
  assert.equal(parameters.length,1,'constructor parameter and generated property have different semantic kinds');
  assert.notEqual(parameters[0].id,items[0].id);
  assert(occurrences.some(o=>o.symbolId===parameters[0].id&&o.role==='declaration'));
  assert(occurrences.some(o=>o.symbolId===parameters[0].id&&o.role==='reference'));
  const appText=fs.readFileSync(source,'utf8');
  for(const expression of ['text.item','number.item']) {
    const position=appText.indexOf(expression)+expression.lastIndexOf('item');
    const startByte=Buffer.byteLength(appText.slice(0,position));
    const uses=occurrences.filter(o=>o.role==='reference'&&o.startByte===startByte&&o.artifactId.startsWith('app/code.ts@')&&o.symbolId.startsWith(appId+':'));
    assert.deepEqual(uses.map(o=>o.symbolId),[items[0].id],'each generic use must bind to the proven declaration root');
  }
  return {compilations,reports,symbols,occurrences,relationships};
}
const initial=inspect(worker('initial'));
fs.appendFileSync(source,'\nexport const broken: number = "wrong";\n');
const saved=inspect(worker('saved'));
const appReport=saved.reports.find(r=>r.compilationId==='ts:app/tsconfig.json');
assert(appReport.diagnostics.some(d=>d.severity==='error'&&d.code.startsWith('TS')));
assert(saved.symbols.size>0&&saved.relationships.length>0);
assert.notEqual(initial.compilations.find(c=>c.id==='ts:app/tsconfig.json').inputFingerprint,
  saved.compilations.find(c=>c.id==='ts:app/tsconfig.json').inputFingerprint);
const result={status:'passed_focused_native_worker',compiler:'7.0.2',run,checks:[
  'exact_selected_native_worker_and_compiler_identity','configured_and_inferred_saved_sources',
  'alias_calls_value_and_type_dependencies','scoped_Unicode_locals_and_UTF8_ranges','generic_declaration_root',
  'actual_consumer_program_source_only','disabled_redirect_preserves_missing_source_boundary',
  'no_invented_independent_project_context_links','saved_partial_compiler_graph_and_changed_snapshot']};
fs.writeFileSync(path.join(run,'result.json'),JSON.stringify(result,null,2)+'\n');console.log(JSON.stringify(result));
