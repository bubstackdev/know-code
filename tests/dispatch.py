"""Cross-project dispatch with compiler-selected composition and exact proof sites."""
import atexit
import json
import os
from pathlib import Path
import subprocess
import time

ROOT=Path(__file__).resolve().parents[1]
RUN=ROOT/'.artifacts/dispatch'/str(time.time_ns())
WORKSPACE=RUN/'workspace'
PROFILE=os.environ.get('KNOW_CODE_BUILD_PROFILE','debug')
HOST=ROOT/f'target/{PROFILE}/know-code'
ENV=dict(os.environ,KNOW_CODE_DATA_DIR=str(RUN/'data'),DOTNET_CLI_TELEMETRY_OPTOUT='1',DOTNET_NOLOGO='1')
def write(path,text):
    file=WORKSPACE/path;file.parent.mkdir(parents=True,exist_ok=True);file.write_text(text)
projects=[
 ('Contracts','netstandard2.1',[],'''public interface IWork { int Run(); }
public interface IGen<T> { T Gen(); }
public class Base : IWork { public virtual int Run() => BaseLeaf(); public static int BaseLeaf() => 1; }
public interface IBorrow { int Borrow(); }
public class PlainBase { public virtual int Borrow() => 1; }
'''),
 ('Middle','netstandard2.1;net10.0',['Contracts'],'''public class Middle : Base { public override int Run() => MiddleLeaf(); public static int MiddleLeaf() => 2; }'''),
 ('Derived','netstandard2.1;net10.0',['Middle'],'''public class Derived : Middle { public override int Run() => DerivedLeaf(); public static int DerivedLeaf() => 3; public int BaseCall() => base.Run(); } public class NumberWork : IGen<int> { public int Gen() => 1; } public class StringWork : IGen<string> { public string Gen() => "text"; }'''),
 ('Borrowed','net10.0',['Contracts'],'public class Borrowed : PlainBase, IBorrow { }'),
 ('Unrelated','net10.0',['Contracts'],'public class Unrelated : PlainBase { public override int Borrow() => 4; }'),
 ('Isolated','net10.0',['Contracts'],'public class Isolated : IWork { public int Run() => 5; }'),
 ('Caller','net10.0',['Contracts'],'''public static class Caller {
 public static int InterfaceCall(IWork w) => w.Run();
 public static int VirtualCall(Base w) => w.Run();
 public static int BorrowCall(IBorrow w) => w.Borrow();
 public static int GenericCall(IGen<int> w) => w.Gen();
}'''),
 ('App','net10.0',['Caller','Derived','Borrowed','Unrelated'],'public class Composition { }')]
def cs_project(name,frameworks,refs):
    return '<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><TargetFrameworks>'+frameworks+'</TargetFrameworks><LangVersion>latest</LangVersion></PropertyGroup><ItemGroup>'+''.join(f'<ProjectReference Include="../{r}/{r}.csproj" />' for r in refs)+'</ItemGroup></Project>'
for name,frameworks,refs,source in projects:
    write(f'cs/{name}/{name}.csproj',cs_project(name,frameworks,refs));write(f'cs/{name}/Code.cs',source)
ts_projects=[
 ('contracts',[],'''export interface IWork { run(): number; }
export interface IGen<T> { gen(): T; }
export class Base implements IWork { run() { return baseLeaf(); } }
export function baseLeaf() { return 1; }
export interface IBorrow { borrow(): number; }
export class PlainBase { borrow() { return 1; } }
'''),
 ('middle',['contracts'],"import {Base} from '../contracts/code.js'; export class Middle extends Base { override run() { return middleLeaf(); } } export function middleLeaf() { return 2; }"),
 ('derived',['middle'],"import {Middle} from '../middle/code.js'; import {IGen} from '../contracts/code.js'; export class Derived extends Middle { override run() { return derivedLeaf(); } baseCall() { return super.run(); } } export function derivedLeaf() { return 3; } export class NumberWork implements IGen<number> { gen() { return 1; } } export class StringWork implements IGen<string> { gen() { return 'text'; } }"),
 ('borrowed',['contracts'],"import {PlainBase, IBorrow} from '../contracts/code.js'; export class Borrowed extends PlainBase implements IBorrow {}"),
 ('unrelated',['contracts'],"import {PlainBase} from '../contracts/code.js'; export class Unrelated extends PlainBase { override borrow() { return 4; } }"),
 ('isolated',['contracts'],"import {IWork} from '../contracts/code.js'; export class Isolated implements IWork { run() { return 5; } }"),
 ('caller',['contracts'],"import {IWork, Base, IBorrow, IGen} from '../contracts/code.js'; export function interfaceCall(w: IWork) { return w.run(); } export function virtualCall(w: Base) { return w.run(); } export function borrowCall(w: IBorrow) { return w.borrow(); } export function genericCall(w: IGen<number>) { return w.gen(); }"),
 ('app',['caller','derived','borrowed','unrelated'],'export const composition = true;')]
