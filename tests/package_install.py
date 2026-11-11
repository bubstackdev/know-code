"""Install archives, restore dependencies, then test with checkout/network access denied."""
from pathlib import Path
import argparse,atexit,hashlib,json,os,selectors,shutil,socket,subprocess,tarfile,time,zipfile
ROOT=Path(__file__).resolve().parents[1]
parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('packages',type=Path);parser.add_argument('--experimental',action='store_true');parser.add_argument('--prefix',type=Path,required=True);parser.add_argument('--deny-checkout',type=Path,action='append',default=[],help='additional build-source checkout to deny when archives came from another tree');args=parser.parse_args()
PACKAGES=args.packages.resolve();PREFIX=args.prefix.resolve();assert PREFIX!=ROOT and ROOT not in PREFIX.parents,'package test prefix must be outside checkout';PREFIX.mkdir(parents=True,exist_ok=False)
CHECKOUTS=sorted({ROOT,*[p.resolve() for p in args.deny_checkout]})
assert all(root!=PREFIX and root not in PREFIX.parents and (root/'LICENSE').is_file() for root in CHECKOUTS),'each denied checkout must exist and exclude the test prefix'
DATA=PREFIX/'d';BIN=PREFIX/'bin';BIN.mkdir();FEED=PREFIX/'feed';FEED.mkdir();WORKSPACE=PREFIX/'w';WORKSPACE.mkdir();SCRATCH=PREFIX/'t';SCRATCH.mkdir();CLIENTS=[];checks=[];PHASE='dependency_setup';PHASES=[]
ENV=dict(os.environ,PATH=str(BIN)+':'+os.environ['PATH'],KNOW_CODE_DATA_DIR=str(DATA),NUGET_PACKAGES=str(PREFIX/'nuget-cache'),DOTNET_CLI_TELEMETRY_OPTOUT='1',DOTNET_NOLOGO='1',MSBUILDDISABLENODEREUSE='1',DOTNET_CLI_USE_MSBUILD_SERVER='0',TMPDIR=str(SCRATCH),npm_config_cache=str(PREFIX/'npm-cache'),npm_config_fetch_retries='0',npm_config_fetch_timeout='3000')
for key in ['KNOW_CODE_RUNTIME_DIR','KNOW_CODE_COORDINATOR','NODE_PATH','NODE_OPTIONS']:ENV.pop(key,None)
POLICY='(version 1) (allow default) '+''.join('(deny file-read* file-write* (subpath '+json.dumps(str(root))+'))' for root in CHECKOUTS)
(PREFIX/'deny-checkout.sb').write_text(POLICY+'\n')
POLICY_FILE=PREFIX/'deny-checkout.sb'
def command(*args,cwd=PREFIX,check=True,env=ENV,sandbox=True):
 cmd=(['/usr/bin/sandbox-exec','-f',str(POLICY_FILE)] if sandbox else [])+list(map(str,args))
 p=subprocess.run(cmd,cwd=cwd,env=env,text=True,capture_output=True,timeout=120)
 with (PREFIX/'commands.jsonl').open('a') as log:log.write(json.dumps({'phase':PHASE,'argv':cmd,'cwd':str(cwd),'returncode':p.returncode,'stdout':p.stdout,'stderr':p.stderr})+'\n')
 assert not check or p.returncode==0,(cmd,p.returncode,p.stdout,p.stderr)
 return p

