"""A minimal MCP client over stdio, for compare-v1's adapters: one request at a time, every exchange logged.

The same client speaks to every tool, so no tool gets a different transport, timeout or retry. It follows
replay_gate.py's Server, generalised to any command.
"""
from __future__ import annotations

import json
import os
import select
import subprocess
import time

PROTOCOL = '2025-03-26'


class Error(Exception):
    """A call that failed: the server errored, exited or passed its time limit."""


class Client:
    def __init__(self, command, *, env=None, cwd=None, stderr=None, log=None, timeout=120):
        """Starts `command` and initialises the session. `stderr` and `log` are open files (or None): the server's
        stderr, and one JSON line per request and reply."""
        self.process = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=stderr or subprocess.DEVNULL, env=env, cwd=cwd)
        self.log = log
        self.next_id = 0
        self.buffer = b''
        init = self.request('initialize', {'protocolVersion': PROTOCOL, 'capabilities': {},
                                           'clientInfo': {'name': 'compare-v1', 'version': '1'}}, timeout)
        self.server = (init.get('result') or {}).get('serverInfo') or {}
        self.instructions = (init.get('result') or {}).get('instructions') or ''
        self.send({'jsonrpc': '2.0', 'method': 'notifications/initialized'})
        self.tools = (self.request('tools/list', {}, timeout).get('result') or {}).get('tools') or []

    def send(self, message):
        self.process.stdin.write((json.dumps(message) + '\n').encode())
        self.process.stdin.flush()

    def request(self, method, params, timeout):
        self.next_id += 1
        message = {'jsonrpc': '2.0', 'id': self.next_id, 'method': method, 'params': params}
        self._log('request', message)
        self.send(message)
        until = time.monotonic() + timeout
        while True:
            while b'\n' in self.buffer:
                line, self.buffer = self.buffer.split(b'\n', 1)
                if not line.strip():
                    continue
                reply = json.loads(line)
                if reply.get('id') == self.next_id:
                    self._log('reply', reply)
                    return reply
                # A notification or a request from the server (logging, progress): kept, not answered.
                self._log('other', reply)
            remaining = until - time.monotonic()
            if remaining <= 0:
                raise Error(f'{method}: no reply in {timeout}s')
            ready, _, _ = select.select([self.process.stdout], [], [], remaining)
            if ready:
                chunk = os.read(self.process.stdout.fileno(), 1 << 20)
                if not chunk:
                    raise Error(f'{method}: the server exited ({self.process.poll()})')
                self.buffer += chunk

    def call(self, tool, args, timeout=60):
        """The text of a tool's answer, and its structured content when it gives one. Raises Error on an MCP error
        or a result marked isError."""
        reply = self.request('tools/call', {'name': tool, 'arguments': args}, timeout)
        if 'error' in reply:
            raise Error(json.dumps(reply['error']))
        result = reply.get('result') or {}
        text = '\n'.join(p.get('text', '') for p in result.get('content') or [] if p.get('type') == 'text')
        if result.get('isError'):
            raise Error(text[:2000])
        return text, result.get('structuredContent')

    def close(self):
        try:
            self.process.stdin.close()
            self.process.wait(timeout=10)
        except (subprocess.TimeoutExpired, BrokenPipeError):
            self.process.kill()
            self.process.wait()

    def _log(self, what, message):
        if self.log:
            self.log.write(json.dumps({'t': round(time.time(), 3), what: message}) + '\n')
            self.log.flush()
