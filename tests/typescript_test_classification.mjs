import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import {createHash} from 'node:crypto';
import {execFileSync, spawnSync} from 'node:child_process';
import {fileURLToPath, pathToFileURL} from 'node:url';
import protobuf from 'protobufjs';
import ts from 'typescript';
import {analyze} from '../providers/typescript/analysis.mjs';
import {classificationPolicy, compilationClassification, sourceClassification} from '../providers/typescript/test-classification.mjs';

const ROOT = fileURLToPath(new URL('..', import.meta.url));
const RUN = path.join(ROOT, '.artifacts/ts-test-classification', `${process.pid}-${process.hrtime.bigint()}`);
fs.mkdirSync(RUN, {recursive: true});
const checks = [];
const write = (filename, text) => {fs.mkdirSync(path.dirname(filename), {recursive: true}); fs.writeFileSync(filename, text);};
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
for (const filename of ['one.test.ts','one.spec.tsx','one.test.mts','one.spec.cts','one.test.js','one.spec.jsx','one.test.mjs','one.spec.cjs','src/__tests__/fixture.ts','src/__tests__/declarations.d.ts']) {
  const value = sourceClassification(filename);
  assert.equal(value.kind, 'test', filename);
  assert.equal(value.policyVersion, 1);
  assert.deepEqual(value.evidence.map(e => [e.basis,e.path,e.inferred]), [['path_convention',filename,true]]);
}
for (const filename of ['test.ts','testing.ts','a.test.d.ts','a.spec.d.mts','x.TEST.ts','__Tests__/x.ts','src/__tests__/../ordinary.ts.map','src/__tests__file.ts','__tests__']) {
  // Input to the adapter is already normalized by path.relative.
  const normalized = path.posix.normalize(filename);
  assert.equal(sourceClassification(normalized).kind, 'unknown', normalized);
}
assert.equal(sourceClassification('x.test.ts',0), undefined);
assert.equal(compilationClassification().kind,'unknown');
assert.equal(classificationPolicy(0),0);assert.equal(classificationPolicy(1),1);
assert.throws(() => classificationPolicy(2),/unsupported.*policy 2/);
const oversized = sourceClassification('😀'.repeat(1100)+'.test.ts');
assert.equal(oversized.kind,'unknown');assert.match(oversized.evidence[0].value,/exceeds/);assert.equal(oversized.evidence[0].path,'');
checks.push('conservative_case_sensitive_path_rules_unknown_and_bounded_evidence');

const workspace = path.join(RUN, 'workspace');
const files = {
  'src/helper.ts':'// café 😀 unchanged bytes\nexport function leaf() { return 1; }\nexport function productionHelper() { return leaf(); }\n',
  'checks/widget.test.ts':'import { productionHelper } from "../src/helper.js";\nexport function testBody() { return productionHelper(); }\nexport const boot = productionHelper();\nexport class StaticOwner { static value = productionHelper(); static { productionHelper(); } }\n',
  '__tests__/plain.ts':'export function testByDirectory() { return 2; }\n',
  'src/ordinary.ts':'export function it() { return "describe and test are ordinary names"; }\n',
  'names.test.d.ts':'export declare function declarationOnly(): number;\n',
  'extra.spec.js':'export function jsProbe() { return 1; }\n',
  'view.spec.tsx':'export const view = 1;\n',
  'script.spec.ts':'function scriptNode() { return 1; }\nconst scriptBoot = scriptNode();\n',
};
for (const [name,text] of Object.entries(files)) write(path.join(workspace,name),text);
const options = {strict:true,noEmit:true,skipLibCheck:true,target:'ES2022',module:'NodeNext',moduleResolution:'NodeNext',moduleDetection:'legacy',allowJs:true,checkJs:true};
const config = path.join(workspace,'tsconfig.json');write(config,JSON.stringify({compilerOptions:options,files:Object.keys(files)}));
write(path.join(workspace,'vitest.config.cjs'),`require('node:fs').writeFileSync(${JSON.stringify(path.join(RUN,'executed-runner-config'))},'wrong');throw new Error('must never execute');`);
const baselineDirectory = path.join(RUN,'baseline');
for (const name of ['analysis.mjs','project-links.mjs','symbols.mjs','graph.mjs'])
  write(path.join(baselineDirectory,name),execFileSync('git',['show',`2377774:providers/typescript/${name}`],{cwd:ROOT}));
