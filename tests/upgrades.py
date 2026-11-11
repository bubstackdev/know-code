"""Owned historical-store upgrade acceptance through CLI and both native SDKs."""
from fixture_trust import grant_fixture
from pathlib import Path
from contextlib import closing
import hashlib,json,os,sqlite3,subprocess,sys,time,traceback
from index_paths import physical_database

ROOT=Path(__file__).resolve().parents[1]
RUN=ROOT/'.artifacts/up'/format(time.time_ns(),'x')[-10:];DATA=RUN/'d'
RUN.mkdir(parents=True)
assert len(os.fsencode(DATA/'c.sock'))<104,'run the assembled fixture from a short durable checkout'
PROFILE=os.environ.get('KNOW_CODE_BUILD_PROFILE','debug');CONFIG='Debug' if PROFILE=='debug' else 'Release'
HOST=ROOT/f'target/{PROFILE}/know-code';NODE=ROOT/f'.artifacts/build/{PROFILE}/node/index.mjs'
SMOKE=ROOT/f'bindings/dotnet/Smoke/bin/{CONFIG}/net10.0/Smoke.dll'
ENV=dict(os.environ,KNOW_CODE_DATA_DIR=str(DATA),DOTNET_CLI_TELEMETRY_OPTOUT='1',DOTNET_NOLOGO='1')
ENV['KNOW_CODE_QUERY_SCOPE']=str(RUN/'query-admission')
for key in ['KNOW_CODE_COORDINATOR','KNOW_CODE_RUNTIME_DIR','NODE_OPTIONS','NODE_PATH']:ENV.pop(key,None)
A=RUN/'a';B=RUN/'b';C=RUN/'c';F=RUN/'f';DATABASES={};CHECKS=[];SEEDS={};passed=False;failure=None
TS='// π😀 saved fixture\nexport function upgradeTarget() { return "initial😀"; }\nexport function legacyHelper() { return upgradeTarget(); }\n'
CS='public static class UpgradeCode {\n public static string Value() => "initial π😀";\n}\n'
for root in [A,B,C,F]:
 root.mkdir();(root/'value.ts').write_text(TS if root==A else f'export const owned{root.name.upper()} = 1;\n')
 (root/'tsconfig.json').write_text(json.dumps({'compilerOptions':{'target':'ES2022','module':'CommonJS','strict':True},'include':['*.ts']}))
(A/'App.csproj').write_text('<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><TargetFramework>net10.0</TargetFramework></PropertyGroup></Project>')
(A/'Code.cs').write_text(CS);(A/'.know-code.json').write_text('{"csharp":{"configuration":"Release"}}\n')

def sha(path):return hashlib.sha256(Path(path).read_bytes()).hexdigest()
def run(args,check=True,timeout=120):
 started=time.monotonic();argv=list(map(str,args))
 try:p=subprocess.run(argv,cwd=ROOT,env=ENV,capture_output=True,text=True,timeout=timeout)
 except subprocess.TimeoutExpired as error:
  with (RUN/'commands.jsonl').open('a') as log:log.write(json.dumps({'args':argv,'timeout':timeout,'stdout':str(error.stdout),'stderr':str(error.stderr)})+'\n')
  raise
 with (RUN/'commands.jsonl').open('a') as log:log.write(json.dumps({'args':argv,'exit':p.returncode,'stdout':p.stdout,'stderr':p.stderr,'elapsed_ms':(time.monotonic()-started)*1000})+'\n')
 assert not check or p.returncode==0,(argv,p.returncode,p.stdout,p.stderr)
 return p

def cli(*args,check=True):return run([HOST,*args],check)
def value(*args):return json.loads(cli(*args).stdout)
def status(root):return next(s for s in value('coordinator','status')['workspaces'] if s['workspace_root']==str(root))
def wait(root,predicate,description):
 until=time.monotonic()+90;last=None
 while time.monotonic()<until:
  last=status(root)
  if predicate(last):return last
  time.sleep(.05)
 raise AssertionError((description,last))
def ready(root,minimum=1):
 return wait(root,lambda s:s['phase']=='idle' and s['generation']>=minimum and s['observed_epoch']==s['published_epoch'],'current saved publication')
