"""Real package/MSBuild preparation, partial coverage, and save deduplication."""
from fixture_trust import grant_fixture
import atexit
import json
import os
from pathlib import Path
import subprocess
import shutil
import time

ROOT = Path(__file__).resolve().parents[1]
RUN = ROOT / '.artifacts' / 'prep' / hex(time.time_ns())[-10:]
WORKSPACE = RUN / 'workspace'
WORKSPACE.mkdir(parents=True)
PROFILE = os.environ.get('KNOW_CODE_BUILD_PROFILE', 'debug')
HOST = ROOT / f'target/{PROFILE}/know-code'
ENV = dict(os.environ, KNOW_CODE_DATA_DIR=str(RUN / 'data'), DOTNET_CLI_TELEMETRY_OPTOUT='1', DOTNET_NOLOGO='1')
ENV['KNOW_CODE_QUERY_SCOPE']=str(RUN/'query-admission')
ENV['DOTNET_CLI_WORKLOAD_UPDATE_NOTIFY_DISABLE']='0'  # The adapter must override inherited SDK advertising.


def run(args, check=True):
    p = subprocess.run([str(a) for a in args], cwd=ROOT, env=ENV, capture_output=True, text=True, timeout=120)
    assert not check or p.returncode == 0, (args, p.stdout, p.stderr)
    return p


atexit.register(lambda: run([HOST, 'coordinator', 'stop'], check=False))
package = RUN / 'dependency'
package.mkdir()
(package / 'package.json').write_text(json.dumps({'name': 'fixture-dep', 'version': '1.0.0', 'types': 'index.d.ts',
                                                'scripts': {'postinstall': 'node -e "require(\'fs\').writeFileSync(\'script-ran\', \'yes\')"'}}))
(package / 'index.d.ts').write_text('export declare const dependency: number;\n')
(WORKSPACE / 'package.json').write_text(json.dumps({'name': 'preparation-fixture', 'private': True,
                                                   'dependencies': {'fixture-dep': f'file:{package}'}}))
(WORKSPACE / 'tsconfig.json').write_text(json.dumps({'compilerOptions': {'target': 'ES2022', 'module': 'NodeNext', 'skipLibCheck': True}, 'include': ['src/*.ts']}))
(WORKSPACE / 'src').mkdir()
source = WORKSPACE / 'src/value.ts'
source.write_text('import {dependency} from "fixture-dep"; export const configuredValue = dependency;\n')
loose = WORKSPACE / 'loose.ts'
loose.write_text('export const looseValue = 1;\n')
(WORKSPACE / 'Directory.Build.props').write_text('''<Project><PropertyGroup>
<UseArtifactsOutput>true</UseArtifactsOutput>
<ArtifactsPath>$(MSBuildThisFileDirectory).compiler-output</ArtifactsPath>
</PropertyGroup><Target Name="CountRestores" BeforeTargets="Restore">
<Error Condition="'$(MSBUILDDISABLENODEREUSE)' != '1'" Text="Restore enabled reusable MSBuild nodes" />
<Error Condition="'$(DOTNET_CLI_USE_MSBUILD_SERVER)' != '0'" Text="Restore enabled the MSBuild server" />
<Error Condition="'$(DOTNET_CLI_WORKLOAD_UPDATE_NOTIFY_DISABLE)' != 'true'" Text="Restore enabled workload update advertising" />
<WriteLinesToFile File="$(MSBuildProjectExtensionsPath)restore-count.txt" Lines="restore" Overwrite="false" />
</Target></Project>''')
cs = WORKSPACE / 'csharp'
cs.mkdir()
project = cs / 'App.csproj'
project_text = '<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><TargetFrameworks>net10.0;netstandard2.1</TargetFrameworks></PropertyGroup><ItemGroup><Compile Remove="obj/Derived.cs" /><Compile Include="obj/Derived.cs" /></ItemGroup></Project>'
project.write_text(project_text)
(cs / 'obj').mkdir()
derived = cs / 'obj/Derived.cs'
derived.write_text('public class Derived { public const int DerivedBefore = 1; }\n')
cs_source = cs / 'Value.cs'
cs_source.write_text('public class CSharpValue { public int Value => 1; }\n')
grant_fixture(HOST,WORKSPACE,ENV,RUN,RUN/'fixture-trust.jsonl')
registration = json.loads(run([HOST, 'watch', WORKSPACE]).stdout)
DATABASE = Path(registration['database'])
seen_stages = set()


def status():
    s = json.loads(run([HOST, 'coordinator', 'status']).stdout)['workspaces'][0]
    if s.get('progress'):
        seen_stages.add(s['progress']['stage'])
    return s


def settled(after=0):
    until = time.monotonic() + 90
    last = None
    while time.monotonic() < until:
        last = status()
        assert last['phase'] != 'failed', last
        if last['phase'] == 'idle' and last['generation'] > after:
            return last
        time.sleep(.05)
    raise AssertionError(last)


def query(name):
    return json.loads(run([HOST, 'query', DATABASE, json.dumps({'op': 'symbols', 'name': name, 'coverage_detail':'full'})]).stdout)['result']


