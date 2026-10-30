"""Opt-in installed-client conformance using a scripted loopback API, never a model.

Set KNOWCODE_NATIVE_CLIENT_TESTS=1, KNOWCODE_TEST_CLAUDE to the installed executable,
and KNOWCODE_TEST_CLAUDE_VERSION to its recorded version. Requires native macOS
sandbox-exec permission. Dummy credentials only; the sandbox permits outbound
traffic solely to the fixture's owned localhost port.

For the MCP case also set KNOWCODE_NATIVE_MCP_TESTS=1 and KNOWCODE_TEST_KNOWCODE.
KNOWCODE_TEST_GIT_TOOLCHAIN optionally selects the Apple developer-tool directory
(default /Library/Developer/CommandLineTools). No manual graph indexing occurs.
"""
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import http.client
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import threading
import unittest

import bare_client
import macos_sandbox
import owned_process
import public_transcript
from model_relay import ModelRelay


def stream_message(blocks, number, model):
    usage = dict(input_tokens=100+number*10, output_tokens=1, cache_creation_input_tokens=0, cache_read_input_tokens=0)
    events = [('message_start', dict(type='message_start', message=dict(
        id='msg_fixture_'+str(number), type='message', role='assistant', content=[], model=model,
        stop_reason=None, stop_sequence=None, usage=usage)))]
    for index, block in enumerate(blocks):
        initial = dict(block)
        if block['type'] == 'tool_use':
            initial['input'] = {}
            delta = dict(type='input_json_delta', partial_json=json.dumps(block['input']))
        else:
            initial['text'] = ''
            delta = dict(type='text_delta', text=block['text'])
        events.extend([
            ('content_block_start', dict(type='content_block_start', index=index, content_block=initial)),
            ('content_block_delta', dict(type='content_block_delta', index=index, delta=delta)),
            ('content_block_stop', dict(type='content_block_stop', index=index)),
        ])
    events.extend([
        ('message_delta', dict(type='message_delta', delta=dict(
            stop_reason='tool_use' if number == 1 else 'end_turn', stop_sequence=None), usage=dict(output_tokens=20))),
        ('message_stop', dict(type='message_stop')),
    ])
    return ''.join('event: '+kind+'\ndata: '+json.dumps(body)+'\n\n' for kind, body in events).encode()


@unittest.skipUnless(sys.platform == 'darwin' and os.environ.get('KNOWCODE_NATIVE_CLIENT_TESTS') == '1',
                     'requires explicit native macOS installed-client opt-in')
