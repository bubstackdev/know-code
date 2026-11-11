"""Native saved events -> persistent coordinator -> scoped publication."""
from fixture_trust import grant_fixture
import atexit
import json
import os
from pathlib import Path
import plistlib
import shutil
import socket
import sqlite3
import subprocess
import time

from index_paths import physical_database

ROOT = Path(__file__).resolve().parents[1]
RUN = ROOT / '.artifacts/cd' / hex(time.time_ns())[-10:]
WORKSPACE = RUN / 'workspace'
WORKSPACE.mkdir(parents=True)
ENV = dict(os.environ, KNOW_CODE_DATA_DIR=str(RUN / 'data'), DOTNET_CLI_TELEMETRY_OPTOUT='1', DOTNET_NOLOGO='1')
ENV['KNOW_CODE_QUERY_SCOPE']=str(RUN/'query-admission')
PROFILE = os.environ.get('KNOW_CODE_BUILD_PROFILE', 'debug')
HOST = ROOT / f'target/{PROFILE}/know-code'
for language in ('typescript', 'csharp'):
    shutil.copytree(ROOT / 'tests/fixtures' / language, WORKSPACE / language,
                    ignore=shutil.ignore_patterns('obj', 'bin', 'node_modules'))
independent = WORKSPACE / 'independent'
independent.mkdir()
(independent / 'tsconfig.json').write_text(json.dumps({'compilerOptions': {'target': 'ES2022', 'module': 'NodeNext', 'skipLibCheck': True}, 'files': ['value.ts']}))
(independent / 'value.ts').write_text('export const independent = 7;\n')
consumer = WORKSPACE / 'consumer'
consumer.mkdir()
(consumer / 'Consumer.csproj').write_text('<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><TargetFramework>net10.0</TargetFramework></PropertyGroup><ItemGroup><ProjectReference Include="../csharp/App.csproj" /></ItemGroup></Project>')
(consumer / 'Consumer.cs').write_text('public static class Consumer { public static string Read() => Example.Greeter.Greet("consumer"); }\n')

BASE = RUN / 'shared.json'
BASE.write_text(json.dumps({'compilerOptions': {'noUncheckedIndexedAccess': False}}))
fixture_config = WORKSPACE / 'typescript/tsconfig.json'
initial_config = json.loads(fixture_config.read_text())
initial_config['extends'] = str(BASE)
fixture_config.write_text(json.dumps(initial_config))

def run(args, check=True):
    process = subprocess.run([str(a) for a in args], cwd=ROOT, env=ENV, capture_output=True, text=True, timeout=120)
    assert not check or not process.returncode, (args, process.stdout, process.stderr)
    return process

def stop():
    run([HOST, 'coordinator', 'stop'], check=False)
    until = time.monotonic() + 15
    while (RUN / 'data/c.sock').exists() and time.monotonic() < until:
        time.sleep(.05)

atexit.register(stop)
def status():
    return json.loads(run([HOST, 'coordinator', 'status']).stdout)['workspaces'][0]
def wait(predicate, description, seconds=45):
    until = time.monotonic() + seconds
    last = None
    while time.monotonic() < until:
        last = status()
        if predicate(last): return last
        time.sleep(.05)
    raise AssertionError((description, last))

grant_fixture(HOST, WORKSPACE, ENV, RUN, RUN/'fixture-trust.jsonl')
registered = json.loads(run([HOST, 'watch', WORKSPACE]).stdout)
DATABASE = registered['database']
with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as old_client:
    old_client.settimeout(2)
    old_client.connect(str(RUN / 'data/c.sock'))
    old_client.sendall(b'\x02\x00\x00\x00\x08\x01')
    length = int.from_bytes(old_client.recv(4), 'little')
    response = b''
    while len(response) < length: response += old_client.recv(length - len(response))
    assert b'update_required' in response
ready = wait(lambda s: s['phase'] == 'idle', 'initial registration')
print('coordinator: initial registration ready', flush=True)

def query(request):
    return json.loads(run([HOST, 'query', DATABASE, json.dumps(request)]).stdout)
def owners():
    with sqlite3.connect(physical_database(DATABASE)) as connection:
        return dict(connection.execute('SELECT compilation_id,owner_generation FROM generation_units WHERE generation=(SELECT current_generation FROM state)'))
def source_text(name):
    result = query({'op': 'symbols', 'name': name})['result']
    symbol = next(s for s in result['symbols'] if s['locations'])
    return query({'op': 'source', 'artifact_id': symbol['locations'][0]['artifact_id']})['result']['source']['text']

