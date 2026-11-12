"""Compiler-selected project bodies, target compatibility and native API parity."""
from cli_assertions import check_exit
from fixture_trust import grant_fixture
import atexit
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import time

ROOT = Path(__file__).resolve().parents[1]
RUN = ROOT / '.artifacts/pl' / format(time.time_ns(),'x')[-10:]
WORKSPACE = RUN / 'workspace'
PROFILE = os.environ.get('KNOW_CODE_BUILD_PROFILE', 'debug')
HOST = ROOT / f'target/{PROFILE}/know-code'
ENV = dict(os.environ, KNOW_CODE_DATA_DIR=str(RUN / 'data'), DOTNET_CLI_TELEMETRY_OPTOUT='1', DOTNET_NOLOGO='1')
ENV['KNOW_CODE_QUERY_SCOPE']=str(RUN/'query-admission')

def write(file, text):
    target = WORKSPACE / file
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(text)

for project, frameworks, references, code in [
    ('Leaf', 'netstandard2.1;net10.0', [], 'public static class Bottom { public static int Leaf() => 1; }'),
    ('Library', 'netstandard2.1;net10.0', ['Leaf'], 'public static class Middle { public static int Run() => Bottom.Leaf(); public static T Echo<T>(T value) => value; public static int Value => Bottom.Leaf(); }'),
    ('App', 'net10.0', ['Library'], 'public static class Top { public static int Start() => Middle.Run(); public static int GenericStart() => Middle.Echo(1); public static int AccessStart() => Middle.Value; public static int ExternalStart() => System.Math.Abs(-1); }'),
    ('LibraryStd', 'netstandard2.1', [], 'public static class StandardMiddle { public static int StandardRun() => StandardLeaf(); public static int StandardLeaf() => 1; }'),
    ('CompatibleApp', 'net10.0', ['LibraryStd'], 'public static class CompatibleTop { public static int CompatibleStart() => StandardMiddle.StandardRun(); }'),
    ('OldApp', 'netstandard2.1', ['Library'], 'public static class OldTop { public static int OldStart() => Middle.Run(); }')]:
    refs = ''.join(f'<ProjectReference Include="../{r}/{r}.csproj" />' for r in references)
    write(f'cs/{project}/{project}.csproj', f'<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><TargetFrameworks>{frameworks}</TargetFrameworks><LangVersion>latest</LangVersion></PropertyGroup><ItemGroup>{refs}</ItemGroup></Project>')
    write(f'cs/{project}/Code.cs', code)
for project, references, code in [
    ('leaf', [], 'export function leaf() { return 1; }'),
    ('library', ['leaf'], "import {leaf} from '../leaf/code.js'; export function middle() { return leaf(); } export class Access { constructor() { leaf(); } get value() { return leaf(); } }"),
    ('app', ['library'], "import {middle, Access} from '../library/code.js'; export function start() { return middle(); } export function construct() { return new Access(); } export function read(a: Access) { return a.value; }")]:
    write(f'ts/{project}/tsconfig.json', json.dumps({'compilerOptions': {'composite': True, 'strict': True, 'module': 'NodeNext', 'moduleResolution': 'NodeNext', 'target': 'ES2022', 'skipLibCheck': True}, 'files': ['code.ts'], 'references': [{'path': f'../{r}'} for r in references]}))
    write(f'ts/{project}/code.ts', code)

def run(args, check=True):
    p = subprocess.run([str(a) for a in args], cwd=ROOT, env=ENV, text=True, capture_output=True, timeout=180)
    check_exit(p,args,check)
    return p

atexit.register(lambda: run([HOST, 'coordinator', 'stop'], False))
grant_fixture(HOST,WORKSPACE,ENV,RUN,RUN/'fixture-trust.jsonl')
registered = json.loads(run([HOST, 'index', WORKSPACE]).stdout)
DATABASE = registered['database']

def settled(after=0):
    end = time.monotonic() + 120
    while time.monotonic() < end:
        s = json.loads(run([HOST, 'coordinator', 'status']).stdout)['workspaces'][0]
        assert s['phase'] != 'failed', s
        if s['phase'] == 'idle' and s['generation'] > after:
            return s['generation']
        time.sleep(.1)
    raise AssertionError(s)

generation = settled()

def query(request, parity=False):
    encoded = json.dumps(dict(request, time_budget_ms=5000))
    result = json.loads(run([HOST, 'query', DATABASE, encoded]).stdout)
    assert 'result' in result, result
    if parity:
        for command in (['node', ROOT / f'.artifacts/build/{PROFILE}/node/smoke.mjs', DATABASE, encoded],
                        ['dotnet', ROOT / f'bindings/dotnet/Smoke/bin/{PROFILE.capitalize()}/net10.0/Smoke.dll', DATABASE, encoded]):
            other = json.loads(run(command).stdout)
            if other != result:
                (RUN / 'parity-expected.json').write_text(json.dumps(result, indent=2))
                (RUN / 'parity-actual.json').write_text(json.dumps(other, indent=2))
                raise AssertionError(f'native query parity failed; see {RUN}/parity-*.json')
    return result['result']

