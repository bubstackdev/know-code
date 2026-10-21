"""Synthetic transcripts exercise accounting, conversion and damaged-run behavior."""
import copy
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location('public_transcript', Path(__file__).with_name('public_transcript.py'))
audit = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(audit)


def assistant(message_id, content, context=100, output=2):
    usage = {'input_tokens':context,'cache_read_input_tokens':10,'cache_creation_input_tokens':5,'output_tokens':output}
    return dict(type='assistant',session_id='session-example',message=dict(id=message_id,content=content,usage=usage))


def call(call_id='call_1', name='Read', **inp):
    return dict(type='tool_use',id=call_id,name=name,input=inp or {'file_path':'/study/source/example/app.ts'})


def tool_result(call_id='call_1', **extra):
    return dict(type='user',session_id='session-example',message=dict(content=[dict(type='tool_result',tool_use_id=call_id,content='source text',**extra)]))


def result(answer=None, **extra):
    answer = answer if answer is not None else dict(status='answered',items=['example/app.ts:8'],notes='source evidence')
    return dict(type='result',session_id='session-example',subtype='success',is_error=False,
                result=json.dumps(answer),usage={'input_tokens':300,'output_tokens':27,'cache_read_input_tokens':10},
                total_cost_usd=0.01,permission_denials=[],**extra)


