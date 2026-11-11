"""Real TS/Roslyn -> atomic store -> Rust/.NET/Node call/impact parity."""
from fixture_trust import grant_fixture
import atexit
import copy
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time

ROOT = Path(__file__).resolve().parents[1]
RUN = ROOT / '.artifacts/cg' / format(time.time_ns(),'x')[-10:]
WORKSPACE = RUN / 'workspace'
(WORKSPACE / 'ts').mkdir(parents=True)
(WORKSPACE / 'cs').mkdir()
PROFILE = os.environ.get('KNOW_CODE_BUILD_PROFILE', 'debug')
HOST = ROOT / f'target/{PROFILE}/know-code'
ENV = dict(os.environ, KNOW_CODE_DATA_DIR=str(RUN / 'data'), DOTNET_CLI_TELEMETRY_OPTOUT='1', DOTNET_NOLOGO='1')
ENV['KNOW_CODE_QUERY_SCOPE']=str(RUN/'query-admission')
(WORKSPACE / 'ts/tsconfig.json').write_text(json.dumps({'compilerOptions': {'strict': True, 'noEmit': True, 'skipLibCheck': True, 'target': 'ES2022', 'module': 'NodeNext', 'moduleResolution': 'NodeNext'}, 'include': ['*.ts']}))
ts_source = '''// π😀 source coordinates
export function leaf() { /* π😀 retained declaration */ return 1; }
export function direct() { return leaf() + leaf(); }
export function onlyReference() { return leaf; }
export function indirect(fn: () => number) { return fn(); }
export function mutableAlias() { let fn = leaf; fn = () => 2; return fn(); }
export const alias = leaf;
export function constantAlias() { return alias(); }
export class Access {
  get value() { return leaf(); }
  set value(n: number) { leaf(); }
  use() { const n = this.value; this.value = n; return n; }
}
export const answer = 42;
export function readAnswer() { return answer; }
export function typed(a: Access) { return a; }
export const initialized = leaf();
export function readInitialized() { return initialized; }
export const literal = () => leaf();
export function literalCaller() { return literal(); }
export function readLiteral() { return literal; }
export class Created { constructor() { leaf(); } }
export function construct() { return new Created(); }
export function tag(parts: TemplateStringsArray) { return leaf(); }
export function tagged() { return tag`π😀`; }
'''
(WORKSPACE / 'ts/code.ts').write_text(ts_source)
(WORKSPACE / 'ts/consumer.ts').write_text("import {leaf} from './code.js'; export function consumer() { return leaf(); }\n")
(WORKSPACE / 'cs/Graph.csproj').write_text('<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><TargetFrameworks>netstandard2.1;net10.0</TargetFrameworks><LangVersion>latest</LangVersion><Nullable>enable</Nullable></PropertyGroup></Project>')
cs_source = '''// π😀 source coordinates
using System;
public interface IWork { int Work(); }
public class Parent { public virtual int Work() => 0; }
public class Worker : Parent, IWork {
  public static int Leaf() { /* π😀 retained declaration */ return 1; }
  public override int Work() => Leaf();
  public int Value { get => Leaf(); set { Leaf(); } }
  public int this[int index] => Leaf();
  public static Worker operator +(Worker a, Worker b) { Leaf(); return a; }
  public static implicit operator int(Worker a) { Leaf(); return 1; }
  public int Use() {
    var w = new Worker(); var n = w.Value; w.Value = n; var indexed = w[0];
    var sum = w + w; int number = w;
    Func<int> reference = Leaf; reference();
    int Local() => Leaf(); Func<int> lambda = () => Leaf();
    return Local() + lambda() + number;
  }
  public static int ViaInterface(IWork target) => target.Work();
  public static int ViaVirtual(Parent target) => target.Work();
  public const int Answer = 42;
  public static int ReadAnswer() => Answer;
  public static int Initialized = Leaf();
  public static int ReadInitialized() => Initialized;
  public static Func<int> Deferred = () => Leaf();
  public static Func<int> ReadDeferred() => Deferred;
  // SAVE
}
'''
(WORKSPACE / 'cs/Code.cs').write_text(cs_source)


def run(args, check=True):
    p = subprocess.run([str(a) for a in args], cwd=ROOT, env=ENV, text=True, capture_output=True, timeout=120)
    assert not check or p.returncode == 0, (p.stdout, p.stderr)
    return p


