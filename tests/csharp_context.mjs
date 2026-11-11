// Direct compiler-worker cases for the optional C# context policy. No host or
// serving database is involved; coordinator invalidation is an integration gate.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import {spawnSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import {fileURLToPath} from 'node:url';
import protobuf from 'protobufjs';

const root = fileURLToPath(new URL('..', import.meta.url));
const profile = process.env.KNOW_CODE_BUILD_PROFILE ?? 'debug';
const worker = path.join(root, `providers/dotnet/bin/${profile === 'debug' ? 'Debug' : 'Release'}/net10.0/KnowCode.Provider.DotNet.dll`);
const run = path.join(root, '.artifacts', 'csharp-context', Date.now().toString(16));
fs.mkdirSync(run, {recursive: true});
const schema = await protobuf.load(path.join(root, 'proto/analysis.proto'));
const Envelope = schema.lookupType('knowcode.Envelope');
const checks = [];
function write(name, contents) {fs.mkdirSync(path.dirname(name), {recursive:true}); fs.writeFileSync(name, contents);}
function invoke(workspace, projects, settings, previous = []) {
  const fingerprint = createHash('sha256');
  for (const name of ['Directory.Build.props', 'Directory.Solution.props', 'Directory.Solution.targets', 'SolutionInputs.props', 'App.sln', ...projects.map(p => path.relative(workspace, p))]) {
    const file = path.join(workspace, name);
    if (fs.existsSync(file)) fingerprint.update(name).update(fs.readFileSync(file));
  }
  const body = Envelope.encode(Envelope.create({protocolVersion:7,jobId:'context-test',analyze:{
    workspaceRoot:workspace, projects, preparationFingerprint:fingerprint.digest('hex'), preparations:previous,
    configuration:'LEGACY_MUST_NOT_OVERRIDE_POLICY', analysisPolicy:{version:1,csharp:settings}
  }})).finish();
  const header = Buffer.alloc(4); header.writeUInt32LE(body.length);
  const result = spawnSync('dotnet', [worker], {cwd:workspace,input:Buffer.concat([header,body]),
    maxBuffer:32*1024*1024,timeout:180000,env:{...process.env,DOTNET_CLI_TELEMETRY_OPTOUT:'1',DOTNET_NOLOGO:'1',
      MSBUILDDISABLENODEREUSE:'1',DOTNET_CLI_USE_MSBUILD_SERVER:'0'}});
  const messages = [];
  for (let offset=0; offset < result.stdout.length;) {
    assert(offset+4 <= result.stdout.length, 'truncated header');
    const size=result.stdout.readUInt32LE(offset); offset+=4;
    assert(size>0 && size<=8*1024*1024 && offset+size<=result.stdout.length, 'invalid frame');
    const message=Envelope.toObject(Envelope.decode(result.stdout.subarray(offset,offset+size)),{defaults:true,arrays:true,objects:true}); offset+=size;
    assert.equal(message.protocolVersion,7); assert.equal(message.jobId,'context-test'); messages.push(message);
  }
  const index=checks.length;
  write(path.join(run, `worker-${index}.json`), JSON.stringify({status:result.status,stderr:result.stderr.toString(),messages},null,2)+'\n');
  return {status:result.status,messages,error:result.stderr.toString(),
    compilations:messages.filter(m=>m.compilation).map(m=>({...m.compilation,options:JSON.parse(m.compilation.optionsJson)})),
    reports:messages.filter(m=>m.report).map(m=>m.report),
    preparations:messages.filter(m=>m.preparation).map(m=>m.preparation),
    symbols:messages.filter(m=>m.facts).flatMap(m=>m.facts.symbols)};
}
function success(result) {
  assert.equal(result.status,0,result.error);
  assert.equal(result.messages[0].provider.analysisPolicyVersion,1);
  assert.equal(result.messages.at(-1).complete.success,true);
  assert(result.reports.every(r=>r.status==='complete'),JSON.stringify(result.reports));
}
function unavailableContext(result, projects, code, message) {
  assert.equal(result.status,0,result.error);
  assert.equal(result.messages[0].provider.analysisPolicyVersion,1);
  assert.equal(result.messages.at(-1).complete.success,true);
  const expected=projects.map(p=>path.relative(workspace,p).replaceAll(path.sep,'/')).sort();
  assert.deepEqual(result.compilations.map(c=>c.projectPath).sort(),expected,'every requested project needs unavailable context evidence');
  assert.equal(result.reports.length,projects.length);
  assert.equal(result.preparations.length,0,'unavailable solution selection must not restore a fallback context');
  assert.equal(result.symbols.length,0,'unavailable solution selection emitted compiler symbols');
  assert(!result.messages.some(m=>m.source||m.facts),'unavailable solution selection emitted source or graph facts');
  for (const compilation of result.compilations) {
    assert.equal(compilation.language,'csharp');
    assert.equal(compilation.compilerVersion,'unavailable');
    assert.equal(compilation.target,'','unknown targets must not be fabricated');
    assert.equal(compilation.configuration,'','unavailable selection must not substitute a configuration');
    assert.equal(compilation.options.context,'unavailable');
    assert.equal(compilation.options.setup_failure,code);
    assert.equal(compilation.options.target_known,false);
    assert(!Object.hasOwn(compilation.options,'build_context'),'unavailable selection claimed an evaluated build context');
    assert(compilation.inputPaths.includes(path.join(workspace,compilation.projectPath)));
    const reports=result.reports.filter(r=>r.compilationId===compilation.id);
    assert.equal(reports.length,1,'unavailable project needs exactly one matching report');
    const report=reports[0];assert.equal(report.status,'unavailable');
    for (const capability of ['calls','dependencies','dispatch','project_links','target_equivalence'])
      assert.equal(report.capabilities[capability],'unavailable');
    assert.equal(report.diagnostics.length,1);
    const diagnostic=report.diagnostics[0];assert.equal(diagnostic.code,code);
    assert.equal(diagnostic.severity,'error');assert.equal(diagnostic.path,compilation.projectPath);
    assert.match(diagnostic.message,message);
  }
}
const workspace=path.join(run,'workspace');
write(path.join(workspace,'Directory.Build.props'),`<Project><PropertyGroup>
  <Configuration Condition="'$(Configuration)' == ''">Repository</Configuration>
  <Platform Condition="'$(Platform)' == ''">AnyCPU</Platform>
  <UseArtifactsOutput>true</UseArtifactsOutput><ArtifactsPath>$(MSBuildThisFileDirectory).compiler-output</ArtifactsPath>
  <DefineConstants Condition="'$(Configuration)' == 'Repository'">$(DefineConstants);REPOSITORY_CONTEXT</DefineConstants>
  <DefineConstants Condition="'$(Configuration)' == 'Release'">$(DefineConstants);RELEASE_CONTEXT</DefineConstants>
</PropertyGroup></Project>`);
const projects=[];
for (const [name,targets,source] of [
  ['Library','net10.0;netstandard2.1',`public static class LibraryApi {
#if RELEASE_CONTEXT
    public static int ReleaseOnly() => 1;
#elif REPOSITORY_CONTEXT
    public static int RepositoryOnly() => 2;
#else
    public static int DebugOnly() => 3;
#endif
}`],
  ['Consumer','net10.0','public static class ConsumerApi { public static int Read() => LibraryApi.ReleaseOnly(); }'],
  ['Outside','net10.0','public static class OutsideApi { public static int Read() => 1; }']
]) {
  const project=path.join(workspace,name,`${name}.csproj`); projects.push(project);
  write(project,`<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><TargetFrameworks>${targets}</TargetFrameworks></PropertyGroup>
    ${name==='Consumer'?'<ItemGroup><ProjectReference Include="../Library/Library.csproj" /></ItemGroup>':''}
    <Target Name="NeverBuild" BeforeTargets="Build"><Error Text="Know Code invoked a full Build" /></Target></Project>`);
  write(path.join(path.dirname(project),'Code.cs'),source);
}
let result=invoke(workspace,[projects[0]],{}); success(result);
assert.deepEqual(result.compilations.map(c=>c.configuration),['Repository','Repository']);
assert(result.symbols.some(s=>s.name==='RepositoryOnly'&&!s.external));
assert(result.compilations.every(c=>c.options.build_context.origin==='repository_default'));
checks.push('repository_defaults_all_frameworks');
result=invoke(workspace,[projects[0]],{configuration:'Release'}); success(result);
assert(result.compilations.every(c=>c.configuration==='Release'&&c.options.build_context.origin==='workspace_override'));
assert(result.symbols.some(s=>s.name==='ReleaseOnly'&&!s.external));
checks.push('explicit_override_beats_legacy_string');

