#!/usr/bin/env python3
"""Focused compiler-backed Rust worker checks; retain evidence under .artifacts."""
import base64
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import threading
import time

ROOT = Path(__file__).resolve().parents[1]
ARTIFACTS = ROOT / '.artifacts/rust-provider'
ARTIFACTS.mkdir(parents=True, exist_ok=True)
NODE = shutil.which('node')
assert NODE, 'Node.js is required'

HELPER = ROOT / 'tests/worker_harness.mjs'


def run(label, workspace, projects=None, env=None):
    request = {'workspaceRoot': str(workspace), 'projects': [str(p) for p in (projects or [workspace / 'Cargo.toml'])], 'configuration': 'Debug'}
    start = time.monotonic()
    result = subprocess.run([NODE, str(HELPER), str(ROOT / 'providers/rust/worker.mjs')], input=json.dumps(request), text=True, cwd=ROOT,
                            capture_output=True, timeout=190, check=True, env=env)
    payload = json.loads(result.stdout)
    payload['elapsed_seconds'] = time.monotonic() - start
    (ARTIFACTS / f'{label}.json').write_text(json.dumps(payload, indent=2))
    return payload


def facts(payload):
    assert payload['code'] == 0, payload
    assert payload['messages'][-1].get('complete', {}).get('success'), payload
    symbols, occurrences, sources = {}, [], {}
    for frame in payload['messages']:
        if 'facts' in frame:
            symbols.update((s['id'], s) for s in frame['facts'].get('symbols', []))
            occurrences.extend(frame['facts'].get('occurrences', []))
        if 'source' in frame:
            source = frame['source']
            sources[source['id']] = base64.b64decode(source['utf8'])
    for occurrence in occurrences:
        assert occurrence['symbolId'] in symbols
        text = sources[occurrence['artifactId']]
        assert 0 <= occurrence.get('startByte', 0) < occurrence['endByte'] <= len(text)
        text[occurrence.get('startByte', 0):occurrence['endByte']].decode('utf8')
    reports = [f['report'] for f in payload['messages'] if 'report' in f]
    assert reports and all(r['status'] == 'partial' for r in reports)
    assert all(any(d['code'] == 'rust_coverage_limits' for d in r['diagnostics']) for r in reports)
    return symbols, occurrences, sources


workspace = ARTIFACTS / 'fixture'
if workspace.exists():
    shutil.rmtree(workspace)
shutil.copytree(ROOT / 'tests/fixtures/rust', workspace)
# The compiler syntax extension uses byte columns and may normalize line ends;
# retain the original CRLF bytes while LSP navigation stays UTF-16.
helper_source = workspace / 'src/helper.rs'
helper_source.write_bytes(('// 🦀 saved CRLF source\n' + helper_source.read_text()).replace('\n', '\r\n').encode())
clean = run('clean', workspace)
symbols, occurrences, sources = facts(clean)
names = {s['name'] for s in symbols.values()}
assert {'Greeter', 'greet', 'café', 'input', 'expected', 'greeting', 'decorate'} <= names, names
local_ids = [s['id'] for s in symbols.values() if s['name'] == 'café']
assert len(local_ids) == 2, local_ids
assert all(any(o['symbolId'] == sid and o['role'] == 'declaration' for o in occurrences) for sid in local_ids)
assert all(any(o['symbolId'] == sid and o['role'] == 'reference' for o in occurrences) for sid in local_ids)
for occurrence in occurrences:
    symbol = symbols[occurrence['symbolId']]
    spelling = sources[occurrence['artifactId']][occurrence.get('startByte', 0):occurrence['endByte']].decode()
    if symbol['name'] == 'café' and occurrence['role'] != 'definition':
        assert spelling == 'café', spelling
