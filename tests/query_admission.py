"""Real shared query slots across FFI/Node/.NET/CLI; one saved TS workspace.

Run only after a matching build. All scopes, coordinator state and evidence are
owned by this durable run; default account admission and live runtime are unused.
"""
from cli_assertions import check_exit
from pathlib import Path
from fixture_trust import grant_fixture
import atexit,ctypes,hashlib,json,os,selectors,subprocess,time,traceback

ROOT=Path(__file__).resolve().parents[1]
RUN=ROOT/'.artifacts/qa-tests'/format(time.time_ns(),'x')[-9:]
WORKSPACE=RUN/'w';DATA=RUN/'d';SCOPE=RUN/'s';ISOLATED=RUN/'i';MISSING=RUN/'absent.db'
WORKSPACE.mkdir(parents=True)
PROFILE=os.environ.get('KNOW_CODE_BUILD_PROFILE','debug');CONFIG=PROFILE.capitalize()
HOST=ROOT/f'target/{PROFILE}/know-code';ADDON=ROOT/f'.artifacts/build/{PROFILE}/node/index.mjs'
SMOKE=ROOT/f'bindings/dotnet/Smoke/bin/{CONFIG}/net10.0/Smoke.dll'
FFI=ROOT/f'target/{PROFILE}/libknow_code_ffi.dylib'
ENV=dict(os.environ,KNOW_CODE_DATA_DIR=str(DATA),KNOW_CODE_QUERY_SCOPE=str(SCOPE),UV_THREADPOOL_SIZE='1',
         DOTNET_CLI_TELEMETRY_OPTOUT='1',DOTNET_NOLOGO='1',MSBUILDDISABLENODEREUSE='1',
         DOTNET_CLI_USE_MSBUILD_SERVER='0',UseSharedCompilation='false')
# ctypes runs in this process; it receives the same explicit owned default scope.
os.environ['KNOW_CODE_QUERY_SCOPE']=str(SCOPE)
SOURCE=WORKSPACE/'value.ts';OLD='// π😀 owned admission fixture\nexport function admissionTarget() { return "before"; }\n'
SOURCE.write_text(OLD)
CHECKS=[];DETAILS={};LEASES=[];CHILDREN=[];IDENTITY={};passed=False;failure=None;database=None;ffi=None
sha=lambda path:hashlib.sha256(Path(path).read_bytes()).hexdigest()

def log(value):
 with (RUN/'commands.jsonl').open('a') as output:output.write(json.dumps(value)+'\n')
def run(args,*,input=None,env=None,check=True,timeout=60):
 args=list(map(str,args));started=time.monotonic();selected=env or ENV
 try:p=subprocess.run(args,cwd=ROOT,env=selected,input=input,text=True,capture_output=True,timeout=timeout)
 except subprocess.TimeoutExpired as error:
  log({'args':args,'timeout':timeout,'stdout':str(error.stdout),'stderr':str(error.stderr)});raise
 log({'args':args,'input':input,'data_directory':selected['KNOW_CODE_DATA_DIR'],
      'query_scope':selected['KNOW_CODE_QUERY_SCOPE'],'exit':p.returncode,'stdout':p.stdout,'stderr':p.stderr,
      'elapsed_ms':(time.monotonic()-started)*1000})
 check_exit(p,args,check)
 return p

def cli(*args,**kwargs):return run([HOST,*args],**kwargs)
def value(*args):return json.loads(cli(*args).stdout)
def error(response,code):assert response.get('error',{}).get('code')==code,response

def node(mode,*,db=None,scope=None,payload=None,env=None):
 result=json.loads(run(['node',ROOT/'tests/query_admission.mjs',ADDON,scope or SCOPE,db or MISSING,
                        mode,json.dumps(payload) if payload is not None else ''],env=env).stdout)
 assert result['passed'] and result['contract']==8,result
 return result

def dotnet(*args,**kwargs):return json.loads(run(['dotnet',SMOKE,*args],**kwargs).stdout)
def decode(pointer):
 assert pointer,'native returned null JSON'
 try:return json.loads(ctypes.string_at(pointer).decode())
 finally:ffi.kc_string_free(pointer)

