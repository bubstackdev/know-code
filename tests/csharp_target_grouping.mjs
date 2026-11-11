// Direct Roslyn evidence for the shared target-grouping policy. No server or app build.
import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import protobuf from 'protobufjs';

const root=fileURLToPath(new URL('..',import.meta.url));
const profile=process.env.KNOW_CODE_BUILD_PROFILE??'debug';
const worker=path.join(root,`providers/dotnet/bin/${profile==='debug'?'Debug':'Release'}/net10.0/KnowCode.Provider.DotNet.dll`);
const run=path.join(root,'.artifacts','csharp-target-grouping',Date.now().toString(16));
const workspace=path.join(run,'workspace');
const Envelope=(await protobuf.load(path.join(root,'proto/analysis.proto'))).lookupType('knowcode.Envelope');
const hash=file=>createHash('sha256').update(fs.readFileSync(file)).digest('hex');
const write=(file,text)=>{fs.mkdirSync(path.dirname(file),{recursive:true});fs.writeFileSync(file,text);};
const projects=['Api','Client','Variant'].map(name=>path.join(workspace,name,`${name}.csproj`));
write(path.join(workspace,'NuGet.Config'),'<configuration><packageSources><clear/></packageSources></configuration>');
const csproj=extra=>`<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><TargetFrameworks>net10.0;netstandard2.1</TargetFrameworks></PropertyGroup>${extra}<Target Name="NeverBuild" BeforeTargets="Build"><Error Text="Grouping invoked a full build"/></Target></Project>`;
write(projects[0],csproj(''));
write(projects[1],csproj('<PropertyGroup><PlatformTarget Condition="\'$(TargetFramework)\'==\'net10.0\'">x64</PlatformTarget></PropertyGroup><ItemGroup><ProjectReference Include="../Api/Api.csproj"/></ItemGroup>'));
write(projects[2],csproj('<PropertyGroup><AssemblyName Condition="\'$(TargetFramework)\'==\'net10.0\'">VariantModern</AssemblyName></PropertyGroup>'));
const code='// π😀\npublic static class SharedApi { public static int Read(int arg) { var local = arg; return local; } }\n';
write(path.join(workspace,'Api','Api.cs'),code);
write(path.join(workspace,'Variant','Api.cs'),code);
write(path.join(workspace,'Client','Use.cs'),'public static class Client { public static int Use() => SharedApi.Read(1); }');
function invoke(policy,preparations=[]){
  const encoded=Envelope.encode(Envelope.create({protocolVersion:7,jobId:'target-grouping',analyze:{workspaceRoot:workspace,
    projects,preparationFingerprint:'target-grouping-fixture',preparations,targetEquivalencePolicy:policy,
    testClassificationPolicy:1,analysisPolicy:{version:1,csharp:{}}}})).finish();
  const header=Buffer.alloc(4);header.writeUInt32LE(encoded.length);
  const result=spawnSync('dotnet',[worker],{cwd:workspace,input:Buffer.concat([header,encoded]),timeout:120000,maxBuffer:16*1024*1024,
    env:{...process.env,DOTNET_CLI_TELEMETRY_OPTOUT:'1',DOTNET_NOLOGO:'1',MSBUILDDISABLENODEREUSE:'1',DOTNET_CLI_USE_MSBUILD_SERVER:'0',UseSharedCompilation:'false'}});
  const messages=[];
  for(let at=0;at<result.stdout.length;){
    assert(at+4<=result.stdout.length);const size=result.stdout.readUInt32LE(at);at+=4;
    assert(size>0&&size<=8*1024*1024&&at+size<=result.stdout.length);
    const m=Envelope.toObject(Envelope.decode(result.stdout.subarray(at,at+size)),{defaults:true,arrays:true,objects:true,longs:Number,bytes:String});
    at+=size;assert.equal(m.protocolVersion,7);assert.equal(m.jobId,'target-grouping');messages.push(m);
  }
  write(path.join(run,`policy-${policy}.json`),JSON.stringify({status:result.status,error:String(result.error??''),stderr:result.stderr.toString(),messages},null,2));
  const payload=key=>messages.flatMap(m=>m[key]?[m[key]]:[]);
  if(policy<=1){assert.equal(result.status,0,result.stderr.toString());assert.equal(messages[0].provider.targetEquivalencePolicy,policy);assert(messages.at(-1).complete.success);}
  return {result,messages,compilations:payload('compilation'),facts:payload('facts'),sources:payload('source'),reports:payload('report'),preparations:payload('preparation')};
}
const modern=invoke(1);
assert.equal(modern.compilations.length,6);
assert(modern.reports.every(r=>r.status==='complete'),JSON.stringify(modern.reports));
const byProject=(run,name)=>run.compilations.filter(c=>c.projectPath===`${name}/${name}.csproj`);
const family=new Map();
for(const name of ['Api','Client','Variant']){
  const contexts=byProject(modern,name);
  assert.deepEqual(contexts.map(c=>c.target).sort(),['net10.0','netstandard2.1']);
  assert(contexts.every(c=>c.targetEquivalencePolicy===1&&/^[a-f0-9]{64}$/.test(c.targetFamily)));
  if(name==='Client') assert.notEqual(contexts[0].targetFamily,contexts[1].targetFamily,'effective compiler platform was ignored');
  else assert.equal(contexts[0].targetFamily,contexts[1].targetFamily);
  family.set(name,contexts[0].targetFamily);
}
assert.equal(new Set(family.values()).size,3,'unrelated projects share a target family');
const symbols=(run,context)=>run.facts.filter(b=>b.compilationId===context.id).flatMap(b=>b.symbols);
const get=(run,context,name)=>symbols(run,context).find(s=>s.name===name&&!s.external);
for(const name of ['Read','arg','local']){
  const [a,b]=byProject(modern,'Api').map(c=>get(modern,c,name));
  assert(a&&b);assert.notEqual(a.id,b.id);assert.equal(a.signature,b.signature);assert.match(a.declarationKey,/^[a-f0-9]{64}$/);assert.equal(a.declarationKey,b.declarationKey);
}
const [variant1,variant2]=byProject(modern,'Variant').map(c=>get(modern,c,'Read'));
assert(variant1&&variant2);assert.notEqual(variant1.declarationKey,variant2.declarationKey,'assembly identity was dropped');
const apiSources=modern.sources.filter(s=>s.path==='Api/Api.cs');
assert.equal(apiSources.length,2);assert.equal(apiSources[0].id,apiSources[1].id);
for(const c of byProject(modern,'Client')){
  const selected=c.references.find(r=>r.projectPath==='Api/Api.csproj');
  assert(selected);const dependency=modern.compilations.find(d=>d.id===selected.compilationId);
  assert(dependency);assert.equal(selected.inputFingerprint,dependency.inputFingerprint);
  const reference=symbols(modern,c).find(s=>s.external&&s.name==='Read'&&s.definitionCompilationId===dependency.id);
  assert(reference);assert.equal(reference.declarationKey,'');assert.equal(reference.definitionFingerprint,dependency.inputFingerprint);
}
assert(modern.sources.some(s=>s.generated),'expected compiler generated assembly info');
assert(modern.reports.every(r=>r.capabilities.target_equivalence==='partial'),'unproven generated origins must expose partial grouping');
const legacy=invoke(0,modern.preparations);
assert.equal(legacy.preparations.length,0,'policy-only request repeated preparation');
assert(legacy.compilations.every(c=>c.targetEquivalencePolicy===0&&c.targetFamily===''));
assert(legacy.facts.every(b=>b.symbols.every(s=>s.declarationKey==='')));
assert(legacy.reports.every(r=>!Object.hasOwn(r.capabilities,'target_equivalence')));
for(const c of modern.compilations){
  const previous=legacy.compilations.find(o=>o.id===c.id);assert(previous);
  assert.notEqual(c.inputFingerprint,previous.inputFingerprint,'policy missing from own snapshot');
  assert.deepEqual(symbols(modern,c).map(s=>s.id).sort(),symbols(legacy,previous).map(s=>s.id).sort());
  if(c.references.length)assert.notEqual(c.references[0].inputFingerprint,previous.references[0].inputFingerprint,'policy missing from referenced snapshot');
}
const unsupported=invoke(2);
assert.notEqual(unsupported.result.status,0);assert.equal(unsupported.messages.length,0);
assert.match(unsupported.result.stderr.toString(),/unsupported target equivalence policy 2/);
assert(!fs.readdirSync(workspace,{recursive:true}).some(p=>p.endsWith('.dll')),'worker built a project assembly');
const files=['proto/analysis.proto','providers/dotnet/Program.cs','providers/dotnet/ProjectLinks.cs','providers/dotnet/TargetPeers.cs','tests/csharp_target_grouping.mjs'];
const summary={passed:true,run,profile,worker,workerSha256:hash(worker),sourceRevision:spawnSync('git',['rev-parse','HEAD'],{cwd:root,encoding:'utf8'}).stdout.trim(),
  sourceSha256:Object.fromEntries(files.map(p=>[p,hash(path.join(root,p))])),
  checks:['families follow evaluated project contexts','exact symbols with equivalent declaration keys','assembly differences remain distinct','external definition pointers and recursive witnesses','unproven generated origins are partial','legacy omission and unchanged preparation','unsupported policy rejected before facts'],
  limits:['Direct worker only; no assembled query/native/installed or performance certification.']};
write(path.join(run,'summary.json'),JSON.stringify(summary,null,2));console.log(JSON.stringify(summary,null,2));
