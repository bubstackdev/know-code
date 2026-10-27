"""Offline macOS filesystem/exec boundary for prepared public-evaluation sessions.

This preserves the recorded bare-client tool interface. It does not implement the
agent runner, model/API egress, client permissions, MCP/index setup or study freeze.
There is no unsandboxed fallback. Parameter values never become profile source.
"""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import sys


class Policy:
    def __init__(self, session, executables):
        session = Path(session)
        if session.is_symlink() or not session.is_dir():
            raise ValueError('session must be a real prepared directory')
        self.session = session.resolve(strict=True)
        descriptor = self.session/'session.json'
        if descriptor.is_symlink() or not descriptor.is_file():
            raise ValueError('prepared session receipt missing or symlinked')
        receipt = json.loads(descriptor.read_text())
        if (receipt.get('source_root'), receipt.get('required_client_cwd'), receipt.get('empty_profile_directory')) != ('source', 'control', 'profile'):
            raise ValueError('unexpected prepared session directory contract')
        self.paths = {}
        self.identities = {}
        for key, name in [('SOURCE', 'source'), ('CONTROL', 'control'), ('PROFILE', 'profile')]:
            path = self.session/name
            if path.is_symlink() or not path.is_dir():
                raise ValueError('session root missing or symlinked: '+name)
            resolved = path.resolve(strict=True)
            if resolved.parent != self.session:
                raise ValueError('session root escapes prepared directory')
            self.paths[key] = resolved
            metadata = resolved.stat()
            self.identities[key] = (metadata.st_dev, metadata.st_ino)
        self.executables = []
        if not executables or len(executables) > 64:
            raise ValueError('supply a bounded explicit executable list')
        for value in executables:
            path = Path(value).resolve(strict=True)
            if not path.is_file() or not os.access(path, os.X_OK):
                raise ValueError('executable is not an executable regular file')
            if path.is_relative_to(self.session):
                raise ValueError('executables must be outside mutable session state and source')
            if path not in self.executables:
                self.executables.append(path)
        self.parameters = dict(self.paths)
        self.parameters.update({'EXEC'+str(i): path for i, path in enumerate(self.executables)})
        literals = ' '.join('(literal (param "EXEC'+str(i)+'"))' for i in range(len(self.executables)))
        self.profile = '\n'.join([
            '(version 1)', '(deny default)', '(import "system.sb")',
            '(allow process-fork)', '(allow process-exec '+literals+')',
            '(allow file-read-data file-map-executable '+literals+')',
            '(allow sysctl-read)', '(allow file-read-metadata)',
            '(allow file-read-data (subpath (param "SOURCE")))',
            '(allow file-read* file-write* (subpath (param "CONTROL")) (subpath (param "PROFILE")))',
            '(allow signal (target self))', '',
        ])

    def command(self, argv):
        if sys.platform != 'darwin' or not Path('/usr/bin/sandbox-exec').is_file():
            raise RuntimeError('this boundary requires macOS sandbox-exec; no unsandboxed fallback')
        if not argv:
            raise ValueError('missing command')
        executable = Path(argv[0]).resolve(strict=True)
        if executable not in self.executables:
            raise ValueError('command executable was not explicitly allowed')
        # Revalidate directory identity immediately before constructing a launch. The kernel enforces
        # resolved source/state boundaries during execution, including symlink traversal.
        for key, path in self.paths.items():
            if path.is_symlink() or not path.is_dir() or path.resolve(strict=True) != path or path.parent != self.session:
                raise ValueError('session directory changed: '+key)
            metadata = path.stat()
            if (metadata.st_dev, metadata.st_ino) != self.identities[key]:
                raise ValueError('session directory identity changed: '+key)
        command = ['/usr/bin/sandbox-exec']
        for key, value in self.parameters.items():
            command.extend(['-D', key+'='+str(value)])
        return command+['-p', self.profile, str(executable), *map(str, argv[1:])]

    def receipt(self):
        shipped = {}
        for name in ['system.sb', 'dyld-support.sb']:
            path = Path('/System/Library/Sandbox/Profiles')/name
            if path.is_file():
                shipped[name] = hashlib.sha256(path.read_bytes()).hexdigest()
        return {'format': 1, 'stage': 'offline boundary; integrated model/MCP runner and freeze pending',
                'profile_sha256': hashlib.sha256(self.profile.encode()).hexdigest(),
                'parameters': {key: str(path) for key, path in self.parameters.items()},
                'shipped_profile_sha256': shipped, 'model_network_enabled': False,
                'source_integrity_verified_here': False, 'runtime_isolation_complete': False,
                'remaining': ['verify source and frozen study inputs before launch',
                              'configure actual bare-client tool permissions and disable skills/settings discovery',
                              'model/API egress and credential separation', 'separate MCP/index runtime state',
                              'supervise only the evaluation-owned process group and audit transcripts']}
