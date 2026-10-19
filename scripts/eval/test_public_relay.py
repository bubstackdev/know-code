"""Local-only transport checks. All credentials and upstream responses are fixtures."""
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import hashlib
import http.client
import json
import threading
import unittest
from unittest.mock import patch
from urllib.parse import urlsplit

from model_relay import ModelRelay


class RelayTests(unittest.TestCase):
    def setUp(self):
        self.seen = []
        self.release = threading.Event()
        self.mode = 'json'
        owner = self
        self.first = b'event: message_start\ndata: {"type":"message_start","message":{"id":"msg_fixture"}}\n\n'
        self.last = b'event: message_stop\ndata: {"type":"message_stop"}\n\n'

        class Upstream(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def do_POST(self):
                body = self.rfile.read(int(self.headers['Content-Length']))
                owner.seen.append(dict(path=self.path, headers=dict(self.headers), body=body))
                if owner.mode == 'redirect':
                    self.send_response(307)
                    self.send_header('Location', 'http://invalid.example/forbidden')
                    self.end_headers()
                    return
                if owner.mode == 'limited':
                    self.send_response(429)
                    self.send_header('Retry-After', '2')
                    self.send_header('Retry-After-Ms', '250')
                    self.send_header('X-Should-Retry', 'false')
                    self.end_headers()
                    self.wfile.write(b'{"type":"error"}')
                    return
                self.send_response(200)
                self.send_header('Content-Type', 'text/event-stream' if owner.mode == 'stream' else 'application/json')
                self.send_header('Request-Id', 'req_fixture')
                self.end_headers()
                if owner.mode == 'stream':
                    self.wfile.write(owner.first)
                    self.wfile.flush()
                    owner.release.wait(timeout=3)
                    try:
                        self.wfile.write(owner.last)
                    except OSError:
                        pass
                else:
                    self.wfile.write(b'{"id":"msg_fixture","type":"message","content":[]}')

        self.upstream = ThreadingHTTPServer(('127.0.0.1', 0), Upstream)
        self.upstream.daemon_threads = True
        self.thread = threading.Thread(target=self.upstream.serve_forever, daemon=True)
        self.thread.start()
        self.relay = ModelRelay('fixture-provider-key', timeout=3, _connection_factory=self.connection)

    def tearDown(self):
        self.release.set()
        self.assertTrue(self.relay.close())
        self.upstream.shutdown()
        self.upstream.server_close()
        self.thread.join(timeout=2)

    def connection(self):
        return http.client.HTTPConnection('127.0.0.1', self.upstream.server_port, timeout=3)

    def request(self, path='/v1/messages?beta=true', *, token=None, body=b'{"model":"fixture-model","messages":[]}', headers=None, method='POST'):
        location = urlsplit(self.relay.base_url)
        client = http.client.HTTPConnection(location.hostname, location.port, timeout=2)
        supplied = {'Content-Type':'application/json', 'x-api-key':token or self.relay.client_token, 'anthropic-version':'2023-06-01'}
        supplied.update(headers or {})
        client.request(method, path, body=body, headers=supplied)
        response = client.getresponse()
        return client, response

    def consume(self, *args, **kwargs):
        client, response = self.request(*args, **kwargs)
        try:
            return response.status, dict(response.getheaders()), response.read()
        finally:
            response.close()
            client.close()

    def test_credentials_are_parent_owned_and_not_logged_or_forwarded_from_client(self):
        status, _, body = self.consume(headers={'Authorization':'Bearer fixture-client-authorization',
                                               'Idempotency-Key':'fixture-retry-key','X-Stainless-Retry-Count':'1'})
        self.assertEqual(status, 200)
        self.assertEqual(json.loads(body)['id'], 'msg_fixture')
        sent = {key.lower():value for key,value in self.seen[0]['headers'].items()}
        self.assertEqual(sent['x-api-key'], 'fixture-provider-key')
        self.assertNotEqual(self.relay.client_token, 'fixture-provider-key')
        self.assertNotIn('authorization', sent)
        self.assertEqual(sent['anthropic-version'], '2023-06-01')
        self.assertEqual(sent['idempotency-key'], 'fixture-retry-key')
        self.assertEqual(sent['x-stainless-retry-count'], '1')
        self.assertTrue(self.relay.close())
        record = self.relay.records()[0]
        self.assertTrue(record['completed'])
        self.assertEqual(record['response_message_id'], 'msg_fixture')
        self.assertEqual(record['request_body_sha256'], hashlib.sha256(self.seen[0]['body']).hexdigest())
        self.assertEqual(record['response_sha256'], hashlib.sha256(body).hexdigest())
        for secret in ['fixture-provider-key', self.relay.client_token, 'fixture-client-authorization']:
            self.assertNotIn(secret, json.dumps(self.relay.records()))

    def test_bad_auth_routes_queries_methods_and_bodies_do_not_reach_upstream(self):
        cases = [dict(token='wrong'), dict(path='/v1/models'), dict(path='/v1/messages?target=elsewhere'),
                 dict(method='GET'), dict(body=b'[]'), dict(body=b'{"model":NaN}'),
                 dict(body=b'{"model":"fixture-model"}', headers={'Content-Type':'text/plain'})]
        for case in cases:
            with self.subTest(case=case):
                status, _, _ = self.consume(**case)
                self.assertGreaterEqual(status, 400)
        self.assertEqual(self.seen, [])
        self.assertEqual(self.relay.records(), [])

    def test_count_tokens_is_forwarded_and_upstream_redirect_is_not_followed(self):
        status, _, _ = self.consume('/v1/messages/count_tokens')
        self.assertEqual(status, 200)
        self.mode = 'redirect'
        status, headers, _ = self.consume()
        self.assertEqual(status, 307)
        self.assertNotIn('Location', headers)
        self.assertEqual(len(self.seen), 2)
        self.assertEqual(self.seen[0]['path'], '/v1/messages/count_tokens')

    def test_rate_limit_status_and_retry_header_are_preserved(self):
        self.mode = 'limited'
        status, headers, _ = self.consume()
        self.assertEqual(status, 429)
        self.assertEqual(headers['Retry-After'], '2')
        self.assertEqual(headers['Retry-After-Ms'], '250')
        self.assertEqual(headers['X-Should-Retry'], 'false')

    def test_sse_is_delivered_before_upstream_finishes(self):
        self.mode = 'stream'
        client, response = self.request()
        try:
            self.assertEqual(response.read(len(self.first)), self.first)
            self.release.set()
            self.assertEqual(response.read(), self.last)
        finally:
            response.close()
            client.close()
        self.assertTrue(self.relay.close())
        self.assertEqual(self.relay.records()[0]['response_message_id'], 'msg_fixture')
        self.assertEqual(self.relay.records()[0]['response_bytes'], len(self.first+self.last))

    def test_close_interrupts_only_the_relays_owned_active_connection(self):
        self.mode = 'stream'
        client, response = self.request()
        try:
            self.assertEqual(response.read(len(self.first)), self.first)
            self.assertTrue(self.relay.close())
            self.assertFalse(self.relay.records()[0]['completed'])
        finally:
            self.release.set()
            response.close()
            client.close()

    def test_transport_error_text_cannot_expose_provider_key(self):
        class Broken(http.client.HTTPConnection):
            def request(self, *_args, **_kwargs):
                raise RuntimeError('fixture-provider-key')
        self.relay._connect = lambda: Broken('127.0.0.1', self.upstream.server_port, timeout=3)
        status, _, body = self.consume()
        self.assertEqual(status, 502)
        self.assertNotIn(b'fixture-provider-key', body)
        self.assertTrue(self.relay.close())
        self.assertEqual(self.relay.records()[0]['error_type'], 'RuntimeError')
        self.assertNotIn('fixture-provider-key', json.dumps(self.relay.records()))

    def test_default_factory_targets_only_fixed_provider(self):
        with patch('model_relay.http.client.HTTPSConnection', return_value=self.connection()) as constructor:
            with ModelRelay('another-fixture-key', timeout=3) as relay:
                old = self.relay
                self.relay = relay
                try:
                    self.assertEqual(self.consume()[0], 200)
                finally:
                    self.relay = old
            constructor.assert_called_once_with('api.anthropic.com', 443, timeout=3)


if __name__ == '__main__':
    unittest.main()
