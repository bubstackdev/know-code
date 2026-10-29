"""review_gate: judging labels inside and outside a review's own diff, pairing two runs, and the gate's verdict."""
from __future__ import annotations

from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))
import review_gate  # noqa: E402

REVIEW = dict(id='demo-1', part='1', repo='demo', lang='csharp', files=['src/A.cs', 'src/B.cs'], tests=['tests/ATests.cs'],
              in_diff=['src/A.cs'])


class JudgeTest(unittest.TestCase):
    def test_labels_outside_the_diff_are_judged_apart(self):
        text = 'src/A.cs:3 A.Run\n' + 'x' * 100 + '\ntests/ATests.cs'
        j = review_gate.judge(REVIEW, text, (50,))
        self.assertEqual(set(j['outside']), {'src/B.cs', 'tests/ATests.cs'})
        self.assertTrue(j['files']['src/A.cs']['50'])
        self.assertFalse(j['files']['src/B.cs']['all'])
        self.assertTrue(j['outside']['tests/ATests.cs']['all'])
        self.assertFalse(j['outside']['tests/ATests.cs']['50'])


class PairedTest(unittest.TestCase):
    def test_each_review_is_compared_with_itself(self):
        def run(names):
            return [review_gate.judge(dict(REVIEW, id=f'r{i}'), t, (4000,)) for i, t in enumerate(names)]
        base = run(['src/A.cs', 'src/A.cs src/B.cs', ''])
        cand = run(['src/A.cs src/B.cs', 'src/A.cs src/B.cs', ''])
        p = review_gate.paired(base, cand, 'files', 'all')
        self.assertEqual((p['n'], p['wins'], p['ties'], p['losses']), (3, 1, 2, 0))
        self.assertAlmostEqual(p['mean'], 0.1667, places=4)

    def test_the_gate_needs_higher_recall_at_no_more_bytes(self):
        def s(recall, median):
            return {'overall': {f'files_recall@{review_gate.GATE_BUDGET}': recall, 'bytes_median': median}}
        self.assertTrue(review_gate.verdict(s(0.4, 3000), s(0.5, 3000)))
        self.assertFalse(review_gate.verdict(s(0.4, 3000), s(0.5, 3001)))
        self.assertFalse(review_gate.verdict(s(0.4, 3000), s(0.4, 2000)))

    def test_each_part_is_judged_on_its_own(self):
        def s(one, two):
            key = f'files_recall@{review_gate.GATE_BUDGET}'
            return {'overall': {key: (one + two) / 2, 'bytes_median': 100},
                    'part': {'1': {key: one, 'bytes_median': 100}, '2': {key: two, 'bytes_median': 100}}}
        # Part 2 lifts the combined figure, but part 1 alone is lower.
        base, cand = s(0.5, 0.1), s(0.4, 0.9)
        self.assertTrue(review_gate.verdict(base, cand))
        self.assertFalse(review_gate.verdict(base, cand, '1'))
        self.assertTrue(review_gate.verdict(base, cand, '2'))


class CompareTest(unittest.TestCase):
    def test_compare_reads_two_runs_and_decides_each_part(self):
        import json
        import tempfile
        with tempfile.TemporaryDirectory() as tmp:
            paths = []
            for name, texts in (('base', ['src/A.cs', '']), ('cand', ['src/A.cs src/B.cs', 'src/B.cs'])):
                judged = [review_gate.judge(dict(REVIEW, id=f'r{i}', part=str(i + 1)), t, (4000,)) for i, t in enumerate(texts)]
                summary = dict(knowcode='x', binary='y', tool=name, args={}, budgets=(4000,), errors={},
                               **review_gate.summarize(judged, (4000,)))
                path = Path(tmp)/f'{name}.json'
                path.write_text(json.dumps(summary))
                path.with_suffix('.judged.jsonl').write_text(''.join(json.dumps(j) + '\n' for j in judged))
                self.assertIn('| part 2 |', review_gate.report(summary))
                paths.append(path)
            out = review_gate.compare(*paths)
        self.assertIn('Gate, part 1', out)
        self.assertIn('Gate, combined', out)
        self.assertIn('| files | all | 0.250 | 0.750 | 2 | 0.500 | 2 | 0 | 0 |', out)


if __name__ == '__main__':
    unittest.main()