def stopped():return not (DATA/'c.sock').exists()
def stop():
 cli('coordinator','stop',check=False);until=time.monotonic()+20
 while not stopped() and time.monotonic()<until:time.sleep(.05)
 assert stopped(),'owned coordinator did not stop'
def lifecycle(root):return json.loads(Path(DATABASES[root]).with_suffix('.lifecycle.json').read_text())
def selector(root):return Path(DATABASES[root]).with_suffix('.lifecycle.json')
def contract_file(root):
 path=selector(root);stat=path.stat();return (path.read_bytes(),stat.st_ino,stat.st_mtime_ns)
def candidates(root):return {p.name for p in (Path(DATABASES[root]).parent/'indexes').iterdir() if p.is_dir()}
def healthy(root,incarnation=None):
 incarnation=incarnation or lifecycle(root)['selected']['id']
 path=Path(DATABASES[root]).parent/'indexes'/incarnation/'health';raw=path.read_bytes()
 assert raw[:8]==b'KCIDX001' and int.from_bytes(raw[16:24],sys.byteorder)==0,(root,incarnation,raw[:24].hex())
def version(root):
 with closing(sqlite3.connect(physical_database(DATABASES[root]))) as db:return db.execute('PRAGMA user_version').fetchone()[0]
def query(host,root,request):
 db=DATABASES[root]
 if host=='cli':return value('query',db,json.dumps(request))
 if host=='node':
  script='import {Index} from '+json.dumps(NODE.as_uri())+';console.log(JSON.stringify(await new Index(process.argv[1]).query(JSON.parse(process.argv[2]))));'
  return json.loads(run(['node','--input-type=module','-e',script,db,json.dumps(request)]).stdout)
 return json.loads(run(['dotnet',SMOKE,db,json.dumps(request)]).stdout)
def stable(response):
 assert isinstance(response.get('result'),dict),response
 result=dict(response['result']);result.pop('freshness',None);result.pop('coordination',None);return result
def parity(root,request,freshness=None,generation=None):
 results=[]
 for host in ['cli','node','dotnet']:
  response=query(host,root,request);result=response.get('result');assert isinstance(result,dict),(host,request,response)
  if freshness is not None:assert result['freshness']==freshness,(host,request,response)
  if generation is not None:assert result['generation']==generation,(host,request,response)
  results.append(stable(response))
 assert results[0]==results[1]==results[2],(request,results)
 return results[0]
def errors(root,request,code):
 for host in ['cli','node','dotnet']:
  response=query(host,root,request);assert response.get('error',{}).get('code')==code,(host,request,response)
def preparation(root):return Path(DATABASES[root]).with_suffix('.preparation.json')
def retained_files(root):
 return {str(path):path.read_bytes() for path in [preparation(root),Path(DATABASES[root]).parent/'settings.json',root/'.know-code.json'] if path.exists()}
def unchanged(files):
 for path,raw in files.items():assert Path(path).read_bytes()==raw,('durable settings/preparation changed',path)
def high_reserve():
 filesystem=os.statvfs(DATA);impossible=filesystem.f_blocks*filesystem.f_frsize//(1024*1024)+1024
 result=value('resources','--min-free-disk-mib',impossible);assert result['min_free_disk_mib']==impossible,result

