// Direct compiler worker; no serving database, query engine or test execution.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import {spawnSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import {fileURLToPath} from 'node:url';
import protobuf from 'protobufjs';
const root=fileURLToPath(new URL('..',import.meta.url)),profile=process.env.KNOW_CODE_BUILD_PROFILE??'debug';
const worker=path.join(root,`providers/dotnet/bin/${profile==='debug'?'Debug':'Release'}/net10.0/KnowCode.Provider.DotNet.dll`);
const run=path.join(root,'.artifacts/csharp-classification',Date.now().toString(16)),workspace=path.join(run,'workspace');
const Envelope=(await protobuf.load(path.join(root,'proto/analysis.proto'))).lookupType('knowcode.Envelope');
const checks=[];let sequence=0;
function write(file,text){fs.mkdirSync(path.dirname(file),{recursive:true});fs.writeFileSync(file,text);}
function invoke(projects,policy=1,preparations=[]){
  const body=Envelope.encode(Envelope.create({protocolVersion:7,jobId:'classification-test',analyze:{workspaceRoot:workspace,
    projects,testClassificationPolicy:policy,preparationFingerprint:'constant-to-isolate-classification-snapshots',preparations,
    analysisPolicy:{version:1,csharp:{}}}})).finish();
  const header=Buffer.alloc(4);header.writeUInt32LE(body.length);
  const result=spawnSync('dotnet',[worker],{cwd:workspace,input:Buffer.concat([header,body]),maxBuffer:32*1024*1024,timeout:180000,
    env:{...process.env,DOTNET_CLI_TELEMETRY_OPTOUT:'1',DOTNET_NOLOGO:'1',MSBUILDDISABLENODEREUSE:'1',DOTNET_CLI_USE_MSBUILD_SERVER:'0',UseSharedCompilation:'false'}});
  const messages=[];
  for(let offset=0;offset<result.stdout.length;){
    assert(offset+4<=result.stdout.length);const size=result.stdout.readUInt32LE(offset);offset+=4;
    assert(size>0&&size<=8*1024*1024&&offset+size<=result.stdout.length);
    const message=Envelope.toObject(Envelope.decode(result.stdout.subarray(offset,offset+size)),{defaults:true,arrays:true,objects:true,bytes:String,longs:Number});offset+=size;
    assert.equal(message.protocolVersion,7);assert.equal(message.jobId,'classification-test');messages.push(message);
  }
  write(path.join(run,`worker-${sequence++}.json`),JSON.stringify({status:result.status,error:String(result.error??''),stderr:result.stderr.toString(),messages},null,2)+'\n');
  return {status:result.status,messages,stderr:result.stderr.toString(),compilations:messages.flatMap(m=>m.compilation?[m.compilation]:[]),
    sources:messages.flatMap(m=>m.source?[m.source]:[]),reports:messages.flatMap(m=>m.report?[m.report]:[]),
    preparations:messages.flatMap(m=>m.preparation?[m.preparation]:[])};
}
function success(result,policy=1,partial=false){
  assert.equal(result.status,0,result.stderr);assert.equal(result.messages[0].provider.testClassificationPolicy,policy);
  assert(result.messages.at(-1).complete.success);assert(result.reports.length>0);
  if(!partial)assert(result.reports.every(r=>r.status==='complete'),result.reports);
}
const project=name=>path.join(workspace,name,`${name}.csproj`);
const flags=path.join(workspace,'Tests/Flags.props');
const flagsText=value=>`<Project><PropertyGroup><IsTestProject Condition="'$(TargetFramework)'=='net10.0'">${value}</IsTestProject><IsTestProject Condition="'$(TargetFramework)'!='net10.0'">false</IsTestProject></PropertyGroup></Project>`;
write(flags,flagsText('true'));
const shared='// π😀 linked source\npublic static class SharedApi { public static int Read(int scopedArg) { var scopedLocal=scopedArg; return scopedLocal+scopedArg; } }\n';
write(path.join(workspace,'Shared.cs'),shared);
for(const name of ['Tests','Production'])write(project(name),`<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><TargetFrameworks>net10.0;netstandard2.1</TargetFrameworks><LangVersion>latest</LangVersion><Nullable>enable</Nullable>${name==='Production'?`<IsTestProject Condition="'$(TargetFramework)'!='net10.0'">false</IsTestProject><IsTestProject Condition="'$(TargetFramework)'=='net10.0'"></IsTestProject>`:''}</PropertyGroup>
  ${name==='Tests'?'<Import Project="Flags.props"/>':''}<ItemGroup><Compile Include="../Shared.cs" Link="Linked.cs"/></ItemGroup>
  <Target Name="NeverBuild" BeforeTargets="Build"><Error Text="Classification requested compilation"/></Target></Project>`);
