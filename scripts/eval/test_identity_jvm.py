"""identity-v2's SCIP reader and JVM labeller, and the draw per language, on a made-up index and sources."""
import importlib.util
from pathlib import Path
import sys
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE/'identity'))
import scip  # noqa: E402


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


sample = load('identity_sample', HERE/'identity/sample.py')
jvm_label = load('identity_jvm_label', HERE/'identity/jvm/label.py')


# A protobuf writer just big enough to build SCIP messages for the reader.
def varint(n):
    n &= 0xFFFFFFFFFFFFFFFF
    out = bytearray()
    while True:
        b = n & 0x7F
        n >>= 7
        if n:
            out.append(b | 0x80)
        else:
            out.append(b)
            return bytes(out)


def field(number, value):
    if isinstance(value, int):
        return varint(number << 3) + varint(value)
    data = value.encode() if isinstance(value, str) else value
    return varint(number << 3 | 2) + varint(len(data)) + data


def occurrence(line, start, end, symbol, roles=0, legacy=False):
    if legacy:
        rng = field(1, b''.join(varint(v) for v in (line, start, end)))
    else:
        rng = field(8, field(1, line) + field(2, start) + field(3, end))
    return field(2, rng + field(2, symbol) + (field(3, roles) if roles else b''))


def symbol_info(symbol, kind, signature='', relationships=()):
    sig = field(7, field(5, signature)) if signature else b''
    rels = b''.join(field(4, field(1, r) + field(3, 1)) for r in relationships)
    return field(3, field(1, symbol) + rels + field(5, kind) + sig)


def document(path, occurrences, symbols=()):
    return field(2, field(1, path) + field(4, 'java') + b''.join(occurrences) + b''.join(symbols))


PKG = 'scip-java maven maven/org.example/demo 1.0 '
JDK = 'scip-java maven jdk 17 '
KT = 'scip-java maven maven/org.jetbrains.kotlin/kotlin-stdlib 2.4.21 '


class ScipReaderTest(unittest.TestCase):
    def test_typed_and_legacy_ranges_symbols_and_roles(self):
        data = document('A.java', [occurrence(3, 4, 8, PKG + 'a/A#', roles=scip.DEFINITION),
                                   occurrence(5, 2, 6, PKG + 'a/A#', legacy=True)],
                        [symbol_info(PKG + 'a/A#', 7, 'public class A', [JDK + 'java/lang/Object#'])])
        doc = scip.parse_index(data)['documents'][0]
        self.assertEqual(doc['relative_path'], 'A.java')
        self.assertEqual([o['range'] for o in doc['occurrences']], [[3, 4, 3, 8], [5, 2, 5, 6]])
        self.assertEqual(doc['occurrences'][0]['symbol_roles'], scip.DEFINITION)
        s = doc['symbols'][0]
        self.assertEqual((s['kind'], s['signature']), (7, 'public class A'))
        self.assertTrue(s['relationships'][0]['is_implementation'])

    def test_a_range_starting_at_zero_keeps_its_zeros(self):
        doc = scip.parse_index(document('A.java', [occurrence(0, 0, 3, PKG + 'a/A#')]))['documents'][0]
        self.assertEqual(doc['occurrences'][0]['range'], [0, 0, 0, 3])

    def test_byte_columns_from_utf16(self):
        # '\U0001F600' is two UTF-16 units and four UTF-8 bytes; 'é' one unit and two bytes.
        line = 'é\U0001F600 x'
        self.assertEqual(scip.byte_column(line, 4, 2), 8)
        self.assertEqual(scip.byte_column(line, 3, 3), 8)
        self.assertEqual(scip.byte_column(line, 7, 1), 8)


SOURCES = {
    'src/a/Shape.java': [
        'package a;',
        'import java.util.List;',
        'public interface Shape { double area(); }',
        'enum Color { RED }',
        'class Square implements Shape {',
        '  public double area() { List<String> xs = null; return side(); }',
        '  double side() { return 1; }',
        '  Color c = Color.RED; Color[] all = Color.values();',
        '  void use(Shape s) { s.area(); getSide(); }',
        '}',
    ],
    'src/a/Marks.java': [
        'package a;',
        '@interface Mark { int level(); }',
        'interface Limits { int MAX = 3; }',
        '@Mark(level = Limits.MAX) class Marked {}',
        'abstract class Base { abstract int size(); }',
        'class Sized extends Base { int size() { return 1; } int twice(Base b) { return b.size() * 2; } }',
    ],
    'src/a/Ext.kt': [
        'package a',
        'fun String.shout(): String = uppercase()',
        'fun use() = "x".shout()',
    ],
    'build/generated/a/Gen.java': [
        'class Gen { void gen() {} }',
    ],
}


