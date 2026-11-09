"""Shared CPU/memory ceilings and explicit indexing cancellation through real clients."""
from pathlib import Path
import atexit,json,os,shutil,subprocess,time
ROOT=Path(__file__).resolve().parents[1]
RUN=ROOT/'.artifacts/rb'/format(time.time_ns(),'x')[-10:]
DATA=RUN/'d';RUNTIME=RUN/'runtime';A=RUN/'a';B=RUN/'b'
for p in [A,B,RUNTIME/'providers/typescript',RUNTIME/'proto']:p.mkdir(parents=True,exist_ok=True)
PROFILE=os.environ.get('KNOW_CODE_BUILD_PROFILE','debug');CONFIG=PROFILE.capitalize();HOST=ROOT/f'target/{PROFILE}/know-code'
ENV=dict(os.environ,KNOW_CODE_DATA_DIR=str(DATA),KNOW_CODE_RUNTIME_DIR=str(RUNTIME),DOTNET_CLI_TELEMETRY_OPTOUT='1',DOTNET_NOLOGO='1')
checks=[];measurements={}
(RUNTIME/'Cargo.toml').write_text('# Owned test runtime override\n')
shutil.copy2(ROOT/'proto/analysis.proto',RUNTIME/'proto/analysis.proto')
# The wrapper adds deterministic test load; actual indexing is still compiler-backed.
wrapper="""import fs from 'node:fs';
import path from 'node:path';
import {spawn} from 'node:child_process';
const run=RUN, root=process.cwd(), name=path.basename(root);
fs.appendFileSync(path.join(run,name+'-starts'),'start\\n');
const mode=fs.existsSync(path.join(root,'.mode'))?fs.readFileSync(path.join(root,'.mode'),'utf8'):'';
if(mode==='hold'||mode==='memory') {
 const script=mode==='memory'?\"const b=Buffer.alloc(384*1024*1024,0x5a); setInterval(()=>{if(b[0]!==90)throw Error();},100);\":'setInterval(()=>{},100);';
 const child=spawn(process.execPath,['-e',script],{stdio:'ignore'});
 fs.writeFileSync(path.join(run,name+'-child'),String(child.pid));
 await new Promise(()=>{});
}
if(mode==='cpu') {
 const start=performance.now();
 const reports=await Promise.all(Array.from({length:4},()=>new Promise((resolve,reject)=>{
  const script=\"const begin=process.cpuUsage(); while(process.cpuUsage(begin).user<350000) Math.sqrt(Math.random()); console.log(JSON.stringify(process.cpuUsage(begin)));\";
  const child=spawn(process.execPath,['-e',script],{stdio:['ignore','pipe','inherit']}); let out='';
  child.stdout.on('data',b=>out+=b); child.on('exit',code=>code===0?resolve(JSON.parse(out)):reject(Error(String(code))));
 })));
 fs.writeFileSync(path.join(run,'cpu.json'),JSON.stringify({wall_seconds:(performance.now()-start)/1000,cpu_seconds:reports.reduce((n,r)=>n+(r.user+r.system)/1e6,0)}));
}
await import(ORIGINAL);
""".replace('RUN',json.dumps(str(RUN))).replace('ORIGINAL',json.dumps((ROOT/'providers/typescript/worker.mjs').as_uri()))
(RUNTIME/'providers/typescript/worker.mjs').write_text(wrapper)
for workspace in [A,B]:
 (workspace/'tsconfig.json').write_text('{"compilerOptions":{"strict":true,"noEmit":true},"files":["value.ts"]}')
 (workspace/'value.ts').write_text('export function answer() { return 42; }\n')

def cli(*args,check=True,timeout=60):
 p=subprocess.run([str(HOST),*map(str,args)],cwd=ROOT,env=ENV,text=True,capture_output=True,timeout=timeout)
 assert not check or p.returncode==0,(args,p.stdout,p.stderr)
 return p
