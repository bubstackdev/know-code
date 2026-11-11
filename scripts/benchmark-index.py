#!/usr/bin/env python3
"""Development-only macOS indexing measurements; no product instrumentation.

Use an isolated corpus worktree. Saved-file probes restore original bytes.
Retained samples/results live under .artifacts/perf, never system temporary storage.
"""
import argparse
import collections
import ctypes
import hashlib
import json
import os
from pathlib import Path
import signal
import sqlite3
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'tests'))
from index_paths import physical_database


class Usage(ctypes.Structure):
    _fields_ = [('uuid', ctypes.c_uint8 * 16)] + [(n, ctypes.c_uint64) for n in
        ['user', 'system', 'idle', 'interrupt', 'pageins', 'wired', 'resident', 'footprint',
         'start', 'exit', 'child_user', 'child_system', 'child_idle', 'child_interrupt',
         'child_pageins', 'child_elapsed', 'read', 'written']]


class Timebase(ctypes.Structure):
    _fields_ = [('numer', ctypes.c_uint32), ('denom', ctypes.c_uint32)]


LIB = ctypes.CDLL('/usr/lib/libproc.dylib')
BASE = Timebase()
ctypes.CDLL('/usr/lib/libSystem.B.dylib').mach_timebase_info(ctypes.byref(BASE))
CPU_SECONDS = BASE.numer / BASE.denom / 1e9


def usage(pid):
    value = Usage()
    return value if LIB.proc_pid_rusage(pid, 2, ctypes.byref(value)) == 0 else None


def cpu(value):
    return (value.user + value.system + value.child_user + value.child_system) * CPU_SECONDS


