"""Install archives, restore dependencies, then test with checkout/network access denied."""
from pathlib import Path
import argparse,atexit,hashlib,json,os,re,selectors,shlex,shutil,socket,subprocess,sys,tarfile,time,zipfile
ROOT=Path(__file__).resolve().parents[1]
parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('packages',type=Path);parser.add_argument('--experimental',action='store_true');parser.add_argument('--prefix',type=Path,required=True);parser.add_argument('--deny-checkout',type=Path,action='append',default=[],help='additional build-source checkout to deny when archives came from another tree');parser.add_argument('--signed-team-id',help='Require Developer ID signatures and this Apple Team ID');parser.add_argument('--signed-signer-sha256',help='Externally pinned signing certificate DER SHA-256');args=parser.parse_args()
assert bool(args.signed_team_id)==bool(args.signed_signer_sha256),'signed mode requires both Team ID and pinned signer SHA-256'
PACKAGES=args.packages.resolve();PREFIX=args.prefix.resolve();assert PREFIX!=ROOT and ROOT not in PREFIX.parents,'package test prefix must be outside checkout';PREFIX.mkdir(parents=True,exist_ok=False)
CHECKOUTS=sorted({ROOT,*[p.resolve() for p in args.deny_checkout]})
assert all(root!=PREFIX and root not in PREFIX.parents and (root/'LICENSE').is_file() for root in CHECKOUTS),'each denied checkout must exist and exclude the test prefix'
DATA=PREFIX/'d';BIN=PREFIX/'bin';BIN.mkdir();FEED=PREFIX/'feed';FEED.mkdir();WORKSPACE=PREFIX/'w';WORKSPACE.mkdir();SCRATCH=PREFIX/'t';SCRATCH.mkdir();CLIENTS=[];checks=[];PHASE='dependency_setup';PHASES=[]
ENV=dict(os.environ,PATH=str(BIN)+':'+os.environ['PATH'],KNOW_CODE_DATA_DIR=str(DATA),NUGET_PACKAGES=str(PREFIX/'nuget-cache'),DOTNET_CLI_TELEMETRY_OPTOUT='1',DOTNET_NOLOGO='1',MSBUILDDISABLENODEREUSE='1',DOTNET_CLI_USE_MSBUILD_SERVER='0',TMPDIR=str(SCRATCH),npm_config_cache=str(PREFIX/'npm-cache'),npm_config_fetch_retries='0',npm_config_fetch_timeout='3000')
ENV['KNOW_CODE_QUERY_SCOPE']=str(PREFIX/'query-admission')
for key in ['KNOW_CODE_RUNTIME_DIR','KNOW_CODE_COORDINATOR','NODE_PATH','NODE_OPTIONS']:ENV.pop(key,None)
# Keep the account HOME and default profile paths; only this fixture's npm
# configuration is selected. Ambient install-links overrides must not mask it.
for key in ['npm_config_install_links','NPM_CONFIG_INSTALL_LINKS','pnpm_config_install_links','PNPM_CONFIG_INSTALL_LINKS']:ENV.pop(key,None)
for name in ['user','global']:
 config=PREFIX/('npm-'+name+'config');config.write_text('')
 for key in ['npm_config_'+name+'config','NPM_CONFIG_'+name.upper()+'CONFIG']:ENV[key]=str(config)
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

# Authenticate signed distributions before any native execution. Unsigned local
# candidates retain the existing path and do not count toward the signing gate.
SIGNING=None
if args.signed_team_id:
 PHASE='signature_verification'
 SIGNING=json.loads(command(sys.executable,ROOT/'scripts/sign_packages.py','verify','--packages',PACKAGES,'--work-dir',PREFIX/'signature-verification','--team-id',args.signed_team_id,'--signer-sha256',args.signed_signer_sha256,sandbox=False).stdout)
 assert SIGNING['verified'] and SIGNING['notarization']=='not_performed'
 checks.append('authenticated_receipt_and_final_archive_Developer_ID_signatures')
 PHASE='dependency_setup'

# Review integrity and archive inventory before installation.
manifest=json.loads((PACKAGES/'manifest.json').read_text());assert manifest['candidate']
VERSION=manifest['version'];assert re.fullmatch(r'[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?',VERSION)
shutil.copy2(PACKAGES/'manifest.json',PREFIX/'candidate-manifest.json')
for artifact in manifest['artifacts']:
 source=PACKAGES/artifact['file'];assert hashlib.sha256(source.read_bytes()).hexdigest()==artifact['sha256']
 shutil.copy2(source,FEED/source.name)
 assert hashlib.sha256((FEED/source.name).read_bytes()).hexdigest()==artifact['sha256']
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
contracts={'worker_protocol':7,'store_format':8,'index_lifecycle_version':4,'analysis_settings_version':2,'resource_control_version':3}
assert all(info[key]==runtime_manifest[key]==version for key,version in contracts.items()),(info,runtime_manifest)
assert runtime_manifest['version']==VERSION
assert {p['language']:p['status'] for p in runtime_manifest['providers']}==dict.fromkeys(['typescript','csharp','rust','swift'],'required'),runtime_manifest
assert command(HOST,'--version').stdout.strip()==f'know-code {VERSION}'
checks+=['checkout_access_denied','runtime_discovery_through_symlink_and_spaces','bundled_SQLite_FTS5']
for native in [bundle/'bin/know-code',PREFIX/'unused']:
 if not native.exists():continue
 assert command('lipo','-archs',native).stdout.strip()=='arm64'

