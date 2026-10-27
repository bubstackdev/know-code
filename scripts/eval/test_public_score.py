"""Study scorer: per-session scoring rules and the pre-registered criteria on synthetic rows."""
import random
import unittest

import score_public_study as study
from frozen_study import ROOT, load

scorer = load('test_study_neutral_score', ROOT/'scripts/eval/neutral/score.py')
Q = {'id': 'q', 'stratum': 'definition-lookup', 'repos': ['lib']}


def label(items, completeness='exhaustive'):
    return {'id': 'q', 'items': [{'item': i, 'evidence': i} for i in items], 'completeness': completeness, 'match': 'file_line'}


class SessionScoreTests(unittest.TestCase):
    def test_exhaustive_f1_and_errors_count_zero(self):
        row = study.score_session(scorer, Q, label(['lib/a.cs:1', 'lib/b.cs:2']), {'status': 'answered', 'items': ['lib/a.cs:1', 'lib/c.cs:3']})
        self.assertAlmostEqual(row['f1'], 0.5)
        failed = study.score_session(scorer, Q, label(['lib/a.cs:1']), {'status': 'error', 'items': []})
        self.assertEqual((failed['f1'], failed['recall']), (0.0, 0.0))
        missing = study.score_session(scorer, Q, label(['lib/a.cs:1']), None)
        self.assertEqual((missing['status'], missing['f1']), ('missing', 0.0))

    def test_empty_labels_are_scored_apart_from_f1(self):
        right = study.score_session(scorer, Q, label([]), {'status': 'answered', 'items': []})
        wrong = study.score_session(scorer, Q, label([]), {'status': 'answered', 'items': ['lib/a.cs:1']})
        self.assertEqual((right['label'], right['f1'], right['correct_empty']), ('exhaustive-empty', None, True))
        self.assertFalse(wrong['correct_empty'])

    def test_lower_bound_scores_recall_only(self):
        row = study.score_session(scorer, Q, label(['lib/a.cs:1', 'lib/b.cs:2'], 'lower_bound'), {'status': 'answered', 'items': ['lib/a.cs:1', 'lib/z.cs:9']})
        self.assertEqual((row['f1'], row['recall'], row['precision']), (None, 0.5, None))

    def test_stale_index_answers_are_counted(self):
        def kc(call_id, text, error=False):
            result = dict(type='tool_result', tool_use_id=call_id, content=[dict(type='text', text=text)])
            if error:
                result['is_error'] = True
            return [dict(type='assistant', message=dict(content=[dict(type='tool_use', id=call_id, name='mcp__knowcode__kc_refs', input={})])),
                    dict(type='user', message=dict(content=[result]))]
        events = kc('a', '{"incomplete":[{"reason":"stale","detail":"x: indexing is queued or running; ..."}]}') + kc('b', '{}') + kc('c', 'indexing is queued or running', error=True)
        self.assertEqual(study.stale_answers(events), {'answers': 2, 'stale': 1})

    def test_tokens_include_every_processed_context_and_output(self):
        self.assertEqual(study.tokens({'processed_context_tokens': 1000, 'terminal_output_tokens': 50}), 1050)
        self.assertIsNone(study.tokens({'processed_context_tokens': None, 'terminal_output_tokens': 50}))


class CriteriaTests(unittest.TestCase):
    def rows(self, recheck_named):
        rows = []
        for n in range(24):
            for arm in study.ARMS:
                kc = arm != 'plain'
                lower = n % 2 == 1
                rows.append(dict(arm=arm, question_id=f'q{n:02}', status='answered', label='lower_bound' if lower else 'exhaustive',
                                 f1=None if lower else (0.8 if arm == 'plain' else 0.9), recall=0.8, correct_empty=None,
                                 tokens=100000 if arm == 'plain' else 90000, fixed_overhead=20000, output_tokens=500,
                                 research_calls=10, knowcode_calls=(3 if kc and (arm == 'knowcode' or n < 21) else 0),
                                 overrun=0, cost_usd=0.3,
                                 recheck={'answers_with_claims': 4, 'rechecked_answers': recheck_named} if kc else None))
        return rows

    def test_criteria_baseline_then_final(self):
        questions = [{'id': f'q{n:02}'} for n in range(24)]
        baseline = study.summarize(self.rows(4), questions, random.Random(1))
        self.assertEqual(baseline['arms']['knowcode']['recheck_rate'], 1.0)
        self.assertIsNone(baseline['criteria']['1_recheck_rate_halved']['met'])
        self.assertTrue(baseline['criteria']['2_f1_at_least_equal']['met'])
        self.assertTrue(baseline['criteria']['3_fewer_research_tokens']['met'])
        self.assertEqual(baseline['criteria']['4_unprompted_use']['sessions_with_call'], 21)
        self.assertTrue(baseline['criteria']['4_unprompted_use']['met'])
        final = study.summarize(self.rows(2), questions, random.Random(1), baseline)
        self.assertTrue(final['criteria']['1_recheck_rate_halved']['met'])
        worse = study.summarize(self.rows(3), questions, random.Random(1), baseline)
        self.assertFalse(worse['criteria']['1_recheck_rate_halved']['met'])

    def test_wilson_interval(self):
        low, high = study.wilson(20, 24)
        self.assertTrue(0.6 < low < 0.7 and 0.9 < high < 0.96)


if __name__ == '__main__':
    unittest.main()
