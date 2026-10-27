"""Supervise only a new process group created by this evaluation session.

The wrapper remains an unreaped session leader until cleanup, reserving its PID/PGID.
No detached timers, process-table sweeps, stops, or signals to pre-existing jobs.
"""
from __future__ import annotations

import json
import math
import os
from pathlib import Path
import select
import signal
import subprocess
import sys
import time

WRAPPER = r'''
import json, os, subprocess, sys
fd = int(sys.argv[1])
try:
    child = subprocess.Popen(sys.argv[2:], stdin=subprocess.DEVNULL, close_fds=True)
    result = {"returncode": child.wait()}
except Exception as exc:
    result = {"returncode": None, "spawn_error": type(exc).__name__}
os.write(fd, (json.dumps(result) + "\n").encode())
os.close(fd)
# The owner kills this group before reaping us. Waiting here keeps the session identity reserved
# even when the research client has exited but its descendants have not.
sys.stdin.buffer.read(1)
'''


def run(argv, *, cwd, env, stdout, stderr, timeout):
    if os.name != 'posix':
        raise RuntimeError('owned process-group supervision requires POSIX; no fallback')
    if signal.getsignal(signal.SIGCHLD) != signal.SIG_DFL:
        raise RuntimeError('requires default SIGCHLD handling so the owned leader is not reaped elsewhere')
    if not argv or not isinstance(env, dict):
        raise ValueError('explicit command and environment required')
    if timeout is not None and (isinstance(timeout, bool) or not isinstance(timeout, (int, float)) or not math.isfinite(timeout) or timeout <= 0):
        raise ValueError('timeout must be positive and finite')
    cwd = Path(cwd).resolve(strict=True)
    stdout, stderr = Path(stdout), Path(stderr)
    if stdout.resolve() == stderr.resolve() or stdout.exists() or stderr.exists() or stdout.is_symlink() or stderr.is_symlink():
        raise ValueError('new, distinct transcript and stderr paths required; existing evidence retained')
    read_fd, write_fd = os.pipe()
    process = None
    result = {'returncode': None, 'timed_out': False, 'cleanup_complete': False}
    started = time.monotonic()
    try:
        with stdout.open('xb') as output, stderr.open('xb') as errors:
            process = subprocess.Popen([sys.executable, '-I', '-c', WRAPPER, str(write_fd), *map(str, argv)],
                                       cwd=cwd, env=env, stdin=subprocess.PIPE, stdout=output, stderr=errors,
                                       start_new_session=True, pass_fds=(write_fd,), close_fds=True)
            os.close(write_fd)
            write_fd = None
            # Never poll/wait/reap this leader before signaling: its reserved PID keeps the PGID unambiguous.
            captured = b''
            until = started + timeout if timeout is not None else None
            try:
                if os.getpgid(process.pid) != process.pid or os.getsid(process.pid) != process.pid:
                    raise RuntimeError('owned wrapper did not acquire its new session/group')
                while b'\n' not in captured:
                    remaining = until-time.monotonic() if until is not None else None
                    if remaining is not None and remaining <= 0:
                        result['timed_out'] = True
                        break
                    ready, _, _ = select.select([read_fd], [], [], min(remaining, 0.25) if remaining is not None else 0.25)
                    if not ready:
                        continue
                    chunk = os.read(read_fd, 4096)
                    if not chunk:
                        raise RuntimeError('owned wrapper exited without terminal status')
                    captured += chunk
                    if len(captured) > 8192:
                        raise RuntimeError('invalid owned-wrapper status')
                if not result['timed_out']:
                    terminal = json.loads(captured.split(b'\n', 1)[0])
                    if not isinstance(terminal, dict) or 'returncode' not in terminal:
                        raise RuntimeError('invalid owned-wrapper terminal status')
                    result.update(terminal)
            finally:
                # The unreaped leader belongs to this invocation. Its group cannot be recycled before wait().
                cleanup_error = None
                try:
                    if os.getpgid(process.pid) != process.pid or os.getsid(process.pid) != process.pid:
                        raise RuntimeError('owned process group changed; refused to signal an unverified group')
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                except Exception as exc:
                    cleanup_error = exc
                    # The original wrapper PID is still unreaped and owned, even when its group cannot be
                    # verified. Kill only that exact child handle; never signal a guessed/replaced group.
                    process.kill()
                finally:
                    try:
                        process.wait(timeout=5)
                    finally:
                        process.stdin.close()
                result['cleanup_complete'] = cleanup_error is None
                if cleanup_error is not None:
                    raise cleanup_error
    finally:
        os.close(read_fd)
        if write_fd is not None:
            os.close(write_fd)
    result['elapsed_seconds'] = time.monotonic()-started
    return result
