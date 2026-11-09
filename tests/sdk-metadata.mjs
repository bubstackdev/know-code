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
assert.deepEqual(result.targets.map(t => [t.target, t.identity.AssemblyVersion]), [['net10.0', '1.0.0.0'], ['netstandard2.1', '2.0.0.0']]);
assert.deepEqual(evaluate(true).targets.map(t => t.identity.AssemblyVersion), ['2.0.0.0', '2.0.0.0']);
const mapped = evaluate(true, {configuration:'Release', platform:'x64'});
assert.equal(mapped.definition.Configuration, 'Release'); assert.equal(mapped.definition.Platform, 'x64');
assert.ok(mapped.targets.every(t => t.identity.Configuration === 'Release' && t.identity.Platform === 'x64'));
assert.equal(evaluate(false, {configuration:'Debug', platform:'AnyCPU'}).definition.Platform, 'AnyCPU');
assert.ok(JSON.parse(mapped.definition.KnowCodeInputPaths).includes(props));
const summary = { result: 'passed', build_profile: profile, checks: ['planning_does_not_run_metadata_hooks', 'changed_imports_between_targets', 'fresh_project_session', 'no_compilation_target', 'mapped_configuration_platform', 'import_inputs_reported'] };
fs.writeFileSync(path.join(directory, 'result.json'), JSON.stringify(summary, null, 2) + '\n');
console.log(JSON.stringify(summary, null, 2));