# Install through the actual package managers, offline, with no checkout project reference.
NODE=PREFIX/'node-consumer';NODE.mkdir();(NODE/'package.json').write_text('{"name":"package-test","private":true,"type":"module"}\n')
command('npm','install','--offline','--ignore-scripts','--no-audit','--no-fund',next(FEED.glob('know-code-node-*.tgz')),cwd=NODE)
node_driver=(ROOT/'tests/workspace_client.mjs').read_text().replace("await import(pathToFileURL(process.argv[2]))","await import('@know-code/node')")
(NODE/'client.mjs').write_text(node_driver)
DOTNET=PREFIX/'net-consumer';DOTNET.mkdir();(DOTNET/'Consumer.csproj').write_text(f'<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><OutputType>Exe</OutputType><TargetFramework>net10.0</TargetFramework><UseAppHost>false</UseAppHost><ImplicitUsings>enable</ImplicitUsings></PropertyGroup><ItemGroup><PackageReference Include="KnowCode" Version="{VERSION}" /></ItemGroup></Project>')
(DOTNET/'Program.cs').write_text((ROOT/'bindings/dotnet/Smoke/Program.cs').read_text())
(PREFIX/'NuGet.Config').write_text('<configuration><packageSources><clear/><add key="candidate" value="'+str(FEED)+'" /></packageSources></configuration>')
command('dotnet','build',DOTNET/'Consumer.csproj','-c','Release','--nologo','-p:UseSharedCompilation=false','-nodeReuse:false',cwd=DOTNET)
checks+=['npm_archive_installed_offline','NuGet_archive_installed_offline_with_native_assets']
(WORKSPACE/'tsconfig.json').write_text('{"compilerOptions":{"target":"ES2022","module":"NodeNext","moduleResolution":"NodeNext","strict":true,"noEmit":true},"files":["value.ts"]}')
(WORKSPACE/'value.ts').write_text('// café 😀 offline source\nimport { dependencyValue } from "@know-code/offline-dependency";\nexport function leaf() { return dependencyValue(); }\nexport function answer() { return leaf(); }\n')
(WORKSPACE/'App.csproj').write_text(f'<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><TargetFramework>net10.0</TargetFramework><LangVersion>latest</LangVersion></PropertyGroup><ItemGroup><PackageReference Include="KnowCode" Version="{VERSION}" /></ItemGroup></Project>')
CSHARP='// café 😀 offline source\nusing System.Text.Json.Serialization; public static class Greeter { public static int Greet() => 42; public static int GreetCaller() => Greet(); public static KnowCode.QueryRequest Request() => new("symbols"); } public record Person(string Name); [JsonSerializable(typeof(Person))] public partial class MyContext : JsonSerializerContext { }\n'
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
assets=json.loads((WORKSPACE/'obj/project.assets.json').read_text());assert f'KnowCode/{VERSION}' in assets['libraries']
# Restore only the two exact additional compiler selections before denial.
# These owned projects contain no serving state; normal installed discovery and
# common trust/preparation must select them after first offline registration.
selected_typescript=[]
for suffix,version in [('6','6.0.2'),('7','7.0.2')]:
 directory=WORKSPACE/('selected-ts'+suffix);directory.mkdir()
 prefix='selected_ts'+suffix
 source=directory/'Code.ts'
 source.write_text('// café 🦀 selected compiler source\n'
                   +f'export function {prefix}_leaf(value: number): number {{ return value + 1; }}\n'
                   +f'export function {prefix}_entry(): number {{ return {prefix}_leaf(41); }}\n'
                   +f'export function {prefix}_reference() {{ return {prefix}_leaf; }}\n')
 (directory/'tsconfig.json').write_text(json.dumps({'compilerOptions':{'target':'ES2022','module':'NodeNext','moduleResolution':'NodeNext','strict':True,'noEmit':True},'files':['Code.ts']})+'\n')
 dependencies={'typescript':version}
 if suffix=='7':dependencies['@typescript/typescript-darwin-arm64']=version
 (directory/'package.json').write_text(json.dumps({'name':'selected-typescript-'+suffix,'private':True,'type':'module','packageManager':'npm@'+npm_version,'devDependencies':dependencies})+'\n')
 command('npm','install','--ignore-scripts','--no-audit','--no-fund',cwd=directory)
 selected=directory/'node_modules/typescript/package.json';compiler=json.loads(selected.read_text())
 assert compiler['name']=='typescript' and compiler['version']==version,compiler
 evidence={'version':version,'project':str(directory.relative_to(WORKSPACE)),'source':str(source),
           'compiler_manifest':str(selected),'compiler_manifest_sha256':hashlib.sha256(selected.read_bytes()).hexdigest(),
           'lock_sha256':hashlib.sha256((directory/'package-lock.json').read_bytes()).hexdigest(),'scripts':'disabled'}
 if suffix=='7':
  native_manifest=directory/'node_modules/@typescript/typescript-darwin-arm64/package.json'
  native=json.loads(native_manifest.read_text());assert native['name']=='@typescript/typescript-darwin-arm64' and native['version']==version,native
  binary=native_manifest.parent/'lib/tsc';assert binary.is_file() and os.access(binary,os.X_OK),binary
  evidence.update(native_manifest=str(native_manifest),native_manifest_sha256=hashlib.sha256(native_manifest.read_bytes()).hexdigest(),
                  native_binary=str(binary),native_binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest())
 selected_typescript.append(dict(evidence,suffix=suffix))
