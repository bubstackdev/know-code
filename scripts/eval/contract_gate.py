#!/usr/bin/env python3
"""Contract gate: what KnowCode says it offers, checked against what its server does, with no model calls.

    contract_gate.py capabilities --knowcode BIN --work WORK [--out REPORT]
    contract_gate.py restricted --knowcode BIN --work WORK [--out REPORT]
    contract_gate.py codes [--same-build] BASE_CALLS CANDIDATE_CALLS [--out REPORT]

Each exits 1 when a check fails, and lists every failure. Nothing here clones or runs third-party code: the
repositories are made-up fixtures written into WORK, and the network is watched from the Mac with loopback-only
means.

`capabilities` starts `BIN serve --tools all,kc_capabilities` (and `--tools one`) over a fixture repository and, for every op in
`kc {op: capabilities}`:
- each parameter the advertised tool lists (`tools/list`) is in it, of the same type, choices and bounds, and each
  parameter it lists is taken: a value of its type isn't refused for it;
- a call with sample arguments answers, and a JSON answer has the keys `output.keys` promises;
- each range: past its maximum, under its minimum, negative and past its type's maximum, a value is clamped or
  refused as `above_max`, `below_min`, `negative` and `above_type_max` say (an older build's `beyond`), and a value
  of the wrong type is refused with `wrong_type`'s code; each closed choice: a word outside it is refused with its `code`;
- through the single `kc` tool, `{op, q}` (and `{op, qs}`) reaches the op;
- read ops leave the notes as they were, and `note_add` (the one write) changes them;
- every refusal carries a code from `codes`, at `errors.at` with `errors.keys`.

`restricted` starts `BIN serve --restricted` bound to one fixture repository with an op allowlist, an empty home and
temporary directory, explicit cache and config directories, and the proxy variables pointing at a loopback listener:
- the startup reply states the scope (the repos and tools), and only the allowed tools are listed;
- calls about the bound repo answer; calls naming the other repo, a path in it, or an op not allowed are refused
  (`not_allowed`);
- no file outside the cache directory is created, changed or removed (the repos, their `.git`, the home, the
  temporary and config directories included);
- no network socket: the server and its children are polled with `lsof -i` throughout, and the loopback proxy
  listener must see no connection.

`codes` compares two replay-gate runs' calls (`calls.jsonl` beside each build's summary under the replay's
WORK/builds) call by call: every refusal is coded, and a call refused in both keeps
its code. With `--same-build` (one build replayed twice), every call is refused or answered as before, and every
answer's `incomplete` and `more` entries keep their codes.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import threading
import time

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import replay_gate  # noqa: E402

GIT_ENV = replay_gate.GIT_ENV
# A made-up repository the ops have something to say about, with two authors' history.
FIXTURE = {
    'src/Shelf.cs': 'namespace Store;\npublic class Shelf\n{\n    public int Count(string owner) => owner.Length;\n'
                    '    public int Put(int item) => item;\n}\n',
    'src/Desk.cs': 'namespace Store;\npublic class Desk\n{\n    private readonly Shelf _shelf = new Shelf();\n'
                   '    public int Total(string owner) => _shelf.Count(owner) + _shelf.Put(1);\n}\n',
    'tests/ShelfTests.cs': 'namespace Store.Tests;\npublic class ShelfTests\n{\n    public void Counts() => new Shelf().Count("a");\n}\n',
    'web/cart.ts': 'export function addToCart(item: string): string {\n  return item;\n}\n',
    'web/app.ts': "import { addToCart } from './cart';\nexport function buy(x: string) {\n  return addToCart(x);\n}\n",
    'Sources/Bin/Bin.swift': 'struct Bin {\n    func put(_ item: Int) -> Int { item }\n}\nfunc fill(bin: Bin) { _ = bin.put(1) }\n',
}
OTHER = {'src/Crate.cs': 'namespace Yard;\npublic class Crate\n{\n    public int Count(int n) => n;\n}\n'}
# Sample arguments per op, over the fixture.
SAMPLES = {
    'impact': {'symbol': 'Shelf.Count'}, 'find': {'query': 'Shelf'}, 'refs': {'name': 'Count', 'limit': 1},
    'context': {'symbol': 'Shelf'}, 'search': {'pattern': 'Count'}, 'locate': {'query': 'count items on a shelf'},
    'help': {'topic': 'kc_impact'}, 'trace': {'symbol': 'Shelf.Count'},
    'path': {'from': 'Desk.Total', 'to': 'Shelf.Count'}, 'flow': {'entry': 'Desk.Total'},
    'symbol_at': {'file': 'src/Shelf.cs', 'line': 4}, 'unused': {}, 'rename_preview': {'symbol': 'Shelf.Put', 'new_name': 'Place'},
    'coupled': {'file': 'src/Shelf.cs'}, 'tests': {'targets': ['src/Shelf.cs']}, 'hotspots': {}, 'ownership': {},
    'reviewers': {'files': ['src/Shelf.cs']}, 'history': {'symbol': 'Shelf.Count'}, 'overview': {}, 'areas': {},
    'notes': {}, 'grep_ast': {'pattern': 'Count($$$)', 'lang': 'cs'}, 'status': {}, 'capabilities': {},
    'note_add': {'symbol': 'Shelf.Count', 'text': 'counts by owner'},
    'task': {'goal': 'count items on a shelf'}, 'test_targets': {}, 'rules': {'regex': 'Count'},
}


def repo(path, files, authors=('Ann Example <ann@example.com>', 'Bo Example <bo@example.com>')):
    """A fixture repository at `path`: `files` committed in two commits by two made-up authors, made once."""
    path = Path(path)
    if (path/'.git').is_dir():
        return path
    path.mkdir(parents=True)
    names = sorted(files)
    for i, chunk in enumerate((names[: len(names) // 2 + 1], names[len(names) // 2 + 1:])):
        for name in chunk:
            (path/name).parent.mkdir(parents=True, exist_ok=True)
            (path/name).write_text(files[name])
        if i == 0:
            subprocess.run(['git', 'init', '--quiet', '--initial-branch=main'], cwd=path, env=GIT_ENV, check=True)
        subprocess.run(['git', 'add', '-A'], cwd=path, env=GIT_ENV, check=True)
        author = authors[i % len(authors)]
        subprocess.run(['git', '-c', 'user.name=x', '-c', 'user.email=x@example.com', 'commit', '--quiet', '--allow-empty',
                        f'--author={author}', '-m', f'fixture {i + 1}'], cwd=path, env=GIT_ENV, check=True)
    return path


class Client(replay_gate.Server):
    """`BIN serve` over MCP, keeping the `initialize` reply."""

    def __init__(self, binary, args, env, cwd):
        self.process = subprocess.Popen([str(binary), 'serve', *args], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=subprocess.DEVNULL, env=env, cwd=cwd)
        self.next_id = 0
        self.buffer = b''
        self.init = self.request('initialize', {'protocolVersion': '2025-03-26', 'capabilities': {},
                                                'clientInfo': {'name': 'contract_gate', 'version': '1'}}, timeout=120)
        self.send({'jsonrpc': '2.0', 'method': 'notifications/initialized'})
        self.tools = (self.request('tools/list', {}, timeout=60).get('result') or {}).get('tools') or []

    def ask(self, tool, args, timeout=120):
        """(error, text, code, structured error) of one call."""
        reply = self.request('tools/call', {'name': tool, 'arguments': args}, timeout)
        if 'error' in reply:
            return True, json.dumps(reply['error']), None, None
        result = reply.get('result') or {}
        text = '\n'.join(p.get('text', '') for p in result.get('content') or [] if p.get('type') == 'text')
        err = (result.get('structuredContent') or {}).get('error')
        return bool(result.get('isError')), text, (err or {}).get('code'), err


def digest_tree(root, skip=()):
    """Every file under `root` (but `skip`) with its size, modification time and content hash."""
    out = {}
    root = Path(root)
    for dirpath, dirnames, filenames in os.walk(root):
        d = Path(dirpath)
        dirnames[:] = [n for n in dirnames if not any((d/n).resolve() == Path(s).resolve() for s in skip)]
        for n in filenames:
            p = d/n
            try:
                st = p.lstat()
                h = hashlib.sha256(p.read_bytes()).hexdigest() if p.is_file() and not p.is_symlink() else 'link'
            except OSError:
                continue
            out[str(p.relative_to(root))] = (st.st_size, st.st_mtime_ns, h)
    return out


def changed(before, after):
    return sorted({k for k in set(before) | set(after) if before.get(k) != after.get(k)})


# --- capabilities ---------------------------------------------------------------------------------------------------

def range_ends(r):
    """(value, what the capabilities say happens to it) at each end of a range: `clamped`, or the refusal's code. Read
    from `above_max`, `below_min`, `negative`, `type_max` and `above_type_max`, or from an older build's `beyond`."""
    lo, hi = r.get('min'), r.get('max')
    if 'beyond' in r:
        refused = 'invalid_argument' if r['beyond'] == 'refused' else r['beyond']
        return [(v, refused) for v in (hi + 1 if hi is not None else None, lo - 1 if lo is not None else None) if v is not None]
    ends = []
    if hi is not None:
        ends.append((hi + 1, r.get('above_max')))
    ends.append((-1, r.get('negative')))
    if lo is not None and lo > 0:
        ends.append((lo - 1, r.get('below_min')))
    if r.get('type_max') is not None:
        ends += [(r['type_max'], r.get('above_max')), (r['type_max'] + 1, r.get('above_type_max'))]
    return ends