class TranscriptTests(unittest.TestCase):
    def run_audit(self, events, **extra):
        config=dict(question_id='public-example-001',arm='plain',research_call_budget=20,source_root='/study/source')
        config.update(extra)
        return audit.audit(events,**config)

    def valid(self):
        return [assistant('message_1',[call()]),tool_result(),assistant('message_2',[dict(type='text',text='answer')],context=180,output=7),result()]

    def test_repeated_blocks_do_not_inflate_calls_or_contexts(self):
        events=self.valid();events.insert(1,copy.deepcopy(events[0]))
        events.insert(2,assistant('message_1',[dict(type='text',text='continuing')],output=5))
        out=self.run_audit(events)
        self.assertTrue(out['transcript_integrity'])
        self.assertEqual(out['research_calls'],1)
        self.assertEqual(out['unique_assistant_messages'],2)
        self.assertEqual(out['fixed_overhead_context'],115)
        self.assertEqual(out['research_context_tokens'],80)
        self.assertEqual(out['processed_context_tokens'],310)
        self.assertEqual(out['observed_turns'][0]['observed_output_tokens'],5)
        self.assertEqual(out['terminal_usage']['output_tokens'],27)
        self.assertFalse(out['runtime_confinement_verified'])

    def test_conflicting_repeated_id_is_an_integrity_error(self):
        events=self.valid();events.insert(1,assistant('message_1',[call(file_path='/study/source/example/other.ts')]))
        out=self.run_audit(events)
        self.assertIn('conflicting repeated tool id',out['transcript_issues'])
        self.assertEqual(out['answer']['status'],'error')
        self.assertEqual(out['research_calls'],1)

    def test_call_budget_counts_attempts_including_denials(self):
        events=[assistant('message_1',[call()]),tool_result(is_error=True),assistant('message_2',[call('call_2','Bash',command='cat example/app.ts')]),tool_result('call_2'),result()]
        events[-1]['permission_denials']=[{'tool_use_id':'call_1','tool_name':'Read'}]
        out=self.run_audit(events,research_call_budget=1)
        self.assertEqual(out['research_calls'],2)
        self.assertEqual(out['research_call_overrun'],1)
        self.assertEqual(out['refused_calls'],1)
        self.assertFalse(out['research_call_budget_enforced'])

    def test_refusal_text_and_terminal_record_count_the_same_call_once(self):
        events=self.valid();events[1]['message']['content'][0].update(is_error=True,content='Permission denied by dontAsk')
        events[-1]['permission_denials']=[{'tool_use_id':'call_1'}]
        out=self.run_audit(events)
        self.assertEqual(out['refused_calls'],1)
        self.assertTrue(out['calls'][0]['permission_refused'])
        self.assertEqual(out['heuristic_refusal_calls'],1)
        self.assertEqual(out['permission_refusal_review_required'],[])
        events[-1]['permission_denials']=[]
        out=self.run_audit(events)
        self.assertEqual(out['refused_calls'],0)
        self.assertEqual(out['heuristic_refusal_calls'],1)
        self.assertEqual(out['permission_refusal_review_required'],['call_1'])

    def test_forbidden_tools_are_visible_without_hiding_an_answer(self):
        events=self.valid();events[0]['message']['content'][0]['name']='mcp__knowcode__kc_context'
        plain=self.run_audit(events)
        self.assertEqual(plain['knowcode_calls'],1)
        self.assertEqual(plain['plain_calls'],0)
        self.assertTrue(plain['policy_flags'])
        self.assertEqual(plain['answer']['status'],'answered')
        allowed=self.run_audit(events,arm='knowcode')
        self.assertEqual(allowed['policy_flags'],[])
        events[0]['message']['content'][0]['name']='mcp__knowcode__kc_note_add'
        self.assertIn('persistent note tool forbidden',self.run_audit(events,arm='knowcode')['policy_flags'])

    def test_missing_or_multiple_terminals_cannot_become_answered_empty(self):
        for events in [self.valid()[:-1],self.valid()+[result()],[]]:
            out=self.run_audit(events)
            self.assertFalse(out['transcript_integrity'])
            self.assertEqual(out['answer']['status'],'error')
        events=self.valid();events[-1]['is_error']=True
        out=self.run_audit(events)
        self.assertEqual(out['answer']['status'],'error')
        self.assertTrue(out['answer_issues'])

    def test_malformed_lines_duplicate_keys_and_nonfinite_json_are_recorded(self):
        data=b'{"type":"system","type":"assistant"}\n{bad\n{"type":"result","cost":NaN}\n'
        events,issues=audit.read_events(data)
        self.assertEqual(len(issues),3)
        out=self.run_audit(events,parse_issues=issues)
        self.assertEqual(out['answer']['status'],'error')
        self.assertFalse(out['transcript_integrity'])
        self.assertTrue(audit.read_events(b'\xff')[1])

    def test_answer_text_is_not_salvaged_from_extra_prose(self):
        for text in ['before {"status":"answered","items":[]} after','{"status":"answered","items":[],"items":[]}', '[1,2]']:
            events=self.valid();events[-1]['result']=text
            self.assertEqual(self.run_audit(events)['answer']['status'],'error')
        events=self.valid();events[-1]['result']='```json\n'+events[-1]['result']+'\n```'
        self.assertEqual(self.run_audit(events)['answer']['status'],'answered')

    def test_prose_before_the_last_json_block_is_allowed(self):
        # Synthetic shapes of the pilot's format failures: prose, then a fenced answer.
        answer=dict(status='answered',items=['example/app.ts:8'],notes='n')
        events=self.valid()
        events[-1]['result']='I found one caller.\n\n```json\n'+json.dumps(answer)+'\n```\nDone.'
        out=self.run_audit(events)
        self.assertEqual((out['answer']['status'],out['answer']['items']),('answered',['example/app.ts:8']))
        draft=dict(status='error',items=[],notes='draft')
        events=self.valid()
        events[-1]['result']='Draft:\n```json\n'+json.dumps(draft)+'\n```\nFinal:\n```json\n'+json.dumps(answer)+'\n```'
        self.assertEqual(self.run_audit(events)['answer']['items'],['example/app.ts:8'])
        events=self.valid();events[-1]['result']='Answer:\n```json\n{"status": "answered", "items": [\n```'
        self.assertEqual(self.run_audit(events)['answer_issues'],['answer is not one complete JSON object'])

    def test_grouped_items_fail_and_kinded_items_are_flattened(self):
        grouped=dict(status='answered',items={'declarations':['example/app.ts:8'],'registrations':[]})
        events=self.valid();events[-1]['result']='```json\n'+json.dumps(grouped)+'\n```'
        out=self.run_audit(events)
        self.assertEqual((out['answer']['status'],out['answer_issues']),('error',['invalid answer contract']))
        kinded=dict(status='answered',items=[{'location':'example/app.ts:8','kind':'declaration'},
                                             {'location':'example/di.ts:3','kind':'registration'},'example/x.ts:1'])
        events=self.valid();events[-1]['result']='Found two.\n```json\n'+json.dumps(kinded)+'\n```'
        out=self.run_audit(events)
        self.assertEqual(out['answer']['items'],['example/app.ts:8','example/di.ts:3','example/x.ts:1'])
        self.assertEqual(out['answer']['item_kinds'],['declaration','registration',None])
        for bad in [{'location':'example/app.ts:8','role':'x'},{'kind':'declaration'},{'location':3}]:
            events=self.valid();events[-1]['result']=json.dumps(dict(status='answered',items=[bad]))
            self.assertEqual(self.run_audit(events)['answer']['status'],'error')

    def test_unknown_paths_and_duplicate_items_are_preserved_for_scoring(self):
        events=self.valid();events[-1]=result(dict(status='answered',items=['/study/source/example/app.ts:8','example/app.ts:8','/outside/wrong.ts:1','example/../wrong.ts'],notes='test'))
        out=self.run_audit(events)
        self.assertEqual(out['answer']['items'],['example/app.ts:8','example/app.ts:8','/outside/wrong.ts:1','example/../wrong.ts'])
        self.assertTrue(out['answer_issues'])

    def test_hop_errors_remain_distinct_from_union_scoring(self):
        events=self.valid();events[-1]=result(dict(status='answered',items=['example/app.ts:8'],hops={'1':['example/app.ts:8'],'2':['example/app.ts:8','example/other.ts:9']}))
        out=self.run_audit(events,two_hop=True)
        self.assertEqual(out['answer']['status'],'answered')
        self.assertIn('answer item union differs from hop assignment',out['answer_issues'])
        self.assertIn('same source location assigned to both shortest distances',out['answer_issues'])

    def test_compaction_does_not_masquerade_as_negative_research_tokens(self):
        events=self.valid();events.insert(2,dict(type='system',session_id='session-example',subtype='compact_boundary'))
        events[3]['message']['usage']['input_tokens']=30
        out=self.run_audit(events)
        self.assertEqual(out['observed_context_delta'],-70)
        self.assertIsNone(out['research_context_tokens'])

    def test_missing_usage_is_unknown_and_invalid_counts_are_errors(self):
        events=self.valid();events[0]['message'].pop('usage');events[-1].pop('usage')
        out=self.run_audit(events)
        self.assertIsNone(out['research_context_tokens'])
        self.assertIsNone(out['processed_context_tokens'])
        self.assertIsNone(out['terminal_usage'])
        events=self.valid();events[0]['message']['usage']['input_tokens']=True
        self.assertFalse(self.run_audit(events)['transcript_integrity'])

    def test_missing_and_orphan_results_and_mixed_sessions_are_errors(self):
        variants=[self.valid(),self.valid(),self.valid()]
        variants[0].pop(1)
        variants[1].insert(2,tool_result('not_a_call'))
        variants[2][1]['session_id']='different-session'
        for events in variants:self.assertFalse(self.run_audit(events)['transcript_integrity'])

    def test_subagent_and_extra_terminal_events_are_reported(self):
        events=self.valid();events[0]['parent_tool_use_id']='parent'
        self.assertIn('subagent event outside study interface',self.run_audit(events)['policy_flags'])
        events.append(dict(type='system',session_id='session-example',subtype='metadata'))
        self.assertFalse(self.run_audit(events)['transcript_integrity'])

    def test_unknown_events_and_server_tools_need_review_and_refusals_are_separate(self):
        events=self.valid();events.insert(1,dict(type='future_event',session_id='session-example'))
        self.assertIn('unrecognized stream event: future_event',self.run_audit(events)['policy_flags'])
        events=self.valid();events[0]['message']['content'].append(dict(type='server_tool_use',id='server_call'))
        self.assertFalse(self.run_audit(events)['transcript_integrity'])
        events=self.valid();events[-1]['stop_reason']='refusal'
        out=self.run_audit(events)
        self.assertTrue(out['model_refusal_observed'])
        self.assertEqual(out['refused_calls'],0)
        self.assertEqual(out['terminal_output_tokens'],27)
        self.assertFalse(self.run_audit([result()])['transcript_integrity'])

    def test_cli_binds_the_transcript_and_scheduled_question_without_running_tools(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            transcript=root/'transcript.jsonl'
            transcript.write_text(''.join(json.dumps(v)+'\n' for v in self.valid()))
            receipt=root/'session.json'
            receipt.write_text(json.dumps(dict(question={'id':'public-example-001'},session={'question_id':'public-example-001','arm':'plain'})))
            command=[sys.executable,'-B',str(Path(audit.__file__)),'--transcript',str(transcript),'--session',str(receipt),'--research-call-budget','20']
            out=json.loads(subprocess.check_output(command))
            self.assertTrue(out['transcript_integrity'])
            self.assertEqual(out['answer']['id'],'public-example-001')
            self.assertEqual(len(out['transcript_sha256']),64)
            self.assertFalse(out['ready_to_run'])

    def transport_fixture(self):
        events=[dict(type='system',subtype='init',session_id='session-example',permissionMode='dontAsk')]+self.valid()
        measured=self.run_audit(events)
        records=[dict(endpoint='/v1/messages',model='example-model',completed=True,response_status=200,
                      response_message_id='message_'+str(n)) for n in (1,2)]
        return events,measured,records

    def bind(self,events,measured,records,**changes):
        options=dict(expected_model='example-model',relay_cleanup_complete=True)
        options.update(changes)
        return audit.audit_transport(measured,events,records,**options)

    def test_transport_binding_retains_failed_retries_and_separates_token_counts(self):
        events,measured,records=self.transport_fixture()
        retry=dict(endpoint='/v1/messages',model='example-model',completed=True,response_status=429,response_message_id=None)
        counter=dict(retry,endpoint='/v1/messages/count_tokens',response_status=200)
        out=self.bind(events,measured,[retry,counter]+records)
        self.assertTrue(out['transport_integrity'])
        self.assertEqual(out['model_request_attempts'],3)
        self.assertEqual(out['token_count_attempts'],1)
        self.assertEqual(len(out['non_success_responses']),1)
        self.assertEqual(out['answer']['status'],'answered')
        self.assertFalse(out['runtime_confinement_verified'])

    def test_abandoned_attempts_are_reported_not_failed(self):
        # The transport-retry rule (protocol.md): a broken or statusless response no client message came from.
        events,measured,records=self.transport_fixture()
        broken=dict(endpoint='/v1/messages',model='example-model',completed=False,response_status=200,
                    response_message_id='message_abandoned',error_type='BrokenPipeError')
        reset=dict(endpoint='/v1/messages',model='example-model',completed=False,response_status=None,
                   response_message_id=None,error_type='ConnectionResetError')
        out=self.bind(events,measured,[records[0],broken,reset,records[1]])
        self.assertTrue(out['transport_integrity'],out['transport_issues'])
        self.assertEqual([a['ordinal'] for a in out['abandoned_attempts']],[2,3])
        self.assertEqual(out['answer']['status'],'answered')

    def test_transport_rule_failures(self):
        events,measured,records=self.transport_fixture()
        abandoned=dict(endpoint='/v1/messages',model='example-model',completed=False,response_status=None,response_message_id=None)
        cases={
            'missing response':[records[0],abandoned],
            'client message only partly received':[records[0],dict(records[1],completed=False)],
            'reordered':[records[1],abandoned,records[0]],
            'repeated':[records[0],records[1],records[1]],
            'model differs, even on an abandoned attempt':records+[dict(abandoned,model='other-model')],
        }
        for name,candidate in cases.items():
            with self.subTest(name):
                out=self.bind(events,measured,candidate)
                self.assertFalse(out['transport_integrity'])
                self.assertEqual(out['answer']['status'],'error')
        changed=copy.deepcopy(events);changed[0]['permissionMode']='default'
        self.assertFalse(self.bind(changed,measured,records+[abandoned])['transport_integrity'])
        self.assertFalse(self.bind(events,measured,records+[abandoned],relay_cleanup_complete=False)['transport_integrity'])

    def test_unmatched_reordered_and_duplicate_responses_cannot_silently_score(self):
        events,measured,records=self.transport_fixture()
        variants=[records[:1],records+[dict(records[0],response_message_id='hidden_message')],
                  records+[records[0]],list(reversed(records))]
        for candidate in variants:
            out=self.bind(events,measured,candidate)
            self.assertFalse(out['transport_integrity'])
            self.assertEqual(out['answer']['status'],'error')
        self.assertEqual(measured['answer']['status'],'answered')

    def test_changed_missing_or_repeated_permission_mode_and_cleanup_failure(self):
        events,measured,records=self.transport_fixture()
        changed=copy.deepcopy(events);changed[0]['permissionMode']='default'
        for candidate in [changed,events[1:],[events[0]]+events]:
            self.assertFalse(self.bind(candidate,measured,records)['transport_integrity'])
        self.assertFalse(self.bind(events,measured,records,relay_cleanup_complete=False)['transport_integrity'])

    def test_malformed_partial_and_wrong_model_relay_records_are_visible(self):
        events,measured,records=self.transport_fixture()
        for updates in [dict(model='other-model'),dict(completed=False),dict(response_status=True),dict(response_message_id=None)]:
            candidate=copy.deepcopy(records);candidate[0].update(updates)
            self.assertFalse(self.bind(events,measured,candidate)['transport_integrity'])
        for candidate in [None,[{}],['not a record']]:
            self.assertFalse(self.bind(events,measured,candidate)['transport_integrity'])
        with self.assertRaises(ValueError):self.bind(events,measured,records,expected_model='')

    def test_cli_preserves_raw_answer_when_transport_binding_fails(self):
        events,measured,records=self.transport_fixture()
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            (root/'transcript.jsonl').write_text(''.join(json.dumps(e)+'\n' for e in events))
            (root/'session.json').write_text(json.dumps(dict(question={'id':'public-example-001'},session={'question_id':'public-example-001','arm':'plain'})))
            (root/'relay.json').write_text(json.dumps(dict(records=records[:1],cleanup_complete=True)))
            command=[sys.executable,'-B',str(Path(audit.__file__)),'--transcript',str(root/'transcript.jsonl'),
                     '--session',str(root/'session.json'),'--research-call-budget','20',
                     '--relay-records',str(root/'relay.json'),'--expected-model','example-model']
            out=json.loads(subprocess.check_output(command))
            self.assertEqual(out['answer']['status'],'error')
            self.assertEqual(out['unverified_answer']['status'],'answered')
            self.assertFalse(out['transport']['transport_integrity'])
            self.assertEqual(len(out['relay_records_sha256']),64)

    def test_cli_rejects_half_configured_transport_audit_before_reading_inputs(self):
        command=[sys.executable,'-B',str(Path(audit.__file__)),'--transcript','missing-transcript',
                 '--session','missing-session','--research-call-budget','20']
        for extra in [['--expected-model','example-model'],['--relay-records','missing-relay']]:
            result=subprocess.run(command+extra,capture_output=True,text=True)
            self.assertEqual(result.returncode,2)
            self.assertIn('must be supplied together',result.stderr)
            self.assertEqual(result.stdout,'')


if __name__=='__main__':
    unittest.main()
