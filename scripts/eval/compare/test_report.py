import unittest

from compare import report


def answer(outcome, items=(), raw=(), tool='knowcode'):
    return {'tool': tool, 'type': 'impact', 'outcome': outcome, 'items': [list(i) for i in items], 'raw': list(raw)}


class ReportTest(unittest.TestCase):
    def test_repetitions_compare_item_sets_not_order(self):
        a = {('knowcode', 'q1'): answer('judged', [('decl', 'a.cs', 1), ('decl', 'b.cs', 2)])}
        b = {('knowcode', 'q1'): answer('judged', [('decl', 'b.cs', 2), ('decl', 'a.cs', 1)])}
        self.assertEqual(report.repetition(a, b), [])
        b[('knowcode', 'q1')] = answer('error')
        self.assertEqual(report.repetition(a, b), [('knowcode', 'q1')])

    def test_reasons(self):
        self.assertEqual(report.reason(answer('not_found', raw=['repo src @ 1\nmethod A.b  a.cs:1'])),
                         'the answer was for a declaration of another name')
        self.assertEqual(report.reason(answer('not_found', raw=['{"groups": []}'], tool='serena')),
                         'the lookup found no declaration of that name holding the line')
        self.assertEqual(report.reason(answer('error', raw=['a.cs:3 didn\'t resolve:\n no indexed file'])),
                         "a.cs:3 didn't resolve: no indexed file")

    def test_variance_lists_only_figures_that_differ(self):
        rate = lambda k, n: {'k': k, 'n': n, 'rate': k / n, 'low': 0.1, 'high': 0.9}
        row = lambda r: {'tool': 'serena', 'type': 'refs', 'language': 'swift', 'precision': rate(9, 10), 'recall': r}
        a = {'answered': [row(rate(5, 10))], 'all_questions': [row(rate(5, 10))]}
        b = {'answered': [row(rate(4, 10))], 'all_questions': [row(rate(5, 10))]}
        rows = report.variance(a, b)
        self.assertEqual(len(rows), 1)
        self.assertIn('| Serena | refs | Swift | answered | recall | 0.500', rows[0])

    def test_depth_finding_reads_the_first_page(self):
        ans = {('knowcode', 'q'): answer('judged', raw=['repo x\nincomplete (depth): ...\nincomplete (capped): ...'])}
        self.assertEqual(report.depth_finding(ans), {'capped,depth': 1})


if __name__ == '__main__':
    unittest.main()
