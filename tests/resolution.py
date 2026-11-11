"""Real Roslyn/MSBuild target negotiation without building dependency assemblies."""
import atexit
import json
import os
from pathlib import Path
import shutil
import subprocess
import time

ROOT = Path(__file__).resolve().parents[1]
RUN = ROOT / '.artifacts/resolve' / hex(time.time_ns())[-10:]
WORKSPACE = RUN / 'workspace'
WORKSPACE.mkdir(parents=True)
PROFILE = os.environ.get('KNOW_CODE_BUILD_PROFILE', 'debug')
HOST = ROOT / f'target/{PROFILE}/know-code'
ENV = dict(os.environ, KNOW_CODE_DATA_DIR=str(RUN / 'data'), DOTNET_CLI_TELEMETRY_OPTOUT='1', DOTNET_NOLOGO='1')
ENV['KNOW_CODE_QUERY_SCOPE']=str(RUN/'query-admission')


def run(args, check=True):
    p = subprocess.run([str(a) for a in args], cwd=ROOT, env=ENV, text=True, capture_output=True, timeout=120)
    assert not check or p.returncode == 0, (args, p.stdout, p.stderr)
    return p


atexit.register(lambda: run([HOST, 'coordinator', 'stop'], check=False))
# Build only a test analyzer, outside the indexed workspace. Product analysis must
# still resolve project references without building dependency assemblies.
generator = RUN / 'generator'
generator.mkdir()
provider = ROOT / f'providers/dotnet/bin/{PROFILE.capitalize()}/net10.0'
(generator / 'Observer.csproj').write_text(f'''<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup>
<TargetFramework>net10.0</TargetFramework><ImplicitUsings>enable</ImplicitUsings>
</PropertyGroup><ItemGroup><Reference Include="Microsoft.CodeAnalysis" HintPath="{provider}/Microsoft.CodeAnalysis.dll" Private="false" />
</ItemGroup></Project>''')
(generator / 'Observer.cs').write_text('''using Microsoft.CodeAnalysis;
[Generator] public sealed class Observer : IIncrementalGenerator {
    public void Initialize(IncrementalGeneratorInitializationContext context) {
        context.RegisterSourceOutput(context.CompilationProvider, (output, compilation) => {
            var changed = compilation.References.OfType<CompilationReference>()
                .SelectMany(r => r.Compilation.SyntaxTrees)
                .Any(t => t.GetText().ToString().Contains("GENERATOR_MARKER", StringComparison.Ordinal));
            var name = changed ? "AfterComment" : "BeforeComment";
            output.AddSource("Observed.g.cs", "public static class Generated { public static int " + name + "() => 1; }");
        });
    }
}
''')
run(['dotnet', 'build', generator / 'Observer.csproj', '--configuration', 'Release', '--nologo'])
generator_dll = generator / 'bin/Release/net10.0/Observer.dll'
(WORKSPACE / 'Directory.Build.props').write_text('<Project><PropertyGroup><UseArtifactsOutput>true</UseArtifactsOutput><ArtifactsPath>$(MSBuildThisFileDirectory).compiler-output</ArtifactsPath></PropertyGroup></Project>')
for name, frameworks in [('Library', 'netstandard2.1;net8.0'), ('AConsumer', 'netstandard2.1'), ('BConsumer', 'net10.0;netstandard2.1')]:
    directory = WORKSPACE / name
    directory.mkdir()
    identity = '<Target Name="ResolveIdentity" BeforeTargets="GetAssemblyVersion" Condition="&apos;$(DesignTimeBuild)&apos; != &apos;true&apos;"><PropertyGroup><Version>' + ('8.7.0' if name == 'Library' else '2.3.0') + '</Version><AssemblyVersion>' + ('8.0.0.0' if name == 'Library' else '2.0.0.0') + '</AssemblyVersion></PropertyGroup></Target>'
    refs = '' if name == 'Library' else '<ItemGroup><ProjectReference Include="../Library/Library.csproj"><Aliases>wire</Aliases></ProjectReference></ItemGroup>'
    if name != 'Library':
        refs += f'<ItemGroup><Analyzer Include="{generator_dll}" /></ItemGroup>'
    (directory / f'{name}.csproj').write_text(f'<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><TargetFrameworks>{frameworks}</TargetFrameworks></PropertyGroup>{refs}{identity}</Project>')
