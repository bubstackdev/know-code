"""Actual host queues/SQLite cancellation, reader release and saved-index recovery."""
from pathlib import Path
import ctypes,hashlib,json,os,selectors,sqlite3,subprocess,time
from index_paths import physical_database

ROOT=Path(__file__).resolve().parents[1]
RUN=ROOT/'.artifacts/qc'/format(time.time_ns(),'x')[-10:];WORKSPACE=RUN/'w';DATA=RUN/'d'
WORKSPACE.mkdir(parents=True)
PROFILE=os.environ.get('KNOW_CODE_BUILD_PROFILE','debug');CONFIG=PROFILE.capitalize()
HOST=ROOT/f'target/{PROFILE}/know-code';ADDON=ROOT/f'.artifacts/build/{PROFILE}/node/index.mjs'
SMOKE=ROOT/f'bindings/dotnet/Smoke/bin/{CONFIG}/net10.0/Smoke.dll';FFI=ROOT/f'target/{PROFILE}/libknow_code_ffi.dylib'
ENV=dict(os.environ,KNOW_CODE_DATA_DIR=str(DATA),UV_THREADPOOL_SIZE='1',DOTNET_CLI_TELEMETRY_OPTOUT='1',
         DOTNET_NOLOGO='1',MSBUILDDISABLENODEREUSE='1',DOTNET_CLI_USE_MSBUILD_SERVER='0',UseSharedCompilation='false')
ENV['KNOW_CODE_QUERY_SCOPE']=str(RUN/'query-admission')
SOURCE=WORKSPACE/'value.ts';OLD='// π😀 owned fixture\nexport function cancellationTarget() { return "before"; }\n'
SOURCE.write_text(OLD);CHECKS=[];passed=False;database=None
digest=lambda path:hashlib.sha256(Path(path).read_bytes()).hexdigest()

def run(args,timeout=30,check=True):
 started=time.monotonic()
 try:
  p=subprocess.run(list(map(str,args)),cwd=ROOT,env=ENV,text=True,capture_output=True,timeout=timeout)
 except subprocess.TimeoutExpired as error:
  (RUN/'timeout.json').write_text(json.dumps({'args':list(map(str,args)),'timeout':timeout,
    'stdout':(error.stdout or b'').decode(errors='replace'),'stderr':(error.stderr or b'').decode(errors='replace')},indent=2)+'\n')
  raise
 with (RUN/'commands.jsonl').open('a') as log:
  log.write(json.dumps({'args':list(map(str,args)),'exit':p.returncode,'stdout':p.stdout,'stderr':p.stderr,
                       'elapsed_ms':(time.monotonic()-started)*1000})+'\n')
 assert not check or p.returncode==0,(args,p.stdout,p.stderr)
 return p
def cli(*args,**kwargs):return run([HOST,*args],**kwargs)
def value(*args):return json.loads(cli(*args).stdout)
def node(mode,db,request=None,delay=50):
 result=json.loads(run(['node',ROOT/'tests/query_cancellation.mjs',ADDON,db,mode,json.dumps(request),delay]).stdout)
 assert result['passed'] and result['contract']==7,result
 return result
def query(request):return value('query',database,json.dumps(request))
def ready(minimum=1):
 until=time.monotonic()+60;last=None
 while time.monotonic()<until:
  last=value('coordinator','status')['workspaces'][0]
  assert last['phase']!='failed',last
  if last['phase']=='idle' and last['generation']>=minimum and last['observed_epoch']==last['published_epoch']:return last
  time.sleep(.05)
 raise AssertionError(('index not ready',last))
def verify(response,text,generation):
 result=response['result'];assert result['freshness']=='current' and result['generation']==generation,result
 whole=result['workspace_coverage']
 assert whole['compilations']>0 and whole['statuses'].get('complete',0)==whole['compilations'] and result['coverage'],result
 assert all(c['language']=='typescript' and c['status']=='complete' for c in result['coverage']),result
 assert len(result['symbols'])==1 and result['symbols'][0]['name']=='cancellationTarget',result
 assert result['symbols'][0]['locations'],result
 for location in result['symbols'][0]['locations']:
  assert location['content_hash']==hashlib.sha256(text.encode()).hexdigest(),location
  assert text.encode()[location['start_byte']:location['end_byte']]==b'cancellationTarget',location
 return result