def prove_network_policy():
 """Positive and negative socket controls; no public host is contacted."""
 probe=PREFIX/'network-probe.py'
 probe.write_text('''import errno,json,socket,sys,uuid
from pathlib import Path
endpoints=json.loads(sys.argv[1]);denied=sys.argv[2]=='denied';results=[]
for endpoint in endpoints:
    handle=None
    try:
        handle=socket.socket(getattr(socket,endpoint['family']),getattr(socket,endpoint['kind']));handle.settimeout(2)
        address=(endpoint['host'],endpoint['port'])
        if endpoint['kind']=='SOCK_STREAM':handle.connect(address);handle.sendall(b'controlled-local-probe')
        else:handle.sendto(b'controlled-local-probe',address)
        assert not denied,endpoint
        results.append(dict(endpoint,outcome='connected'))
    except OSError as error:
        assert denied and error.errno in (errno.EPERM,errno.EACCES),(endpoint,error)
        results.append(dict(endpoint,outcome='denied',errno=error.errno))
    finally:
        if handle is not None:handle.close()
for path in [Path(sys.argv[3])/'unix-probe.sock',Path('/private/tmp')/str(uuid.uuid4())]:
    try:
        with socket.socket(socket.AF_UNIX,socket.SOCK_STREAM) as server:
            server.bind(str(path));server.listen(1)
            with socket.socket(socket.AF_UNIX,socket.SOCK_STREAM) as client:
                client.settimeout(2);client.connect(str(path));client.sendall(b'unix-control')
                accepted,_=server.accept()
                with accepted:assert accepted.recv(64)==b'unix-control'
    finally:path.unlink(missing_ok=True)
other=Path('/private/tmp')/('know-code-probe-'+str(uuid.uuid4()))
try:
    with socket.socket(socket.AF_UNIX,socket.SOCK_STREAM) as server:
        try:server.bind(str(other));assert not denied
        except OSError as error:assert denied and error.errno in (errno.EPERM,errno.EACCES)
finally:other.unlink(missing_ok=True)
print(json.dumps({'network':results,'unix_bind_connect_accept':True,'roslyn_guid_unix_bind_connect_accept':True,'other_temporary_unix_denied':denied}))
''')
 listeners=[];endpoints=[]
 try:
  for family,host in [(socket.AF_INET,'127.0.0.1'),(socket.AF_INET6,'::1')]:
   for kind in [socket.SOCK_STREAM,socket.SOCK_DGRAM]:
    listener=socket.socket(family,kind);listener.bind((host,0));listener.settimeout(.2)
    if kind==socket.SOCK_STREAM:listener.listen(2)
    listeners.append(listener);endpoints.append({'family':family.name,'kind':kind.name,'host':host,'port':listener.getsockname()[1]})
  baseline=json.loads(command('python3',probe,json.dumps(endpoints),'allowed',PREFIX).stdout)
  for listener in listeners:
   if listener.type==socket.SOCK_STREAM:
    accepted,_=listener.accept()
    with accepted:assert accepted.recv(64)==b'controlled-local-probe'
   else:assert listener.recv(64)==b'controlled-local-probe'
  global POLICY_FILE
  POLICY_FILE=PREFIX/'deny-network-and-checkout.sb'
  # Roslyn 5.9 hardcodes /tmp/<GUID> for BuildHost pipes rather than TMPDIR.
  # This permits its transient local IPC, never Internet sockets or artifacts.
  # Seatbelt's matcher accepts explicit repetition, not {n} repetition.
  guid='-'.join('[0-9a-f]'*n for n in [8,4,4,4,12])
  roslyn_pipe='(regex #"^(/private)?/tmp/'+guid+'$")'
  POLICY_FILE.write_text(POLICY+' (deny network*) (allow network* (local unix-socket (subpath '+json.dumps(str(PREFIX))+'))) (allow network* (remote unix-socket (subpath '+json.dumps(str(PREFIX))+'))) (allow network* (local unix-socket '+roslyn_pipe+')) (allow network* (remote unix-socket '+roslyn_pipe+'))\n')
  denied=json.loads(command('python3',probe,json.dumps(endpoints),'denied',PREFIX).stdout)
  for listener in listeners:
   try:
    if listener.type==socket.SOCK_STREAM:unexpected,_=listener.accept();unexpected.close()
    else:listener.recv(64)
   except TimeoutError:pass
   else:raise AssertionError('denied network probe reached the listener')
  evidence={'baseline':baseline,'denied':denied,'policy_sha256':hashlib.sha256(POLICY_FILE.read_bytes()).hexdigest()}
  (PREFIX/'network-policy-proof.json').write_text(json.dumps(evidence,indent=2)+'\n')
  checks.append('network_denial_IPv4_IPv6_TCP_UDP_with_Unix_control')
 finally:
  for listener in listeners:listener.close()

