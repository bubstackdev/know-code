"""Owned real coordinator trust boundary; run after a matching build.

No test changes HOME, the live coordinator, or original corpora. Compiler-module
side effects make an accidental pre-grant worker visible. All grants use the
real public CLI, never private receipt synthesis.
"""
from pathlib import Path
import atexit,hashlib,json,os,sqlite3,subprocess,time,traceback
ROOT=Path(__file__).resolve().parents[1];RUN=ROOT/'.artifacts/pt'/format(time.time_ns(),'x')[-9:]
DATA=RUN/'d';SCOPE=RUN/'s';A=RUN/'a';B=RUN/'b';C=RUN/'c';MARKER=RUN/'compiler-requires.txt'
for root in [A,B,C]:root.mkdir(parents=True)
PROFILE=os.environ.get('KNOW_CODE_BUILD_PROFILE','debug');HOST=ROOT/f'target/{PROFILE}/know-code'
ENV=dict(os.environ,KNOW_CODE_DATA_DIR=str(DATA),KNOW_CODE_QUERY_SCOPE=str(SCOPE),
 DOTNET_CLI_TELEMETRY_OPTOUT='1',DOTNET_NOLOGO='1',MSBUILDDISABLENODEREUSE='1',DOTNET_CLI_USE_MSBUILD_SERVER='0')
for key in ['KNOW_CODE_COORDINATOR','KNOW_CODE_RUNTIME_DIR','NODE_PATH','NODE_OPTIONS']:ENV.pop(key,None)
CONFIG='{"compilerOptions":{"strict":true,"noEmit":true},"include":["*.ts"]}'
for root in [A,B,C]:
 (root/'tsconfig.json').write_text(CONFIG);(root/'value.ts').write_text('export function trustedValue() { return 1; }\n')
# This is a deliberately executable project-selected compiler module. It forwards
# the actual bundled compiler, but records if/when the worker requires it.
module=A/'node_modules/typescript';module.mkdir(parents=True)
(module/'package.json').write_text('{"name":"typescript","version":"5.9.3","main":"index.cjs"}')
(module/'index.cjs').write_text('require("node:fs").appendFileSync('+json.dumps(str(MARKER))+',"required\\n");module.exports=require('+json.dumps(str(ROOT/'node_modules/typescript/lib/typescript.js'))+');\n')
(A/'package.json').write_text('{"name":"owned-project-trust","private":true,"devDependencies":{"typescript":"5.9.3"}}')
CHECKS=[];DETAILS={};BUILD={};PASSED=False;FAILURE=None;PHASE='pregrant';DATABASES={}
sha=lambda path:hashlib.sha256(Path(path).read_bytes()).hexdigest()
def run(argv,*,check=True,timeout=120):
 argv=list(map(str,argv));start=time.monotonic()
 try:p=subprocess.run(argv,cwd=ROOT,env=ENV,text=True,capture_output=True,timeout=timeout)
 except subprocess.TimeoutExpired as error:
  with (RUN/'commands.jsonl').open('a') as stream:stream.write(json.dumps({'phase':PHASE,'argv':argv,'timeout':timeout,'stdout':str(error.stdout),'stderr':str(error.stderr)})+'\n')
  raise
 with (RUN/'commands.jsonl').open('a') as stream:stream.write(json.dumps({'phase':PHASE,'argv':argv,'exit':p.returncode,'stdout':p.stdout,'stderr':p.stderr,'elapsed_ms':(time.monotonic()-start)*1000})+'\n')
 assert not check or p.returncode==0,(argv,p.stdout,p.stderr)
 return p
def cli(*args,**kwargs):return run([HOST,*args],**kwargs)
def value(*args):return json.loads(cli(*args).stdout)
def all_status():return value('coordinator','status')['workspaces']
def status(root):return next(s for s in all_status() if s['workspace_root']==str(root))
def wait(root,predicate,description):
 until=time.monotonic()+120;last=None
 while time.monotonic()<until:
  last=status(root)
  if predicate(last):return last
  time.sleep(.05)
 raise AssertionError((description,last))
