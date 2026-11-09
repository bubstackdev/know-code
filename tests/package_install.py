"""Install real local archives outside the checkout; deny tested processes checkout access."""
from pathlib import Path
import argparse,atexit,hashlib,json,os,selectors,shutil,subprocess,tarfile,time,zipfile
ROOT=Path(__file__).resolve().parents[1]
parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('packages',type=Path);parser.add_argument('--experimental',action='store_true');parser.add_argument('--prefix',type=Path,required=True);args=parser.parse_args()
PACKAGES=args.packages.resolve();PREFIX=args.prefix.resolve();assert PREFIX!=ROOT and ROOT not in PREFIX.parents,'package test prefix must be outside checkout';PREFIX.mkdir(parents=True,exist_ok=False)
DATA=PREFIX/'d';BIN=PREFIX/'bin';BIN.mkdir();FEED=PREFIX/'feed';FEED.mkdir();WORKSPACE=PREFIX/'w';WORKSPACE.mkdir();CLIENTS=[];checks=[]
ENV=dict(os.environ,PATH=str(BIN)+':'+os.environ['PATH'],KNOW_CODE_DATA_DIR=str(DATA),NUGET_PACKAGES=str(PREFIX/'nuget-cache'),DOTNET_CLI_TELEMETRY_OPTOUT='1',DOTNET_NOLOGO='1',MSBUILDDISABLENODEREUSE='1',DOTNET_CLI_USE_MSBUILD_SERVER='0',npm_config_cache=str(PREFIX/'npm-cache'))
for key in ['KNOW_CODE_RUNTIME_DIR','KNOW_CODE_COORDINATOR','NODE_PATH','NODE_OPTIONS']:ENV.pop(key,None)
POLICY=f'(version 1) (allow default) (deny file-read* file-write* (subpath {json.dumps(str(ROOT))}))'
(PREFIX/'deny-checkout.sb').write_text(POLICY+'\n')
def command(*args,cwd=PREFIX,check=True,env=ENV,sandbox=True):
 cmd=(['/usr/bin/sandbox-exec','-f',str(PREFIX/'deny-checkout.sb')] if sandbox else [])+list(map(str,args))
 p=subprocess.run(cmd,cwd=cwd,env=env,text=True,capture_output=True,timeout=120)
 assert not check or p.returncode==0,(cmd,p.returncode,p.stdout,p.stderr)
 return p

# Review integrity and archive inventory before installation.
manifest=json.loads((PACKAGES/'manifest.json').read_text());assert manifest['candidate']
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
assert command('/bin/cat',ROOT/'LICENSE',check=False).returncode!=0,'checkout deny policy ineffective'
info=json.loads(command(HOST,'runtime-info').stdout);assert info['layout']=='bundle' and info['sqlite']['fts5'] and str(PREFIX) in info['resource_directory']
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
(WORKSPACE/'value.ts').write_text('export function leaf() { return 42; }\nexport function answer() { return leaf(); }\n')
(WORKSPACE/'App.csproj').write_text('<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><TargetFramework>net10.0</TargetFramework><LangVersion>latest</LangVersion></PropertyGroup></Project>')
(WORKSPACE/'Code.cs').write_text('using System.Text.Json.Serialization; public static class Greeter { public static int Greet() => 42; } public record Person(string Name); [JsonSerializable(typeof(Person))] public partial class MyContext : JsonSerializerContext { }\n')

class Client:
 def __init__(self,kind):
  opts=json.dumps({'dataDirectory':str(DATA),'configuration':'Release'})
  argv=['node',str(NODE/'client.mjs'),'unused',str(WORKSPACE),opts] if kind=='node' else ['dotnet',str(DOTNET/'bin/Release/net10.0/Consumer.dll'),'--workspace',str(WORKSPACE),opts]
  self.kind=kind;self.events=[];self.buffer=b''
  self.p=subprocess.Popen(['/usr/bin/sandbox-exec','-f',str(PREFIX/'deny-checkout.sb'),*argv],cwd=PREFIX,env=ENV,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=(PREFIX/(kind+'-stderr.log')).open('ab'))
  self.selector=selectors.DefaultSelector();self.selector.register(self.p.stdout,selectors.EVENT_READ);CLIENTS.append(self);self.opened=self.read();assert 'opened' in self.opened,self.opened
 def read(self):
  until=time.monotonic()+30
  while b'\n' not in self.buffer:
   assert time.monotonic()<until,('client timeout',self.kind,self.events)
   if not self.selector.select(.1):continue
   block=os.read(self.p.stdout.fileno(),65536);assert block,('client EOF',self.kind,self.p.poll());self.buffer+=block
  line,self.buffer=self.buffer.split(b'\n',1);v=json.loads(line);self.events.append(v);return v
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
node=Client('node');initial=ready(node);net=Client('net');assert net.opened['opened']['database']==initial['database'];DATABASE=initial['database']
def query(req):
 n=node.send('query',request=req);d=net.send('query',request=req);c=json.loads(command(HOST,'query',DATABASE,json.dumps(req)).stdout);assert n==d==c,(req,n,d,c);return n['result']