(PREFIX/'selected-typescript-setup.json').write_text(json.dumps(selected_typescript,indent=2)+'\n')
checks.append('exact_TS6_TS7_and_native_platform_packages_restored_scripts_off_before_denial')
# Acquire only a tool distribution, through the shipped helper, before Internet
# denial. This cache lives outside DATA and cannot pre-seed a serving index.
TOOL_CACHE=PREFIX/'tool-cache';npm10_prime=PREFIX/'prime-npm10.mjs'
npm10_prime.write_text("import {pathToFileURL} from 'node:url';const {packageTool,selection}=await import(pathToFileURL(process.argv[2]));console.log(JSON.stringify(await packageTool(selection('npm@10.0.0'),process.argv[3],()=>{})));\n")
npm10_tool=json.loads(command('node',npm10_prime,resources/'providers/typescript/package-tools.mjs',TOOL_CACHE).stdout)
assert npm10_tool['name']=='npm' and npm10_tool['version']=='10.0.0' and npm10_tool['source']=='user-cache',npm10_tool
assert Path(npm10_tool['cacheDirectory']).is_relative_to(TOOL_CACHE),npm10_tool
def cache_inventory(directory):
 return {str(p.relative_to(directory)):hashlib.sha256(p.read_bytes()).hexdigest() for p in sorted(directory.rglob('*')) if p.is_file()}
npm10_cache_inventory=cache_inventory(TOOL_CACHE)
assert npm10_cache_inventory and len(list(TOOL_CACHE.glob('*/tool.json')))==1
guard_relative=Path('providers/typescript/npm10-guard.mjs');guard_sha256=hashlib.sha256((resources/guard_relative).read_bytes()).hexdigest()
npm10_evidence={'pin':'npm@10.0.0','primed_tool':npm10_tool,'cache_inventory':npm10_cache_inventory,'primed_guard':str(resources/guard_relative),'guard_sha256':guard_sha256,'steps':[]}
(PREFIX/'npm10-installed.json').write_text(json.dumps(npm10_evidence,indent=2)+'\n')
assert not DATA.exists(),'dependency setup created coordinator state before the denial boundary'
PHASES.append({'phase':PHASE,'finished_ns':time.time_ns(),'typescript_version':json.loads((WORKSPACE/'node_modules/typescript/package.json').read_text())['version'],'npm_version':npm_version,'npm10_tool_cache':str(TOOL_CACHE),'nuget_libraries':sorted(assets['libraries']),'serving_indexes':[],'coordinator_registry_exists':False})
prove_network_policy()
PHASE='network_denied_runtime';PHASES.append({'phase':PHASE,'started_ns':time.time_ns(),'policy':str(POLICY_FILE),'index_state_before_start':[]})
(PREFIX/'phases.json').write_text(json.dumps(PHASES,indent=2)+'\n')
assert not DATA.exists(),'an index/coordinator appeared before first offline registration'
for root in CHECKOUTS:assert command('/bin/cat',root/'LICENSE',check=False).returncode!=0
checks+=['project_dependencies_restored_before_registration','fresh_initial_index_starts_under_network_denial']
# Only after both original no-state assertions and actual network denial proof:
# copy the tool cache, never registry/settings/workspace/index state.
DATA.mkdir(mode=0o700)
shutil.copytree(TOOL_CACHE,DATA/'package-tools')
assert sorted(p.name for p in DATA.iterdir())==['package-tools']
assert cache_inventory(DATA/'package-tools')==npm10_cache_inventory
npm10_evidence['installed_cache_directory']=str(DATA/'package-tools')
(PREFIX/'npm10-installed.json').write_text(json.dumps(npm10_evidence,indent=2)+'\n')
checks.append('installed_npm10_tool_primed_without_coordinator_or_index_before_denial')

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
assert WORKSPACE.resolve().is_relative_to(PREFIX) and DATA.resolve().is_relative_to(PREFIX)
command(HOST,'trust',WORKSPACE) # Real CLI under the existing denied-network/checkout policy.
checks.append('explicit_project_trust_granted_offline_before_registration')
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
 # Completeness belongs to the compilations supporting these findings;
 # the independent selected TS7 project intentionally remains partial.
 owners={symbol['compilation_id'] for symbol in result['symbols']}
 owners.update(support['compilation_id'] for symbol in result['symbols'] for support in symbol.get('supporting_targets',[]))
 reports={row['compilation_id']:row for row in result['coverage']}
 assert owners and owners<=reports.keys(),result
 expected_language='typescript' if name=='answer' else 'csharp'
 assert all(reports[owner]['status']=='complete' and reports[owner]['language']==expected_language for owner in owners),result
 if name=='answer':assert all(reports[owner]['compiler_version']==json.loads((WORKSPACE/'node_modules/typescript/package.json').read_text())['version'] for owner in owners),result
 symbol=result['symbols'][0];context=query({'op':'symbol_context','symbol_id':symbol['id'],'max_bytes':1024});assert context['contexts'][0]['declarations'][0]['source']
 exact_source(context['contexts'][0]['declarations'][0]['source'],WORKSPACE/('value.ts' if name=='answer' else 'Code.cs'))