# Review integrity and archive inventory before installation.
manifest=json.loads((PACKAGES/'manifest.json').read_text());assert manifest['candidate']
shutil.copy2(PACKAGES/'manifest.json',PREFIX/'candidate-manifest.json')
for artifact in manifest['artifacts']:
 source=PACKAGES/artifact['file'];assert hashlib.sha256(source.read_bytes()).hexdigest()==artifact['sha256']
 shutil.copy2(source,FEED/source.name)
archive=next(FEED.glob('know-code-runtime-*.tar.gz'))
with tarfile.open(archive) as tar:
 for item in tar.getmembers():
  assert not Path(item.name).is_absolute() and '..' not in Path(item.name).parts and not item.issym(),item.name
 tar.extractall(PREFIX/'first installation',filter='data')
with tarfile.open(next(FEED.glob('know-code-node-*.tgz'))) as tar:
 names=[m.name for m in tar.getmembers()];assert 'package/know_code.node' in names and not any('/smoke.' in n for n in names)
with zipfile.ZipFile(next(FEED.glob('*.nupkg'))) as z:
 names=z.namelist();assert 'runtimes/osx-arm64/native/libknow_code_ffi.dylib' in names and 'lib/net10.0/KnowCode.dll' in names
 assert not any(n.endswith('.pdb') for n in names)
checks.append('archive_hashes_and_native_package_contents')
bundle=next((PREFIX/'first installation').glob('know-code-*'));HOST=BIN/'know-code';HOST.symlink_to(bundle/'bin/know-code')
for root in CHECKOUTS:assert command('/bin/cat',root/'LICENSE',check=False).returncode!=0,('checkout deny policy ineffective',root)
info=json.loads(command(HOST,'runtime-info').stdout);assert info['layout']=='bundle' and info['sqlite']['fts5'] and str(PREFIX) in info['resource_directory']
runtime_manifest=json.loads((bundle/'lib/know-code/runtime.json').read_text())
contracts={'worker_protocol':7,'store_format':8,'index_lifecycle_version':2,'analysis_settings_version':1,'resource_control_version':3}
assert all(info[key]==runtime_manifest[key]==version for key,version in contracts.items()),(info,runtime_manifest)
assert command(HOST,'--version').stdout.startswith('know-code 0.0.1')
checks+=['checkout_access_denied','runtime_discovery_through_symlink_and_spaces','bundled_SQLite_FTS5']
for native in [bundle/'bin/know-code',PREFIX/'unused']:
 if not native.exists():continue
 assert command('lipo','-archs',native).stdout.strip()=='arm64'

# Install through the actual package managers, offline, with no checkout project reference.
NODE=PREFIX/'node-consumer';NODE.mkdir();(NODE/'package.json').write_text('{"name":"package-test","private":true,"type":"module"}\n')
command('npm','install','--offline','--ignore-scripts','--no-audit','--no-fund',next(FEED.glob('know-code-node-*.tgz')),cwd=NODE)
node_driver=(ROOT/'tests/workspace_client.mjs').read_text().replace("await import(pathToFileURL(process.argv[2]))","await import('@know-code/node')")
(NODE/'client.mjs').write_text(node_driver)
DOTNET=PREFIX/'net-consumer';DOTNET.mkdir();(DOTNET/'Consumer.csproj').write_text('<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><OutputType>Exe</OutputType><TargetFramework>net10.0</TargetFramework><UseAppHost>false</UseAppHost><ImplicitUsings>enable</ImplicitUsings></PropertyGroup><ItemGroup><PackageReference Include="KnowCode" Version="0.0.1" /></ItemGroup></Project>')
(DOTNET/'Program.cs').write_text((ROOT/'bindings/dotnet/Smoke/Program.cs').read_text())
(PREFIX/'NuGet.Config').write_text('<configuration><packageSources><clear/><add key="candidate" value="'+str(FEED)+'" /></packageSources></configuration>')
command('dotnet','build',DOTNET/'Consumer.csproj','-c','Release','--nologo','-p:UseSharedCompilation=false','-nodeReuse:false',cwd=DOTNET)
checks+=['npm_archive_installed_offline','NuGet_archive_installed_offline_with_native_assets']
(WORKSPACE/'tsconfig.json').write_text('{"compilerOptions":{"target":"ES2022","module":"NodeNext","moduleResolution":"NodeNext","strict":true,"noEmit":true},"files":["value.ts"]}')
(WORKSPACE/'value.ts').write_text('// café 😀 offline source\nimport { dependencyValue } from "@know-code/offline-dependency";\nexport function leaf() { return dependencyValue(); }\nexport function answer() { return leaf(); }\n')
(WORKSPACE/'App.csproj').write_text('<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><TargetFramework>net10.0</TargetFramework><LangVersion>latest</LangVersion></PropertyGroup><ItemGroup><PackageReference Include="KnowCode" Version="0.0.1" /></ItemGroup></Project>')
CSHARP='// café 😀 offline source\nusing System.Text.Json.Serialization; public static class Greeter { public static int Greet() => 42; public static KnowCode.QueryRequest Request() => new("symbols"); } public record Person(string Name); [JsonSerializable(typeof(Person))] public partial class MyContext : JsonSerializerContext { }\n'
(WORKSPACE/'Code.cs').write_text(CSHARP)