def check_capabilities(binary, work):
    work = Path(work).resolve()
    root = repo(work/'fixture'/'store', FIXTURE)
    env = replay_gate.build_env(work/'kc-capabilities')
    subprocess.run([str(binary), 'index', str(root)], env=env, cwd=root, check=True, capture_output=True)
    failures, notes = [], []
    server = Client(binary, ['--tools', 'all,kc_capabilities'], env, root)
    try:
        error, text, code, _ = server.ask('kc_capabilities', {})
        caps = json.loads(text)
        # The tools served on request only (not in `all`): named too, so every op can be called as its tool.
        on_request = [e['tool'] for e in caps['ops'] if not e['advertised'] and e['tool'] != 'kc_capabilities']
        if on_request:
            server.close()
            server = Client(binary, ['--tools', ','.join(['all', 'kc_capabilities', *on_request])], env, root)
        tools = {t['name']: t for t in server.tools}
        codes = set(caps['codes'])
        seen = {}

        def call(op, args, tool=None):
            error, text, code, err = server.ask(tool or f'kc_{op}', args)
            if error:
                if code is None:
                    failures.append(f'{op} {json.dumps(args)}: refused without a code: {text[:160]}')
                elif code not in codes:
                    failures.append(f'{op}: refused with {code!r}, not among the capabilities\' codes')
                elif err and not set(caps['errors']['keys']) <= set(err):
                    failures.append(f'{op}: its error lacks {sorted(set(caps["errors"]["keys"]) - set(err))}')
            return error, text, code

        notes_before = digest_tree(work/'kc-capabilities'/'data')
        for entry in caps['ops']:
            op = entry['op']
            tool = tools.get(entry['tool'])
            if entry['advertised'] and tool is None:
                failures.append(f'{op}: advertised, but {entry["tool"]} isn\'t listed')
                continue
            props = entry['input'].get('properties', {})
            if tool is not None:
                # The advertised schema is kept short (fewer words, the less common parameters left out): each
                # parameter it lists is in the capabilities, of the same type, choices and bounds.
                schema = tool.get('inputSchema') or {}
                for name, got in sorted((schema.get('properties') or {}).items()):
                    want = props.get(name)
                    if want is None:
                        failures.append(f'{op}.{name}: advertised, missing from the capabilities')
                        continue
                    for k in ('type', 'items', 'enum', 'minimum', 'maximum'):
                        if k in got and got.get(k) != want.get(k):
                            failures.append(f'{op}.{name}.{k}: capabilities {json.dumps(want.get(k))} vs tool {json.dumps(got.get(k))}')
                if sorted(entry['input'].get('required') or []) != sorted(schema.get('required') or []):
                    failures.append(f'{op}: required {entry["input"].get("required")} vs tool {schema.get("required")}')
            if op == 'note_add':
                continue
            if op == 'more':
                if 'cursor' not in seen:
                    notes.append('more: no cursor in the sample answers, not called')
                    continue
                args = {'cursor': seen['cursor']}
            else:
                args = dict(SAMPLES.get(op, {}))
            fmt = entry['output'].get('formats') or []
            if 'json' in fmt and entry['output'].get('format') != 'json':
                args['format'] = 'json'
            error, text, code = call(op, args)
            if error:
                failures.append(f'{op} {json.dumps(args)}: refused ({code}): {text[:160]}')
                continue
            if 'json' in fmt:
                try:
                    answer = json.loads(text)
                except json.JSONDecodeError:
                    failures.append(f'{op}: a JSON answer promised, got {text[:80]!r}')
                    answer = {}
                missing = set(entry['output'].get('keys') or []) - set(answer)
                if missing:
                    failures.append(f'{op}: the answer lacks {sorted(missing)} that output.keys promises')
                if isinstance(answer, dict) and answer.get('cursor'):
                    seen.setdefault('cursor', answer['cursor'])
            # Every parameter the capabilities list is one the op takes: a value of its type isn't refused for it.
            for name, spec in sorted(props.items()):
                if name in args or name in entry['ranges'] or name in entry['choices']:
                    continue
                value = {'integer': spec.get('default', spec.get('minimum', 1)), 'boolean': False, 'array': [],
                         'string': (spec.get('enum') or [None])[0]}.get(spec.get('type'))
                if value is None:
                    continue
                error, text, code = call(op, {**args, name: value})
                if code == 'invalid_argument' and name in text:
                    failures.append(f'{op}.{name}={json.dumps(value)}: refused: {text[:120]}')
            for name, r in entry['ranges'].items():
                for value, said in range_ends(r):
                    error, text, code = call(op, {**args, name: value})
                    if said == 'clamped' and error:
                        failures.append(f'{op}.{name}={value}: refused ({code}) where capabilities say clamped')
                    if said not in (None, 'clamped') and code != said:
                        failures.append(f'{op}.{name}={value}: {"answered" if not error else code} where capabilities say {said}')
                error, text, code = call(op, {**args, name: 'many'})
                if code != r.get('wrong_type'):
                    failures.append(f'{op}.{name}="many": {code or "answered"}, capabilities say {r.get("wrong_type")}')
            for name, c in entry['choices'].items():
                error, text, code = call(op, {**args, name: 'zzz-not-a-choice'})
                if c.get('other') == 'refused' and code != c.get('code'):
                    failures.append(f'{op}.{name}=zzz-not-a-choice: {code or "answered"}, capabilities say {c.get("code")}')
        if changed(notes_before, digest_tree(work/'kc-capabilities'/'data')):
            failures.append('read ops changed the notes')
        error, text, code = call('note_add', dict(SAMPLES['note_add']))
        if error or not changed(notes_before, digest_tree(work/'kc-capabilities'/'data')):
            failures.append(f'note_add: {"refused " + str(code) if error else "left the notes as they were"}')
    finally:
        server.close()
    # The single `kc` tool reaches each op through `q` and `qs`.
    one = Client(binary, ['--tools', 'one'], env, root)
    try:
        for entry in caps['ops']:
            op, q = entry['op'], entry.get('q')
            if not entry.get('takes_query') or q is None or op in ('more', 'note_add') or q not in SAMPLES.get(op, {}):
                continue
            value = SAMPLES[op][q]
            one_value = value[0] if isinstance(value, list) else value
            rest = {k: v for k, v in SAMPLES[op].items() if k != q}
            error, text, code, _ = one.ask('kc', {'op': op, 'q': one_value, **rest})
            if error:
                failures.append(f'kc {{op: {op}, q}}: refused ({code}): {text[:120]}')
            if entry.get('qs'):
                error, text, code, _ = one.ask('kc', {'op': op, 'qs': [one_value], **rest})
                if error:
                    failures.append(f'kc {{op: {op}, qs}}: refused ({code}): {text[:120]}')
    finally:
        one.close()
    return failures, notes, len(caps['ops'])