def reserve(scope=None):
 handle=ctypes.c_void_p()
 pointer=ffi.kc_query_reserve(None if scope is None else os.fsencode(scope),ctypes.byref(handle))
 if pointer:
  assert not handle.value
  return None,decode(pointer)
 assert handle.value;LEASES.append(handle.value)
 return handle.value,None

def acquire(scope=None):
 handle,problem=reserve(scope);assert not problem,problem;return handle

def release(handle):
 ffi.kc_query_reservation_free(handle);LEASES.remove(handle)

def reserved(handle,request,db=None):
 if not isinstance(request,str):request=json.dumps(request,ensure_ascii=False,separators=(',',':'))
 return decode(ffi.kc_query_json_reserved(os.fsencode(db or MISSING),request.encode(),0,None,handle))

def control(op):
 request={'op':op,'database':database,'data_directory':str(DATA),'coordinator_executable':str(HOST)}
 response=decode(ffi.kc_workspace_json(json.dumps(request).encode()))
 assert 'result' in response,response
 return response['result']

def ready(minimum=1):
 until=time.monotonic()+90;last=None
 while time.monotonic()<until:
  statuses=value('coordinator','status')['workspaces']
  last=next((s for s in statuses if s['workspace_root']==str(WORKSPACE)),None)
  assert last and last['phase']!='failed',last
  if last['phase']=='idle' and last['generation']>=minimum and last['published_epoch']==last['observed_epoch']:return last
  time.sleep(.05)
 raise AssertionError(('index did not become current',last))

def holder():
 stderr=(RUN/'node-holder.stderr').open('wb')
 child=subprocess.Popen(['node',str(ROOT/'tests/query_admission.mjs'),str(ADDON),str(SCOPE),str(MISSING),'hold'],
     cwd=ROOT,env=ENV,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=stderr,text=True)
 CHILDREN.append((child,stderr));selector=selectors.DefaultSelector();selector.register(child.stdout,selectors.EVENT_READ)
 try:
  assert selector.select(10),('Node holder did not report ready',child.poll())
  line=child.stdout.readline();receipt=json.loads(line);assert receipt['ready'] and receipt['contract']==8,receipt
  (RUN/'node-holder-ready.json').write_text(json.dumps(receipt,indent=2)+'\n')
  log({'args':child.args,'pid':child.pid,'ready':receipt})
  return child
 finally:selector.close()

def cleanup():
 for child,stderr in CHILDREN:
  if child.poll() is None:child.kill()
  try:child.wait(timeout=5)
  except subprocess.TimeoutExpired:child.kill();child.wait(timeout=5)
  if child.stdin:child.stdin.close()
  if child.stdout:
   remaining=child.stdout.read();child.stdout.close()
   if remaining:log({'child_pid':child.pid,'remaining_stdout':remaining})
  stderr.close()
 CHILDREN.clear()
 if ffi is not None:
  for handle in LEASES[:]:release(handle)
 if (DATA/'c.sock').exists():
  cli('coordinator','stop',check=False,timeout=20)
  until=time.monotonic()+5
  while (DATA/'c.sock').exists() and time.monotonic()<until:time.sleep(.02)
  assert not (DATA/'c.sock').exists(),'owned coordinator did not remove its socket after shutdown'

