"""Parent-owned Messages API relay; provider credentials never enter the client env.

The client receives a temporary relay token and may connect only to this owned
localhost port. The upstream is fixed; no forwarding URL comes from a request.
Both arms must use the same relay configuration. This transport component does
not establish study readiness or validate transcript/request correspondence.
"""
from __future__ import annotations

import hashlib
import hmac
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import http.client
import json
import math
import secrets
import socket
import threading
import time

PROVIDER = 'api.anthropic.com'
PATHS = {'/v1/messages', '/v1/messages/count_tokens'}
FORWARD_HEADERS = {'anthropic-version', 'anthropic-beta', 'content-type', 'accept', 'user-agent',
                   'idempotency-key', 'x-stainless-retry-count', 'x-stainless-timeout'}


class ModelRelay:
    def __init__(self, api_key, *, timeout=300, _connection_factory=None):
        if not isinstance(api_key, str) or not api_key.strip() or '\n' in api_key or '\r' in api_key:
            raise ValueError('nonempty provider credential required')
        if type(timeout) not in (int, float) or not math.isfinite(timeout) or timeout <= 0:
            raise ValueError('positive finite transport timeout required')
        self._key = api_key
        self.client_token = secrets.token_urlsafe(32)
        self._connect = _connection_factory or (lambda: http.client.HTTPSConnection(PROVIDER, 443, timeout=timeout))
        self._condition = threading.Condition()
        self._active = {}
        self._records = []
        self._closed = False
        owner = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def reject(self, status):
                body = b'{"type":"error","error":{"type":"invalid_request_error","message":"Relay rejected request"}}'
                self.send_response(status)
                self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def do_GET(self):
                self.reject(405)

            def do_POST(self):
                self.connection.settimeout(15)
                supplied = self.headers.get('x-api-key', '')
                if not hmac.compare_digest(supplied.encode(), owner.client_token.encode()):
                    self.reject(401)
                    return
                endpoint, separator, query = self.path.partition('?')
                if endpoint not in PATHS or (separator and query != 'beta=true'):
                    self.reject(403)
                    return
                if self.headers.get_content_type() != 'application/json' or self.headers.get('Transfer-Encoding'):
                    self.reject(415)
                    return
                try:
                    lengths = self.headers.get_all('Content-Length', [])
                    if len(lengths) != 1 or not lengths[0].isdecimal():
                        raise ValueError('one content length required')
                    size = int(lengths[0])
                    if not 0 < size <= 32*1024*1024:
                        raise ValueError('request body outside transport bound')
                    body = self.rfile.read(size)
                    if len(body) != size:
                        raise ValueError('incomplete request body')
                    def invalid_number(_value):
                        raise ValueError('non-finite JSON value')
                    data = json.loads(body, parse_constant=invalid_number)
                    if not isinstance(data, dict) or not isinstance(data.get('model'), str) or not data['model']:
                        raise ValueError('Messages API object with explicit model required')
                except (ValueError, OSError):
                    self.reject(400)
                    return
                record = dict(endpoint=endpoint, request_body_sha256=hashlib.sha256(body).hexdigest(),
                              request_bytes=len(body), model=data['model'], response_status=None,
                              response_bytes=0, response_sha256=None, response_message_id=None, completed=False)
                connection = owner._connect()
                with owner._condition:
                    if owner._closed:
                        self.reject(503)
                        return
                    owner._active[connection] = None
                response_hash = hashlib.sha256()
                sent_headers = False
                response = None
                try:
                    connection.connect()
                    with owner._condition:
                        owner._active[connection] = connection.sock
                        if owner._closed:
                            return
                    headers = {key: value for key, value in self.headers.items() if key.lower() in FORWARD_HEADERS}
                    headers['x-api-key'] = owner._key
                    connection.request('POST', self.path, body=body, headers=headers)
                    response = connection.getresponse()
                    record['response_status'] = response.status
                    record['provider_request_id'] = response.getheader('Request-Id')
                    self.send_response(response.status)
                    for key in ['Content-Type', 'Request-Id', 'Retry-After', 'Retry-After-Ms', 'X-Should-Retry']:
                        value = response.getheader(key)
                        if value is not None:
                            self.send_header(key, value)
                    # HTTP/1.0 close-delimited output preserves streaming without
                    # buffering the response or implementing another chunk parser.
                    self.send_header('Connection', 'close')
                    self.end_headers()
                    sent_headers = True
                    prefix = b''
                    while chunk := response.read1(65536):
                        response_hash.update(chunk)
                        record['response_bytes'] += len(chunk)
                        if not record['response_message_id'] and len(prefix) < 65536:
                            prefix += chunk[:65536-len(prefix)]
                            try:
                                candidates = [json.loads(prefix)] if response.getheader('Content-Type', '').startswith('application/json') else [
                                    json.loads(line[5:].strip()) for line in prefix.splitlines() if line.startswith(b'data:') and line.rstrip().endswith(b'}')]
                                for candidate in candidates:
                                    message = candidate.get('message') if candidate.get('type') == 'message_start' else candidate
                                    if isinstance(message, dict) and isinstance(message.get('id'), str):
                                        record['response_message_id'] = message['id']
                                        break
                            except (ValueError, AttributeError):
                                pass
                        self.wfile.write(chunk)
                        self.wfile.flush()
                    with owner._condition:
                        record['completed'] = not owner._closed
                        if owner._closed:
                            record['error_type'] = 'Cancelled'
                except Exception as exc:
                    # Never include exception text, headers or credentials in logs.
                    record['error_type'] = type(exc).__name__
                    if not sent_headers:
                        try:
                            self.reject(502)
                        except OSError:
                            pass
                finally:
                    if response is not None:
                        response.close()
                    connection.close()
                    record['response_sha256'] = response_hash.hexdigest()
                    with owner._condition:
                        owner._records.append(record)
                        owner._active.pop(connection, None)
                        owner._condition.notify_all()

        self._server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        self._server.daemon_threads = True
        self.base_url = 'http://127.0.0.1:'+str(self._server.server_port)
        self._thread = threading.Thread(target=self._server.serve_forever, daemon=True)
        self._thread.start()

    def records(self):
        with self._condition:
            return [dict(row) for row in self._records]

    def close(self):
        with self._condition:
            self._closed = True
            active = list(self._active.items())
        self._server.shutdown()
        self._server.server_close()
        for connection, channel in active:
            if channel is not None:
                try:
                    channel.shutdown(socket.SHUT_RDWR)
                except OSError:
                    pass
            connection.close()
        self._thread.join(timeout=2)
        until = time.monotonic()+2
        with self._condition:
            while self._active and time.monotonic() < until:
                self._condition.wait(timeout=until-time.monotonic())
            return not self._active and not self._thread.is_alive()

    def __enter__(self):
        return self

    def __exit__(self, *_args):
        self.close()
