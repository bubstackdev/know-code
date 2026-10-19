"""Synthetic study freezes exercise gates without indexing or provider requests."""
import copy
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import frozen_study as frozen
import session_runner as runner
import test_public_plan
planner=test_public_plan.planner


class FreezeTests(unittest.TestCase):
    def fixture(self, base, *, client=None, knowcode=None, client_version='1'):
        evidence=base/'evidence';evidence.mkdir()
        manifest,source,questions,_=test_public_plan.PublicPlanTests().fixture(evidence)
        records=[]
        for stratum,(kind,answer) in planner.TASKS.items():
            for i in range(6):
                records.append(dict(id=f'{stratum}-{i}',stratum=stratum,source='structural',type=kind,
                                    question=f'Find synthetic subject {stratum} {i}.',repos=['example-client'],
                                    commit={'example-client':'a'*40},index_state={'example-client':'a'*40},answer_kind=answer))
        questions.write_text(''.join(json.dumps(q,sort_keys=True)+'\n' for q in sorted(records,key=lambda q:q['id'])))
        binary=client
        if binary is None:
            binary=base/'client';binary.write_text('#!/bin/sh\nexit 0\n');binary.chmod(0o700)
        knowcode=knowcode or binary
        config=evidence/'config/run.json';config.parent.mkdir()
        config.write_text(json.dumps(dict(model='example-model',effort='medium',client='claude-code',client_version=client_version,
                                         research_call_budget=20,knowcode_sha256=frozen.digest(knowcode.read_bytes()),knowcode_profile='core')))
        plan_root=base/'plan'
        planner.plan(manifest,source,questions,plan_root,config_path=config)
        files=dict(manifest=manifest,source_receipt=source/'source-receipt.json',plan=plan_root/'plan.json',
                   questions=questions,order=plan_root/'order.jsonl',config=config)
        def put(name,value,lines=False):
            path=evidence/(name+('.jsonl' if lines else '.json'))
            path.write_text(''.join(json.dumps(r)+'\n' for r in value) if lines else json.dumps(value))
            files[name]=path
        labels=[dict(id=q['id'],items=[dict(item='example-client/api.ts:1',evidence='example-client/api.ts:1')],
                     completeness='lower_bound',match='file_line') for q in records]
        put('labels',labels,True)
        put('scope',{'fixture':True});put('candidates',[dict(id='candidate-'+q['id']) for q in records],True)
        put('provisional',{'fixture':True})
        put('sampling',{k+'_sha256':frozen.digest(files[k].read_bytes()) for k in ['scope','candidates','provisional']})
        put('subject_review',dict(sampling_sha256=frozen.digest(files['sampling'].read_bytes()),
                                 accepted=[dict(candidate_id='candidate-'+q['id'],decision='accept') for q in records]))
        put('question_subjects',[dict(id=q['id'],candidate_id='candidate-'+q['id']) for q in records])
        # These attestations are test data only, never a review of the real cohort.
        put('review',dict(status='complete',reviewer='synthetic-fixture-reviewer',source_only=True,
                          primary_labels_read=False,compared_tool_outputs_read=False,
                          questions_sha256=frozen.digest(questions.read_bytes()),labels=labels,
                          adjudications=[dict(id=l['id'],independent_label_sha256=frozen.canonical(l),
                                              final_label_sha256=frozen.canonical(l),resolution='Synthetic fixture only.') for l in labels]))
        put('method',{'fixture':True});put('sampling_rules',{'fixture':True})
        spec=dict(format='knowcode-public-v1-freeze-1',client_sha256=frozen.digest(binary.read_bytes()),
                  files={k:dict(path=str(p),sha256=frozen.digest(p.read_bytes())) for k,p in files.items()},
                  producers={p:frozen.digest((frozen.ROOT/p).read_bytes()) for p in frozen.PRODUCERS})
        freeze=evidence/'freeze.json';freeze.write_text(json.dumps(spec))
        return freeze,files,binary,source,plan_root

    def rebind(self, freeze, files, name, value, lines=False):
        files[name].write_text(''.join(json.dumps(r)+'\n' for r in value) if lines else json.dumps(value))
        spec=json.loads(freeze.read_bytes());spec['files'][name]['sha256']=frozen.digest(files[name].read_bytes())
        freeze.write_text(json.dumps(spec))

    def test_complete_synthetic_freeze_is_valid_and_detects_later_mutation(self):
        with tempfile.TemporaryDirectory() as temp:
            freeze,files,binary,_,_=self.fixture(Path(temp))
            study=frozen.FrozenStudy(freeze);study.binaries(binary,binary)
            self.assertEqual(len(study.questions),24);self.assertEqual(len(study.order),48)
            files['method'].write_text('changed')
            with self.assertRaisesRegex(ValueError,'changed'):
                study.unchanged()

    def test_missing_labels_or_unfinished_review_cannot_launch_anything(self):
        for missing in ['labels','review']:
            with self.subTest(missing=missing),tempfile.TemporaryDirectory() as temp:
                base=Path(temp);freeze,files,binary,_,_=self.fixture(base)
                value=[] if missing=='labels' else dict(status='not started')
                self.rebind(freeze,files,missing,value,lines=missing=='labels')
                with patch.object(runner,'ModelRelay') as relay,patch.object(runner.owned_process,'run') as process:
                    with self.assertRaises(ValueError):
                        runner.execute(freeze,base/'absent-session',binary,binary,base/'out',api_key='synthetic-key')
                    relay.assert_not_called();process.assert_not_called()
                self.assertFalse((base/'out').exists())

    def test_rehashed_schedule_edit_is_rejected(self):
        with tempfile.TemporaryDirectory() as temp:
            freeze,files,_,_,_=self.fixture(Path(temp))
            rows=[json.loads(s) for s in files['order'].read_text().splitlines()]
            rows.reverse();self.rebind(freeze,files,'order',rows,True)
            plan=json.loads(files['plan'].read_bytes());plan['order_sha256']=frozen.digest(files['order'].read_bytes())
            self.rebind(freeze,files,'plan',plan)
            with self.assertRaisesRegex(ValueError,'schedule'):
                frozen.FrozenStudy(freeze)

    def test_duplicate_review_and_adjudication_mismatch_rejected(self):
        for mutation in ['duplicate','binding','unblind']:
            with self.subTest(mutation=mutation),tempfile.TemporaryDirectory() as temp:
                freeze,files,_,_,_=self.fixture(Path(temp));review=json.loads(files['review'].read_bytes())
                if mutation=='duplicate':review['labels'].append(copy.deepcopy(review['labels'][0]))
                elif mutation=='binding':
                    for decision in review['adjudications']:decision['final_label_sha256']='0'*64
                else:review['primary_labels_read']=True
                self.rebind(freeze,files,'review',review)
                with self.assertRaises(ValueError):frozen.FrozenStudy(freeze)

    def test_changed_implementation_or_duplicate_json_keys_rejected(self):
        with tempfile.TemporaryDirectory() as temp:
            freeze,_,_,_,_=self.fixture(Path(temp));spec=json.loads(freeze.read_bytes())
            spec['producers']['scripts/eval/session_runner.py']='0'*64;freeze.write_text(json.dumps(spec))
            with self.assertRaisesRegex(ValueError,'implementation differs'):frozen.FrozenStudy(freeze)
            freeze.write_text('{"format":1,"format":2}')
            with self.assertRaises(ValueError):frozen.FrozenStudy(freeze)

    def test_version_failure_retained_and_provider_never_started(self):
        with tempfile.TemporaryDirectory() as temp:
            base=Path(temp);freeze,files,binary,source,plan_root=self.fixture(base)
            order=[json.loads(s) for s in files['order'].read_text().splitlines()]
            selected=next(row for row in order if row['arm']=='plain')
            preparer=frozen.load('freeze_test_session',frozen.ROOT/'scripts/prepare-public-eval-session.py')
            session=base/'session';preparer.prepare(files['manifest'],source,plan_root,selected['session'],session)
            def failed_version(*args,**kwargs):
                kwargs['stdout'].write_text('wrong version\n');kwargs['stderr'].touch()
                return dict(returncode=0,cleanup_complete=True)
            with patch.object(runner.owned_process,'run',side_effect=failed_version),patch.object(runner,'ModelRelay') as relay:
                with self.assertRaisesRegex(ValueError,'client version'):
                    runner.execute(freeze,session,binary,binary,base/'output',api_key='synthetic-key')
                relay.assert_not_called()
            failure=json.loads((base/'output/failure.json').read_bytes())
            self.assertEqual(failure['exception_type'],'ValueError')
            self.assertEqual(json.loads((base/'output/answer.json').read_bytes())['status'],'error')

    def test_session_composition_and_relay_cleanup_on_client_exception(self):
        for client_raises in [False,True]:
            with self.subTest(client_raises=client_raises),tempfile.TemporaryDirectory() as temp:
                base=Path(temp);freeze,files,binary,source,plan_root=self.fixture(base)
                selected=next(json.loads(s) for s in files['order'].read_text().splitlines() if json.loads(s)['arm']=='plain')
                preparer=frozen.load('composition_session',frozen.ROOT/'scripts/prepare-public-eval-session.py')
                session=base/'session';preparer.prepare(files['manifest'],source,plan_root,selected['session'],session)
                class SyntheticRelay:
                    base_url='http://127.0.0.1:12345'
                    client_token='synthetic-client-token'
                    closed=False
                    def close(self):self.closed=True;return True
                    def records(self):
                        return [] if client_raises else [dict(endpoint='/v1/messages',model='example-model',completed=True,
                                                              response_status=200,response_message_id='fixture-message')]
                relay=SyntheticRelay()
                class SyntheticPolicy:
                    parameters={};profile=''
                    def command(self,argv):return argv
                    def receipt(self):return {'synthetic':True}
                def process(argv,**kwargs):
                    if '--version' in argv:
                        kwargs['stdout'].write_text('1 (Claude Code)\n');kwargs['stderr'].touch()
                    else:
                        self.assertEqual(kwargs['env']['ANTHROPIC_API_KEY'],relay.client_token)
                        self.assertNotIn('synthetic-parent-key',json.dumps(kwargs['env']))
                        self.assertIsNone(kwargs['timeout'])
                        if client_raises:raise RuntimeError('synthetic-parent-key')
                        events=[dict(type='system',subtype='init',permissionMode='dontAsk'),
                                dict(type='assistant',message=dict(id='fixture-message',content=[dict(type='text',text='source evidence')],
                                                                   usage=dict(input_tokens=100,output_tokens=10))),
                                dict(type='result',subtype='success',is_error=False,permission_denials=[],
                                     result=json.dumps(dict(status='answered',items=['example-client/api.ts:1'],notes='fixture')))]
                        for event in events:event['session_id']='synthetic-session'
                        kwargs['stdout'].write_text(''.join(json.dumps(e)+'\n' for e in events));kwargs['stderr'].touch()
                    return dict(returncode=0,cleanup_complete=True)
                with patch.object(runner,'ModelRelay',return_value=relay),patch.object(runner,'policy_for',return_value=SyntheticPolicy()),patch.object(runner.owned_process,'run',side_effect=process):
                    if client_raises:
                        with self.assertRaises(RuntimeError):runner.execute(freeze,session,binary,binary,base/'output',api_key='synthetic-parent-key')
                        self.assertNotIn('synthetic-parent-key',(base/'output/failure.json').read_text())
                    else:
                        result=runner.execute(freeze,session,binary,binary,base/'output',api_key='synthetic-parent-key')
                        self.assertEqual(result['answer']['status'],'answered',result)
                        self.assertTrue(result['transport']['transport_integrity'])
                        self.assertEqual(result['integrity_issues'],[])
                self.assertTrue(relay.closed)
                self.assertTrue(json.loads((base/'output/relay.json').read_bytes())['cleanup_complete'])


if __name__=='__main__':
    unittest.main()
