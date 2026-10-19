"""Opt-in full runner integration: synthetic study/source/API, installed executables.

Uses the same native-client opt-ins as test_public_stream; no real provider calls,
corpus indexing, label review or scored evaluation is performed by these tests.
"""
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import http.client
import json
import os
from pathlib import Path
import sys
import tempfile
import threading
import unittest

import frozen_study
import session_runner
import test_public_freeze
from test_public_stream import stream_message


@unittest.skipUnless(sys.platform=='darwin' and os.environ.get('KNOWCODE_NATIVE_CLIENT_TESTS')=='1',
                     'requires explicit installed-client and macOS sandbox opt-in')
class NativeRunnerTests(unittest.TestCase):
    def test_plain_complete_runner_with_scripted_provider(self):
        self.exercise('plain')

    @unittest.skipUnless(os.environ.get('KNOWCODE_NATIVE_MCP_TESTS')=='1','requires native KnowCode opt-in')
    def test_knowcode_complete_runner_with_scripted_provider(self):
        self.exercise('knowcode')

    def exercise(self,arm):
        client=Path(os.environ['KNOWCODE_TEST_CLAUDE']).resolve(strict=True)
        knowcode=Path(os.environ['KNOWCODE_TEST_KNOWCODE']).resolve(strict=True)
        version=os.environ['KNOWCODE_TEST_CLAUDE_VERSION']
        with tempfile.TemporaryDirectory(prefix='knowcode-runner-test-',dir='/private/tmp') as temp:
            base=Path(temp)
            freeze,files,_,source,plan_root=test_public_freeze.FreezeTests().fixture(
                base,client=client,knowcode=knowcode,client_version=version)
            # This synthetic fixture explicitly freezes a finite test deadline.
            specification=json.loads(freeze.read_bytes());specification['process_timeout_seconds']=45
            freeze.write_text(json.dumps(specification))
            order=[json.loads(line) for line in files['order'].read_text().splitlines()]
            selected=next(row for row in order if row['arm']==arm and row['question_id'].startswith('definition-lookup'))
            prepare=frozen_study.load('native_runner_session',frozen_study.ROOT/'scripts/prepare-public-eval-session.py')
            session=base/'session'
            prepare.prepare(files['manifest'],source,plan_root,selected['session'],session)
            source_file=session/'source/example-client/api.ts'
            calls=[dict(type='tool_use',id='fixture_read',name='Read',input=dict(file_path=str(source_file))),
                   dict(type='tool_use',id='fixture_labels',name='Read',input=dict(file_path=str(files['labels'])))]
            if arm=='knowcode':
                calls.append(dict(type='tool_use',id='fixture_context',name='mcp__knowcode__kc_context',
                                  input=dict(symbol='example',source='full')))
            requests=[];errors=[]
            class Handler(BaseHTTPRequestHandler):
                def log_message(self,*_args):pass
                def do_POST(self):
                    try:
                        size=int(self.headers.get('Content-Length','0'))
                        if not 0<size<=16*1024*1024 or self.headers.get('x-api-key')!='synthetic-provider-key':
                            raise ValueError('invalid synthetic request')
                        request=json.loads(self.rfile.read(size));endpoint=self.path.split('?',1)[0]
                        if endpoint=='/v1/messages/count_tokens':
                            output=b'{"input_tokens":110}';kind='application/json'
                        elif endpoint=='/v1/messages':
                            requests.append(request)
                            if len(requests)>3:raise ValueError('unexpected fixture loop')
                            results=[block for message in request.get('messages',[]) if isinstance(message.get('content'),list)
                                     for block in message['content'] if block.get('type')=='tool_result']
                            answer=dict(status='answered',items=['example-client/api.ts:1'],notes='Synthetic integration response.')
                            blocks=[dict(type='text',text=json.dumps(answer))] if results else calls
                            output=stream_message(blocks,2 if results else 1,'example-model');kind='text/event-stream'
                        else:raise ValueError('unexpected synthetic endpoint')
                        self.send_response(200);self.send_header('Content-Type',kind)
                        self.send_header('Content-Length',str(len(output)));self.send_header('Connection','close')
                        self.end_headers();self.wfile.write(output);self.wfile.flush()
                    except Exception as exc:
                        errors.append(type(exc).__name__);self.send_error(400,'Fixture request rejected')
            server=ThreadingHTTPServer(('127.0.0.1',0),Handler);server.daemon_threads=True
            thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
            try:
                result=session_runner.execute(freeze,session,client,knowcode,base/'output',api_key='synthetic-provider-key',
                    git_toolchain=Path(os.environ.get('KNOWCODE_TEST_GIT_TOOLCHAIN','/Library/Developer/CommandLineTools')),
                    _connection_factory=lambda:http.client.HTTPConnection('127.0.0.1',server.server_port,timeout=10))
            finally:
                server.shutdown();server.server_close();thread.join(timeout=5)
            self.assertFalse(thread.is_alive());self.assertEqual(errors,[])
            self.assertEqual(result['process']['returncode'],0,(base/'output/client.stderr').read_text())
            self.assertTrue(result['process']['cleanup_complete']);self.assertFalse(result['process']['timed_out'])
            self.assertEqual(result['integrity_issues'],[])
            self.assertTrue(result['transport']['transport_integrity'],result['transport']['transport_issues'])
            self.assertEqual(result['answer']['items'],['example-client/api.ts:1'])
            self.assertEqual(result['answer']['status'],'answered');self.assertEqual(len(requests),2)
            results={b['tool_use_id']:b for m in requests[-1]['messages'] if isinstance(m.get('content'),list)
                     for b in m['content'] if b.get('type')=='tool_result'}
            self.assertFalse(results['fixture_read'].get('is_error',False))
            self.assertTrue(results['fixture_labels']['is_error'])
            self.assertEqual(result['audit']['refused_calls'],1)
            self.assertEqual(result['audit']['research_calls'],3 if arm=='knowcode' else 2)
            if arm=='knowcode':
                self.assertFalse(results['fixture_context'].get('is_error',False),results['fixture_context'])
                body=results['fixture_context']['content']
                context=json.loads(body if isinstance(body,str) else ''.join(b['text'] for b in body if b['type']=='text'))
                self.assertTrue(context['complete']);self.assertFalse(context['truncated'])
                self.assertEqual(context['items'][0]['source'],source_file.read_text().strip())
                self.assertEqual(context['items'][0]['symbol']['file'],'api.ts')
                advertised={tool['name'] for tool in requests[0]['tools']}
                self.assertIn('mcp__knowcode__kc_context',advertised)
                self.assertNotIn('mcp__knowcode__kc_note_add',advertised)
                self.assertEqual(json.loads((base/'output/index-process.json').read_bytes())['returncode'],0)
                self.assertTrue(list((session/'control/kc-cache/repos').glob('*.sqlite')))
                self.assertFalse(result['audit']['policy_flags'])
            else:self.assertFalse((base/'output/index-process.json').exists())
            self.assertNotIn('synthetic-provider-key',(base/'output/relay.json').read_text())
            self.assertFalse((base/'output/failure.json').exists())


if __name__=='__main__':unittest.main()