# Dependency setup is explicit and cannot create a serving index. The local
# tarballs/feed keep this fixture reproducible without requiring a registry.
resources=bundle/'lib/know-code'
typescript_pack=json.loads(command('npm','pack',resources/'node_modules/typescript','--offline','--ignore-scripts','--json','--pack-destination',FEED).stdout)[0]['filename']
dependency=PREFIX/'dependency';dependency.mkdir()
(dependency/'package.json').write_text('{"name":"@know-code/offline-dependency","version":"1.0.0","private":true,"type":"module","types":"index.d.ts","main":"index.js"}\n')
(dependency/'index.d.ts').write_text('export declare function dependencyValue(): number;\n')
(dependency/'index.js').write_text('export function dependencyValue() { return 42; }\n')
dependency_pack=json.loads(command('npm','pack',dependency,'--offline','--ignore-scripts','--json','--pack-destination',FEED).stdout)[0]['filename']
npm_version=command('npm','--version').stdout.strip()
(WORKSPACE/'package.json').write_text(json.dumps({'name':'offline-workspace','private':True,'type':'module','packageManager':'npm@'+npm_version,'devDependencies':{'typescript':'file:../feed/'+typescript_pack},'dependencies':{'@know-code/offline-dependency':'file:../feed/'+dependency_pack}})+'\n')
command('npm','install','--offline','--ignore-scripts','--no-audit','--no-fund',cwd=WORKSPACE)
command('dotnet','restore',WORKSPACE/'App.csproj','--configfile',PREFIX/'NuGet.Config','--nologo',cwd=WORKSPACE)
assert (WORKSPACE/'node_modules/typescript/lib/typescript.js').is_file()
assert (WORKSPACE/'node_modules/@know-code/offline-dependency/index.d.ts').is_file()
assets=json.loads((WORKSPACE/'obj/project.assets.json').read_text());assert 'KnowCode/0.0.1' in assets['libraries']
assert not DATA.exists(),'dependency setup created coordinator state before the denial boundary'
PHASES.append({'phase':PHASE,'finished_ns':time.time_ns(),'typescript_version':json.loads((WORKSPACE/'node_modules/typescript/package.json').read_text())['version'],'npm_version':npm_version,'nuget_libraries':sorted(assets['libraries']),'serving_indexes':[],'coordinator_registry_exists':False})
prove_network_policy()
PHASE='network_denied_runtime';PHASES.append({'phase':PHASE,'started_ns':time.time_ns(),'policy':str(POLICY_FILE),'index_state_before_start':[]})
(PREFIX/'phases.json').write_text(json.dumps(PHASES,indent=2)+'\n')
assert not DATA.exists(),'an index/coordinator appeared before first offline registration'
for root in CHECKOUTS:assert command('/bin/cat',root/'LICENSE',check=False).returncode!=0
checks+=['project_dependencies_restored_before_registration','fresh_initial_index_starts_under_network_denial']