decorate = next(sid for sid, s in symbols.items() if s['name'] == 'decorate')
assert len({o['artifactId'].split('@')[0] for o in occurrences if o['symbolId'] == decorate}) == 2
extents = [o for o in occurrences if o['symbolId'] == decorate and o['role'] == 'definition']
assert extents, 'compiler document outline did not provide the function extent'
assert any('fn decorate' in sources[o['artifactId']][o.get('startByte',0):o['endByte']].decode() for o in extents)
assert all(len(s['signature']) < 4096 for s in symbols.values()), 'hover docs leaked into signatures'
assert any(s.get('external') and s['name'] == 'String' for s in symbols.values())
assert len(sources) == 3, 'dependency source was indexed'
assert not (workspace / 'know-code-build-ran').exists(), 'analysis executed the build script'

# Actual outgoing-call evidence, syntax ownership and unique definitions must
# agree. Navigation references alone are not calls, including a function value.
relationships = [edge for frame in clean['messages'] if 'facts' in frame
                 for edge in frame['facts'].get('relationships', [])]
for edge in relationships:
    assert edge['sourceId'] in symbols and edge['targetId'] in symbols, edge
    assert edge['resolution'] == 'compiler_bound', edge
    text = sources[edge['artifactId']]
    assert 0 <= edge.get('startByte', 0) < edge['endByte'] <= len(text), edge
    text[edge.get('startByte', 0):edge['endByte']].decode('utf8')

def symbol_named(name):
    matches = [sid for sid, symbol in symbols.items() if symbol['name'] == name]
    assert len(matches) == 1, (name, matches)
    return matches[0]

def edges(source, target, kind):
    return [edge for edge in relationships if edge['sourceId'] == symbol_named(source)
            and edge['targetId'] == symbol_named(target) and edge['kind'] == kind]

assert edges('graph_entry', 'graph_leaf', 'call'), 'direct call missing'
assert edges('graph_leaf', 'GRAPH_VALUE', 'reference'), 'constant read missing'
assert edges('graph_value_only', 'graph_leaf', 'reference'), 'function value dependency missing'
assert not edges('graph_value_only', 'graph_leaf', 'call'), 'function value fabricated a call'
assert edges('graph_inner', 'graph_leaf', 'call'), 'nested function lost its call'
assert edges('graph_nested', 'graph_inner', 'call'), 'nested function invocation missing'
assert not edges('graph_nested', 'graph_leaf', 'call'), 'inner call attributed to containing function'
assert not edges('graph_deferred', 'graph_leaf', 'call'), 'closure/async call attributed to containing function'
assert edges('graph_type_reference', 'Greeter', 'type_reference'), 'signature type dependency missing'
assert any(o.get('enclosingSymbolId') == symbol_named('graph_entry')
           and o['symbolId'] == symbol_named('graph_leaf') for o in occurrences), 'enclosing owner missing'
for frame in clean['messages']:
    if 'report' in frame:
        assert frame['report']['capabilities']['calls'] == 'partial'
        assert frame['report']['capabilities']['dependencies'] == 'partial'
        assert frame['report']['capabilities']['dispatch'] == 'partial'


# Same project scope, changed saved text: a replacement has new exact source
# payload/ranges and no old symbol. Storage publication/parity is tested above
# the worker boundary by the root integration suite.
filename = workspace / 'src/helper.rs'
filename.write_text(filename.read_text().replace('decorate', 'wrap'))
main = workspace / 'src/lib.rs'
main.write_text(main.read_text().replace('helper::decorate', 'helper::wrap') + '\npub fn broken() { missing_function(); }\n')
updated = run('updated', workspace)
new_symbols, new_occurrences, new_sources = facts(updated)
assert 'wrap' in {s['name'] for s in new_symbols.values()}
assert 'decorate' not in {s['name'] for s in new_symbols.values()}
assert 'missing_function' not in {s['name'] for s in new_symbols.values()}
assert next(f['compilation']['id'] for f in clean['messages'] if 'compilation' in f) == next(f['compilation']['id'] for f in updated['messages'] if 'compilation' in f)
assert {s for s in sources if s.startswith('src/')}.isdisjoint(new_sources), 'saved source identities did not change'
assert not (workspace / 'know-code-build-ran').exists(), 'saved change executed the build script'