initial = settled()
assert {'restoring', 'analyzing'} <= seen_stages, seen_stages
assert query('configuredValue')['symbols'] and query('looseValue')['symbols']
assert all(not location['generated'] for row in query('DerivedBefore')['symbols'] for location in row['locations']), 'an arbitrary obj path was labeled generated'
assert any(c['inferred'] and c['analysis_options'].get('strict') and 'inferredRoot' in c['analysis_options'] for c in query('looseValue')['coverage'])
assert all(c['status'] == 'complete' for c in query('configuredValue')['coverage'])
assert not (WORKSPACE / 'package-lock.json').exists(), 'restoration synthesized a source lockfile'
assert not (package / 'script-ran').exists(), 'install lifecycle script ran'
counter = WORKSPACE / '.compiler-output/obj/App/restore-count.txt'
assert counter.exists() and not (cs / 'obj/project.assets.json').exists()
count = counter.read_text()
installed = DATABASE.with_suffix('.preparation.json').stat().st_mtime_ns
source.write_text(source.read_text() + 'export const savedValue = 2;\n')
saved = settled(initial['generation'])
cs_source.write_text(cs_source.read_text().replace('=> 1', '=> 2'))
saved = settled(saved['generation'])
assert counter.read_text() == count, 'ordinary source save repeated centralized MSBuild restore'
assert DATABASE.with_suffix('.preparation.json').stat().st_mtime_ns == installed, 'ordinary save repeated dependency preparation'
assert query('savedValue')['symbols']

derived.write_text(derived.read_text().replace('DerivedBefore', 'DerivedAfter'))
saved = settled(saved['generation'])
assert query('DerivedAfter')['symbols'] and not query('DerivedBefore').get('symbols')
assert counter.read_text() == count, 'saved compiler-selected excluded source repeated restore'

# A dependency install can generate thousands of irrelevant watcher events. It must
# neither force another install nor publish a replacement for unchanged inputs.
flood = WORKSPACE / 'node_modules/.event-flood'
flood.mkdir()
for i in range(6000): (flood / f'{i}.json').write_text('{}')
shutil.rmtree(flood)
time.sleep(3)
quiet = settled()
assert quiet['generation'] == saved['generation'], 'dependency event flood triggered redundant analysis'
assert DATABASE.with_suffix('.preparation.json').stat().st_mtime_ns == installed, 'dependency event flood repeated restore'

# Missing local dependencies fail quickly without network; valid compiler facts still publish.
manifest = json.loads((WORKSPACE / 'package.json').read_text())
manifest['dependencies']['absent-fixture'] = f'file:{RUN / "does-not-exist.tgz"}'
(WORKSPACE / 'package.json').write_text(json.dumps(manifest))
partial = settled(saved['generation'])
coverage = query('configuredValue')['coverage']
assert any(d['code'] == 'dependency_restore_failed' and 'ENOENT' in d['message']
           for c in coverage for d in c.get('diagnostics',[])), coverage
checkpoint = DATABASE.with_suffix('.preparation.json').stat().st_mtime_ns
source.write_text(source.read_text() + 'export const afterFailure = 3;\n')
partial = settled(partial['generation'])
assert DATABASE.with_suffix('.preparation.json').stat().st_mtime_ns == checkpoint, 'failed restore repeated on save'
assert query('afterFailure')['symbols']
# An explicit refresh deliberately retries the same failed preparation.
run([HOST, 'index', WORKSPACE])
partial = settled(partial['generation'])
assert DATABASE.with_suffix('.preparation.json').stat().st_mtime_ns > checkpoint
del manifest['dependencies']['absent-fixture']
(WORKSPACE / 'package.json').write_text(json.dumps(manifest))
repaired = settled(partial['generation'])

# MSBuild errors are normally stdout: retain them and keep healthy compiler facts.
project.write_text(project_text.replace('</Project>', '<Target Name="RejectRestore" BeforeTargets="Restore"><Error Text="KnowCode restore stdout evidence" /></Target></Project>'))
partial = settled(repaired['generation'])
coverage = query('CSharpValue')['coverage']
assert any(d['code'] == 'dependency_restore_failed' and 'KnowCode restore stdout evidence' in d['message']
           for c in coverage for d in c.get('diagnostics',[])), coverage
assert query('CSharpValue')['symbols'] and query('configuredValue')['symbols']
checkpoint = DATABASE.with_suffix('.preparation.json').stat().st_mtime_ns
cs_source.write_text(cs_source.read_text().replace('=> 2', '=> 3'))
partial = settled(partial['generation'])
assert DATABASE.with_suffix('.preparation.json').stat().st_mtime_ns == checkpoint, 'failed NuGet restore repeated on save'
restores_before_repair = len(counter.read_text().splitlines())
project.write_text(project_text)
settled(partial['generation'])
assert len(counter.read_text().splitlines()) > restores_before_repair, 'healthy project configuration repair did not restore dependencies'
assert all(c['status'] == 'complete' for c in query('configuredValue')['coverage'])
summary = {'result': 'passed', 'database': str(DATABASE), 'checks': ['npm_restore_without_scripts', 'no_synthetic_lockfile',
           'mixed_configured_inferred_typescript', 'centralized_msbuild_assets', 'source_save_restore_deduplication',
           'failed_restore_deduplication', 'explicit_restore_retry', 'saved_compiler_selected_excluded_inputs', 'restore_child_server_and_advertising_policy', 'dependency_event_flood', 'restore_error_stdout', 'partial_graph_and_repair', 'visible_progress']}
(RUN / 'result.json').write_text(json.dumps(summary, indent=2) + '\n')
(ROOT / '.artifacts/prep/latest.json').write_text(json.dumps(summary, indent=2) + '\n')
print(json.dumps(summary, indent=2))