leaf=query({'op':'symbols','name':'leaf'})['symbols'][0]
references=query({'op':'references','symbol_id':leaf['id']});assert references['freshness']=='current' and references['occurrences']
lexical=query({'op':'symbols','query':'Greeter'});assert lexical['symbols']
all_symbols=query({'op':'symbols','limit':1000});assert any(l['generated'] for s in all_symbols['symbols'] for l in s['locations'])
checks+=['packaged_TS_and_Csharp_compiler_analysis','both_inprocess_SDKs_match_CLI','compiler_context_and_FTS','Roslyn_generated_sources']
for caller,callee,filename in [('answer','leaf','value.ts'),('GreetCaller','Greet','Code.cs')]:
 root=query({'op':'symbols','name':caller})['symbols'][0]
 req={'op':'callees','symbol_id':root['id']}
 compact=query(req)['graph'];assert len(compact['findings'])==1 and compact['findings'][0]['symbol']['name']==callee,compact
 assert 'context' not in compact['findings'][0],compact
 enriched=query(dict(req,include_source=True))['graph'];declarations=enriched['findings'][0]['context']['declarations']
 assert declarations and all(d['source_status']=='included' for d in declarations),enriched
 for declaration in declarations:exact_source(declaration['source'],WORKSPACE/filename)
 limited=query(dict(req,include_source=True,source_max_bytes=1))
 assert limited['truncated'] and 'source_budget' in limited['graph']['limits'],limited
 assert [f['symbol'] for f in limited['graph']['findings']]==[f['symbol'] for f in compact['findings']],limited
checks.append('installed_offline_TS_Csharp_graph_source_defaults_exact_bodies_limits_native_parity')
# Fixed supported compiler cells, not a package-manager/configuration matrix.
# All reads use the installed runtime and both installed in-process SDKs.
selected_typescript_results=[]
for case in selected_typescript:
 prefix='selected_ts'+case['suffix'];filename=Path(case['source'])
 found=query({'op':'symbols','name':prefix+'_entry','coverage_detail':'full','detail_max_bytes':262144})
 reports=[row for row in found['coverage'] if row['language']=='typescript' and row['project_path']==case['project']+'/tsconfig.json']
 assert len(reports)==1 and reports[0]['compiler_version']==case['version'],reports
 report=reports[0];options=report['analysis_options'];compilation=report['compilation_id']
 assert not report['inferred'] and (WORKSPACE/report['project_path']).resolve()==(filename.parent/'tsconfig.json').resolve(),report
 if case['suffix']=='7':
  assert options['knowCodeNativeCompiler']=='7.0.2' and options['knowCodeNativeApi']=='unstable/sync',report
  assert report['status']=='partial' and report['capabilities']['project_links']=='unavailable',report
 else:
  assert 'knowCodeNativeCompiler' not in options and options['knowCodeSourceReferenceRedirect'] is True,report
 symbols={}
 for role in ['entry','leaf','reference']:
  values=query({'op':'symbols','name':prefix+'_'+role,'compilation_id':compilation})['symbols']
  matches=[symbol for symbol in values if symbol['name']==prefix+'_'+role and not symbol['external'] and symbol['compilation_id']==compilation]
  assert len(matches)==1,(role,matches);symbols[role]=matches[0]
 scope={'snapshot':found['snapshot'],'max_depth':1}
 callees=query(dict(scope,op='callees',symbol_id=symbols['entry']['id'],include_source=True,source_max_bytes=4096))
 called=next(finding for finding in callees['graph']['findings'] if finding['symbol']['id']==symbols['leaf']['id'])
 assert called['paths'] and all(len(path)==1 and path[0]['kind']=='call' and path[0]['resolution']=='compiler_bound'
                               and path[0]['source_id']==symbols['entry']['id'] and path[0]['target_id']==symbols['leaf']['id']
                               for path in called['paths']),called
 declarations=called['context']['declarations'];assert declarations and all(d['source_status']=='included' for d in declarations),called
 for declaration in declarations:exact_source(declaration['source'],filename)
 impact=query(dict(scope,op='impact',symbol_id=symbols['leaf']['id']))
 assert {symbols['entry']['id'],symbols['reference']['id']}<={finding['symbol']['id'] for finding in impact['graph']['findings']},impact
 location=symbols['leaf']['locations'][0]
 retained=query({'op':'source','artifact_id':location['artifact_id'],'snapshot':found['snapshot'],'content_hash':location['content_hash'],'max_bytes':4096})['source']
 exact_source(retained,filename)
 offset=retained['start_byte'];assert retained['text'].encode()[location['start_byte']-offset:location['end_byte']-offset].decode()==prefix+'_leaf'
 selected_typescript_results.append({'setup':case,'coverage':report,'snapshot':found['snapshot'],'generation':found['generation'],
                                     'callees':callees['graph'],'impact':impact['graph'],'source':retained})
