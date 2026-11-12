// SDK metadata hooks may change imports between target contexts. No builds run.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import protobuf from 'protobufjs';

const root = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const profile = process.env.KNOW_CODE_BUILD_PROFILE ?? 'debug';
const configuration = profile[0].toUpperCase() + profile.slice(1);
const directory = path.join(root, '.artifacts', 'sdk-meta', Date.now().toString(16));
fs.mkdirSync(directory, { recursive: true });
const project = path.join(directory, 'Example.csproj');
const props = path.join(directory, 'Version.props');
fs.writeFileSync(props, '<Project><PropertyGroup><ProbeAssemblyVersion>1.0.0.0</ProbeAssemblyVersion></PropertyGroup></Project>');
fs.writeFileSync(project, `<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup><TargetFrameworks>net10.0;netstandard2.1</TargetFrameworks></PropertyGroup>
  <Import Project="Version.props"/>
  <Target Name="Identity" BeforeTargets="GetAssemblyVersion">
    <PropertyGroup><AssemblyVersion>$(ProbeAssemblyVersion)</AssemblyVersion></PropertyGroup>
    <WriteLinesToFile Condition="'$(TargetFramework)' == 'net10.0'" File="Version.props" Overwrite="true"
      Lines="&lt;Project&gt;&lt;PropertyGroup&gt;&lt;ProbeAssemblyVersion&gt;2.0.0.0&lt;/ProbeAssemblyVersion&gt;&lt;/PropertyGroup&gt;&lt;/Project&gt;"/>
  </Target>
  <Target Name="RejectCompile" BeforeTargets="CoreCompile"><Error Text="Metadata invoked compilation"/></Target>
</Project>`);
function dotnet(args, options = {}) {
  const result = spawnSync('dotnet', args, { cwd: root, maxBuffer: 1024 * 1024, timeout: 60000, ...options });
  assert.equal(result.status, 0, result.stderr?.toString() ?? String(result.error));
  return result.stdout;
}
const selected = dotnet(['--version']).toString().trim();
const line = dotnet(['--list-sdks']).toString().split('\n').find(s => s.startsWith(`${selected} [`));
const sdkPath = path.join(line.slice(line.indexOf('[') + 1, line.lastIndexOf(']')), selected);
const schema = await protobuf.load(path.join(root, 'proto/analysis.proto'));
const Envelope = schema.lookupType('knowcode.Envelope');
function evaluate(includeIdentity, overrides = {}) {
  const body = Envelope.encode(Envelope.create({ protocolVersion: 7, jobId: 'metadata-test',
    sdkMetadataRequest: { sdkPath, projectPath: project, configuration: 'Debug', includeIdentity, ...overrides } })).finish();
  const header = Buffer.alloc(4); header.writeUInt32LE(body.length);
  const output = dotnet([path.join(root, `providers/dotnet/bin/${configuration}/net10.0/KnowCode.Provider.DotNet.dll`), '--sdk-metadata'],
    { cwd: directory, input: Buffer.concat([header, body]) });
  let offset = 0; let metadata; let complete = false;
  while (offset < output.length) {
    const size = output.readUInt32LE(offset); offset += 4;
    const message = Envelope.decode(output.subarray(offset, offset + size)); offset += size;
    assert.equal(message.protocolVersion, 7); assert.equal(message.jobId, 'metadata-test');
    if (message.sdkMetadataResult) metadata = message.sdkMetadataResult;
    if (message.complete) complete = message.complete.success;
  }
  assert.ok(complete && metadata); return metadata;
}
const plan = evaluate(false);
assert.equal(plan.targets.length, 2);
assert.ok(plan.targets.every(t => !Object.keys(t.identity).length && path.isAbsolute(t.assetsFile)));
assert.ok(fs.readFileSync(props, 'utf8').includes('1.0.0.0'), 'Planning ran identity hooks');
const result = evaluate(true);
const generatedDirectories=JSON.parse(result.definition.KnowCodeGeneratedDirectories);
assert.deepEqual(Object.keys(generatedDirectories).sort(),['net10.0','netstandard2.1']);
assert(Object.values(generatedDirectories).every(paths=>paths.length>0&&paths.every(path.isAbsolute)));
assert(result.targets.every(t=>!Object.hasOwn(t.identity,'KnowCodeGeneratedDirectories')),
  'source-origin metadata leaked into compiler identity/global properties');
