"""Real TS/C# scoped setup failures, encoded-source gaps and call evidence.

Run after one matching build. The sole downloaded compiler is an exact real
unsupported TS version in this owned fixture. No SDK installation or builds run
here; the coordinator performs its normal trusted compiler analysis.
"""
from pathlib import Path
import atexit,hashlib,json,os,shutil,subprocess,time,traceback
ROOT=Path(__file__).resolve().parents[1];RUN=ROOT/'.artifacts/pf'/format(time.time_ns(),'x')[-9:]
WORK=RUN/'w';DATA=RUN/'d';SCOPE=RUN/'s';WORK.mkdir(parents=True)
PROFILE=os.environ.get('KNOW_CODE_BUILD_PROFILE','debug');CONFIG=PROFILE.capitalize()
HOST=ROOT/f'target/{PROFILE}/know-code';NODE=ROOT/f'.artifacts/build/{PROFILE}/node/index.mjs'
SMOKE=ROOT/f'bindings/dotnet/Smoke/bin/{CONFIG}/net10.0/Smoke.dll'
ENV=dict(os.environ,KNOW_CODE_DATA_DIR=str(DATA),KNOW_CODE_QUERY_SCOPE=str(SCOPE),
 DOTNET_CLI_TELEMETRY_OPTOUT='1',DOTNET_NOLOGO='1',MSBUILDDISABLENODEREUSE='1',DOTNET_CLI_USE_MSBUILD_SERVER='0')
for key in ['KNOW_CODE_COORDINATOR','KNOW_CODE_RUNTIME_DIR','NODE_PATH','NODE_OPTIONS']:ENV.pop(key,None)
GOOD_TS=WORK/'ts/good';BAD_TS=WORK/'ts/bad';OLD_TS=WORK/'ts/old-version'
GOOD_CS=WORK/'cs/good';BAD_CS=WORK/'cs/bad'
for directory in [GOOD_TS,BAD_TS,OLD_TS,GOOD_CS,BAD_CS]:directory.mkdir(parents=True)
TS_CONFIG='{"compilerOptions":{"strict":true,"noEmit":true,"skipLibCheck":true},"include":["*.ts"]}'
CS_PROJECT='<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><TargetFramework>net10.0</TargetFramework><LangVersion>latest</LangVersion></PropertyGroup></Project>'
TS_CODE='''export function TsRecursive(n: number): number { return n ? TsRecursive(n-1) : 0; }
export function TsMutualA(n: number): number { return n ? TsMutualB(n-1) : 0; }
export function TsMutualB(n: number): number { return n ? TsMutualA(n-1) : 0; }
export class TsCell { get value() { return 1; } set value(v: number) {} }
const cell = new TsCell();
export function TsWrite() { cell.value = 2; }
export function TsRead() { return cell.value; }
export function TsCompound() { cell.value += 1; }
export function TsCoalesce() { cell.value ??= 2; }
export function TsTupleWrite() { [cell.value] = [2]; }
export function TsDestructureWrite() { ({v: cell.value} = {v: 2}); }
export function TsBracketWrite() { cell['value'] = 2; }
'''
CS_CODE='''public static class CsRecursiveOwner {
 public static int CsRecursive(int n) => n > 0 ? CsRecursive(n-1) : 0;
 public static int CsMutualA(int n) => n > 0 ? CsMutualB(n-1) : 0;
 public static int CsMutualB(int n) => n > 0 ? CsMutualA(n-1) : 0;
}
public class CsCell { public int? Value { get; set; } }
public static class CsAccess {
 static readonly CsCell cell = new();
 public static void CsWrite() { cell.Value = 2; }
 public static int? CsRead() => cell.Value;
 public static void CsCompound() { cell.Value += 1; }
 public static void CsCoalesce() { cell.Value ??= 2; }
 public static void CsTupleWrite() { (cell.Value, cell.Value) = (1, 2); }
}
'''
for directory in [GOOD_TS,BAD_TS,OLD_TS]:(directory/'tsconfig.json').write_text(TS_CONFIG)
(GOOD_TS/'code.ts').write_text(TS_CODE);(BAD_TS/'code.ts').write_text('export function TsRecoverableOld() { return 1; }\n')
(OLD_TS/'code.ts').write_text('export function TsUnsupportedBody() { return 0; }\n')
(OLD_TS/'package.json').write_text('{"name":"owned-unsupported-compiler","private":true,"devDependencies":{"typescript":"5.8.3"}}')
(GOOD_CS/'Good.csproj').write_text(CS_PROJECT);(BAD_CS/'Bad.csproj').write_text(CS_PROJECT)
(GOOD_CS/'Code.cs').write_text(CS_CODE);(BAD_CS/'Old.cs').write_text('public class CsRecoverableOld { public int Read() => 1; }\n')
# Under/over cases exercise the actual source envelope, not increased frame caps.
for directory,extension,prefix in [(GOOD_TS,'ts','Ts'),(GOOD_CS,'cs','Cs')]:
 for label,amount in [('NearLimit',7*1024*1024),('Oversized',9*1024*1024)]:
  suffix=f'export function {prefix}{label}() {{ return 1; }}\n' if extension=='ts' else f'public class {prefix}{label} {{ public int Read() => 1; }}\n'
  (directory/f'{label}.{extension}').write_text('/* π😀 saved source\n'+'x'*amount+'\n*/\n'+suffix)