def rewrite_offline(root,format):
 """Replace owned bytes in the guarded inode; original repositories are absent."""
 assert stopped(),'offline seeding requires the isolated coordinator to stop'
 path=physical_database(DATABASES[root]);before=path.stat();identity=(before.st_dev,before.st_ino)
 assert path.is_relative_to(DATA) and before.st_nlink==1,(path,identity)
 with closing(sqlite3.connect(path)) as db:assert db.execute('PRAGMA wal_checkpoint(TRUNCATE)').fetchone()==(0,0,0)
 # Closed/offline fixture connections are the only handles. Removing sidecars
 # before truncation prevents old frames from replaying into the historical DB.
 for suffix in ['-wal','-shm']:
  sidecar=Path(str(path)+suffix)
  if sidecar.exists():assert sidecar.is_file() and sidecar.stat().st_nlink==1;sidecar.unlink()
 with path.open('r+b') as file:file.truncate(0);file.flush();os.fsync(file.fileno())
 with closing(sqlite3.connect(path)) as db:
  if format==999:
   db.executescript("CREATE TABLE foreign_layout(key TEXT PRIMARY KEY,value BLOB NOT NULL); INSERT INTO foreign_layout VALUES('owner',X'010203'); PRAGMA user_version=999;")
  else:
   db.executescript((ROOT/'tests/fixtures/schema-v2.sql').read_text())
   assert db.execute('PRAGMA journal_mode=WAL').fetchone()==('wal',)
   db.execute('INSERT INTO generations VALUES(42)');db.execute('UPDATE state SET current_generation=42')
   documents=[('legacy-ts','typescript','value.ts',(root/'value.ts').read_text())]
   if (root/'Code.cs').exists():documents.append(('legacy-cs','csharp','Code.cs',(root/'Code.cs').read_text()))
   for comp,language,filename,text in documents:
    raw=text.encode();digest=hashlib.sha256(raw).hexdigest();artifact=comp+'-source'
    db.execute('INSERT INTO compilations VALUES(42,?,?,?,?,?,0,?,?,?,?)',(comp,language,str(root/('tsconfig.json' if language=='typescript' else 'App.csproj')),'legacy-target','Release','owned historical fixture','{}','[]','[]'))
    db.execute('INSERT INTO generation_units VALUES(42,?,42)',[comp]);db.execute('INSERT INTO contents VALUES(?,?)',(digest,raw))
    db.execute('INSERT INTO artifacts VALUES(42,?,?,?,0)',(artifact,filename,digest));db.execute('INSERT INTO unit_artifacts VALUES(42,?,?)',(comp,artifact))
    names=['upgradeTarget','legacyHelper'] if root==A and language=='typescript' else ['UpgradeCode','Value'] if language=='csharp' else ['owned'+root.name.upper()]
    for name in names:
     start=raw.index(name.encode());end=start+len(name.encode());symbol=comp+'-'+name
     line_start=raw.rfind(b'\n',0,start)+1;line_end=raw.find(b'\n',end);line_end=len(raw) if line_end<0 else line_end
     extent=(0,len(raw.rstrip(b'\n'))) if name=='UpgradeCode' else (line_start,line_end)
     db.execute('INSERT INTO symbols(generation,compilation_id,id,name,kind,signature,external) VALUES(42,?,?,?,?,?,0)',(comp,symbol,name,'class' if name=='UpgradeCode' else 'function','legacy '+name+' π😀'))
     for role,begin,finish in [('declaration',start,end),('definition',*extent)]:db.execute('INSERT INTO occurrences VALUES(42,?,?,?,?,?,?,?)',(comp,symbol,artifact,begin,finish,role,''))
    db.execute('INSERT INTO reports VALUES(42,?,?,?)',(comp,'partial','[]'))
   if format==1:
    db.commit();db.executescript('PRAGMA foreign_keys=OFF; DROP TABLE generation_units; DROP TABLE unit_artifacts; ALTER TABLE compilations DROP COLUMN dependencies_json; ALTER TABLE compilations DROP COLUMN input_paths_json; PRAGMA user_version=1;')
   db.commit()
  assert db.execute('PRAGMA integrity_check(1)').fetchone()==('ok',)
  db.commit()
  if format!=999:
   assert db.execute('PRAGMA journal_mode').fetchone()==('wal',)
   assert db.execute('PRAGMA wal_checkpoint(TRUNCATE)').fetchone()==(0,0,0)
 with path.open('rb') as file:os.fsync(file.fileno())
 assert (path.stat().st_dev,path.stat().st_ino)==identity,'offline fixture changed guarded file identity'
 SEEDS[str(root)]={'format':format,'path':str(path),'identity':identity,'sha256':sha(path),'generation':42 if format!=999 else None,'selected':lifecycle(root)['selected']['id']}
 (RUN/f'seed-{root.name}.json').write_text(json.dumps(SEEDS[str(root)],indent=2)+'\n')
 healthy(root)

