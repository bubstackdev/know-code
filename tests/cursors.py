"""Real CLI/Node/.NET cursor replay, pinned text, invalidation and central settings.

Run after a matching build; this harness never builds, mutates live state or uses
external corpora. Every subprocess response is retained in the isolated run.
"""
from pathlib import Path
import hashlib,json,os,subprocess,time

ROOT=Path(__file__).resolve().parents[1]
RUN=ROOT/'.artifacts/cu'/format(time.time_ns(),'x')[-10:]
WORKSPACE=RUN/'w';OTHER=RUN/'v';DATA=RUN/'d'
WORKSPACE.mkdir(parents=True);OTHER.mkdir()
PROFILE=os.environ.get('KNOW_CODE_BUILD_PROFILE','debug')
CONFIG='Debug' if PROFILE=='debug' else 'Release'
HOST=ROOT/f'target/{PROFILE}/know-code'
ADDON=ROOT/f'.artifacts/build/{PROFILE}/node/index.mjs'
SMOKE=ROOT/f'bindings/dotnet/Smoke/bin/{CONFIG}/net10.0/Smoke.dll'
ENV=dict(os.environ,KNOW_CODE_DATA_DIR=str(DATA),DOTNET_CLI_TELEMETRY_OPTOUT='1')
# A long symbol name guarantees a replay cannot fit the minimum response
# budget, independently of signature truncation or coverage metadata.
TARGET='alpha_'+('x'*1400)
OLD=('// saved old 😀\n'+''.join(
 f'export function {name}(value: string) {{ return value; }}\n'
 for name in [TARGET,'bravo','charlie','delta','echo','foxtrot']))
SOURCE=WORKSPACE/'value.ts';SOURCE.write_text(OLD)
(OTHER/'value.ts').write_text('export const otherWorkspace = 1;\n')
CHECKS=[];EVENTS=RUN/'commands.jsonl'

def run(args,*,input=None,check=True):
 p=subprocess.run(list(map(str,args)),cwd=ROOT,env=ENV,input=input,
                  capture_output=True,text=True,timeout=120)
 with EVENTS.open('a') as log:
  log.write(json.dumps({'args':list(map(str,args)),'input':input,
                       'exit':p.returncode,'stdout':p.stdout,'stderr':p.stderr})+'\n')
 assert not check or p.returncode==0,(args,p.stdout,p.stderr)
 return p

def cli(*args,**kwargs):return run([HOST,*args],**kwargs)
def value(*args):return json.loads(cli(*args).stdout)
def error(response,code):assert response.get('error',{}).get('code')==code,response

def ready(root=WORKSPACE,minimum=1):
 until=time.monotonic()+60;last=None
 while time.monotonic()<until:
  statuses=value('coordinator','status')['workspaces']
  last=next((s for s in statuses if s['workspace_root']==str(root)),None)
  assert last and last['phase']!='failed',last
  if (last['phase']=='idle' and last['published_epoch']==last['observed_epoch']
      and last['generation']>=minimum):return last
  time.sleep(.05)
 raise AssertionError(('index not ready',last))

def query(language,request,database=None):
 database=database or DATABASE
 if language=='cli':return value('query',database,json.dumps(request))
 if language=='node':
  js=('import {Index} from '+json.dumps(ADDON.as_uri())+';'
      'console.log(JSON.stringify(await new Index(process.argv[1]).query(JSON.parse(process.argv[2]))));')
  return json.loads(run(['node','--input-type=module','-e',js,database,json.dumps(request)]).stdout)
 if request['op']=='next_page':
  # This invokes Index.NextPageAsync, not the raw QueryJsonAsync path.
  args=['--next-page',database,request['cursor'],request.get('deadline_ms',5000),
        request.get('response_max_bytes',262144)]
 else:args=[database,json.dumps(request)]
 return json.loads(run(['dotnet',SMOKE,*args]).stdout)

