"""review_labels: the introducing commit a fix's blame names, and the fix's files as labels, on a throwaway repository."""
from __future__ import annotations

import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))
import review_labels  # noqa: E402

DAY = 86400
START = 1700000000


class Repo:
    def __init__(self, root):
        self.root = root
        self.git('init', '-q', '-b', 'main')

    def git(self, *args, at=START):
        env = {**review_labels.task_labels.GIT_ENV, 'GIT_AUTHOR_NAME': 'A', 'GIT_AUTHOR_EMAIL': 'a@example.com',
               'GIT_COMMITTER_NAME': 'A', 'GIT_COMMITTER_EMAIL': 'a@example.com',
               'GIT_AUTHOR_DATE': f'{at} +0000', 'GIT_COMMITTER_DATE': f'{at} +0000'}
        return subprocess.run(['git', *args], cwd=self.root, env=env, check=True, capture_output=True,
                              text=True).stdout.strip()

    def commit(self, message, files, day):
        for path, text in files.items():
            p = Path(self.root)/path
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text(text)
        self.git('add', '-A')
        self.git('commit', '-q', '-m', message, at=START + day * DAY)
        return self.git('rev-parse', 'HEAD')


class IntroducingTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.repo = Repo(self.tmp.name)
        self.repo.commit('Initial commit of the parser', {'src/Parser.cs': 'class Parser {\n  int A() { return 1; }\n}\n',
                                                          'src/Lexer.cs': 'class Lexer {\n}\n'}, 0)

    def tearDown(self):
        self.tmp.cleanup()

    def test_the_blamed_commit_within_the_window_is_the_one_reviewed(self):
        intro = self.repo.commit('Add a fast path to the parser', {
            'src/Parser.cs': 'class Parser {\n  int A() { return 1; }\n  int B() { return A() + 2; }\n}\n',
            'src/Lexer.cs': 'class Lexer {\n  int C() { return 0; }\n}\n'}, 200)
        fix = self.repo.commit('Fix the wrong result of the fast path', {
            'src/Parser.cs': 'class Parser {\n  int A() { return 1; }\n  int B() { return A() + 3; }\n}\n',
            'tests/ParserTests.cs': 'class ParserTests {}\n'}, 230)
        got, why = review_labels.review(self.tmp.name, 'demo', 'csharp', fix)
        self.assertIsNone(why)
        self.assertEqual(got['commit'], intro)
        self.assertEqual(got['parent'], self.repo.git('rev-parse', f'{intro}^'))
        self.assertEqual(got['files'], ['src/Parser.cs'])
        # A test the fix added didn't exist at the introducing commit, so it isn't a label.
        self.assertEqual(got['tests'], [])
        self.assertEqual(got['in_diff'], ['src/Parser.cs'])
        self.assertEqual(got['days'], 30.0)

    def test_a_commit_older_than_the_window_is_no_review(self):
        self.repo.commit('Add a fast path to the parser',
                         {'src/Parser.cs': 'class Parser {\n  int A() { return 2; }\n}\n'}, 10)
        fix = self.repo.commit('Fix the wrong result of the parser',
                               {'src/Parser.cs': 'class Parser {\n  int A() { return 3; }\n}\n'}, 10 + 91)
        got, why = review_labels.review(self.tmp.name, 'demo', 'csharp', fix)
        self.assertIsNone(got)
        self.assertEqual(why, 'none within window')

    def test_a_reformat_is_never_the_introducing_commit(self):
        self.repo.commit('Format the parser', {'src/Parser.cs': 'class Parser {\n  int A()  { return 1 ; }\n}\n'}, 20)
        fix = self.repo.commit('Fix the wrong result of the parser',
                               {'src/Parser.cs': 'class Parser {\n  int A() { return 4; }\n}\n'}, 30)
        got, why = review_labels.review(self.tmp.name, 'demo', 'csharp', fix)
        self.assertIsNone(got)
        self.assertEqual(why, 'none within window')

    def test_trivial_lines_are_not_blamed(self):
        self.assertEqual(review_labels.TRIVIAL.match('  });') is not None, True)
        self.assertIsNone(review_labels.TRIVIAL.match('  return x;'))

    def test_a_pure_addition_has_no_blamed_line(self):
        fix = self.repo.commit('Fix the missing check in the lexer',
                               {'src/Lexer.cs': 'class Lexer {\n  int D() { return 1; }\n}\n'}, 5)
        got, why = review_labels.review(self.tmp.name, 'demo', 'csharp', fix)
        self.assertIsNone(got)
        self.assertEqual(why, 'no blamed line')


class DrawTest(unittest.TestCase):
    def test_a_language_takes_its_repositories_in_turn_up_to_its_quota(self):
        def g(repo, n, lang='csharp'):
            return [dict(id=f'{repo}-{i}', repo=repo, lang=lang, commit=f'{repo}{i}') for i in range(n)]
        pools = {'big': g('big', 10), 'small': g('small', 1), 'other': g('other', 5, 'swift')}
        got = review_labels.draw(pools, {'csharp': 4, 'swift': 2})
        by = {}
        for r in got:
            by[r['repo']] = by.get(r['repo'], 0) + 1
        # The small repository gives all it has; the rest of the language's quota comes from the others.
        self.assertEqual(by, {'big': 3, 'small': 1, 'other': 2})
        self.assertEqual(got, review_labels.draw(pools, {'csharp': 4, 'swift': 2}))


if __name__ == '__main__':
    os.environ.setdefault('LC_ALL', 'C')
    unittest.main()