(PREFIX/'selected-typescript-results.json').write_text(json.dumps(selected_typescript_results,indent=2)+'\n')
checks.append('installed_offline_exact_TS6_legacy_TS7_native_graph_value_impact_source_and_three_client_parity')
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
 enriched_args={'symbol_id':leaf['id'],'include_source':True,'source_max_bytes':1024}
 assert tool_result(34,'callers',enriched_args)==query(dict(enriched_args,op='callers'))
 checks.append('installed_offline_MCP_graph_source_matches_native_projection')
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
 old=ready(node)['generation'];(WORKSPACE/'Code.cs').write_text(CSHARP.replace('Greet() => 42','SavedGreet() => 43').replace('=> Greet();','=> SavedGreet();'))
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
 rust_package=WORKSPACE/'rust-package';rust_package.mkdir()
 swift_package=WORKSPACE/'swift-package';swift_sources=swift_package/'Sources/PackagedSwift';swift_sources.mkdir(parents=True)
 (rust_package/'Cargo.toml').write_text('[package]\nname="packaged"\nversion="0.1.0"\nedition="2021"\n[lib]\npath="lib.rs"\n[workspace]\n')
 rust_file=rust_package/'lib.rs';swift_file=swift_sources/'Model.swift'
 rust_text='// café 🦀 installed offline source\npub fn packaged_rust_leaf(value: i32) -> i32 { value + 1 }\npub fn packaged_rust_entry() -> i32 { packaged_rust_leaf(41) }\npub fn packaged_rust_reference() -> fn(i32) -> i32 { packaged_rust_leaf }\n'
 swift_text='// café 🦀 installed offline source\npublic func packagedSwiftLeaf(_ value: Int) -> Int { value + 1 }\npublic func packagedSwiftEntry() -> Int { packagedSwiftLeaf(41) }\npublic func packagedSwiftReference() -> (Int) -> Int { packagedSwiftLeaf }\n'
 (swift_package/'Package.swift').write_text('// swift-tools-version: 6.0\nimport PackageDescription\nlet package = Package(name: "PackagedSwift", targets: [.target(name: "PackagedSwift")])\n')
 rust_file.write_text(rust_text);swift_file.write_text(swift_text)
 ready(node,before+1)
 # More saves can queue while discovery reconciles; wait for both names.
 until=time.monotonic()+90
 while time.monotonic()<until:
  ready(node)
  rust=query({'op':'symbols','name':'packaged_rust_entry'})
  swift=query({'op':'symbols','name':'packagedSwiftEntry()'})
  if rust.get('symbols') and swift.get('symbols'):break
 else:raise AssertionError((rust,swift))
 MIN_GENERATION=ready(node)['generation']
 def language_symbol(name,language,compilation=None):
  request={'op':'symbols','name':name}
  if compilation is None:request.update(coverage_detail='full',detail_max_bytes=262144)
  else:request['compilation_id']=compilation
  found=query(request)
  reports={row['compilation_id']:row for row in found['coverage'] if row['language']==language}
  if language=='rust' and compilation is None:
   reports={key:row for key,row in reports.items() if isinstance(row.get('analysis_options',{}).get('context'),dict)
            and row['analysis_options']['context']['profile']['test'] is False}
  values=[symbol for symbol in found.get('symbols',[]) if not symbol['external'] and symbol['compilation_id'] in reports
          and (compilation is None or symbol['compilation_id']==compilation)]
  assert len(values)==1,(name,values,found)
  return values[0],reports[values[0]['compilation_id']]
 def language_graph(language,entry_name,leaf_name,reference_name,filename):
  entry,report=language_symbol(entry_name,language);compilation=entry['compilation_id']
  assert report['capabilities']['calls']=='partial' and report['capabilities']['dependencies']=='partial',report
  if language=='swift':assert report['analysis_options']['origin']=='swiftpm_build_server',report
  else:assert report['status']=='partial' and report['analysis_options']['rustContextPolicy']==2,report
  leaf,_=language_symbol(leaf_name,language,compilation);reference,_=language_symbol(reference_name,language,compilation)
  callees=query({'op':'callees','symbol_id':entry['id'],'max_depth':1})['graph']
  called=next(finding for finding in callees['findings'] if finding['symbol']['id']==leaf['id'])
  assert called['paths'] and all(edge['kind']=='call' and edge['resolution']=='compiler_bound' for path in called['paths'] for edge in path),called
  callers=query({'op':'callers','symbol_id':leaf['id'],'max_depth':1})['graph']
  assert entry['id'] in {finding['symbol']['id'] for finding in callers['findings']},callers
  assert reference['id'] not in {finding['symbol']['id'] for finding in callers['findings']},callers
  impact=query({'op':'impact','symbol_id':leaf['id']})['graph']
  assert {entry['id'],reference['id']}<={finding['symbol']['id'] for finding in impact['findings']},impact
  occurrences=query({'op':'references','symbol_id':leaf['id']})
  assert any(item['role']=='reference' for item in occurrences['occurrences']),occurrences
  for occurrence in occurrences['occurrences']:
   location=occurrence['location']
   source=query({'op':'source','artifact_id':location['artifact_id'],'snapshot':occurrences['snapshot'],
                 'content_hash':location['content_hash'],'max_bytes':4096})['source']
   exact_source(source,filename)
   assert filename.read_bytes()[location['start_byte']:location['end_byte']].decode()==leaf_name.split('(')[0],occurrence
 language_cases=[('rust','packaged_rust_entry','packaged_rust_leaf','packaged_rust_reference',rust_file,rust_text,
                  '\npub fn broken_package() -> i32 { missing_package_value }\n'),
                 ('swift','packagedSwiftEntry()','packagedSwiftLeaf(_:)','packagedSwiftReference()',swift_file,swift_text,
                  '\npublic func brokenPackage() -> Int { missingPackageValue }\n')]
 for language,entry_name,leaf_name,reference_name,filename,text,broken in language_cases:
  language_graph(language,entry_name,leaf_name,reference_name,filename)
  previous=ready(node)['generation'];filename.write_text(text+broken)
  MIN_GENERATION=ready(node,previous+1)['generation']
  anchor=query({'op':'symbols','limit':1})
  status=query({'op':'coverage','generation':anchor['generation'],'snapshot':anchor['snapshot'],'max_bytes':16384})
  assert status['coverage_scope']=='workspace',status
  reports=[row for row in status['coverage'] if row['language']==language]
  assert reports and any(row['status']=='partial' and row['diagnostic_counts'].get('error',0) for row in reports),status
  previous=ready(node)['generation'];filename.write_text(text)
  MIN_GENERATION=ready(node,previous+1)['generation']
  language_graph(language,entry_name,leaf_name,reference_name,filename)
 # Reuse the same installed MCP framing/helpers for one new-language graph;
 # the earlier TS/C# session ended before the Cargo/SwiftPM fixtures existed.
 rust_entry,rust_report=language_symbol('packaged_rust_entry','rust')
 rust_leaf,_=language_symbol('packaged_rust_leaf','rust',rust_entry['compilation_id'])
 rust_anchor=query({'op':'symbols','name':'packaged_rust_entry','compilation_id':rust_entry['compilation_id']})
 rust_graph_args={'symbol_id':rust_entry['id'],'target':rust_report['target'],'max_depth':1,
                  'snapshot':rust_anchor['snapshot'],'include_source':True,'source_max_bytes':4096}
 mcp=subprocess.Popen(['/usr/bin/sandbox-exec','-f',str(POLICY_FILE),str(HOST),'mcp','--workspace',str(WORKSPACE)],cwd=PREFIX,env=ENV,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=(PREFIX/'mcp-rust-stderr.log').open('w'))
 mcp_selector=selectors.DefaultSelector();mcp_selector.register(mcp.stdout,selectors.EVENT_READ);mcp_buffer=b'';mcp_events=[]
 try:
  assert rpc(100,'initialize',{'protocolVersion':'2025-11-25','capabilities':{},'clientInfo':{'name':'package-test-rust','version':'1'}})['result']['protocolVersion']=='2025-11-25'
  mcp.stdin.write(b'{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}\n');mcp.stdin.flush()
  rust_mcp=tool_result(101,'callees',rust_graph_args)
  assert rust_mcp==query(dict(rust_graph_args,op='callees')),rust_mcp
  assert rust_mcp['snapshot']==rust_anchor['snapshot'] and rust_mcp['generation']==rust_anchor['generation'],rust_mcp
  called=next(finding for finding in rust_mcp['graph']['findings'] if finding['symbol']['id']==rust_leaf['id'])
  assert called['symbol']['compilation_id']==rust_entry['compilation_id'],called
  assert called['paths'] and all(len(path)==1 and path[0]['kind']=='call' and path[0]['resolution']=='compiler_bound'
                                and path[0]['source_id']==rust_entry['id'] and path[0]['target_id']==rust_leaf['id']
                                for path in called['paths']),called
  declarations=called['context']['declarations'];assert declarations and all(d['source_status']=='included' for d in declarations),called
  for declaration in declarations:exact_source(declaration['source'],rust_file)
  checks.append('installed_offline_Cargo_MCP_graph_snapshot_source_matches_native_projection')
 finally:mcp.stdin.close();mcp.wait(timeout=10);mcp_selector.close();(PREFIX/'mcp-rust.json').write_text(json.dumps(mcp_events,indent=2)+'\n')
 checks+=['installed_offline_SwiftPM_Cargo_direct_calls_value_impact_exact_source_native_parity',
          'installed_offline_SwiftPM_Cargo_saved_compiler_partial_repair']
