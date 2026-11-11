"""Exercise runtime lifecycle using isolated durable state and a unique launchd label."""
from pathlib import Path
import argparse, atexit, fcntl, hashlib, json, os, plistlib, shutil, sqlite3, subprocess, tarfile, threading, time

from index_paths import physical_database

ROOT = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('packages', type=Path)
parser.add_argument('--prior-packages', type=Path, help='certified real lifecycle1 runtime archives for isolated no-login upgrade/rollback checks')
parser.add_argument('--prefix', type=Path, required=True, help='new durable test directory outside checkout')
args = parser.parse_args()
RUN = args.prefix.resolve()
assert ROOT not in RUN.parents and RUN != ROOT and '/tmp/' not in str(RUN)
RUN.mkdir(parents=True, exist_ok=False)
HOME = RUN/'home'
HOME.mkdir()
PREFIX = HOME/'.local'
DATA = RUN/'d'
WORK = RUN/'w'
WORK.mkdir()
BLOCK = RUN/'block'
LABEL = 'dev.bubstack.know-code.test.' + hashlib.sha256(str(RUN).encode()).hexdigest()[:12]
ENV = dict(os.environ, HOME=str(HOME), KNOW_CODE_DATA_DIR=str(DATA), DOTNET_CLI_TELEMETRY_OPTOUT='1', DOTNET_NOLOGO='1', MSBUILDDISABLENODEREUSE='1', DOTNET_CLI_USE_MSBUILD_SERVER='0')
ENV['KNOW_CODE_QUERY_SCOPE']=str(RUN/'query-admission')
for key in ['KNOW_CODE_RUNTIME_DIR', 'KNOW_CODE_COORDINATOR', 'NODE_PATH', 'NODE_OPTIONS']:
    ENV.pop(key, None)
checks = []
PRIOR_CONTEXT = None
EVIDENCE = {
    'harness_sha256': hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
    'index_paths_sha256': hashlib.sha256((ROOT/'tests/index_paths.py').read_bytes()).hexdigest(),
    'commands': str(RUN/'commands.jsonl'),
}

