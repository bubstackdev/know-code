"""Re-check cost: tokens of the calls the 0.2 detector classes as re-checks, and whether each changed the answer."""
import json
import unittest

import recheck_cost

REFS = json.dumps({'items': [{'file': 'src/Lib/Factory.cs', 'line': 40, 'name': 'BuildProvider',
                              'from': {'name': 'Configure', 'container': 'Startup'}, 'repo': 'app'}], 'total': 1})


def turn(mid, context, *calls, text=None):
    """One model turn: an assistant message with context usage and its tool calls, then their results."""
    content = ([dict(type='text', text=text)] if text else []) + [
        dict(type='tool_use', id=cid, name=name, input=inp) for cid, name, inp, _ in calls]
    usage = dict(input_tokens=context, cache_creation_input_tokens=0, cache_read_input_tokens=0, output_tokens=5)
    events = [dict(type='assistant', message=dict(id=mid, content=content, usage=usage))]
    if calls:
        events.append(dict(type='user', message=dict(content=[
            dict(type='tool_result', tool_use_id=cid, content=[dict(type='text', text=result)]) for cid, _, _, result in calls])))
    return events


class RecheckCostTests(unittest.TestCase):
    def test_tokens_are_the_context_a_call_adds_times_the_turns_that_read_it(self):
        events = (turn('m0', 100, ('k1', 'mcp__knowcode__kc_refs', {'name': 'BuildProvider'}, REFS))
                  + turn('m1', 400, ('r1', 'Read', {'file_path': '/s/app/src/Lib/Factory.cs', 'offset': 35, 'limit': 10}, 'x' * 40))
                  + turn('m2', 700)
                  + turn('m3', 750, text='{"status": "answered", "items": []}'))
        result = recheck_cost.cost(events, ['app/src/Lib/Factory.cs:40'])
        self.assertEqual(result['recheck_calls'], 1)
        call = result['calls'][0]
        self.assertEqual((call['call'], call['tool']), ('r1', 'Read'))
        # The read is the only call of turn m1: it added 700 - 400 = 300 tokens, read by turns m2 and m3.
        self.assertEqual(call['tokens'], 600)
        self.assertEqual(result['recheck_tokens'], 600)
        # The final answer names only what KnowCode already said: confirmed, not changed.
        self.assertFalse(call['changed'])

    def test_calls_of_one_turn_share_its_growth_by_size_and_non_rechecks_are_left_out(self):
        big, small = 'y' * 900, 'z' * 100
        events = (turn('m0', 100, ('k1', 'mcp__knowcode__kc_refs', {'name': 'BuildProvider'}, REFS))
                  + turn('m1', 400, ('b1', 'Bash', {'command': 'grep -rn BuildProvider app'}, big),
                         ('b2', 'Bash', {'command': 'ls app'}, small))
                  + turn('m2', 1400, text='done'))
        costs = recheck_cost.call_tokens(recheck_cost.turns_of(events),
                                         {c['id']: c for c in recheck_cost.recheck.calls_in_order(events)})
        self.assertGreater(costs['b1'], 7 * costs['b2'])  # by input and result length
        self.assertEqual(costs['b1'] + costs['b2'], 1000)
        result = recheck_cost.cost(events, [])
        self.assertEqual([c['call'] for c in result['calls']], ['b1'])  # `ls app` re-checks nothing

    def test_a_recheck_that_finds_what_knowcode_missed_changed_the_answer(self):
        hits = 'app/src/Lib/Factory.cs:40: BuildProvider()\napp/src/Web/Program.cs:12: BuildProvider()'
        events = (turn('m0', 100, ('k1', 'mcp__knowcode__kc_refs', {'name': 'BuildProvider'}, REFS))
                  + turn('m1', 400, ('b1', 'Bash', {'command': 'grep -rn BuildProvider app'}, hits))
                  + turn('m2', 500, text='answer'))
        result = recheck_cost.cost(events, ['app/src/Lib/Factory.cs:40', {'location': 'app/src/Web/Program.cs:12'}])
        call = result['calls'][0]
        self.assertTrue(call['changed'])
        self.assertEqual(call['added'], ['app/src/Web/Program.cs'])
        self.assertEqual(result['changed_calls'], 1)

    def test_a_call_rechecking_two_answers_counts_once(self):
        events = (turn('m0', 100, ('k1', 'mcp__knowcode__kc_refs', {'name': 'BuildProvider'}, REFS),
                       ('k2', 'mcp__knowcode__kc_refs', {'name': 'BuildProvider', 'limit': 5}, REFS))
                  + turn('m1', 300, ('r1', 'Read', {'file_path': '/s/app/src/Lib/Factory.cs'}, 'body'))
                  + turn('m2', 400, text='answer'))
        result = recheck_cost.cost(events, [])
        self.assertEqual(result['recheck_calls'], 1)
        self.assertEqual(result['calls'][0]['answers'], ['k1', 'k2'])
        self.assertEqual(result['recheck_tokens'], 100)

    def test_missing_usage_or_compaction_gives_no_token_figure(self):
        events = (turn('m0', 100, ('k1', 'mcp__knowcode__kc_refs', {'name': 'BuildProvider'}, REFS))
                  + turn('m1', 400, ('r1', 'Read', {'file_path': '/s/app/src/Lib/Factory.cs'}, 'body'))
                  + turn('m2', 200, text='answer'))  # context shrank: compacted
        result = recheck_cost.cost(events, [])
        self.assertEqual(result['recheck_calls'], 1)
        self.assertIsNone(result['calls'][0]['tokens'])
        self.assertIsNone(result['recheck_tokens'])


if __name__ == '__main__':
    unittest.main()
