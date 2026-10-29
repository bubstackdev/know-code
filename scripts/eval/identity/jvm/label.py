#!/usr/bin/env python3
"""Java and Kotlin references and declarations from a scip-java index, as identity-v2 enumerates them.

    label.py --repo NAME --src DIR INDEX.scip | gzip > enumeration-NAME.jsonl.gz

Runs in the labelling container, with scip.py in the directory above it. Every occurrence in one of the repository's own
`.java` or `.kt` files gives a reference or a definition; a symbol defined in one of them is inside, any other is
outside. The rule is eval/identity-v2/README.md's.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from scip import DEFINITION, byte_column, read_index  # noqa: E402

LANGUAGES = {'.java': 'java', '.kt': 'kotlin'}
# compare-v1 (eval/compare-v1/README.md) adds `in` to each reference row; identity-v2's output is unchanged without it.
ENCLOSING = os.environ.get('LABEL_ENCLOSING') == '1'
# Build output and generated sources: annotation processors' and KSP's output, Kotlin's and Gradle's build dirs.
GENERATED = ('build', 'target', 'generated', 'out', '.gradle')
ABSTRACT = re.compile(r'\babstract\b')
IMPORT_LINE = re.compile(r'\s*import\s')
IDENT = re.compile(r'[A-Za-z_$][A-Za-z0-9_$]*')
# A property's accessor, as scip-kotlinc names it: `name.get().` or `name.set().` under the property's term.
ACCESSOR = re.compile(r'\.(?:get|set)\(\)\.$')
# Kotlin's operator conventions and the functions a `for` loop or destructuring calls.
OPERATORS = {'get', 'set', 'contains', 'iterator', 'next', 'hasNext', 'plus', 'minus', 'times', 'div', 'rem', 'mod',
             'inc', 'dec', 'not', 'unaryMinus', 'unaryPlus', 'compareTo', 'equals', 'invoke', 'rangeTo', 'rangeUntil',
             'plusAssign', 'minusAssign', 'timesAssign', 'divAssign', 'remAssign', 'getValue', 'setValue',
             'provideDelegate'} | {f'component{n}' for n in range(1, 23)}
# SymbolInformation kinds (scip.proto) of the declarations the rule counts, and of the types whose members dispatch.
INTERFACE, ABSTRACT_METHOD, CONSTRUCTOR, TYPE_PARAMETER = 21, 66, 9, 58
# Outside packages that are the platform's standard library; any other outside package is a package.
STDLIB_PACKAGES = ('jdk', 'maven/org.jetbrains.kotlin/kotlin-stdlib', 'maven/org.jetbrains.kotlin/kotlin-reflect')
# The first package segments of the JDK's and Kotlin's standard libraries, for symbols that name no artifact (javax is
# left out: javax.inject and others like it are packages).
STDLIB_ROOTS = ('java', 'jdk', 'kotlin')


def split_symbol(symbol):
    """A global SCIP symbol as (package, [(name, suffix)]); suffix is '/', '#', '.', '()', '[]', '(param)', ':' or '!'.

    The package is the manager, name and version, with each field's escaped spaces kept. Returns None for a local.
    """
    if symbol.startswith('local '):
        return None
    fields, i = [], 0
    while len(fields) < 4:
        j = i
        while True:
            j = symbol.index(' ', j)
            if symbol.startswith('  ', j):
                j += 2
                continue
            break
        fields.append(symbol[i:j])
        i = j + 1
    package = ' '.join(fields[1:4])
    rest, descriptors = symbol[i:], []
    k = 0
    while k < len(rest):
        if rest[k] == '`':
            end = k + 1
            while True:
                end = rest.index('`', end)
                if rest.startswith('``', end):
                    end += 2
                    continue
                break
            name, k = rest[k + 1:end].replace('``', '`'), end + 1
        elif rest[k] in '[(':
            close = ']' if rest[k] == '[' else ')'
            end = rest.index(close, k)
            name, k = rest[k + 1:end], end + 1
            descriptors.append((name, '[]' if close == ']' else '(param)'))
            continue
        else:
            m = re.match(r'[A-Za-z0-9_+\-$<>]+', rest[k:])
            name, k = (m.group(0), k + m.end()) if m else ('', k)
        if k < len(rest) and rest[k] == '(':
            end = rest.index(').', k)
            descriptors.append((name, '()'))
            k = end + 2
        elif k < len(rest):
            descriptors.append((name, rest[k]))
            k += 1
        else:
            break
    return package, descriptors


def enclosing(symbol):
    """The symbol of the declaration enclosing a global symbol, by cutting its last descriptor off the text."""
    s = symbol
    if s.endswith(').'):
        # A method: name(disambiguator).
        open_paren = s.rfind('(')
        s = s[:open_paren]
    elif s.endswith((')', ']')):
        s = s[:s.rfind('(' if s.endswith(')') else '[')]
        return s
    else:
        s = s[:-1]
    if s.endswith('`'):
        start = s.rfind('`', 0, len(s) - 1)
        while start > 0 and s[start - 1] == '`':
            start = s.rfind('`', 0, start - 1)
        return s[:start]
    m = re.search(r'[A-Za-z0-9_+\-$<>]*$', s)
    return s[:m.start()]


def descriptor_of(symbol):
    """The last descriptor of a global symbol, as (name, suffix)."""
    parts = split_symbol(symbol)
    return parts[1][-1] if parts and parts[1] else (None, None)


def enumerate_repo(repo, src, index):
    src = Path(src).resolve()

    def generated(path):
        return any(part in GENERATED for part in Path(path).parts[:-1])

    defs = {}  # symbol -> {'locs': set((file, line)), 'generated': bool}
    info = {}  # symbol -> SymbolInformation
    texts = {}

    def lines_of(rel):
        if rel not in texts:
            try:
                texts[rel] = (src/rel).read_bytes().decode('utf-8', 'replace').split('\n')
            except OSError:
                texts[rel] = []
        return texts[rel]

    docs = []
    for d in index['documents']:
        rel = d['relative_path']
        lang = LANGUAGES.get(Path(rel).suffix)
        if not lang:
            continue
        gen = generated(rel)
        for s in d['symbols']:
            info.setdefault(s['symbol'], s)
        for o in d['occurrences']:
            if o['symbol_roles'] & DEFINITION and not o['symbol'].startswith('local ') and o['range']:
                e = defs.setdefault(o['symbol'], dict(locs=set(), generated=True))
                if not gen:
                    e['generated'] = False
                    e['locs'].add((rel, name_line(lines_of(rel), o['range'][0] + 1, descriptor_of(o['symbol'])[0])))
        if not gen:
            docs.append((rel, lang, d))

    def name_and_group(symbol):
        """The declaration's simple name and its group (type, callable, member), or None when the rule leaves it out."""
        name, suffix = descriptor_of(symbol)
        if not name or not IDENT.fullmatch(name):
            return None
        s = info.get(symbol)
        kind = s['kind'] if s else 0
        if kind in (CONSTRUCTOR, TYPE_PARAMETER) or name in ('<init>', '<clinit>'):
            return None
        if suffix == '#':
            return name, 'type'
        if suffix == '()':
            return name, 'callable'
        if suffix == '.':
            return name, 'member'
        return None

    out = []
    for symbol, d in sorted(defs.items()):
        if d['generated'] or not d['locs']:
            continue
        got = name_and_group(symbol)
        if got:
            f, ln = sorted(d['locs'])[0]
            out.append(dict(t='decl', repo=repo, name=got[0], key=symbol, file=f, line=ln, decl=got[1]))

    # Types the repository declares: those the index defines, and those any of its source files may declare.
    own_types = declared_types(src, {d['relative_path'] for d in index['documents']}, generated)
    own_types |= {top_type(symbol) for symbol, d in defs.items() if not d['generated']} - {None}
    def synthesised(symbol):
        """A member the compiler makes for one of the repository's types (an enum's `values()`, a record's accessor,
        a data class's `copy`), which has no definition in source: not outside, and not labelled."""
        up = enclosing(symbol)
        return up in defs

    members = {(enclosing(symbol), descriptor_of(symbol)[0]) for symbol, d in defs.items() if not d['generated']}
    # compare-v1's `in`: each counted declaration's span, from its name line to its end (extent), per file.
    spans = {}
    if ENCLOSING:
        for symbol, d in defs.items():
            got = None if d['generated'] else name_and_group(symbol)
            if got:
                word = re.compile(r'(?<![\w$])' + re.escape(got[0]) + r'(?![\w$])')
                for f, ln in d['locs']:
                    lines = lines_of(f)
                    # A member the compiler synthesises at another declaration (a data class's `componentN`, a
                    # property's `get`) has no source of its own: its name isn't on its line, and it encloses nothing.
                    if ln > len(lines) or not word.search(lines[ln - 1]):
                        continue
                    last = extent(lines, ln, LANGUAGES[Path(f).suffix], got[1] in ('type', 'callable'))
                    spans.setdefault(f, []).append((ln, last, symbol))
    seen = set()
    for rel, lang, d in docs:
        lines = lines_of(rel)
        for o in d['occurrences']:
            symbol, rng = o['symbol'], o['range']
            if o['symbol_roles'] & DEFINITION or symbol.startswith('local ') or not rng or rng[0] != rng[2]:
                continue
            # scip-kotlinc binds some property reads and writes to the property's accessor (`rawType.get()`); the
            # token names the property, so the reference is the property's.
            if ACCESSOR.search(symbol):
                symbol = enclosing(symbol)
            got = name_and_group(symbol)
            if not got:
                continue
            name, group = got
            line = rng[0] + 1
            text = lines[line - 1] if line <= len(lines) else ''
            col = byte_column(text, rng[1], d['position_encoding'])
            if (rel, line, col, symbol) in seen:
                continue
            seen.add((rel, line, col, symbol))
            target = defs.get(symbol)

            def exclude(reason):
                out.append(dict(t='excluded', reason=reason, repo=repo, lang=lang, file=rel, line=line, col=col,
                                name=name, key=symbol))
            # A reference the toolchain binds that the rule doesn't label is recorded with its reason, so that what
            # the set doesn't judge is counted (eval/identity-v2/README.md).
            # scip-java doesn't mark import occurrences with the Import role; an import is one line in both languages.
            if IMPORT_LINE.match(text):
                exclude('import')
                continue
            # The token must be the declaration's own name: an implicit reference has none, and a renamed one (a
            # Kotlin property read through a Java getter, an import alias) has another.
            token = IDENT.match(text.encode('utf-8')[col - 1:].decode('utf-8', 'replace'))
            if token and token.group(0) != name:
                # scip-kotlinc's range for a qualified name (`java.util.List`) covers the qualifier; the name ends it.
                end = byte_column(text, rng[3], d['position_encoding'])
                tail = text.encode('utf-8')[:end - 1].decode('utf-8', 'replace')
                if tail.endswith(name) and not IDENT.fullmatch(tail[-len(name) - 1:]):
                    col = end - len(name.encode('utf-8'))
                    token = IDENT.match(text.encode('utf-8')[col - 1:].decode('utf-8', 'replace'))
            if group == 'member' and name == 'class':
                exclude('class_literal')
                continue
            if not token or token.group(0) != name:
                # An operator's function (`!x`, `a < b`, `x[i]`, `for (e in xs)`) has no token of its own name.
                exclude('operator' if name in OPERATORS else 'renamed_or_implicit')
                continue
            if target and target['generated']:
                exclude('generated_target')
                continue
            if not target and (enclosing(symbol), name) in members:
                # The toolchain's symbol names a member the type declares, under another overload's disambiguator
                # (scip-kotlinc's "methodDisambiguator not found"): no one declaration can be named.
                exclude('overload_unmatched')
                continue
            if not target and synthesised(symbol):
                exclude('synthesised')
                continue
            # Outside, in a type the repository declares: a file the build didn't compile (a source set for another
            # platform, a module left out), or a published build of the repository's own code that a module depends
            # on. Neither says where a name binds in this source, so neither is labelled.
            if not target and top_type(symbol) in own_types:
                exclude('outside_own_type')
                continue
            up = info.get(enclosing(symbol)) if group != 'type' else None
            s = info.get(symbol)
            if s and extension(s, lang_of_def(target)):
                kind = 'extension'
            elif dispatched(s, up, group, lang_of_def(target)):
                kind = 'dispatch'
            elif group == 'callable':
                kind = 'call'
            elif group == 'member':
                kind = 'member'
            else:
                kind = 'type'
            row = dict(t='ref', repo=repo, lang=lang, file=rel, line=line, col=col, name=name, kind=kind, key=symbol,
                       symbol=symbol.split(' ', 4)[4] if symbol.count(' ') >= 4 else symbol)
            if target:
                row.update(inside=True, locs=sorted([list(loc) for loc in target['locs']]), origin=None)
            else:
                row.update(inside=False, locs=[], origin=origin(symbol))
            if ENCLOSING:
                row['in'] = innermost(spans.get(rel, []), line)
            out.append(row)
    return out