atexit.register(lambda: run([HOST, 'coordinator', 'stop'], check=False))
grant_fixture(HOST, WORKSPACE, ENV, RUN, RUN/'fixture-trust.jsonl')
registered = json.loads(run([HOST, 'watch', WORKSPACE]).stdout)
DATABASE = registered['database']


def ready(after=0):
    deadline = time.monotonic() + 90
    while time.monotonic() < deadline:
        s = json.loads(run([HOST, 'coordinator', 'status']).stdout)['workspaces'][0]
        assert s['phase'] != 'failed', s
        if s['phase'] == 'idle' and s['generation'] > after:
            return s['generation']
        time.sleep(.05)
    raise AssertionError(s)


generation = ready()
checks = []


def query(request, parity=False):
    request = dict(request, time_budget_ms=5000)
    encoded = json.dumps(request)
    result = json.loads(run([HOST, 'query', DATABASE, encoded]).stdout)
    assert 'result' in result, result
    assert result['result']['freshness'] == 'current', result
    if parity:
        for command in (['node', ROOT / f'.artifacts/build/{PROFILE}/node/smoke.mjs', DATABASE, encoded],
                        ['dotnet', ROOT / f'bindings/dotnet/Smoke/bin/{PROFILE.capitalize()}/net10.0/Smoke.dll', DATABASE, encoded]):
            other = json.loads(run(command).stdout)
            assert result == other, (result, other)
    return result['result']


def symbol(name, language, signature=None):
    result = query({'op': 'symbols', 'name': name, 'target': 'net10.0' if language == 'cs' else None})
    rows = [s for s in result['symbols'] if s['compilation_id'].startswith('ts:' if language == 'ts' else 'cs:')
            and not s['kind'].startswith('Alias') and (language == 'ts' or s['target'] == 'net10.0')]
    if signature:
        rows = [s for s in rows if signature in s['signature']]
    assert len(rows) == 1, (name, rows)
    return rows[0]['id']


def graph(op, identifier, **options):
    return query(dict(op=op, symbol_id=identifier, **options), parity=True)['graph']


ts_leaf = symbol('leaf', 'ts')
callers = graph('callers', ts_leaf, paths_per_symbol=2)
names = {f['symbol']['name'] for f in callers['findings']}
assert {'direct', 'consumer'} <= names, callers
assert 'onlyReference' not in names, callers
direct = next(f for f in callers['findings'] if f['symbol']['name'] == 'direct')
assert len(direct['paths']) == 2 and len({p[0]['location']['start_byte'] for p in direct['paths']}) == 2
assert any(f['symbol']['kind'] == 'initializer' for f in callers['findings']), callers
checks += ['TS calls distinct from references', 'multiple callsites', 'explicit initializer ownership']
initializers = [f for f in callers['findings'] if f['symbol']['kind'] == 'initializer']
assert len(initializers) == 1 and initializers[0]['symbol']['name'] == '<initialize initialized>', initializers
initializer = initializers[0]
edge = initializer['paths'][0][0]
assert edge['source_id'] == initializer['symbol']['id'] and edge['target_id'] == ts_leaf
assert edge['resolution'] == 'compiler_bound' and not edge['implicit'], edge
assert [f['symbol']['id'] for f in graph('callees', initializer['symbol']['id'])['findings']] == [ts_leaf]
for reader in ['readInitialized', 'readLiteral']:
    assert not graph('callees', symbol(reader, 'ts'))['findings'], reader
for caller, expected, kind, span in [('literalCaller', 'literal', None, 'literal()'),
                                     ('construct', '<constructor>', 'constructor', 'new Created()'),
                                     ('tagged', 'tag', None, 'tag`π😀`')]:
    found = graph('callees', symbol(caller, 'ts'), max_depth=2)
    assert {f['symbol']['name'] for f in found['findings']} == {expected, 'leaf'}, found
    called = next(f for f in found['findings'] if f['symbol']['name'] == expected)
    if kind:
        assert called['symbol']['kind'] == kind, called
    site = called['paths'][0][0]
    assert site['resolution'] == 'compiler_bound' and site['dispatch'] == 'direct' and not site['implicit'], site
    source = query({'op': 'source', 'artifact_id': site['location']['artifact_id']})['source']
    assert source['content_hash'] == site['location']['content_hash']
    assert source['text'].encode()[site['location']['start_byte']:site['location']['end_byte']].decode() == span, site