def ready(root,minimum=1):return wait(root,lambda s:s['phase']=='idle' and s['generation']>=minimum and s['observed_epoch']==s['published_epoch'],'trusted saved publication')
def query(root,name):
 result=value('query',DATABASES[root],json.dumps({'op':'symbols','name':name,'freshness_wait_ms':0,'coverage_detail':'full','response_max_bytes':2097152}))
 assert 'result' in result,result
 return result['result']
def trust(root,allowed=True):return value('trust',root,*([] if allowed else ['--revoke']))
def requires():return MARKER.read_text().count('required\n') if MARKER.exists() else 0
def quiet(root,generation,seconds=2.2):
 until=time.monotonic()+seconds
 while time.monotonic()<until:
  s=status(root);assert s['generation']==generation and s['phase']=='trust_required',s
  time.sleep(.1)
 return s
def baseline(database):
 with sqlite3.connect(DATA/'registry.db') as db:
  return db.execute('SELECT outcome_code,failed_input_hash,cancel_pending,error FROM workspaces WHERE database=?',(database,)).fetchone()
def cleanup():
 if (DATA/'c.sock').exists():
  cli('coordinator','stop',check=False,timeout=30)
  until=time.monotonic()+10
  while (DATA/'c.sock').exists() and time.monotonic()<until:time.sleep(.05)
  assert not (DATA/'c.sock').exists(),'owned coordinator remained after cleanup'