def command(*argv, check=True, timeout=65, env=ENV, gate_trace=None):
    argv = list(map(str, argv))
    started = time.time_ns()
    process = None
    try:
        if gate_trace is None:
            result = subprocess.run(argv, cwd=RUN, env=env, text=True, capture_output=True, timeout=timeout)
        else:
            process = subprocess.Popen(argv, cwd=RUN, env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            gate_trace['command_pid'] = process.pid
            gate_trace['command_started_monotonic'] = time.monotonic()
            stdout, stderr = process.communicate(timeout=timeout)
            result = subprocess.CompletedProcess(argv, process.returncode, stdout, stderr)
    except subprocess.TimeoutExpired as error:
        if process is not None:
            process.kill()
            process.communicate(timeout=5)
        output = {key: value.decode(errors='replace') if isinstance(value, bytes) else value
                  for key, value in [('stdout', error.stdout), ('stderr', error.stderr)]}
        with (RUN/'commands.jsonl').open('a') as log:
            log.write(json.dumps(dict(argv=argv, started_ns=started, finished_ns=time.time_ns(), timed_out=True, **output))+'\n')
        raise
    with (RUN/'commands.jsonl').open('a') as log:
        log.write(json.dumps({'argv': argv, 'started_ns': started, 'finished_ns': time.time_ns(),
                             'home': env['HOME'], 'data': env['KNOW_CODE_DATA_DIR'],
                             'returncode': result.returncode, 'stdout': result.stdout, 'stderr': result.stderr,
                             'command_pid': process.pid if process is not None else None})+'\n')
    assert not check or result.returncode == 0, (argv, result.returncode, result.stdout, result.stderr)
    return result

def record(name):
    checks.append(name)
    (RUN/'validation.json').write_text(json.dumps({'checks': checks, 'label': LABEL, 'prefix': str(PREFIX),
                                                 'evidence': EVIDENCE}, indent=2)+'\n')
    print('installation:', name, flush=True)

def extract_runtime(packages, destination):
    packages = packages.resolve()
    manifest_file = packages/'manifest.json'
    manifest = json.loads(manifest_file.read_text())
    archive = next(packages.glob('know-code-runtime-*.tar.gz'))
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    artifact = next(item for item in manifest['artifacts'] if item['file'] == archive.name)
    assert digest == artifact['sha256'] and archive.stat().st_size == artifact['bytes'], artifact
    with tarfile.open(archive) as tar:
        assert all(not Path(m.name).is_absolute() and '..' not in Path(m.name).parts and not m.issym() for m in tar.getmembers())
        tar.extractall(destination, filter='data')
    return next(destination.glob('know-code-*')), {
        'packages': str(packages), 'runtime_archive': str(archive), 'runtime_sha256': digest,
        'manifest_sha256': hashlib.sha256(manifest_file.read_bytes()).hexdigest(),
        'source_revision': manifest['source_revision'], 'build_provenance': manifest['build_provenance'],
    }

A, EVIDENCE['candidate'] = extract_runtime(args.packages, RUN/'source')
HOST = A/'bin/know-code'
LAUNCHER = PREFIX/'bin/know-code'
PLIST = HOME/'Library/LaunchAgents'/f'{LABEL}.plist'
UID = os.getuid()
SERVICE = f'gui/{UID}/{LABEL}'

def lifecycle(*args, check=True, gate_trace=None):
    return command(HOST, *args, '--prefix', PREFIX, check=check, gate_trace=gate_trace)

def info():
    return json.loads(lifecycle('install-info').stdout)

def status(launcher=LAUNCHER, env=ENV):
    return json.loads(command(launcher, 'coordinator', 'status', env=env).stdout)['workspaces']

def wait(predicate, description, seconds=30, launcher=LAUNCHER, env=ENV):
    until = time.monotonic()+seconds
    while time.monotonic() < until:
        value = status(launcher, env)
        if predicate(value):
            return value
        time.sleep(.05)
    raise AssertionError((description, value))

def cleanup():
    BLOCK.unlink(missing_ok=True)
    # Only this test's randomly named login job and separate data directory.
    subprocess.run(['/bin/launchctl', 'bootout', SERVICE], capture_output=True)
    command(HOST, 'coordinator', 'stop', check=False)
    if PRIOR_CONTEXT:
        prior_host, prior_env = PRIOR_CONTEXT
        # Exactly one normal client matches whichever isolated coordinator is
        # selected after a failed assertion. Neither can address live state.
        for binary in [HOST, prior_host]:
            command(binary, 'coordinator', 'stop', check=False, env=prior_env)
atexit.register(cleanup)

# Observe real development service without ever modifying it.
DEV = f'gui/{UID}/dev.bubstack.know-code'
def development_pid():
    p = subprocess.run(['/bin/launchctl', 'print', DEV], text=True, capture_output=True)
    return next((l.strip() for l in p.stdout.splitlines() if l.strip().startswith('pid = ')), None)
before_dev = development_pid()
EVIDENCE['candidate']['runtime_info'] = json.loads(command(HOST, 'runtime-info').stdout)
assert EVIDENCE['candidate']['runtime_info']['index_lifecycle_version'] == 3
EVIDENCE['candidate']['host_sha256'] = hashlib.sha256(HOST.read_bytes()).hexdigest()

# Simulate a matching-contract runtime that cannot start its coordinator.
BAD = RUN/'candidate-fails-start'
shutil.copytree(A, BAD)
binary = BAD/'bin/know-code'
binary.rename(binary.with_name('know-code-real'))
binary.write_text('#!/bin/sh\nif [ "$1" = coordinator ]; then exit 23; fi\nexec "$(dirname "$0")/know-code-real" "$@"\n')
binary.chmod(0o755)
EVIDENCE['failed_start_candidate'] = {'path': str(BAD), 'wrapper_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
                                       'runtime_binary_sha256': hashlib.sha256(binary.with_name('know-code-real').read_bytes()).hexdigest()}

def prior_lifecycle_upgrade():
    global PRIOR_CONTEXT
    prior = RUN/'prior'
    prior_home = prior/'home'
    prior_home.mkdir(parents=True)
    prior_prefix = prior_home/'.local'
    prior_data = prior/'d'
    prior_work = prior/'w'
    prior_work.mkdir()
    prior_env = dict(ENV, HOME=str(prior_home), KNOW_CODE_DATA_DIR=str(prior_data))
    prior_bundle, evidence = extract_runtime(args.prior_packages, prior/'source')
    EVIDENCE['prior'] = evidence
    prior_host = prior_bundle/'bin/know-code'
    prior_launcher = prior_prefix/'bin/know-code'
    prior_label = LABEL+'.prior'
    prior_plist = prior_home/'Library/LaunchAgents'/f'{prior_label}.plist'
    PRIOR_CONTEXT = (prior_host, prior_env)
    evidence.update(prefix=str(prior_prefix), data=str(prior_data), workspace=str(prior_work), label=prior_label,
                    runtime_info=json.loads(command(prior_host, 'runtime-info', env=prior_env).stdout),
                    host_sha256=hashlib.sha256(prior_host.read_bytes()).hexdigest())
    assert evidence['runtime_info']['index_lifecycle_version'] == 1, evidence
    command(prior_host, 'install', '--from', prior_bundle, '--prefix', prior_prefix,
            '--label', prior_label, '--no-login', env=prior_env)

    def prior_info(binary=HOST):
        return json.loads(command(binary, 'install-info', '--prefix', prior_prefix, env=prior_env).stdout)

    def prior_wait(after=1):
        return wait(lambda rows: len(rows)==1 and rows[0]['phase']=='idle'
                    and rows[0]['observed_epoch']==rows[0]['published_epoch'] and rows[0]['generation']>=after,
                    'prior-runtime saved publication', launcher=prior_launcher, env=prior_env)[0]

    (prior_work/'value.ts').write_text('export function priorAnswer() { return 42; }\n')
    (prior_work/'tsconfig.json').write_text('{"compilerOptions":{"strict":true,"noEmit":true},"files":["value.ts"]}')
    registered = json.loads(command(prior_launcher, 'watch', prior_work, env=prior_env).stdout)
    database = registered['database']
    selector = Path(database).with_suffix('.lifecycle.json')
    initial = prior_wait()
    request = '{"op":"symbols","name":"priorAnswer","freshness_wait_ms":0}'

    def prior_query(binary=prior_launcher):
        envelope = json.loads(command(binary, 'query', database, request, env=prior_env).stdout)
        assert envelope.get('result', {}).get('symbols') and envelope['result']['freshness']=='current', envelope
        return envelope['result']

    prior_query(prior_host)
    old_info = prior_info(prior_host)
    old = old_info['selected']
    assert old_info['previous'] is None and not old_info['login_enabled'] and not old_info['login_loaded']
    assert not prior_plist.exists() and json.loads(selector.read_text())['version']==1
    evidence.update(database=database, initial_generation=initial['generation'], selected_prior=old)
    record('prior_lifecycle1_real_runtime_and_compiler_graph_without_login')

    # This real new runtime reports lifecycle3, then its owned wrapper fails
    # before coordinator startup can write a format2 receipt.
    receipt = selector.read_bytes()
    failed = command(HOST, 'update', '--from', BAD, '--prefix', prior_prefix, env=prior_env, check=False)
    assert failed.returncode and 'previous selection restored' in failed.stderr, failed.stderr
    restored = prior_info()
    assert restored['selected']==old and restored['previous'] is None
    assert not restored['login_enabled'] and not restored['login_loaded'] and not prior_plist.exists()
    assert selector.read_bytes()==receipt and json.loads(selector.read_text())['version']==1
    assert prior_wait()['generation']==initial['generation']
    prior_query(prior_host)
    evidence['failed_start'] = {'returncode': failed.returncode, 'stderr': failed.stderr,
                                'unchanged_selector_sha256': hashlib.sha256(receipt).hexdigest()}
    record('prior_lifecycle1_failed_start_restores_before_new_state_writes')

    command(HOST, 'update', '--from', A, '--prefix', prior_prefix, env=prior_env)
    selected = prior_info()
    assert selected['selected']!=old and selected['previous']==old
    assert not selected['login_enabled'] and not selected['login_loaded'] and not prior_plist.exists()
    prior_wait()
    prior_query()
    assert json.loads(command(prior_launcher, 'runtime-info', env=prior_env).stdout)['index_lifecycle_version']==3
    rejected = json.loads(command(prior_host, 'query', database, request, env=prior_env).stdout)
    assert rejected.get('error', {}).get('code')=='update_required', rejected
    previous_generation = prior_wait()['generation']
    saved = 'export function priorAnswer() { return 43; }\n'
    (prior_work/'value.ts').write_text(saved)
    current = prior_wait(previous_generation+1)
    found = prior_query()
    assert found['generation']==current['generation'] and json.loads(selector.read_text())['version']==2
    location = found['symbols'][0]['locations'][0]
    source = json.loads(command(prior_launcher, 'query', database,
                        json.dumps({'op':'source','artifact_id':location['artifact_id']}), env=prior_env).stdout)['result']['source']
    assert source['text']==saved and source['content_hash']==hashlib.sha256(saved.encode()).hexdigest(), source
    evidence.update(selected_current=selected['selected'], old_client_rejection=rejected,
                    saved_generation=current['generation'], saved_source_sha256=source['content_hash'])
    record('prior_lifecycle1_bridge_quiesces_then_lifecycle3_rejects_old_client_and_saves')

    receipt = selector.read_bytes()
    failed = command(HOST, 'rollback', '--prefix', prior_prefix, env=prior_env, check=False)
    assert failed.returncode and 'index-lifecycle' in failed.stderr, failed.stderr
    unchanged = prior_info()
    assert unchanged['selected']==selected['selected'] and unchanged['previous']==old
    assert not unchanged['login_enabled'] and not unchanged['login_loaded'] and not prior_plist.exists()
    after_rollback = prior_query()
    assert selector.read_bytes()==receipt and after_rollback['generation']==current['generation']
    assert after_rollback['snapshot']==found['snapshot']
    source_after = json.loads(command(prior_launcher, 'query', database,
                            json.dumps({'op':'source','artifact_id':location['artifact_id']}), env=prior_env).stdout)['result']['source']
    assert source_after==source, (source_after, source)
    with sqlite3.connect(physical_database(database)) as connection:
        assert connection.execute('SELECT current_generation FROM state').fetchone()[0]==current['generation']
    evidence['refused_rollback'] = {'returncode': failed.returncode, 'stderr': failed.stderr,
                                   'unchanged_selector_sha256': hashlib.sha256(receipt).hexdigest()}
    command(HOST, 'uninstall', '--prefix', prior_prefix, env=prior_env)
    assert not prior_launcher.is_symlink() and not prior_plist.exists() and not (prior_data/'c.sock').exists()
    assert physical_database(database).is_file() and (prior_data/'registry.db').is_file()
    record('prior_lifecycle1_rollback_refused_without_runtime_or_index_selection_change')

if args.prior_packages:
    prior_lifecycle_upgrade()

lifecycle('install', '--from', A, '--label', LABEL)
a = info()['selected']
assert Path(a).parent == PREFIX/'share/know-code/versions'
assert LAUNCHER.readlink() == Path('../share/know-code/current/bin/know-code')
assert info()['login_loaded']
plist = plistlib.loads(PLIST.read_bytes())
assert plist['ProgramArguments'] == [str(LAUNCHER), 'coordinator', 'run'] and plist['RunAtLoad'] and plist['KeepAlive']
record('initial_install_and_real_login_job')
lifecycle('update', '--from', A)
assert info()['selected'] == a and info()['previous'] is None
record('identical_update_preserves_selection')

(WORK/'value.ts').write_text('export function answer() { return 42; }\n')
(WORK/'tsconfig.json').write_text('{"compilerOptions":{"strict":true,"noEmit":true},"files":["value.ts"]}')
registered = json.loads(command(LAUNCHER, 'watch', WORK, '--configuration', 'Release').stdout)
DATABASE = registered['database']
ready = wait(lambda rows: rows and rows[0]['phase']=='idle', 'initial index')[0]
generation = ready['generation']

def query():
    return json.loads(command(LAUNCHER, 'query', DATABASE, '{"op":"symbols","name":"answer","freshness_wait_ms":0}').stdout)
assert query()['result']['symbols']
record('registration_and_compiler_graph')

# A test-only worker wait point makes the 30-second busy path deterministic.
B = RUN/'candidate-b'
shutil.copytree(A, B)
worker = B/'lib/know-code/providers/typescript/worker.mjs'
worker.rename(worker.with_name('worker-real.mjs'))
worker.write_text("import fs from 'node:fs';\nconst gate = "+json.dumps(str(BLOCK))+";\nwhile(fs.existsSync(gate)) Atomics.wait(new Int32Array(new SharedArrayBuffer(4)),0,0,50);\nawait import('./worker-real.mjs');\n")
lifecycle('update', '--from', B)
b = info()['selected']
assert b != a and info()['previous'] == a
unchanged = wait(lambda rows: rows and rows[0]['phase']=='idle', 'updated unchanged inputs')[0]
assert unchanged['generation'] == generation and unchanged['database'] == DATABASE
personal_settings = json.loads((Path(DATABASE).parent/'settings.json').read_text())
assert personal_settings['csharp']['configuration'] == 'Release', personal_settings
assert unchanged['requested_settings']['csharp']['configuration'] == 'Release', unchanged
record('update_preserves_index_registration_and_configuration')
lifecycle('update', '--from', B)
assert info()['previous'] == a
record('repeated_update_keeps_rollback_target')

C = RUN/'candidate-c'
shutil.copytree(B, C)
(C/'candidate-note.txt').write_text('test-only distinct runtime candidate\n')
BLOCK.touch()
(WORK/'value.ts').write_text('export function answer() { return 43; }\n')
wait(lambda rows: rows[0]['phase']=='analyzing', 'held worker')
# Runtime copying/hashing precedes the maintenance wait. Observe the actual
# exclusive lifecycle gate without retaining any probe lock across a sample.
gate_trace = {'probe_interval_seconds': .05, 'gate_path': str(DATA/'lifecycle.lock')}
monitor_ready = threading.Event()
monitor_stop = threading.Event()

def observe_maintenance_gate():
    try:
        with (DATA/'lifecycle.lock').open('rb') as gate:
            fcntl.flock(gate, fcntl.LOCK_SH | fcntl.LOCK_NB)
            fcntl.flock(gate, fcntl.LOCK_UN)
            gate_trace['last_free_before'] = time.monotonic()
            monitor_ready.set()
            while not monitor_stop.wait(gate_trace['probe_interval_seconds']):
                now = time.monotonic()
                try:
                    fcntl.flock(gate, fcntl.LOCK_SH | fcntl.LOCK_NB)
                except BlockingIOError:
                    gate_trace.setdefault('first_held', now)
                    gate_trace['last_held'] = now
                else:
                    fcntl.flock(gate, fcntl.LOCK_UN)
                    if 'first_held' in gate_trace:
                        gate_trace['first_free_after'] = now
                        return
                    gate_trace['last_free_before'] = now
    except Exception as error:
        gate_trace['monitor_error'] = repr(error)
    finally:
        monitor_ready.set()

monitor = threading.Thread(target=observe_maintenance_gate, daemon=True)
monitor.start()
assert monitor_ready.wait(2) and 'monitor_error' not in gate_trace, gate_trace
started = time.monotonic()
try:
    failed = lifecycle('update', '--from', C, check=False, gate_trace=gate_trace)
    gate_trace['command_returned_monotonic'] = time.monotonic()
    gate_trace['total_started_monotonic'] = started
    elapsed = gate_trace['command_returned_monotonic']-started
    monitor.join(timeout=2)
finally:
    monitor_stop.set()
    monitor.join(timeout=2)
assert not monitor.is_alive(), gate_trace
busy_timing = {'total_command_seconds': elapsed, 'gate': gate_trace,
               'returncode': failed.returncode, 'stderr': failed.stderr}
(RUN/'busy-timing.json').write_text(json.dumps(busy_timing, indent=2)+'\n')
assert failed.returncode and 'still busy after 30 seconds' in failed.stderr, failed.stderr
assert 'monitor_error' not in gate_trace and 'first_free_after' in gate_trace, gate_trace
# Sampling brackets both lock transitions. Prove the accepted interval across
# that uncertainty rather than treating runtime staging as maintenance time.
lower = gate_trace['last_held']-gate_trace['first_held']
upper = gate_trace['first_free_after']-gate_trace['last_free_before']
assert 29.5 <= lower <= upper < 38, busy_timing
assert info()['selected'] == b and info()['previous'] == a and info()['login_loaded']
assert query()['result']['freshness'] == 'stale'
BLOCK.unlink()
wait(lambda rows: rows[0]['phase']=='idle' and rows[0]['generation']>generation, 'worker resumed after busy update')
# A second save proves the maintenance lease was released, rather than just the held job ending.
prior = status()[0]['generation']
(WORK/'value.ts').write_text('export function answer() { return 44; }\n')
wait(lambda rows: rows[0]['phase']=='idle' and rows[0]['generation']>prior, 'scheduler resumed')
busy_timing.update(gate_held_lower_seconds=lower, gate_held_upper_seconds=upper, resumed_generation=status()[0]['generation'])
(RUN/'busy-timing.json').write_text(json.dumps(busy_timing, indent=2)+'\n')
record('busy_update_waits_30_seconds_keeps_selection_and_resumes_scheduler')

lifecycle('rollback')
assert info()['selected'] == a and info()['previous'] == b
wait(lambda rows: rows and rows[0]['phase']=='idle', 'rollback restart')
lifecycle('rollback')
assert info()['selected'] == b and info()['previous'] == a
record('rollback_is_reversible')

failed = lifecycle('update', '--from', BAD, check=False)
assert failed.returncode and 'previous selection restored' in failed.stderr, failed.stderr
assert info()['selected'] == b and info()['previous'] == a and info()['login_loaded']
wait(lambda rows: rows and rows[0]['phase']=='idle', 'failed update restored coordinator')
record('failed_start_restores_previous_runtime_and_service')

INCOMPATIBLE = RUN/'candidate-incompatible'
shutil.copytree(A, INCOMPATIBLE)
manifest = INCOMPATIBLE/'lib/know-code/runtime.json'
contents = json.loads(manifest.read_text())
contents['worker_protocol'] = 999
manifest.write_text(json.dumps(contents))
assert lifecycle('update', '--from', INCOMPATIBLE, check=False).returncode
assert info()['selected'] == b and info()['previous'] == a
record('incompatible_candidate_is_rejected_before_switch')

# Foreign launchers/plists must survive untouched.
plist_bytes = PLIST.read_bytes()
PLIST.write_bytes(plist_bytes+b'\n')
assert lifecycle('uninstall', check=False).returncode
assert LAUNCHER.exists() and PLIST.read_bytes()==plist_bytes+b'\n'
PLIST.write_bytes(plist_bytes)
other = RUN/'occupied'
(other/'bin').mkdir(parents=True)
(other/'bin/know-code').write_text('foreign launcher\n')
assert command(HOST, 'install', '--prefix', other, '--no-login', '--from', A, check=False).returncode
assert (other/'bin/know-code').read_text()=='foreign launcher\n'
if before_dev is not None:
    refused = command(HOST, 'install', '--prefix', RUN/'occupied-service', '--from', A, check=False)
    assert refused.returncode and 'unmanaged login job' in refused.stderr, refused.stderr
    assert development_pid() == before_dev
record('unmanaged_files_are_not_replaced')

lifecycle('service', 'disable')
assert not info()['login_enabled'] and not info()['login_loaded'] and not PLIST.exists()
assert command(LAUNCHER, 'coordinator', 'status', check=False).returncode
# CLI uses the same guarded automatic startup as SDKs.
command(LAUNCHER, 'watch', WORK, '--configuration', 'Release')
wait(lambda rows: rows and rows[0]['phase']=='idle', 'on-demand start')
lifecycle('update', '--from', C)
assert not info()['login_loaded'] and info()['selected'] != b
wait(lambda rows: rows and rows[0]['phase']=='idle', 'on-demand update restarted')
failed = lifecycle('update', '--from', BAD, check=False)
assert failed.returncode and 'previous selection restored' in failed.stderr
wait(lambda rows: rows and rows[0]['phase']=='idle', 'on-demand recovery')
record('on_demand_update_and_failure_recovery')
lifecycle('service', 'enable')
assert info()['login_enabled'] and info()['login_loaded']
record('login_disable_and_enable')

before = status()[0]['generation']
assert DATA.exists() and physical_database(DATABASE).is_file()
lifecycle('uninstall')
assert not LAUNCHER.is_symlink() and not PLIST.exists() and not (PREFIX/'share/know-code/versions').exists()
assert DATA.exists() and physical_database(DATABASE).is_file() and (DATA/'registry.db').exists()
assert not info()['installed']
record('uninstall_retains_index_registry_and_settings')
lifecycle('install', '--from', A, '--label', LABEL)
restored = wait(lambda rows: rows and rows[0]['phase']=='idle', 'reinstall restored registration')[0]
assert restored['generation']==before and restored['database']==DATABASE
assert query()['result']['symbols']
record('reinstall_reuses_retained_registration_and_index')
lifecycle('uninstall')
assert development_pid()==before_dev, 'development service was changed'
record('development_service_left_unchanged')
assert len(checks)==15+(4 if args.prior_packages else 0), checks
result = {'passed': True, 'checks': checks, 'current_lifecycle_checks': 15,
          'prior_lifecycle_checks': 4 if args.prior_packages else 0, 'artifacts': str(RUN),
          'label': LABEL, 'prefix': str(PREFIX), 'evidence': EVIDENCE}
(RUN/'validation.json').write_text(json.dumps(result, indent=2)+'\n')
print(json.dumps({'passed': len(checks), 'artifacts': str(RUN)}), flush=True)
