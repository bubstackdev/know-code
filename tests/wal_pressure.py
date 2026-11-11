"""Finite managed WAL pressure: actual bundled-SQLite raw views and TS workers.

No builds, live runtime, account admission state, original corpora or HOME edits.
The ignored Rust holder is the same core test binary built with this checkout.
"""
from pathlib import Path
from index_paths import physical_database
import atexit,hashlib,json,os,selectors,shlex,shutil,sqlite3,subprocess,time,traceback
ROOT=Path(__file__).resolve().parents[1];RUN=ROOT/'.artifacts/wp'/format(time.time_ns(),'x')[-9:]
DATA=RUN/'d';A=RUN/'a';B=RUN/'b';BIN=RUN/'bin';SCOPE=RUN/'s';GATE=RUN/'worker-gate'
for path in [A,B,BIN]:path.mkdir(parents=True,exist_ok=True)
PROFILE=os.environ.get('KNOW_CODE_BUILD_PROFILE','debug');HOST=ROOT/f'target/{PROFILE}/know-code'
ENV=dict(os.environ,KNOW_CODE_DATA_DIR=str(DATA),KNOW_CODE_QUERY_SCOPE=str(SCOPE),
 DOTNET_CLI_TELEMETRY_OPTOUT='1',DOTNET_NOLOGO='1',MSBUILDDISABLENODEREUSE='1',DOTNET_CLI_USE_MSBUILD_SERVER='0')
for key in ['KNOW_CODE_COORDINATOR','KNOW_CODE_RUNTIME_DIR','NODE_OPTIONS','NODE_PATH']:ENV.pop(key,None)
REAL_NODE=Path(shutil.which('node')).resolve();WRAPPER=BIN/'node'
WRAPPER.write_text('#!/bin/sh\nworker=false\nfor argument do\n case "$argument" in */providers/typescript/worker.mjs) worker=true;; esac\ndone\n'
 f'gate={shlex.quote(str(GATE))}\nif "$worker" && [ -f "$gate" ]; then\n echo "$$" > "$gate.ready"\n while [ -f "$gate" ]; do /bin/sleep 0.05; done\nfi\n'
 f'exec {shlex.quote(str(REAL_NODE))} "$@"\n');WRAPPER.chmod(0o755)
ENV['PATH']=str(BIN)+os.pathsep+ENV['PATH']
DB={};CHILDREN=[];CHECKS=[];DETAILS={};BUILD={};passed=False;failure=None
sha=lambda path:hashlib.sha256(Path(path).read_bytes()).hexdigest()
def text(revision):
 return f'/* owned saved revision {revision} π😀\0\n'+chr(97+revision%20)*(2*1024*1024)+f'\n*/\nexport function walTarget() {{ return {revision}; }}\n'
for root in [A,B]:
 (root/'value.ts').write_text(text(0) if root==A else 'export function walTarget() { return 0; }\n')
 (root/'tsconfig.json').write_text('{"compilerOptions":{"strict":true,"noEmit":true},"files":["value.ts"]}')

def log(value):
 with (RUN/'commands.jsonl').open('a') as out:out.write(json.dumps(value)+'\n')
def run(argv,check=True,timeout=120):
 argv=list(map(str,argv));started=time.monotonic()
 try:p=subprocess.run(argv,cwd=ROOT,env=ENV,text=True,capture_output=True,timeout=timeout)
 except subprocess.TimeoutExpired as error:
  log({'argv':argv,'timeout':timeout,'stdout':str(error.stdout),'stderr':str(error.stderr)});raise
 log({'argv':argv,'exit':p.returncode,'stdout':p.stdout,'stderr':p.stderr,'elapsed_ms':(time.monotonic()-started)*1000})
 assert not check or p.returncode==0,(argv,p.stdout,p.stderr)
 return p

def cli(*args,**kw):return run([HOST,*args],**kw)
def value(*args):return json.loads(cli(*args).stdout)
def status(root):return next(s for s in value('coordinator','status')['workspaces'] if s['workspace_root']==str(root))
def wait(root,predicate,description,seconds=90):
 until=time.monotonic()+seconds;last=None
 while time.monotonic()<until:
  last=status(root)
  if predicate(last):return last
  time.sleep(.05)
 raise AssertionError((description,last))
