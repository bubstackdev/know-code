"""History gate: scoring and the verdict, on made-up scores (the run itself needs the full-history clones)."""
import unittest

import history_gate


def row(family, **scores):
    return {'id': family + '-q', 'family': family,
            'scores': {name: dict(hits=h, truth=t, listed=l) for name, (h, t, l) in scores.items()}}


class ScoringTest(unittest.TestCase):
    def test_sets_ranked_lists_and_lines(self):
        self.assertEqual(history_gate.score_set(['a', 'b', 'b', 'x'], ['a', 'b', 'c']), dict(hits=2, truth=3, listed=3))
        # Ranked: only the first len(truth) count.
        self.assertEqual(history_gate.score_ranked(['x', 'a', 'b', 'c'], ['a', 'b', 'c']), dict(hits=2, truth=3, listed=3))
        self.assertEqual(history_gate.score_lines({'ann': 5, 'bob': 3}, {'ann': 4, 'cy': 2}), dict(hits=4, truth=6, listed=8))

    def test_an_optional_item_is_neither_a_hit_nor_a_miss(self):
        # Listed or not, an optional item counts in neither truth nor what was listed.
        self.assertEqual(history_gate.score_set(['a', 'o'], ['a'], ['o']), dict(hits=1, truth=1, listed=1))
        self.assertEqual(history_gate.score_set(['a'], ['a'], ['o']), dict(hits=1, truth=1, listed=1))
        # A question whose every item is optional: nothing to find, and listing it is not wrong.
        self.assertEqual(history_gate.score_set(['o'], [], ['o']), dict(hits=0, truth=0, listed=0))
        self.assertEqual(history_gate.score_set([], [], ['o']), dict(hits=0, truth=0, listed=0))


class VerdictTest(unittest.TestCase):
    def test_knowcode_must_beat_each_plain_approach(self):
        rows = [row('symbol-commits', knowcode=(5, 5, 5), **{'log --follow': (5, 5, 40), 'log -L': (2, 5, 2)})]
        self.assertEqual(history_gate.verdict(rows), [])
        # Equal to an approach on both is not better.
        rows = [row('symbol-commits', knowcode=(2, 5, 2), **{'log -L': (2, 5, 2)})]
        self.assertEqual(len(history_gate.verdict(rows)), 1)
        # More recall at lower precision is not better either.
        rows = [row('symbol-commits', knowcode=(5, 5, 10), **{'log -L': (2, 5, 2)})]
        self.assertEqual(len(history_gate.verdict(rows)), 1)

    def test_controls_must_match_the_best_plain_approach(self):
        ok = [row('file-cochange', knowcode=(50, 55, 55), log=(52, 55, 55))]
        self.assertEqual(history_gate.verdict(ok), [])
        short = [row('file-cochange', knowcode=(33, 55, 55), log=(46, 55, 55))]
        self.assertEqual(len(history_gate.verdict(short)), 1)


if __name__ == '__main__':
    unittest.main()