# --- restricted -----------------------------------------------------------------------------------------------------

class ProxyListener:
    """A loopback listener the proxy variables point at, counting every connection made to it."""

    def __init__(self):
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.sock.bind(('127.0.0.1', 0))
        self.sock.listen(16)
        self.sock.settimeout(0.2)
        self.port = self.sock.getsockname()[1]
        self.connections = 0
        self.stop = False
        self.thread = threading.Thread(target=self.run, daemon=True)
        self.thread.start()

    def run(self):
        while not self.stop:
            try:
                conn, _ = self.sock.accept()
                self.connections += 1
                conn.close()
            except OSError:
                continue

    def close(self):
        self.stop = True
        self.thread.join()
        self.sock.close()


class SocketWatch:
    """Polls `lsof -i` on a process and its descendants, keeping every network socket seen."""

    def __init__(self, pid):
        self.pid = pid
        self.seen = set()
        self.stop = False
        self.thread = threading.Thread(target=self.run, daemon=True)
        self.thread.start()

    def pids(self):
        out, frontier = [self.pid], [self.pid]
        while frontier:
            p = subprocess.run(['pgrep', '-P', ','.join(map(str, frontier))], capture_output=True, text=True)
            frontier = [int(x) for x in p.stdout.split()]
            out += frontier
        return out

    def run(self):
        while not self.stop:
            p = subprocess.run(['lsof', '-nP', '-a', '-i', '-p', ','.join(map(str, self.pids()))], capture_output=True, text=True)
            for line in p.stdout.splitlines()[1:]:
                self.seen.add(' '.join(line.split()[7:]))
            time.sleep(0.02)

    def close(self):
        self.stop = True
        self.thread.join()


