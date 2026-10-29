"""Identity gate: the draw, the Swift dump reader, and judging KnowCode's rows, on made-up labels and answers."""
import importlib.util
from pathlib import Path
import tempfile
import unittest

import identity_gate

HERE = Path(__file__).resolve().parent


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


sample = load('identity_sample', HERE/'identity/sample.py')
swift_label = load('identity_swift_label', HERE/'identity/swift/label.py')


def label(**kw):
    out = dict(id='r:a.swift:3:5', repo='r', lang='swift', file='a.swift', line=3, col=5, name='load', kind='call',
               where='inside', bucket='2-4', multiplicity=2, locs=[['b.swift', 2]], origin=None, symbol='load()')
    out.update(kw)
    return out


SOURCES = {
    'a.swift': ['import Foo', '', '    x.load() + y.load()', '    load()'],
    'b.swift': ['struct B {', '    func load() {}', '}', '@discardableResult', 'func load() -> Int { 0 }',
                '/// Calls load once.', '@available(*, deprecated, message: "use load")', '@inlinable func load(x: Int) {}',
                '    [Obsolete("load")]', '    public void load() {}'],
}


def lines_of(rel):
    return SOURCES.get(rel, [])


class RowTest(unittest.TestCase):
    def test_the_row_at_the_labels_column(self):
        rows = [dict(file='a.swift', line=3, col=7, name='load'), dict(file='a.swift', line=3, col=18, name='load')]
        self.assertIs(identity_gate.find_row(rows, label(col=18), SOURCES['a.swift'][2]), rows[1])
        # Two uses on the line and no row at the column: no row, rather than a guess.
        self.assertIsNone(identity_gate.find_row(rows, label(col=9), SOURCES['a.swift'][2]))

    def test_the_only_row_when_the_name_appears_once(self):
        rows = [dict(file='a.swift', line=4, col=1, name='load')]
        self.assertIs(identity_gate.find_row(rows, label(line=4, col=5), SOURCES['a.swift'][3]), rows[0])
        self.assertIsNone(identity_gate.find_row(rows, label(line=4, col=5, name='save'), SOURCES['a.swift'][3]))

    def test_a_target_matches_at_its_name_line(self):
        self.assertEqual(identity_gate.name_line(SOURCES['b.swift'], 4, 'load'), 5)
        self.assertEqual(identity_gate.name_line(SOURCES['b.swift'], 2, 'load'), 2)
        # Comments and attributes naming it come before the name.
        self.assertEqual(identity_gate.name_line(SOURCES['b.swift'], 6, 'load'), 8)
        self.assertEqual(identity_gate.name_line(SOURCES['b.swift'], 9, 'load'), 10)

    def test_a_go_embedded_pointer_field_is_not_a_comment(self):
        lines = ['type Request struct {', '\t*Base', '\tBase int', '}']
        self.assertEqual(identity_gate.name_line(lines, 2, 'Base', identity_gate.comment_pattern('shop/request.go')), 2)
        self.assertEqual(identity_gate.name_line(lines, 2, 'Base', identity_gate.comment_pattern('Shop/Request.swift')), 3)
        self.assertTrue(identity_gate.target_matches(dict(file='b.swift', line=4), label(locs=[['b.swift', 5]]), lines_of))
        self.assertFalse(identity_gate.target_matches(dict(file='b.swift', line=2), label(locs=[['b.swift', 5]]), lines_of))
        self.assertFalse(identity_gate.target_matches(dict(file='c.swift', line=2), label(), lines_of))


class OverloadTest(unittest.TestCase):
    def test_a_rows_overloads_are_its_targets(self):
        overloads = ['1: load() at b.swift:2 (3 sites)', '2: load(x: Int) at b.swift:5 (test) (2 sites)', '3: load(y:) at Parsable Types/b.swift:9 (1 site)']
        rows = identity_gate.with_overloads([dict(overload=2), dict(overload=[1, 3]), dict(targets=[dict(file='c', line=1)])],
                                            overloads)
        self.assertEqual(rows[0]['targets'], [dict(file='b.swift', line=5)])
        self.assertEqual([(t['file'], t['line']) for t in rows[1]['targets']], [('b.swift', 2), ('Parsable Types/b.swift', 9)])
        self.assertEqual(rows[2]['targets'], [dict(file='c', line=1)])


