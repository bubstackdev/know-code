"""Re-check detector: unit cases and agreement with the hand-labelled synthetic transcripts in recheck_fixtures/.

The fixtures were written and labelled by a separate session from the labelling guide in protocol.md, without
reading recheck.py. The development set (cases/) found two rule bugs, which were fixed; the held-out set
(holdout/) was labelled afterwards and never used to change the rule. `python3 test_recheck.py` prints precision
and recall for both; the tests hold them at the values reported in the protocol.
"""
import json
from pathlib import Path
import unittest

import recheck

FIXTURES = Path(__file__).with_name('recheck_fixtures')
HOLDOUT = FIXTURES / 'holdout'
# Cases the labeller marked ambiguous; reported both with and without them.
AMBIGUOUS = {'05', '36'}
HOLDOUT_AMBIGUOUS = {'h04', 'h08', 'h10', 'h17', 'h30'}


def kc(call_id, name, inp, text, error=False):
    block = dict(type='tool_result', tool_use_id=call_id, content=[dict(type='text', text=text)])
    if error:
        block['is_error'] = True
    return [dict(type='assistant', message=dict(id='m' + call_id, content=[dict(type='tool_use', id=call_id, name='mcp__knowcode__' + name, input=inp)])),
            dict(type='user', message=dict(content=[block]))]


def plain(call_id, name, **inp):
    return [dict(type='assistant', message=dict(id='m' + call_id, content=[dict(type='tool_use', id=call_id, name=name, input=inp)])),
            dict(type='user', message=dict(content=[dict(type='tool_result', tool_use_id=call_id, content='text')]))]


REFS = json.dumps({'items': [{'file': 'src/Lib/Factory.cs', 'line': 40, 'name': 'BuildProvider',
                              'from': {'name': 'Configure', 'container': 'Startup'}}], 'total': 1})


def measure(skip=(), root=FIXTURES):
    labels = json.loads((root / 'labels.json').read_text())
    tp = fp = fn = tn = 0
    disagreements = []
    for case, calls in sorted(labels.items()):
        if case.split('-')[0] in skip:
            continue
        found = {a['id']: a for a in recheck.detect(recheck.read_events(root / 'cases' / case))['answers']}
        for call_id, label in calls.items():
            if not label['scored']:
                continue
            got = found.get(call_id, {}).get('rechecked', False)
            want = bool(label['rechecked'])
            tp += got and want; fp += got and not want; fn += want and not got; tn += not got and not want
            if got != want:
                disagreements.append((case, call_id, want))
    return dict(tp=tp, fp=fp, fn=fn, tn=tn, precision=tp / (tp + fp) if tp + fp else None,
                recall=tp / (tp + fn) if tp + fn else None, disagreements=disagreements)


class RecheckUnitTests(unittest.TestCase):
    def test_read_at_a_stated_line_rechecks_and_a_far_range_does_not(self):
        events = kc('k1', 'kc_refs', {'symbol': 'BuildProvider'}, REFS)
        near = events + plain('p1', 'Read', file_path='/study/source/lib/src/Lib/Factory.cs', offset=30, limit=20)
        far = events + plain('p1', 'Read', file_path='/study/source/lib/src/Lib/Factory.cs', offset=200, limit=20)
        self.assertTrue(recheck.detect(near)['answers'][0]['rechecked'])
        self.assertFalse(recheck.detect(far)['answers'][0]['rechecked'])

    def test_grep_for_a_stated_symbol_and_sed_range(self):
        events = kc('k1', 'kc_refs', {'symbol': 'BuildProvider'}, REFS)
        grep = events + plain('p1', 'Bash', command="grep -rn 'BuildProvider(' /study/source/lib/src | head -20")
        sed = events + plain('p1', 'Bash', command="sed -n '35,45p' lib/src/Lib/Factory.cs")
        other = events + plain('p1', 'Bash', command="grep -rn 'class Unrelated' /study/source/lib")
        self.assertTrue(recheck.detect(grep)['answers'][0]['rechecked'])
        self.assertTrue(recheck.detect(sed)['answers'][0]['rechecked'])
        self.assertFalse(recheck.detect(other)['answers'][0]['rechecked'])

    def test_calls_before_the_answer_and_errors_do_not_count(self):
        events = plain('p0', 'Read', file_path='/study/source/lib/src/Lib/Factory.cs') + kc('k1', 'kc_refs', {'symbol': 'BuildProvider'}, REFS)
        self.assertFalse(recheck.detect(events)['answers'][0]['rechecked'])
        failed = recheck.detect(kc('k1', 'kc_refs', {'symbol': 'BuildProvider'}, 'boom', error=True))
        self.assertEqual((failed['knowcode_errors'], failed['answers_with_claims'], failed['recheck_rate']), (1, 0, None))

    def test_identical_call_is_a_re_ask(self):
        events = kc('k1', 'kc_refs', {'symbol': 'BuildProvider'}, REFS) + kc('k2', 'kc_refs', {'symbol': 'BuildProvider'}, REFS)
        out = recheck.detect(events)
        self.assertTrue(out['answers'][0]['rechecked'])
        self.assertEqual(out['answers'][0]['by'][0]['reasons'], ['repeated KnowCode call'])

    def test_tree_answers_state_their_symbols_and_dotted_names_are_not_files(self):
        tree = 'class Factory  src/Lib/Factory.cs:12\n  called by method Program.Main  src/App/Program.cs:9 [exact]'
        locations, symbols = recheck.claims_of({'symbol': 'Factory'}, tree)
        self.assertEqual(set(locations), {'src/Lib/Factory.cs', 'src/App/Program.cs'})
        self.assertIn('Program', symbols)
        self.assertIn('Main', symbols)