const guids=['{11111111-1111-1111-1111-111111111111}','{22222222-2222-2222-2222-222222222222}'];
function solution(libraryConfiguration='Release',libraryPlatform='x64') {
  return `Microsoft Visual Studio Solution File, Format Version 12.00
# Visual Studio Version 17
${['Library','Consumer'].map((name,i)=>`Project("{FAE04EC0-301F-11D3-BF4B-00C04F79EFBC}") = "${name}", "${name}\\${name}.csproj", "${guids[i]}"\nEndProject`).join('\n')}
Global
  GlobalSection(SolutionConfigurationPlatforms) = preSolution
    Debug|Any CPU = Debug|Any CPU
    Release|Any CPU = Release|Any CPU
  EndGlobalSection
  GlobalSection(ProjectConfigurationPlatforms) = postSolution
    ${guids[0]}.Debug|Any CPU.ActiveCfg = ${libraryConfiguration}|${libraryPlatform}
    ${guids[0]}.Debug|Any CPU.Build.0 = ${libraryConfiguration}|${libraryPlatform}
    ${guids[1]}.Debug|Any CPU.ActiveCfg = Debug|Any CPU
    ${guids[1]}.Debug|Any CPU.Build.0 = Debug|Any CPU
    ${guids[0]}.Release|Any CPU.ActiveCfg = Release|Any CPU
    ${guids[0]}.Release|Any CPU.Build.0 = Release|Any CPU
    ${guids[1]}.Release|Any CPU.ActiveCfg = Release|Any CPU
    ${guids[1]}.Release|Any CPU.Build.0 = Release|Any CPU
  EndGlobalSection
EndGlobal
`;
}
const selected=path.join(workspace,'App.sln');write(selected,solution());
result=invoke(workspace,projects,{}); success(result);
assert.equal(result.compilations.length,4);
const library=result.compilations.filter(c=>c.projectPath==='Library/Library.csproj');
assert(library.every(c=>c.configuration==='Release'&&c.options.platform==='x64'));
const consumer=result.compilations.find(c=>c.projectPath==='Consumer/Consumer.csproj');
assert.equal(consumer.configuration,'Debug'); assert.equal(consumer.options.platform,'AnyCPU');
const outside=result.compilations.find(c=>c.projectPath==='Outside/Outside.csproj');
assert.equal(outside.configuration,'Repository'); assert.equal(outside.options.build_context.origin,'repository_default');
const witness=consumer.references.find(r=>r.projectPath==='Library/Library.csproj');
const body=library.find(c=>c.target==='net10.0');
assert.equal(witness?.compilationId,body.id); assert.equal(witness.inputFingerprint,body.inputFingerprint);
assert(result.symbols.some(s=>s.definitionCompilationId===body.id&&s.definitionFingerprint===body.inputFingerprint));
assert(result.compilations.every(c=>c.inputPaths.includes(selected)));
assert(consumer.inputPaths.includes(path.join(workspace,'Directory.Build.props')));
checks.push('solution_mapping_platform_outside_project_and_exact_reference');
const savedPreparations=result.preparations;
result=invoke(workspace,projects,{},savedPreparations); success(result);
assert.equal(result.preparations.length,0,'unchanged contexts reran restoration');
checks.push('unchanged_context_preparation_reused');

