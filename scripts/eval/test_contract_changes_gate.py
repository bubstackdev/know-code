from __future__ import annotations

import hashlib
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))
import contract_changes_gate as g  # noqa: E402

ROWS = [
    dict(verb='GET', path='api/items/{id:int}', consumers=[dict(by='send', file='src/api.ts', line=10),
                                                         dict(by='wrap', file='src/view.js', line=3)]),
    dict(verb='POST', path='api/items', consumers=[]),
]


def answer(*consumers):
    return {'contracts': [{'contract': c, 'consumers': [dict(repo=r, file=f, line=n, verdict=v)]}
                          for c, r, f, n, v in consumers]}


class JudgeTest(unittest.TestCase):
    def test_a_broken_consumer_is_right_on_the_labelled_file_within_the_call(self):
        j = g.judge(ROWS, answer(('GET api/items/{itemId}', 'web', 'src/api.ts', 12, 'broken')), 'web')
        self.assertEqual([x['right'] for x in j['reported']], [True])
        self.assertEqual((j['matched'], j['labels'], j['matched_own'], j['labels_own']), (1, 2, 1, 1))

    def test_a_consumer_too_far_from_the_call_or_of_another_route_is_wrong(self):
        j = g.judge(ROWS, answer(('GET api/items/{id}', 'web', 'src/api.ts', 15, 'broken'),
                                 ('POST api/items', 'web', 'src/api.ts', 10, 'broken'),
                                 ('GET api/items/{id}', 'web', 'src/api.ts', 9, 'broken')), 'web')
        self.assertEqual([x['right'] for x in j['reported']], [False, False, False])
        self.assertEqual(j['matched'], 0)

    def test_other_repos_are_not_scored_and_possible_counts_for_recall_only(self):
        j = g.judge(ROWS, answer(('GET api/items/{id}', 'server', 'src/api.ts', 10, 'broken'),
                                 ('GET api/items/{id}', 'web', 'src/view.js', 3, 'possible')), 'web')
        self.assertEqual([(x['verdict'], x['right']) for x in j['reported']], [('possible', True)])
        t = g.tally([j])
        self.assertEqual((t['broken'], t['precision'], t['matched'], t['matched_own']), (0, None, 1, 0))


class GateTest(unittest.TestCase):
    def test_a_run_with_a_commit_unanswered_does_not_pass_whatever_its_precision(self):
        pooled = dict(precision=1.0)
        self.assertTrue(g.passed(pooled, {}))
        self.assertFalse(g.passed(pooled, {'jellyfin/abc': 'index jellyfin-web: failed'}))
        self.assertFalse(g.passed(dict(precision=0.94), {}))
        self.assertFalse(g.passed(dict(precision=None), {}))


class RowsTest(unittest.TestCase):
    def test_the_readme_records_a_checksum(self):
        self.assertRegex(g.frozen_sha256(), '^[0-9a-f]{64}$')

    def test_only_the_rows_the_readme_records_are_read(self):
        with tempfile.TemporaryDirectory() as d:
            rows, readme = Path(d)/'rows.jsonl', Path(d)/'README.md'
            rows.write_text('{"pair": "made-up", "commit": "c1"}\n')
            readme.write_text(f'shasum -a 256 rows.jsonl   # {hashlib.sha256(rows.read_bytes()).hexdigest()}\n')
            self.assertEqual(g.read_rows(rows, readme), [{'pair': 'made-up', 'commit': 'c1'}])
            rows.write_text('{"pair": "made-up", "commit": "c2"}\n')
            with self.assertRaises(SystemExit):
                g.read_rows(rows, readme)



class ClientCommitTest(unittest.TestCase):
    def test_a_commit_older_than_its_client_has_no_client_to_check_out(self):
        self.assertEqual(g.client_commit([dict(client_commit='')]), '')
        self.assertEqual(g.client_commit([dict(client_commit=''), dict(client_commit='abc')]), 'abc')


if __name__ == '__main__':
    unittest.main()