# Discovered member manifests must collapse onto one Cargo workspace owner.
virtual = ARTIFACTS / 'virtual'
if virtual.exists():
    shutil.rmtree(virtual)
(virtual / 'member/src').mkdir(parents=True)
(virtual / 'Cargo.toml').write_text('[workspace]\nmembers = ["member"]\nresolver = "3"\n')
(virtual / 'member/Cargo.toml').write_text('[package]\nname = "fixture-member"\nversion = "0.1.0"\nedition = "2024"\n')
(virtual / 'member/src/lib.rs').write_text('pub fn member_function(value: usize) -> usize { value }\n')
dedup = run('workspace', virtual, [virtual / 'Cargo.toml', virtual / 'member/Cargo.toml'])
facts(dedup)
compilations = [f['compilation'] for f in dedup['messages'] if 'compilation' in f]
assert len(compilations) == 1 and compilations[0]['projectPath'] == 'Cargo.toml', compilations
assert any(p.endswith('member/Cargo.toml') for p in compilations[0]['inputPaths'])

# Registering only a member directory must not silently index an enclosing
# workspace that the host cannot select again during an incremental refresh.
outside = run('outside-registered-root', virtual / 'member')
assert outside['code'] != 0 and 'outside registered root' in outside['stderr']
assert 'register the enclosing' in outside['stderr']
assert not any('compilation' in f or f.get('complete', {}).get('success') for f in outside['messages'])

missing = ARTIFACTS / 'missing-bin'
missing.mkdir(exist_ok=True)
shim = missing / 'rust-analyzer'
shim.write_text('#!/bin/sh\necho "rust-analyzer component unavailable" >&2\nexit 23\n')
shim.chmod(0o755)
env = dict(os.environ, PATH=str(missing) + os.pathsep + os.environ['PATH'])
failed = run('missing-prerequisite', virtual, env=env)
assert failed['code'] != 0 and 'rust-analyzer' in failed['stderr'] and 'install' in failed['stderr']
assert not any(f.get('complete', {}).get('success') for f in failed['messages'])

# Real rustup shims must honor an unavailable repository pin without attempting
# to install it, even when the embedding host allows rustup auto-installation.
pin_workspace = ARTIFACTS / 'missing-pin'
if pin_workspace.exists():
    shutil.rmtree(pin_workspace)
(pin_workspace / 'src').mkdir(parents=True)
(pin_workspace / 'Cargo.toml').write_text('[package]\nname = "missing-pin"\nversion = "0.1.0"\n[workspace]\n')
(pin_workspace / 'src/lib.rs').write_text('pub fn saved() {}\n')
(pin_workspace / 'rust-toolchain.toml').write_text('[toolchain]\nchannel = "1.0.999"\n')
rustup = Path(shutil.which('rustup') or Path.home() / '.cargo/bin/rustup')
assert rustup.is_file(), 'Rustup is required for the missing-pin regression'
pin_bin = pin_workspace / 'tools'
pin_bin.mkdir()
for tool in ('cargo', 'rustc', 'rust-analyzer'):
    (pin_bin / tool).symlink_to(rustup)
install_marker = pin_workspace / 'install-invocation'
rustup_spy = pin_bin / 'rustup'
rustup_spy.write_text(f'#!{sys.executable}\nfrom pathlib import Path\nPath({str(install_marker)!r}).write_text("unexpected rustup invocation")\nraise SystemExit(91)\n')
rustup_spy.chmod(0o755)
requests = []


class DownloadTrap(BaseHTTPRequestHandler):
    def do_GET(self):
        requests.append(self.path)
        self.send_error(404)

    do_HEAD = do_GET

    def log_message(self, *_):
        pass


server = HTTPServer(('127.0.0.1', 0), DownloadTrap)
thread = threading.Thread(target=server.serve_forever, daemon=True)
thread.start()
pin_env = dict(os.environ, PATH=str(pin_bin) + os.pathsep + os.environ['PATH'],
               RUSTUP_HOME=str(pin_workspace / 'rustup-home'), RUSTUP_AUTO_INSTALL='1',
               RUSTUP_DIST_SERVER=f'http://127.0.0.1:{server.server_port}',
               RUSTUP_UPDATE_ROOT=f'http://127.0.0.1:{server.server_port}/rustup')