for name,refs,source in ts_projects:
    write(f'ts/{name}/tsconfig.json',json.dumps({'compilerOptions':{'composite':True,'strict':True,'module':'NodeNext','moduleResolution':'NodeNext','target':'ES2022','skipLibCheck':True},'files':['code.ts'],'references':[{'path':f'../{r}'} for r in refs]}))
    write(f'ts/{name}/code.ts',source)
def run(args,check=True):
    p=subprocess.run([str(a) for a in args],cwd=ROOT,env=ENV,text=True,capture_output=True,timeout=180)
    assert not check or p.returncode==0,(p.stdout,p.stderr)
    return p
atexit.register(lambda:run([HOST,'coordinator','stop'],False))
registered=json.loads(run([HOST,'index',WORKSPACE]).stdout);DATABASE=registered['database']
def settled(after=0):
    end=time.monotonic()+120
    while time.monotonic()<end:
        s=json.loads(run([HOST,'coordinator','status']).stdout)['workspaces'][0]
        assert s['phase']!='failed',s
        if s['phase']=='idle' and s['generation']>after:return s['generation']
        time.sleep(.1)
    raise AssertionError(s)
generation=settled()
def query(request,parity=True):
    encoded=json.dumps(dict(request,time_budget_ms=5000,max_bytes=8*1024*1024))
    response=json.loads(run([HOST,'query',DATABASE,encoded]).stdout)
    assert 'result' in response,response
    if parity:
        for command in (['node',ROOT/f'.artifacts/build/{PROFILE}/node/smoke.mjs',DATABASE,encoded],['dotnet',ROOT/f'bindings/dotnet/Smoke/bin/{PROFILE.capitalize()}/net10.0/Smoke.dll',DATABASE,encoded]):
            other=json.loads(run(command).stdout)
            if other!=response:
                (RUN/'parity-expected.json').write_text(json.dumps(response,indent=2));(RUN/'parity-actual.json').write_text(json.dumps(other,indent=2))
                raise AssertionError(f'parity mismatch: {RUN}')
    return response['result']
def symbol(name,project,target=None,signature=None):
    rows=query({'op':'symbols','name':name},False)['symbols']
    rows=[s for s in rows if s['compilation_id'].startswith(project) and not s['external'] and (target is None or s['target']==target) and (signature is None or signature in s['signature'])]
    assert len(rows)==1,(name,project,rows);return rows[0]['id']