def mcp_eof(connection,generation):
 """Prove the query has a live SQLite snapshot before closing the client."""
 events=[];buffer=b''
 # Same-value UPDATEs may not write a WAL frame. Use changed bytes in a
 # disposable table so an empty WAL cannot falsely imply no active reader.
 connection.execute('CREATE TABLE fixture_wal_probe(value INTEGER NOT NULL)')
 connection.execute('INSERT INTO fixture_wal_probe VALUES(0)');connection.commit()
 journal=connection.execute('PRAGMA journal_mode').fetchone()[0];assert journal=='wal',journal
 assert connection.execute('PRAGMA wal_checkpoint(TRUNCATE)').fetchone()[0]==0
 events.append({'workspace':str(WORKSPACE),'database':database,'journal_mode':journal})
 with (RUN/'mcp-stderr.log').open('wb') as stderr:
  p=subprocess.Popen([str(HOST),'mcp','--workspace',str(WORKSPACE)],cwd=ROOT,env=ENV,
                     stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=stderr)
  selector=selectors.DefaultSelector();selector.register(p.stdout,selectors.EVENT_READ)
  def send(message):
   events.append({'sent':message});p.stdin.write((json.dumps(message)+'\n').encode());p.stdin.flush()
  def response(identifier):
   nonlocal buffer
   until=time.monotonic()+10
   while time.monotonic()<until:
    if b'\n' in buffer:
     line,buffer=buffer.split(b'\n',1);message=json.loads(line);events.append({'received':message})
     if message.get('id')==identifier:return message
    elif selector.select(.05):
     block=os.read(p.stdout.fileno(),65536);assert block,('MCP EOF',p.poll());buffer+=block
   raise AssertionError('MCP initialization timed out')
  try:
   send({'jsonrpc':'2.0','id':1,'method':'initialize','params':{'protocolVersion':'2025-11-25','capabilities':{},'clientInfo':{'name':'cancel-fixture','version':'1'}}})
   assert response(1)['result']['protocolVersion']=='2025-11-25'
   send({'jsonrpc':'2.0','method':'notifications/initialized','params':{}})
   send({'jsonrpc':'2.0','id':2,'method':'tools/call','params':{'name':'search_symbols','arguments':{'name':'cancellationTarget','deadline_ms':120000}}})
   send({'jsonrpc':'2.0','id':3,'method':'ping','params':{}})
   until=time.monotonic()+2;busy=None
   while time.monotonic()<until:
    # Let the query enter SQLite without our checkpoint writer monopolizing
    # admission. Still require a proven reader before testing EOF cancellation.
    time.sleep(.1)
    if selector.select(0):
     block=os.read(p.stdout.fileno(),65536);assert block,('MCP EOF before active reader',p.poll());buffer+=block
     while b'\n' in buffer:
      line,buffer=buffer.split(b'\n',1);message=json.loads(line);events.append({'received':message})
      assert message.get('id')!=2,('MCP query completed before active-reader gate',message)
    connection.execute('UPDATE fixture_wal_probe SET value=value+1');connection.commit()
    wal=Path(str(physical_database(database))+'-wal');wal_bytes=wal.stat().st_size if wal.exists() else 0
    busy=connection.execute('PRAGMA wal_checkpoint(TRUNCATE)').fetchone()
    events.append({'probe_value':connection.execute('SELECT value FROM fixture_wal_probe').fetchone()[0],
                   'process_poll':p.poll(),'wal_bytes':wal_bytes,'checkpoint':busy})
    if busy[0]==1:break
    time.sleep(.01)
   assert busy and busy[0]==1,('MCP query never held its SQLite reader',busy)
   bound=next(s for s in value('coordinator','status')['workspaces'] if s['workspace_root']==str(WORKSPACE))
   assert bound['database']==database and bound['generation']==generation,bound
   events.append({'bound_workspace_status':bound})
   started=time.monotonic();p.stdin.close();p.wait(timeout=3)
   assert p.returncode==0,p.returncode
   remainder=buffer+p.stdout.read();buffer=b'';events.append({'remaining_stdout':remainder.decode(errors='replace')})
   after=connection.execute('PRAGMA wal_checkpoint(TRUNCATE)').fetchone();assert after[0]==0,after
   assert connection.execute('SELECT current_generation FROM state').fetchone()[0]==generation
   return {'workspace':str(WORKSPACE),'database':database,'journal_mode':journal,'wal_bytes_before':wal_bytes,
           'reader_before':busy,'reader_after':after,'exit_ms':(time.monotonic()-started)*1000}
  finally:
   if p.poll() is None:p.kill();p.wait(timeout=3)
   if not p.stdin.closed:p.stdin.close()
   remaining=buffer+p.stdout.read()
   if remaining:events.append({'cleanup_stdout':remaining.decode(errors='replace')})
   p.stdout.close();selector.close()
   connection.execute('DROP TABLE IF EXISTS fixture_wal_probe');connection.commit()
   (RUN/'mcp-wire.json').write_text(json.dumps(events,indent=2)+'\n')

