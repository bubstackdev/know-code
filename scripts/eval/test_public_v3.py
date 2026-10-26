"""public-v3 scoring: the 0.4 criteria over a build's runs, and the detector form of a full client's transcript."""
import unittest

import public_v3
import recheck

QUESTIONS = [{'id': f'q{n}', 'stratum': list(public_v3.COUNTS)[n % 4]} for n in range(6)]


def rows(runs, named_tokens=30000, plain_tokens=31000, recheck=lambda n, r: 1000, unprompted=lambda n: 1, label=lambda n: 'exhaustive'):
    out = {}
    for r in runs:
        for q in QUESTIONS:
            n = int(q['id'][1:])
            base = dict(status='answered', label=label(n), f1=1.0 if label(n) == 'exhaustive' else None, recall=1.0)
            out[(r, public_v3.NAMED, q['id'])] = dict(base, tokens=named_tokens, recheck_tokens=recheck(n, r), knowcode_calls=2, changed=[])
            out[(r, public_v3.PLAIN, q['id'])] = dict(base, recall=0.5 if label(n) == 'lower_bound' else 1.0,
                                                    f1=0.9 if label(n) == 'exhaustive' else None, tokens=plain_tokens)
            out[(r, public_v3.UNPROMPTED, q['id'])] = dict(base, tokens=named_tokens, knowcode_calls=unprompted(n), changed=[])
            out[(r, public_v3.BARE, q['id'])] = dict(base, tokens=named_tokens, knowcode_calls=0, changed=[])
    return out


class PublicV3Tests(unittest.TestCase):
    def test_criterion_two_is_the_median_of_per_question_means_so_one_session_cannot_decide_it(self):
        runs = ['b1', 'b2']
        base = public_v3.summarize(rows(runs), QUESTIONS, runs)['criteria']
        self.assertEqual(base['2_recheck_cost_halved']['value'], 1000)
        # One huge session moves the mean but not the median of per-question means.
        spiky = rows(runs, recheck=lambda n, r: 400 if not (n == 0 and r == 'b2') else 900000)
        c = public_v3.summarize(spiky, QUESTIONS, runs, baseline={'criteria': base})['criteria']['2_recheck_cost_halved']
        self.assertEqual(c['value'], 400)
        self.assertTrue(c['met'])
        self.assertGreater(c['mean_per_session'], 1000)

    def test_tokens_recall_over_every_label_with_items_and_unprompted_use(self):
        runs = ['b1']
        labels = lambda n: 'lower_bound' if n == 1 else ('exhaustive-empty' if n == 2 else 'exhaustive')
        c = public_v3.summarize(rows(runs, 31000, 31000, label=labels), QUESTIONS, runs)['criteria']
        self.assertTrue(c['1_research_tokens_at_most_equal']['met'])
        c3 = c['3_f1_and_recall_at_least_equal']
        self.assertEqual((c3['f1_n'], c3['recall_n'], c3['recall_questions']), (4, 5, 5))
        self.assertAlmostEqual(c3['recall_difference'], 0.1)
        self.assertTrue(c3['met'])
        c4 = c['4_unprompted_use']
        self.assertEqual((c4['sessions'], c4['sessions_with_call'], c4['met']), (6, 6, True))
        self.assertEqual(c4['bare_descriptive']['sessions_with_call'], 0)
        edge = public_v3.summarize(rows(runs, unprompted=lambda n: 0 if n == 0 else 1), QUESTIONS, runs)['criteria']
        self.assertTrue(edge['4_unprompted_use']['met'])  # 5 of 6 is exactly 20 of 24
        worse = public_v3.summarize(rows(runs, 32000, 31000, unprompted=lambda n: 0 if n < 2 else 1), QUESTIONS, runs)['criteria']
        self.assertFalse(worse['1_research_tokens_at_most_equal']['met'])
        self.assertFalse(worse['4_unprompted_use']['met'])  # 4 of 6 is under 20 of 24

    def test_full_client_tool_calls_take_the_form_the_detector_reads(self):
        events = [dict(type='assistant', message=dict(id='m1', content=[
            dict(type='tool_use', id='a', name='mcp__plugin_knowcode_knowcode__kc_refs', input={'name': 'Build'}),
            dict(type='tool_use', id='b', name='Grep', input={'pattern': 'Build(', 'path': '/w/app/src'}),
            dict(type='tool_use', id='c', name='Glob', input={'pattern': '**/Factory.cs', 'path': '/w/app'}),
            dict(type='tool_use', id='d', name='Read', input={'file_path': '/w/app/src/Factory.cs'})]))]
        converted = public_v3.detector_form(events)[0]['message']['content']
        self.assertEqual(converted[0]['name'], recheck.KC_PREFIX + 'kc_refs')
        self.assertEqual(converted[1], dict(type='tool_use', id='b', name='Bash', input={'command': "grep -rn 'Build(' /w/app/src"}))
        self.assertEqual(converted[2]['input'], {'command': 'find /w/app -name Factory.cs'})
        self.assertEqual(converted[3]['name'], 'Read')
        self.assertEqual(events[0]['message']['content'][1]['name'], 'Grep')  # the transcript itself is unchanged


    def test_unprompted_use_is_83_percent_of_every_session_pooled(self):
        questions = [{'id': f'q{n:02}', 'stratum': list(public_v3.COUNTS)[n % 4]} for n in range(40)]
        def sessions(used):
            out = {}
            for i, (r, q) in enumerate((r, q['id']) for r in ('b1', 'b2') for q in questions):
                for arm in public_v3.ARMS:
                    out[(r, arm, q)] = dict(status='answered', label='exhaustive', f1=1.0, recall=1.0, tokens=1,
                                            knowcode_calls=int(i < used) if arm == public_v3.UNPROMPTED else 1, changed=[])
            return out
        for used, met in ((67, True), (66, False)):
            c4 = public_v3.summarize(sessions(used), questions, ['b1', 'b2'])['criteria']['4_unprompted_use']
            self.assertEqual((c4['sessions'], c4['sessions_with_call'], c4['required'], c4['met']), (80, used, 67, met))


if __name__ == '__main__':
    unittest.main()