def blocked_upgrade(root,format):
 observed=wait(root,lambda s:s['phase']=='resource_limited','upgrade preflight disk deferral')
 state=lifecycle(root)
 assert observed['outcome_code']=='disk_pressure' and not state['failed'] and state['pending'] is None,(observed,state)
 assert state.get('failed_reason') is None,state
 assert state['selected']['id']==SEEDS[str(root)]['selected'],state
 assert version(root)==format,(format,state)
 healthy(root);return observed

def old_reads(root):
 result=parity(root,{'op':'symbols','limit':100},'stale',42)
 assert {c['language'] for c in result['coverage']}=={'typescript','csharp'},result
 assert all(c['status']=='partial' for c in result['coverage']),result
 target=next(s for s in result['symbols'] if s['name']=='upgradeTarget');location=target['locations'][0]
 source=parity(root,{'op':'source','artifact_id':location['artifact_id'],'content_hash':location['content_hash'],'snapshot':result['snapshot'],'max_bytes':65536},'stale',42)['source']
 assert source['text']==TS and source['content_hash']==hashlib.sha256(TS.encode()).hexdigest(),source
 coverage=parity(root,{'op':'coverage','snapshot':result['snapshot']},'stale',42)
 assert len(coverage['coverage'])==2 and all(c['status']=='partial' for c in coverage['coverage']),coverage
 return result,target

def capture_cursor(root):
 response=query('cli',root,{'op':'symbols','limit':100,'page_size':1});assert response.get('result',{}).get('freshness')=='stale',response
 first=stable(response);assert first['generation']==42 and first['snapshot'],first
 pages=[];page=first
 while True:
  token=page['page']['cursor'];assert len(page['symbols'])<=1,page
  replay=parity(root,{'op':'next_page','cursor':token},'stale',42);assert replay==page,(replay,page)
  pages.append((token,page))
  next_token=page['page']['next_cursor']
  if next_token is None:break
  page=parity(root,{'op':'next_page','cursor':next_token},'stale',42)
 assert len(pages)==4,pages
 return first,pages

def replay_old(root,first,pages,target):
 for token,page in pages:assert parity(root,{'op':'next_page','cursor':token},'stale',42)==page
 token=first['page']['cursor'];location=target['locations'][0]
 source=parity(root,{'op':'source','cursor':token,'artifact_id':location['artifact_id'],'content_hash':location['content_hash'],'max_bytes':65536},'stale',42)['source']
 assert source['text']==TS and source['content_hash']==hashlib.sha256(TS.encode()).hexdigest(),source
 context=parity(root,{'op':'symbol_context','cursor':token,'symbol_id':target['id'],'max_bytes':65536},'stale',42)
 declaration=context['contexts'][0]['declarations'][0];raw=TS.encode();extent=declaration['extent']
 assert declaration['source']['text']==raw[extent['start_byte']:extent['end_byte']].decode(),declaration
 assert 'initial😀' in declaration['source']['text'] and context['snapshot']==first['snapshot'],context

