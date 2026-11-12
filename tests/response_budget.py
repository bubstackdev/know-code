"""Response caps and compact query/page status agree across CLI and native bindings."""
from cli_assertions import check_exit
from fixture_trust import grant_fixture
from pathlib import Path
import atexit,ctypes,json,os,subprocess,time
ROOT=Path(__file__).resolve().parents[1]
PROFILE=os.environ.get('KNOW_CODE_BUILD_PROFILE','debug');CONFIG=PROFILE.capitalize()
RUN=ROOT/'.artifacts/qb'/format(time.time_ns(),'x')[-10:];WORKSPACE=RUN/'w';WORKSPACE.mkdir(parents=True)
DATA=RUN/'d';HOST=ROOT/f'target/{PROFILE}/know-code'
ENV=dict(os.environ,KNOW_CODE_DATA_DIR=str(DATA),DOTNET_CLI_TELEMETRY_OPTOUT='1')
ENV['KNOW_CODE_QUERY_SCOPE']=str(RUN/'query-admission')
(WORKSPACE/'values.ts').write_text(''.join(f'export function budgetFunction{i:04d}{"Name"*20}() {{ return {i}; }}\n' for i in range(1000)))
def command(argv):
 p=subprocess.run(list(map(str,argv)),cwd=ROOT,env=ENV,text=True,capture_output=True,timeout=120)
 check_exit(p,argv)
 return p.stdout
atexit.register(lambda:subprocess.run([str(HOST),'coordinator','stop'],env=ENV,capture_output=True,timeout=20))
grant_fixture(HOST,WORKSPACE,ENV,RUN,RUN/'fixture-trust.jsonl')
indexed=json.loads(command([HOST,'index',WORKSPACE]));DATABASE=indexed['database']
ffi=ctypes.CDLL(str(ROOT/f'target/{PROFILE}/libknow_code_ffi.dylib'));ffi.kc_query_contract_version.restype=ctypes.c_uint32
assert ffi.kc_query_contract_version()==9
ffi.kc_query_reserve.argtypes=[ctypes.c_char_p,ctypes.POINTER(ctypes.c_void_p)];ffi.kc_query_reserve.restype=ctypes.c_void_p
ffi.kc_query_json_reserved.argtypes=[ctypes.c_char_p,ctypes.c_char_p,ctypes.c_uint64,ctypes.c_void_p,ctypes.c_void_p];ffi.kc_query_json_reserved.restype=ctypes.c_void_p
ffi.kc_query_reservation_free.argtypes=[ctypes.c_void_p];ffi.kc_string_free.argtypes=[ctypes.c_void_p]
prefixes={'cli':[HOST,'query',DATABASE],
          'node':['node',ROOT/f'.artifacts/build/{PROFILE}/node/smoke.mjs',DATABASE],
          'dotnet':['dotnet',ROOT/f'bindings/dotnet/Smoke/bin/{CONFIG}/net10.0/Smoke.dll',DATABASE], 'ffi':None}
def invoke(name,request):
 if name!='ffi':
  text=command([*prefixes[name],json.dumps(request)]);return text,json.loads(text)
 reservation=ctypes.c_void_p();raw=ffi.kc_query_reserve(ENV['KNOW_CODE_QUERY_SCOPE'].encode(),ctypes.byref(reservation))
 try:
  if not raw:raw=ffi.kc_query_json_reserved(DATABASE.encode(),json.dumps(request).encode(),0,None,reservation)
  assert raw,'null C-ABI query response'
  text=ctypes.string_at(raw).decode();return text,json.loads(text)
 finally:
  if raw:ffi.kc_string_free(raw)
  if reservation.value:ffi.kc_query_reservation_free(reservation)
def compact(result):
 coordination=result['coordination']
 assert set(coordination)=={'workspace_root','database','phase','observed_epoch','published_epoch','generation','available','has_error','outcome_code','status_limited'},coordination
 assert coordination['workspace_root']==str(WORKSPACE) and coordination['database']==DATABASE,coordination
 assert isinstance(coordination['has_error'],bool) and isinstance(coordination['status_limited'],bool)
 for key in ['phase','outcome_code']:
  token=coordination[key];assert len(token.encode())<=32 and all(c in 'abcdefghijklmnopqrstuvwxyz0123456789_' for c in token),coordination
 assert result['coverage'] and result['workspace_coverage']['compilations'],result
checks=[];responses={}
for name in prefixes:
 for label,request,code in [
  ('default',{'op':'symbols','limit':1000},'response_limit_exceeded'),
  ('small',{'op':'symbols','limit':1000,'response_max_bytes':1024},'response_limit_exceeded'),
  ('invalid_small',{'op':'symbols','response_max_bytes':1023},'invalid_request'),
  ('invalid_large',{'op':'symbols','response_max_bytes':2097153},'invalid_request'),
  ('rejected_envelope',{'op':'symbols','response_max_bytes':1024,'x'*5000:True},'response_limit_exceeded'),
  ('larger',{'op':'symbols','limit':1000,'response_max_bytes':2097152},None)]:
  text,value=invoke(name,request);(RUN/(name+'-'+label+'.json')).write_text(text)
  if code!='invalid_request':assert len(text.rstrip('\n').encode()) <= request.get('response_max_bytes',262144)
  responses[name,label]=value
  if code:assert value['error']['code']==code,(name,label,value)
  else:
   result=value['result'];assert len(result['symbols'])==1000 and result['freshness']=='current'
   compact(result)
 for label in ['default','small','invalid_small','invalid_large','rejected_envelope','larger']:
  if name!='cli':assert responses[name,label]==responses['cli',label],(name,label)
 checks.append(name+'_bounded_errors_and_explicit_larger_response')
first_text,first=invoke('ffi',{'op':'symbols','limit':3,'page_size':1,'response_max_bytes':2097152})
(RUN/'ffi-first-page.json').write_text(first_text);compact(first['result'])
for name in prefixes:
 text,replay=invoke(name,{'op':'next_page','cursor':first['result']['page']['cursor'],'response_max_bytes':2097152})
 (RUN/(name+'-compact-cursor-replay.json')).write_text(text)
 compact(replay['result']);assert replay==first,(name,replay,first)
checks+=['CLI_Node_DotNet_C_ABI_complete_response_parity','ordinary_and_cursor_queries_omit_full_configuration_progress_and_error_payloads','C_ABI_advertises_bounded_query_contract']
summary={'passed':True,'profile':PROFILE,'run':str(RUN),'checks':checks}
(RUN/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary))