OPENERS, CLOSERS = '([{', ')]}'
# A Kotlin line that continues the expression above it, so a newline before it doesn't end a declaration.
CONTINUES = re.compile(r'^\s*(?:\.|\?\.|\?:|&&|\|\||[-+*%]|/(?![/*])|as\b|is\b|in\b|!in\b|else\b)')


def extent(lines, start, lang, body):
    """The last line of the declaration whose name is on line `start` (1-based), for compare-v1's `in` field: scanning
    from the name, outside strings, characters and comments (history-v1's span rule), it ends at a `;` at depth 0; for
    a type or a callable (`body`), at the `}` closing a block opened at depth 0; in Kotlin also at a newline at depth 0
    when the next line doesn't continue the expression (`fun f() = …`, `val x = …`, a class with no body). A field's
    initializer may hold braces (an anonymous class, a lambda): it runs on to its end. Unended, it ends at the file's."""
    depth, i, n = 0, start - 1, len(lines)
    text = lines[i] if i < n else ''
    j = 0
    while i < n:
        text = lines[i]
        while j < len(text):
            c, two, three = text[j], text[j:j + 2], text[j:j + 3]
            if two == '//':
                break
            if two == '/*':
                end = text.find('*/', j + 2)
                while end < 0 and i + 1 < n:
                    i, j, text = i + 1, 0, lines[i + 1]
                    end = text.find('*/')
                j = (end + 2) if end >= 0 else len(text)
                continue
            if three == '"""':  # a Java text block or a Kotlin raw string, possibly over several lines
                end = text.find('"""', j + 3)
                while end < 0 and i + 1 < n:
                    i, text = i + 1, lines[i + 1]
                    end = text.find('"""')
                j = (end + 3) if end >= 0 else len(text)
                continue
            if c in '"\'':
                k = j + 1
                while k < len(text) and text[k] != c:
                    k += 2 if text[k] == '\\' else 1
                j = k + 1
                continue
            if c in OPENERS:
                depth += 1
            elif c in CLOSERS:
                depth -= 1
                if depth < 0:  # a bracket opened before the declaration closes: it sat in a list (a parameter)
                    return i + 1
                if c == '}' and depth == 0 and body:
                    return i + 1
            elif c == ';' and depth == 0:
                return i + 1
            elif c == ',' and depth == 0 and not body:  # one of several: parameters, or fields declared together
                return i + 1
            j += 1
        if lang == 'kotlin' and depth == 0:
            following = next((lines[k] for k in range(i + 1, n)
                              if lines[k].strip() and not lines[k].lstrip().startswith(('//', '/*', '*'))), '')
            if text.strip() and not text.rstrip().endswith(('=', ',', '(', '->', '.')) and not CONTINUES.match(following) \
                    and not following.lstrip().startswith('{'):
                return i + 1
        i, j = i + 1, 0
    return n


