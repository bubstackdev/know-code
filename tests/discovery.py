"""Owned TS repositories: durable parent enrollment through the real coordinator."""
from fixture_trust import grant_fixture
from pathlib import Path
import hashlib,json,os,shutil,subprocess,time

ROOT=Path(__file__).resolve().parents[1]
RUN=ROOT/'.artifacts/discovery'/format(time.time_ns(),'x');RUN.mkdir(parents=True)
HOST=Path(os.environ.get('KNOW_CODE_TEST_HOST',ROOT/f'target/{os.environ.get("KNOW_CODE_BUILD_PROFILE","debug")}/know-code')).resolve()
DATA=Path(os.environ.get('KNOW_CODE_TEST_DATA',ROOT/'.artifacts/pd'/format(time.time_ns(),'x')[-10:])).resolve()
assert not DATA.exists() and len(str(DATA/'c.sock').encode())<104,'choose fresh, durable short data directory'
assert DATA.is_relative_to((ROOT/Path(subprocess.check_output(['git','rev-parse','--git-common-dir'],cwd=ROOT,text=True).strip())).resolve().parent), 'test data must be under the durable project root'
ENV=dict(os.environ,KNOW_CODE_DATA_DIR=str(DATA),MSBUILDDISABLENODEREUSE='1',DOTNET_CLI_USE_MSBUILD_SERVER='0',DOTNET_CLI_TELEMETRY_OPTOUT='1')
ENV['KNOW_CODE_QUERY_SCOPE']=str(RUN/'query-admission')
PARENT=RUN/'estate';GROUP=PARENT/'group';GROUP.mkdir(parents=True)
checks=[]

def command(*args,check=True):
    result=subprocess.run([str(HOST),*map(str,args)],cwd=ROOT,env=ENV,capture_output=True,text=True,timeout=120)
    with (RUN/'commands.jsonl').open('a') as log:log.write(json.dumps({'argv':list(map(str,args)),'returncode':result.returncode,'stdout':result.stdout,'stderr':result.stderr})+'\n')
    assert not check or result.returncode==0,(args,result.stdout,result.stderr)
    return result
def value(*args):return json.loads(command(*args).stdout)
def status():return value('coordinator','status')
def parents():return value('parents','list')['parents']
def wait(predicate,seconds=60):
    until=time.monotonic()+seconds;last=None
    while time.monotonic()<until:
        last=predicate()
        if last:return last
        time.sleep(.1)
    raise AssertionError(('condition timed out',last,RUN))
def settled():
    states=parents()
    return states if states and all(s['phase'] not in ['queued','scanning'] for s in states) else None
def workspace(root):return next((w for w in status()['workspaces'] if w['workspace_root']==str(root)),None)
def current(root,minimum=1):
    s=workspace(root)
    return s if s and s['phase']=='idle' and not s['error'] and s['generation']>=minimum and s['published_epoch']==s['observed_epoch'] else None
def repo(path,name,gitfile=False,trust=True):
    path.mkdir(parents=True)
    if gitfile:(path/'.git').write_text('gitdir: owned-fixture-metadata\n')
    else:(path/'.git').mkdir()
    (path/'value.ts').write_text(f'export function {name}() {{ return 1; }}\n')
    if trust:grant_fixture(HOST,path,ENV,Path(os.path.commonpath([RUN,DATA])),RUN/'fixture-trust.jsonl')
    return path.resolve()
def query(root,name):
    s=wait(lambda:current(root));result=value('query',s['database'],json.dumps({'op':'symbols','name':name}))
    assert 'error' not in result and result['result']['freshness']=='current',result
    result=result['result'];assert result['generation']==s['generation'],result
    whole=result['workspace_coverage'];assert whole['compilations']>0 and whole['statuses'].get('complete',0)==whole['compilations'],result
    # An absent symbol has no relevant compilation reports. Positive findings
    # still require nonempty complete compiler coverage, separately from status.
    assert (not result.get('symbols') or result['coverage']) and all(c['status']=='complete' for c in result['coverage']),result
    return result

