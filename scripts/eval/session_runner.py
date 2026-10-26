"""Execute one prepared, frozen public-evaluation session with owned resources.

No default task deadline or model/call cap is added. The frozen study may specify
an operational process timeout; overruns of its soft research budget are reported.
Labels stay in the parent-side freeze; the client receives only its question.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import sys

import bare_client
from frozen_study import FrozenStudy, ROOT, load
from macos_sandbox import Policy
from model_relay import ModelRelay
import owned_process
import public_transcript


def write(path,value):
    with path.open('x') as stream:
        json.dump(value,stream,indent=2);stream.write('\n')


def verify_source(study,session):
    census=load('runner_source_census',ROOT/'scripts/census-public-eval.py')
    source=session/'source'
    if source.is_symlink() or not source.is_dir() or {p.name for p in source.iterdir()}!={r['name'] for r in study.receipt['repos']}:
        raise ValueError('session source repository set differs')
    for repo in study.receipt['repos']:
        census.inspect_view(source/repo['name'],repo)


def policy_for(session,client,knowcode=None,git_toolchain=None,full=False):
    # A full client runs plugin hooks through sh; every full arm gets it, so their executables differ only by KnowCode.
    executables=[client,Path('/bin/bash'),*bare_client.PROGRAMS.values(),*([Path('/bin/sh')] if full else [])]
    dependencies=[]
    if knowcode is not None:
        if git_toolchain is None:
            raise ValueError('explicit native Git toolchain required for the KnowCode arm')
        toolchain=Path(git_toolchain).resolve(strict=True)
        binary=toolchain/'usr/bin/git';library=toolchain/'usr/lib/libxcrun.dylib'
        if not binary.is_file() or not library.is_file():
            raise ValueError('native Git executable/library missing')
        executables.extend([knowcode,Path('/usr/bin/git'),binary]);dependencies=[toolchain,library]
    policy=Policy(session,executables)
    for number,path in enumerate(dependencies):
        key='GIT_SUPPORT'+str(number);policy.parameters[key]=path
        operations='file-read-data file-map-executable' if path.is_file() else 'file-read-data'
        policy.profile+='(allow '+operations+' (literal (param "'+key+'")))\n'
    return policy


def open_study(freeze):
    """A public-v1 freeze, or a public-v2 or -v3 one (public_v2.Study, public_v3.Study), checked from content either way."""
    form=public_transcript.load_json(Path(freeze).read_bytes()).get('format')
    if form=='knowcode-public-v2-freeze-1':
        import public_v2
        return public_v2.Study(freeze)
    if form=='knowcode-public-v3-freeze-1':
        import public_v3
        return public_v3.Study(freeze)
    return FrozenStudy(freeze)


def execute(freeze,session,client,knowcode,out,*,api_key,git_toolchain=None,_connection_factory=None):
    state={}
    try:
        return _execute(freeze,session,client,knowcode,out,api_key=api_key,git_toolchain=git_toolchain,
                        _connection_factory=_connection_factory,_state=state)
    except BaseException as exc:
        if 'out' in state:
            # Retain failed attempts without logging exception text, which may
            # contain provider or environment values. Never overwrite evidence.
            failure=dict(format=1,session=state['session'],exception_type=type(exc).__name__,
                         status='error',notes='Execution failed; inspect retained stage evidence.')
            write(state['out']/'failure.json',failure)
            answer=state['out']/'answer.json'
            if not answer.exists():
                write(answer,dict(id=state['session']['question_id'],status='error',items=[],notes=failure['notes']))
        raise


def _execute(freeze,session,client,knowcode,out,*,api_key,git_toolchain=None,_connection_factory=None,_state):
    study=open_study(freeze)  # All content gates precede configuration/index/client/network startup.
    client,knowcode=Path(client).resolve(strict=True),Path(knowcode).resolve(strict=True)
    study.binaries(client,knowcode)
    session=Path(session)
    if session.is_symlink() or not session.is_dir():
        raise ValueError('real prepared session required')
    session=session.resolve(strict=True)
    receipt_path=session/'session.json'
    if receipt_path.is_symlink() or not receipt_path.is_file():
        raise ValueError('real prepared session receipt required')
    receipt_bytes=receipt_path.read_bytes();receipt=public_transcript.load_json(receipt_bytes)
    selected=receipt['session']
    if selected not in study.order:
        raise ValueError('prepared session differs from frozen schedule')
    question=next(q for q in study.questions if q['id']==selected['question_id'])
    bindings={'plan_sha256':'plan','manifest_sha256':'manifest','source_receipt_sha256':'source_receipt'}
    if (receipt['question']!=question or any(receipt.get(k)!=study.hashes[v] for k,v in bindings.items())
            or receipt.get('source_files_sha256')!=study.plan.get('source_files_sha256')):
        raise ValueError('session source/question differs from frozen study')
    producers=['scripts/prepare-public-eval-session.py','scripts/census-public-eval.py']
    if any(receipt.get('producer_hashes',{}).get(Path(p).name)!=bare_client.file_hash(ROOT/p) for p in producers):
        raise ValueError('session preparation implementation differs')
    if any(p.is_symlink() or not p.is_dir() or list(p.iterdir()) for p in [session/'control',session/'profile']):
        raise ValueError('new empty real control/profile directories required')
    out=Path(out).absolute()
    forbidden=[ROOT,session,study.path.parent,*[p.parent for p in study.inputs.values()]]
    if out.exists() or out.is_symlink() or any(out.resolve().is_relative_to(p) for p in forbidden):
        raise ValueError('new output outside source/study/session required')
    verify_source(study,session)
    timeout=json.loads(study.raw).get('process_timeout_seconds')
    if timeout is not None and (type(timeout) not in (int,float) or not math.isfinite(timeout) or timeout<=0):
        raise ValueError('invalid frozen operational timeout')
    if not isinstance(api_key,str) or not api_key.strip():
        raise ValueError('parent-side provider credential required')
    out.mkdir(mode=0o700,parents=True)
    _state.update(out=out,session=selected)
    write(out/'started.json',dict(format=1,freeze_sha256=study.freeze_sha256,session=selected,
                                 stage='validated study; execution started',process_timeout_seconds=timeout))
    roots=None
    full=selected['arm'] in bare_client.FULL_ARMS
    served=selected['arm'] in bare_client.KNOWCODE_ARMS
    if served or full:
        preparer=load('runner_neutral_index',ROOT/'scripts/prepare-public-eval-index.py')
        preparer.prepare(study.inputs['manifest'],study.inputs['source_receipt'],session)
        roots={r['name']:session/'control/index'/r['name'] for r in study.receipt['repos']}
    plan=bare_client.prepare(session,study.config,client,knowcode=knowcode if served else None,index_repos=roots)
    policy=policy_for(session,client,knowcode if served else None,git_toolchain,full)
    if plan.get('setup'):
        # Install the plugin as shipped into the session's own profile, with the pinned client as `claude`. This is
        # preparation, like the neutral workspaces: no model call, and nothing outside the session is written.
        shim=session/'control/setup-bin';shim.mkdir();(shim/'claude').symlink_to(client)
        setup_env={**plan['setup']['env'],'PATH':str(shim)+':'+plan['setup']['env']['PATH']}
        (session/'control/home').mkdir(exist_ok=True)
        installed=owned_process.run(plan['setup']['argv'],cwd=plan['cwd'],env=setup_env,
                                    stdout=out/'setup.stdout',stderr=out/'setup.stderr',timeout=timeout)
        (shim/'claude').unlink();shim.rmdir()
        write(out/'setup-process.json',installed)
        if installed.get('returncode')!=0 or not installed['cleanup_complete']:
            raise ValueError('plugin setup failed; client not started')
    version=owned_process.run(policy.command([client,'--version']),cwd=plan['cwd'],env=plan['env'],
                              stdout=out/'client-version.stdout',stderr=out/'client-version.stderr',timeout=15)
    if (version.get('returncode')!=0 or not version['cleanup_complete']
            or (out/'client-version.stdout').read_text().strip()!=study.config['client_version']+' (Claude Code)'):
        raise ValueError('actual client version differs from frozen configuration')
    if served:
        server={'env':{'KNOWCODE_CACHE_DIR':str(session/'control/kc-cache'),'KNOWCODE_CONFIG_DIR':str(session/'control/kc-config'),
                       'KNOWCODE_DATA_DIR':str(session/'control/kc-data'),'KNOWCODE_UPDATE_CHECK':'0','ANTHROPIC_API_KEY':'',
                       'GIT_CONFIG_NOSYSTEM':'1','GIT_CONFIG_SYSTEM':'/dev/null','GIT_CONFIG_GLOBAL':'/dev/null','GIT_TERMINAL_PROMPT':'0'}}
        # Warm indexing is preparation, outside the agent research interval. The
        # same isolated roots/cache are used by the subsequent MCP server.
        indexed=owned_process.run(policy.command([knowcode,'index','--threads','1','--verify','--json',*map(str,roots.values())]),
                                  cwd=plan['cwd'],env={**plan['env'],**server['env']},
                                  stdout=out/'index.stdout',stderr=out/'index.stderr',timeout=timeout)
        write(out/'index-process.json',indexed)
        if indexed.get('returncode')!=0 or not indexed['cleanup_complete']:
            raise ValueError('isolated index preparation failed; client not started')
    study.unchanged();study.binaries(client,knowcode);verify_source(study,session)
    config_paths=[session/'control/client-settings.json',session/'control/mcp.json']
    config_bytes={p:p.read_bytes() for p in config_paths}
    relay=ModelRelay(api_key,_connection_factory=_connection_factory)
    try:
        policy.parameters['API_LOOPBACK']='localhost:'+relay.base_url.rsplit(':',1)[1]
        policy.profile+='(allow network-outbound (remote ip (param "API_LOOPBACK")))\n'
        write(out/'launch.json',dict(format=1,freeze_sha256=study.freeze_sha256,session=selected,
                                   client_sha256=plan['client_sha256'],settings_sha256=plan['settings_sha256'],mcp_sha256=plan['mcp_sha256'],
                                   policy=policy.receipt(),actual_loopback_relay_enabled=True,provider='api.anthropic.com',
                                   operational_timeout_seconds=timeout,research_call_budget_enforced=False))
        outcome=owned_process.run(policy.command(plan['argv']),cwd=plan['cwd'],
                                  env={**plan['env'],'ANTHROPIC_API_KEY':relay.client_token,'ANTHROPIC_BASE_URL':relay.base_url},
                                  stdout=out/'transcript.jsonl',stderr=out/'client.stderr',timeout=timeout)
    finally:
        relay_closed=relay.close()
        records=relay.records();write(out/'relay.json',dict(records=records,cleanup_complete=relay_closed))
    events,parse_issues=public_transcript.read_events((out/'transcript.jsonl').read_bytes())
    # A full arm's agent works in the neutral workspaces, so its answers' absolute paths are under control/index.
    measured=public_transcript.audit(events,question_id=question['id'],arm=selected['arm'],
                                      research_call_budget=study.config['research_call_budget'],
                                      source_root=session/('control/index' if full else 'source'),
                                      two_hop=question['stratum']=='two-hop-impact',parse_issues=parse_issues)
    transport=public_transcript.audit_transport(measured,events,records,expected_model=study.config['model'],relay_cleanup_complete=relay_closed)
    problems=[]
    try:
        study.unchanged();study.binaries(client,knowcode);verify_source(study,session)
        if roots:
            for repo in study.receipt['repos']:
                preparer.verify_payload(roots[repo['name']],repo)
        if receipt_path.read_bytes()!=receipt_bytes or any(p.read_bytes()!=data for p,data in config_bytes.items()):
            raise ValueError('session receipt or client configuration changed')
    except Exception as exc:
        problems.append(type(exc).__name__+': frozen input/source/configuration revalidation failed')
    if outcome.get('returncode')!=0 or not outcome['cleanup_complete']:
        problems.append('client process or owned cleanup failed')
    answer=transport['answer']
    if problems:
        answer=dict(id=question['id'],status='error',items=[],notes='Frozen run integrity failed; inspect preserved evidence.')
    result=dict(format=1,freeze_sha256=study.freeze_sha256,session=selected,process=outcome,audit=measured,transport=transport,
                integrity_issues=problems,answer=answer,unverified_answer=measured['answer'],
                transcript_sha256=bare_client.file_hash(out/'transcript.jsonl'),relay_sha256=bare_client.file_hash(out/'relay.json'))
    write(out/'result.json',result);write(out/'answer.json',answer)
    return result


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ['freeze','session','client','knowcode','out']:
        parser.add_argument('--'+name,type=Path,required=True)
    parser.add_argument('--git-toolchain',type=Path)
    args=parser.parse_args()
    # Reading a credential does not launch a request; execute validates the freeze
    # and source before constructing its relay or any client API connection.
    result=execute(args.freeze,args.session,args.client,args.knowcode,args.out,
                   api_key=os.environ.get('ANTHROPIC_API_KEY',''),git_toolchain=args.git_toolchain)
    print(json.dumps(dict(id=result['answer']['id'],status=result['answer']['status'],
                          transport_integrity=result['transport']['transport_integrity'],integrity_issues=result['integrity_issues'])))


if __name__=='__main__':
    main()