checks += ['TS const-owned function literal/constructor/tagged calls with exact compiler sites',
           'TS initialization is separate from field/function-value reads']
callees = graph('callees', symbol('indirect', 'ts'))
assert any(b['reason'] == 'unresolved_target' for b in callees['boundaries']), callees
assert not callees['findings'], callees
for name in ['mutableAlias', 'constantAlias']:
    indirect = graph('callees', symbol(name, 'ts'))
    assert not indirect['findings'] and any(b['reason'] == 'unresolved_target' for b in indirect['boundaries']), indirect
access = graph('callees', symbol('use', 'ts'))
assert {f['symbol']['name'] for f in access['findings']} >= {'get value', 'set value'}, access
assert not graph('callees', symbol('use', 'ts'), include_implicit=False)['findings']
checks += ['indirect calls retain unresolved evidence', 'TS implicit accessor filter']

cs_leaf = symbol('Leaf', 'cs')
callers = graph('callers', cs_leaf, target='net10.0')
assert {'Work', 'get_Value', 'set_Value', 'get_Item', 'Local'} <= {f['symbol']['name'] for f in callers['findings']}, callers
assert any(f['symbol']['kind'] == 'lambda' and f['symbol']['locations'] for f in callers['findings']), callers
assert all(f['symbol']['locations'] for f in callers['findings'] if f['symbol']['name'] in {'get_Value', 'set_Value', 'get_Item'}), callers
initializers = [f for f in callers['findings'] if f['symbol']['kind'] == 'initializer']
assert len(initializers) == 1, initializers
initializer = initializers[0]
edge = initializer['paths'][0][0]
assert edge['source_id'] == initializer['symbol']['id'] and edge['target_id'] == cs_leaf
assert edge['resolution'] == 'compiler_bound' and not edge['implicit'], edge
source = query({'op': 'source', 'artifact_id': edge['location']['artifact_id']})['source']
assert source['content_hash'] == edge['location']['content_hash']
assert source['text'].encode()[edge['location']['start_byte']:edge['location']['end_byte']].decode() == 'Leaf()', edge
assert [f['symbol']['id'] for f in graph('callees', initializer['symbol']['id'], target='net10.0')['findings']] == [cs_leaf]
for reader in ['ReadInitialized', 'ReadDeferred']:
    assert not graph('callees', symbol(reader, 'cs'), target='net10.0')['findings'], reader
checks += ['Roslyn initializer call ownership and no invented calls from field/function-value reads']
use = graph('callees', symbol('Use', 'cs'), target='net10.0')
assert any(b['reason'] == 'unresolved_target' for b in use['boundaries']), use
assert {'get_Value', 'set_Value', 'get_Item', 'op_Addition', 'op_Implicit'} <= {f['symbol']['name'] for f in use['findings']}, use
explicit = graph('callees', symbol('Use', 'cs'), target='net10.0', include_implicit=False)
assert not any(p[0]['implicit'] for f in explicit['findings'] for p in f['paths'])
for method in ['ViaInterface', 'ViaVirtual']:
    found = graph('callees', symbol(method, 'cs'), target='net10.0')
    possible = [p[0] for f in found['findings'] for p in f['paths'] if p[0]['resolution'] == 'possible_dispatch']
    assert possible and all(e['dispatch_evidence'] and e['binding_target_id'] for e in possible), found
    exact = graph('callees', symbol(method, 'cs'), target='net10.0', exact_only=True)
    assert not any(e['resolution'] == 'possible_dispatch' for f in exact['findings'] for p in f['paths'] for e in p)
checks += ['Roslyn calls/local functions/accessors/operators/conversions', 'possible dispatch with proof', 'exact and implicit filters']

for language, constant, reader in [('ts', 'answer', 'readAnswer'), ('cs', 'Answer', 'ReadAnswer')]:
    impact = graph('impact', symbol(constant, language), kinds=['reference'])
    assert reader in {f['symbol']['name'] for f in impact['findings']}, impact
    assert all(e['kind'] == 'reference' for f in impact['findings'] for p in f['paths'] for e in p)
checks += ['value-reference impact and dependency-kind filter']
for name, language, expected in [('Access', 'ts', 'typed'), ('IWork', 'cs', 'ViaInterface')]:
    impacted = graph('impact', symbol(name, language), kinds=['type_reference'])
    assert expected in {f['symbol']['name'] for f in impacted['findings']}, impacted
