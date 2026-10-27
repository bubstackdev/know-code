"""Owned-process cleanup tests use synthetic jobs only; never inspect or signal other sessions."""
import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location('owned_process', Path(__file__).with_name('owned_process.py'))
owned = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(owned)
ENV = {'PATH': '/usr/bin:/bin', 'LC_ALL': 'C'}


class OwnedProcessTests(unittest.TestCase):
    def execute(self, base, code, timeout=5):
        return owned.run([sys.executable, '-I', '-c', code], cwd=base, env=ENV,
                         stdout=base/'stdout.jsonl', stderr=base/'stderr.txt', timeout=timeout)

    def test_success_preserves_exit_status_and_separate_streams(self):
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            result = self.execute(base, 'import sys; print("answer"); print("diagnostic",file=sys.stderr);sys.exit(7)')
            self.assertEqual(result['returncode'], 7)
            self.assertFalse(result['timed_out'])
            self.assertTrue(result['cleanup_complete'])
            self.assertEqual((base/'stdout.jsonl').read_text(), 'answer\n')
            self.assertEqual((base/'stderr.txt').read_text(), 'diagnostic\n')

    def test_explicit_uncapped_run_keeps_owned_cleanup(self):
        with tempfile.TemporaryDirectory() as temp:
            result=self.execute(Path(temp),'print("complete")',timeout=None)
            self.assertEqual(result['returncode'],0)
            self.assertFalse(result['timed_out'])
            self.assertTrue(result['cleanup_complete'])

    def test_timeout_cleans_own_group_and_preserves_another_owned_control_job(self):
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            control = subprocess.Popen([sys.executable, '-I', '-c', 'import time; time.sleep(30)'], env=ENV)
            try:
                result = self.execute(base, 'import time; time.sleep(30)', timeout=0.3)
                self.assertTrue(result['timed_out'])
                self.assertIsNone(result['returncode'])
                self.assertTrue(result['cleanup_complete'])
                self.assertIsNone(control.poll(), 'supervisor touched the separate control job')
            finally:
                # This exact control process was started by this test; no process-table lookup is used.
                control.terminate()
                control.wait(timeout=5)

    def test_normal_completion_cleans_still_running_descendants(self):
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            trigger, marker = base/'trigger', base/'descendant-finished'
            descendant = ('import pathlib,time; trigger=pathlib.Path('+repr(str(trigger))+'); '
                          'marker=pathlib.Path('+repr(str(marker))+'); start=time.monotonic()\n'
                          'while not trigger.exists() and time.monotonic()-start<5: time.sleep(.01)\n'
                          'if trigger.exists(): marker.write_text("survived")\n')
            code = 'import subprocess,sys;subprocess.Popen([sys.executable,"-I","-c",'+repr(descendant)+']);print("complete")'
            result = self.execute(base, code)
            self.assertEqual(result['returncode'], 0)
            self.assertTrue(result['cleanup_complete'])
            trigger.touch()
            time.sleep(0.2)
            self.assertFalse(marker.exists(), 'a descendant survived normal-session cleanup')

    def test_missing_executable_is_recorded_and_output_is_not_overwritten(self):
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            result = owned.run([str(base/'missing')], cwd=base, env=ENV, stdout=base/'out', stderr=base/'err', timeout=2)
            self.assertEqual(result['spawn_error'], 'FileNotFoundError')
            self.assertTrue(result['cleanup_complete'])
            (base/'out').write_text('retained')
            with self.assertRaisesRegex(ValueError, 'existing evidence'):
                owned.run(['/bin/true'], cwd=base, env=ENV, stdout=base/'out', stderr=base/'other', timeout=2)
            self.assertEqual((base/'out').read_text(), 'retained')

    def test_invalid_deadlines_and_reused_stream_paths_do_not_launch(self):
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            for deadline in [0, -1, True, float('inf')]:
                with self.assertRaises(ValueError):
                    owned.run(['/bin/true'], cwd=base, env=ENV, stdout=base/'out', stderr=base/'err', timeout=deadline)
            with self.assertRaises(ValueError):
                owned.run(['/bin/true'], cwd=base, env=ENV, stdout=base/'same', stderr=base/'same', timeout=1)
            self.assertFalse((base/'out').exists())


    def test_unverified_group_is_never_signalled(self):
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            with patch.object(owned.os, 'getpgid', return_value=-1):
                with patch.object(owned.os, 'killpg') as group_signal:
                    with self.assertRaisesRegex(RuntimeError, 'unverified group'):
                        owned.run(['/bin/true'], cwd=base, env=ENV, stdout=base/'out', stderr=base/'err', timeout=2)
                    group_signal.assert_not_called()

    def test_malformed_wrapper_status_still_cleans_up_before_raising(self):
        wrapper = 'import os,sys;os.write(int(sys.argv[1]),b"invalid-json\\n");sys.stdin.read(1)'
        with tempfile.TemporaryDirectory() as temp:
            with patch.object(owned, 'WRAPPER', wrapper):
                with self.assertRaises(ValueError):
                    self.execute(Path(temp), 'pass')


if __name__ == '__main__':
    unittest.main()