BUILD={}
try:
 record_path=ROOT/f'.artifacts/build/{PROFILE}/build-record.json';record=json.loads(record_path.read_text())
 assert record['status']=='complete' and record['profile']==PROFILE,record
 artifacts=[HOST,NODE,NODE.parent/'know_code.node',SMOKE,SMOKE.parent/'KnowCode.dll',ROOT/f'target/{PROFILE}/libknow_code_ffi.dylib']
 hashes={str(path.relative_to(ROOT)):sha(path) for path in artifacts}
 assert all(record['outputs'][path]==digest for path,digest in hashes.items()),'test artifacts differ from producer record'
 BUILD={'build_id':record['build_id'],'source_revision':record['source_revision'],'record_sha256':sha(record_path),'outputs':hashes}
 info=value('runtime-info');assert info['store_format']==8 and info['index_lifecycle_version']==4,info
 value('resources','--min-free-disk-mib',0)
 for root in [A,B,C,F]:
  grant_fixture(HOST,root,ENV,RUN,RUN/'fixture-trust.jsonl')
  value('watch',root);initial=ready(root);DATABASES[root]=initial['database'];assert initial['generation']<42 and version(root)==8,initial
 value('settings',A,json.dumps({'csharp':{'configuration':'Release'},'allow_install_scripts':False}));ready(A)
 current=parity(A,{'op':'symbols','limit':100},'current')
 assert {c['language'] for c in current['coverage']}=={'typescript','csharp'} and all(c['status']=='complete' for c in current['coverage']),current
 assert any(s['name']=='upgradeTarget' for s in current['symbols']) and any(s['name']=='UpgradeCode' for s in current['symbols']),current
 assert preparation(A).is_file() and (Path(DATABASES[A]).parent/'settings.json').is_file()
 durable=retained_files(A);CHECKS.append('real_TS_Csharp_format8_and_three_host_native_parity_with_preparation_settings')
 high_reserve();stop();rewrite_offline(A,2);value('watch',A);blocked_upgrade(A,2)
 unchanged(durable);old,target=old_reads(A);pre_restart,pages_before=capture_cursor(A);replay_old(A,pre_restart,pages_before,target)
 frozen=contract_file(A);inventory=candidates(A)
 (A/'value.ts').write_text(TS.replace('initial😀','saved while blocked😀'))
 before_b=ready(B)['generation'];(B/'value.ts').write_text('export const ownedB = "saved during deferred upgrade";\n')
 wait(B,lambda s:s['phase']=='resource_limited' and s['outcome_code']=='disk_pressure','shared sibling write deferral')
 sibling=parity(B,{'op':'symbols','name':'ownedB'},'stale',before_b)
 sibling_symbol=sibling['symbols'][0]
 sibling_source=parity(B,{'op':'source','artifact_id':sibling_symbol['locations'][0]['artifact_id'],'snapshot':sibling['snapshot']},'stale',before_b)['source']
 assert sibling_source['text']=='export const ownedB = 1;\n',sibling_source
 for _ in range(4):status(A);time.sleep(.15)
 assert contract_file(A)==frozen and candidates(A)==inventory,'saves/status consumed a blocked upgrade attempt'
 unchanged(durable);stop();value('watch',A)
 wait(A,lambda s:s['phase']=='resource_limited' and s['outcome_code']=='disk_pressure','persisted deferred upgrade')
 assert contract_file(A)==frozen and candidates(A)==inventory,'restart allocated a blocked upgrade attempt'
 assert version(A)==2;unchanged(durable);healthy(A)
 errors(A,{'op':'next_page','cursor':pre_restart['page']['cursor']},'cursor_invalidated')
 old,target=old_reads(A);first,pages=capture_cursor(A)
 assert first['snapshot']==pre_restart['snapshot'],'restart changed healthy physical snapshot identity'
 CHECKS.append('preflight_deferral_stale_v2_all_hosts_portable_pages_source_context_no_attempt_and_queryable_sibling')
 # Keep this coordinator running: its healthy old cursors must survive cutover.
 value('resources','--min-free-disk-mib',0);upgraded=ready(A,43);ready(B,before_b+1);assert version(A)==8 and lifecycle(A)['selected']['id']!=SEEDS[str(A)]['selected']
 replacement=parity(A,{'op':'symbols','limit':100},'current',upgraded['generation'])
 assert {c['language'] for c in replacement['coverage']}=={'typescript','csharp'} and all(c['status']=='complete' for c in replacement['coverage']),replacement
 new_target=next(s for s in replacement['symbols'] if s['name']=='upgradeTarget')
 new_source=parity(A,{'op':'source','artifact_id':new_target['locations'][0]['artifact_id'],'snapshot':replacement['snapshot'],'max_bytes':65536},'current',upgraded['generation'])['source']
 assert new_source['text']==(A/'value.ts').read_text() and 'saved while blocked😀' in new_source['text'],new_source
 errors(A,{'op':'source','artifact_id':target['locations'][0]['artifact_id'],'snapshot':old['snapshot']},'snapshot_invalidated')
 replay_old(A,first,pages,target);healthy(A);healthy(A,SEEDS[str(A)]['selected']);unchanged(durable)
 (A/'value.ts').write_text((A/'value.ts').read_text().replace('saved while blocked😀','ordinary later save😀'));later=ready(A,upgraded['generation']+1)
 replay_old(A,first,pages,target);healthy(A);healthy(A,SEEDS[str(A)]['selected']);unchanged(durable)
 assert parity(A,{'op':'symbols','name':'upgradeTarget'},'current',later['generation'])['symbols']
 CHECKS.append('automatic_after_preflight_real_TS_Csharp_replacement_advances_actual42_old_cursors_survive_cutover_and_later_publication')
 # No usable format1 fallback. A separate owned historical fixture shares the
 # same guarded replacement path; schema1 is derived as in publication.rs.
 high_reserve();stop();rewrite_offline(C,1);value('watch',C);blocked_upgrade(C,1)
 errors(C,{'op':'symbols'},'not_ready');healthy(C)
 value('resources','--min-free-disk-mib',0);current_c=ready(C,43)
 assert version(C)==8 and parity(C,{'op':'symbols','name':'ownedC'},'current',current_c['generation'])['symbols']
 CHECKS.append('actual_schema1_no_readable_fallback_not_ready_and_auto_rebuild_after_preflight')
 # A genuinely foreign layout has no Know Code fact tables. Unsupported format
 # refusal must happen before interpreting them or authorizing replacement.
 stop();rewrite_offline(F,999)
 foreign_path=physical_database(DATABASES[F]);foreign_hash=sha(foreign_path);foreign_selector=contract_file(F);foreign_inventory=candidates(F)
 value('watch',B);wait(F,lambda s:s['phase']=='failed' and s['outcome_code']=='update_required','unsupported foreign format')
 errors(F,{'op':'symbols'},'update_required')
 refused=cli('index',F,check=False);assert refused.returncode!=0,(refused.stdout,refused.stderr)
 assert status(F)['outcome_code']=='update_required',status(F)
 assert sha(foreign_path)==foreign_hash and contract_file(F)==foreign_selector and candidates(F)==foreign_inventory,'unsupported layout was rewritten or replacement attempted'
 with closing(sqlite3.connect(foreign_path)) as db:
  assert db.execute('PRAGMA user_version').fetchone()==(999,)
  assert db.execute("SELECT value FROM foreign_layout WHERE key=?",["owner"]).fetchone()==(b'\x01\x02\x03',)
  assert db.execute("SELECT count(*) FROM sqlite_master WHERE name='state'").fetchone()==(0,)
 healthy(F);before_b=ready(B)['generation'];(B/'value.ts').write_text('export const ownedB = "healthy beside unsupported format";\n');sibling=ready(B,before_b+1)
 assert parity(B,{'op':'symbols','name':'ownedB'},'current',sibling['generation'])['symbols']
 assert sha(foreign_path)==foreign_hash and contract_file(F)==foreign_selector and candidates(F)==foreign_inventory
 CHECKS.append('foreign_format999_query_refresh_refusal_no_rewrite_no_candidate_no_BAD_healthy_sibling_progress')
 assert all(sha(ROOT/path)==digest for path,digest in hashes.items()) and sha(record_path)==BUILD['record_sha256'],'producer artifacts changed during acceptance'
 passed=True
except BaseException:
 failure=traceback.format_exc();(RUN/'failure.txt').write_text(failure);raise
finally:
 cleanup=None
 try:stop()
 except BaseException:cleanup=traceback.format_exc();passed=False
 summary={'passed':passed,'profile':PROFILE,'run':str(RUN),'checks':CHECKS,'databases':{str(k):v for k,v in DATABASES.items()},'seeds':SEEDS,'build':BUILD,'test_sha256':sha(Path(__file__)),'schema_v2_sha256':sha(ROOT/'tests/fixtures/schema-v2.sql'),'failure':failure,'cleanup_error':cleanup,'limits':['finite owned historical SQLite/native fixture; actual TS/Csharp analysis on replacement','no full suite/performance certification, network policy, live installation or original corpus changes']}
 (RUN/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary,indent=2))
 if cleanup and failure is None:raise AssertionError(cleanup)
