"""History labels: member spans, and every label family on a small repository built here with a known history."""
import json
from pathlib import Path
import subprocess
import tempfile
import unittest

import history_labels

SOURCE = '''namespace Shop;

public class Cart
{
    // a } in a comment
    public int Total(int a)
    {
        var s = "}{";
        var v = @"a "" } b";
        var c = '}';
        /* } */
        if (a > 0) { return a; }
        return 0;
    }

    public int Twice(int a) => a * 2;

    public abstract void Clear();
}
'''


class SpanTest(unittest.TestCase):
    def test_braces_in_strings_chars_and_comments_are_skipped(self):
        self.assertEqual(history_labels.span(SOURCE, 6, 'Total'), (6, 14))

    def test_expression_bodied_and_abstract_members_end_at_their_semicolon(self):
        self.assertEqual(history_labels.span(SOURCE, 16, 'Twice'), (16, 16))
        self.assertEqual(history_labels.span(SOURCE, 18, 'Clear'), (18, 18))

    def test_a_line_not_declaring_the_name_is_refused(self):
        with self.assertRaises(ValueError):
            history_labels.span(SOURCE, 16, 'Total')


class FamiliesTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.repos = Path(self.tmp.name)
        self.repo = self.repos / 'shop'
        self.repo.mkdir()
        self.git('init', '-q')
        self.commit('ann', {'src/Cart.cs': SOURCE, 'src/Price.cs': 'p0'})
        # Changes Total's body, with Price.
        self.commit('bob', {'src/Cart.cs': SOURCE.replace('return 0;', 'return -1;'), 'src/Price.cs': 'p1'})
        # Changes Twice only, with Tax.
        self.commit('cy', {'src/Cart.cs': SOURCE.replace('return 0;', 'return -1;').replace('a * 2', 'a + a'), 'src/Tax.cs': 't'})
        # Changes Total again, with Price.
        self.commit('bob', {'src/Cart.cs': SOURCE.replace('return 0;', 'return -2;').replace('a * 2', 'a + a'), 'src/Price.cs': 'p2'})
        self.pin = self.git('rev-parse', 'HEAD').strip()

    def tearDown(self):
        self.tmp.cleanup()

    def git(self, *a, env=None):
        return subprocess.run(['git', '-C', str(self.repo), *a], capture_output=True, check=True, text=True, env=env).stdout

    def commit(self, who, files):
        for f, text in files.items():
            p = self.repo / f
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text(text)
        self.git('add', '-A')
        self.git('-c', f'user.name={who}', '-c', f'user.email={who}@example.com', 'commit', '-q', '-m', f'change by {who}')

    def label(self, family, **extra):
        q = {'id': 'q', 'family': family, 'repo': 'shop', 'pin': self.pin,
             'subject': {'file': 'src/Cart.cs', 'line': 6, 'name': 'Total'}, **extra}
        return history_labels.label(q, self.repos)

    def test_symbol_commits_are_the_commits_changing_the_member(self):
        got = self.label('symbol-commits')
        self.assertEqual([c['author'] for c in got['items']], ['bob', 'bob', 'ann'])
        self.assertEqual((got['span']['start'], got['span']['end']), (6, 14))

    def test_symbol_last_and_blame(self):
        self.assertEqual(self.label('symbol-last')['items'][0]['author'], 'bob')
        blame = self.label('symbol-blame')['items']
        self.assertEqual([(b['author'], b['lines']) for b in blame], [('ann', 8), ('bob', 1)])

    def test_symbol_cochange_counts_partners_of_the_members_commits_only(self):
        got = self.label('symbol-cochange', params={'min_shared': 1})
        # Tax changed only with Twice, never with Total.
        self.assertEqual(got['items'], [{'file': 'src/Price.cs', 'shared': 3}])

    def test_file_cochange_counts_every_commit_of_the_file(self):
        q = {'id': 'q', 'family': 'file-cochange', 'repo': 'shop', 'pin': self.pin, 'subject': {'file': 'src/Cart.cs'}, 'params': {'min_shared': 1}}
        got = history_labels.label(q, self.repos)
        self.assertEqual(got['items'], [{'file': 'src/Price.cs', 'shared': 3}, {'file': 'src/Tax.cs', 'shared': 1}])

    def callers(self, items):
        """The callers-history label against a published label of `items` (in a made-up study)."""
        study = Path(self.tmp.name) / 'eval'
        (study / 'made-up' / 'study').mkdir(parents=True, exist_ok=True)
        (study / 'made-up' / 'study' / 'labels.jsonl').write_text(json.dumps({'id': 'made-up-1', 'items': items}) + '\n')
        old, history_labels.STUDIES = history_labels.STUDIES, study
        try:
            return self.label('callers-history', callers='made-up#made-up-1')['items']
        finally:
            history_labels.STUDIES = old

    def test_callers_history_carries_the_published_labels_optional(self):
        # A file with a required caller is required, even beside an optional one; Tax never changed with Total.
        got = self.callers([{'item': 'shop/src/Price.cs:1', 'hop': 1, 'optional': True}, {'item': 'shop/src/Price.cs:2', 'hop': 1},
                            {'item': 'shop/src/Tax.cs:1', 'hop': 1}])
        self.assertEqual([(i['file'], i.get('optional', False)) for i in got], [('src/Price.cs', False)])
        self.assertNotIn('optional', got[0])
        # A file whose every caller is optional is optional.
        got = self.callers([{'item': 'shop/src/Price.cs:1', 'hop': 1, 'optional': True}])
        self.assertEqual([(i['file'], i.get('optional', False)) for i in got], [('src/Price.cs', True)])

    def test_a_clone_off_its_pin_is_refused(self):
        q = {'id': 'q', 'family': 'symbol-last', 'repo': 'shop', 'pin': '0' * 40, 'subject': {'file': 'src/Cart.cs', 'line': 6, 'name': 'Total'}}
        with self.assertRaises(SystemExit):
            history_labels.label(q, self.repos)

    def test_labels_are_deterministic(self):
        a = json.dumps(self.label('symbol-cochange', params={'min_shared': 1}), sort_keys=True)
        b = json.dumps(self.label('symbol-cochange', params={'min_shared': 1}), sort_keys=True)
        self.assertEqual(a, b)


if __name__ == '__main__':
    unittest.main()