write(path.join(workspace,'Other.sln'),solution());
result=invoke(workspace,projects,{});
unavailableContext(result,projects,'solution_selection_required',/multiple solution files require one workspace solution_path selection; no solution contexts were expanded/);
checks.push('multiple_solutions_require_selection');
result=invoke(workspace,projects,{solutionPath:selected}); success(result);
assert.equal(result.compilations.length,4);
checks.push('explicit_solution_selects_one_context');
result=invoke(workspace,projects,{solutionPath:selected,configuration:'Release'});success(result);
assert(result.compilations.every(c=>c.options.build_context.solution_configuration==='Release'));
assert(result.compilations.every(c=>c.configuration==='Release'&&c.options.platform==='AnyCPU'));
assert.equal(result.compilations.find(c=>c.projectPath==='Outside/Outside.csproj').options.build_context.origin,'workspace_override');
checks.push('explicit_override_selects_solution_context_and_direct_project_defaults');
result=invoke(workspace,projects,{solutionPath:path.join(workspace,'missing.sln')});
unavailableContext(result,projects,'solution_unavailable',/selected solution is unavailable/);
assert(result.reports.every(r=>r.diagnostics[0].message.includes(path.join(workspace,'missing.sln'))),'diagnostics must identify the unavailable explicit selection');
checks.push('missing_selected_solution_no_substitution');