def ready(root,minimum=1):return wait(root,lambda s:s['phase']=='idle' and s['generation']>=minimum and s['observed_epoch']==s['published_epoch'],'current publication')
def pressure(root):return wait(root,lambda s:s['phase']=='resource_limited' and s['outcome_code']=='disk_pressure','disk pressure')
def lifecycle(root):return json.loads(Path(DB[root]).with_suffix('.lifecycle.json').read_text())
def query(root):return value('query',DB[root],'{"op":"symbols","name":"walTarget","freshness_wait_ms":0}')['result']
def pause_record(root):
 with sqlite3.connect(f'file:{DATA}/registry.db?mode=ro',uri=True) as connection:
  row=connection.execute('SELECT disk_pause,failed_input_hash FROM workspaces WHERE database=?',[DB[root]]).fetchone()
 return row

def stop():
 if (DATA/'c.sock').exists():cli('coordinator','stop',check=False)
 until=time.monotonic()+10
 while (DATA/'c.sock').exists() and time.monotonic()<until:time.sleep(.05)
 assert not (DATA/'c.sock').exists(),'owned coordinator did not stop'

def holder(root):
 candidates=[p for p in (ROOT/f'target/{PROFILE}/deps').glob('wal_pressure-*') if p.is_file() and p.suffix not in ['.d','.rlib','.rmeta'] and os.access(p,os.X_OK)]
 assert candidates,'matching cargo test binary for crates/core/tests/wal_pressure.rs is required'
 binary=max(candidates,key=lambda p:p.stat().st_mtime_ns)
 env=dict(ENV,KNOW_CODE_WAL_HOLDER_DATABASE=DB[root])
 stderr=(RUN/f'holder-{len(CHILDREN)}.stderr').open('wb')
 p=subprocess.Popen([str(binary),'--exact','hold_managed_read_view','--ignored','--nocapture'],cwd=ROOT,env=env,
  stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=stderr)
 selector=selectors.DefaultSelector();selector.register(p.stdout,selectors.EVENT_READ)
 child={'process':p,'selector':selector,'buffer':b'','stderr':stderr,'binary':str(binary),'sha256':sha(binary)};CHILDREN.append(child)
 receipt=holder_read(child);assert receipt['ready'],receipt
 DETAILS.setdefault('holders',[]).append({'binary':str(binary),'sha256':sha(binary),'ready':receipt,'pid':p.pid})
 return child

def holder_read(child):
 p=child['process'];until=time.monotonic()+10
 while time.monotonic()<until:
  if b'\n' in child['buffer']:
   line,child['buffer']=child['buffer'].split(b'\n',1)
   prefix=b'KC_WAL_HOLDER '
   position=line.find(prefix)
   if position>=0:
    reply=json.loads(line[position+len(prefix):]);log({'holder_pid':p.pid,'reply':reply});return reply
  elif child['selector'].select(.05):
   data=os.read(p.stdout.fileno(),65536);assert data,('holder EOF',p.poll());child['buffer']+=data
 raise AssertionError('holder reply timed out')
def holder_query(child):
 child['process'].stdin.write(b'{"op":"symbols","name":"walTarget","deadline_ms":120000}\n');child['process'].stdin.flush()
 return holder_read(child)['result']
def release(child):
 p=child['process']
 p.stdin.write(b'{"op":"release"}\n');p.stdin.flush();assert holder_read(child)['released']
 p.stdin.close();p.wait(timeout=10);assert p.returncode==0