def read_json_line(process,selector,buffer,context):
 until=time.monotonic()+30
 while b'\n' not in buffer:
  assert time.monotonic()<until,('client timeout',context)
  if not selector.select(.1):continue
  block=os.read(process.stdout.fileno(),65536);assert block,('client EOF',context,process.poll());buffer+=block
 line,buffer=buffer.split(b'\n',1);return json.loads(line),buffer

class Client:
 def __init__(self,kind):
  opts=json.dumps({'dataDirectory':str(DATA),'configuration':'Release'})
  argv=['node',str(NODE/'client.mjs'),'unused',str(WORKSPACE),opts] if kind=='node' else ['dotnet',str(DOTNET/'bin/Release/net10.0/Consumer.dll'),'--workspace',str(WORKSPACE),opts]
  self.kind=kind;self.events=[];self.buffer=b''
  self.p=subprocess.Popen(['/usr/bin/sandbox-exec','-f',str(POLICY_FILE),*argv],cwd=PREFIX,env=ENV,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=(PREFIX/(kind+'-stderr.log')).open('ab'))
  self.selector=selectors.DefaultSelector();self.selector.register(self.p.stdout,selectors.EVENT_READ);CLIENTS.append(self);self.opened=self.read();assert 'opened' in self.opened,self.opened;assert Path(self.opened['opened']['database']).is_relative_to(DATA),self.opened
 def read(self):
  v,self.buffer=read_json_line(self.p,self.selector,self.buffer,(self.kind,self.events));self.events.append(v);return v
 def send(self,op,**kwargs):self.p.stdin.write((json.dumps(dict(op=op,**kwargs))+'\n').encode());self.p.stdin.flush();return self.read()
 def close(self):
  if self.p.poll() is None:self.p.stdin.close();self.p.wait(timeout=10)
  self.selector.close();(PREFIX/(f'{self.kind}-{self.p.pid}.json')).write_text(json.dumps(self.events,indent=2)+'\n')

def cleanup():
 for client in CLIENTS:client.close()
 if HOST.exists():command(HOST,'coordinator','stop',check=False)
atexit.register(cleanup)
def ready(client,after=1):
 until=time.monotonic()+90
 while time.monotonic()<until:
  s=client.send('wait',wait_ms=1000);assert not isinstance(s.get('error'),dict),s;assert s['phase']!='failed',s
  if s['phase']=='idle' and s['observed_epoch']==s['published_epoch'] and s['generation']>=after:return s
 raise AssertionError(s)
node=Client('node');initial=ready(node);net=Client('net');assert net.opened['opened']['database']==initial['database'];DATABASE=initial['database'];MIN_GENERATION=initial['generation']
def checked_result(envelope):
 assert isinstance(envelope,dict) and 'error' not in envelope and isinstance(envelope.get('result'),dict),envelope
 result=envelope['result'];assert result['freshness']=='current' and result['generation']>=MIN_GENERATION,result
 return result
def query(req):
 n=node.send('query',request=req);d=net.send('query',request=req);c=json.loads(command(HOST,'query',DATABASE,json.dumps(req)).stdout);assert n==d==c,(req,n,d,c);return checked_result(n)
def exact_source(source,filename):
 raw=filename.read_bytes();path=Path(source['path']);path=path if path.is_absolute() else WORKSPACE/path;assert path.resolve()==filename.resolve(),source
 assert source['content_hash']==hashlib.sha256(raw).hexdigest() and source['total_bytes']==len(raw),source
 assert 0<=source['start_byte']<=source['end_byte']<=source['total_bytes'] and source['text'],source
 assert raw[source['start_byte']:source['end_byte']].decode('utf-8')==source['text'],source