def symbol(name, project, target=None, external=False):
    rows = query({'op': 'symbols', 'name': name, 'target': target})['symbols']
    rows = [s for s in rows if s['compilation_id'].startswith(project) and s['external'] == external and (target is None or s['target'] == target)]
    assert len(rows) == 1, (name, project, target, rows)
    return rows[0]['id']

checks = []
for name, project, target, expected, units in [
    ('Start', 'cs:cs/App/', 'net10.0', ['Run', 'Leaf'], ['cs:cs/Library/Library.csproj:net10.0:Debug', 'cs:cs/Leaf/Leaf.csproj:net10.0:Debug']),
    ('CompatibleStart', 'cs:cs/CompatibleApp/', 'net10.0', ['StandardRun', 'StandardLeaf'], ['cs:cs/LibraryStd/LibraryStd.csproj:netstandard2.1:Debug', 'cs:cs/LibraryStd/LibraryStd.csproj:netstandard2.1:Debug']),
    ('OldStart', 'cs:cs/OldApp/', 'netstandard2.1', ['Run', 'Leaf'], ['cs:cs/Library/Library.csproj:netstandard2.1:Debug', 'cs:cs/Leaf/Leaf.csproj:netstandard2.1:Debug']),
    ('start', 'ts:ts/app/', None, ['middle', 'leaf'], ['ts:ts/library/tsconfig.json', 'ts:ts/leaf/tsconfig.json'])]:
    root = symbol(name, project, target)
    graph = query({'op': 'callees', 'symbol_id': root, 'max_depth': 5, **({'target': target} if target else {})}, True)['graph']
    (RUN / f'{name}-callees.json').write_text(json.dumps(graph, indent=2))
    assert [f['symbol']['name'] for f in graph['findings']] == expected, graph
    assert [f['symbol']['compilation_id'] for f in graph['findings']] == units, graph
    assert all(not f['symbol']['external'] and f['symbol']['locations'] for f in graph['findings']), graph
    enriched = query({'op': 'callees', 'symbol_id': root, 'max_depth': 5, 'include_source': True,
                      **({'target': target} if target else {})}, True)['graph']
    assert [f['symbol'] for f in enriched['findings']] == [f['symbol'] for f in graph['findings']], enriched
    selected_files = {
        'Start': ['cs/Library/Code.cs', 'cs/Leaf/Code.cs'],
        'CompatibleStart': ['cs/LibraryStd/Code.cs', 'cs/LibraryStd/Code.cs'],
        'OldStart': ['cs/Library/Code.cs', 'cs/Leaf/Code.cs'],
        'start': ['ts/library/code.ts', 'ts/leaf/code.ts'],
    }[name]
    for finding, selected in zip(enriched['findings'], selected_files, strict=True):
        assert finding['context']['declarations'], finding
        assert all(d['source_status'] == 'included' and d['source']['text']
                   for d in finding['context']['declarations']), finding
        raw = (WORKSPACE / selected).read_bytes()
        for declaration in finding['context']['declarations']:
            body = declaration['source']; path = Path(body['path'])
            assert (path if path.is_absolute() else WORKSPACE / path).resolve() == (WORKSPACE / selected).resolve(), body
            assert body['content_hash'] == declaration['location']['content_hash'] == hashlib.sha256(raw).hexdigest(), declaration
            assert body['start_byte'] == declaration['extent']['start_byte'] and body['end_byte'] == declaration['extent']['end_byte'], declaration
            assert body['text'].encode() == raw[body['start_byte']:body['end_byte']], declaration
        assert not finding.get('supporting_targets'), 'source projection expanded untraversed peer frameworks'
    (RUN / f'{name}-source.json').write_text(json.dumps(enriched, indent=2))
    path = graph['findings'][-1]['paths'][0]
    assert len(path) == 2 and path[0].get('definition_link') and (path[1].get('definition_link') or name == 'CompatibleStart'), graph
    impact = query({'op': 'impact', 'symbol_id': graph['findings'][-1]['symbol']['id']}, True)['graph']
    assert any(f['symbol']['id'] == root and len(f['paths'][0]) == 2 for f in impact['findings']), impact
    checks.append(f'{name}: compiler-selected two-project body traversal and reverse impact')
    checks.append(f'{name}: exact selected body source without root-target re-filtering or peer expansion')