def fill_wal(root,revision):
 held=holder(root);initial=holder_query(held);before=ready(root)['generation']
 for current in [revision,revision+1]:
  (root/'value.ts').write_text(text(current));ready(root,before+1);before=ready(root)['generation']
 physical=physical_database(DB[root]);used=sum(Path(str(physical)+suffix).stat().st_size for suffix in ['-wal','-shm'] if Path(str(physical)+suffix).exists())
 assert used>4*1024*1024,used
 cap=max(1,used//(1024*1024))
 value('resources','--temporary-disk-mib',cap)
 DETAILS.setdefault('wal_caps',[]).append({'used_bytes':used,'cap_mib':cap,'physical':str(physical)})
 return held,initial,before,cap

def damage(root):
 path=physical_database(DB[root])
 with sqlite3.connect(path) as db:assert db.execute('PRAGMA wal_checkpoint(TRUNCATE)').fetchone()[0]==0
 with path.open('r+b') as out:out.write(b'broken-owned-header');out.flush();os.fsync(out.fileno())
 return lifecycle(root)['selected']['id']
def cleanup():
 GATE.unlink(missing_ok=True)
 for child in CHILDREN:
  p=child['process']
  if p.poll() is None:p.kill()
  p.wait(timeout=5)
  if p.stdin and not p.stdin.closed:p.stdin.close()
  p.stdout.close();child['selector'].close();child['stderr'].close()
 CHILDREN.clear()
 if (DATA/'c.sock').exists():
  for root in DB:cli('cancel',root,check=False,timeout=20)
 stop()
atexit.register(cleanup)
try:
 record_path=ROOT/f'.artifacts/build/{PROFILE}/build-record.json';record=json.loads(record_path.read_text())
 assert record['status']=='complete' and record['outputs'][f'target/{PROFILE}/know-code']==sha(HOST)
 BUILD={'id':record['build_id'],'source_revision':record['source_revision'],'producer_sha256':sha(record_path),
  'host_sha256':sha(HOST),'harness_sha256':sha(__file__),'holder_source_sha256':sha(ROOT/'crates/core/tests/wal_pressure.rs'),
  'node_wrapper_sha256':sha(WRAPPER),'real_node':str(REAL_NODE),'real_node_sha256':sha(REAL_NODE)}
 value('resources','--min-free-disk-mib',0)
 for root in [A,B]:DB[root]=value('watch',root)['database'];ready(root)
 sibling=ready(B);held,old,before,cap=fill_wal(A,1)
 (A/'value.ts').write_text(text(3));paused=pressure(A)
 stale=query(A);assert stale['freshness']=='stale' and stale['generation']==before,stale
 assert stale['coordination']['outcome_code']=='disk_pressure'
 raw=holder_query(held);assert raw['generation']==old['generation'] and raw['symbols']==old['symbols'],raw
 assert query(B)['freshness']=='current' and ready(B)['generation']==sibling['generation']
 baseline=(pause_record(A),lifecycle(A)['high_water']);assert baseline[0][0]
 time.sleep(4.2)
 assert (pause_record(A),lifecycle(A)['high_water'])==baseline,'unchanged timer retried its own aborted work'
 CHECKS.append('shared_pressure_stale_prior_graph_raw_reader_coherence_healthy_sibling_and_no_retry_loop')
 # Cancel is an explicit revocation of deferred work; capture must not need
 # free query/disk slots or a provider. A later changed save may defer again.
 value('cancel',A);wait(A,lambda s:s['phase']=='cancelled','paused cancellation baseline')
 assert pause_record(A)[0]=='',pause_record(A)
 assert holder_query(held)['generation']==old['generation']
 (A/'value.ts').write_text(text(4));pressure(A)
 stop();value('watch',B);pressure(A)
 restarted=pause_record(A);assert restarted[0]
 (B/'value.ts').write_text('export function walTarget() { return 7; }\n');pressure(B)
 time.sleep(2.2);assert pause_record(A)==restarted,'restart timer altered deferred authorization'
 release(held)
 final_a=ready(A,before+1);final_b=ready(B,sibling['generation']+1)
 assert value('resources')['rebuild_disk_mib']==cap
 assert query(A)['generation']==final_a['generation'] and query(B)['generation']==final_b['generation']
 assert query(A)['symbols'][0]['locations'][0]['content_hash']==sha(A/'value.ts')
 CHECKS.append('cancel_while_paused_restart_preserves_deferral_and_reclamation_auto_resumes_both_workspaces')
 # A different workspace's pinned WAL fills the same pool before corruption
 # rebuilding. Preflight must wait without allocating/consuming an attempt.
 value('resources','--temporary-disk-mib',2048)
 held2,old2,before2,_=fill_wal(A,5)
 stop();bad=damage(B);value('watch',B);pressure(B)
 deferred=lifecycle(B);assert deferred['selected']['id']==bad and deferred['pending'] is None and not deferred['failed'],deferred
 dirs={p.name for p in (Path(DB[B]).parent/'indexes').iterdir() if p.is_dir()}
 time.sleep(2.2);assert lifecycle(B)==deferred and {p.name for p in (Path(DB[B]).parent/'indexes').iterdir() if p.is_dir()}==dirs
 release(held2);rebuilt=ready(B,final_b['generation']+1)
 assert lifecycle(B)['selected']['id']!=bad and not lifecycle(B)['failed']
 CHECKS.append('rebuild_preflight_defers_before_receipt_allocation_and_auto_resumes_after_shared_reclamation')
 # Deterministic active attempt: gate only the owned Node worker launch,
 # observe a real candidate and worker PID, then force sampled reserve pressure.
 value('resources','--temporary-disk-mib',2048)
 stop();bad2=damage(B);(B/'value.ts').write_text(text(9));GATE.write_text('owned worker gate')
 Path(str(GATE)+'.ready').unlink(missing_ok=True)
 value('watch',B);until=time.monotonic()+30;pending=None
 while time.monotonic()<until:
  pending=lifecycle(B).get('pending')
  if pending and Path(str(GATE)+'.ready').exists():break
  time.sleep(.02)
 assert pending and Path(str(GATE)+'.ready').exists(),'candidate/real worker gate was not reached'
 DETAILS['active_candidate']={'pending':pending,'worker_pid':int(Path(str(GATE)+'.ready').read_text())}
 stats=os.statvfs(DATA);impossible=stats.f_blocks*stats.f_frsize//(1024*1024)+1024
 value('resources','--min-free-disk-mib',impossible)
 pressure(B);failed=lifecycle(B)
 assert failed['failed'] and failed['pending'] is None and failed['selected']['id']==bad2,failed
 GATE.unlink();value('resources','--min-free-disk-mib',0)
 high=failed['high_water'];(B/'value.ts').write_text(text(10));time.sleep(2.2)
 assert lifecycle(B)['failed'] and lifecycle(B)['selected']['id']==bad2 and lifecycle(B)['high_water']==high
 stop();value('watch',A);pressure(B);time.sleep(2.2)
 assert lifecycle(B)['failed'] and lifecycle(B)['high_water']==high
 cli('index',B);fresh=ready(B,high+1)
 assert lifecycle(B)['selected']['id']!=bad2 and not lifecycle(B)['failed']
 assert query(B)['symbols'][0]['locations'][0]['content_hash']==sha(B/'value.ts')
 CHECKS.append('allocated_rebuild_pressure_remains_failed_across_policy_save_restart_until_explicit_refresh')
 assert sha(HOST)==BUILD['host_sha256'] and sha(record_path)==BUILD['producer_sha256']
 passed=True
except BaseException:
 failure=traceback.format_exc();(RUN/'failure.txt').write_text(failure);raise
finally:
 try:cleanup()
 except BaseException:
  passed=False;(RUN/'cleanup-failure.txt').write_text(traceback.format_exc())
  if failure is None:raise
 finally:
  summary={'passed':passed,'run':str(RUN),'checks':CHECKS,'details':DETAILS,'build':BUILD,'failure':failure,
   'cleanup':{'socket_exists':(DATA/'c.sock').exists(),'owned_holders':len(CHILDREN),'worker_gate_exists':GATE.exists()},
   'limits':['finite owned TS/native SQLite checks; no full suite/performance/WAL latency SLO/package/live-runtime certification']}
  (RUN/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary,indent=2))
