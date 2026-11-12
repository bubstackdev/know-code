"""Stateful native consumers exercise live certification without diagnostic APIs."""
from cli_assertions import check_exit
from fixture_trust import grant_fixture
from pathlib import Path
import json, os, selectors, shutil, signal, socket, subprocess, time

ROOT = Path(__file__).resolve().parents[1]
PROFILE = os.environ.get('KNOW_CODE_BUILD_PROFILE', 'debug')
CONFIGURATION = 'Debug' if PROFILE == 'debug' else 'Release'
HOST = ROOT / f'target/{PROFILE}/know-code'
RUN = ROOT / '.artifacts/lv' / format(time.time_ns(), 'x')
WORKSPACE = RUN / 'workspace'
DATA = RUN / 'd'
WORKSPACE.mkdir(parents=True)
SOURCE = WORKSPACE / 'value.ts'
SOURCE.write_text('export class LiveValue { value = 1; }\n')
REQUEST = json.dumps({'op': 'symbols', 'name': 'LiveValue', 'limit': 100,
                      'freshness_wait_ms': 0, 'deadline_ms': 1000})
ENV = dict(os.environ, KNOW_CODE_DATA_DIR=str(DATA), DOTNET_CLI_TELEMETRY_OPTOUT='1')
ENV['KNOW_CODE_QUERY_SCOPE']=str(RUN/'query-admission')
process = None
checks = []

def command(*args):
    result = subprocess.run([str(HOST), *map(str, args)], cwd=ROOT, env=ENV,
                            capture_output=True, text=True, timeout=120)
    check_exit(result,[HOST,*args])
    return result.stdout

def status():
    result = subprocess.run([str(HOST), 'coordinator', 'status'], env=ENV,
                            capture_output=True, text=True, timeout=5)
    return json.loads(result.stdout) if result.returncode == 0 else None

def ready(minimum=0):
    until = time.monotonic() + 45
    while time.monotonic() < until:
        value = status()
        if value and all(w['phase'] == 'idle' and not w['error'] and
                         w['observed_epoch'] == w['published_epoch'] and
                         w['generation'] >= minimum for w in value['workspaces']):
            return value
        assert process.poll() is None, (RUN/'coordinator.stderr.log').read_text()
        time.sleep(.05)
    raise AssertionError(('readiness timeout', value))

def start():
    global process
    process = subprocess.Popen([str(HOST), 'coordinator', 'run'], cwd=ROOT, env=ENV,
                               stdout=(RUN/'coordinator.stdout.log').open('a'),
                               stderr=(RUN/'coordinator.stderr.log').open('a'))
    ready()

def stop(kill=False):
    global process
    if process and process.poll() is None:
        if kill:
            process.kill()
        else:
            command('coordinator', 'stop')
        process.wait(timeout=15)
    process = None

class Consumer:
    def __init__(self, host):
        if host == 'node':
            module = (ROOT / f'.artifacts/build/{PROFILE}/node/index.mjs').as_uri()
            js = ('import {Index} from '+json.dumps(module)+';'
                  "import readline from 'node:readline';"
                  'const input=readline.createInterface({input:process.stdin})[Symbol.asyncIterator]();'
                  'const index=new Index(process.argv[1]),request=JSON.parse(process.argv[2]);'
                  'console.log(JSON.stringify(await index.query(request)));'
                  'await input.next();console.log(JSON.stringify(await index.query(request)));process.exit(0);')
            args = ['node', '--input-type=module', '-e', js, DATABASE, REQUEST]
        else:
            args = ['dotnet', str(ROOT/f'bindings/dotnet/Smoke/bin/{CONFIGURATION}/net10.0/Smoke.dll'),
                    DATABASE, REQUEST, '--repeat']
        self.process = subprocess.Popen(args, cwd=ROOT, env=ENV, stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=(RUN/f'{host}.stderr.log').open('a'))
        self.selector = selectors.DefaultSelector()
        self.selector.register(self.process.stdout, selectors.EVENT_READ)
    def read(self):
        assert self.selector.select(10), 'native consumer timeout'
        value = json.loads(self.process.stdout.readline())
        assert 'error' not in value, value
        assert len(value['result']['symbols']) == 1, value
        return value['result']
    def second(self):
        self.process.stdin.write(b'next\n')
        self.process.stdin.flush()
        result = self.read()
        self.close()
        return result
    def close(self):
        if self.process.poll() is None:
            self.process.stdin.close()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()
        self.selector.close()