for name in ['answer','Greet']:
 result=query({'op':'symbols','name':name});assert result['freshness']=='current' and result['symbols']
 assert result['coverage'] and all(c['status']=='complete' for c in result['coverage']),result
 symbol=result['symbols'][0];context=query({'op':'symbol_context','symbol_id':symbol['id'],'max_bytes':1024});assert context['contexts'][0]['declarations'][0]['source']
 exact_source(context['contexts'][0]['declarations'][0]['source'],WORKSPACE/('value.ts' if name=='answer' else 'Code.cs'))
leaf=query({'op':'symbols','name':'leaf'})['symbols'][0]
references=query({'op':'references','symbol_id':leaf['id']});assert references['freshness']=='current' and references['occurrences']
lexical=query({'op':'symbols','query':'Greeter'});assert lexical['symbols']
all_symbols=query({'op':'symbols','limit':1000});assert any(l['generated'] for s in all_symbols['symbols'] for l in s['locations'])
checks+=['packaged_TS_and_Csharp_compiler_analysis','both_inprocess_SDKs_match_CLI','compiler_context_and_FTS','Roslyn_generated_sources']
# MCP startup/query/EOF over the installed executable, using the same saved publication.
mcp=subprocess.Popen(['/usr/bin/sandbox-exec','-f',str(POLICY_FILE),str(HOST),'mcp','--workspace',str(WORKSPACE)],cwd=PREFIX,env=ENV,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=(PREFIX/'mcp-stderr.log').open('w'))
mcp_selector=selectors.DefaultSelector();mcp_selector.register(mcp.stdout,selectors.EVENT_READ);mcp_buffer=b'';mcp_events=[]
def rpc(identifier,method,params):
 global mcp_buffer
 mcp.stdin.write((json.dumps({'jsonrpc':'2.0','id':identifier,'method':method,'params':params})+'\n').encode());mcp.stdin.flush()
 while True:
  value,mcp_buffer=read_json_line(mcp,mcp_selector,mcp_buffer,('mcp',identifier))
  if value.get('id')==identifier:mcp_events.append({'id':identifier,'method':method,'params':params,'response':value});return value
def tool_result(identifier,name,arguments):
 response=rpc(identifier,'tools/call',{'name':name,'arguments':arguments});assert 'error' not in response,response
 payload=response['result'];assert not payload.get('isError',False),response
 return checked_result(payload['structuredContent'])
try:
 assert rpc(1,'initialize',{'protocolVersion':'2025-11-25','capabilities':{},'clientInfo':{'name':'package-test','version':'1'}})['result']['protocolVersion']=='2025-11-25'
 mcp.stdin.write(b'{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}\n');mcp.stdin.flush()
 listing=rpc(2,'tools/list',{})['result']['tools'];assert len(listing)==11 and len({tool['name'] for tool in listing})==11
 continuation=next(tool['inputSchema'] for tool in listing if tool['name']=='next_page')
 assert set(continuation['properties'])=={'cursor','deadline_ms','response_max_bytes'} and continuation['required']==['cursor'] and continuation['additionalProperties'] is False
 found=tool_result(3,'search_symbols',{'name':'answer'});assert found['symbols']==query({'op':'symbols','name':'answer'})['symbols']
 page=tool_result(30,'search_symbols',{'limit':1000,'page_size':1});token=page['page']['cursor'];assert page['page']['next_cursor'] and len(page['symbols'])==1
 def frozen_page(result):
  result=dict(result);result.pop('freshness',None);result.pop('coordination',None);return result
 replay=tool_result(31,'next_page',{'cursor':token});assert frozen_page(replay)==frozen_page(page)
 assert frozen_page(query({'op':'next_page','cursor':token}))==frozen_page(page)
 next_page=tool_result(32,'next_page',{'cursor':page['page']['next_cursor']});assert next_page['generation']==page['generation'] and next_page['page']['cursor']!=token
 rejected=rpc(33,'tools/call',{'name':'next_page','arguments':{'cursor':token,'page_size':2}})['result']
 assert rejected['isError'] and rejected['structuredContent']['error']['code']=='invalid_request',rejected
 checks.append('installed_offline_MCP_pagination_replay_and_native_CLI_token_portability')
 old=ready(node)['generation'];(WORKSPACE/'value.ts').write_text('export function savedAnswer() { return 43; }\n');saved_ts=ready(node,old+1);MIN_GENERATION=saved_ts['generation']
 saved=query({'op':'symbols','name':'savedAnswer'});assert saved['freshness']=='current' and saved['symbols']
 removed=query({'op':'symbols','name':'answer'});assert removed.get('symbols',[])==[],removed
 ts_context=query({'op':'symbol_context','symbol_id':saved['symbols'][0]['id'],'max_bytes':1024});exact_source(ts_context['contexts'][0]['declarations'][0]['source'],WORKSPACE/'value.ts')
 old=ready(node)['generation'];(WORKSPACE/'Code.cs').write_text(CSHARP.replace('Greet() => 42','SavedGreet() => 43'))
 saved_cs=ready(node,old+1);MIN_GENERATION=saved_cs['generation']
 saved=query({'op':'symbols','name':'SavedGreet'});assert saved['freshness']=='current' and saved['symbols']
 removed=query({'op':'symbols','name':'Greet'});assert removed.get('symbols',[])==[],removed
 symbol=saved['symbols'][0];context=query({'op':'symbol_context','symbol_id':symbol['id'],'max_bytes':1024});source=context['contexts'][0]['declarations'][0]['source'];assert 'SavedGreet() => 43' in source['text'];exact_source(source,WORKSPACE/'Code.cs')
 for identifier,name in [(4,'savedAnswer'),(5,'SavedGreet')]:
  found=tool_result(identifier,'search_symbols',{'name':name})
  assert found['freshness']=='current' and found['symbols']==query({'op':'symbols','name':name})['symbols']
 final_generation=saved_cs['generation'];checks.append('offline_saved_TS_and_Csharp_semantic_and_source_updates')
