"""Focused assembled finding filters, native parity, cursor and policy backfill."""
from pathlib import Path
import hashlib,json,os,sqlite3,subprocess,time
ROOT=Path(__file__).resolve().parents[1]
RUN=ROOT/'.artifacts/test-classification/integration'/str(time.time_ns());W=RUN/'w';DATA=RUN/'d'
W.mkdir(parents=True);PROFILE=os.environ.get('KNOW_CODE_BUILD_PROFILE','debug')
HOST=ROOT/f'target/{PROFILE}/know-code';NODE=ROOT/f'.artifacts/build/{PROFILE}/node/index.mjs'
SMOKE=ROOT/f'bindings/dotnet/Smoke/bin/{PROFILE.capitalize()}/net10.0/Smoke.dll'
ENV=dict(os.environ,KNOW_CODE_DATA_DIR=str(DATA),DOTNET_CLI_TELEMETRY_OPTOUT='1',DOTNET_NOLOGO='1')
(W/'tsconfig.json').write_text(json.dumps({'compilerOptions':{'target':'ES2022','module':'CommonJS','strict':True},'include':['*.ts']}))
(W/'prod.ts').write_text('export function target() { return 1; }\nexport function helper() { return target(); }\n')
(W/'suite.test.ts').write_text('import {helper, target} from "./prod";\nexport function testEntry() { return helper() + target(); }\n')
(W/'global.ts').write_text('interface Mixed { ordinary: string; }\n')
(W/'ambient.test.ts').write_text('interface Mixed { tested: number; }\n')
for name,is_test in [('Lib','false'),('Checks','true')]:
 p=W/name;p.mkdir();reference='<ItemGroup><ProjectReference Include="../Lib/Lib.csproj" /></ItemGroup>' if name=='Checks' else ''
 (p/f'{name}.csproj').write_text(f'<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><TargetFramework>net10.0</TargetFramework><IsTestProject>{is_test}</IsTestProject></PropertyGroup>{reference}<ItemGroup><Compile Include="../Linked.cs" Link="Linked.cs" /></ItemGroup></Project>')
 (p/'Code.cs').write_text('public static class Core { public static int Target() => 1; public static int Helper() => Target(); }\n' if name=='Lib' else 'public static class Checks { public static int TestEntry() => Core.Helper(); }\n')
(W/'Linked.cs').write_text('public class Linked { public int Value => 1; }\n')
CHECKS=[];DATABASE=None;passed=False

def run(args,check=True):
 p=subprocess.run(list(map(str,args)),cwd=ROOT,env=ENV,capture_output=True,text=True,timeout=120)
 with (RUN/'commands.jsonl').open('a') as f:f.write(json.dumps({'args':list(map(str,args)),'exit':p.returncode,'stdout':p.stdout,'stderr':p.stderr})+'\n')
 assert not check or p.returncode==0,(args,p.stdout,p.stderr)
 return p

def cli(*args,check=True):return run([HOST,*args],check)
def value(*args):return json.loads(cli(*args).stdout)
def ready(minimum=1):
 until=time.monotonic()+90;last=None
 while time.monotonic()<until:
  last=next(s for s in value('coordinator','status')['workspaces'] if s['workspace_root']==str(W))
  assert last['phase'] not in ['failed','resource_limited'],last
  if last['phase']=='idle' and last['generation']>=minimum and last['published_epoch']==last['observed_epoch']:return last
  time.sleep(.05)
 raise AssertionError(('not ready',last))
def query(language,request):
 if language=='cli':return value('query',DATABASE,json.dumps(request))
 if language=='node':
  source='import {Index} from '+json.dumps(NODE.as_uri())+'; console.log(JSON.stringify(await new Index(process.argv[1]).query(JSON.parse(process.argv[2]))));'
  return json.loads(run(['node','--input-type=module','-e',source,DATABASE,json.dumps(request)]).stdout)
 return json.loads(run(['dotnet',SMOKE,DATABASE,json.dumps(request)]).stdout)
def stable(response):
 assert 'result' in response,response
 result=dict(response['result']);result.pop('freshness',None);result.pop('coordination',None);return result
def parity(request):
 results=[stable(query(language,request)) for language in ['cli','node','dotnet']]
 assert results[0]==results[1]==results[2],(request,results)
 return results[0]
def stop():
 cli('coordinator','stop',check=False);until=time.monotonic()+15
 while (DATA/'c.sock').exists() and time.monotonic()<until:time.sleep(.05)
 assert not (DATA/'c.sock').exists(),'isolated coordinator did not stop'