write(selected,solution('Debug','Any CPU'));
result=invoke(workspace,projects,{solutionPath:selected},savedPreparations);
assert.equal(result.status,0,result.error);
assert(result.compilations.filter(c=>c.projectPath==='Library/Library.csproj').every(c=>c.configuration==='Debug'&&c.options.platform==='AnyCPU'));
assert(result.symbols.some(s=>s.name==='DebugOnly'&&!s.external));
assert(result.reports.some(r=>r.status==='partial'),'compiler error should publish a partial graph');
assert(result.preparations.length>0,'mapping change did not invalidate preparation');
checks.push('changed_mapping_changes_compiler_facts_and_reports_partial');
write(selected,solution());
write(path.join(workspace,'App.slnf'),JSON.stringify({solution:{path:'App.sln',projects:['Consumer/Consumer.csproj']}}));
result=invoke(workspace,projects,{solutionPath:path.join(workspace,'App.slnf')});success(result);
assert.equal(result.compilations.length,4,'solution filter suppressed indexed projects');
assert(result.compilations.every(c=>c.inputPaths.includes(path.join(workspace,'App.slnf'))));
checks.push('solution_filter_retains_outside_projects');
const migrated=spawnSync('dotnet',['sln',selected,'migrate'],{cwd:workspace,encoding:'utf8',timeout:30000});
assert.equal(migrated.status,0,migrated.stderr);
result=invoke(workspace,projects,{solutionPath:path.join(workspace,'App.slnx')});success(result);
assert(result.compilations.filter(c=>c.projectPath==='Library/Library.csproj').every(c=>c.configuration==='Release'&&c.options.platform==='x64'));
checks.push('sdk_parsed_slnx_preserves_mapping');
const props=path.join(workspace,'Directory.Build.props');
fs.writeFileSync(props,fs.readFileSync(props,'utf8').replace('</Project>', '<Import Project="$(SolutionDir)SolutionInputs.props" Condition="'+"'$(SolutionDir)' != ''"+'" /></Project>'));
const imported=path.join(workspace,'SolutionInputs.props');
write(imported,'<Project><PropertyGroup><DefineConstants>$(DefineConstants);SOLUTION_INPUT_ONE</DefineConstants></PropertyGroup></Project>');
const librarySource=path.join(workspace,'Library/Code.cs');
fs.appendFileSync(librarySource, '\n#if SOLUTION_INPUT_ONE\npublic class InitialSolutionMarker {}\n#else\npublic class ChangedSolutionMarker {}\n#endif\n');
result=invoke(workspace,projects,{solutionPath:selected});success(result);
assert(result.symbols.some(s=>s.name==='InitialSolutionMarker'&&!s.external));
assert(result.compilations.filter(c=>c.projectPath==='Library/Library.csproj').every(c=>c.inputPaths.includes(imported)));
const beforeImportChange=result.preparations;
write(imported,'<Project><PropertyGroup><DefineConstants>$(DefineConstants);SOLUTION_INPUT_TWO</DefineConstants></PropertyGroup></Project>');
result=invoke(workspace,projects,{solutionPath:selected},beforeImportChange);success(result);
assert(result.symbols.some(s=>s.name==='ChangedSolutionMarker'&&!s.external));
assert(!result.symbols.some(s=>s.name==='InitialSolutionMarker'&&!s.external));
assert(result.preparations.length>0);
checks.push('solution_relative_imports_and_changed_input_facts');
const solutionProps=path.join(workspace,'Directory.Solution.props');
write(solutionProps,`<Project><PropertyGroup><Configuration Condition="'$(Configuration)' == ''">Release</Configuration>
  <Platform Condition="'$(Platform)' == ''">Any CPU</Platform></PropertyGroup>
  <Target Name="DoNotExecuteSolutionTargets" BeforeTargets="Build;CoreCompile"><Error Text="Solution planning executed targets" /></Target></Project>`);
