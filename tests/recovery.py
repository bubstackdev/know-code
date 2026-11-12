"""Owned TS coordinator recovery; logical/native read fencing is certified by root.

This finite lane fixture checks real publication/rebuild/restart/retry behavior.
It reads private SQLite facts only to verify saved-source bytes, not to implement
another user query path. Every service/data/process is isolated from live state.
"""
from cli_assertions import check_exit
from fixture_trust import grant_fixture
from pathlib import Path
import hashlib,json,os,sqlite3,subprocess,time
ROOT=Path(__file__).resolve().parents[1]
ARTIFACTS=Path(os.environ.get('KNOW_CODE_TEST_ARTIFACT_ROOT',ROOT/'.artifacts/ir'))
RUN=ARTIFACTS/format(time.time_ns(),'x')[-10:];DATA=RUN/'d';A=RUN/'a';B=RUN/'b'
A.mkdir(parents=True);B.mkdir();PROFILE=os.environ.get('KNOW_CODE_BUILD_PROFILE','debug');HOST=ROOT/f'target/{PROFILE}/know-code'
ENV=dict(os.environ,KNOW_CODE_DATA_DIR=str(DATA),DOTNET_CLI_TELEMETRY_OPTOUT='1',DOTNET_NOLOGO='1')
ENV['KNOW_CODE_QUERY_SCOPE']=str(RUN/'query-admission')
for key in ['KNOW_CODE_COORDINATOR','NODE_OPTIONS','NODE_PATH']:ENV.pop(key,None)
(A/'value.ts').write_text('export const alpha = "saved original";\n');(B/'value.ts').write_text('export const beta = 1;\n')
checks=[];passed=False;events=RUN/'commands.jsonl';databases={}

def cli(*args,check=True):
 p=subprocess.run([str(HOST),*map(str,args)],cwd=ROOT,env=ENV,capture_output=True,text=True,timeout=120)
 with events.open('a') as log:log.write(json.dumps({'args':list(map(str,args)),'exit':p.returncode,'stdout':p.stdout,'stderr':p.stderr})+'\n')
 check_exit(p,[HOST,*args],check)
 return p

def value(*args):return json.loads(cli(*args).stdout)
def status(root):return next(s for s in value('coordinator','status')['workspaces'] if s['workspace_root']==str(root))
def wait(root,predicate,description):
 until=time.monotonic()+60;last=None
 while time.monotonic()<until:
  last=status(root)
  if predicate(last):return last
  time.sleep(.05)
 raise AssertionError((description,last))
def ready(root,minimum=1):return wait(root,lambda s:s['phase']=='idle' and s['generation']>=minimum and s['observed_epoch']==s['published_epoch'],'saved publication')
def state(root):return json.loads(Path(databases[root]).with_suffix('.lifecycle.json').read_text())
def physical(root):
 d=state(root);selected=d['selected'];assert not selected['legacy'],d
 return Path(databases[root]).parent/'indexes'/selected['id']/'index.db'
def saved(root):
 with sqlite3.connect(physical(root)) as db:
  generation=db.execute('SELECT current_generation FROM state').fetchone()[0]
  contents=[bytes(r[0]).decode() for r in db.execute('SELECT utf8 FROM contents')]
 return generation,contents

def damage(root):
 path=physical(root)
 with sqlite3.connect(path) as db:assert db.execute('PRAGMA wal_checkpoint(TRUNCATE)').fetchone()==(0,0,0)
 with path.open('r+b') as file:file.write(b'broken-db-header');file.flush();os.fsync(file.fileno())
 return state(root)['selected']['id']
def stop():
 cli('coordinator','stop',check=False);until=time.monotonic()+15
 while (DATA/'c.sock').exists() and time.monotonic()<until:time.sleep(.05)
 assert not (DATA/'c.sock').exists(),'owned coordinator did not stop'
def invariant(root):
 d=state(root);assert d['version']==2 and not d.get('initializing',False),d
 assert isinstance(d['high_water'],int);return d