final_generation=ready(node)['generation'];node.close();net.close();checks.append('packaged_saved_file_updates')
command(HOST,'coordinator','stop');until=time.monotonic()+20
while (DATA/'c.sock').exists() and time.monotonic()<until:time.sleep(.05)
new_parent=PREFIX/'second installation';new_parent.mkdir();bundle=bundle.rename(new_parent/bundle.name);HOST.unlink();HOST.symlink_to(bundle/'bin/know-code')
info=json.loads(command(HOST,'runtime-info').stdout);assert str(new_parent) in info['resource_directory']
assert all(info[key]==version for key,version in contracts.items()),info
node=Client('node');ready(node);assert node.send('query',request={'op':'symbols','name':'savedAnswer'})['result']['freshness']=='current';node.close();checks.append('runtime_relocation_and_SDK_reconnect_without_reinstall')
# Exercise the shipped preload through real coordinator preparation AFTER the
# runtime move, with both network and every supplied checkout still denied.
PHASE='relocated_npm10_policy'
resources=bundle/'lib/know-code';relocated_guard=resources/guard_relative
assert not Path(npm10_evidence['primed_guard']).exists()
assert hashlib.sha256(relocated_guard.read_bytes()).hexdigest()==guard_sha256
npm10_evidence.update({'relocated_guard':str(relocated_guard),'policy':str(POLICY_FILE),'checkout_denied':True,'network_denied':True})
linked=PREFIX/'npm10-linked';linked.mkdir();prepare_marker=PREFIX/'npm10-prepare-ran'
prepare_script='node -e '+shlex.quote("require('fs').writeFileSync("+json.dumps(str(prepare_marker))+",'unexpected prepare')")
(linked/'package.json').write_text(json.dumps({'name':'@know-code/npm10-linked','version':'1.0.0','types':'index.d.ts','bin':{'know-code-owned-tool':'cli.cjs'},'scripts':{'prepare':prepare_script}})+'\n')
(linked/'index.d.ts').write_text('export declare const linkedValue: number;\n')
(linked/'cli.cjs').write_text('#!/usr/bin/env node\nconsole.log("owned installed npm10 bin");\n');(linked/'cli.cjs').chmod(0o755)
package=json.loads((WORKSPACE/'package.json').read_text());package['packageManager']='npm@10.0.0';package['dependencies']['@know-code/npm10-linked']='file:../npm10-linked'
(WORKSPACE/'package.json').write_text(json.dumps(package)+'\n')
# This positive case uses unlocked restoration. Initial npm installation's lock
# remains covered above; remove it only in this owned workspace for this case.
(WORKSPACE/'package-lock.json').unlink(missing_ok=True)
(WORKSPACE/'.npmrc').write_text('install-links=false\nignore-scripts=false\n')
owned_configs=[WORKSPACE/'.npmrc',PREFIX/'npm-userconfig',PREFIX/'npm-globalconfig']
config_hashes={str(p):hashlib.sha256(p.read_bytes()).hexdigest() for p in owned_configs}
def npm10_step(label):
 global MIN_GENERATION,final_generation
 step={'label':label,'requested_package':json.loads((WORKSPACE/'package.json').read_text()),'generation_before':final_generation,'started_ns':time.time_ns()}
 npm10_evidence['steps'].append(step)
 # Retain the exact requested inputs even when the following gate fails.
 (PREFIX/'npm10-installed.json').write_text(json.dumps(npm10_evidence,indent=2)+'\n')
 refresh=json.loads(command(HOST,'index',WORKSPACE,'--configuration','Release').stdout)
 assert refresh['database']==DATABASE and refresh['generation']>step['generation_before'],refresh
 MIN_GENERATION=final_generation=refresh['generation'];step['refresh']=refresh
 request={'op':'symbols','name':'SavedGreet','coverage_detail':'full','detail_max_bytes':65536}
 envelope=json.loads(command(HOST,'query',DATABASE,json.dumps(request)).stdout);result=checked_result(envelope);assert result['symbols'],result
 step.update({'request':request,'result':envelope,'prepare_marker_exists':prepare_marker.exists(),'finished_ns':time.time_ns()})
 (PREFIX/'npm10-installed.json').write_text(json.dumps(npm10_evidence,indent=2)+'\n')
 assert not prepare_marker.exists(),'installed npm10 linked dependency executed prepare'
 assert not (WORKSPACE/'package-lock.json').exists(),'unlocked preparation wrote a source lockfile'
 assert {str(p):hashlib.sha256(p.read_bytes()).hexdigest() for p in owned_configs}==config_hashes,'preparation rewrote selected config'
 assert cache_inventory(DATA/'package-tools')==npm10_cache_inventory,'preparation modified the primed tool distribution'
 # This policy probe mutates only the root TS5.9 npm10 project, not the
 # independently pinned TS6/TS7 child projects restored before denial.
 typescript=[c for c in result['coverage'] if c['language']=='typescript' and c['project_path']=='tsconfig.json']
 assert len(typescript)==1,typescript
 assert all(not c['details_omitted'] for c in typescript),typescript
 for compilation in typescript:
  manager=compilation['analysis_options']['knowCodePackageManager']
  assert manager['name']=='npm' and manager['version']=='10.0.0' and manager['source']=='user-cache' and manager['scriptsAuthorization']=='disabled',manager
  assert manager['restorationSafety']=={'gitFetching':'disabled','installLinks':False,'installLinksOrigin':'npm-effective-query-pinned'},manager
 assert all(c['status']=='complete' for c in result['coverage'] if c['language']=='csharp'),result
 return typescript