try:
 value('watch',W);initial=ready();DATABASE=initial['database']
 baseline=parity({'op':'symbols','limit':1000})
 assert set(c['language'] for c in baseline['coverage'])=={'typescript','csharp'},baseline
 assert baseline['workspace_coverage']['compilations']==3,baseline['workspace_coverage']
 for mode in ['only','exclude']:
  filtered=parity({'op':'symbols','tests':mode,'limit':1000})
  assert filtered['symbols'] and not filtered['truncated'],filtered
  assert all(s['test_classification']['kind'] in (['test','mixed'] if mode=='only' else ['unknown','non_test','mixed']) for s in filtered['symbols']),filtered
  typed=json.loads(run(['dotnet',SMOKE,'--typed-test-filter',DATABASE,mode]).stdout)
  assert stable(typed)['symbols']==filtered['symbols'],typed
 CHECKS.append('three_host_filters_and_dotnet_typed_enum')
 mixed=parity({'op':'symbols','name':'Mixed','tests':'only'})['symbols'][0]
 assert mixed['test_classification']['kind']=='mixed' and len(mixed['locations'])==2,mixed
 assert any(e['kind']=='test' for e in mixed['test_classification']['evidence']),mixed
 assert parity({'op':'symbols','name':'Mixed','tests':'exclude'})['symbols'][0]==mixed
 linked=parity({'op':'symbols','name':'Linked'})['symbols']
 assert len(linked)==2 and {s['test_classification']['kind'] for s in linked}=={'test','non_test'},linked
 assert len({l['content_hash'] for s in linked for l in s['locations']})==1,linked
 CHECKS.append('mixed_declarations_and_linked_source_contexts')
 target=parity({'op':'symbols','name':'target'})['symbols'][0]['id']
 for mode,expected in [('only','suite.test.ts'),('exclude','prod.ts')]:
  refs=parity({'op':'references','symbol_id':target,'tests':mode})['occurrences']
  assert refs and all(o['location']['path']==expected for o in refs),refs
  graph=parity({'op':'impact','symbol_id':target,'tests':mode})['graph']
  assert graph['findings'],graph
  assert all(f['symbol']['test_classification']['kind'] in (['test','mixed'] if mode=='only' else ['unknown','non_test','mixed']) for f in graph['findings']),graph
 for op in ['source','coverage']:
  for language in ['cli','node','dotnet']:
   response=query(language,{'op':op,'tests':'only','artifact_id':mixed['locations'][0]['artifact_id']})
   assert response.get('error',{}).get('code')=='invalid_request',response
 CHECKS.append('reference_use_sites_graph_filters_expansion_rejection')
 first=stable(query('cli',{'op':'symbols','tests':'only','page_size':1,'limit':100}))
 token=first['page']['cursor'];old_generation=first['generation']
 # Tokens are independently minted; compare replay, rather than equal token strings.
 expected=stable(query('cli',{'op':'next_page','cursor':token}))
 for language in ['node','dotnet']:assert stable(query(language,{'op':'next_page','cursor':token}))==expected
 assert query('cli',{'op':'next_page','cursor':token,'tests':'exclude'})['error']['code']=='invalid_request'
 pin=parity({'op':'symbol_context','cursor':token,'symbol_id':mixed['id'],'tests':'only','include_source':True})
 assert pin['generation']==old_generation and pin['contexts'][0]['symbol']['test_classification']['kind']=='mixed',pin
 (W/'ambient.test.ts').rename(W/'ambient.ts');ready(old_generation+1)
 assert not parity({'op':'symbols','name':'Mixed','tests':'only'}).get('symbols'), 'renamed unknown declarations still test'
 for language in ['cli','node','dotnet']:assert stable(query(language,{'op':'next_page','cursor':token}))==expected
 pin=parity({'op':'symbol_context','cursor':token,'symbol_id':mixed['id'],'tests':'only','include_source':True})
 assert pin['generation']==old_generation and len(pin['contexts'][0]['symbol']['locations'])==2,pin
 CHECKS.append('immutable_filtered_pages_and_pinned_context_across_rename')
 stop()
 with sqlite3.connect(DATABASE) as db:
  db.execute("UPDATE compilations SET test_classification_json=''")
  db.execute("UPDATE unit_artifacts SET test_classification_json=''")
  db.execute('UPDATE symbols SET test_mask=1')
  db.execute("UPDATE reports SET capabilities_json=json_remove(capabilities_json,'$.test_classification')")
  before=db.execute('SELECT current_generation FROM state').fetchone()[0]
  prep=Path(DATABASE).with_suffix('.preparation.json');prep_hash=hashlib.sha256(prep.read_bytes()).hexdigest() if prep.exists() else None
 value('watch',W);upgraded=ready(before+1)
 with sqlite3.connect(DATABASE) as db:
  policies=db.execute("SELECT json_extract(c.test_classification_json,'$.policy_version') FROM generation_units u JOIN compilations c ON c.generation=u.owner_generation AND c.id=u.compilation_id WHERE u.generation=?",[upgraded['generation']]).fetchall()
  assert policies and all(p==(1,) for p in policies),policies
 assert prep_hash==(hashlib.sha256(prep.read_bytes()).hexdigest() if prep.exists() else None),'policy backfill altered restoration checkpoints'
 assert parity({'op':'symbols','name':'testEntry','tests':'only'})['symbols'], 'policy backfill lost positive findings'
 stop();value('watch',W);assert ready()['generation']==upgraded['generation'],'completed backfill repeated on restart'
 CHECKS.append('policy_only_backfill_without_restore_and_noop_restart')
 passed=True
finally:
 stop()
 summary={'passed':passed,'profile':PROFILE,'run':str(RUN),'database':DATABASE,'checks':CHECKS,'build_record':str(ROOT/f'.artifacts/build/{PROFILE}/build-record.json')}
 (RUN/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary,indent=2))