write(path.join(workspace,'Tests/Generated.cs'),`#if NET10_0
using System.Text.Json.Serialization;
public class Payload { public int Value { get; set; } }
[JsonSerializable(typeof(Payload))] internal partial class PayloadContext : JsonSerializerContext { }
#endif
`);
write(project('Consumer'),'<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><TargetFramework>net10.0</TargetFramework></PropertyGroup><ItemGroup><ProjectReference Include="../Tests/Tests.csproj"/></ItemGroup><Target Name="NeverBuild" BeforeTargets="Build"><Error Text="Classification requested compilation"/></Target></Project>');
write(path.join(workspace,'Consumer/Code.cs'),'public static class Consumer { public static int Call() => SharedApi.Read(3); }\n');
const projects=['Tests','Production','Consumer'].map(project);
const first=invoke(projects);success(first);assert.equal(first.compilations.length,5);
for(const compilation of first.compilations){
  const unmarked=compilation.projectPath==='Consumer/Consumer.csproj';
  const inconclusive=compilation.projectPath==='Production/Production.csproj'&&compilation.target==='net10.0';
  const expected=unmarked||inconclusive?'unknown':compilation.projectPath==='Tests/Tests.csproj'&&compilation.target==='net10.0'?'test':'non_test';
  assert.equal(compilation.testClassification.kind,expected);assert.equal(compilation.testClassification.policyVersion,1);
  assert(compilation.testClassification.evidence.every(e=>e.path===compilation.projectPath&&e.value.includes(`target=${compilation.target}`)&&e.value.includes('stage=post_GetAssemblyVersion')&&!e.inferred));
  assert.equal(first.reports.find(r=>r.compilationId===compilation.id).capabilities.test_classification,unmarked?'unknown':inconclusive?'partial':'complete');
  assert(!Object.keys(JSON.parse(compilation.optionsJson).compiler_metadata).some(k=>k==='IsTestProject'||k==='IsTestingPlatformApplication'));
}
checks.push('exact_policy_ack_all_targets_and_separate_context_metadata');
checks.push('policy1_unmarked_and_inconclusive_signals_report_unknown_and_partial');
const linked=first.sources.filter(s=>s.path==='Shared.cs');assert.equal(linked.length,4);assert.equal(new Set(linked.map(s=>s.id)).size,1);
for(const source of first.sources){
  const bytes=Buffer.from(source.utf8,'base64');assert.equal(createHash('sha256').update(bytes).digest('hex'),source.sha256);
  assert(!source.testClassification,'C# documents should inherit their owning compilation');
}
assert(first.sources.some(s=>s.generated&&s.path.includes('PayloadContext')),'compiler-generated source absent');
for(const source of linked){
  assert.equal(Buffer.from(source.utf8,'base64').toString(),shared);
  const uses=first.messages.filter(m=>m.facts?.compilationId===source.compilationId).flatMap(m=>m.facts.occurrences)
    .filter(o=>o.artifactId===source.id&&o.role==='reference');
  assert(uses.some(o=>Buffer.from(source.utf8,'base64').subarray(o.startByte,o.endByte).toString()==='scopedArg'),'linked UTF-8 reference missing');
}
checks.push('linked_and_generated_sources_inherit_distinct_contexts_without_changed_bytes_or_ranges');
const legacy=invoke([project('Tests')],0,first.preparations);success(legacy,0);
assert(legacy.compilations.every(c=>!c.testClassification)&&legacy.reports.every(r=>r.capabilities.test_classification==='unknown'));
assert.equal(legacy.preparations.length,0,'classification policy change repeated dependency restoration');
checks.push('legacy_absence_and_policy_only_change_reuses_preparation');
write(flags,flagsText('false'));
const changed=invoke(projects,1,first.preparations);success(changed);
const owner=result=>result.compilations.find(c=>c.projectPath==='Tests/Tests.csproj'&&c.target==='net10.0');
const consumer=result=>result.compilations.find(c=>c.projectPath==='Consumer/Consumer.csproj');
assert.equal(owner(changed).testClassification.kind,'non_test');assert(owner(changed).inputPaths.includes(flags));
assert.notEqual(owner(first).inputFingerprint,owner(changed).inputFingerprint,'classification missing from own witness');
assert.notEqual(consumer(first).inputFingerprint,consumer(changed).inputFingerprint,'referenced classification missing from consumer witness');
const declarations=result=>result.messages.flatMap(m=>m.facts?.occurrences??[]).filter(o=>o.role==='declaration')
  .map(o=>[o.symbolId,o.artifactId,o.startByte,o.endByte]).sort((a,b)=>JSON.stringify(a).localeCompare(JSON.stringify(b)));