def check_restricted(binary, work):
    work = Path(work).resolve()/'restricted'
    bound = repo(work/'repos'/'store', FIXTURE)
    other = repo(work/'repos'/'yard', OTHER)
    dirs = {name: work/name for name in ('home', 'tmp', 'cache', 'config')}
    for d in dirs.values():
        d.mkdir(parents=True, exist_ok=True)
    proxy = ProxyListener()
    url = f'http://127.0.0.1:{proxy.port}'
    env = dict(GIT_ENV, HOME=str(dirs['home']), TMPDIR=str(dirs['tmp']), HTTP_PROXY=url, HTTPS_PROXY=url, ALL_PROXY=url,
               http_proxy=url, https_proxy=url, all_proxy=url, NO_PROXY='', no_proxy='')
    before = digest_tree(work, skip=[dirs['cache']])
    failures = []
    allowed = ['find', 'refs']
    base_args = ['--restricted', '--repo', str(bound), '--ops', ','.join(allowed), '--cache-dir', str(dirs['cache']),
                 '--config-dir', str(dirs['config'])]
    sockets = set()
    # Every tool named (the allowed ones and capabilities are listed), then the default single `kc` tool.
    sessions = [
        (['--tools', 'all,kc_capabilities'], sorted([f'kc_{o}' for o in allowed] + ['kc_capabilities']), [
            ('kc_find', {'query': 'Shelf'}, None),
            ('kc_refs', {'name': 'Count'}, None),
            ('kc_capabilities', {}, None),
            ('kc_find', {'query': 'Crate', 'repo': str(other)}, 'not_allowed'),
            ('kc_refs', {'name': 'Count', 'repo': 'yard'}, 'not_allowed'),
            ('kc_refs', {'name': f'{other}/src/Crate.cs:4'}, 'not_allowed'),
        ]),
        ([], ['kc'], [
            ('kc', {'op': 'find', 'q': 'Shelf'}, None),
            ('kc', {'op': 'capabilities'}, None),
            ('kc', {'op': 'context', 'q': 'Shelf'}, 'not_allowed'),
            ('kc', {'op': 'note_add', 'q': 'Shelf.Count', 'text': 'x'}, 'not_allowed'),
            ('kc', {'op': 'search', 'q': 'Count'}, 'not_allowed'),
            ('kc', {'op': 'find', 'q': 'Crate', 'repo': str(other)}, 'not_allowed'),
        ]),
    ]
    try:
        for tools_args, expected, cases in sessions:
            server = Client(binary, base_args + tools_args, env, dirs['tmp'])
            watch = SocketWatch(server.process.pid)
            try:
                scope = ((server.init.get('result') or {}).get('capabilities') or {}).get('experimental', {}).get('knowcode/scope')
                if not scope:
                    failures.append('the initialize reply states no scope (capabilities.experimental["knowcode/scope"])')
                else:
                    text = json.dumps(scope)
                    if str(bound.resolve()) not in text or str(other.resolve()) in text:
                        failures.append(f'the stated scope isn\'t the bound repo: {text[:200]}')
                listed = sorted(t['name'] for t in server.tools)
                if listed != expected:
                    failures.append(f'tools listed {listed}, expected {expected}')
                for tool, args, want in cases:
                    error, text, code, _ = server.ask(tool, args)
                    if want is None and error:
                        failures.append(f'{tool} {json.dumps(args)}: refused ({code}) inside the scope: {text[:120]}')
                    if want is not None and (not error or code != want):
                        failures.append(f'{tool} {json.dumps(args)}: {"answered" if not error else code}, expected {want}')
                    if want is None and not error and 'Crate' in text:
                        failures.append(f'{tool} {json.dumps(args)}: the answer names the other repo')
            finally:
                server.close()
                watch.close()
                sockets |= watch.seen
    finally:
        proxy.close()
    if sockets:
        failures.append(f'network sockets opened: {sorted(sockets)[:5]}')
    if proxy.connections:
        failures.append(f'{proxy.connections} connections to the loopback proxy')
    outside = changed(before, digest_tree(work, skip=[dirs['cache']]))
    if outside:
        failures.append(f'{len(outside)} files written outside the cache directory: {outside[:8]}')
    return failures