linked_coverage=npm10_step('linked_directory_prepare_suppressed_with_bin_preserved')
assert all(c['status']=='complete' for c in linked_coverage),linked_coverage
installed_link=WORKSPACE/'node_modules/@know-code/npm10-linked';installed_bin=WORKSPACE/'node_modules/.bin/know-code-owned-tool'
assert installed_link.is_symlink() and installed_link.resolve()==linked.resolve(),installed_link
assert installed_bin.is_symlink() and installed_bin.resolve()==(linked/'cli.cjs').resolve(),installed_bin
assert command('node',installed_bin).stdout.strip()=='owned installed npm10 bin'
npm10_evidence['linked_outputs']={'dependency':str(installed_link),'dependency_target':str(installed_link.resolve()),'bin':str(installed_bin),'bin_target':str(installed_bin.resolve())}
checks.append('relocated_installed_npm10_linked_prepare_suppressed_bin_and_effective_policy_preserved')
# A hosted full SHA can bypass the git executable through archive fetching;
# only the shipped process-local Pacote guard yields this exact policy error.
package['dependencies']['know-code-blocked-git']='github:know-code-fixture/never-fetch#'+'1'*40
(WORKSPACE/'package.json').write_text(json.dumps(package)+'\n')
git_coverage=npm10_step('hosted_full_SHA_Git_refused_by_shipped_guard')
assert any(c['status']=='partial' and any(d['code']=='dependency_restore_failed' and 'EKNOWCODE_NPM10_GIT_DISABLED' in d['message'] for d in c.get('diagnostics',[])) for c in git_coverage),git_coverage
saved_ts=json.loads(command(HOST,'query',DATABASE,json.dumps({'op':'symbols','name':'savedAnswer'})).stdout);assert checked_result(saved_ts)['symbols'],saved_ts
npm10_evidence.update({'healthy_TS_result_after_Git_refusal':saved_ts,'config_sha256':config_hashes,'final_generation':final_generation,'passed':True})
(PREFIX/'npm10-installed.json').write_text(json.dumps(npm10_evidence,indent=2)+'\n')
checks.append('relocated_installed_npm10_full_SHA_Git_policy_refusal_with_fresh_partial_TS_and_healthy_Csharp')
PHASE='network_denied_runtime'
missing=PREFIX/'missing';missing.mkdir();missing_package='KnowCodeOfflineMissingDependency'
(missing/'Missing.csproj').write_text('<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><TargetFramework>net10.0</TargetFramework></PropertyGroup><ItemGroup><PackageReference Include="'+missing_package+'" Version="1.0.0" /></ItemGroup></Project>')
(missing/'Missing.cs').write_text('public class MissingConsumer { MissingDependency.Type value; }\n')
assert not (missing/'obj/project.assets.json').exists()
assert missing.resolve().is_relative_to(PREFIX)
command(HOST,'trust',missing)
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
final_generation=healthy['generation']
checks.append('offline_missing_restoration_is_actionable_and_other_workspace_stays_healthy')
command(HOST,'coordinator','stop');until=time.monotonic()+20
while (DATA/'c.sock').exists() and time.monotonic()<until:time.sleep(.05)
resources=bundle/'lib/know-code';worker=resources/'providers/typescript/worker.mjs';backup=worker.with_suffix('.held');worker.rename(backup)
try:failure=command(HOST,'runtime-info',check=False);assert failure.returncode and 'missing' in failure.stderr
finally:backup.rename(worker)
manifest_file=resources/'runtime.json';saved=manifest_file.read_bytes()
for language in ['rust','swift']:
 # Keep provider bytes intact: omission from the manifest must itself fail.
 original=json.loads(saved);entry=next(p for p in original['providers'] if p['language']==language)
 assert (resources/entry['entrypoint']).is_file(),entry
 broken=dict(original,providers=[p for p in original['providers'] if p['language']!=language])
 manifest_file.write_text(json.dumps(broken))
 try:
  failure=command(HOST,'runtime-info',check=False)
  assert failure.returncode and f'runtime lacks required {language} provider' in failure.stderr,failure.stderr
 finally:manifest_file.write_bytes(saved)
