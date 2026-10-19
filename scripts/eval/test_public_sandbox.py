"""Synthetic macOS boundary checks. Native integration is opt-in and makes no model calls."""
import importlib.util
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location('public_macos_sandbox', Path(__file__).with_name('macos_sandbox.py'))
sandbox = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(sandbox)


def fixture(base):
    root = base/'session'
    for name in ['source', 'control', 'profile']:
        (root/name).mkdir(parents=True)
    (root/'session.json').write_text(json.dumps({'source_root': 'source', 'required_client_cwd': 'control', 'empty_profile_directory': 'profile'}))
    (root/'source/public.txt').write_text('public-source-marker\n')
    (base/'labels.txt').write_text('synthetic-label-marker\n')
    return root


class PolicyTests(unittest.TestCase):
    def test_paths_are_parameters_and_mutable_roots_do_not_cover_source(self):
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            root = fixture(base)
            policy = sandbox.Policy(root, ['/bin/cat'])
            self.assertNotIn(str(root), policy.profile)
            self.assertNotIn('network-outbound', policy.profile)
            self.assertNotIn('allow default', policy.profile)
            self.assertIn('(subpath (param "SOURCE"))', policy.profile)
            self.assertNotIn('file-write* (subpath (param "SOURCE"))', policy.profile)
            self.assertFalse(policy.receipt()['runtime_isolation_complete'])
            self.assertFalse(policy.receipt()['source_integrity_verified_here'])

    def test_symlinked_roots_and_session_owned_executables_are_refused(self):
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            root = fixture(base)
            executable = root/'control/program'
            executable.write_text('#!/bin/sh\nexit 0\n')
            executable.chmod(0o700)
            with self.assertRaisesRegex(ValueError, 'outside mutable'):
                sandbox.Policy(root, [executable])
            (root/'source').rename(root/'source-retained')
            (root/'source').symlink_to(root/'source-retained', target_is_directory=True)
            with self.assertRaisesRegex(ValueError, 'symlinked'):
                sandbox.Policy(root, ['/bin/cat'])

    def test_receipt_path_override_and_missing_prepared_receipt_fail_closed(self):
        with tempfile.TemporaryDirectory() as temp:
            root = fixture(Path(temp))
            (root/'session.json').write_text(json.dumps({'source_root': '../labels'}))
            with self.assertRaisesRegex(ValueError, 'contract'):
                sandbox.Policy(root, ['/bin/cat'])
            (root/'session.json').unlink()
            with self.assertRaisesRegex(ValueError, 'receipt'):
                sandbox.Policy(root, ['/bin/cat'])

    def test_no_executable_or_platform_fallback(self):
        with tempfile.TemporaryDirectory() as temp:
            root = fixture(Path(temp))
            with self.assertRaises(ValueError):
                sandbox.Policy(root, [])
            policy = sandbox.Policy(root, ['/bin/cat'])
            with patch.object(sandbox.sys, 'platform', 'linux'):
                with self.assertRaisesRegex(RuntimeError, 'no unsandboxed fallback'):
                    policy.command(['/bin/cat', 'public.txt'])

    def test_directory_replacement_is_rejected_before_launch(self):
        with tempfile.TemporaryDirectory() as temp:
            root = fixture(Path(temp))
            policy = sandbox.Policy(root, ['/bin/cat'])
            (root/'control').rename(root/'control-old')
            (root/'control').symlink_to(root/'profile', target_is_directory=True)
            with patch.object(sandbox.sys, 'platform', 'darwin'):
                with patch.object(Path, 'is_file', return_value=True):
                    with self.assertRaisesRegex(ValueError, 'directory changed'):
                        policy.command(['/bin/cat'])

    def test_replaced_real_directory_is_not_silently_granted_access(self):
        with tempfile.TemporaryDirectory() as temp:
            root = fixture(Path(temp))
            policy = sandbox.Policy(root, ['/bin/cat'])
            (root/'control').rename(root/'control-old')
            (root/'control').mkdir()
            with patch.object(sandbox.sys, 'platform', 'darwin'):
                with patch.object(Path, 'is_file', return_value=True):
                    with self.assertRaisesRegex(ValueError, 'identity changed'):
                        policy.command(['/bin/cat'])


@unittest.skipUnless(sys.platform == 'darwin' and os.environ.get('KNOWCODE_TEST_MACOS_SANDBOX') == '1', 'requires opt-in native macOS sandbox execution')
class NativeBoundaryTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='knowcode-isolation-test-')
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name).resolve()
        self.root = fixture(self.base)
        self.policy = sandbox.Policy(self.root, ['/bin/cat', '/bin/bash', '/usr/bin/nc'])
        self.env = {'PATH': '/usr/bin:/bin', 'LC_ALL': 'C'}

    def run_owned(self, argv):
        return subprocess.run(self.policy.command(argv), cwd=self.root/'control', env=self.env,
                              stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=5)

    def denied(self, result):
        self.assertGreater(result.returncode, 0, (result.returncode, result.stdout, result.stderr))
        self.assertEqual(result.stdout, '')
        self.assertIn('Operation not permitted', result.stderr)

    def test_read_boundary_and_source_state_writes(self):
        read = self.run_owned(['/bin/cat', self.root/'source/public.txt'])
        self.assertEqual((read.returncode, read.stdout), (0, 'public-source-marker\n'))
        self.denied(self.run_owned(['/bin/cat', self.base/'labels.txt']))
        script = 'printf changed > "$1"'
        self.denied(self.run_owned(['/bin/bash', '--noprofile', '--norc', '-c', script, 'probe', self.root/'source/public.txt']))
        control = self.run_owned(['/bin/bash', '--noprofile', '--norc', '-c', script, 'probe', self.root/'control/owned.txt'])
        self.assertEqual(control.returncode, 0, control.stderr)
        self.assertEqual((self.root/'control/owned.txt').read_text(), 'changed')
        self.assertEqual((self.root/'source/public.txt').read_text(), 'public-source-marker\n')
        self.assertEqual((self.base/'labels.txt').read_text(), 'synthetic-label-marker\n')

    def test_symlink_traversal_and_volume_aliases_cannot_read_labels(self):
        link = self.root/'source/escape'
        link.symlink_to(self.base/'labels.txt')
        for path in [link, self.root/'source/../../labels.txt', Path('/System/Volumes/Data'+str(self.base/'labels.txt'))]:
            self.denied(self.run_owned(['/bin/cat', path]))

    def test_unlisted_executable_is_refused_by_builder_and_kernel(self):
        with self.assertRaisesRegex(ValueError, 'not explicitly allowed'):
            self.policy.command(['/usr/bin/id'])
        result = self.run_owned(['/bin/bash', '--noprofile', '--norc', '-c', '/usr/bin/id'])
        self.denied(result)

    def test_default_network_denial_with_own_loopback_control(self):
        with socket.socket() as listener:
            listener.bind(('127.0.0.1', 0))
            listener.listen(2)
            argv = ['/usr/bin/nc', '-z', '-v', '-w', '1', '127.0.0.1', str(listener.getsockname()[1])]
            control = subprocess.run(argv, env=self.env, capture_output=True, text=True, timeout=3)
            self.assertEqual(control.returncode, 0, control.stderr)
            self.denied(self.run_owned(argv))


if __name__ == '__main__':
    unittest.main()