class JudgeTest(unittest.TestCase):
    def judge(self, l, confidence, *targets):
        row = None if confidence is None else dict(confidence=confidence,
                                                   targets=[dict(file=f, line=n) for f, n in targets])
        return identity_gate.judge(l, row, lines_of)

    def test_inside(self):
        self.assertTrue(self.judge(label(), 'exact', ('b.swift', 2))['right'])
        self.assertFalse(self.judge(label(), 'exact', ('b.swift', 5))['right'])
        ambiguous = self.judge(label(), 'ambiguous', ('b.swift', 2), ('b.swift', 5))
        self.assertEqual((ambiguous['right'], ambiguous['among']), (False, True))
        self.assertFalse(self.judge(label(), 'unresolved')['right'])
        self.assertEqual(self.judge(label(), None)['confidence'], 'none')

    def test_outside_is_right_only_unlinked(self):
        out = label(where='outside', bucket='1+', locs=[], origin='stdlib')
        self.assertTrue(self.judge(out, 'unresolved')['right'])
        self.assertTrue(self.judge(out, None)['right'])
        self.assertFalse(self.judge(out, 'exact', ('b.swift', 2))['right'])

    def test_summary_and_verdict(self):
        out = label(id='r:a.swift:4:5', line=4, where='outside', bucket='1+', locs=[], origin='stdlib')
        judged = [self.judge(label(), 'exact', ('b.swift', 2)), self.judge(out, 'exact', ('b.swift', 2))]
        summary = identity_gate.summarize(judged, [label(), out])
        swift = summary['lang']['swift']
        self.assertEqual((swift['exact_precision'], swift['outside_linked'], swift['recall_linked']), (0.5, 1, 1.0))
        self.assertEqual([w['at'] for w in summary['wrong_exact']], ['r/a.swift:4:5'])
        self.assertEqual(len(identity_gate.verdict(summary)), 2)
        judged = [self.judge(label(), 'exact', ('b.swift', 2)), self.judge(out, 'ambiguous', ('b.swift', 2))]
        summary = identity_gate.summarize(judged, [label(), out])
        self.assertEqual(identity_gate.verdict(summary), [])
        self.assertEqual(len(summary['outside_candidates']), 1)


class CauseTest(unittest.TestCase):
    def judge(self, l, *targets):
        return identity_gate.judge(l, dict(confidence='exact', targets=[dict(file=f, line=n) for f, n in targets]),
                                   lines_of)

    def test_each_wrong_exact_row_has_one_cause(self):
        out = label(id='r:a.swift:4:5', line=4, where='outside', bucket='1+', locs=[], origin='stdlib')
        rows = {
            'several_with_label': self.judge(label(id='a'), ('b.swift', 2), ('b.swift', 5)),
            'several_without_label': self.judge(label(id='b'), ('b.swift', 5), ('b.swift', 8)),
            'linked_outside': self.judge(out, ('b.swift', 2)),
            'one_wrong': self.judge(label(id='c'), ('b.swift', 5)),
        }
        for want, j in rows.items():
            self.assertEqual(identity_gate.cause(j), want)
        labels = [label(id='a'), label(id='b'), out, label(id='c')]
        summary = identity_gate.summarize(list(rows.values()), labels)
        self.assertEqual(summary['causes']['swift'], {c: 1 for c in identity_gate.CAUSES})
        with tempfile.TemporaryDirectory() as temp:
            identity_gate.write_causes(summary, temp)
            self.assertIn('(1)', (Path(temp)/'one_wrong.md').read_text())

    def test_compare_says_what_moved(self):
        base_rows = [self.judge(label(id='a'), ('b.swift', 2), ('b.swift', 5)), self.judge(label(id='c'), ('b.swift', 2))]
        cand_rows = [self.judge(label(id='a'), ('b.swift', 2)), self.judge(label(id='c'), ('b.swift', 5))]
        labels = [label(id='a'), label(id='c')]
        text = identity_gate.compare(identity_gate.summarize(base_rows, labels), identity_gate.summarize(cand_rows, labels))
        self.assertIn('| swift | wrong `exact`: several_with_label | 1 | 0 |', text)
        self.assertIn('| swift | wrong `exact`: one_wrong | 0 | 1 |', text)
        self.assertIn('fixed: 1; newly wrong: 1', text)


def ref(file, line, key, name='load', inside=True, kind='call'):
    return dict(t='ref', repo='r', file=file, line=line, col=1, name=name, kind=kind, key=key, symbol=name,
                inside=inside, locs=[['b.swift', 2]] if inside else [], origin=None if inside else 'stdlib')


