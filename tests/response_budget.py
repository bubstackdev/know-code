"""Complete response caps agree across CLI and both real in-process SDKs."""
from pathlib import Path
import atexit,ctypes,json,os,subprocess,time
ROOT=Path(__file__).resolve().parents[1]
PROFILE=os.environ.get('KNOW_CODE_BUILD_PROFILE','debug');CONFIG=PROFILE.capitalize()
RUN=ROOT/'.artifacts/qb'/format(time.time_ns(),'x')[-10:];WORKSPACE=RUN/'w';WORKSPACE.mkdir(parents=True)
DATA=RUN/'d';HOST=ROOT/f'target/{PROFILE}/know-code'
ENV=dict(os.environ,KNOW_CODE_DATA_DIR=str(DATA),DOTNET_CLI_TELEMETRY_OPTOUT='1')
(WORKSPACE/'values.ts').write_text(''.join(f'export function budgetFunction{i:04d}{"Name"*20}() {{ return {i}; }}\n' for i in range(1000)))
def command(argv):
 p=subprocess.run(list(map(str,argv)),cwd=ROOT,env=ENV,text=True,capture_output=True,timeout=120)
 assert p.returncode==0,(argv,p.stdout,p.stderr)
 return p.stdout
atexit.register(lambda:subprocess.run([str(HOST),'coordinator','stop'],env=ENV,capture_output=True,timeout=20))
indexed=json.loads(command([HOST,'index',WORKSPACE]));DATABASE=indexed['database']
ffi=ctypes.CDLL(str(ROOT/f'target/{PROFILE}/libknow_code_ffi.dylib'));ffi.kc_query_contract_version.restype=ctypes.c_uint32
assert ffi.kc_query_contract_version()==6
prefixes={'cli':[HOST,'query',DATABASE],
          'node':['node',ROOT/f'.artifacts/build/{PROFILE}/node/smoke.mjs',DATABASE],
          'dotnet':['dotnet',ROOT/f'bindings/dotnet/Smoke/bin/{CONFIG}/net10.0/Smoke.dll',DATABASE]}
checks=[];responses={}
for name,prefix in prefixes.items():
 for label,request,code in [
  ('default',{'op':'symbols','limit':1000},'response_limit_exceeded'),
  ('small',{'op':'symbols','limit':1000,'response_max_bytes':1024},'response_limit_exceeded'),
  ('invalid_small',{'op':'symbols','response_max_bytes':1023},'invalid_request'),
  ('invalid_large',{'op':'symbols','response_max_bytes':2097153},'invalid_request'),
  ('rejected_envelope',{'op':'symbols','response_max_bytes':1024,'x'*5000:True},'response_limit_exceeded'),
  ('larger',{'op':'symbols','limit':1000,'response_max_bytes':2097152},None)]:
  text=command([*prefix,json.dumps(request)]);(RUN/(name+'-'+label+'.json')).write_text(text)
  if code!='invalid_request':assert len(text.rstrip('\n').encode()) <= request.get('response_max_bytes',262144)
  value=json.loads(text);responses[name,label]=value
  if code:assert value['error']['code']==code,(name,label,value)
  else:
   result=value['result'];assert len(result['symbols'])==1000 and result['freshness']=='current'
   assert result['coverage'] and result['coordination'] and result['workspace_coverage']['compilations']
 for label in ['default','small','invalid_small','invalid_large','rejected_envelope','larger']:
  if name!='cli':assert responses[name,label]==responses['cli',label],(name,label)
 checks.append(name+'_bounded_errors_and_explicit_larger_response')
checks+=['CLI_Node_DotNet_complete_response_parity','C_ABI_advertises_bounded_query_contract']
summary={'passed':True,'profile':PROFILE,'run':str(RUN),'checks':checks}
(RUN/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary))
