"""The adapters' parsers, on answers each tool gave to the made-up fixture (fixture/README.md)."""
import json
import unittest

from compare import adapters

KC_IMPACT = """repo typescript @ 795406c
by proximity: direct 1, near 1; low-confidence 0
method Basket.weigh  src/basket.ts:4
     4 | weigh(): number {
  called by function gather  src/harvest.ts:7  [exact] direct (declared at line 4)
       7 | return basket.weigh();
    called by function harvest  src/harvest.ts:11  [exact] near (declared at line 10)
        11 | return gather(new HandPicker()) + gather(new HandPicker());
  stopped at function harvest  src/harvest.ts:10  [at the depth asked; 1+ dependents not listed]
"""

CBM_SEARCH = json.dumps({"qn_rule": "", "cols": ["name", "label", "lines", "in", "out"], "groups": [
    {"qn_prefix": "p.src.basket.Basket", "file": "src/basket.ts", "rows": [["weigh", "Method", "4-6", 1, 0]]},
    {"qn_prefix": "p.src.scale", "file": "src/scale.ts", "rows": [["weigh", "Function", "1-3", 0, 0]]}],
    "has_more": False})

CBM_TRACE = json.dumps({"function": "weigh", "callers": {"cols": ["name", "hop", "test"], "groups": [
    {"qn_prefix": "p.src.harvest", "rows": [["gather", 1, False], ["harvest", 2, False]]}]}})

SERENA_REFS = json.dumps({"src/harvest.ts": {
    "Function": [{"name_path": "gather", "body_location": {"start_line": 3, "end_line": 7},
                  "content_around_reference": "...   5:  picker.pick(basket);\n  >   6:  return basket.weigh();\n...   7:}"}],
    "File": [{"name_path": "harvest", "body_location": {"start_line": 0, "end_line": 17},
              "content_around_reference": "  >   0:import { Basket } from './basket';"}]}})


class KnowCodeTest(unittest.TestCase):
    def test_impact_rows_are_declarations_and_the_frontier_is_skipped(self):
        items, cursor = adapters.kc_impact_decls(KC_IMPACT)
        self.assertEqual(items, [('decl', 'src/harvest.ts', 4), ('decl', 'src/harvest.ts', 10)])
        self.assertIsNone(cursor)

    def test_impact_cursor_as_the_answer_writes_it(self):
        _, cursor = adapters.kc_impact_decls(KC_IMPACT + 'incomplete (capped): items[]: cut short; kc_more {cursor: "kc~1"}\n')
        self.assertEqual(cursor, 'kc~1')
        _, cursor = adapters.kc_impact_decls(KC_IMPACT + 'more: kc_more {"cursor": "abc"}\n')
        self.assertEqual(cursor, 'abc')

    def test_impact_continuation_pages_are_json(self):
        page = json.dumps({'cursor': 'kc~2', 'list': 'items[0].affected', 'offset': 30, 'next_offset': 45,
                           'items': [{'file': 'src/hooks/useFetchItems.ts', 'line': 54, 'site': 'src/hooks/useFetchItems.ts:55',
                                      'hop': 1, 'kind': 'function', 'qualified': 'useGetItems'}],
                           'incomplete': [{'hint': 'kc_more {cursor: <`cursor`>}', 'reason': 'capped'}]})
        self.assertEqual(adapters.kc_impact_decls(page), ([('decl', 'src/hooks/useFetchItems.ts', 54)], 'kc~2'))
        last = json.dumps({'items': [{'file': 'a.ts', 'line': 3, 'hop': 2}]})
        self.assertEqual(adapters.kc_impact_decls(last), ([('decl', 'a.ts', 3)], None))

    def test_impact_rows_in_paths_with_spaces(self):
        text = ('method ParsableCommand.run  Sources/ArgumentParser/Parsable Types/ParsableCommand.swift:32\n'
                '  called by method Main.main  Sources/Main Things/main.swift:9  [exact] direct (declared at line 7)\n')
        self.assertEqual(adapters.kc_impact_decls(text)[0], [('decl', 'Sources/Main Things/main.swift', 7)])
        self.assertEqual(adapters.kc_target_name('impact', text), 'run')

    def test_refs_sites(self):
        text = json.dumps({"items": [{"file": "src/harvest.ts", "line": 7}], "cursor": "c1"})
        self.assertEqual(adapters.kc_refs_sites(text), ([('site', 'src/harvest.ts', 7)], 'c1'))

    def test_the_answered_target_is_named(self):
        self.assertEqual(adapters.kc_target_name('impact', KC_IMPACT), 'weigh')
        self.assertEqual(adapters.kc_target_name('refs', json.dumps({"name": "Basket.weigh", "items": []})), 'weigh')

    def test_a_refusal_naming_no_definition_is_not_found(self):
        self.assertTrue(adapters.kc_not_found('src/a.ts:3 names no definition'))
        self.assertFalse(adapters.kc_not_found('deadline'))


class CodebaseMemoryTest(unittest.TestCase):
    def test_lookup_takes_the_candidate_holding_the_line(self):
        rows, more = adapters.cbm_rows(CBM_SEARCH)
        self.assertFalse(more)
        self.assertEqual(adapters.cbm_pick(rows, 'src/basket.ts', 4, 'weigh')['qn'], 'p.src.basket.Basket.weigh')
        self.assertIsNone(adapters.cbm_pick(rows, 'src/basket.ts', 9, 'weigh'))

    def test_trace_callers_carry_their_hop(self):
        self.assertEqual(adapters.cbm_callers(CBM_TRACE), ([('p.src.harvest.gather', 1), ('p.src.harvest.harvest', 2)], None))


class SerenaTest(unittest.TestCase):
    def test_symbols_are_one_based(self):
        text = json.dumps([{"name_path": "Basket/weigh", "kind": "Method", "relative_path": "src/basket.ts",
                            "body_location": {"start_line": 3, "end_line": 5}}])
        s = adapters.serena_symbols(text)
        self.assertEqual((s[0]['first'], s[0]['last']), (4, 6))
        self.assertEqual(adapters.serena_pick(s, 'src/basket.ts', 4, 'weigh')['name_path'], 'Basket/weigh')

    def test_overload_index_and_type_parameters_are_not_part_of_the_name(self):
        self.assertEqual(adapters.serena_last('Basket/weigh[1]'), 'weigh')
        self.assertEqual(adapters.serena_last('Ns/InboundWebSocketMessage<T>'), 'InboundWebSocketMessage')
        self.assertEqual(adapters.serena_last('Logger/Write<T0, T1, T2>[3]'), 'Write')
        self.assertEqual(adapters.plain('pick(_:)'), 'pick')

    def test_references_read_the_marked_lines(self):
        refs = adapters.serena_references(SERENA_REFS)
        self.assertEqual([(r['kind'], r['sites']) for r in refs], [('Function', [7]), ('File', [1])])

    def test_a_shortened_answer_names_nothing(self):
        self.assertEqual(adapters._symbols('The answer is too long (12000 characters).'), [])


if __name__ == '__main__':
    unittest.main()
