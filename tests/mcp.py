"""Real stdio sessions, shared native operators and persistent saved-file indexing."""
from pathlib import Path
import json,os,selectors,signal,struct,subprocess,time
ROOT=Path(__file__).resolve().parents[1]
RUN=ROOT/'.artifacts/mc'/format(time.time_ns(),'x');WORKSPACE=RUN/'workspace';DATA=RUN/'d';WORKSPACE.mkdir(parents=True)
PROFILE=os.environ.get('KNOW_CODE_BUILD_PROFILE','debug');HOST=ROOT/f'target/{PROFILE}/know-code';CONFIG='Debug' if PROFILE=='debug' else 'Release'
ENV=dict(os.environ,KNOW_CODE_DATA_DIR=str(DATA),DOTNET_CLI_TELEMETRY_OPTOUT='1')
SOURCE=WORKSPACE/'value.ts';SOURCE.write_text('// '+('a'*20000)+'\nexport function target(value: string) { return value; }\nexport function caller() { return target("hello😀"); }\n')
(WORKSPACE/'App.csproj').write_text('<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><TargetFramework>net10.0</TargetFramework></PropertyGroup></Project>')
(WORKSPACE/'Greeter.cs').write_text('public static class Greeter { public static string Greet() => "hello"; }\n')
(WORKSPACE/'Greeter.cs').write_text('public static partial class Greeter { public static string Greet() => "hello😀"; }\n')
(WORKSPACE/'Part.cs').write_text('public static partial class Greeter { public static int Number() => 1; }\n')
(WORKSPACE/'over.ts').write_text('export function choose(x: string): string;\nexport function choose(x: number): number;\nexport function choose(x: string | number) { return x; }\n')
checks=[];clients=[];paused=None

def cli(*args,check=True):
 p=subprocess.run([str(HOST),*map(str,args)],cwd=ROOT,env=ENV,capture_output=True,text=True,timeout=120)
 assert not check or p.returncode==0,(args,p.stdout,p.stderr)
 return p

def status():return json.loads(cli('coordinator','status').stdout)['workspaces'][0]
def ready(minimum=1):
 until=time.monotonic()+45;last=None
 while time.monotonic()<until:
  last=status()
  if last['phase']=='idle' and not last['error'] and last['published_epoch']==last['observed_epoch'] and last['generation']>=minimum:return last
  time.sleep(.05)
 raise AssertionError(('not ready',last))

class Client:
 def __init__(self,modern=False,configuration=None):
  self.modern=modern;self.sequence=0;self.buffer=b'';self.pending={};self.events=[]
  args=[str(HOST),'mcp','--workspace',str(WORKSPACE)]
  if configuration:args+=['--configuration',configuration]
  self.process=subprocess.Popen(args,cwd=ROOT,env=ENV,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=(RUN/'stderr.log').open('ab'))
  self.selector=selectors.DefaultSelector();self.selector.register(self.process.stdout,selectors.EVENT_READ)
  clients.append(self)
  if modern:
   found=self.request('server/discover',{})['result'];assert '2026-07-28' in found['supportedVersions'],found
  else:
   found=self.request('initialize',{'protocolVersion':'2025-11-25','capabilities':{},'clientInfo':{'name':'know-code-regression','version':'1'}})['result']
   assert found['protocolVersion']=='2025-11-25',found
   self.notify('notifications/initialized',{})
 def params(self,method,value):
  value=dict(value)
  if self.modern:
   value['_meta']={'io.modelcontextprotocol/protocolVersion':'2026-07-28','io.modelcontextprotocol/clientCapabilities':{},'io.modelcontextprotocol/clientInfo':{'name':'know-code-regression','version':'1'}}
  return value
 def send(self,method,params):
  self.sequence+=1;self.write({'jsonrpc':'2.0','id':self.sequence,'method':method,'params':self.params(method,params)});return self.sequence
 def write(self,value):self.process.stdin.write((json.dumps(value)+'\n').encode());self.process.stdin.flush()
 def notify(self,method,params):self.write({'jsonrpc':'2.0','method':method,'params':params})
 def response(self,identifier,seconds=15):
  until=time.monotonic()+seconds
  while identifier not in self.pending:
   assert time.monotonic()<until,('MCP timeout',identifier,self.process.poll(),(RUN/'stderr.log').read_text())
   if not self.selector.select(min(.1,max(0,until-time.monotonic()))):continue
   block=os.read(self.process.stdout.fileno(),65536);assert block,('MCP EOF',self.process.poll(),(RUN/'stderr.log').read_text())
   self.buffer+=block
   while b'\n' in self.buffer:
    line,self.buffer=self.buffer.split(b'\n',1);value=json.loads(line);assert value['jsonrpc']=='2.0',value;self.events.append(value)
    if 'id' in value:self.pending[value['id']]=value
  return self.pending.pop(identifier)
 def request(self,method,params):return self.response(self.send(method,params))
 def tool(self,name,args=None):
  reply=self.request('tools/call',{'name':name,'arguments':args or {}});assert 'error' not in reply,reply
  tool=reply['result'];value=tool['structuredContent'];assert json.loads(tool['content'][0]['text'])==value
  return tool,value
 def close(self):
  if self.process.poll() is None:
   self.process.stdin.close();self.process.wait(timeout=6)
  self.selector.close()
  (RUN/f'wire-{self.process.pid}.json').write_text(json.dumps(self.events,indent=2)+'\n')