def index():
    shape = PKG + 'a/Shape#'
    area = PKG + 'a/Shape#area().'
    sq_area = PKG + 'a/Square#area().'
    side = PKG + 'a/Square#side().'
    color, red = PKG + 'a/Color#', PKG + 'a/Color#RED.'
    shout = PKG + 'a/ExtKt#shout().'
    gen = PKG + 'a/Gen#gen().'
    java = document('src/a/Shape.java', [
        occurrence(1, 17, 21, JDK + 'java/util/List#'),          # an import: left out
        occurrence(2, 17, 22, shape, roles=scip.DEFINITION),
        occurrence(2, 32, 36, area, roles=scip.DEFINITION),
        occurrence(3, 5, 10, color, roles=scip.DEFINITION),
        occurrence(3, 13, 16, red, roles=scip.DEFINITION),
        occurrence(4, 24, 29, shape),                            # type, inside
        occurrence(5, 16, 20, sq_area, roles=scip.DEFINITION),
        occurrence(5, 25, 29, JDK + 'java/util/List#'),          # type, outside, stdlib
        occurrence(5, 56, 60, side),                             # call, inside
        occurrence(6, 9, 13, side, roles=scip.DEFINITION),
        occurrence(7, 2, 7, color),
        occurrence(7, 12, 17, color),
        occurrence(7, 18, 21, red),                              # member, inside
        occurrence(7, 43, 49, PKG + 'a/Color#values().'),        # synthesised: left out
        occurrence(8, 11, 16, shape),
        occurrence(8, 24, 28, area),                             # dispatch: an interface's member
        occurrence(8, 33, 40, side),                             # renamed (the token isn't `side`): left out
    ], [symbol_info(shape, 21, 'public interface Shape'), symbol_info(area, 66, 'public abstract double area()'),
        symbol_info(color, 11), symbol_info(red, 0), symbol_info(sq_area, 26), symbol_info(side, 26)])
    kotlin = document('src/a/Ext.kt', [
        occurrence(1, 11, 16, shout, roles=scip.DEFINITION),
        occurrence(1, 29, 38, KT + 'kotlin/text/StringsKt#uppercase().'),  # call, outside, stdlib
        occurrence(2, 16, 21, shout),                                       # extension, inside
    ], [symbol_info(shout, 26, 'public final fun String.shout(): String')])
    mark, level = PKG + 'a/Mark#', PKG + 'a/Mark#level().'
    base, size = PKG + 'a/Base#', PKG + 'a/Base#size().'
    limits, max_ = PKG + 'a/Limits#', PKG + 'a/Limits#MAX.'
    marks = document('src/a/Marks.java', [
        occurrence(1, 11, 15, mark, roles=scip.DEFINITION),
        occurrence(1, 22, 27, level, roles=scip.DEFINITION),
        occurrence(2, 10, 16, limits, roles=scip.DEFINITION),
        occurrence(2, 23, 26, max_, roles=scip.DEFINITION),
        occurrence(3, 6, 11, level),                             # an annotation's element: a call, not dispatch
        occurrence(3, 21, 24, max_),                             # an interface's constant: a member
        occurrence(4, 15, 19, base, roles=scip.DEFINITION),
        occurrence(4, 35, 39, size, roles=scip.DEFINITION),
        occurrence(5, 20, 24, base),                             # an abstract class named: a type
        occurrence(5, 81, 85, size),                             # abstract by its signature: dispatch
    ], [symbol_info(mark, 21, '@interface Mark', [JDK + 'java/lang/annotation/Annotation#']),
        symbol_info(level, 66, 'abstract int level()'), symbol_info(limits, 21, 'interface Limits'),
        symbol_info(max_, 79, 'static final int MAX'), symbol_info(base, 7, 'abstract class Base'),
        symbol_info(size, 26, '@Override\nabstract int size()')])
    generated = document('build/generated/a/Gen.java', [occurrence(0, 17, 20, gen, roles=scip.DEFINITION)])
    return scip.parse_index(java + marks + kotlin + generated)