for name in ['answer','Greet']:
 result=query({'op':'symbols','name':name});assert result['freshness']=='current' and result['symbols']
 symbol=result['symbols'][0];context=query({'op':'symbol_context','symbol_id':symbol['id'],'max_bytes':1024});assert context['contexts'][0]['declarations'][0]['source']
lexical=query({'op':'symbols','query':'Greeter'});assert lexical['symbols']
all_symbols=query({'op':'symbols','limit':1000});assert any(l['generated'] for s in all_symbols['symbols'] for l in s['locations'])
checks+=['packaged_TS_and_Csharp_compiler_analysis','both_inprocess_SDKs_match_CLI','compiler_context_and_FTS','Roslyn_generated_sources']
# MCP startup/query/EOF over the installed executable, using the same saved publication.
mcp=subprocess.Popen(['/usr/bin/sandbox-exec','-f',str(PREFIX/'deny-checkout.sb'),str(HOST),'mcp','--workspace',str(WORKSPACE)],cwd=PREFIX,env=ENV,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=(PREFIX/'mcp-stderr.log').open('w'))
def rpc(identifier,method,params):
 mcp.stdin.write((json.dumps({'jsonrpc':'2.0','id':identifier,'method':method,'params':params})+'\n').encode());mcp.stdin.flush()
 while True:
  value=json.loads(mcp.stdout.readline());
  if value.get('id')==identifier:return value
try:
 assert rpc(1,'initialize',{'protocolVersion':'2025-11-25','capabilities':{},'clientInfo':{'name':'package-test','version':'1'}})['result']['protocolVersion']=='2025-11-25'
 mcp.stdin.write(b'{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}\n');mcp.stdin.flush()
 assert len(rpc(2,'tools/list',{})['result']['tools'])==10
 found=rpc(3,'tools/call',{'name':'search_symbols','arguments':{'name':'answer'}})['result']['structuredContent'];assert found['result']['freshness']=='current'
finally:mcp.stdin.close();mcp.wait(timeout=10)
checks.append('installed_MCP_shared_queries_and_EOF')
old=initial['generation'];(WORKSPACE/'value.ts').write_text('export function savedAnswer() { return 43; }\n');ready(node,old+1);assert query({'op':'symbols','name':'savedAnswer'})['symbols']
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
node.close();net.close();checks.append('packaged_saved_file_updates')
command(HOST,'coordinator','stop');until=time.monotonic()+20
while (DATA/'c.sock').exists() and time.monotonic()<until:time.sleep(.05)
new_parent=PREFIX/'second installation';new_parent.mkdir();bundle=bundle.rename(new_parent/bundle.name);HOST.unlink();HOST.symlink_to(bundle/'bin/know-code')
info=json.loads(command(HOST,'runtime-info').stdout);assert str(new_parent) in info['resource_directory']
node=Client('node');ready(node);assert node.send('query',request={'op':'symbols','name':'savedAnswer'})['result']['freshness']=='current';node.close();checks.append('runtime_relocation_and_SDK_reconnect_without_reinstall')
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
result={'passed':True,'prefix':str(PREFIX),'packages':str(PACKAGES),'matrix':manifest['runtime'],'checks':checks,'final_generation':initial['generation'],'checkout_denied':True}
(PREFIX/'result.json').write_text(json.dumps(result,indent=2)+'\n');(ROOT/'.artifacts/packaging/installed-test.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result,indent=2))