const baselineAnalyze = (await import(pathToFileURL(path.join(baselineDirectory,'analysis.mjs')))).analyze;
function run(label, compilerAnalyze, selectedConfig=config, loose=[], policy=1) {
  const messages=[];
  compilerAnalyze(ts,workspace,selectedConfig,loose,[],workspace,(kind,value)=>messages.push({kind,value}),'same-preparation-epoch',policy);
  write(path.join(RUN,`${label}.json`),JSON.stringify(messages,null,2)+'\n');
  return messages;
}
const values = (messages,kind) => messages.filter(m=>m.kind===kind).map(m=>m.value);
const facts = (messages,key) => values(messages,'facts').flatMap(batch=>batch[key]);
const sorted = items => items.map(item=>JSON.stringify(item)).sort();
const baseline=run('baseline-2377774',baselineAnalyze);
const legacy=run('legacy-policy0',analyze,config,[],0);
const classified=run('classified-policy1',analyze);
assert.deepEqual(values(legacy,'compilation'),values(baseline,'compilation'),'legacy compilation witness preserved');
assert.deepEqual(values(legacy,'source'),values(baseline,'source'),'legacy source facts preserved');
assert.deepEqual(sorted(facts(classified,'symbols')),sorted(facts(legacy,'symbols')),'semantic symbols and IDs unchanged');
for (const current of [legacy,classified]) {
  assert.deepEqual(sorted(facts(current,'relationships')),sorted(facts(baseline,'relationships')),'calls/dependencies unchanged');
  const all = new Set(sorted(facts(current,'occurrences')));
  for (const old of sorted(facts(baseline,'occurrences'))) assert(all.has(old),'no old occurrence removed');
  assert.deepEqual(sorted(facts(current,'occurrences').filter(o=>o.role==='reference')),sorted(facts(baseline,'occurrences').filter(o=>o.role==='reference')),'references unchanged');
}
const sources = values(classified,'source');const sourceById = new Map(sources.map(s=>[s.id,s]));
for (const source of sources) {
  assert.equal(source.sha256,hash(source.utf8));assert.equal(source.id,`${source.path}@${source.sha256}`);
  assert.equal(source.utf8.toString(),files[source.path]);
  assert.equal(source.testClassification.kind,sourceClassification(source.path).kind);
}
assert.equal(values(classified,'compilation')[0].testClassification.kind,'unknown');
assert.notEqual(values(classified,'compilation')[0].inputFingerprint,values(legacy,'compilation')[0].inputFingerprint);
assert.equal(values(classified,'report')[0].status,'complete');
assert.equal(values(classified,'report')[0].capabilities.test_classification,'partial');
assert.equal(sourceById.get(facts(classified,'occurrences').find(o=>o.role==='declaration' && facts(classified,'symbols').find(s=>s.id===o.symbolId)?.name==='productionHelper').artifactId).testClassification.kind,'unknown');
const oldOccurrences=new Set(sorted(facts(baseline,'occurrences')));
const extra=facts(classified,'occurrences').filter(o=>!oldOccurrences.has(JSON.stringify(o)));
assert(extra.length>0);
const symbolsById=new Map(facts(classified,'symbols').map(s=>[s.id,s]));
assert(extra.every(o=>['declaration','definition'].includes(o.role) && ['module','initializer','ValueModule'].includes(symbolsById.get(o.symbolId).kind)));
for (const occurrence of extra) {
  const source=sourceById.get(occurrence.artifactId);assert(source);
  assert(occurrence.startByte>=0 && occurrence.startByte<=occurrence.endByte && occurrence.endByte<=source.utf8.length);
  const text=source.utf8.subarray(occurrence.startByte,occurrence.endByte).toString('utf8');assert(!text.includes('\ufffd'),'UTF-8 anchor boundary');
}
for (const symbol of facts(classified,'symbols').filter(s=>!s.external && ['initializer','module','ValueModule'].includes(s.kind))) {
  assert(facts(classified,'occurrences').some(o=>o.symbolId===symbol.id && o.role==='declaration'),'owned module/initializer has declaration anchor');
  assert(facts(classified,'occurrences').some(o=>o.symbolId===symbol.id && o.role==='definition'),'owned module/initializer has definition anchor');
}
assert(!fs.existsSync(path.join(RUN,'executed-runner-config')));
checks.push('configured_TS_TSX_JS_and_declarations_emit_owned_origin_evidence','legacy_bytes_witnesses_and_all_existing_occurrences_preserved','identities_calls_and_references_unchanged','module_and_initializer_owned_UTF8_declaration_anchors','production_helpers_remain_unknown_despite_test_callers');