assert.deepEqual(result.targets.map(t => [t.target, t.identity.AssemblyVersion]), [['net10.0', '1.0.0.0'], ['netstandard2.1', '2.0.0.0']]);
assert.deepEqual(evaluate(true).targets.map(t => t.identity.AssemblyVersion), ['2.0.0.0', '2.0.0.0']);
const mapped = evaluate(true, {configuration:'Release', platform:'x64'});
assert.equal(mapped.definition.Configuration, 'Release'); assert.equal(mapped.definition.Platform, 'x64');
assert.ok(mapped.targets.every(t => t.identity.Configuration === 'Release' && t.identity.Platform === 'x64'));
assert.equal(evaluate(false, {configuration:'Debug', platform:'AnyCPU'}).definition.Platform, 'AnyCPU');
assert.ok(JSON.parse(mapped.definition.KnowCodeInputPaths).includes(props));
const signalsProject=path.join(directory,'Signals.csproj');
const xml=value=>value.replaceAll('&','&amp;').replaceAll('<','&lt;').replaceAll('>','&gt;');
for(const [signals,kind] of [[{},'unknown'],[{IsTestProject:''},'unknown'],[{IsTestProject:'maybe'},'unknown'],
  [{IsTestProject:'true'},'test'],[{IsTestProject:'false'},'non_test'],
  [{IsTestingPlatformApplication:'true'},'test'],[{IsTestingPlatformApplication:'false'},'unknown'],
  [{IsTestProject:'false',IsTestingPlatformApplication:'true'},'unknown'],
  [{IsTestProject:'true',IsTestingPlatformApplication:'false'},'test'],
  [{IsTestProject:'false',IsTestingPlatformApplication:'false'},'non_test'],
  [{IsTestProject:'😀'.repeat(200)},'unknown']]) {
  fs.writeFileSync(signalsProject,`<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><TargetFramework>net10.0</TargetFramework>
    ${Object.entries(signals).map(([key,value])=>`<${key}>${xml(value)}</${key}>`).join('')}</PropertyGroup>
    <Target Name="NeverCompile" BeforeTargets="Build;CoreCompile"><Error Text="Classification invoked compilation"/></Target></Project>`);
  const target=evaluate(true,{projectPath:signalsProject}).targets[0],classification=target.testClassification;
  assert.equal(classification.policyVersion,1);assert.equal(classification.kind,kind,JSON.stringify(signals));
  assert.equal(classification.evidence.length,Object.keys(signals).length);
  assert(classification.evidence.every(e=>e.basis==='msbuild_property'&&!e.inferred&&Buffer.byteLength(e.value)<=512&&Buffer.byteLength(e.path)<=4096));
  assert(Object.keys(target.identity).every(key=>!key.startsWith('IsTest')&&key!=='IsTestingPlatformApplication'),'classification leaked into identity globals');
  if(signals.IsTestProject?.includes('😀')) assert(classification.evidence[0].value.endsWith('...'),'oversized UTF-8 evidence not explicitly abbreviated');
}
fs.writeFileSync(signalsProject,`<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><TargetFrameworks>net10.0;netstandard2.1</TargetFrameworks><IsTestProject>false</IsTestProject></PropertyGroup>
  <Target Name="LateSignal" BeforeTargets="GetAssemblyVersion"><PropertyGroup><IsTestProject Condition="'$(TargetFramework)'=='net10.0'">true</IsTestProject></PropertyGroup></Target>
  <Target Name="NeverCompile" BeforeTargets="Build;CoreCompile"><Error Text="Classification invoked compilation"/></Target></Project>`);
const signalPlan=evaluate(false,{projectPath:signalsProject});assert(signalPlan.targets.every(t=>!t.testClassification),'planning classified outer/pre-hook state');
const finalSignals=evaluate(true,{projectPath:signalsProject});
assert.deepEqual(finalSignals.targets.map(t=>t.testClassification.kind),['test','non_test']);
assert.deepEqual(finalSignals.targets[0].identity,finalSignals.targets[1].identity,'classification changed identity/batching metadata');
assert(finalSignals.targets.every(t=>t.testClassification.evidence.some(e=>e.value.includes('stage=post_GetAssemblyVersion')&&e.value.includes(`target=${t.target}`))));
const summary = { result: 'passed', build_profile: profile, checks: ['planning_does_not_run_metadata_hooks', 'changed_imports_between_targets', 'fresh_project_session', 'no_compilation_target', 'mapped_configuration_platform', 'import_inputs_reported',
  'classification_frozen_truth_table_missing_invalid_conflict', 'UTF8_bounded_oversized_property_evidence', 'classification_after_identity_hooks_per_target', 'classification_does_not_change_identity_globals_or_batching'] };
fs.writeFileSync(path.join(directory, 'result.json'), JSON.stringify(summary, null, 2) + '\n');
console.log(JSON.stringify(summary, null, 2));