result=invoke(workspace,projects,{solutionPath:selected});success(result);
assert(result.compilations.every(c=>c.options.build_context.solution_configuration==='Release'));
assert(result.compilations.filter(c=>c.projectPath!=='Outside/Outside.csproj').every(c=>c.configuration==='Release'&&c.options.platform==='AnyCPU'));
assert.equal(result.compilations.find(c=>c.projectPath==='Outside/Outside.csproj').configuration,'Repository');
assert(result.compilations.every(c=>c.inputPaths.includes(solutionProps)));
result=invoke(workspace,projects,{solutionPath:selected,configuration:'Debug'});success(result);
assert(result.compilations.every(c=>c.options.build_context.solution_configuration==='Debug'));
checks.push('solution_props_defaults_without_requesting_build_targets');
fs.unlinkSync(solutionProps);
const solutionTargets=path.join(workspace,'Directory.Solution.targets');
write(solutionTargets,'<Project><PropertyGroup><Configuration>Release</Configuration><Platform>Any CPU</Platform></PropertyGroup><Target Name="NeverRun" BeforeTargets="Build;CoreCompile"><Error Text="Solution targets executed" /></Target></Project>');
result=invoke(workspace,projects,{solutionPath:selected});
assert.equal(result.status,0,result.error);
assert(result.compilations.every(c=>c.options.build_context.solution_configuration==='Release'));
assert(result.compilations.every(c=>c.inputPaths.includes(solutionTargets)));
checks.push('solution_targets_properties_without_requesting_build_targets');
fs.unlinkSync(solutionTargets);
write(solutionTargets,`<Project InitialTargets="RecordMetadataHook"><Target Name="RecordMetadataHook">
  <WriteLinesToFile File="$(SolutionDir)metadata-hook.txt" Lines="metadata" Overwrite="false" /></Target>
  <Target Name="NeverBuild" BeforeTargets="Build;CoreCompile"><Error Text="Planner requested build" /></Target></Project>`);
result=invoke(workspace,projects,{solutionPath:selected});success(result);
assert.equal(fs.readFileSync(path.join(workspace,'metadata-hook.txt'),'utf8').trim(),'metadata');
checks.push('normal_sdk_initial_metadata_hook_is_visible_not_a_full_build');
fs.unlinkSync(solutionTargets);
const outsideProject=projects[2];
fs.writeFileSync(outsideProject,fs.readFileSync(outsideProject,'utf8').replace('</Project>', '<ItemGroup><ProjectReference Include="../Library/Library.csproj" /></ItemGroup></Project>'));
write(path.join(workspace,'Outside/Code.cs'),'public static class OutsideApi { public static int Read() => LibraryApi.RepositoryOnly(); }');
result=invoke(workspace,projects,{solutionPath:selected});
assert.equal(result.status,0,result.error);
const outsideContext=result.compilations.find(c=>c.projectPath==='Outside/Outside.csproj');
assert.equal(outsideContext.configuration,'Repository');
assert(result.symbols.some(s=>s.name==='RepositoryOnly'&&s.external&&!s.definitionCompilationId));
assert.equal(outsideContext.references.length,0,'outside context was linked to incompatible selected solution body');
assert(result.reports.find(r=>r.compilationId===outsideContext.id).diagnostics.some(d=>d.message.includes('outside the indexed workspace contexts')));
checks.push('outside_caller_preserves_compiler_context_without_fabricated_body_link');
assert(!fs.existsSync(path.join(workspace,'Library/bin')),'analysis ran a full build');
checks.push('no_full_build');
const sourceRevision=spawnSync('git',['rev-parse','HEAD'],{cwd:root,encoding:'utf8'}).stdout.trim();
const sourceFiles=['proto/analysis.proto','providers/dotnet/BuildContexts.cs','providers/dotnet/Program.cs',
  'providers/dotnet/ProjectCatalog.cs','providers/dotnet/ProjectLinks.cs','providers/dotnet/SdkMetadata.cs',
  'tests/csharp_context.mjs','tests/sdk-metadata.mjs'].map(file=>({path:file,sha256:createHash('sha256').update(fs.readFileSync(path.join(root,file))).digest('hex')}));
const summary={result:'passed',profile,checks,run,worker,sourceRevision,
  workerSha256:createHash('sha256').update(fs.readFileSync(worker)).digest('hex'),sourceFiles};
write(path.join(run,'result.json'),JSON.stringify(summary,null,2)+'\n');
write(path.join(root,'.artifacts/csharp-context/latest.json'),JSON.stringify(summary,null,2)+'\n');
console.log(JSON.stringify(summary,null,2));