def cleanup():
 # Shutdown deliberately drains active work. Cancel our held fixture jobs first
 # so a failed assertion cannot strand its coordinator/descendants indefinitely.
 if (DATA/'c.sock').exists():
  for root in [A,B]:
   try:cli('cancel',root,check=False,timeout=5)
   except subprocess.TimeoutExpired:pass
 try:cli('coordinator','stop',check=False,timeout=5)
 except subprocess.TimeoutExpired:pass
atexit.register(cleanup)
def rows():return json.loads(cli('coordinator','status').stdout)['workspaces']
def status(root):return next(s for s in rows() if s['workspace_root']==str(root))
def wait(root,predicate,description,seconds=45):
 until=time.monotonic()+seconds
 while time.monotonic()<until:
  s=status(root)
  if predicate(s):return s
  time.sleep(.04)
 raise AssertionError((description,s))
def record(name):
 checks.append(name);(RUN/'validation.json').write_text(json.dumps({'profile':PROFILE,'checks':checks,'measurements':measurements,'run':str(RUN)},indent=2)+'\n');print('resources:',name,flush=True)
def save(root,n): (root/'value.ts').write_text(f'export function answer() {{ return {n}; }}\n')
def start_count(root):
 p=RUN/(root.name+'-starts');return len(p.read_text().splitlines()) if p.exists() else 0

def sdk_cancel(language,root):
 options=json.dumps({'dataDirectory':str(DATA),'coordinatorExecutable':str(HOST)})
 if language=='node':
  script=RUN/'cancel.mjs';script.write_text('import {Workspace} from '+json.dumps((ROOT/f'.artifacts/build/{PROFILE}/node/index.mjs').as_uri())+'; const w=await Workspace.open(process.argv[2],JSON.parse(process.argv[3])); console.log(JSON.stringify(await w.cancelIndexing()));w.close();\n')
  argv=['node',str(script),str(root),options];data=None
 else:
  argv=['dotnet',str(ROOT/f'bindings/dotnet/Smoke/bin/{CONFIG}/net10.0/Smoke.dll'),'--workspace',str(root),options];data='{"op":"cancel"}\n'
 p=subprocess.run(argv,input=data,env=ENV,cwd=ROOT,text=True,capture_output=True,timeout=10)
 assert p.returncode==0,(p.stdout,p.stderr)
 value=json.loads(p.stdout.strip().splitlines()[-1]);assert value['phase'] in ['cancelling','cancelled'],value

limits=json.loads(cli('resources').stdout);assert limits=={'cpu_cores':2.0,'memory_mib':2048},limits
registered=json.loads(cli('watch',A).stdout);database=registered['database']
initial=wait(A,lambda s:s['phase']=='idle','initial compiler graph');generation=initial['generation']
def query():return json.loads(cli('query',database,'{"op":"symbols","name":"answer","freshness_wait_ms":0}').stdout)['result']
assert query()['freshness']=='current'
record('automatic_shared_defaults_and_initial_compiler_index')
# These are personal control state, separate from repository analysis settings.
assert cli('resources','--cpu','0',check=False).returncode
assert cli('resources','--memory-mib','0',check=False).returncode
assert json.loads(cli('resources').stdout)==limits
record('invalid_limits_do_not_change_active_policy')

for language,number in [('node',43),('dotnet',44)]:
 old_text=(A/'value.ts').read_text()
 (A/'.mode').write_text('hold');save(A,number)
 wait(A,lambda s:s['phase']=='analyzing','held analysis')
 until=time.monotonic()+5
 while not (RUN/'a-child').exists() and time.monotonic()<until:time.sleep(.02)
 started=time.monotonic();sdk_cancel(language,A)
 stopped=wait(A,lambda s:s['phase']=='cancelled',language+' cancellation',seconds=5)
 assert time.monotonic()-started<3 and stopped['outcome_code']=='cancelled',stopped
 assert stopped['generation']==generation and query()['freshness']=='stale'
 symbol=query()['symbols'][0]
 prior=json.loads(cli('query',database,json.dumps({'op':'source','artifact_id':symbol['locations'][0]['artifact_id'],'freshness_wait_ms':0})).stdout)
 assert prior['result']['source']['text']==old_text
 helper=int((RUN/'a-child').read_text())
 deadline=time.monotonic()+2
 while time.monotonic()<deadline:
  ps=subprocess.run(['/bin/ps','-p',str(helper),'-o','stat='],capture_output=True,text=True)
  if ps.returncode or ps.stdout.strip().startswith('Z'):break
  time.sleep(.02)
 else:raise AssertionError(('helper survived cancellation',helper,ps.stdout))
 count=start_count(A);time.sleep(.35);assert start_count(A)==count,'unchanged cancelled work retried'
 (A/'.mode').unlink();save(A,number+100)
 healthy=wait(A,lambda s:s['phase']=='idle' and s['generation']>generation,'save resumed indexing')
 generation=healthy['generation']
 record(language+'_cancellation_preserves_stale_graph_and_next_save_resumes')
 (RUN/'a-child').unlink(missing_ok=True)