inherited = graph('impact', symbol('Parent', 'cs'), kinds=['inherits'])
assert 'Worker' in {f['symbol']['name'] for f in inherited['findings']}, inherited
modules = [s for s in query({'op': 'symbols', 'limit': 1000})['symbols']
           if s['kind'] == 'ValueModule' and '/ts/code' in s['name']]
assert len(modules) == 1, modules
imported = graph('impact', modules[0]['id'], kinds=['imports'])
assert any('/ts/consumer' in f['symbol']['name'] for f in imported['findings']), imported
checks += ['compiler-backed type/inheritance/import impact']

locations = 0
for finding in use['findings']:
    for path in finding['paths']:
        for e in path:
            loc = e['location']
            source = query({'op': 'source', 'artifact_id': loc['artifact_id']})['source']['text'].encode()
            span = source[loc['start_byte']:loc['end_byte']].decode()
            assert span and ('(' in span or 'Value' in span or '+' in span or 'w' in span), (span, e)
            assert e['compilation_id'] == finding['symbol']['compilation_id'], 'mixed compilation path'
            locations += 1
checks += ['exact retained UTF8 callsite source and compilation provenance']

# Exercise source projection through the real compiler/store/native path. Keep
# the existing fixture and walks so clipping cannot conceal lost findings.
def compact_findings(value):
    rows = copy.deepcopy(value['findings'])
    for finding in rows:
        finding.pop('context', None)
        for support in finding.get('supporting_targets', []):
            support.pop('context', None)
    return rows


def projected_bodies(value):
    bodies = []
    for finding in value['findings']:
        assert 'context' in finding, finding
        for member in [finding, *finding.get('supporting_targets', [])]:
            context = member['context']
            for declaration in context['declarations']:
                location = declaration['location']
                assert location in member.get('symbol', member)['locations'], member
                body = declaration.get('source')
                if body is None:
                    continue
                path = Path(body['path'])
                raw = (path if path.is_absolute() else WORKSPACE / path).read_bytes()
                extent = declaration['extent']
                assert body['content_hash'] == location['content_hash'] == hashlib.sha256(raw).hexdigest(), declaration
                assert extent['start_byte'] == body['start_byte'] <= body['end_byte'] <= extent['end_byte'], declaration
                assert body['text'].encode() == raw[body['start_byte']:body['end_byte']], declaration
                bodies.append(body)
        assert all('context' not in edge for path in finding['paths'] for edge in path)
    assert all('context' not in root for root in value.get('root_definitions', []))
    return bodies


for op, identifier in [('callers', ts_leaf), ('callees', symbol('direct', 'ts')),
                       ('impact', symbol('answer', 'ts')), ('callers', cs_leaf),
                       ('callees', symbol('Use', 'cs'))]:
    compact = graph(op, identifier)
    assert graph(op, identifier, include_source=False) == compact
    assert all('context' not in f and all('context' not in s for s in f.get('supporting_targets', []))
               for f in compact['findings']), compact
    included = graph(op, identifier, include_source=True, source_max_bytes=262144)
    assert compact_findings(included) == compact['findings'], (compact, included)
    bodies = projected_bodies(included)
    assert bodies and sum(len(b['text'].encode()) for b in bodies) <= 262144
    limited = query(dict(op=op, symbol_id=identifier, include_source=True, source_max_bytes=1), parity=True)
    assert compact_findings(limited['graph']) == compact['findings'], limited
    assert limited['truncated'] and 'source_budget' in limited['graph']['limits'], limited
    small_bodies = projected_bodies(limited['graph'])
    assert sum(len(b['text'].encode()) for b in small_bodies) <= 1
    statuses = [d['source_status'] for f in limited['graph']['findings']
                for m in [f, *f.get('supporting_targets', [])] for d in m['context']['declarations']]
    assert any(s in {'truncated', 'budget_exhausted'} for s in statuses), statuses
    if identifier == cs_leaf:
        grouped = [f for f in included['findings'] if f.get('supporting_targets')]
        assert grouped and all({s['target'] for s in f['supporting_targets']} == {'net10.0', 'netstandard2.1'}
                               for f in grouped), included
checks += ['opt_in_graph_source_preserves_TS_Csharp_walks_and_compact_defaults',
           'source_exhaustion_retains_findings_and_labeled_limits',
           'exact_hash_extent_UTF8_target_source_and_all_wire_copy_budget',
           'source_is_nested_on_findings_not_roots_or_paths']

