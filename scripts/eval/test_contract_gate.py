"""Contract gate: its watchers and the code comparison, on loopback connections and made-up files."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest

import contract_gate


class WatchTest(unittest.TestCase):
    def test_a_childs_loopback_connection_is_seen(self):
        proxy = contract_gate.ProxyListener()
        try:
            child = subprocess.Popen([sys.executable, '-c',
                                      'import socket, time\n'
                                      f's = socket.create_connection(("127.0.0.1", {proxy.port}))\n'
                                      'time.sleep(1.5)\n'])
            watch = contract_gate.SocketWatch(os.getpid())
            child.wait()
            time.sleep(0.2)
            watch.close()
        finally:
            proxy.close()
        self.assertEqual(proxy.connections, 1)
        self.assertTrue(any(str(proxy.port) in s for s in watch.seen), watch.seen)

    def test_files_changed_outside_a_skipped_directory(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            (root/'cache').mkdir()
            (root/'a.txt').write_text('a')
            before = contract_gate.digest_tree(root, skip=[root/'cache'])
            (root/'cache'/'index').write_text('x')
            self.assertEqual(contract_gate.changed(before, contract_gate.digest_tree(root, skip=[root/'cache'])), [])
            (root/'b.txt').write_text('b')
            (root/'a.txt').write_text('A')
            self.assertEqual(contract_gate.changed(before, contract_gate.digest_tree(root, skip=[root/'cache'])), ['a.txt', 'b.txt'])


class CodesTest(unittest.TestCase):
    def call(self, args, error=False, code=None, entries=()):
        return dict(question='q1', tool='kc_refs', args=args, error=error, code=code, entries=list(entries))

    def test_codes_stay_and_refusals_are_coded(self):
        base = [self.call({'name': 'a'}, True, 'not_found'), self.call({'name': 'b'}, entries=['capped'])]
        same = [self.call({'name': 'a'}, True, 'not_found'), self.call({'name': 'b'}, entries=['capped'])]
        self.assertEqual(contract_gate.check_codes(base, same), ([], 2))
        moved = [self.call({'name': 'a'}, True, 'invalid_argument'), self.call({'name': 'b'}, entries=['cut']),
                 self.call({'name': 'c'}, True, None)]
        failures, compared = contract_gate.check_codes(base, moved)
        # Two builds: the refusal's new code and the uncoded refusal; the changed answer's entries may change.
        self.assertEqual((len(failures), compared), (2, 2))
        self.assertEqual(len(contract_gate.check_codes(base, moved, strict=True)[0]), 3)


if __name__ == '__main__':
    unittest.main()