artifacts=[HOST,ADDON,ADDON.parent/'know_code.node',SMOKE,SMOKE.parent/'KnowCode.dll',FFI]
artifacts.extend(SMOKE.parent.glob('*.dylib'))
record_path=ROOT/f'.artifacts/build/{PROFILE}/build-record.json'
identity={};mcp=None
try:
 record=json.loads(record_path.read_text());assert record['status']=='complete' and record['profile']==PROFILE
 hashes={str(p.relative_to(ROOT)):digest(p) for p in artifacts}
 assert all(record['outputs'][p]==sha for p,sha in hashes.items()),'artifacts differ from build record'
 identity={'source_revision':record['source_revision'],'build_id':record['build_id'],'record_sha256':digest(record_path),
           'artifacts_sha256':hashes,'test_source_sha256':{p:digest(ROOT/p) for p in ['tests/query_cancellation.py','tests/query_cancellation.mjs']}}
 ffi=ctypes.CDLL(str(FFI));ffi.kc_query_contract_version.argtypes=[];ffi.kc_query_contract_version.restype=ctypes.c_uint32
 assert ffi.kc_query_contract_version()==7,'matching FFI cancellation contract required'
 node('preabort',RUN/'preabort.db');CHECKS.append('Node_preabort_before_serialization_and_index_open')
 node('queue',RUN/'node-queue.db');CHECKS.append('Node_actual_libuv_queue_cancel_waits_for_native_exit_and_recovers')
 queued=json.loads(run(['dotnet',SMOKE,'--cancel-queue',RUN/'dotnet-queue.db']).stdout)
 assert queued['passed'] and not (RUN/'dotnet-queue.db').exists(),queued
 CHECKS.append('DotNet_actual_host_queue_cancel_waits_for_native_exit_and_recovers')
 value('watch',WORKSPACE);status=ready();database=status['database'];generation=status['generation']
 request={'op':'symbols','name':'cancellationTarget','deadline_ms':5000}
 baseline=verify(query(request),OLD,generation)
 with sqlite3.connect(physical_database(database),timeout=.01) as connection:
  connection.execute('PRAGMA busy_timeout=10')
  assert connection.execute('SELECT count(*) FROM reports').fetchone()[0]>0
  connection.execute('ALTER TABLE reports RENAME TO fixture_reports')
  try:
   connection.execute('CREATE VIEW reports AS WITH RECURSIVE n(i) AS (VALUES(0) UNION ALL SELECT i+1 FROM n WHERE i<100000000) SELECT r.* FROM fixture_reports r CROSS JOIN n');connection.commit()
   for iteration in range(2):
    node('query',database,request)
    cancelled=json.loads(run(['dotnet',SMOKE,'--cancel-query',database,json.dumps(request),50]).stdout)
    assert cancelled=={'cancelled':True,'name':'OperationCanceledException'},cancelled
    checkpoint=connection.execute('PRAGMA wal_checkpoint(TRUNCATE)').fetchone();assert checkpoint[0]==0,checkpoint
    assert connection.execute('SELECT current_generation FROM state').fetchone()[0]==generation
   CHECKS.append('Node_AbortError_and_DotNet_OCE_interrupt_actual_SQL_twice_release_readers_preserve_generation')
   mcp=mcp_eof(connection,generation);CHECKS.append('MCP_EOF_cancels_proven_active_SQL_reader_before_five_second_drain')
  finally:
   connection.execute('DROP VIEW IF EXISTS reports');connection.execute('ALTER TABLE fixture_reports RENAME TO reports');connection.commit()
   connection.execute('DROP TABLE IF EXISTS fixture_wal_probe');connection.commit()
  assert connection.execute('PRAGMA wal_checkpoint(TRUNCATE)').fetchone()[0]==0
 verify(query(request),OLD,generation)
 source=query({'op':'source','artifact_id':baseline['symbols'][0]['locations'][0]['artifact_id']})['result']['source']
 assert source['text']==OLD and source['content_hash']==hashlib.sha256(OLD.encode()).hexdigest(),source
 recovered=node('recover',database,request)
 for response in recovered['responses']:verify(response,OLD,generation)
 verify(json.loads(run(['dotnet',SMOKE,database,json.dumps(request)]).stdout),OLD,generation)
 CHECKS.append('same_Node_client_repeated_late_cancel_and_uncancelled_queries_preserve_source_coverage')
 NEW=OLD.replace('"before"','"after"');SOURCE.write_text(NEW);updated=ready(generation+1)
 verify(query(request),NEW,updated['generation'])
 for response in node('recover',database,request)['responses']:verify(response,NEW,updated['generation'])
 CHECKS.append('saved_background_indexing_and_native_queries_continue_after_cancellation')
 assert all(digest(ROOT/p)==sha for p,sha in identity['artifacts_sha256'].items()) and digest(record_path)==identity['record_sha256']
 passed=True
finally:
 cli('coordinator','stop',check=False)
 summary={'passed':passed,'profile':PROFILE,'run':str(RUN),'database':database,'checks':CHECKS,'build':identity,'mcp':mcp,
          'limits':['isolated TS fixture and deliberate bounded SQL view; no corpus/full-suite/package/performance certification']}
 (RUN/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary,indent=2))