# --- codes across the replay -----------------------------------------------------------------------------------------

def check_codes(base, cand, strict=False):
    key = lambda c: (c.get('question'), c['tool'], json.dumps(c['args'], sort_keys=True))
    b = {key(c): c for c in base}
    failures = []
    compared = 0
    for c in cand:
        if c['error'] and not c.get('code'):
            failures.append(f'{c["tool"]} {json.dumps(c["args"])[:100]}: refused without a code')
        old = b.get(key(c))
        if old is None:
            continue
        compared += 1
        if old['error'] and c['error'] and old.get('code') and old.get('code') != c.get('code'):
            failures.append(f'{c["tool"]} {json.dumps(c["args"])[:100]}: code {old.get("code")} → {c.get("code")}')
        # The same build twice: every answer's entries the same. Two builds: an answer may change, its codes with it.
        if strict and sorted(old.get('entries') or []) != sorted(c.get('entries') or []):
            failures.append(f'{c["tool"]} {json.dumps(c["args"])[:100]}: entry codes {sorted(old.get("entries") or [])} → '
                            f'{sorted(c.get("entries") or [])}')
        if strict and old['error'] != c['error']:
            failures.append(f'{c["tool"]} {json.dumps(c["args"])[:100]}: {"refused" if old["error"] else "answered"} → '
                            f'{"refused" if c["error"] else "answered"}')
    return failures, compared