assert.deepEqual(declarations(first),declarations(changed),'classification changed semantic identities or declaration ranges');
checks.push('saved_import_changes_own_and_recursive_snapshots_without_identity_or_source_changes');
write(path.join(workspace,'Tests/Broken.cs'),'public static class Broken { public static int Value() => missingName; }\n');
const partial=invoke([project('Tests')],1,first.preparations);success(partial,1,true);
assert(partial.reports.every(r=>r.status==='partial'&&r.capabilities.test_classification==='complete'));
assert(partial.compilations.every(c=>c.testClassification.kind==='non_test'));
checks.push('fresh_partial_compiler_graph_keeps_independent_classification_evidence');
const unsupported=invoke([path.join(workspace,'missing.csproj')],2);
assert.notEqual(unsupported.status,0);assert.match(unsupported.stderr,/unsupported test classification policy 2/);assert.equal(unsupported.messages.length,0);
checks.push('unknown_policy_rejected_before_sdk_or_facts');
assert.equal(fs.readdirSync(workspace,{recursive:true}).filter(p=>p.endsWith('.dll')).length,0,'worker built a project assembly');
checks.push('no_full_project_build_or_dependency_assembly_output');
const sourceFiles=['proto/analysis.proto','providers/dotnet/SdkMetadata.cs','providers/dotnet/ProjectCatalog.cs','providers/dotnet/Program.cs','providers/dotnet/ProjectLinks.cs','providers/dotnet/TestClasses.cs','tests/csharp_classification.mjs'];
const summary={passed:true,profile,run,checks,worker,workerSha256:createHash('sha256').update(fs.readFileSync(worker)).digest('hex'),sourceRevision:spawnSync('git',['rev-parse','HEAD'],{cwd:root,encoding:'utf8'}).stdout.trim(),
  sourceSha256:Object.fromEntries(sourceFiles.map(p=>[p,createHash('sha256').update(fs.readFileSync(path.join(root,p))).digest('hex')])),limits:['direct worker fixture only; root owns storage/query/filter/backfill/native assembly','constant request fingerprint isolates classification snapshot changes, not coordinator invalidation certification']};
write(path.join(run,'summary.json'),JSON.stringify(summary,null,2)+'\n');console.log(JSON.stringify(summary,null,2));