class DrawTest(unittest.TestCase):
    def test_buckets_spread_and_quota(self):
        decls = [dict(t='decl', repo='r', name='load', key=k, file='b.swift', line=n, decl='callable')
                 for k, n in (('k1', 2), ('k2', 5))]
        refs = [ref('a.swift', n, 'k1') for n in range(1, 11)] + [ref('c.swift', 1, 'x', name='save', inside=False)]
        labels, stats = sample.draw(decls + refs, {'r': 'swift'})
        self.assertEqual(stats['r/call/inside/2-4'], dict(available=10, drawn=3))
        self.assertEqual(stats['r/call/outside/0'], dict(available=1, drawn=1))
        self.assertTrue(all(l['lang'] == 'swift' for l in labels))
        # The draw depends on the enumeration alone.
        again, _ = sample.draw(list(reversed(decls + refs)), {'r': 'swift'})
        self.assertEqual(labels, again)

    def test_an_excluded_projects_references_are_left_out(self):
        row = dict(ref('test/Autofac.Test.CodeGen/A.cs', 1, 'k1'), repo='autofac')
        labels, _ = sample.draw([row])
        self.assertEqual(labels, [])

    def test_a_site_bound_two_ways_is_left_out(self):
        labels, _ = sample.draw([ref('a.swift', 1, 'k1'), ref('a.swift', 1, 'k2')])
        self.assertEqual(labels, [])


DUMP = '''R\trec1\t/src/Sources/A/a.swift
O\ts:1A1BV4loadyyF\tload()\t16\t0\t2\t2\t10
L\t512\ts:1A1BV\tB\t6\t0
O\ts:1A1PP4loadyyF\tload()\t16\t0\t1\t6\t10
L\t512\ts:1A1PP\tP\t8\t0
O\ts:1A1BV4loadyyF\tload()\t16\t0\t36\t9\t7
O\ts:1A1PP4loadyyF\tload()\t16\t0\t36\t10\t7
O\ts:Sa5countSivp\tcount\t19\t0\t4\t11\t7
O\ts:1A1BV4loadyyF\tload()\t16\t0\t260\t12\t7
R\trec2\t/src/.build/checkouts/Dep/x.swift
O\ts:3Dep1XV\tX\t6\t0\t2\t1\t8
R\trec3\t/src/Sources/A/c.swift
O\ts:3Dep1XV\tX\t6\t0\t4\t1\t8
'''


class SwiftDumpTest(unittest.TestCase):
    def test_references_from_a_dump(self):
        with tempfile.TemporaryDirectory() as temp:
            src = Path(temp)
            (src/'Sources/A').mkdir(parents=True)
            lines = [''] * 12
            lines[1] = 'struct B { func load() {} }'
            lines[5] = 'protocol P { func load() }'
            lines[8] = '    b.load()'
            lines[9] = '    p.load()'
            lines[10] = '    a.count'
            lines[11] = '    b.load()'
            (src/'Sources/A/a.swift').write_text('\n'.join(lines))
            (src/'Sources/A/c.swift').write_text('let x: X')
            dump = DUMP.replace('/src/', str(src.resolve()) + '/')
            rows = swift_label.enumerate_repo('r', src, swift_label.parse(dump.splitlines(True)))
        refs = {(r['file'], r['line']): r for r in rows if r['t'] == 'ref'}
        self.assertEqual(refs[('Sources/A/a.swift', 9)]['kind'], 'call')
        self.assertEqual(refs[('Sources/A/a.swift', 9)]['locs'], [['Sources/A/a.swift', 2]])
        self.assertEqual(refs[('Sources/A/a.swift', 10)]['kind'], 'dispatch')
        self.assertEqual((refs[('Sources/A/a.swift', 11)]['inside'], refs[('Sources/A/a.swift', 11)]['origin']),
                         (False, 'stdlib'))
        self.assertEqual(refs[('Sources/A/c.swift', 1)]['origin'], 'package')
        # The implicit reference on line 12 is left out.
        self.assertNotIn(('Sources/A/a.swift', 12), refs)
        decls = sorted((r['name'], r['line']) for r in rows if r['t'] == 'decl')
        self.assertEqual(decls, [('load', 2), ('load', 6)])


if __name__ == '__main__':
    unittest.main()