def finish(name, failures, extra, out):
    text = '\n'.join([f'{name}: {"pass" if not failures else str(len(failures)) + " failures"}{extra}', *[f'- {f}' for f in failures]])
    print(text)
    if out:
        Path(out).write_text(text + '\n')
    sys.exit(1 if failures else 0)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest='cmd', required=True)
    for name in ('capabilities', 'restricted'):
        p = sub.add_parser(name)
        p.add_argument('--knowcode', required=True)
        p.add_argument('--work', required=True)
        p.add_argument('--out')
    c = sub.add_parser('codes')
    c.add_argument('base')
    c.add_argument('candidate')
    c.add_argument('--same-build', action='store_true', help='the two runs are one build: every answer\'s codes the same')
    c.add_argument('--out')
    args = ap.parse_args()
    if args.cmd == 'capabilities':
        failures, notes, ops = check_capabilities(Path(args.knowcode).resolve(), args.work)
        finish('capabilities', failures, f' ({ops} ops)' + ''.join(f'; {n}' for n in notes), args.out)
    elif args.cmd == 'restricted':
        finish('restricted', check_restricted(Path(args.knowcode).resolve(), args.work), '', args.out)
    else:
        read = lambda p: [json.loads(l) for l in Path(p).read_text().splitlines() if l.strip()]
        failures, compared = check_codes(read(args.base), read(args.candidate), args.same_build)
        finish('codes', failures, f' ({compared} calls compared)', args.out)


if __name__ == '__main__':
    main()