def innermost(spans, line):
    """The key of the narrowest span holding `line`, or None at file level."""
    best = None
    for first, last, key in spans:
        if first <= line <= last and (best is None or last - first < best[1] - best[0]):
            best = (first, last, key)
    return best[2] if best else None


ANNOTATIONS = re.compile(r'^\s*(?:@\w+(?:\.\w+)*(?:\([^)]*\))?\s*)+')


def name_line(lines, line, name):
    """The line of a declaration's name token. scip-java puts the definition of an annotated declaration at the
    name's occurrence in its own annotation (`@JsonAdapter(A.Adapter.class)` above `class A`): from a line that starts
    with an annotation, the next line, within 10, with the name as a word outside annotations."""
    if not name or line > len(lines) or not lines[line - 1].lstrip().startswith('@'):
        return line
    word = re.compile(r'(?<![\w$])' + re.escape(name) + r'(?![\w$])')
    for n in range(line, min(line + 10, len(lines)) + 1):
        if word.search(ANNOTATIONS.sub('', lines[n - 1], count=1)):
            return n
    return line


TYPE_DECLARATION = re.compile(r'\b(?:class|interface|object|enum|record|typealias)\s+([A-Za-z_$][A-Za-z0-9_$]*)')
PACKAGE_LINE = re.compile(r'^\s*package\s+([A-Za-z0-9_.`]+)', re.M)
JVM_NAME = re.compile(r'@file\s*:\s*JvmName\(\s*"([^"]+)"')


