import unittest

from compare import score


class RefsTest(unittest.TestCase):
    # D is bound at a.cs:3 and b.cs:7; a.cs:9 holds D's name bound to another declaration.
    LABELS = {('a.cs', 3), ('b.cs', 7)}
    BOUND = {('a.cs', 3): {'D'}, ('b.cs', 7): {'D'}, ('a.cs', 9): {'Other'}}

    def test_sites_are_right_wrong_or_unjudged(self):
        j = score.judge_refs([('site', 'a.cs', 3), ('site', 'a.cs', 9), ('site', 'a.cs', 1)], self.LABELS, self.BOUND)
        self.assertEqual(j.counts(), {'hits': 1, 'misses': 1, 'right': 1, 'wrong': 1, 'unjudged': 1})
        self.assertEqual(j.misses, [('b.cs', 7)])

    def test_a_repeated_item_counts_once(self):
        j = score.judge_refs([('site', 'a.cs', 3), ['site', 'a.cs', 3]], self.LABELS, self.BOUND)
        self.assertEqual(len(j.right), 1)

    def test_a_span_holding_labels_credits_each(self):
        j = score.judge_refs([('span', 'a.cs', 1, 12)], {('a.cs', 3), ('a.cs', 5)}, self.BOUND)
        self.assertEqual((len(j.right), j.hits), (1, [('a.cs', 3), ('a.cs', 5)]))

    def test_a_span_holding_only_another_binding_is_wrong(self):
        j = score.judge_refs([('span', 'a.cs', 8, 10)], self.LABELS, self.BOUND)
        self.assertEqual(j.counts()['wrong'], 1)

    def test_a_span_holding_nothing_is_unjudged(self):
        j = score.judge_refs([('span', 'c.cs', 1, 99)], self.LABELS, self.BOUND)
        self.assertEqual(j.counts()['unjudged'], 1)


class DeclsTest(unittest.TestCase):
    SOURCE = {
        'a.ts': ['/** doc */', '@decorated()', 'export function save() {', '}', 'function load() {}'],
        'b.ts': ['const x = 1'],
    }
    DECLS = {('a.ts', 3, 'save'), ('a.ts', 5, 'load')}

    def judge(self, items, labels):
        return score.judge_decls(items, labels, self.DECLS, self.SOURCE.get)

    def test_a_line_above_the_name_reaches_it(self):
        j = self.judge([('decl', 'a.ts', 1)], {('a.ts', 3, 'save')})
        self.assertEqual(j.hits, [('a.ts', 3, 'save')])

    def test_another_declaration_is_wrong(self):
        j = self.judge([('decl', 'a.ts', 5)], {('a.ts', 3, 'save')})
        self.assertEqual(j.counts(), {'hits': 0, 'misses': 1, 'right': 0, 'wrong': 1, 'unjudged': 0})

    def test_no_declaration_or_no_file_is_unjudged(self):
        j = self.judge([('decl', 'b.ts', 1), ('decl', 'gone.ts', 1)], {('a.ts', 3, 'save')})
        self.assertEqual(j.counts()['unjudged'], 2)

    def test_a_line_reaches_the_next_declaration_within_ten(self):
        # identity-v1's rule: a closing brace is read as the start of the declaration below it.
        j = self.judge([('decl', 'a.ts', 4)], {('a.ts', 3, 'save')})
        self.assertEqual(j.counts()['wrong'], 1)

    def test_name_line_needs_a_whole_word(self):
        self.assertIsNone(score.name_line(['saveAll()'], 1, 'save'))
        self.assertEqual(score.name_line(['x', 'save()'], 1, 'save'), 2)


class PathsTest(unittest.TestCase):
    def test_recall_within_budgets_uses_the_first_mention(self):
        out = score.judge_paths([('a.cs', 100), ('b.cs', 3000), ('a.cs', 9000)], {'a.cs', 'b.cs', 'c.cs'})
        self.assertEqual(out, {'all': (2, 3), 2000: (1, 3), 4000: (2, 3), 8000: (2, 3)})


class PoolTest(unittest.TestCase):
    def test_pooled_and_mean_figures(self):
        a = score.Judged(hits=[1], misses=[2, 3], right=[1], wrong=[], unjudged=[9])
        b = score.Judged(hits=[1, 2], misses=[], right=[1], wrong=[2])
        out = score.pool([a, b])
        self.assertEqual((out['recall']['k'], out['recall']['n']), (3, 5))
        self.assertEqual((out['precision']['k'], out['precision']['n']), (2, 3))
        self.assertEqual(out['unjudged'], 1)
        self.assertAlmostEqual(out['mean_recall']['mean'], (1 / 3 + 1) / 2)

    def test_wilson(self):
        self.assertEqual(score.wilson(0, 0), (None, None))
        low, high = score.wilson(9, 10)
        self.assertAlmostEqual(low, 0.5958, places=3)
        self.assertAlmostEqual(high, 0.9821, places=3)

    def test_tally_keeps_every_outcome(self):
        self.assertEqual(score.tally(['error', 'judged', 'error']), {'no_counterpart': 0, 'not_found': 0, 'error': 2})


if __name__ == '__main__':
    unittest.main()