# Save immediately after first idle: compiler-learned external watches may
# still be changing. Native watch reconfiguration must reconcile this gap;
# a sleep or explicit refresh here would hide a missed-event regression.
before = owners()
source = WORKSPACE / 'typescript/src/greeter.ts'
original = source.read_text()
replacement = source.with_suffix('.swap')
replacement.write_text(original.replace('Hello', 'Welcome'))
os.replace(replacement, source)
saved = wait(lambda s: s['phase'] == 'idle' and s['generation'] > ready['generation'], 'atomic save without consumers')
assert 'Welcome' in source_text('greet')
after = owners()
assert after['ts:typescript/tsconfig.json'] != before['ts:typescript/tsconfig.json']
assert all(after[k] == v for k, v in before.items() if k != 'ts:typescript/tsconfig.json'), (before, after)
csharp = WORKSPACE / 'csharp/Greeter.cs'
csharp.write_text(csharp.read_text().replace('Greet(string name)', 'Greet(string name, int count = 1)'))
cs_ready = wait(lambda s: s['phase'] == 'idle' and s['generation'] > saved['generation'], 'C# dependent invalidation')
cs_owners = owners()
assert cs_owners['cs:consumer/Consumer.csproj:net10.0:Debug'] != after['cs:consumer/Consumer.csproj:net10.0:Debug']
assert cs_owners['ts:independent/tsconfig.json'] == after['ts:independent/tsconfig.json']
assert all('string, int' in s['signature'] for s in query({'op': 'symbols', 'name': 'Greet'})['result']['symbols'])
source.write_text(original.replace('Hello', 'First'))
wait(lambda s: s['phase'] == 'analyzing', 'active update')
started = time.monotonic()
fallback = query({'op': 'symbols', 'name': 'greet', 'freshness_wait_ms': 10, 'deadline_ms': 1000})
assert time.monotonic() - started < .75
assert fallback['result']['freshness'] == 'stale', fallback
source.write_text(original.replace('Hello', 'Final'))
final = wait(lambda s: s['phase'] == 'idle' and s['generation'] > cs_ready['generation'], 'save during analysis')
assert 'Final' in source_text('greet')
config = WORKSPACE / 'typescript/tsconfig.json'
config_text = config.read_text()
config.write_text('{ invalid JSON')
partial = wait(lambda s: s['phase'] == 'idle' and s['generation'] > final['generation'], 'invalid project configuration publishes unavailable coverage')
invalid = query({'op': 'symbols', 'name': 'greet', 'coverage_detail': 'full'})['result']
assert invalid['freshness'] == 'current' and not invalid.get('symbols'), invalid
unavailable = [c for c in invalid['coverage'] if c['project_path'] == 'typescript/tsconfig.json']
assert len(unavailable) == 1 and unavailable[0]['status'] == 'unavailable', invalid
assert any(d['code'] == 'invalid_project_configuration' for d in unavailable[0].get('diagnostics', [])), unavailable
assert all(c['status'] == 'complete' for c in invalid['coverage'] if c['project_path'] != 'typescript/tsconfig.json'), invalid
assert query({'op': 'symbols', 'name': 'independent'})['result']['symbols']
assert not any(c['inferred'] and (c.get('analysis_options') or {}).get('inferredRoot', '').startswith('typescript')
               for c in invalid['coverage']), invalid
time.sleep(.5)
unchanged_partial = status()
assert unchanged_partial['phase'] == 'idle' and unchanged_partial['generation'] == partial['generation'], 'unchanged unavailable project retried without another save'
config.write_text(config_text)
healthy = wait(lambda s: s['phase'] == 'idle' and s['generation'] > partial['generation'], 'repair configuration')
repaired = query({'op': 'symbols', 'name': 'greet', 'coverage_detail': 'full'})['result']
assert repaired['freshness'] == 'current' and repaired['symbols'] and all(c['status'] == 'complete' for c in repaired['coverage']), repaired
away = RUN / 'workspace-away'
WORKSPACE.rename(away)
missing = wait(lambda s: not s['available'], 'missing workspace', seconds=10)
assert missing['generation'] == healthy['generation']
WORKSPACE.mkdir()
time.sleep(2.2)
assert not status()['available'] and 'different workspace' in status()['error']
WORKSPACE.rmdir()
away.rename(WORKSPACE)
returned = wait(lambda s: s['available'] and s['phase'] == 'idle', 'returning workspace')
BASE.write_text(json.dumps({'compilerOptions': {'noUncheckedIndexedAccess': True}}))
returned = wait(lambda s: s['phase'] == 'idle' and s['generation'] > returned['generation'], 'external config save')
BASE.unlink()
missing_base = wait(lambda s: s['phase'] == 'idle' and s['generation'] > returned['generation'], 'missing external config')
assert any(c['language'] == 'typescript' and c['status'] == 'partial' for c in query({'op': 'symbols', 'name': 'greet'})['result']['coverage'])
BASE.write_text(json.dumps({'compilerOptions': {'noUncheckedIndexedAccess': False}}))
returned = wait(lambda s: s['phase'] == 'idle' and s['generation'] > missing_base['generation'], 'external config return')
assert all(c['status'] == 'complete' for c in query({'op': 'symbols', 'name': 'greet'})['result']['coverage'])
print('coordinator: saves, dependent scopes, stale fallback, root identity and external config passed', flush=True)