# Queue B behind A; cancel B without launching its worker, then a save resumes it.
(A/'.mode').write_text('hold');save(A,45);wait(A,lambda s:s['phase']=='analyzing','queue blocker')
cli('watch',B);cancelled_queued=json.loads(cli('cancel',B).stdout)
queued_status=status(B)
assert queued_status['phase']=='cancelled' and start_count(B)==0,(cancelled_queued,queued_status,start_count(B))
not_ready=json.loads(cli('query',status(B)['database'],'{"op":"symbols","name":"answer","freshness_wait_ms":0}').stdout)
assert not_ready['error']['code']=='not_ready',not_ready
save(B,99);cli('cancel',A)
wait(B,lambda s:s['phase']=='idle','saved queued workspace resumes')
wait(A,lambda s:s['phase']=='cancelled','CLI cancellation')
(A/'.mode').unlink();record('queued_cancellation_and_shared_scheduler_progress')
# Restart must not turn cancelled, unchanged data into current results.
cli('coordinator','stop');time.sleep(.15)
cli('resources')
wait(A,lambda s:s['phase']=='cancelled','cancelled restart reconciliation')
assert query()['freshness']=='stale'
record('cancelled_state_survives_restart_without_compiler_retry')
cli('index',A);generation=status(A)['generation'];assert query()['freshness']=='current'
record('explicit_refresh_resumes_cancelled_analysis')

# Memory belongs to a helper, proving that worker-parent-only accounting is insufficient.
cli('resources','--memory-mib',256)
(A/'.mode').write_text('memory');save(A,46)
limited=wait(A,lambda s:s['phase']=='resource_limited','descendant allocation exceeds shared budget')
assert limited['outcome_code']=='resource_limited' and 'shared memory limit exceeded' in limited['error'],limited
assert limited['generation']==generation and query()['freshness']=='stale'
count=start_count(A);time.sleep(.4);assert start_count(A)==count
(A/'.mode').unlink()
cli('resources','--memory-mib',1024)
ready=wait(A,lambda s:s['phase']=='idle' and s['generation']>generation,'raising personal cap resumes analysis')
generation=ready['generation'];record('helper_memory_limit_aborts_publication_and_cap_change_resumes')

# Four helper processes burn measured CPU, sharing one 0.5-core allocation.
cli('resources','--cpu',.5)
(A/'.mode').write_text('cpu');save(A,47)
wait(A,lambda s:s['phase']=='idle' and s['generation']>generation,'bounded aggregate CPU',seconds=60)
cpu=json.loads((RUN/'cpu.json').read_text());measurements['cpu']=cpu
assert cpu['cpu_seconds']>1.35 and cpu['cpu_seconds']/cpu['wall_seconds']<.7,cpu
(A/'.mode').unlink();record('aggregate_cpu_budget_covers_multiple_helpers')
cli('coordinator','stop');time.sleep(.15)
assert json.loads(cli('resources').stdout)=={'cpu_cores':.5,'memory_mib':1024}
record('personal_limits_survive_coordinator_restart')
cli('resources','--cpu',2,'--memory-mib',2048)
print(json.dumps({'passed':len(checks),'run':str(RUN),'measurements':measurements}),flush=True)