CHECKS=[];DETAILS={};BUILD={};database=None;passed=False;failure=None;PHASE='dependency_setup'
sha=lambda path:hashlib.sha256(Path(path).read_bytes()).hexdigest()

def run(argv,*,input=None,check=True,timeout=120,cwd=ROOT):
 argv=list(map(str,argv));started=time.monotonic()
 try:p=subprocess.run(argv,cwd=cwd,env=ENV,input=input,text=True,capture_output=True,timeout=timeout)
 except subprocess.TimeoutExpired as error:
  with (RUN/'commands.jsonl').open('a') as out:out.write(json.dumps({'phase':PHASE,'argv':argv,'timeout':timeout,'stdout':str(error.stdout),'stderr':str(error.stderr)})+'\n')
  raise
 with (RUN/'commands.jsonl').open('a') as out:out.write(json.dumps({'phase':PHASE,'argv':argv,'cwd':str(cwd),'exit':p.returncode,'stdout':p.stdout,'stderr':p.stderr,'elapsed_ms':(time.monotonic()-started)*1000})+'\n')
 assert not check or p.returncode==0,(argv,p.stdout,p.stderr)
 return p

def cli(*args,**kwargs):return run([HOST,*args],**kwargs)
def value(*args):return json.loads(cli(*args).stdout)
def status():return next(s for s in value('coordinator','status')['workspaces'] if s['workspace_root']==str(WORK))
def wait(predicate,description):
 until=time.monotonic()+120;last=None
 while time.monotonic()<until:
  last=status()
  if predicate(last):return last
  time.sleep(.05)
 raise AssertionError((description,last))
def ready(minimum=1):return wait(lambda s:s['phase']=='idle' and s['generation']>=minimum and s['observed_epoch']==s['published_epoch'],'saved partial publication')
def query(request,host='cli'):
 encoded=json.dumps(request)
 if host=='cli':response=value('query',database,encoded)
 elif host=='node':
  script='import {Index} from '+json.dumps(NODE.as_uri())+';console.log(JSON.stringify(await new Index(process.argv[1]).query(JSON.parse(process.argv[2]))));'
  response=json.loads(run(['node','--input-type=module','-e',script,database,encoded]).stdout)
 else:response=json.loads(run(['dotnet',SMOKE,database,encoded]).stdout)
 assert 'result' in response,response
 return response['result']
def symbols(name):return query({'op':'symbols','name':name,'limit':1000})['symbols']
def one(name):
 found=symbols(name);assert len(found)==1,(name,found);return found[0]
def calls(name,op='callees',host='cli'):
 return query({'op':op,'symbol_id':one(name)['id'],'exact_only':True,'max_depth':1,'limit':100},host)['graph']
def coverage():return query({'op':'coverage','max_bytes':262144,'response_max_bytes':2097152})['coverage']
def controls():
 if (DATA/'c.sock').exists():
  cli('cancel',WORK,check=False,timeout=20);cli('coordinator','stop',check=False,timeout=30)
  until=time.monotonic()+10
  while (DATA/'c.sock').exists() and time.monotonic()<until:time.sleep(.05)
  assert not (DATA/'c.sock').exists(),'owned coordinator remained after cleanup'