for proxy in ('HTTP_PROXY', 'HTTPS_PROXY', 'ALL_PROXY', 'http_proxy', 'https_proxy', 'all_proxy'):
    pin_env[proxy] = ''
pin_env['NO_PROXY'] = pin_env['no_proxy'] = '127.0.0.1,localhost'
pin_env.pop('RUSTUP_TOOLCHAIN', None)
try:
    missing_pin = run('missing-pin', pin_workspace, env=pin_env)
finally:
    server.shutdown()
    server.server_close()
    thread.join()
assert missing_pin['code'] != 0 and 'not installed' in missing_pin['stderr'] and 'install the repository-selected' in missing_pin['stderr']
assert not requests, ('rustup attempted a download', requests)
assert not install_marker.exists(), 'provider invoked rustup instead of reporting the prerequisite'
assert not (pin_workspace / 'rustup-home/toolchains').exists(), 'provider installed a compiler'
assert not any(f.get('complete', {}).get('success') for f in missing_pin['messages'])
(ARTIFACTS / 'missing-pin-network.json').write_text(json.dumps({'download_requests': requests, 'install_invocation': False, 'installed_toolchains': False}) + '\n')

# Detect absent selected rust-src before even launching the analyzer, whose
# own prerequisite discovery must never install a missing component.
source_bin = ARTIFACTS / 'missing-source-bin'
source_bin.mkdir(exist_ok=True)
source_marker = source_bin / 'analyzer-launched'
source_marker.unlink(missing_ok=True)
real_rustc = shutil.which('rustc')
assert real_rustc
source_rustc = source_bin / 'rustc'
source_rustc.write_text(f'#!{sys.executable}\nimport os,sys\nif sys.argv[1:] == ["--print", "sysroot"]:\n print({str(source_bin / "sysroot-without-sources")!r})\nelse:\n os.execv({real_rustc!r}, [{real_rustc!r}, *sys.argv[1:]])\n')
source_rustc.chmod(0o755)
source_analyzer = source_bin / 'rust-analyzer'
source_analyzer.write_text(f'#!{sys.executable}\nfrom pathlib import Path\nPath({str(source_marker)!r}).write_text("unexpected analyzer launch")\nraise SystemExit(92)\n')
source_analyzer.chmod(0o755)
missing_source = run('missing-rust-src', virtual, env=dict(os.environ, PATH=str(source_bin) + os.pathsep + os.environ['PATH']))
assert missing_source['code'] != 0 and 'missing rust-src prerequisite' in missing_source['stderr']
assert 'install rust-src separately' in missing_source['stderr'] and not source_marker.exists()
assert not any('compilation' in f or f.get('complete', {}).get('success') for f in missing_source['messages'])

summary = {'passed': True, 'checks': ['compiler-defined method/cross-file references', 'distinct same-named Unicode locals and exact byte ranges',
           'outgoing direct calls with compiler locations', 'value/type dependency kinds and enclosing owners',
           'function values and deferred/nested execution are not fabricated calls',
           'test symbols', 'external identity without dependency source publication', 'saved-source replacement scope',
           'unresolved call does not invent symbol', 'Cargo workspace member deduplication', 'out-of-root workspace rejection', 'build script execution disabled', 'missing analyzer prerequisite', 'uninstalled pin without install/network', 'missing rust-src before analyzer launch'],
           'clean_symbols': len(symbols), 'clean_occurrences': len(occurrences),
           'compiler_version': next(f['compilation']['compilerVersion'] for f in clean['messages'] if 'compilation' in f),
           'elapsed_seconds': {label: payload['elapsed_seconds'] for label, payload in [('clean', clean), ('updated', updated), ('workspace', dedup), ('outside', outside), ('missing', failed)]}}
(ARTIFACTS / 'latest.json').write_text(json.dumps(summary, indent=2))
print(json.dumps(summary, indent=2))
