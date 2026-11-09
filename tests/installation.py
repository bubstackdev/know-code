"""Exercise runtime lifecycle using isolated durable state and a unique launchd label."""
from pathlib import Path
import argparse, atexit, hashlib, json, os, plistlib, shutil, sqlite3, subprocess, tarfile, time

ROOT = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('packages', type=Path)
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
for key in ['KNOW_CODE_RUNTIME_DIR', 'KNOW_CODE_COORDINATOR', 'NODE_PATH', 'NODE_OPTIONS']:
    ENV.pop(key, None)
checks = []

def command(*argv, check=True, timeout=65):
    result = subprocess.run(list(map(str, argv)), cwd=RUN, env=ENV, text=True, capture_output=True, timeout=timeout)
    assert not check or result.returncode == 0, (argv, result.returncode, result.stdout, result.stderr)
    return result

def record(name):
    checks.append(name)
    (RUN/'validation.json').write_text(json.dumps({'checks': checks, 'label': LABEL, 'prefix': str(PREFIX)}, indent=2)+'\n')
    print('installation:', name, flush=True)

archive = next(args.packages.resolve().glob('know-code-runtime-*.tar.gz'))
with tarfile.open(archive) as tar:
    assert all(not Path(m.name).is_absolute() and '..' not in Path(m.name).parts and not m.issym() for m in tar.getmembers())
    tar.extractall(RUN/'source', filter='data')
A = next((RUN/'source').glob('know-code-*'))
HOST = A/'bin/know-code'
LAUNCHER = PREFIX/'bin/know-code'
PLIST = HOME/'Library/LaunchAgents'/f'{LABEL}.plist'
UID = os.getuid()
SERVICE = f'gui/{UID}/{LABEL}'

def lifecycle(*args, check=True):
    return command(HOST, *args, '--prefix', PREFIX, check=check)

def info():
    return json.loads(lifecycle('install-info').stdout)

def status():
    return json.loads(command(LAUNCHER, 'coordinator', 'status').stdout)['workspaces']

def wait(predicate, description, seconds=30):
    until = time.monotonic()+seconds
    while time.monotonic() < until:
        value = status()
        if predicate(value):
            return value
        time.sleep(.05)
    raise AssertionError((description, value))

def cleanup():
    BLOCK.unlink(missing_ok=True)
    # Only this test's randomly named login job and separate data directory.
    subprocess.run(['/bin/launchctl', 'bootout', SERVICE], capture_output=True)
    command(HOST, 'coordinator', 'stop', check=False)
atexit.register(cleanup)

# Observe real development service without ever modifying it.
DEV = f'gui/{UID}/dev.bubstack.know-code'
def development_pid():
    p = subprocess.run(['/bin/launchctl', 'print', DEV], text=True, capture_output=True)
    return next((l.strip() for l in p.stdout.splitlines() if l.strip().startswith('pid = ')), None)
before_dev = development_pid()

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
with sqlite3.connect(DATA/'registry.db') as connection:
    assert connection.execute('select configuration from workspaces').fetchone()[0] == 'Release'
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
started = time.monotonic()
failed = lifecycle('update', '--from', C, check=False)
elapsed = time.monotonic()-started
assert failed.returncode and 'still busy after 30 seconds' in failed.stderr, failed.stderr
assert 29.5 <= elapsed < 38, elapsed
assert info()['selected'] == b and info()['previous'] == a and info()['login_loaded']
assert query()['result']['freshness'] == 'stale'
BLOCK.unlink()
wait(lambda rows: rows[0]['phase']=='idle' and rows[0]['generation']>generation, 'worker resumed after busy update')
# A second save proves the maintenance lease was released, rather than just the held job ending.
prior = status()[0]['generation']
(WORK/'value.ts').write_text('export function answer() { return 44; }\n')
wait(lambda rows: rows[0]['phase']=='idle' and rows[0]['generation']>prior, 'scheduler resumed')
(RUN/'busy-timing.json').write_text(json.dumps({'elapsed_seconds': elapsed, 'stderr': failed.stderr}, indent=2)+'\n')
record('busy_update_waits_30_seconds_keeps_selection_and_resumes_scheduler')

lifecycle('rollback')
assert info()['selected'] == a and info()['previous'] == b
wait(lambda rows: rows and rows[0]['phase']=='idle', 'rollback restart')
lifecycle('rollback')
assert info()['selected'] == b and info()['previous'] == a
record('rollback_is_reversible')

# Simulate a matching-contract runtime that cannot start its coordinator.
BAD = RUN/'candidate-fails-start'
shutil.copytree(A, BAD)
binary = BAD/'bin/know-code'
binary.rename(binary.with_name('know-code-real'))
binary.write_text('#!/bin/sh\nif [ "$1" = coordinator ]; then exit 23; fi\nexec "$(dirname "$0")/know-code-real" "$@"\n')
binary.chmod(0o755)
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
assert DATA.exists() and Path(DATABASE).exists()
lifecycle('uninstall')
assert not LAUNCHER.is_symlink() and not PLIST.exists() and not (PREFIX/'share/know-code/versions').exists()
assert DATA.exists() and Path(DATABASE).exists() and (DATA/'registry.db').exists()
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
print(json.dumps({'passed': len(checks), 'artifacts': str(RUN)}), flush=True)