atexit.register(controls)
try:
 producer=ROOT/f'.artifacts/build/{PROFILE}/build-record.json';record=json.loads(producer.read_text())
 artifacts=[HOST,NODE,NODE.parent/'know_code.node',SMOKE,SMOKE.parent/'KnowCode.dll']
 BUILD={'source_revision':record['source_revision'],'build_id':record['build_id'],'producer_sha256':sha(producer),
  'outputs_sha256':{str(path.relative_to(ROOT)):sha(path) for path in artifacts},'harness_sha256':sha(__file__)}
 assert record['status']=='complete' and all(record['outputs'][path]==digest for path,digest in BUILD['outputs_sha256'].items())
 # Install a real pinned unsupported compiler, not a version-string-only mock.
 run(['npm','install','--ignore-scripts','--no-audit','--no-fund','--package-lock=true'],cwd=OLD_TS)
 DETAILS['unsupported_compiler']={'package_json_sha256':sha(OLD_TS/'node_modules/typescript/package.json'),
  'package_lock_sha256':sha(OLD_TS/'package-lock.json'),'version':json.loads((OLD_TS/'node_modules/typescript/package.json').read_text())['version']}
 assert DETAILS['unsupported_compiler']['version']=='5.8.3'
 PHASE='managed_analysis'
 registration=value('watch',WORK);database=registration['database']
 value('trust',WORK);initial=ready()
 assert symbols('TsRecoverableOld') and symbols('CsRecoverableOld')
 first=coverage();DETAILS['initial_coverage']=first
 assert any(c['project_path']=='ts/old-version/tsconfig.json' and c['status']=='unavailable' and any(d['code']=='unsupported_compiler' for d in c.get('diagnostics',[])) for c in first),first
 assert not symbols('TsUnsupportedBody')
 for language,prefix in [('typescript','Ts'),('csharp','Cs')]:
  assert symbols(prefix+'NearLimit') and not symbols(prefix+'Oversized')
  entry=next(c for c in first if c['language']==language and c['project_path'].endswith('good/'+('tsconfig.json' if language=='typescript' else 'Good.csproj')))
  assert entry['status']=='partial' and any(d['code']=='artifact_too_large' for d in entry.get('diagnostics',[])),entry
 CHECKS.append('real_unsupported_TS_and_encoded_source_gaps_preserve_healthy_TS_Csharp_bodies')
 # The current public graph operator must return self-calls, not just stored rows.
 for prefix in ['Ts','Cs']:
  for op in ['callers','callees']:
   graph=calls(prefix+'Recursive',op)
   self_findings=[f for f in graph['findings'] if f['symbol']['name']==prefix+'Recursive']
   assert len(self_findings)==1 and len(self_findings[0]['paths'])==1,self_findings
   path=self_findings[0]['paths'][0];assert len(path)==1 and path[0]['kind']=='call' and path[0]['resolution']=='compiler_bound',path
  graph=query({'op':'impact','symbol_id':one(prefix+'Recursive')['id'],'max_depth':10,'limit':100})['graph']
  assert all(f['symbol']['name']!=prefix+'Recursive' for f in graph['findings']),graph
  mutual=calls(prefix+'MutualA');assert any(f['symbol']['name']==prefix+'MutualB' for f in mutual['findings']),mutual
 for language,prefix,writes in [('typescript','Ts',['Write','TupleWrite','DestructureWrite','BracketWrite']),('csharp','Cs',['Write','TupleWrite'])]:
  def accessors(suffix):
   return [f['symbol'] for f in calls(prefix+suffix)['findings'] if f['symbol']['kind'] in ['accessor','method'] and ('value' in f['symbol']['name'].lower())]
  # TS emits labeled synthetic get/set nodes; Roslyn accessor names are get_Value/set_Value.
  for suffix in writes:
   names=[s['name'].lower() for s in accessors(suffix)]
   assert any(name.startswith(('set ','set_')) for name in names) and not any(name.startswith(('get ','get_')) for name in names),(prefix,suffix,names)
  read=[s['name'].lower() for s in accessors('Read')]
  assert any(name.startswith(('get ','get_')) for name in read) and not any(name.startswith(('set ','set_')) for name in read),(prefix,read)
  for suffix in ['Compound','Coalesce']:
   names=[s['name'].lower() for s in accessors(suffix)]
   assert any(name.startswith(('get ','get_')) for name in names) and any(name.startswith(('set ','set_')) for name in names),(prefix,suffix,names)
 # One actual graph response has identical native projection across bindings.
 recursive=one('TsRecursive')['id'];request={'op':'callees','symbol_id':recursive,'exact_only':True,'max_depth':1}
 reference=query(request)
 for host in ['node','dotnet']:assert query(request,host)==reference
 CHECKS.append('actual_TS_Csharp_recursion_and_nested_coalescing_accessor_directions_through_shared_native_queries')
 (BAD_TS/'tsconfig.json').write_text('{"compilerOptions":')
 (BAD_CS/'Bad.csproj').write_text('<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup>')
 (GOOD_TS/'code.ts').write_text(TS_CODE+'export function TsFresh() { return 2; }\n')
 (GOOD_CS/'Code.cs').write_text(CS_CODE+'public class CsFresh {}\n')
 partial=ready(initial['generation']+1)
 assert symbols('TsFresh') and symbols('CsFresh')
 assert not symbols('TsRecoverableOld') and not symbols('CsRecoverableOld'),'failed project retained old facts as current'
 second=coverage();DETAILS['failed_coverage']=second
 for path in ['ts/bad/tsconfig.json','cs/bad/Bad.csproj']:
  unavailable=[c for c in second if c['project_path']==path]
  assert unavailable and all(c['status']=='unavailable' and c.get('diagnostics') for c in unavailable),(path,unavailable)
 assert not any(c['inferred'] and (c.get('analysis_options') or {}).get('inferredRoot','').startswith('ts/bad') for c in second),second
 assert query({'op':'symbols','name':'TsFresh'})['freshness']=='current'
 CHECKS.append('bad_TS_config_and_Csharp_evaluation_are_unavailable_without_stale_fact_merge_or_inferred_fallback')
 (BAD_TS/'tsconfig.json').write_text(TS_CONFIG);(BAD_CS/'Bad.csproj').write_text(CS_PROJECT)
 repaired=ready(partial['generation']+1)
 assert symbols('TsRecoverableOld') and symbols('CsRecoverableOld')
 assert all(c['status']!='unavailable' for c in coverage() if c['project_path'] in ['ts/bad/tsconfig.json','cs/bad/Bad.csproj'])
 CHECKS.append('saved_project_repairs_recover_compilation_contexts_without_whole_language_failure')
 # Deliberate extraction fault after successful planning must remain fatal.
 # This is an owned faulty compiler package, not a hidden production switch.
 fault=WORK/'ts/fault';(fault/'node_modules/typescript').mkdir(parents=True)
 (fault/'tsconfig.json').write_text(TS_CONFIG);(fault/'code.ts').write_text('export const faultSource=1;\n')
 (fault/'package.json').write_text('{"private":true,"devDependencies":{"typescript":"5.9.3"}}')
 (fault/'node_modules/typescript/package.json').write_text('{"name":"typescript","version":"5.9.3","main":"index.cjs"}')
 actual=ROOT/'node_modules/typescript/lib/typescript.js';assert actual.exists()
 (fault/'node_modules/typescript/index.cjs').write_text('const ts=require('+json.dumps(str(actual))+');module.exports=new Proxy(ts,{get(target,key){if(key==="createProgram")return ()=>{throw new Error("owned unexpected extraction fault")};return Reflect.get(target,key)}});\n')
 (GOOD_TS/'code.ts').write_text((GOOD_TS/'code.ts').read_text()+'export function TsShouldNotPublish() { return 3; }\n')
 failed=wait(lambda s:s['phase']=='failed','unexpected worker extraction failure')
 assert failed['generation']==repaired['generation'],failed
 retained=query({'op':'symbols','name':'TsFresh'})
 assert retained['freshness']=='stale' and retained['generation']==repaired['generation'] and not symbols('TsShouldNotPublish'),retained
 DETAILS['fatal_worker_status']=failed
 shutil.rmtree(fault);final=ready(repaired['generation']+1)
 assert symbols('TsShouldNotPublish') and query({'op':'symbols','name':'TsFresh'})['freshness']=='current'
 CHECKS.append('unknown_extraction_fault_preserves_prior_generation_stale_and_never_publishes_incomplete_stream')
 assert sha(producer)==BUILD['producer_sha256'] and all(sha(ROOT/path)==digest for path,digest in BUILD['outputs_sha256'].items())
 passed=True
except BaseException:
 failure=traceback.format_exc();(RUN/'failure.txt').write_text(failure);raise
finally:
 try:controls()
 except BaseException:
  passed=False;(RUN/'cleanup-failure.txt').write_text(traceback.format_exc())
  if failure is None:raise
 finally:
  summary={'passed':passed,'run':str(RUN),'checks':CHECKS,'details':DETAILS,'build':BUILD,'failure':failure,
   'cleanup':{'owned_socket_exists':(DATA/'c.sock').exists()},'limits':['finite owned TS/Csharp cases; no full suite/performance/release/corpus certification']}
  (RUN/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary,indent=2))