(WORKSPACE / 'Library/Api.cs').write_text('''public static class LibraryApi {
#if NET8_0
    public static int Net8Only() => 8;
#else
    public static int StandardOnly() => 2;
#endif
#if DEBUG
    public static int DebugOnly() => 0;
#else
    public static int ReleaseOnly() => 1;
#endif
}
''')
(WORKSPACE / 'AConsumer/Consumer.cs').write_text('extern alias wire; using Api = wire::LibraryApi; public class AConsumer { public int Read() => Api.StandardOnly() + Api.ReleaseOnly(); public int GeneratedRead() => Generated.BeforeComment(); }')
(WORKSPACE / 'BConsumer/Consumer.cs').write_text('''extern alias wire;
using Api = wire::LibraryApi;
public class BConsumer {
    public int GeneratedRead() => Generated.BeforeComment();
#if NET10_0
    public int Read() => Api.Net8Only() + Api.ReleaseOnly();
#else
    public int Read() => Api.StandardOnly() + Api.ReleaseOnly();
#endif
}
''')
registration = json.loads(run([HOST, 'watch', WORKSPACE, '--configuration', 'Release']).stdout)
DATABASE = registration['database']
def wait(predicate):
    until = time.monotonic() + 90
    while time.monotonic() < until:
        status = json.loads(run([HOST, 'coordinator', 'status']).stdout)['workspaces'][0]
        if predicate(status):
            return status
        time.sleep(.05)
    raise AssertionError(status)


ready = wait(lambda s: s['phase'] in ('idle', 'failed'))
assert ready['phase'] == 'idle', ready


def query(request):
    request = dict(request, coverage_detail="full")
    return json.loads(run([HOST, 'query', DATABASE, json.dumps(request)]).stdout)['result']


result = query({'op': 'symbols', 'name': 'Read'})
coverage = result['coverage']
assert len(coverage) == 5 and all(c['status'] == 'complete' and c['configuration'] == 'Release' for c in coverage), coverage
by_context = {(c['project_path'], c['target']): c for c in coverage}
modern = by_context[('BConsumer/BConsumer.csproj', 'net10.0')]['analysis_options']
legacy = by_context[('AConsumer/AConsumer.csproj', 'netstandard2.1')]['analysis_options']
assert 'Version=2.0.0.0' in modern['assembly_identity'], modern
assert modern['compiler_metadata']['AssemblyVersion'] == '2.0.0.0', modern
assert all('Version=8.0.0.0' in c['analysis_options']['assembly_identity'] for c in coverage if c['project_path']=='Library/Library.csproj'), coverage
assert modern['references'] == [{'project_path': 'Library/Library.csproj', 'context': 'Library(net8.0)'}], modern
assert legacy['references'] == [{'project_path': 'Library/Library.csproj', 'context': 'Library(netstandard2.1)'}], legacy
assert query({'op': 'symbols', 'name': 'Net8Only', 'target': 'net10.0'})['symbols'], 'compatible project-reference binding was lost'
assert not query({'op': 'symbols', 'name': 'DebugOnly'}).get('symbols'), 'workspace configuration did not propagate'
assert not list(WORKSPACE.rglob('*.dll')), 'source analysis built a dependency assembly'

