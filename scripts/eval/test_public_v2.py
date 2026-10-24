"""public-v2 scoring: the five criteria over a build's runs, the baseline comparison and the Bash refusal audit."""
import unittest

import public_v2

QUESTIONS = [{'id': f'q{n}', 'stratum': public_v2.STRATA[n % 4]} for n in range(8)]


def rows(runs, named, plain, unprompted_calls):
    out = {}
    for r in runs:
        for q in QUESTIONS:
            n = int(q['id'][1:])
            out[(r, 'knowcode', q['id'])] = dict(status='answered', label='exhaustive', f1=1.0, recall=1.0,
                                               tokens=named, recheck_tokens=1000 + 100 * n, knowcode_calls=2, changed=[])
            out[(r, 'plain', q['id'])] = dict(status='answered', label='exhaustive', f1=0.9, recall=1.0, tokens=plain)
            out[(r, 'knowcode-unprompted', q['id'])] = dict(status='answered', label='exhaustive', f1=1.0, recall=1.0,
                                                          tokens=named, recheck_tokens=500, knowcode_calls=unprompted_calls(n))
    return out


class PublicV2Tests(unittest.TestCase):
    def test_criteria_over_two_runs_and_against_a_baseline(self):
        runs = ['b1', 'b2']
        base = public_v2.summarize(rows(runs, 30000, 31000, lambda n: 1), QUESTIONS, runs)
        c = base['criteria']
        self.assertAlmostEqual(c['1_recheck_cost_halved']['value'], 1350)
        self.assertIsNone(c['1_recheck_cost_halved']['met'])
        self.assertEqual(c['1_recheck_cost_halved']['sessions'], 16)
        self.assertAlmostEqual(c['3_f1_and_recall_at_least_equal']['f1_difference'], 0.1)
        self.assertTrue(c['3_f1_and_recall_at_least_equal']['met'])
        self.assertEqual(c['3_f1_and_recall_at_least_equal']['recall_by_question'], {})
        self.assertEqual(c['4_research_tokens_at_most_equal']['difference'], -1000)
        self.assertTrue(c['4_research_tokens_at_most_equal']['met'])
        self.assertEqual((c['5_unprompted_use']['sessions'], c['5_unprompted_use']['sessions_with_call']), (16, 16))
        self.assertEqual(base['arms']['knowcode-unprompted']['use_by_task_type']['http-cross-repo'],
                         {'sessions': 4, 'with_call': 4})
        # A candidate whose re-checks cost half or less meets criterion 1; equal tokens meet criterion 4.
        half = rows(runs, 31000, 31000, lambda n: 1)
        for key, row in half.items():
            if 'recheck_tokens' in row:
                row['recheck_tokens'] = row['recheck_tokens'] / 2
        cand = public_v2.summarize(half, QUESTIONS, runs, baseline=base)['criteria']
        self.assertTrue(cand['1_recheck_cost_halved']['met'])
        self.assertAlmostEqual(cand['1_recheck_cost_halved']['ratio'], 0.5)
        self.assertTrue(cand['4_research_tokens_at_most_equal']['met'])
        worse = public_v2.summarize(rows(runs, 32000, 31000, lambda n: 1), QUESTIONS, runs, baseline=base)['criteria']
        self.assertFalse(worse['1_recheck_cost_halved']['met'])
        self.assertFalse(worse['4_research_tokens_at_most_equal']['met'])

    def test_the_lower_bound_question_is_reported_per_arm_and_run(self):
        runs = ['b1', 'b2']
        r = rows(runs, 1, 1, lambda n: 1)
        for run in runs:
            for arm in public_v2.ARMS:
                r[(run, arm, 'q3')].update(label='lower_bound', recall=0.5 if arm == 'plain' else 1.0)
        c = public_v2.summarize(r, QUESTIONS, runs)['criteria']['3_f1_and_recall_at_least_equal']
        self.assertEqual(c['recall_n'], 2)
        self.assertAlmostEqual(c['recall_difference'], 0.5)
        self.assertEqual(c['recall_by_question']['q3']['b2/plain'], 0.5)
        self.assertEqual(len(c['recall_by_question']['q3']), 6)

    def test_unprompted_use_threshold_is_twenty_of_every_twenty_four(self):
        runs = ['b1', 'b2', 'b3']  # 24 unprompted sessions
        def share(missing):
            return public_v2.summarize(rows(runs, 1, 1, lambda n: 0 if n < missing else 1), QUESTIONS, runs)['criteria']['5_unprompted_use']
        self.assertTrue(share(0)['met'])
        # Without a call: one question in all three runs (3 of 24, 21 left) still meets; two questions (6 of 24) don't.
        self.assertTrue(share(1)['met'])
        self.assertFalse(share(2)['met'])

    def test_bash_refusals_are_split_by_whether_they_name_the_binary(self):
        def call(cid, command, refused):
            text = 'Permission to use Bash with command ' + command + ' has been denied.' if refused else 'ok'
            return [dict(type='assistant', message=dict(id='m' + cid, content=[dict(type='tool_use', id=cid, name='Bash', input=dict(command=command))])),
                    dict(type='user', message=dict(content=[dict(type='tool_result', tool_use_id=cid, content=text, is_error=refused)]))]
        events = (call('a', '/opt/kc/knowcode-v0.2.0 --version', True) + call('b', 'grep -rn x /src', True)
                  + call('c', 'grep -rn y /src', False))
        self.assertEqual(public_v2.refused_bash(events, 'knowcode-v0.2.0'), (1, 1))


if __name__ == '__main__':
    unittest.main()