class CompactFormTests(unittest.TestCase):
    """0.2 compact answers state the same claims as the JSON rows they replace."""

    def claims(self, answer, inp=None):
        # As the rule reads them: a file named without a line counts only when no line is given for it.
        locations, symbols = recheck.claims_of(inp or {}, json.dumps(answer))
        return {f: ({l for l in ls if l is not None} or {None}) for f, ls in locations.items()}, symbols

    def test_search_rows_match_their_json_rows(self):
        rows = {'items': ['src/Lib/Factory.cs:40 def Factory.Build | public X Build()',
                          'app/src/Lib/Use.cs:7 decl UseCase | class UseCase',
                          'src/Lib/Use.cs:12 code in UseCase.Run | var x = Build();',
                          'test/FactoryTests.cs:3 test in FactoryTests.Builds | Build();\n  2- // before']}
        json_rows = {'items': [{'file': 'src/Lib/Factory.cs', 'line': 40, 'kind': 'def', 'symbol': 'Factory.Build', 'text': 'public X Build()'},
                               {'file': 'app/src/Lib/Use.cs', 'line': 7, 'kind': 'decl', 'symbol': 'UseCase', 'text': 'class UseCase'},
                               {'file': 'src/Lib/Use.cs', 'line': 12, 'kind': 'code', 'in': 'UseCase.Run', 'text': 'var x = Build();'},
                               {'file': 'test/FactoryTests.cs', 'line': 3, 'kind': 'test', 'in': 'FactoryTests.Builds', 'text': 'Build();'}]}
        self.assertEqual(self.claims(rows), self.claims(json_rows))
        locations, symbols = self.claims(rows)
        self.assertEqual(symbols, {'Factory', 'Build', 'UseCase'})  # `in X` is a container, never a claim
        self.assertEqual(locations['src/Lib/Use.cs'], {12})

    def test_files_only_and_structural_rows(self):
        locations, _ = self.claims({'items': ['src/Lib/Factory.cs:40 def (3 matches)', 'src/Lib/Use.cs:7:5-9 [$X=Build] | Build()']})
        self.assertEqual((locations['src/Lib/Factory.cs'], locations['src/Lib/Use.cs']), ({40}, {7}))

    def test_from_strings_and_shared_fields_name_symbols(self):
        _, symbols = self.claims({'items': [{'file': 'a.cs', 'line': 3, 'from': 'Checkout.Submit'}], 'targets_from': 'Basket.Append',
                                  'callers_name': 'Totals'})
        self.assertTrue({'Checkout', 'Submit', 'Basket', 'Append', 'Totals'} <= symbols)

    def test_declared_overloads_and_hierarchy_name_symbols_and_lines(self):
        answer = {'declared': 'the only 2 class declarations of Cart in shop: class Cart, partial in 2 parts in src/ (Cart.cs:12,40, CartTotals.cs:5)',
                  'overloads': ['1: Append(int qty) at src/Cart.cs:20 (2 sites)', '2: Append<T>(T item) at src/Cart.cs:31'],
                  'hierarchy': {'implementations': ['Shop.SqlStore src/SqlStore.cs:8', 'Shop.MemStore other:src/MemStore.cs:4'],
                                'none_found': ['subclasses: none found (searched: every type base list in shop)']}}
        locations, symbols = self.claims(answer)
        self.assertEqual(locations['Cart.cs'], {12, 40})
        self.assertEqual(locations['src/Cart.cs'], {20, 31})
        self.assertTrue({'Cart', 'Append', 'Shop', 'SqlStore', 'MemStore'} <= symbols)
        self.assertNotIn('subclasses', symbols)

    def test_summary_patterns_stay_out_of_other_text(self):
        # Outside declared/overloads/hierarchy, "word at path:line" is not read as a name.
        _, symbols = self.claims({'items': [{'file': 'a.cs', 'line': 1, 'note': 'direct at src/a.cs:1'}]})
        self.assertNotIn('direct', symbols)


class RecheckFixtureTests(unittest.TestCase):
    def test_agreement_with_hand_labels(self):
        everything, unambiguous = measure(), measure(AMBIGUOUS)
        self.assertGreaterEqual(everything['precision'], 0.9, everything)
        self.assertGreaterEqual(everything['recall'], 0.9, everything)
        self.assertGreaterEqual(unambiguous['precision'], 0.9, unambiguous)
        self.assertGreaterEqual(unambiguous['recall'], 0.9, unambiguous)

    def test_agreement_with_held_out_hand_labels(self):
        # Labelled after the rule was fixed and never used to change it: these are the reported numbers.
        held, clear = measure(root=HOLDOUT), measure(HOLDOUT_AMBIGUOUS, HOLDOUT)
        self.assertEqual((held['tp'], held['fp'], held['fn'], held['tn']), (16, 3, 2, 13), held)
        self.assertEqual((clear['tp'], clear['fp'], clear['fn'], clear['tn']), (15, 1, 0, 13), clear)


if __name__ == '__main__':
    print(json.dumps({'development': {'all': measure(), 'without_ambiguous': measure(AMBIGUOUS)},
                      'holdout': {'all': measure(root=HOLDOUT), 'without_ambiguous': measure(HOLDOUT_AMBIGUOUS, HOLDOUT)}}, indent=2))