finally:mcp.stdin.close();mcp.wait(timeout=10);mcp_selector.close();(PREFIX/'mcp.json').write_text(json.dumps(mcp_events,indent=2)+'\n')
checks.append('installed_MCP_shared_queries_and_EOF')
if args.experimental:
 before=ready(node)['generation']
 (WORKSPACE/'Cargo.toml').write_text('[package]\nname="packaged"\nversion="0.1.0"\nedition="2021"\n[lib]\npath="lib.rs"\n')
 (WORKSPACE/'lib.rs').write_text('pub fn packaged_rust() -> i32 { 42 }\n')
 (WORKSPACE/'Sample.swift').write_text('struct PackagedSwiftType { let value: Int }\n')
 ready(node,before+1)
 # More saves can queue while discovery reconciles; wait for both names.
 until=time.monotonic()+90
 while time.monotonic()<until:
  ready(node)
  rust=query({'op':'symbols','name':'packaged_rust'})
  swift=query({'op':'symbols','name':'PackagedSwiftType'})
  if rust.get('symbols') and swift.get('symbols'):break
 else:raise AssertionError((rust,swift))
 assert any(c['status']=='partial' for c in rust['coverage'])
 checks.append('experimental_packaged_Swift_reader_and_Rust_adapter')
final_generation=ready(node)['generation'];node.close();net.close();checks.append('packaged_saved_file_updates')
command(HOST,'coordinator','stop');until=time.monotonic()+20
while (DATA/'c.sock').exists() and time.monotonic()<until:time.sleep(.05)
new_parent=PREFIX/'second installation';new_parent.mkdir();bundle=bundle.rename(new_parent/bundle.name);HOST.unlink();HOST.symlink_to(bundle/'bin/know-code')
info=json.loads(command(HOST,'runtime-info').stdout);assert str(new_parent) in info['resource_directory']
assert all(info[key]==version for key,version in contracts.items()),info
node=Client('node');ready(node);assert node.send('query',request={'op':'symbols','name':'savedAnswer'})['result']['freshness']=='current';node.close();checks.append('runtime_relocation_and_SDK_reconnect_without_reinstall')
missing=PREFIX/'missing';missing.mkdir();missing_package='KnowCodeOfflineMissingDependency'
(missing/'Missing.csproj').write_text('<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><TargetFramework>net10.0</TargetFramework></PropertyGroup><ItemGroup><PackageReference Include="'+missing_package+'" Version="1.0.0" /></ItemGroup></Project>')
(missing/'Missing.cs').write_text('public class MissingConsumer { MissingDependency.Type value; }\n')
assert not (missing/'obj/project.assets.json').exists()
failure=command(HOST,'index',missing,'--configuration','Release',check=False)
negative={'returncode':failure.returncode,'stdout':failure.stdout,'stderr':failure.stderr}
if failure.returncode==0:
 indexed=json.loads(failure.stdout)
 lookup=json.loads(command(HOST,'query',indexed['database'],json.dumps({'op':'symbols','limit':1})).stdout);assert lookup.get('result',{}).get('snapshot'),lookup
 coverage=json.loads(command(HOST,'query',indexed['database'],json.dumps({'op':'coverage','generation':indexed['generation'],'snapshot':lookup['result']['snapshot']})).stdout)
 assert 'result' in coverage and coverage['result']['coverage'] and any(c['status']!='complete' for c in coverage['result']['coverage']),coverage
 negative['coverage']=coverage