# The typed managed request must preserve omission and explicit false rather
# than inject symbol_context's default into graph calls.
for mode, cap in [('default', None), ('false', None), ('true', None), ('true', 1)]:
    request = dict(op='callers', symbol_id=ts_leaf)
    if mode != 'default': request['include_source'] = mode == 'true'
    if cap is not None: request['source_max_bytes'] = cap
    expected = json.loads(run([HOST, 'query', DATABASE, json.dumps(request)]).stdout)
    actual = json.loads(run(['dotnet', ROOT / f'bindings/dotnet/Smoke/bin/{PROFILE.capitalize()}/net10.0/Smoke.dll',
                             '--typed-graph-source', DATABASE, 'callers', ts_leaf, mode,
                             cap if cap is not None else '-']).stdout)
    assert actual == expected, (mode, cap, actual, expected)
checks.append('typed_DotNet_optional_source_and_budget_match_native_requests')

for request, code in [
    ({'op': 'callers', 'symbol_id': ts_leaf, 'include_source': True, 'source_max_bytes': 0}, 'invalid_request'),
    ({'op': 'callers', 'symbol_id': ts_leaf, 'source_max_bytes': 262145}, 'invalid_request'),
    ({'op': 'symbols', 'source_max_bytes': 1024}, 'invalid_request'),
    ({'op': 'symbol_context', 'symbol_id': ts_leaf, 'source_max_bytes': 1024}, 'invalid_request'),
    ({'op': 'callers', 'symbol_id': ts_leaf, 'include_source': True, 'response_max_bytes': 1024}, 'response_limit_exceeded'),
    ({'op': 'callers', 'symbol_id': ts_leaf, 'include_source': True, 'deadline_ms': 0}, 'deadline_exceeded')]:
    responses = [json.loads(run([*prefix, json.dumps(request)]).stdout) for prefix in [
        [HOST, 'query', DATABASE], ['node', ROOT / f'.artifacts/build/{PROFILE}/node/smoke.mjs', DATABASE],
        ['dotnet', ROOT / f'bindings/dotnet/Smoke/bin/{PROFILE.capitalize()}/net10.0/Smoke.dll', DATABASE]]]
    assert responses[0] == responses[1] == responses[2] and responses[0]['error']['code'] == code, (request, responses)
checks.append('graph_source_shared_validation_complete_response_and_deadline_errors')

(WORKSPACE / 'ts/code.ts').write_text(ts_source + '\nexport function added() { return leaf(); }\n')
generation = ready(generation)
assert 'added' in {f['symbol']['name'] for f in graph('callers', ts_leaf)['findings']}
(WORKSPACE / 'cs/Code.cs').write_text(cs_source.replace('// SAVE', 'public static int Added() => Leaf();'))
generation = ready(generation)
assert 'Added' in {f['symbol']['name'] for f in graph('callers', cs_leaf, target='net10.0')['findings']}
checks += ['saved TS/C# graph replacement', 'Rust/.NET/Node parity']
healthy_cs = cs_source.replace('// SAVE', 'public static int Added() => Leaf();')
(WORKSPACE / 'cs/Code.cs').write_text(healthy_cs.replace('public static int Added()', 'public static void Broken() { MissingCall(); } public static int Added()'))
generation = ready(generation)
broken = graph('callees', symbol('Broken', 'cs'), target='net10.0')
assert not broken['findings'] and any(b['reason'] == 'unresolved_target' for b in broken['boundaries']), broken
assert any(c['status'] == 'partial' for c in query({'op': 'symbols', 'name': 'Broken'})['coverage'])
(WORKSPACE / 'cs/Code.cs').write_text(healthy_cs)
generation = ready(generation)
checks += ['fresh compiler-error partial graph with unresolved callsite and repair']
summary = {'passed': True, 'profile': PROFILE, 'database': DATABASE, 'generation': generation,
           'checked_source_ranges': locations, 'checks': checks,
           'capabilities': 'explicitly partial; selected project bodies link; broader dispatch closure remains partial'}
(RUN / 'summary.json').write_text(json.dumps(summary, indent=2)+'\n')
(ROOT / '.artifacts/call-graph').mkdir(parents=True,exist_ok=True)
(ROOT / '.artifacts/call-graph/latest.json').write_text(json.dumps(summary, indent=2)+'\n')
print(json.dumps(summary, indent=2))