def frozen(response):
 assert 'result' in response,response
 result=dict(response['result']);result.pop('freshness',None);result.pop('coordination',None)
 assert result['page']['cursor'] and 'next_cursor' in result['page'],result
 return result

def replay(cursor,expected):
 for language in ['cli','node','dotnet']:
  assert frozen(query(language,{'op':'next_page','cursor':cursor}))==expected

def workspace_next(cursor,**budgets):
 options={'dataDirectory':str(DATA),'coordinatorExecutable':str(HOST)}
 action=json.dumps({'op':'next_page','cursor':cursor,**budgets})+'\n'
 replies=run(['dotnet',SMOKE,'--workspace',WORKSPACE,json.dumps(options)],input=action).stdout.splitlines()
 assert len(replies)==2 and 'opened' in json.loads(replies[0]),replies
 return json.loads(replies[1])

passed=False
try:
 value('watch',WORKSPACE);initial=ready();DATABASE=initial['database']
 baseline=query('cli',{'op':'symbols','limit':1000})['result']
 assert len(baseline['symbols'])>4 and not baseline['truncated'],baseline
 first=query('cli',{'op':'symbols','limit':3,'page_size':1})
 pages=[frozen(first)]
 while pages[-1]['page']['next_cursor'] is not None:
  assert len(pages)<4,pages
  pages.append(frozen(query('node',{'op':'next_page','cursor':pages[-1]['page']['next_cursor']})))
 assert len(pages)==3 and all(p['truncated'] for p in pages),pages
 assert all(len(p['symbols'])==1 and p['page']['position']==i
            and p['page']['sections']['symbols']=={'total':3,'start':i,'count':1}
            and p['page']['page_count']==3 and p['page']['total_limit']==3
            and p['page']['page_size']==1 for i,p in enumerate(pages)),pages
 assert [p['symbols'][0] for p in pages]==baseline['symbols'][:3]
 for page in pages:replay(page['page']['cursor'],page)
 CHECKS+=['limit_is_total_cap_terminal_truncated','CLI_Node_dotnet_frozen_page_replay']

 target=query('cli',{'op':'symbols','name':TARGET})['result']['symbols'][0]
 retained=frozen(query('node',{'op':'symbols','name':TARGET,'limit':1,'page_size':1}))
 token=retained['page']['cursor'];generation=retained['generation'];location=target['locations'][0]
 for language in ['cli','node','dotnet']:
  for budget,code in [({'response_max_bytes':1024},'response_limit_exceeded'),
                      ({'deadline_ms':0},'deadline_exceeded')]:
   error(query(language,{'op':'next_page','cursor':token,**budget}),code)
   assert frozen(query(language,{'op':'next_page','cursor':token}))==retained
 assert frozen(workspace_next(token))==retained
 error(workspace_next(token,deadline_ms=0),'deadline_exceeded')
 error(workspace_next(token,response_max_bytes=1024),'response_limit_exceeded')
 assert frozen(workspace_next(token))==retained
 CHECKS+=['failed_small_cap_and_deadline_preserve_replay','Index_and_Workspace_NextPageAsync_paths']
 error(query('cli',{'op':'next_page','cursor':token,'limit':3}),'invalid_request')
 CHECKS.append('continuation_rejects_scope_mutation')

 SOURCE.write_text(OLD.replace('saved old','saved new').replace('return value;', 'return "changed";'))
 current=ready(minimum=generation+1)
 for page in pages:replay(page['page']['cursor'],page)
 replay(token,retained)
 source_request={'op':'source','cursor':token,'artifact_id':location['artifact_id'],
                 'content_hash':location['content_hash'],'start_byte':0,'max_bytes':len(OLD.encode())+1}
 context_request={'op':'symbol_context','cursor':token,'symbol_id':target['id'],
                  'include_source':True,'max_bytes':len(OLD.encode())+1}
 for language in ['cli','node','dotnet']:
  source=query(language,source_request)['result']
  assert source['generation']==generation and source['freshness']=='stale',source
  text=source['source'];assert text['text']==OLD and text['start_byte']==0,text
  assert text['content_hash']==location['content_hash']==hashlib.sha256(OLD.encode()).hexdigest(),text
  context=query(language,context_request)['result'];assert context['generation']==generation,context
  assert context['contexts'][0]['symbol']==target,context
  declarations=context['contexts'][0]['declarations'];assert declarations,context
  for declaration in declarations:
   extent=declaration['extent'];body=declaration['source']
   assert body['text'].encode()==OLD.encode()[extent['start_byte']:extent['end_byte']],declaration
   assert body['content_hash']==location['content_hash'] and 'return value;' in body['text'],declaration
 CHECKS+=['frozen_pages_after_saved_publication','pinned_source_and_context_exact_old_hash_bytes_ranges']

 value('watch',OTHER);other=ready(OTHER)
 for language in ['cli','node','dotnet']:
  error(query(language,{'op':'next_page','cursor':token},other['database']),'cursor_invalidated')
 replay(token,retained)
 CHECKS.append('wrong_workspace_token_rejected_without_poisoning_owner')

 defaults=value('cursors')['settings']
 assert defaults=={'max_cursors':128,'retention_bytes':1073741824},defaults
 configured=value('cursors','--max-count',8,'--retention-bytes',16777216)
 assert configured['settings']==value('cursors')['settings']=={'max_cursors':8,'retention_bytes':16777216}
 older=frozen(query('cli',{'op':'symbols','name':TARGET,'page_size':1}))
 newer=frozen(query('node',{'op':'symbols','name':'bravo','page_size':1}))
 lowered=value('cursors','--max-count',1)
 assert lowered['settings']=={'max_cursors':1,'retention_bytes':16777216} and not lowered.get('pending_pressure',False),lowered
 error(query('cli',{'op':'next_page','cursor':older['page']['cursor']}),'cursor_invalidated')
 replay(newer['page']['cursor'],newer)
 restored=value('cursors','--max-count',defaults['max_cursors'],'--retention-bytes',defaults['retention_bytes'])
 assert restored['settings']==defaults,restored
 CHECKS+=['central_cursor_settings_get_set','lower_count_evicts_idle_LRU_and_preserves_latest']

 removed=newer['page']['cursor'];cli('unwatch',WORKSPACE)
 value('watch',WORKSPACE);ready()
 for language in ['cli','node','dotnet']:
  error(query(language,{'op':'next_page','cursor':removed}),'cursor_invalidated')
 CHECKS.append('unregister_reregister_invalidates_prior_token')
 before_restart=frozen(query('cli',{'op':'symbols','page_size':1}))['page']['cursor']
 persisted=value('cursors','--max-count',7,'--retention-bytes',33554432)['settings']
 cli('coordinator','stop');until=time.monotonic()+15
 while (DATA/'c.sock').exists() and time.monotonic()<until:time.sleep(.05)
 assert not (DATA/'c.sock').exists(),'coordinator socket remained after shutdown'
 value('watch',WORKSPACE);ready()
 assert value('cursors')['settings']==persisted
 for language in ['cli','node','dotnet']:
  error(query(language,{'op':'next_page','cursor':before_restart}),'cursor_invalidated')
 assert query('dotnet',{'op':'symbols','page_size':1})['result']['page']['cursor']!=before_restart
 CHECKS+=['restart_invalidates_and_new_queries_recover','central_cursor_settings_persist_restart']
 passed=True
finally:
 cli('coordinator','stop',check=False)
 summary={'passed':passed,'profile':PROFILE,'run':str(RUN),'checks':CHECKS}
 (RUN/'summary.json').write_text(json.dumps(summary,indent=2)+'\n')
 print(json.dumps(summary,indent=2))