const lib = path.join(workspace,'lib/tsconfig.json'),app=path.join(workspace,'app/tsconfig.json');
write(path.join(workspace,'lib/shared.spec.ts'),'export function shared() { return 1; }\n');
write(lib,JSON.stringify({compilerOptions:{...options,composite:true,declaration:true,rootDir:'.',outDir:'out'},files:['shared.spec.ts']}));
write(path.join(workspace,'app/main.ts'),'import {shared} from "../lib/shared.spec.js"; export function consumer() { return shared(); }\n');
write(app,JSON.stringify({compilerOptions:{...options,composite:true,declaration:true,rootDir:'.',outDir:'out'},files:['main.ts'],references:[{path:'../lib'}]}));
const library=run('library-policy1',analyze,lib),consumer=run('consumer-policy1',analyze,app);
const child=values(library,'compilation')[0],parent=values(consumer,'compilation')[0];
assert.equal(parent.references.find(r=>r.compilationId===child.id).inputFingerprint,child.inputFingerprint);
assert(facts(consumer,'symbols').some(s=>s.definitionCompilationId===child.id && s.definitionFingerprint===child.inputFingerprint),'body link uses same classified snapshot');
checks.push('classification_policy_and_origins_join_referenced_snapshot_witnesses');

const loose=path.join(workspace,'loose/plain.ts'),renamed=path.join(workspace,'loose/plain.test.ts');
write(loose,'// café 😀\nexport function renamed() { return 1; }\n');
const before=run('inferred-before-rename',analyze,null,[loose]);fs.renameSync(loose,renamed);
const after=run('inferred-after-same-byte-rename',analyze,null,[renamed]);
assert.equal(values(before,'source')[0].sha256,values(after,'source')[0].sha256);
assert.equal(values(before,'source')[0].testClassification.kind,'unknown');assert.equal(values(after,'source')[0].testClassification.kind,'test');
assert.notEqual(values(before,'compilation')[0].inputFingerprint,values(after,'compilation')[0].inputFingerprint);
assert(!fs.existsSync(path.join(workspace,'loose/tsconfig.json')));
const broken=path.join(workspace,'loose/broken.spec.ts');write(broken,'export function broken() { return missingName; }\n');
const partial=run('fresh-partial',analyze,null,[broken]);assert.equal(values(partial,'report')[0].status,'partial');assert.equal(values(partial,'source')[0].testClassification.kind,'test');
checks.push('inferred_and_same_byte_rename_refresh_path_classification','compiler_error_partial_keeps_path_evidence');

const schema=await protobuf.load(path.join(ROOT,'proto/analysis.proto'));const Envelope=schema.lookupType('knowcode.Envelope');
function worker(policy) {
  const request={workspaceRoot:workspace,projects:[],configuredProjects:[],looseSources:[renamed],preparationFingerprint:'same-preparation-epoch',testClassificationPolicy:policy};
  const bytes=Envelope.encode(Envelope.create({protocolVersion:7,jobId:'classification-test',analyze:request})).finish();const header=Buffer.alloc(4);header.writeUInt32LE(bytes.length);
  const result=spawnSync(process.execPath,['--expose-gc',path.join(ROOT,'providers/typescript/worker.mjs')],{cwd:workspace,input:Buffer.concat([header,bytes]),timeout:30000,maxBuffer:16*1024*1024,env:{...process.env,npm_config_update_notifier:'false'}});
  assert.ifError(result.error);const messages=[];
  for (let offset=0;offset<result.stdout.length;) {
    const size=result.stdout.readUInt32LE(offset);offset+=4;assert(size>0 && size<=8*1024*1024 && offset+size<=result.stdout.length);
    const message=Envelope.toObject(Envelope.decode(result.stdout.subarray(offset,offset+size)),{defaults:true,longs:Number,bytes:String});assert.equal(message.jobId,'classification-test');assert.equal(message.protocolVersion,7);messages.push(message);offset+=size;
  }
  write(path.join(RUN,`worker-policy${policy}.json`),JSON.stringify({status:result.status,stderr:result.stderr.toString(),messages},null,2)+'\n');
  return {status:result.status,stderr:result.stderr.toString(),messages};
}
for (const policy of [0,1]) {
  const result=worker(policy);assert.equal(result.status,0,result.stderr);assert.equal(result.messages[0].provider.testClassificationPolicy,policy);assert(result.messages.at(-1).complete.success);
  for (const message of result.messages) if(message.source) assert(policy ? message.source.testClassification.policyVersion===1 : message.source.testClassification===null);
}
const rejected=worker(2);assert.notEqual(rejected.status,0);assert.equal(rejected.messages.length,0);assert.match(rejected.stderr,/unsupported TypeScript test classification policy 2/);
checks.push('real_protocol7_workers_exact_policy_ack_and_pre_fact_rejection','runner_configuration_never_executed');
const summary={passed:true,checks,run:RUN,baseline:'2377774',compiler:ts.version,addedAnchors:extra.length,harnessSha256:hash(fs.readFileSync(fileURLToPath(import.meta.url))),limits:['adapter facts/worker only; no store/query/filter/backfill/native/MCP/package or performance certification','all test evidence is inferred path convention; no runtime execution claim']};
write(path.join(RUN,'summary.json'),JSON.stringify(summary,null,2)+'\n');console.log(JSON.stringify(summary,null,2));