def declared_types(src, indexed, generated):
    """`package/path/Name` for each type a repository source file may declare: every name after a type keyword, the
    file's own name, its Kotlin facade (`NameKt`, or its `@file:JvmName`). A superset read from the text, so that
    nothing the repository declares is taken for outside. Logs the source files the index lacks."""
    out, missing = set(), []
    for path in sorted(src.rglob('*')):
        rel = path.relative_to(src).as_posix()
        if path.suffix not in LANGUAGES or generated(rel) or not path.is_file():
            continue
        if rel not in indexed:
            missing.append(rel)
        text = path.read_bytes().decode('utf-8', 'replace')
        m = PACKAGE_LINE.search(text)
        package = m.group(1).replace('`', '').replace('.', '/') + '/' if m else ''
        names = set(TYPE_DECLARATION.findall(text)) | set(JVM_NAME.findall(text)) | {path.stem, path.stem + 'Kt'}
        out |= {package + name for name in names}
    if missing:
        print(f'label.py: {len(missing)} source files not in the index: ' + ' '.join(missing), file=sys.stderr)
    return out


def top_type(symbol):
    """`package/path/Name` of the outermost type a global symbol is in, or None."""
    parts = split_symbol(symbol)
    if not parts:
        return None
    path = ''
    for name, suffix in parts[1]:
        if suffix == '/':
            path += name + '/'
        elif suffix == '#':
            return path + name
        else:
            return None
    return None