# A dependency comment is observable by a consumer's generator even when public
# declarations are unchanged. Reused compiler state must not cross indexing jobs.
before_generated = query({'op': 'symbols', 'name': 'BeforeComment'})['symbols']
assert len(before_generated) == 3, before_generated
library_source = WORKSPACE / 'Library/Api.cs'
saved_library = library_source.read_text()
library_source.write_text(saved_library + '// GENERATOR_MARKER\n')
ready = wait(lambda s: s['phase'] == 'idle' and s['generation'] > ready['generation'])
partial_coverage = query({'op': 'symbols', 'name': 'GeneratedRead'})['coverage']
assert all(c['status'] == ('complete' if c['project_path'].startswith('Library/') else 'partial') for c in partial_coverage), partial_coverage
after_generated = query({'op': 'symbols', 'name': 'AfterComment'})['symbols']
assert len(after_generated) == 3, after_generated
assert not query({'op': 'symbols', 'name': 'BeforeComment'}).get('symbols')
for symbol in after_generated:
    location = next(loc for loc in symbol['locations'] if loc['generated'])
    text = query({'op': 'source', 'artifact_id': location['artifact_id']})['source']['text']
    assert 'AfterComment' in text and 'BeforeComment' not in text, text
    context = query({'op':'symbol_context','symbol_id':symbol['id'],'target':symbol['target']})['contexts'][0]
    assert any(d['location']['generated'] and 'AfterComment' in d['source']['text'] for d in context['declarations']), context
library_source.write_text(saved_library)
ready = wait(lambda s: s['phase'] == 'idle' and s['generation'] > ready['generation'])
assert all(c['status'] == 'complete' for c in query({'op': 'symbols', 'name': 'Read'})['coverage'])
assert len(query({'op': 'symbols', 'name': 'BeforeComment'})['symbols']) == 3

# SDK startup-directory state must remain isolated per project invocation.
# This hook is intentionally disabled during Roslyn's design-time compilation.
startup_target = WORKSPACE / 'Directory.Build.targets'
startup_target.write_text('<Project><Target Name="CheckStartup" BeforeTargets="GetAssemblyVersion" Condition="&apos;$(DesignTimeBuild)&apos; != &apos;true&apos;"><Error Condition="&apos;$(MSBuildStartupDirectory)&apos; != &apos;$(MSBuildProjectDirectory)&apos;" Code="KCSDK002" Text="SDK startup directory crossed project scopes" /></Target></Project>')
ready = wait(lambda s: s['phase'] == 'failed' or s['phase'] == 'idle' and s['generation'] > ready['generation'])
assert ready['phase'] == 'idle', ready
assert all(c['status'] == 'complete' for c in query({'op': 'symbols', 'name': 'Read'})['coverage'])
startup_target.unlink()
ready = wait(lambda s: s['phase'] == 'idle' and s['generation'] > ready['generation'])

# SDK target errors must retain diagnostic details and the prior graph.
failure_target = WORKSPACE / 'Directory.Build.targets'
failure_target.write_text('<Project><Target Name="RejectIdentity" BeforeTargets="GetAssemblyVersion"><Error Code="KCSDK001" Text="SDK identity failure probe" /></Target></Project>')
failed = wait(lambda s: s['phase'] == 'failed')
assert failed['generation'] == ready['generation'], failed
assert 'KCSDK001' in failed['error'] and 'SDK identity failure probe' in failed['error'], failed
assert query({'op': 'symbols', 'name': 'Read'})['coverage'] == coverage
failure_target.unlink()
ready = wait(lambda s: s['phase'] == 'idle' and s['observed_epoch'] > failed['observed_epoch'] and s['published_epoch'] == s['observed_epoch'])

# Amortizing SDK selection must still notice a nested, unavailable SDK pin.
nested_pin = WORKSPACE / 'BConsumer/global.json'
nested_pin.write_text(json.dumps({'sdk': {'version': '9999.0.100', 'rollForward': 'disable'}}))
failed = wait(lambda s: s['phase'] == 'failed')
assert failed['generation'] == ready['generation'] and '9999.0.100' in failed['error'], failed
nested_pin.unlink()
ready = wait(lambda s: s['phase'] == 'idle' and s['observed_epoch'] > failed['observed_epoch'] and s['published_epoch'] == s['observed_epoch'])