try:
    start()
    grant_fixture(HOST,WORKSPACE,ENV,RUN,RUN/'fixture-trust.jsonl')
    registered = json.loads(command('watch', WORKSPACE))
    DATABASE = registered['database']
    initial = ready(1)['workspaces'][0]['generation']
    until = time.monotonic()+5
    while not list((DATA/'live').glob('*.page')):
        assert time.monotonic() < until
        time.sleep(.01)
    for host in ['node', 'dotnet']:
        endpoint, held = DATA/'c.sock', DATA/'held.sock'
        endpoint.rename(held)
        try:
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as observer:
                observer.bind(str(endpoint))
                observer.listen(1)
                observer.settimeout(2)
                consumer = Consumer(host)
                try:
                    # No server has accepted or replied. The first query must
                    # use independent live evidence and a one-way activity hint.
                    assert consumer.read()['freshness'] == 'current'
                    connection, _ = observer.accept()
                    with connection:
                        connection.settimeout(2)
                        with connection.makefile('rb') as stream:
                            length = int.from_bytes(stream.read(4), 'little')
                            assert 0 < length <= 4092
                            assert len(stream.read(length)) == length
                            assert stream.read(1) == b''
                    assert consumer.second()['freshness'] == 'current'
                    observer.setblocking(False)
                    try:
                        extra, _ = observer.accept()
                    except BlockingIOError:
                        pass
                    else:
                        extra.close()
                        raise AssertionError('burst queries must coalesce activity hints')
                finally:
                    consumer.close()
        finally:
            endpoint.unlink(missing_ok=True)
            held.rename(endpoint)
        checks.append(host+'_first_current_without_control_reply')
        checks.append(host+'_current_without_control_round_trip')

    for host in ['node', 'dotnet']:
        consumer = Consumer(host)
        try:
            assert consumer.read()['freshness'] == 'current'
            stop(kill=True)
            assert consumer.second()['freshness'] == 'stale'
        finally:
            consumer.close()
        checks.append(host+'_crash_is_stale')
        start()
        ready(initial)

    for host in ['node', 'dotnet']:
        consumer = Consumer(host)
        try:
            before = consumer.read()
            stop(kill=True)
            SOURCE.write_text(SOURCE.read_text()+'\n// offline saved update '+host+'\n')
            start()
            current = ready(before['generation']+1)['workspaces'][0]['generation']
            after = consumer.second()
            assert after['freshness'] == 'current' and after['generation'] == current
        finally:
            consumer.close()
        checks.append(host+'_restart_reconciles_saved_inputs')

    # A stopped owned coordinator cannot complete the freshness RPC. Deadline
    # expiry stops that query; it must not cancel later saved-file indexing.
    before = ready()['workspaces'][0]['generation']
    os.kill(process.pid, signal.SIGSTOP)
    try:
        expired = json.loads(command('query', DATABASE, json.dumps({
            'op':'symbols','name':'LiveValue','freshness_wait_ms':120000,'deadline_ms':50})))
        assert expired['error']['code']=='deadline_exceeded',expired
    finally:
        os.kill(process.pid, signal.SIGCONT)
    SOURCE.write_text(SOURCE.read_text()+'\n// saved after query deadline\n')
    ready(before+1)
    checks.append('deadline_during_freshness_does_not_cancel_saved_indexing')

    # A failed optional live directory must leave baseline coordination usable.
    stop()
    shutil.rmtree(DATA/'live')
    (DATA/'live').write_text('optional acceleration is unavailable')
    start()
    ready(initial)
    for host in ['node', 'dotnet']:
        consumer = Consumer(host)
        try:
            assert consumer.read()['freshness'] == 'current'
            assert consumer.second()['freshness'] == 'current'
        finally:
            consumer.close()
        checks.append(host+'_unavailable_live_setup_uses_rpc')
    result = {'profile': PROFILE, 'workspace': str(WORKSPACE), 'database': DATABASE,
              'passed': True, 'checks': checks}
    (RUN/'summary.json').write_text(json.dumps(result, indent=2)+'\n')
    latest = ROOT/'.artifacts/live/latest.json'
    latest.parent.mkdir(exist_ok=True)
    latest.write_text(json.dumps(result, indent=2)+'\n')
    print(json.dumps(result, indent=2))
finally:
    stop()