try:
 defaults=value('resources');assert defaults['rebuild_disk_mib']==2048 and defaults['min_free_disk_mib']==512,defaults
 value('resources','--min-free-disk-mib',0)
 for root in [A,B]:
  grant_fixture(HOST,root,ENV,RUN,RUN/'fixture-trust.jsonl')
  value('watch',root);initial=ready(root);databases[root]=initial['database']
 first=ready(A);initial_b=ready(B)
 assert saved(A)[0]==first['generation'] and (A/'value.ts').read_text() in saved(A)[1]
 assert not Path(databases[A]).is_file(),'new registration used unleased legacy database'
 original_settings=Path(databases[A]).with_suffix('.settings.json')
 settings_before=original_settings.read_bytes() if original_settings.exists() else None
 prep=Path(databases[A]).with_suffix('.preparation.json');prep_before=prep.read_bytes() if prep.exists() else None
 checks.append('new_registration_uses_guarded_physical_index_and_saved_source')
 # Offline corruption must be detected automatically by startup reconciliation.
 stop();old=damage(A);value('watch',A);recovered=ready(A,first['generation']+1)
 d=invariant(A);assert d['selected']['id']!=old and d['pending'] is None and not d['failed'],d
 assert saved(A)[0]==recovered['generation'] and (A/'value.ts').read_text() in saved(A)[1]
 assert prep_before==(prep.read_bytes() if prep.exists() else None),'recovery forked/reran preparation checkpoints'
 assert settings_before==(original_settings.read_bytes() if original_settings.exists() else None)
 ready(B);assert saved(B)[0]==initial_b['generation'],'healthy sibling unnecessarily rebuilt'
 checks.append('automatic_startup_corruption_rebuild_preserves_state_and_healthy_sibling')
 # An impossible reserve defers before allocating a rebuild attempt. Shared
 # pressure also defers sibling writes, while its old coherent graph survives.
 free=os.statvfs(DATA);free_mib=free.f_bavail*free.f_frsize//(1024*1024)
 value('resources','--min-free-disk-mib',free_mib+max(1024,free_mib//10))
 stop();failed_old=damage(A);value('watch',A)
 failure=wait(A,lambda s:s['phase']=='resource_limited' and s['outcome_code']=='disk_pressure','recovery preflight deferral')
 d=invariant(A);assert d['selected']['id']==failed_old and not d['failed'] and d['pending'] is None,d
 candidates=len(list((Path(databases[A]).parent/'indexes').iterdir()))
 (A/'value.ts').write_text('export const alpha = "saved while blocked";\n')
 (B/'value.ts').write_text('export const beta = 2;\n')
 wait(B,lambda s:s['phase']=='resource_limited' and s['outcome_code']=='disk_pressure','shared sibling write deferral')
 assert saved(B)[0]==initial_b['generation'],'pressure replaced a coherent sibling graph'
 for _ in range(4):status(A);time.sleep(.1)
 assert state(A)['selected']['id']==failed_old and not state(A)['failed'],state(A)
 assert len(list((Path(databases[A]).parent/'indexes').iterdir()))==candidates,'preflight consumed another candidate'
 stop();value('watch',A)
 wait(A,lambda s:s['phase']=='resource_limited' and s['outcome_code']=='disk_pressure','persisted preflight deferral')
 assert len(list((Path(databases[A]).parent/'indexes').iterdir()))==candidates,'restart consumed a blocked attempt'
 checks.append('rebuild_preflight_defers_across_save_and_restart_without_consuming_attempt')
 value('resources','--min-free-disk-mib',0)
 final=ready(A,recovered['generation']+1)
 d=invariant(A);assert d['selected']['id']!=failed_old and not d['failed'] and d['pending'] is None,d
 assert (A/'value.ts').read_text() in saved(A)[1]
 assert ready(B)['generation']>initial_b['generation']
 assert prep_before==(prep.read_bytes() if prep.exists() else None)
 checks.append('space_policy_recovery_automatically_rebuilds_current_saved_state_and_advances_generation')
 stop();value('watch',A);assert ready(A)['generation']==final['generation'],'healthy restart published unnecessarily'
 checks.append('healthy_replacement_restart_is_noop')
 # Malformed/incompatible lifecycle and invalid health metadata belong only to
 # A. Presence and dispatch must keep B's shared coordinator alive and writable;
 # fixing metadata alone does not authorize another analysis of A.
 selector=Path(databases[A]).with_suffix('.lifecycle.json')
 healthy_selector=selector.read_bytes()
 health=physical(A).parent/'health';healthy_health=health.read_bytes()
 for fault,code in [('malformed','invalid_request'),('version','update_required'),('health','invalid_request')]:
  if fault=='malformed':selector.write_text('{broken lifecycle')
  elif fault=='version':
   wrong=json.loads(healthy_selector);wrong['version']=999;selector.write_text(json.dumps(wrong))
   stop();value('watch',B)
  else:
   # Keep the page size stable while readers may have mappings. Invalid magic
   # is an ownership/metadata failure, never SQLite corruption evidence.
   with health.open('r+b') as file:file.write(b'BADHEALT');file.flush();os.fsync(file.fileno())
  last_b=ready(B)['generation']
  (A/'value.ts').write_text(f'export const alpha = "metadata {fault}";\n')
  (B/'value.ts').write_text(f'export const beta = "healthy during {fault}";\n')
  failure=wait(A,lambda s:s['phase']=='failed','isolated lifecycle fault '+fault)
  assert failure['outcome_code']==code,failure
  ready(B,last_b+1)
  assert state(B)['failed'] is None,'healthy sibling was incorrectly rebuilt'
  if fault=='health':
   with health.open('r+b') as file:file.write(healthy_health);file.flush();os.fsync(file.fileno())
  else:selector.write_bytes(healthy_selector)
  old_generation=failure['generation']
  time.sleep(2.2)
  still_blocked=status(A)
  assert still_blocked['phase']=='failed' and still_blocked['generation']==old_generation,still_blocked
  cli('index',A);ready(A,old_generation+1)
  healthy_selector=selector.read_bytes();healthy_health=health.read_bytes()
 checks.append('workspace_lifecycle_and_health_faults_preserve_healthy_sibling_updates_and_require_refresh')
 passed=True
finally:
 stop()
 summary={'passed':passed,'profile':PROFILE,'run':str(RUN),'checks':checks,'databases':{str(k):v for k,v in databases.items()},'source_revision':subprocess.run(['git','rev-parse','HEAD'],cwd=ROOT,capture_output=True,text=True).stdout.strip(),'source_sha256':{p:hashlib.sha256((ROOT/p).read_bytes()).hexdigest() for p in ['crates/core/src/recovery.rs','crates/core/src/resource.rs','crates/core/src/workspace.rs','crates/core/src/update.rs','crates/core/src/store.rs','crates/host/src/coordinator.rs']},'host_sha256':hashlib.sha256(HOST.read_bytes()).hexdigest(),'limits':['lane coordinator/TS/publication/private-SQLite fixture only','root certifies logical native/MCP/read-view/snapshot/cursor fencing on assembled source','no full Debug/performance/live installation/corpus mutation']}
 (RUN/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary,indent=2))