def origin(symbol):
    """stdlib or package. scip-java names a Java symbol's artifact; its Kotlin symbols name none (`. .`), so theirs
    is read from the symbol's own package path."""
    package, descriptors = split_symbol(symbol)
    name = package.split(' ')[1]
    if name != '.':
        return 'stdlib' if name in STDLIB_PACKAGES else 'package'
    path = descriptors[0][0] if descriptors else ''
    return 'stdlib' if path in STDLIB_ROOTS else 'package'


def dispatched(s, up, group, lang):
    """A member called through a conformance: an abstract member, an interface's method, or a Kotlin interface's
    property. An annotation's elements are abstract methods but aren't dispatched, and a Java interface's field is a
    constant."""
    if group == 'type' or up and annotation(up):
        return False
    # scip-kotlinc doesn't give an abstract member the AbstractMethod kind; its signature says `abstract`.
    if s and (s['kind'] == ABSTRACT_METHOD or ABSTRACT.search(s['signature'].split('\n')[-1])):
        return True
    return bool(up) and up['kind'] == INTERFACE and (group == 'callable' or lang == 'kotlin')


def annotation(s):
    """An annotation type: an interface implementing java.lang.annotation.Annotation (or Kotlin's)."""
    return any(r['symbol'].endswith(('java/lang/annotation/Annotation#', 'kotlin/Annotation#'))
               for r in s['relationships'])


def lang_of_def(target):
    if not target or not target['locs']:
        return None
    return LANGUAGES.get(Path(sorted(target['locs'])[0][0]).suffix)


def extension(s, lang):
    """A Kotlin extension function or property: its signature names a receiver type before the name."""
    if lang != 'kotlin':
        return False
    sig = s['signature']
    m = re.search(r'\b(fun|val|var)\s+(<[^>]*>\s*)?([^\s(=:]+)\.' + re.escape(s['display_name']) + r'\b', sig)
    return bool(m)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--repo', required=True)
    ap.add_argument('--src', required=True)
    ap.add_argument('index')
    args = ap.parse_args()
    for row in enumerate_repo(args.repo, args.src, read_index(args.index)):
        sys.stdout.write(json.dumps(row, sort_keys=True) + '\n')


if __name__ == '__main__':
    main()
