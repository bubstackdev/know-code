import unittest

from compare import questions


def decl(key, name, line, group='callable', file='a.cs'):
    return {'t': 'decl', 'repo': 'demo', 'key': key, 'name': name, 'file': file, 'line': line, 'decl': group}


def ref(key, name, line, within, kind='call', file='b.cs', inside=True):
    return {'t': 'ref', 'repo': 'demo', 'key': key, 'name': name, 'file': file, 'line': line, 'col': 1,
            'kind': kind, 'inside': inside, 'in': within}


# Save is called from Load and Run; Load is called from Main; Store is a type named once, in Run.
ROWS = [
    decl('M:Save', 'Save', 3), decl('M:Load', 'Load', 9), decl('M:Run', 'Run', 15), decl('M:Main', 'Main', 20),
    decl('T:Store', 'Store', 1, group='type'),
    ref('M:Save', 'Save', 10, 'M:Load'), ref('M:Save', 'Save', 16, 'M:Run'),
    ref('M:Load', 'Load', 21, 'M:Main'),
    ref('T:Store', 'Store', 17, 'M:Run', kind='type'),
    ref('M:Save', 'Save', 30, None),                       # at file level: a site, no caller
    ref('X:Out', 'Save', 40, 'M:Run', inside=False),       # bound outside: neither a site nor a target
]


class DrawTest(unittest.TestCase):
    def setUp(self):
        self.repo = questions.Repo(ROWS)

    def test_impact_joins_two_hops_and_skips_type_references(self):
        self.assertEqual(self.repo.callers('M:Save'), ['M:Load', 'M:Run'])
        self.assertEqual(self.repo.impact('M:Save'), ['M:Load', 'M:Main', 'M:Run'])
        self.assertEqual(self.repo.callers('T:Store'), [])

    def test_refs_labels_are_inside_sites(self):
        self.assertEqual(self.repo.sites('M:Save'), [('b.cs', 10), ('b.cs', 16), ('b.cs', 30)])

    def test_the_draw_is_fixed_by_the_rows_alone(self):
        a, sa = questions.draw(ROWS)
        b, sb = questions.draw(list(reversed(ROWS)))
        self.assertEqual((a, sa), (b, sb))
        self.assertEqual({q['type'] for q in a}, {'refs', 'impact', 'find'})
        impact = [q for q in a if q['type'] == 'impact']
        self.assertEqual([q['key'] for q in impact], ['M:Load', 'M:Save'])

    def test_each_impact_question_also_asks_callers(self):
        qs, _ = questions.draw(ROWS)
        asked = questions.expand(qs)
        callers = [q for q in asked if q['type'] == 'callers']
        self.assertEqual(len(callers), len([q for q in qs if q['type'] == 'impact']))
        save = next(q for q in callers if q['key'] == 'M:Save')
        self.assertEqual((save['id'], save['labels']), ('demo:callers:M:Save', [['a.cs', 9, 'Load'], ['a.cs', 15, 'Run']]))

    def test_bound_lines_names_every_binding_of_the_name(self):
        self.assertEqual(questions.bound_lines(ROWS, 'Save')[('b.cs', 40)], {'X:Out'})

    def test_buckets(self):
        self.assertEqual(questions.bucket(10, questions.IMPACT_BUCKETS), '10+')
        self.assertIsNone(questions.bucket(201, questions.REFS_BUCKETS))


class CheckTest(unittest.TestCase):
    def test_identity_labels_drawn_again_ignore_the_new_field(self):
        import gzip, json, tempfile
        from pathlib import Path
        # Two autofac references, one of them drawn; the committed set is the draw of the same rows without `in`.
        rows = [dict(ref('M:Save', 'Save', 10, 'M:Load'), repo='autofac', symbol='Save', locs=[['a.cs', 3]], origin=None),
                dict(decl('M:Save', 'Save', 3), repo='autofac')]
        with tempfile.TemporaryDirectory() as d:
            enum = Path(d)/'enumeration-autofac.jsonl.gz'
            with gzip.open(enum, 'wt') as f:
                f.write(''.join(json.dumps(r) + '\n' for r in rows))
            import sys
            sys.path.insert(0, str(questions.ROOT/'scripts/eval/identity'))
            import sample
            labels = sample.draw([{k: v for k, v in r.items() if k != 'in'} for r in rows], sample.languages())[0]
            committed = Path(d)/'labels.jsonl'
            committed.write_text(''.join(json.dumps(l) + '\n' for l in labels))
            self.assertEqual(questions.check([enum], committed), ([], [], []))
            committed.write_text(committed.read_text().replace('"line": 10', '"line": 11'))
            self.assertNotEqual(questions.check([enum], committed), ([], [], []))


if __name__ == '__main__':
    unittest.main()