assert missing_package.lower() in json.dumps(negative).lower(),negative
(PREFIX/'missing-restoration.json').write_text(json.dumps(negative,indent=2)+'\n')
healthy=json.loads(command(HOST,'query',DATABASE,json.dumps({'op':'symbols','name':'SavedGreet'})).stdout)['result'];assert healthy['freshness']=='current' and healthy['symbols'],healthy
checks.append('offline_missing_restoration_is_actionable_and_other_workspace_stays_healthy')
command(HOST,'coordinator','stop');until=time.monotonic()+20
while (DATA/'c.sock').exists() and time.monotonic()<until:time.sleep(.05)
resources=bundle/'lib/know-code';worker=resources/'providers/typescript/worker.mjs';backup=worker.with_suffix('.held');worker.rename(backup)
try:failure=command(HOST,'runtime-info',check=False);assert failure.returncode and 'missing' in failure.stderr
finally:backup.rename(worker)
manifest_file=resources/'runtime.json';saved=manifest_file.read_bytes();broken=json.loads(saved);broken['worker_protocol']=999;manifest_file.write_text(json.dumps(broken))
try:failure=command(HOST,'runtime-info',check=False);assert failure.returncode and 'incompatible' in failure.stderr
finally:manifest_file.write_bytes(saved)
invalid=PREFIX/'invalid-resources';invalid.mkdir();failure=command(HOST,'runtime-info',check=False,env=dict(ENV,KNOW_CODE_RUNTIME_DIR=str(invalid)));assert failure.returncode and 'manifest' in failure.stderr
bare=PREFIX/'bare';bare.mkdir();shutil.copy2(bundle/'bin/know-code',bare/'know-code');failure=command(bare/'know-code','runtime-info',check=False);assert failure.returncode and 'not found' in failure.stderr
checks+=['missing_resource_no_checkout_fallback','manifest_protocol_mismatch','invalid_override_no_silent_fallback','bare_executable_actionable_error']
result={'passed':True,'prefix':str(PREFIX),'packages':str(PACKAGES),'matrix':manifest['runtime'],'checks':checks,'final_generation':final_generation,'checkout_denied':True,'denied_checkouts':[str(root) for root in CHECKOUTS],'network_denied_after_restoration':True,'archive_sha256':{a['file']:a['sha256'] for a in manifest['artifacts']},'candidate_source_revision':manifest.get('source_revision'),'candidate_manifest_sha256':hashlib.sha256((PREFIX/'candidate-manifest.json').read_bytes()).hexdigest(),'phase_evidence':str(PREFIX/'phases.json'),'network_policy_proof':str(PREFIX/'network-policy-proof.json')}
(PREFIX/'result.json').write_text(json.dumps(result,indent=2)+'\n');report=ROOT/'.artifacts/packaging/installed-test.json';report.parent.mkdir(parents=True,exist_ok=True);report.write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result,indent=2))