class NativeStreamTests(unittest.TestCase):
    def test_real_stream_and_permissions_against_scripted_local_api(self):
        self.exercise('plain')

    def test_real_stream_via_credential_isolating_relay(self):
        self.exercise('plain', relay_transport=True)

    @unittest.skipUnless(os.environ.get('KNOWCODE_NATIVE_MCP_TESTS') == '1', 'requires explicit native KnowCode MCP opt-in')
    def test_real_mcp_discovery_first_query_and_unavailable_note_write(self):
        self.exercise('knowcode')

    @unittest.skipUnless(os.environ.get('KNOWCODE_NATIVE_MCP_TESTS') == '1', 'requires explicit native KnowCode MCP opt-in')
    def test_real_mcp_via_credential_isolating_relay(self):
        self.exercise('knowcode', relay_transport=True)

    def exercise(self, arm, relay_transport=False):
        client = Path(os.environ['KNOWCODE_TEST_CLAUDE']).resolve(strict=True)
        expected_version = os.environ['KNOWCODE_TEST_CLAUDE_VERSION']
        self.assertRegex(expected_version, r'^\d+\.\d+\.\d+$')
        with tempfile.TemporaryDirectory(prefix='knowcode-stream-test-', dir='/private/tmp') as temp:
            root = Path(temp)
            session = root/'session'
            for name in ['source/example', 'control', 'profile']:
                (session/name).mkdir(parents=True, exist_ok=True)
            source = session/'source/example/app.ts'
            source_text = 'export function counter() { return 7; }\n' if arm == 'knowcode' else 'export const counter = 7;\n'
            source.write_text(source_text)
            outside = root/'outside.txt'
            outside.write_text('outside-source-fixture-marker\n')
            side_effect = root/'should-not-exist'
            question = dict(id='public-fixture-001', question='Find counter in the example repository.',
                            commit={'example':'a'*40}, stratum='definition-lookup')
            receipt = dict(source_root='source', required_client_cwd='control', empty_profile_directory='profile',
                           session=dict(arm=arm, question_id=question['id']), question=question)
            (session/'session.json').write_text(json.dumps(receipt))
            config = dict(client='claude-code', client_version=expected_version, model='claude-opus-5-5',
                          effort='medium', research_call_budget=20)
            dependencies = []
            extra_executables = []
            preparation = {}
            if arm == 'knowcode':
                binary = Path(os.environ['KNOWCODE_TEST_KNOWCODE']).resolve(strict=True)
                toolchain = Path(os.environ.get('KNOWCODE_TEST_GIT_TOOLCHAIN', '/Library/Developer/CommandLineTools')).resolve(strict=True)
                real_git = toolchain/'usr/bin/git'
                shim_library = toolchain/'usr/lib/libxcrun.dylib'
                self.assertTrue(real_git.is_file())
                self.assertTrue(shim_library.is_file())
                index = session/'control/index/example'
                index.mkdir(parents=True)
                shutil.copyfile(source, index/'app.ts')
                git_env = dict(PATH='/usr/bin:/bin', LC_ALL='C', GIT_CONFIG_NOSYSTEM='1',
                               GIT_CONFIG_GLOBAL='/dev/null', GIT_CONFIG_SYSTEM='/dev/null', GIT_TERMINAL_PROMPT='0')
                subprocess.run(['/usr/bin/git', 'init', '--quiet', '--initial-branch=public-eval', '--template=', str(index)],
                               env=git_env, check=True, capture_output=True, timeout=15)
                config.update(knowcode_profile='all', knowcode_sha256=bare_client.file_hash(binary))
                preparation = dict(knowcode=binary, index_repos={'example':index})
                extra_executables = [binary, Path('/usr/bin/git'), real_git]
                dependencies = [shim_library, toolchain]
                self.assertFalse((session/'control/kc-cache').exists())
            plan = bare_client.prepare(session, config, client, **preparation)
            calls = [
                dict(type='tool_use', id='fixture_read', name='Read', input={'file_path':str(source)}),
                dict(type='tool_use', id='fixture_cat', name='Bash', input={'command':'cat '+str(source), 'description':'Read the synthetic fixture'}),
                dict(type='tool_use', id='fixture_outside', name='Read', input={'file_path':str(outside)}),
                dict(type='tool_use', id='fixture_touch', name='Bash', input={'command':'touch '+str(side_effect), 'description':'Forbidden synthetic write attempt'}),
            ]
            if arm == 'knowcode':
                calls = [calls[0],
                         dict(type='tool_use', id='fixture_context', name='mcp__knowcode__kc_context',
                              input={'symbol':'counter', 'source':'full'}),
                         dict(type='tool_use', id='fixture_note', name='mcp__knowcode__kc_note_add',
                              input={'symbol':'counter', 'text':'synthetic forbidden note'})]
            requests, errors = [], []

            class Handler(BaseHTTPRequestHandler):
                def log_message(self, *_args):
                    pass

                def do_POST(self):
                    try:
                        size = int(self.headers.get('Content-Length', '0'))
                        if not 0 < size <= 16*1024*1024 or self.headers.get('x-api-key') != 'synthetic-key':
                            raise ValueError('unexpected fixture request')
                        data = json.loads(self.rfile.read(size))
                        endpoint = self.path.split('?', 1)[0]
                        if endpoint == '/v1/messages/count_tokens':
                            output, kind = b'{"input_tokens":110}', 'application/json'
                        elif endpoint == '/v1/messages':
                            requests.append(data)
                            if len(requests) > 6:
                                raise ValueError('unexpected fixture loop')
                            results = [b for m in data.get('messages', []) if isinstance(m.get('content'), list)
                                       for b in m['content'] if b.get('type') == 'tool_result']
                            blocks = [dict(type='text', text=json.dumps(dict(status='answered', items=['example/app.ts:1'], notes='Synthetic response.')))] if results else calls
                            output, kind = stream_message(blocks, 2 if results else 1, config['model']), 'text/event-stream'
                        else:
                            raise ValueError('unexpected fixture endpoint')
                        self.send_response(200)
                        self.send_header('Content-Type', kind)
                        self.send_header('Content-Length', str(len(output)))
                        self.send_header('Connection', 'close')
                        self.end_headers()
                        self.wfile.write(output)
                        self.wfile.flush()
                    except Exception as exc:
                        errors.append(type(exc).__name__)
                        self.send_error(400, 'Fixture rejected request')

            server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
            server.daemon_threads = True
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            relay = None
            try:
                endpoint = 'http://127.0.0.1:'+str(server.server_port)
                client_key = 'synthetic-key'
                if relay_transport:
                    relay = ModelRelay('synthetic-key', timeout=10, _connection_factory=lambda:
                                       http.client.HTTPConnection('127.0.0.1', server.server_port, timeout=10))
                    endpoint, client_key = relay.base_url, relay.client_token
                    self.assertNotEqual(client_key, 'synthetic-key')
                policy = macos_sandbox.Policy(session, [client, *extra_executables, '/bin/bash', *bare_client.PROGRAMS.values()])
                for number, path in enumerate(dependencies):
                    key = 'GIT_SUPPORT'+str(number)
                    policy.parameters[key] = path
                    # Exact literals: Git's shim library and selected developer directory,
                    # with no recursive access to developer files or unrelated directories.
                    operations = 'file-read-data file-map-executable' if path.is_file() else 'file-read-data'
                    policy.profile += '(allow '+operations+' (literal (param "'+key+'")))\n'
                policy.parameters['API_LOOPBACK'] = 'localhost:'+endpoint.rsplit(':',1)[1]
                policy.profile += '(allow network-outbound (remote ip (param "API_LOOPBACK")))\n'
                env = dict(plan['env'], ANTHROPIC_API_KEY=client_key,
                           ANTHROPIC_BASE_URL=endpoint,
                           DISABLE_AUTOUPDATER='1', DISABLE_ERROR_REPORTING='1', DISABLE_TELEMETRY='1')
                version = owned_process.run(policy.command([client, '--version']), cwd=session/'control', env=env,
                                            stdout=root/'version.stdout', stderr=root/'version.stderr', timeout=15)
                self.assertEqual(version['returncode'], 0)
                self.assertTrue(version['cleanup_complete'])
                self.assertEqual((root/'version.stdout').read_text().strip(), expected_version+' (Claude Code)')
                outcome = owned_process.run(policy.command(plan['argv']), cwd=session/'control', env=env,
                                            stdout=root/'transcript.jsonl', stderr=root/'client.stderr', timeout=40)
            finally:
                relay_closed = relay.close() if relay else None
                server.shutdown()
                server.server_close()
                thread.join(timeout=5)
            self.assertEqual(errors, [])
            self.assertEqual(outcome['returncode'], 0, (root/'client.stderr').read_text())
            self.assertFalse(outcome['timed_out'])
            self.assertTrue(outcome['cleanup_complete'])
            self.assertEqual(len(requests), 2)
            self.assertNotIn('outside-source-fixture-marker', json.dumps(requests))
            self.assertFalse(side_effect.exists())
            self.assertEqual(source.read_text(), source_text)
            results = {b['tool_use_id']:b for m in requests[-1]['messages'] if isinstance(m.get('content'), list)
                       for b in m['content'] if b.get('type') == 'tool_result'}
            for key in ['fixture_read', 'fixture_cat'] if arm == 'plain' else ['fixture_read']:
                self.assertFalse(results[key].get('is_error', False))
                self.assertIn(source_text.strip(), json.dumps(results[key]['content']))
            if arm == 'plain':
                for key in ['fixture_outside', 'fixture_touch']:
                    self.assertTrue(results[key]['is_error'])
            else:
                advertised = {tool['name']:tool for tool in requests[0]['tools']}
                self.assertTrue({'Bash','Read'}.issubset(advertised))
                self.assertIn('mcp__knowcode__kc_context', advertised)
                self.assertNotIn('mcp__knowcode__kc_note_add', advertised)
                self.assertEqual(set(advertised)-{'Bash','Read'}, {key for key in advertised if key.startswith('mcp__knowcode__')})
                properties = advertised['mcp__knowcode__kc_context']['input_schema']['properties']
                self.assertIn('symbol', properties)  # 0.2 advertises common parameters only
                self.assertFalse(results['fixture_context'].get('is_error', False), results['fixture_context'])
                body = results['fixture_context']['content']
                context = json.loads(body if isinstance(body, str) else ''.join(block['text'] for block in body if block['type']=='text'))
                # 0.2 answers carry `complete`/`truncated` only when the answer is cut.
                self.assertTrue(context.get('complete', True))
                self.assertFalse(context.get('truncated', False))
                self.assertEqual(context['items'][0]['source'], source_text.strip())
                self.assertEqual(context['items'][0]['symbol']['file'], 'app.ts')
                self.assertEqual(context['freshness']['reparsed'], 1)
                self.assertTrue(list((session/'control/kc-cache').glob('repos/*.sqlite')) + list((session/'control/kc-cache').glob('v/*/repos/*.sqlite')))
                self.assertTrue(results['fixture_note']['is_error'])
                self.assertIn('No such tool available', json.dumps(results['fixture_note']['content']))
                self.assertFalse(any(b'synthetic forbidden note' in path.read_bytes()
                                     for path in (session/'control/kc-data').rglob('*') if path.is_file()))
            events, issues = public_transcript.read_events((root/'transcript.jsonl').read_bytes())
            measured = public_transcript.audit(events, question_id=question['id'], arm=arm,
                                               research_call_budget=20, source_root=session/'source', parse_issues=issues)
            self.assertTrue(measured['transcript_integrity'], measured['transcript_issues'])
            self.assertEqual(measured['research_calls'], 4 if arm == 'plain' else 3)
            self.assertEqual(measured['refused_calls'], 2 if arm == 'plain' else 0)
            if arm == 'knowcode':
                self.assertIn('persistent note tool forbidden', measured['policy_flags'])
            self.assertEqual(measured['unique_assistant_messages'], 2)
            self.assertEqual(measured['fixed_overhead_context'], 110)
            self.assertEqual(measured['research_context_tokens'], 10)
            self.assertEqual(measured['terminal_output_tokens'], 40)
            self.assertEqual(measured['answer']['items'], ['example/app.ts:1'])
            self.assertEqual(measured['permission_refusal_review_required'], [])
            if relay_transport:
                self.assertTrue(relay_closed)
                records = relay.records()
                self.assertEqual(len(records), 2)
                self.assertTrue(all(row['completed'] for row in records))
                self.assertEqual([row['response_message_id'] for row in records], [row['id'] for row in measured['observed_turns']])
                self.assertNotIn('synthetic-key', json.dumps(records))
                self.assertNotIn(client_key, json.dumps(records))
                modes = [event['permissionMode'] for event in events if event.get('type')=='system' and event.get('subtype')=='init']
                self.assertEqual(modes, ['dontAsk'])
                binding = public_transcript.audit_transport(measured, events, records, expected_model=config['model'],
                                                            relay_cleanup_complete=relay_closed)
                self.assertTrue(binding['transport_integrity'], binding['transport_issues'])


if __name__ == '__main__':
    unittest.main()