class JvmLabelTest(unittest.TestCase):
    def test_references_and_declarations_from_an_index(self):
        with tempfile.TemporaryDirectory() as temp:
            src = Path(temp)
            for path, lines in SOURCES.items():
                (src/path).parent.mkdir(parents=True, exist_ok=True)
                (src/path).write_text('\n'.join(lines))
            rows = jvm_label.enumerate_repo('r', src, index())
        refs = {(r['file'], r['line'], r['name']): r for r in rows if r['t'] == 'ref'}
        java = 'src/a/Shape.java'
        self.assertNotIn((java, 2, 'List'), refs)
        self.assertEqual(refs[(java, 5, 'Shape')]['kind'], 'type')
        self.assertEqual(refs[(java, 5, 'Shape')]['locs'], [[java, 3]])
        self.assertEqual((refs[(java, 6, 'List')]['inside'], refs[(java, 6, 'List')]['origin']), (False, 'stdlib'))
        self.assertEqual(refs[(java, 6, 'side')]['kind'], 'call')
        self.assertEqual(refs[(java, 6, 'side')]['col'], 57)
        self.assertEqual(refs[(java, 8, 'RED')]['kind'], 'member')
        self.assertNotIn((java, 8, 'values'), refs)
        self.assertEqual(refs[(java, 9, 'area')]['kind'], 'dispatch')
        self.assertNotIn((java, 9, 'side'), refs)
        self.assertEqual(refs[('src/a/Marks.java', 4, 'level')]['kind'], 'call')
        self.assertEqual(refs[('src/a/Marks.java', 4, 'MAX')]['kind'], 'member')
        self.assertEqual(refs[('src/a/Marks.java', 6, 'Base')]['kind'], 'type')
        self.assertEqual(refs[('src/a/Marks.java', 6, 'size')]['kind'], 'dispatch')
        self.assertEqual(refs[('src/a/Ext.kt', 3, 'shout')]['kind'], 'extension')
        self.assertEqual(refs[('src/a/Ext.kt', 3, 'shout')]['lang'], 'kotlin')
        self.assertEqual(refs[('src/a/Ext.kt', 2, 'uppercase')]['origin'], 'stdlib')
        self.assertEqual(refs[(java, 6, 'side')]['lang'], 'java')
        decls = sorted((r['name'], r['decl']) for r in rows if r['t'] == 'decl')
        # The generated file's `gen` isn't the repository's source.
        self.assertEqual(decls, [('Base', 'type'), ('Color', 'type'), ('Limits', 'type'), ('MAX', 'member'),
                                 ('Mark', 'type'), ('RED', 'member'), ('Shape', 'type'), ('area', 'callable'), ('area', 'callable'),
                                 ('level', 'callable'), ('shout', 'callable'), ('side', 'callable'),
                                 ('size', 'callable')])

    def test_symbols_split_into_package_and_descriptors(self):
        package, descriptors = jvm_label.split_symbol(PKG + 'a/Outer#`in`.inner(+1).(x)')
        self.assertEqual(package, 'maven maven/org.example/demo 1.0')
        self.assertEqual(descriptors, [('a', '/'), ('Outer', '#'), ('in', '.'), ('inner', '()'), ('x', '(param)')])
        self.assertEqual(jvm_label.enclosing(PKG + 'a/Outer#inner(+1).'), PKG + 'a/Outer#')
        self.assertEqual(jvm_label.enclosing(PKG + 'a/Outer#`in`.'), PKG + 'a/Outer#')
        self.assertIsNone(jvm_label.split_symbol('local 3'))


class DrawLanguageTest(unittest.TestCase):
    def test_a_rows_language_draws_it_apart(self):
        def ref(file, line, lang):
            return dict(t='ref', repo='m', lang=lang, file=file, line=line, col=1, name='f', kind='call', key='k',
                        symbol='f', inside=False, locs=[], origin='stdlib')
        rows = [ref('A.java', n, 'java') for n in range(1, 4)] + [ref('B.kt', n, 'kotlin') for n in range(1, 4)]
        labels, stats = sample.draw(rows, {'m': 'jvm'})
        self.assertEqual(stats['m/java/call/outside/0'], dict(available=3, drawn=3))
        self.assertEqual(stats['m/kotlin/call/outside/0'], dict(available=3, drawn=3))
        self.assertEqual(sorted({l['lang'] for l in labels}), ['java', 'kotlin'])


if __name__ == '__main__':
    unittest.main()