checks.append('required_Rust_Swift_manifest_entries_cannot_be_omitted')
broken=json.loads(saved);broken['worker_protocol']=999;manifest_file.write_text(json.dumps(broken))
try:failure=command(HOST,'runtime-info',check=False);assert failure.returncode and 'incompatible' in failure.stderr
finally:manifest_file.write_bytes(saved)
invalid=PREFIX/'invalid-resources';invalid.mkdir();failure=command(HOST,'runtime-info',check=False,env=dict(ENV,KNOW_CODE_RUNTIME_DIR=str(invalid)));assert failure.returncode and 'manifest' in failure.stderr
bare=PREFIX/'bare';bare.mkdir();shutil.copy2(bundle/'bin/know-code',bare/'know-code');failure=command(bare/'know-code','runtime-info',check=False);assert failure.returncode and 'not found' in failure.stderr
checks+=['missing_resource_no_checkout_fallback','manifest_protocol_mismatch','invalid_override_no_silent_fallback','bare_executable_actionable_error']
result={'passed':True,'signing_verification':SIGNING,'prefix':str(PREFIX),'packages':str(PACKAGES),'matrix':manifest['runtime'],'checks':checks,'final_generation':final_generation,'checkout_denied':True,'denied_checkouts':[str(root) for root in CHECKOUTS],'network_denied_after_restoration':True,'archive_sha256':{a['file']:a['sha256'] for a in manifest['artifacts']},'candidate_source_revision':manifest.get('source_revision'),'candidate_manifest_sha256':hashlib.sha256((PREFIX/'candidate-manifest.json').read_bytes()).hexdigest(),'phase_evidence':str(PREFIX/'phases.json'),'network_policy_proof':str(PREFIX/'network-policy-proof.json'),'installed_npm10_evidence':str(PREFIX/'npm10-installed.json')}
(PREFIX/'result.json').write_text(json.dumps(result,indent=2)+'\n');report=ROOT/'.artifacts/packaging/installed-test.json';report.parent.mkdir(parents=True,exist_ok=True);report.write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result,indent=2))