stable_owners = owners()
stop()
(independent / 'value.ts').write_text('export const independent = 8;\n')
def start():
    process = subprocess.Popen([str(HOST), 'coordinator', 'run'], cwd=ROOT, env=ENV, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    until = time.monotonic() + 5
    while not (RUN / 'data/c.sock').exists() and time.monotonic() < until: time.sleep(.02)
    return process
daemon = start()
restarted = wait(lambda s: s['phase'] == 'idle', 'offline edit reconciliation')
assert restarted['generation'] > returned['generation']
assert '8' in source_text('independent')
assert all(owners()[k] == v for k, v in stable_owners.items() if k != 'ts:independent/tsconfig.json')
duplicate = run([HOST, 'coordinator', 'run'], check=False)
assert duplicate.returncode and 'already owns' in duplicate.stderr
stop(); daemon.wait(timeout=15)
daemon = start()
unchanged = wait(lambda s: s['phase'] == 'idle', 'unchanged restart')
assert unchanged['generation'] == restarted['generation'], 'startup rebuilt unchanged inputs'
request = json.dumps({'op': 'symbols', 'name': 'greet'})
node = json.loads(run(['node', ROOT / f'.artifacts/build/{PROFILE}/node/smoke.mjs', DATABASE, request]).stdout)
dotnet = json.loads(run(['dotnet', ROOT / f'bindings/dotnet/Smoke/bin/{PROFILE.capitalize()}/net10.0/Smoke.dll', DATABASE, request]).stdout)
assert node == dotnet and node['result']['freshness'] == 'current'
plist = plistlib.loads(run([HOST, 'coordinator', 'login-plist']).stdout.encode())
assert plist['RunAtLoad'] and plist['KeepAlive']
assert plist['ProgramArguments'][0] == str(HOST)
# An unexpected extraction failure can leave saved inputs identical to the last
# good manifest. Expected compiler/setup errors instead publish unavailable
# coverage; use a real compiler with an owned extraction fault for this case.
# A no-op rescan cannot repair this failure or relabel the prior graph current.
fake_compiler = WORKSPACE / 'node_modules/typescript'
fake_compiler.mkdir(parents=True)
(fake_compiler / 'package.json').write_text(json.dumps({'name': 'typescript', 'version': '5.9.3', 'main': 'index.cjs'}))
actual_compiler = ROOT / 'node_modules/typescript/lib/typescript.js'
assert actual_compiler.exists()
(fake_compiler / 'index.cjs').write_text('const ts=require(' + json.dumps(str(actual_compiler)) + ');module.exports=new Proxy(ts,{get(target,key){if(key==="createProgram")return ()=>{throw new Error("owned unexpected coordinator extraction fault")};return Reflect.get(target,key)}});\n')
prior = status()
assert run([HOST, 'index', WORKSPACE], check=False).returncode != 0
broken = wait(lambda s: s['phase'] == 'failed', 'compiler failure without saved input changes')
assert broken['generation'] == prior['generation'] and 'owned unexpected coordinator extraction fault' in broken['error'], broken
os.utime(source, None)
rescanned = wait(lambda s: s['observed_epoch'] > broken['observed_epoch'] and s['phase'] == 'failed', 'unchanged-input rescan preserves analyzer failure')
assert rescanned['generation'] == prior['generation'] and rescanned['error'] == broken['error'], rescanned
assert query({'op': 'symbols', 'name': 'greet'})['result']['freshness'] == 'stale'
WORKSPACE.rename(away)
wait(lambda s: not s['available'], 'failed workspace becomes unavailable')
shutil.rmtree(away / 'node_modules/typescript')
away.rename(WORKSPACE)
wait(lambda s: s['available'] and s['phase'] == 'idle' and s['generation'] > prior['generation'], 'returning workspace retries interrupted analysis and publishes repaired facts')

stop(); daemon.wait(timeout=15)
assert query({'op': 'symbols', 'name': 'greet'})['result']['freshness'] == 'stale'

summary = {'result': 'passed', 'database': DATABASE, 'checks': ['save_without_consumers', 'atomic_save', 'unchanged_unit_reuse', 'C#_dependent_targets', 'bounded_stale_fallback', 'saves_during_analysis', 'invalid_project_unavailable_without_stale_merge_or_inferred_fallback', 'unexpected_failure_preserves_graph', 'unchanged_rescan_preserves_analyzer_failure', 'returning_workspace_reanalyzes_after_failure', 'missing_root', 'restart_reconciliation', 'no_op_restart', 'single_owner', 'native_equivalence', 'login_plist', 'incompatible_protocol', 'workspace_identity', 'external_config_delete_return']}
(RUN / 'result.json').write_text(json.dumps(summary, indent=2) + '\n')
(ROOT / '.artifacts/cd/latest.json').write_text(json.dumps(summary, indent=2) + '\n')
print(json.dumps(summary, indent=2))