# SDK evaluations must not reuse metadata from before a saved project change.
consumer = WORKSPACE / 'BConsumer/BConsumer.csproj'
consumer.write_text(consumer.read_text().replace('<AssemblyVersion>2.0.0.0', '<AssemblyVersion>3.0.0.0'))
ready = wait(lambda s: s['phase'] == 'idle' and s['generation'] > ready['generation'])
coverage = query({'op': 'symbols', 'name': 'Read'})['coverage']
assert len(coverage) == 5 and all(c['status'] == 'complete' for c in coverage), coverage
assert all('Version=3.0.0.0' in c['analysis_options']['assembly_identity'] for c in coverage if c['project_path'] == 'BConsumer/BConsumer.csproj'), coverage
assert all('Version=8.0.0.0' in c['analysis_options']['assembly_identity'] for c in coverage if c['project_path'] == 'Library/Library.csproj'), coverage
# Differing identity properties must keep separate Roslyn loads/target ownership.
consumer.write_text(consumer.read_text().replace('</PropertyGroup></Target>', '</PropertyGroup><PropertyGroup Condition="&apos;$(TargetFramework)&apos; == &apos;netstandard2.1&apos;"><AssemblyVersion>4.0.0.0</AssemblyVersion></PropertyGroup></Target>'))
ready = wait(lambda s: s['phase'] == 'idle' and s['generation'] > ready['generation'])
coverage = query({'op': 'symbols', 'name': 'Read'})['coverage']
assert len(coverage) == 5 and all(c['status'] == 'complete' for c in coverage), coverage
for context in coverage:
    if context['project_path'] == 'BConsumer/BConsumer.csproj':
        expected = '4.0.0.0' if context['target'] == 'netstandard2.1' else '3.0.0.0'
        assert context['analysis_options']['compiler_metadata']['AssemblyVersion'] == expected, context
        assert f'Version={expected}' in context['analysis_options']['assembly_identity'], context
assert not list(WORKSPACE.rglob('*.dll')), 'SDK evaluation built a dependency assembly'

# An unscoped loader warning belongs to its load/dependency closure, not every
# project loaded earlier in the same job. Shared output paths trigger this warning.
overlap = WORKSPACE / 'SharedOutputs'
overlap.mkdir()
(overlap / 'SharedOutputs.csproj').write_text('''<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup>
<TargetFrameworks>net10.0;netstandard2.1</TargetFrameworks><ArtifactsPivots>shared</ArtifactsPivots>
</PropertyGroup></Project>''')
(overlap / 'Value.cs').write_text('public class SharedOutputValue { }')
ready = wait(lambda s: s['phase'] == 'idle' and s['generation'] > ready['generation'])
mixed = query({'op': 'symbols', 'name': 'Read'})['coverage']
warned = [c for c in mixed if c['project_path'].startswith('SharedOutputs/')]
healthy = [c for c in mixed if not c['project_path'].startswith('SharedOutputs/')]
assert len(warned) == 2 and all(c['status'] == 'partial' for c in warned), mixed
assert len(healthy) == 5 and all(c['status'] == 'complete' for c in healthy), mixed
shutil.rmtree(overlap)
ready = wait(lambda s: s['phase'] == 'idle' and s['generation'] > ready['generation'])
coverage = query({'op': 'symbols', 'name': 'Read'})['coverage']
assert len(coverage) == 5 and all(c['status'] == 'complete' for c in coverage), coverage
summary = {'result': 'passed', 'database': DATABASE, 'coverage': coverage,
           'checks': ['nearest_compatible_framework', 'legacy_implementation_output', 'project_aliases',
                      'one_workspace_configuration', 'sdk_derived_assembly_identity', 'distinct_dependency_identity', 'all_declared_targets',
                      'sdk_startup_directory_isolated', 'sdk_target_error_preserves_graph', 'nested_sdk_pin_checked', 'saved_identity_change',
                      'target_specific_identity_isolated', 'no_dependency_assembly_build', 'dependency_comment_generator_refresh',
                      'loader_warning_scope']}
(RUN / 'result.json').write_text(json.dumps(summary, indent=2) + '\n')
(ROOT / '.artifacts/resolve/latest.json').write_text(json.dumps(summary, indent=2) + '\n')
print(json.dumps({k: v for k, v in summary.items() if k != 'coverage'}, indent=2))