try:
    a=repo(GROUP/'a','discoveredA');nested=repo(a/'nested','nestedExplicit',gitfile=True)
    excluded=repo(GROUP/'excluded'/'e','excludedRepo');outside=repo(RUN/'outside','linkedExplicit',gitfile=True)
    (GROUP/'alias').symlink_to(outside,target_is_directory=True)
    alias=RUN/'parent-alias';alias.symlink_to(PARENT,target_is_directory=True)
    first=value('parents','add',alias,'--exclude','group/excluded');assert first['parent_discovery_version']==1
    wait(settled);assert query(a,'discoveredA')['symbols']
    assert {w['workspace_root'] for w in status()['workspaces']}=={str(a)}
    checks+=['automatic_TS_compiler_indexing','repository_boundary_stops_nested_enrollment','directory_symlinks_skipped','prefix_exclusions']
    before=parents();bad=command('parents','add',PARENT,'--exclude','../outside',check=False);assert bad.returncode and 'relative' in bad.stderr
    assert parents()[0]['exclusions']==before[0]['exclusions']
    value('parents','add',GROUP,'--exclude','excluded');wait(settled)
    value('watch',GROUP/'a');assert len(status()['workspaces'])==1
    checks+=['invalid_exclusions_atomic','canonical_parent_alias_and_overlapping_registration_dedup']
    db=workspace(a)['database'];value('parents','remove',PARENT);assert workspace(a)['database']==db
    value('parents','add',PARENT,'--exclude','group/excluded');wait(settled)
    command('unwatch',a);value('parents','scan',PARENT);value('parents','scan',GROUP);wait(settled)
    assert workspace(a) is None and Path(db).exists()
    command('coordinator','stop');wait(lambda:not (DATA/'c.sock').exists())
    assert len(parents())==2;wait(settled);assert workspace(a) is None
    grant_fixture(HOST,a,ENV,Path(os.path.commonpath([RUN,DATA])),RUN/'fixture-trust.jsonl')
    command('watch',a);assert query(a,'discoveredA')['symbols']
    checks+=['parent_removal_retains_children','explicit_unwatch_suppression_survives_overlap_and_restart','explicit_watch_clears_suppression']
    command('watch',outside);assert query(outside,'linkedExplicit')['symbols']
    checks.append('explicit_gitfile_repository_supported')
    future=repo(GROUP/'future','futureRepository')
    assert wait(lambda:current(future),seconds=45) and query(future,'futureRepository')['symbols']
    checks.append('future_child_auto_enrollment_without_manual_scan')
    before=wait(lambda:current(a))['generation'];(a/'value.ts').write_text('export function savedDiscovery() { return 2; }\n')
    wait(lambda:current(a,before+1));assert query(a,'savedDiscovery')['symbols'];assert not query(a,'discoveredA').get('symbols')
    checks.append('discovered_workspace_uses_saved_update_path')
    held=RUN/'held-repository';a.rename(held);wait(lambda:workspace(a) and not workspace(a)['available'])
    repo(a,'replacementMustNotInherit',trust=False);value('parents','scan',GROUP);wait(settled)
    assert not workspace(a)['available'] and workspace(a)['database']==db
    assert any(p['errors'] and 'identity changed' in p['error'] for p in parents())
    shutil.rmtree(a);held.rename(a);wait(lambda:current(a));assert query(a,'savedDiscovery')['symbols']
    checks.append('missing_and_replaced_child_retain_index_and_same_identity_resumes')
    held_parent=RUN/'held-parent';PARENT.rename(held_parent);value('parents','scan',PARENT);wait(settled)
    assert next(p for p in parents() if p['root']==str(PARENT))['phase']=='unavailable'
    PARENT.mkdir();value('parents','scan',PARENT);wait(settled)
    assert next(p for p in parents() if p['root']==str(PARENT))['phase']=='unavailable'
    PARENT.rmdir();held_parent.rename(PARENT);value('parents','scan',PARENT);value('parents','scan',GROUP);wait(settled)
    wait(lambda:current(a));checks.append('parent_missing_replacement_identity_and_restore')
    value('parents','add',PARENT,'--exclude','group');value('parents','remove',GROUP);wait(settled)
    assert query(a,'savedDiscovery')['symbols'];assert query(future,'futureRepository')['symbols']
    value('parents','remove',PARENT);assert parents()==[]
    assert query(a,'savedDiscovery')['symbols'];checks.append('exclusions_and_parent_removal_never_unregister_existing_workspaces')
    management=RUN/'management';management.mkdir();redirect=RUN/'redirect';redirect.mkdir()
    value('parents','add',management);wait(settled);management.rename(RUN/'management-held');management.symlink_to(redirect,target_is_directory=True)
    value('parents','scan',management);wait(settled);assert parents()[0]['root']==str(management) and parents()[0]['phase']=='unavailable'
    value('parents','remove',management);assert parents()==[] and redirect.is_dir()
    checks.append('management_retains_original_key_when_parent_becomes_symlink')
    result={'passed':True,'checks':checks,'run':str(RUN),'data':str(DATA),'host':str(HOST),'host_sha256':hashlib.sha256(HOST.read_bytes()).hexdigest(),'harness_sha256':hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),'runtime_resources':ENV.get('KNOW_CODE_RUNTIME_DIR'),'worker_protocol':7,'limits':['owned TS fixtures only; no SDK/MCP/package/performance or full-suite certification','nested-source/compiler behavior remains independent from repository enrollment']}
    (RUN/'summary.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result,indent=2))
finally:
    command('coordinator','stop',check=False)
