"""Task-answers gate: labels from a commit, and scoring an answer against them, on made-up data."""
from pathlib import Path
import subprocess
import tempfile
import unittest

import task_gate
import task_labels


def commit(repo, files, message):
    for path, text in files.items():
        p = Path(repo)/path
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(text)
    task_labels.git(repo, 'add', '-A')
    task_labels.git(repo, '-c', 'user.name=t', '-c', 'user.email=t@localhost', 'commit', '-q', '-m', message)
    return task_labels.git(repo, 'rev-parse', 'HEAD').strip()


class LabelsTest(unittest.TestCase):
    def test_a_fix_commits_existing_source_and_tests(self):
        with tempfile.TemporaryDirectory() as repo:
            task_labels.git(repo, 'init', '-q')
            commit(repo, {'src/Shelf.cs': 'a', 'src/Desk.cs': 'a', 'test/ShelfTests.cs': 'a', 'README.md': 'a'}, 'start')
            sha = commit(repo, {'src/Shelf.cs': 'b', 'src/New.cs': 'b', 'test/ShelfTests.cs': 'b', 'README.md': 'b'},
                         'Fix the shelf count when empty')
            # The added file can't be named at the parent; the readme isn't source.
            self.assertEqual(task_labels.labels(repo, sha, 'csharp'), (['src/Shelf.cs'], ['test/ShelfTests.cs']))
            only_tests = commit(repo, {'test/ShelfTests.cs': 'c'}, 'Fix a test')
            self.assertIsNone(task_labels.labels(repo, only_tests, 'csharp'))

    def test_fix_subjects(self):
        fix = lambda s: bool(task_labels.FIX.search(s)) and not task_labels.NOT_A_FIX.search(s)
        self.assertTrue(fix('Fix null reference when saving plugin configuration'))
        self.assertTrue(fix('fix(jwt): make the header typ field optional'))
        for s in ('Fix CI workflow for Android', 'fix: resolve CA1849 synchronous IO', 'Fix merge conflict',
                  'Fix typo in docs', 'Bump deps to fix build'):
            self.assertFalse(fix(s), s)

    def test_tests_by_path(self):
        for p in ('test/Serilog.Tests/A.cs', 'src/a.test.ts', 'Tests/NIOCoreTests/B.swift', 'src/__tests__/c.tsx', 'src/ShelfTests.cs'):
            self.assertTrue(task_labels.is_test(p), p)
        for p in ('src/Testing/Clock.cs', 'src/contest.ts', 'Sources/NIOCore/Channel.swift'):
            self.assertFalse(task_labels.is_test(p), p)


class ScoringTest(unittest.TestCase):
    def test_named_within_a_budget(self):
        text = 'x' * 10 + 'src/a.cs' + 'y' * 100 + 'src/b.cs'
        got = task_gate.named(text, ['src/a.cs', 'src/b.cs', 'src/c.cs'], (20, 200))
        self.assertEqual(got['src/a.cs'], {'all': True, '20': True, '200': True})
        self.assertEqual(got['src/b.cs'], {'all': True, '20': False, '200': True})
        self.assertEqual(got['src/c.cs'], {'all': False, '20': False, '200': False})

    def test_recall_and_compare(self):
        task = dict(id='t', repo='r', lang='csharp', files=['src/a.cs', 'src/b.cs'], tests=['test/a.cs'])
        quiet = task_gate.judge(dict(task, id='u', tests=[]), 'src/a.cs', (100,))
        loud = task_gate.judge(task, 'src/a.cs src/b.cs test/a.cs', (100,))
        base = task_gate.summarize([quiet], (100,))
        self.assertEqual((base['overall']['files_recall@all'], base['overall']['tests_recall@all']), (0.5, None))
        cand = task_gate.summarize([loud], (100,))
        self.assertEqual(cand['overall']['tests_recall@100'], 1.0)
        table = task_gate.compare(dict(base, tool='kc_locate', args={}, budgets=(100,)), dict(cand, tool='kc_task', args={}, budgets=(100,)))
        self.assertIn('| all | files_recall@all | 0.500 | 1.000 |', table)


if __name__ == '__main__':
    unittest.main()
