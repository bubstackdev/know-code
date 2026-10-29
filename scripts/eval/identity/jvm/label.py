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
from pathlib import Path
import re
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from scip import DEFINITION, byte_column, read_index  # noqa: E402

LANGUAGES = {'.java': 'java', '.kt': 'kotlin'}
# Build output and generated sources: annotation processors' and KSP's output, Kotlin's and Gradle's build dirs.
GENERATED = ('build', 'target', 'generated', 'out', '.gradle')
ABSTRACT = re.compile(r'\babstract\b')
IMPORT_LINE = re.compile(r'\s*import\s')
IDENT = re.compile(r'[A-Za-z_$][A-Za-z0-9_$]*')
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
                    e['locs'].add((rel, o['range'][0] + 1))
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
    dropped = 0

    def synthesised(symbol):
        """A member the compiler makes for one of the repository's types (an enum's `values()`, a record's accessor,
        a data class's `copy`), which has no definition in source: not outside, and not labelled."""
        up = enclosing(symbol)
        return up in defs

    seen = set()
    for rel, lang, d in docs:
        lines = lines_of(rel)
        for o in d['occurrences']:
            symbol, rng = o['symbol'], o['range']
            if o['symbol_roles'] & DEFINITION or symbol.startswith('local ') or not rng or rng[0] != rng[2]:
                continue
            got = name_and_group(symbol)
            if not got:
                continue
            name, group = got
            line = rng[0] + 1
            if line > len(lines):
                continue
            text = lines[line - 1]
            # scip-java doesn't mark import occurrences with the Import role; an import is one line in both languages.
            if IMPORT_LINE.match(text):
                continue
            # The token must be the declaration's own name: an implicit reference has none, and a renamed one (a
            # Kotlin property read through a Java getter, an import alias) has another.
            col = byte_column(text, rng[1], d['position_encoding'])
            token = IDENT.match(text.encode('utf-8')[col - 1:].decode('utf-8', 'replace'))
            if not token or token.group(0) != name:
                continue
            if (rel, line, col, symbol) in seen:
                continue
            seen.add((rel, line, col, symbol))
            target = defs.get(symbol)
            if target and target['generated']:
                continue
            if not target and synthesised(symbol):
                continue
            # Outside, in a type the repository declares: a file the build didn't compile (a source set for another
            # platform, a module left out), or a published build of the repository's own code that a module depends
            # on. Neither says where a name binds in this source, so neither is labelled.
            if not target and top_type(symbol) in own_types:
                dropped += 1
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
            out.append(row)
    if dropped:
        print(f'label.py: {dropped} outside references into types the repository declares, left out', file=sys.stderr)
    return out


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