for name, expected in [('construct', '<constructor>'), ('read', 'get value')]:
    g = query({'op': 'callees', 'symbol_id': symbol(name, 'ts:ts/app/'), 'max_depth': 3}, True)['graph']
    assert [f['symbol']['name'] for f in g['findings']] == [expected, 'leaf'], g
    assert all(e.get('definition_link') for e in g['findings'][-1]['paths'][0]), g
checks.append('TS cross-project constructors and implicit accessors')

generic = query({'op': 'callees', 'symbol_id': symbol('GenericStart', 'cs:cs/App/', 'net10.0')}, True)['graph']
assert [f['symbol']['name'] for f in generic['findings']] == ['Echo'] and generic['findings'][0]['paths'][0][0].get('definition_link'), generic
access = query({'op': 'callees', 'symbol_id': symbol('AccessStart', 'cs:cs/App/', 'net10.0'), 'max_depth': 3}, True)['graph']
assert [f['symbol']['name'] for f in access['findings']] == ['get_Value', 'Leaf'], access
assert access['findings'][0]['paths'][0][0]['implicit'], access
external = query({'op': 'callees', 'symbol_id': symbol('ExternalStart', 'cs:cs/App/', 'net10.0')}, True)['graph']
assert external['findings'][0]['symbol']['external'] and external['boundaries'], external
assert not external['findings'][0]['paths'][0][0].get('definition_link'), external
external_source = query({'op': 'callees', 'symbol_id': symbol('ExternalStart', 'cs:cs/App/', 'net10.0'),
                         'include_source': True}, True)['graph']['findings'][0]['context']
assert external_source == {'declarations': [], 'boundary': 'external_or_other_compilation_body'}, external_source
checks += ['generic declaration identity', 'cross-project implicit accessor calls', 'binary/package bodies remain boundaries']

# Project-source redirects must not generate build output into the source tree.
assert not list((WORKSPACE / 'ts').rglob('*.js')) and not list((WORKSPACE / 'ts').rglob('*.d.ts'))
coverage = query({'op': 'symbols', 'name': 'Start'})['coverage']
assert all(c['status'] == 'complete' and c['capabilities'].get('project_links') == 'complete' for c in coverage), coverage
checks += ['no TypeScript build outputs', 'complete compiler/reference selection coverage', 'Rust/.NET/Node parity']
# Saved source changes must invalidate dependent pointers through the shared path.
write('ts/leaf/code.ts', 'export function leaf() { return 2; }')
write('cs/Leaf/Code.cs', 'public static class Bottom { public static int Leaf() => 2; }')
generation = settled(generation)
for name, project, target in [('start', 'ts:ts/app/', None), ('Start', 'cs:cs/App/', 'net10.0')]:
    g = query({'op': 'callees', 'symbol_id': symbol(name, project, target), 'max_depth': 5}, True)['graph']
    assert len(g['findings']) == 2 and all(not f['symbol']['external'] for f in g['findings']), g
checks.append('saved dependency edits republish matching project snapshots')
# Respect explicitly disabled TS source redirects, and retain honest link coverage.
config = WORKSPACE / 'ts/app/tsconfig.json'
options = json.loads(config.read_text())
options['compilerOptions']['disableSourceOfProjectReferenceRedirect'] = True
config.write_text(json.dumps(options))
generation = settled(generation)
c = next(c for c in query({'op': 'symbols', 'name': 'start'})['coverage'] if c['compilation_id'] == 'ts:ts/app/tsconfig.json')
assert c['capabilities']['project_links'] == 'partial', c
g = query({'op': 'callees', 'symbol_id': symbol('start', 'ts:ts/app/'), 'max_depth': 5}, True)['graph']
assert not any(e.get('definition_link') for f in g['findings'] for p in f['paths'] for e in p), g
checks.append('disabled source redirects remain partial without invented body links')
# A snapshot must not certify a source inventory that omits consumed files.
write('ts/library/helper.ts', 'export const extra = 3;')
library = WORKSPACE / 'ts/library/code.ts'
library.write_text(library.read_text() + "\nimport {extra} from './helper.js'; export const another = extra;\n")
generation = settled(generation)
c = next(c for c in query({'op': 'symbols', 'name': 'middle'})['coverage'] if c['compilation_id'] == 'ts:ts/library/tsconfig.json')
assert c['status'] == 'partial' and c['capabilities']['project_links'] == 'partial', c
checks.append('undeclared compiler-consumed source cannot certify a project snapshot')
summary = {'profile': PROFILE, 'database': DATABASE, 'generation': generation, 'checks': checks}
(RUN / 'validation.json').write_text(json.dumps(summary, indent=2))
print(json.dumps(summary))