checks=[]
for language in ['cs','ts']:
    cs=language=='cs';prefix='cs:cs/' if cs else 'ts:ts/'
    caller_project=prefix+('Caller/' if cs else 'caller/')
    derived_project=prefix+('Derived/' if cs else 'derived/')
    root=symbol('InterfaceCall' if cs else 'interfaceCall',caller_project)
    def graph(op,identifier,**kwargs):return query(dict(op=op,symbol_id=identifier,**kwargs))['graph']
    for name in ['InterfaceCall','VirtualCall'] if cs else ['interfaceCall','virtualCall']:
        g=graph('callees',symbol(name,caller_project),max_depth=2,limit=100)
        (RUN/f'{language}-{name}.json').write_text(json.dumps(g,indent=2))
        possible=[f for f in g['findings'] if any(e['resolution']=='possible_dispatch' for p in f['paths'] for e in p)]
        selected=[f for f in possible if f['symbol']['compilation_id'].startswith(derived_project) and f['symbol']['name']==('Run' if cs else 'run')]
        assert len(selected)==1 and (not cs or selected[0]['symbol']['target']=='net10.0'),str(RUN/f'{language}-{name}.json')
        edge=selected[0]['paths'][0][0]
        assert edge['dispatch_context_id'].startswith(prefix+('App/' if cs else 'app/')) and edge['dispatch_proof']['compilation_id'].startswith(derived_project),edge
        assert edge['binding_definition_link'] and edge['dispatch_proof']['target_definition_link'],edge
        assert not any('Isolated/' in f['symbol']['compilation_id'] or 'isolated/' in f['symbol']['compilation_id'] for f in possible),g
        if cs or name=='interfaceCall': assert any(b['reason']=='dispatch_context_unproven' for b in g['boundaries']),g
        if cs:assert all(f['symbol']['target']!='netstandard2.1' for f in possible if f['symbol']['compilation_id'].startswith(('cs:cs/Middle/','cs:cs/Derived/'))),g
        exact=graph('callees',symbol(name,caller_project),exact_only=True)
        assert not any(e['resolution']=='possible_dispatch' for f in exact['findings'] for p in f['paths'] for e in p),exact
    generic=graph('callees',symbol('GenericCall' if cs else 'genericCall',caller_project))
    specializations=[f for f in generic['findings'] if f['symbol']['compilation_id'].startswith(derived_project)]
    assert len(specializations)==1 and (not cs or 'NumberWork' in specializations[0]['symbol']['signature']),generic
    if not cs: assert specializations[0]['symbol']['id']==symbol('gen',derived_project,signature='number'),generic
    assert specializations[0]['paths'][0][0].get('dispatch_key'),generic
    changed=symbol('Run' if cs else 'run',derived_project,'net10.0' if cs else None)
    incoming=graph('callers',changed)
    assert root in {f['symbol']['id'] for f in incoming['findings']},incoming
    impacted=graph('impact',changed,kinds=['call'],max_depth=2)
    assert root in {f['symbol']['id'] for f in impacted['findings']},impacted
    base_call=graph('callees',symbol('BaseCall' if cs else 'baseCall',derived_project,'net10.0' if cs else None))
    assert all(e['resolution']=='compiler_bound' and e['dispatch']=='direct' for f in base_call['findings'] for p in f['paths'] for e in p),base_call
    borrowed=graph('callees',symbol('BorrowCall' if cs else 'borrowCall',caller_project))
    bodies=[f for f in borrowed['findings'] if f['symbol']['name']==('Borrow' if cs else 'borrow') and f['symbol']['compilation_id'].startswith(prefix+('Contracts/' if cs else 'contracts/')) and f['paths'][0][0].get('dispatch_proof')]
    assert bodies and bodies[0]['paths'][0][0]['dispatch_proof']['source_definition_link'],borrowed
    assert not any('Unrelated/' in f['symbol']['compilation_id'] or 'unrelated/' in f['symbol']['compilation_id'] for f in borrowed['findings']),borrowed
    unrelated=symbol('Borrow' if cs else 'borrow',prefix+('Unrelated/' if cs else 'unrelated/'))
    assert not any(f['symbol']['id']==symbol('BorrowCall' if cs else 'borrowCall',caller_project) for f in graph('callers',unrelated)['findings'])
    for finding in incoming['findings']:
        for path in finding['paths']:
            for edge in path:
                if 'dispatch_proof' not in edge:continue
                for location in [edge['location'],edge['dispatch_proof']['location']]:
                    text=(WORKSPACE/location['path']).read_bytes()
                    assert 0<=location['start_byte']<location['end_byte']<=len(text)
    checks += [f'{language}: transitive interface/virtual ancestry, sibling composition, target exclusion, reverse callers/impact, direct base call, inherited-body receiver safety, native parity']
# Removing the compiler-selected composition must remove the sibling candidate,
# even while all the analyzed implementation units remain in the index.
write('cs/App/App.csproj',cs_project('App','net10.0',['Caller','Borrowed','Unrelated']))
file=WORKSPACE/'ts/app/tsconfig.json';config=json.loads(file.read_text());config['references']=[r for r in config['references'] if r['path']!='../derived'];file.write_text(json.dumps(config))
generation=settled(generation)
for name,project in [('InterfaceCall','cs:cs/Caller/'),('interfaceCall','ts:ts/caller/')]:
    g=query({'op':'callees','symbol_id':symbol(name,project)})['graph']
    assert not any('/Derived/' in f['symbol']['compilation_id'] or '/derived/' in f['symbol']['compilation_id'] for f in g['findings']),g
checks.append('saved composition removal invalidates candidate context without removing analyzed implementations')
coverage=query({'op':'symbols','name':'composition'},False)['coverage']
assert all(c['status']=='complete' for c in coverage),[(c['compilation_id'],c['diagnostics']) for c in coverage if c['status']!='complete']
summary={'profile':PROFILE,'database':DATABASE,'generation':generation,'checks':checks,'capabilities':'partial; explicit compiler ancestry only, no unconnected or structural candidates'}
(RUN/'validation.json').write_text(json.dumps(summary,indent=2));print(json.dumps(summary))