atexit.register(cleanup)
try:
 record_path=ROOT/f'.artifacts/build/{PROFILE}/build-record.json'
 record=json.loads(record_path.read_text());assert record['status']=='complete' and record['profile']==PROFILE
 artifacts=[HOST,FFI,ADDON,ADDON.parent/'know_code.node',SMOKE,SMOKE.parent/'KnowCode.dll']
 artifacts.extend(SMOKE.parent.glob('*.dylib'))
 hashes={str(p.relative_to(ROOT)):sha(p) for p in artifacts}
 assert all(record['outputs'][path]==digest for path,digest in hashes.items()),'outputs differ from producer'
 IDENTITY={'source_revision':record['source_revision'],'build_id':record['build_id'],'record_sha256':sha(record_path),
           'artifacts_sha256':hashes,'test_source_sha256':{path:sha(ROOT/path) for path in [
               'tests/query_admission.py','tests/query_admission.mjs','bindings/dotnet/Smoke/Program.cs','tests/fixture_trust.py']}}
 ffi=ctypes.CDLL(str(FFI))
 ffi.kc_query_contract_version.restype=ctypes.c_uint32
 ffi.kc_query_max_request_bytes.restype=ctypes.c_uint32
 ffi.kc_query_reserve.argtypes=[ctypes.c_char_p,ctypes.POINTER(ctypes.c_void_p)];ffi.kc_query_reserve.restype=ctypes.c_void_p
 ffi.kc_query_reservation_free.argtypes=[ctypes.c_void_p];ffi.kc_query_reservation_free.restype=None
 ffi.kc_query_json_reserved.argtypes=[ctypes.c_char_p,ctypes.c_char_p,ctypes.c_uint64,ctypes.c_void_p,ctypes.c_void_p]
 ffi.kc_query_json_reserved.restype=ctypes.c_void_p
 ffi.kc_query_json.argtypes=[ctypes.c_char_p,ctypes.c_char_p];ffi.kc_query_json.restype=ctypes.c_void_p
 ffi.kc_query_json_elapsed.argtypes=[ctypes.c_char_p,ctypes.c_char_p,ctypes.c_uint64];ffi.kc_query_json_elapsed.restype=ctypes.c_void_p
 ffi.kc_query_json_cancellable.argtypes=[ctypes.c_char_p,ctypes.c_char_p,ctypes.c_uint64,ctypes.c_void_p]
 ffi.kc_query_json_cancellable.restype=ctypes.c_void_p
 ffi.kc_query_cancel_new.restype=ctypes.c_void_p
 ffi.kc_query_cancel.argtypes=[ctypes.c_void_p];ffi.kc_query_cancel.restype=None
 ffi.kc_query_cancel_free.argtypes=[ctypes.c_void_p];ffi.kc_query_cancel_free.restype=None
 ffi.kc_workspace_json.argtypes=[ctypes.c_char_p];ffi.kc_workspace_json.restype=ctypes.c_void_p
 ffi.kc_string_free.argtypes=[ctypes.c_void_p];ffi.kc_string_free.restype=None
 assert ffi.kc_query_contract_version()==8 and ffi.kc_query_max_request_bytes()==262144
 policy=value('queries')['result'];assert policy=={'version':1,'max_active_queries':4},policy
 assert value('queries','--scope',SCOPE)['result']==policy
 guard_and_slots=[SCOPE/'policy.lock',*[SCOPE/f'slot-{i:02}.lock' for i in range(4)]]
 held=[acquire() for _ in range(4)]
 inodes={p.name:(p.stat().st_dev,p.stat().st_ino) for p in guard_and_slots}
 _,problem=reserve();error(problem,'busy')
 error(decode(ffi.kc_query_json(os.fsencode(MISSING),b'{"op":"symbols"}')),'busy')
 # Invalid UTF-8 must not even be converted before automatic admission.
 error(decode(ffi.kc_query_json(os.fsencode(MISSING),b'\xff')),'busy')
 error(decode(ffi.kc_query_json_elapsed(os.fsencode(MISSING),b'\xff',0)),'busy')
 cancellation=ffi.kc_query_cancel_new()
 try:
  error(decode(ffi.kc_query_json_cancellable(os.fsencode(MISSING),b'\xff',0,cancellation)),'busy')
  ffi.kc_query_cancel(cancellation)
  error(decode(ffi.kc_query_json_cancellable(os.fsencode(MISSING),b'\xff',0,cancellation)),'cancelled')
 finally:ffi.kc_query_cancel_free(cancellation)
 assert not MISSING.exists(),'saturated query opened/created absent index'
 error(value('queries','--max-active','2'),'busy')
 assert value('queries')['result']==policy
 alternate=dict(ENV,KNOW_CODE_DATA_DIR=str(RUN/'other-data'))
 DETAILS['node_busy']=node('busy',env=alternate)
 error(dotnet('--scoped-query',MISSING,SCOPE,'{"op":"symbols"}',env=alternate),'busy')
 error(dotnet('--admission-typed',MISSING,SCOPE),'busy')
 CHECKS.append('four_FFI_slots_shared_with_default_Node_explicit_Node_DotNet_CLI_across_data_directories_before_serialization')
 # An explicit different host scope remains usable while the shared scope is full.
 error(node('query',scope=ISOLATED)['response'],'not_ready')
 isolated=acquire(ISOLATED);release(isolated)
 CHECKS.append('explicit_host_scope_isolation')
 release(held.pop());child=holder()
 _,problem=reserve();error(problem,'busy')
 child.kill();child.wait(timeout=5)
 held.append(acquire())
 _,problem=reserve();error(problem,'busy')
 CHECKS.append('partial_FFI_release_real_Node_native_image_reuse_and_process_crash_release')
 # Consumer saturation must not block coordinator registration, internal
 # readiness, cancellation or saved-source publication.
 grant_fixture(HOST,WORKSPACE,ENV,RUN,RUN/'fixture-trust.jsonl')
 registration=value('watch',WORKSPACE);database=registration['database'];initial=ready()
 assert control('status')['status']['database']==database
 assert control('refresh')['status']['database']==database
 refreshed=ready(initial['generation']+1)
 cancelled=control('cancel')['status']
 assert cancelled['database']==database and cancelled['generation']==refreshed['generation'],cancelled
 if cancelled['phase']=='idle':
  # Cancel is a legitimate no-op when no job is active. It must not change
  # the coherent baseline or manufacture a cancellation transition.
  assert cancelled['published_epoch']==cancelled['observed_epoch'],cancelled
  checked=control('status')['status']
  assert checked['phase']=='idle' and checked['generation']==refreshed['generation'],checked
  assert checked['published_epoch']==checked['observed_epoch'],checked
  DETAILS['cancel_mode']='idle_noop'
 else:
  until=time.monotonic()+10
  while cancelled['phase']=='cancelling' and time.monotonic()<until:
   time.sleep(.05);cancelled=control('status')['status']
   assert cancelled['generation']==refreshed['generation'],cancelled
  assert cancelled['phase']=='cancelled',cancelled
  # An active cancellation ack precedes its saved-input baseline. Reach that
  # boundary before the new save; never hide a missed save with refresh.
  assert control('status')['status']['phase']=='cancelled'
  DETAILS['cancel_mode']='active_settled'
 DETAILS['cancelled_before_save']=cancelled
 NEW=OLD.replace('"before"','"after"');SOURCE.write_text(NEW)
 current=ready(refreshed['generation']+1)
 assert current['generation']>initial['generation']
 DETAILS['progress_while_full']={'initial':initial,'after_refresh':refreshed,'after_save':current}
 DETAILS['node_workspace']=node('workspace-busy',db=database,payload={'root':str(WORKSPACE),'data':str(DATA),'host':str(HOST)})
 options={'dataDirectory':str(DATA),'coordinatorExecutable':str(HOST),'queryLimitScope':str(SCOPE)}
 lines=run(['dotnet',SMOKE,'--workspace',WORKSPACE,json.dumps(options)],input=
   '{"op":"typed_query","name":"admissionTarget"}\n{"op":"status"}\n').stdout.splitlines()
 replies=[json.loads(line) for line in lines];assert len(replies)==3 and 'opened' in replies[0],replies
 error(replies[1],'busy');assert replies[2]['database']==database,replies
 CHECKS.append('control_readiness_refresh_cancel_saved_publication_and_Workspace_typed_busy_progress_at_capacity')
 for handle in held:release(handle)
 held=[]
 single=acquire();error(reserved(single,{'op':'symbols'}),'not_ready')
 error(reserved(single,{'op':'symbols'}),'invalid_request')
 others=[acquire() for _ in range(3)];_,problem=reserve();error(problem,'busy')
 release(single)
 for handle in others:release(handle)
 CHECKS.append('FFI_single_submission_and_retained_reservation_hold_until_free')
 # Exact byte boundary includes literal non-BMP characters, not escaped ASCII.
 maximum=ffi.kc_query_max_request_bytes();base='{"op":"symbols","name":""}'
 available=maximum-len(base.encode());name='😀'*(available//4)+'x'*(available%4)
 exact=json.dumps({'op':'symbols','name':name},ensure_ascii=False,separators=(',',':'))
 assert len(exact.encode())==maximum
 for text,code in [(exact,'not_ready'),(exact[:-2]+'x'+exact[-2:],'invalid_request')]:
  handle=acquire();error(reserved(handle,text),code);release(handle)
 cancellation=ffi.kc_query_cancel_new()
 try:
  for text,code in [(exact,'not_ready'),(exact[:-2]+'x'+exact[-2:],'invalid_request')]:
   encoded=text.encode();db=os.fsencode(MISSING)
   error(decode(ffi.kc_query_json(db,encoded)),code)
   error(decode(ffi.kc_query_json_elapsed(db,encoded,0)),code)
   error(decode(ffi.kc_query_json_cancellable(db,encoded,0,cancellation)),code)
  for pointer in [None,b'\xff']:
   error(decode(ffi.kc_query_json(os.fsencode(MISSING),pointer)),'invalid_request')
 finally:ffi.kc_query_cancel_free(cancellation)
 DETAILS['node_bytes']=node('bytes')
 CHECKS.append('FFI_and_Node_exact_256KiB_UTF8_nonBMP_boundary_and_oversize_before_open')
 DETAILS['node_release']=node('failure-release')
 DETAILS['node_queue']=node('duplicate-queue')
 assert dotnet('--admission-typed',MISSING,SCOPE)=={'serialization_failed':True}
 queued=dotnet('--admission-queue',MISSING,SCOPE,4);assert queued['passed'],queued
 CHECKS.append('Node_serializer_parse_failure_and_parse_lifetime_raw_duplicate_dispatch_plus_DotNet_typed_serializer_queue_cancel_ownership')
 # Resize through the supported setter only; no stable lock inode is replaced.
 assert value('queries','--max-active','2')['result']['max_active_queries']==2
 two=[acquire() for _ in range(2)];_,problem=reserve();error(problem,'busy')
 error(value('queries','--max-active','4'),'busy')
 for handle in two:release(handle)
 assert value('queries','--max-active','4')['result']==policy
 assert {p.name:(p.stat().st_dev,p.stat().st_ino) for p in guard_and_slots}==inodes
 CHECKS.append('CLI_quiescent_shrink_grow_busy_rejection_and_stable_policy_slot_inodes')
 request={'op':'symbols','name':'admissionTarget'}
 cli_response=value('query',database,json.dumps(request));node_response=node('query',db=database,payload=request)['response']
 net_response=dotnet('--scoped-query',database,SCOPE,json.dumps(request))
 assert cli_response==node_response==net_response,(cli_response,node_response,net_response)
 result=cli_response['result'];assert result['freshness']=='current' and result['generation']==current['generation'],result
 assert len(result['symbols'])==1 and result['symbols'][0]['name']=='admissionTarget',result
 assert result['coverage'] and result['workspace_coverage']['compilations']>0,result
 for location in result['symbols'][0]['locations']:
  assert location['content_hash']==hashlib.sha256(NEW.encode()).hexdigest(),location
  assert NEW.encode()[location['start_byte']:location['end_byte']]==b'admissionTarget',location
 CHECKS.append('CLI_Node_DotNet_current_saved_TS_native_result_source_coverage_parity_after_release')
 assert all(sha(ROOT/path)==digest for path,digest in hashes.items()) and sha(record_path)==IDENTITY['record_sha256']
 passed=True
except BaseException:
 failure=traceback.format_exc();(RUN/'failure.txt').write_text(failure);raise
finally:
 try:cleanup()
 except BaseException:
  passed=False;cleanup_failure=traceback.format_exc();(RUN/'cleanup-failure.txt').write_text(cleanup_failure)
  if failure is None:raise
 finally:
  summary={'passed':passed,'profile':PROFILE,'run':str(RUN),'database':database,'checks':CHECKS,'details':DETAILS,
           'build':IDENTITY,'failure':failure,'cleanup':{'held_leases':len(LEASES),'owned_children':len(CHILDREN),'owned_socket_exists':(DATA/'c.sock').exists()},
           'limits':['one owned TS fixture; no full suite, performance, WAL bound, package or live-runtime certification']}
  (RUN/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary,indent=2))