def process_tree(root_pid):
    rows = subprocess.check_output(['ps', '-axo', 'pid=,ppid=,comm='], text=True)
    processes = {}
    for line in rows.splitlines():
        parts = line.split(None, 2)
        if len(parts) == 3:
            processes[int(parts[0])] = (int(parts[1]), parts[2])
    descendants = {root_pid}
    while True:
        added = {pid for pid, (parent, _) in processes.items() if parent in descendants}
        if added <= descendants:
            break
        descendants |= added
    result = []
    for pid in descendants:
        value = usage(pid)
        if value:
            result.append({'pid': pid, 'name': Path(processes.get(pid, (0, 'exited'))[1]).name,
                           'footprint': value.footprint, 'rss': value.resident})
    return result


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--profile', choices=['debug', 'release'], default='debug', help='host, native core, and C# worker build profile; indexed configuration is unchanged')
    parser.add_argument('--workspace', required=True, type=Path)
    parser.add_argument('--label', required=True)
    parser.add_argument('--symbol', required=True)
    parser.add_argument('--edit', action='append', default=[], help='relative saved-file probe; original bytes restored')
    parser.add_argument('--mode', choices=['all', 'updates'], default='all')
    parser.add_argument('--data', type=Path, help='reuse benchmark registration/index for a later update-only comparison')
    args = parser.parse_args()
    host = ROOT / f'target/{args.profile}/know-code'
    worker = ROOT / f'providers/dotnet/bin/{args.profile.capitalize()}/net10.0/KnowCode.Provider.DotNet.dll'
    workspace = args.workspace.resolve()
    run = ROOT / '.artifacts/perf' / hex(time.time_ns())[-10:]
    run.mkdir(parents=True)
    data = args.data.resolve() if args.data else run / 'data'
    env = dict(os.environ, KNOW_CODE_DATA_DIR=str(data), DOTNET_CLI_TELEMETRY_OPTOUT='1', DOTNET_NOLOGO='1')
    measurements = []
    metadata = {'label': args.label, 'workspace': str(workspace), 'data': str(data), 'profile': args.profile,
                'commit': subprocess.check_output(['git', '-C', workspace, 'rev-parse', 'HEAD'], text=True).strip(),
                'host_sha256': digest(host),
                'provider_sha256': digest(worker),
                'typescript_worker_sha256': digest(ROOT / 'providers/typescript/worker.mjs'),
                'typescript_sources_sha256': {str(p.relative_to(ROOT)): digest(p) for p in
                    [*sorted((ROOT / 'providers/typescript').glob('*.mjs')), ROOT / 'providers/shared/protocol.mjs']},
                'os': subprocess.check_output(['sw_vers'], text=True),
                'hardware': subprocess.check_output(['sysctl', '-n', 'machdep.cpu.brand_string', 'hw.memsize', 'hw.logicalcpu'], text=True),
                'mach_timebase': {'numer': BASE.numer, 'denom': BASE.denom},
                'sampling_interval_seconds': .25,
                'cache_state': 'shared dependency and filesystem caches not flushed; cold means empty workspace index',
                'measurements': measurements}

    def command(*arguments):
        p = subprocess.run([str(host), *map(str, arguments)], cwd=ROOT, env=env,
                           capture_output=True, text=True, timeout=15)
        if p.returncode:
            raise RuntimeError(p.stderr or p.stdout)
        return json.loads(p.stdout) if p.stdout.strip() else None

    log = (run / 'coordinator.log').open('w')
    daemon = subprocess.Popen([str(host), 'coordinator', 'run'], cwd=ROOT, env=env,
                              stdout=subprocess.DEVNULL, stderr=log, start_new_session=True)
    database = None

    def status():
        return next(s for s in command('coordinator', 'status')['workspaces'] if s['workspace_root'] == str(workspace))

    def checkpoint():
        (run / 'result.json').write_text(json.dumps(metadata, indent=2) + '\n')

    def measure(name, action, after=0, changed_file=None):
        started = time.monotonic()
        initial_usage = usage(daemon.pid)
        initial_cpu = cpu(initial_usage)
        if database:
            after = max(after, status()['generation'])
        action()
        peak_footprint = peak_rss = peak_processes = 0
        phase_seconds = collections.defaultdict(float)
        seen = {}
        previous = started
        announced = started
        with (run / f'{name}.jsonl').open('w') as samples:
            while time.monotonic() - started < 1200:
                state = status()
                now = time.monotonic()
                stage = (state.get('progress') or {}).get('stage', state['phase'])
                phase_seconds[stage] += now - previous
                previous = now
                tree = process_tree(daemon.pid)
                for p in tree:
                    seen[p['pid']] = p['name']
                footprint = sum(p['footprint'] for p in tree)
                rss = sum(p['rss'] for p in tree)
                peak_footprint = max(peak_footprint, footprint)
                peak_rss = max(peak_rss, rss)
                peak_processes = max(peak_processes, len(tree))
                samples.write(json.dumps({'elapsed': round(now - started, 4), 'stage': stage,
                    'phase': state['phase'], 'generation': state['generation'], 'processes': tree}) + '\n')
                if state['phase'] == 'failed':
                    raise RuntimeError(state['error'])
                if state['phase'] == 'idle' and state['generation'] > after:
                    probe = command('query', database, json.dumps({'op': 'symbols', 'name': args.symbol, 'freshness_wait_ms': 0}))['result']
                    if probe['freshness'] == 'current':
                        break
                if now - announced >= 15:
                    print(f'{args.label} {name}: {now-started:.1f}s {stage}, {footprint/2**20:.0f} MiB footprint', flush=True)
                    announced = now
                time.sleep(.25)
            else:
                raise TimeoutError(state)
        elapsed = time.monotonic() - started
        result = probe
        assert result['freshness'] == 'current' and result.get('symbols'), (result['freshness'], result['generation'])
        if changed_file:
            with sqlite3.connect(physical_database(database)) as connection:
                artifact = connection.execute('SELECT a.id FROM generation_units u JOIN unit_artifacts ua ON ua.owner_generation=u.owner_generation AND ua.compilation_id=u.compilation_id JOIN artifacts a ON a.generation=ua.owner_generation AND a.id=ua.artifact_id WHERE u.generation=(SELECT current_generation FROM state) AND a.path=? LIMIT 1', [str(changed_file.relative_to(workspace))]).fetchone()
            assert artifact, changed_file
            source = command('query', database, json.dumps({'op': 'source', 'artifact_id': artifact[0]}))['result']['source']
            assert source['text'] == changed_file.read_bytes().decode('utf-8-sig'), 'published source differs from normalized saved probe'
        with sqlite3.connect(physical_database(database)) as connection:
            recompiled_units = connection.execute('SELECT count(*) FROM generation_units WHERE generation=(SELECT current_generation FROM state) AND owner_generation=generation').fetchone()[0]
        selected_database = physical_database(database)
        database_files = {p.name: p.stat().st_size for p in selected_database.parent.glob(selected_database.name + '*')}
        final_usage = usage(daemon.pid)
        measured = {'name': name, 'wall_seconds': round(elapsed, 4),
                    'coordinator_cpu_seconds': round((final_usage.user+final_usage.system-initial_usage.user-initial_usage.system)*CPU_SECONDS,4),
                    'accounted_process_tree_cpu_seconds': round(cpu(usage(daemon.pid)) - initial_cpu, 4),
                    'peak_sum_physical_footprint_bytes': peak_footprint, 'peak_sum_rss_bytes': peak_rss,
                    'peak_process_count': peak_processes, 'sampled_process_starts_by_name': dict(collections.Counter(seen.values())),
                    'sampled_phase_seconds': dict(phase_seconds), 'generation': state['generation'],
                    'coverage': dict(collections.Counter(c['status'] for c in result['coverage'])),
                    'analysis_configurations': sorted({c['configuration'] for c in result['coverage'] if c['language'] == 'csharp'}),
                    'recompiled_units': recompiled_units,
                    'database_bytes': sum(database_files.values()),
                    'database_files': database_files}
        measurements.append(measured)
        checkpoint()
        print(json.dumps(measured), flush=True)
        return state['generation']

    try:
        until = time.monotonic() + 10
        while not (data / 'c.sock').exists() and time.monotonic() < until:
            if daemon.poll() is not None:
                raise RuntimeError('benchmark coordinator exited; see coordinator.log')
            time.sleep(.05)
        if args.mode == 'all':
            def register():
                nonlocal database
                database = command('watch', workspace)['database']
            generation = measure('cold-index', register)
        else:
            database = command('watch', workspace)['database']
            until = time.monotonic() + 1200
            while time.monotonic() < until:
                current = status()
                if current['phase'] == 'failed':
                    raise RuntimeError(current['error'])
                if current['phase'] == 'idle':
                    break
                time.sleep(.25)
            generation = current['generation']
        # Idle window excludes benchmark clients/sampler from coordinator-tree CPU.
        idle_start = cpu(usage(daemon.pid))
        idle_at = time.monotonic()
        time.sleep(3)
        metadata['idle'] = {'seconds': time.monotonic() - idle_at,
                            'cpu_seconds': cpu(usage(daemon.pid)) - idle_start,
                            'physical_footprint_bytes': usage(daemon.pid).footprint}
        for index, relative in enumerate(args.edit):
            path = workspace / relative
            original = path.read_bytes()
            try:
                generation = measure(f'save-{index}', lambda: path.write_bytes(original + b'\n// Know Code saved-file performance probe\n'), generation, path)
            finally:
                generation = measure(f'restore-{index}', lambda: path.write_bytes(original), generation, path)
            assert path.read_bytes() == original
        metadata['result'] = 'passed'
        checkpoint()
        print('Results: ' + str(run / 'result.json'), flush=True)
    finally:
        try:
            command('coordinator', 'stop')
            daemon.wait(timeout=30)
        except (subprocess.TimeoutExpired, RuntimeError, StopIteration):
            os.killpg(daemon.pid, signal.SIGTERM)
            daemon.wait(timeout=10)
        log.close()


if __name__ == '__main__':
    main()