atexit.register(cleanup)
try:
 producer=ROOT/f'.artifacts/build/{PROFILE}/build-record.json';record=json.loads(producer.read_text())
 BUILD={'build_id':record['build_id'],'source_revision':record['source_revision'],'producer_sha256':sha(producer),'host_sha256':sha(HOST),'harness_sha256':sha(__file__)}
 assert record['status']=='complete' and record['outputs'][str(HOST.relative_to(ROOT))]==BUILD['host_sha256']
 # Permission can be granted before parent discovery/enrollment, without enrolling.
 granted=trust(B);assert granted['phase']=='unregistered',granted
 assert all(s['workspace_root']!=str(B) for s in all_status())
 DATABASES[B]=value('watch',B)['database'];b=ready(B)
 assert b['requested_settings']['project_execution_allowed']
 CHECKS.append('real_pregrant_is_inert_until_registration_and_does_not_bypass_discovery')
 DATABASES[A]=value('watch',A)['database'];a=status(A)
 assert a['phase']=='trust_required' and a['generation']==0 and not requires(),a
 noindex=cli('query',DATABASES[A],'{"op":"symbols","name":"trustedValue","freshness_wait_ms":0}',check=False)
 assert json.loads(noindex.stdout)['error']['code'] in ['not_ready','trust_required'],(noindex.stdout,noindex.stderr)
 # Script permission cannot synthesize the separate execution grant.
 value('settings',A,json.dumps({'allow_install_scripts':True}))
 assert not status(A)['requested_settings']['project_execution_allowed']
 quiet(A,0);assert not requires()
 value('settings',A,json.dumps({'allow_install_scripts':False}))
 cleanup();value('watch',A);quiet(A,0)
 assert not requires(),'registration/settings/restart executed project-selected compiler'
 PHASE='granted_analysis';trust(A);a=ready(A)
 assert requires()>0 and query(A,'trustedValue')['symbols']
 assert a['requested_settings']['project_execution_allowed'] and not a['requested_settings']['node']['allow_install_scripts'],a
 CHECKS.append('explicit_registration_settings_and_restart_never_grant_execution_real_module_runs_only_after_personal_trust')
 trust(A,False);before=requires();(A/'value.ts').write_text('export function afterRevocation() { return 2; }\n')
 quiet(A,a['generation']);stale=query(A,'trustedValue')
 assert stale['freshness']=='stale' and stale['symbols'] and not query(A,'afterRevocation').get('symbols',[]),stale
 assert requires()==before
 cleanup();value('watch',A);quiet(A,a['generation']);assert requires()==before
 trust(A);a=ready(A,a['generation']+1)
 assert query(A,'afterRevocation')['symbols'] and query(A,'afterRevocation')['freshness']=='current'
 CHECKS.append('revocation_blocks_saved_analysis_keeps_prior_graph_stale_and_survives_restart_then_regrant_resumes')
 # A real failed local dependency restoration remains deduplicated through
 # revoke/regrant, and through a later source save. No network failure is needed.
 PHASE='failed_preparation';manifest=json.loads((A/'package.json').read_text());manifest['dependencies']={'owned-missing':'file:'+str(RUN/'absent.tgz')}
 (A/'package.json').write_text(json.dumps(manifest));a=ready(A,a['generation']+1)
 result=query(A,'afterRevocation')
 assert any(d['code']=='dependency_restore_failed' and 'ENOENT' in d['message'] for c in result['coverage'] for d in c.get('diagnostics',[])),result
 checkpoint=Path(DATABASES[A]).with_suffix('.preparation.json');stamp=checkpoint.stat().st_mtime_ns;checkpoint_hash=sha(checkpoint)
 count=requires();trust(A,False);trust(A);time.sleep(2.2)
 assert status(A)['generation']==a['generation'] and requires()==count
 assert checkpoint.stat().st_mtime_ns==stamp and sha(checkpoint)==checkpoint_hash
 (A/'value.ts').write_text((A/'value.ts').read_text()+'export const afterRegrantSave=3;\n');a=ready(A,a['generation']+1)
 assert query(A,'afterRegrantSave')['symbols'] and checkpoint.stat().st_mtime_ns==stamp
 CHECKS.append('regrant_does_not_retry_preparation_or_create_new_restore_authorization')
 # Cancellation is permitted while untrusted, but baseline capture runs no
 # compiler and granting trust does not erase that saved cancellation.
 PHASE='untrusted_cancel';DATABASES[C]=value('watch',C)['database'];value('cancel',C)
 until=time.monotonic()+120
 while time.monotonic()<until:
  saved=baseline(DATABASES[C])
  if saved[0]=='cancelled' and saved[1] and not saved[2]:break
  time.sleep(.05)
 else:raise AssertionError(('cancellation baseline not recorded',saved,status(C)))
 assert status(C)['phase']=='trust_required' and status(C)['generation']==0
 trust(C);time.sleep(.3);assert status(C)['phase']=='cancelled' and status(C)['generation']==0
 (C/'value.ts').write_text('export const afterCancelledSave=4;\n');ready(C)
 assert query(C,'afterCancelledSave')['symbols']
 CHECKS.append('untrusted_cancellation_captures_baseline_without_execution_and_grant_preserves_cancel_until_save')
 # A missing path is still revocable/unregisterable; a returning directory does
 # not inherit the revoked grant from its retained identity.
 PHASE='missing_workspace';backup=RUN/'b-retained';B.rename(backup)
 trust(B,False);cli('unwatch',B);backup.rename(B)
 DATABASES[B]=value('watch',B)['database'];assert status(B)['phase']=='trust_required'
 assert not status(B)['requested_settings']['project_execution_allowed']
 CHECKS.append('revoke_and_unregister_missing_root_clear_execution_permission_for_returning_identity')
 DETAILS['final_status']=all_status();DETAILS['failed_preparation_sha256']=checkpoint_hash
 assert sha(producer)==BUILD['producer_sha256'] and sha(HOST)==BUILD['host_sha256']
 PASSED=True
except BaseException:
 FAILURE=traceback.format_exc();(RUN/'failure.txt').write_text(FAILURE);raise
finally:
 try:cleanup()
 except BaseException:
  PASSED=False;(RUN/'cleanup-failure.txt').write_text(traceback.format_exc())
  if FAILURE is None:raise
 finally:
  summary={'passed':PASSED,'run':str(RUN),'checks':CHECKS,'details':DETAILS,'build':BUILD,'failure':FAILURE,'owned_socket_exists':(DATA/'c.sock').exists()}
  (RUN/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary,indent=2))