def equivalent(request):
 baseline=json.loads(cli('query',DATABASE,json.dumps(request)).stdout)
 js='import {Index} from '+json.dumps((ROOT/f'.artifacts/build/{PROFILE}/node/index.mjs').as_uri())+';console.log(JSON.stringify(await new Index(process.argv[1]).query(JSON.parse(process.argv[2]))));'
 node=json.loads(subprocess.check_output(['node','--input-type=module','-e',js,DATABASE,json.dumps(request)],cwd=ROOT,env=ENV,text=True))
 dotnet=json.loads(subprocess.check_output(['dotnet',str(ROOT/f'bindings/dotnet/Smoke/bin/{CONFIG}/net10.0/Smoke.dll'),DATABASE,json.dumps(request)],cwd=ROOT,env=ENV,text=True))
 assert node==dotnet==baseline
 return baseline

try:
 missing=cli('mcp',check=False);assert missing.returncode and not missing.stdout and '--workspace' in missing.stderr
 early=subprocess.Popen([str(HOST),'mcp','--workspace',str(WORKSPACE),'--configuration','Release'],cwd=ROOT,env=ENV,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=(RUN/'early-stderr.log').open('ab'))
 early.stdin.close();early.wait(timeout=8);assert early.returncode==0 and early.stdout.read()==b''
 checks.append('explicit_selection_and_EOF_before_initialization')
 c=Client(configuration='Release');initial=ready();DATABASE=initial['database'];generation=initial['generation']
 assert len(json.loads(cli('coordinator','status').stdout)['workspaces'])==1
 c.write({'jsonrpc':'2.0','id':'oversized','method':'tools/call','params':{'name':'search_symbols','arguments':{'name':'x'*70000}}})
 listing=c.request('tools/list',{})['result']['tools'];names=[t['name'] for t in listing]
 assert not any(e.get('id')=='oversized' for e in c.events)
 checks.append('oversized_input_discarded_and_connection_usable')
 assert names==sorted(names) and len(names)==10 and len(set(names))==10
 assert all(t['annotations']['readOnlyHint'] is True and t['inputSchema']['additionalProperties'] is False for t in listing)
 checks+=['explicit_workspace_auto_registration','legacy_2025_lifecycle','stable_read_only_tools']
 tool,value=c.tool('workspace_status');assert not tool['isError'] and value['result']['workspaces'][0]['workspace_root']==str(WORKSPACE)
 tool,value=c.tool('search_symbols',{'name':'target'});assert not tool['isError'] and value==equivalent({'op':'symbols','name':'target'})
 target=value['result']['symbols'][0];location=target['locations'][0]
 tool,detail=c.tool('coverage_details',{'generation':generation})
 assert not tool['isError'] and detail==equivalent({'op':'coverage','generation':generation})
 assert any(r['language']=='csharp' and r['configuration']=='Release' for r in detail['result']['coverage'])
 caller=c.tool('search_symbols',{'name':'caller'})[1]['result']['symbols'][0]
 for name,args,op in [('find_references',{'symbol_id':target['id']},'references'),('callees',{'symbol_id':caller['id']},'callees'),('callers',{'symbol_id':target['id']},'callers'),('impact',{'symbol_id':target['id'],'max_depth':2},'impact')]:
  tool,value=c.tool(name,args);assert not tool['isError'] and value==equivalent({'op':op,**args}),(name,value)
 tool,value=c.tool('query_graph',{'op':'callees','symbol_id':caller['id']});assert not tool['isError'] and value==c.tool('callees',{'symbol_id':caller['id']})[1]
 checks+=['CLI_Node_dotnet_full_query_parity','focused_and_structured_graph_parity','Csharp_configuration_preserved']
 tool,value=c.tool('read_source',{'artifact_id':location['artifact_id']});assert value['result']['truncated'] and len(value['result']['source']['text'].encode())==16384
 source_args={k:location[k] for k in ['artifact_id','content_hash','start_byte','end_byte']}
 tool,value=c.tool('read_source',source_args);assert not tool['isError'] and value==equivalent({'op':'source',**source_args});assert value['result']['source']['text']=='target'
 for name,args in [('read_source',{'artifact_id':location['artifact_id'],'start_byte':1}),('query_graph',{'op':'sql','symbol_id':target['id']}),('search_symbols',{'sql':'DROP TABLE symbols'}),('find_references',{})]:
  reply=c.request('tools/call',{'name':name,'arguments':args});assert 'error' in reply or reply['result']['isError'],reply
 context_args={'symbol_id':target['id'],'max_bytes':1024}
 tool,value=c.tool('symbol_context',context_args);assert not tool['isError'] and value==equivalent({'op':'symbol_context',**context_args})
 context=value['result']['contexts'][0];decl=context['declarations'][0]
 assert decl['line']==2 and decl['source']['start_line']==2
 assert decl['source']['text']=='export function target(value: string) { return value; }'
 assert decl['location']['start_byte']>decl['extent']['start_byte'] and context['symbol']['signature']
 assert value['result']['coverage_scope']=='relevant' and len(value['result']['coverage'])==1
 assert all('analysis_options' not in cov and 'diagnostics' not in cov and cov['details_omitted'] for cov in value['result']['coverage'])
 assert value['result']['workspace_coverage']['compilations']>=2
 for symbol_name,expected in [('choose',3),('Greeter',2),('Greet',1)]:
  symbol=c.tool('search_symbols',{'name':symbol_name})[1]['result']['symbols'][0]
  args={'symbol_id':symbol['id']}
  tool,value=c.tool('symbol_context',args);assert not tool['isError'] and value==equivalent({'op':'symbol_context',**args})
  declarations=value['result']['contexts'][0]['declarations'];assert len(declarations)==expected,(symbol_name,value)
  assert all(d['extent'] and d['source_status']=='included' for d in declarations)
  if symbol_name=='choose':
   tool,small=c.tool('symbol_context',dict(args,max_bytes=10));assert small['result']['truncated']
   assert sum(len(d.get('source',{}).get('text','').encode()) for d in small['result']['contexts'][0]['declarations'])<=10
   tool,limited=c.tool('symbol_context',dict(args,limit=1,include_source=False));assert limited['result']['truncated']
   assert len(limited['result']['contexts'][0]['declarations'])==1
 tool,clipped=c.tool('coverage_details',{'generation':generation,'max_bytes':1});assert clipped['result']['truncated']
 assert clipped['result']['workspace_coverage']['compilations']>=2 and all(cov['details_omitted'] for cov in clipped['result']['coverage'])
 tool,missing=c.tool('coverage_details');assert tool['isError']
 checks+=['compiler_context_and_source_parity','overloads_partial_types_and_unicode','aggregate_context_budgets','compact_relevant_coverage_with_workspace_summary','generation_bound_bounded_coverage_details']
 checks+=['bounded_source_default','hash_guarded_byte_ranges','no_SQL_surface','visible_argument_errors']
 owner=(DATA/'live/owner').read_bytes();pid=json.loads(owner[76:76+int.from_bytes(owner[72:76],'little')])['pid']
 os.kill(pid,signal.SIGSTOP);paused=pid
 first=c.send('tools/call',{'name':'search_symbols','arguments':{'name':'target','freshness_wait_ms':2000,'deadline_ms':2000}})
 second=c.send('tools/call',{'name':'search_symbols','arguments':{'name':'target','freshness_wait_ms':2000,'deadline_ms':2000}})
 time.sleep(.15);c.notify('notifications/cancelled',{'requestId':first,'reason':'test cancellation'})
 third=c.send('tools/call',{'name':'search_symbols','arguments':{'name':'target'}})
 busy=c.response(third);assert busy['result']['isError'] and busy['result']['structuredContent']['error']['code']=='busy',busy
 os.kill(pid,signal.SIGCONT);paused=None;c.response(second)
 until=time.monotonic()+5
 while True:
  tool,_=c.tool('search_symbols',{'name':'target'})
  if not tool['isError']:break
  assert time.monotonic()<until;time.sleep(.05)
 checks.append('cancelled_waiter_retains_bounded_work_slot')
 SOURCE.write_text(SOURCE.read_text().replace('hello😀','saved😀'));ready(generation+1)
 tool,value=c.tool('read_source',source_args);assert tool['isError'] and value['error']['code']=='invalid_request'
 tool,old_detail=c.tool('coverage_details',{'generation':generation});assert tool['isError'] and 'restart lookup' in old_detail['error']['message']
 tool,old_context=c.tool('symbol_context',dict(context_args,generation=generation));assert tool['isError'] and 'restart lookup' in old_context['error']['message']
 checks.append('changed_source_rejects_old_range_and_detail_generation')
 generation=ready()['generation'];c.close()
 SOURCE.write_text(SOURCE.read_text()+'\n// saved after MCP disconnect\n');after=ready(generation+1);generation=after['generation']
 checks.append('EOF_leaves_coordinator_indexing')
 modern=Client(modern=True);assert ready()['generation']==generation
 listing=modern.request('tools/list',{})['result'];assert listing['resultType']=='complete' and 'ttlMs' in listing and 'cacheScope' in listing
 assert [t['name'] for t in listing['tools']]==names
 tool,value=modern.tool('search_symbols',{'name':'Greeter'});assert not tool['isError'] and value['result']['freshness']=='current'
 modern.close();checks+=['current_2026_discovery_lifecycle','reconnect_reuses_index_without_forced_refresh']
 result={'passed':True,'profile':PROFILE,'database':DATABASE,'checks':checks}
 (RUN/'summary.json').write_text(json.dumps(result,indent=2)+'\n');(ROOT/'.artifacts/mcp/latest.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result,indent=2))
finally:
 if paused:os.kill(paused,signal.SIGCONT)
 for client in clients:client.close()
 cli('coordinator','stop',check=False)
