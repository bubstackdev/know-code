#!/usr/bin/env python3
"""Swift references and declarations from an index store dump (indexdump.c), as identity-v1 enumerates them.

    label.py --repo NAME --src DIR DUMP.tsv | gzip > enumeration-NAME.jsonl.gz

Runs in the labelling container. Every record whose source file is the repository's own (under DIR, not under
.build) gives references; every record of a non-system unit gives definitions, so a package's declarations are known
as a package's. The rule is eval/identity-v1/README.md's.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import sys

# indexstore.h: symbol roles and kinds.
DECLARATION, DEFINITION, REFERENCE, CALL, IMPLICIT = 1 << 0, 1 << 1, 1 << 2, 1 << 5, 1 << 8
REL_CHILD_OF = 1 << 9
ENUM, STRUCT, CLASS, PROTOCOL, EXTENSION, TYPEALIAS = 5, 6, 7, 8, 9, 11
CALLABLE = {12, 16, 17, 18}  # function, instance/class/static method
MEMBER = {13, 14, 15, 19, 20, 21}  # variable, field, enum case, instance/class/static property
TYPE = {ENUM, STRUCT, CLASS, PROTOCOL, TYPEALIAS}
IDENT = re.compile(r'[A-Za-z_][A-Za-z0-9_]*')
# Modules of the toolchain's standard and core libraries; any other module the toolchain ships is the SDK's.
STDLIB = {'Swift', '_Concurrency', '_StringProcessing', 'Foundation', 'FoundationEssentials',
          'FoundationInternationalization', 'Dispatch', 'Synchronization', 'XCTest', 'Testing', 'RegexBuilder',
          'Observation', 'Distributed'}


def base(name):
    """`parse(_:from:)` → `parse`; None for an operator, a subscript or an accessor."""
    m = IDENT.match(name)
    return m.group(0) if m and (len(name) == m.end() or name[m.end()] == '(') and m.group(0) != 'subscript' else None


def module_of(usr):
    """The module a Swift USR names: `s:S…`/`s:s…` is the standard library, `s:10Foundation…` Foundation."""
    if usr.startswith(('s:S', 's:s')):
        return 'Swift'
    m = re.match(r's:(\d+)', usr)
    if m:
        n = int(m.group(1))
        start = m.end()
        return usr[start:start + n]
    return None


def parse(lines):
    """Records as (path, [occurrence]); an occurrence is a dict with its relations."""
    records, current, occ = [], None, None
    for line in lines:
        parts = line.rstrip('\n').split('\t')
        if parts[0] == 'R':
            current = (parts[2], [])
            records.append(current)
        elif parts[0] == 'O':
            usr, name, kind, subkind, roles, ln, col = parts[1:8]
            occ = dict(usr=usr, name=name, kind=int(kind), subkind=int(subkind), roles=int(roles), line=int(ln),
                       col=int(col), rels=[])
            current[1].append(occ)
        elif parts[0] == 'L':
            roles, usr, name, kind, subkind = parts[1:6]
            occ['rels'].append(dict(roles=int(roles), usr=usr, name=name, kind=int(kind), subkind=int(subkind)))
    return records


def enumerate_repo(repo, src, records):
    src = Path(src).resolve()

    def place(path):
        p = Path(path)
        try:
            rel = p.resolve().relative_to(src)
        except ValueError:
            return None, 'outside'
        if rel.parts and rel.parts[0] == '.build':
            return rel.as_posix(), 'package' if 'checkouts' in rel.parts else 'build'
        return rel.as_posix(), 'inside'

    defs = {}  # usr -> {'locs': set((file, line)), 'where': inside|package, 'parent': kind, 'name': base}
    refs = []
    for path, occs in records:
        rel, where = place(path)
        if where in ('build', 'outside'):
            continue
        for o in occs:
            if o['roles'] & (DEFINITION | DECLARATION):
                parents = [r for r in o['rels'] if r['roles'] & REL_CHILD_OF]
                d = defs.setdefault(o['usr'], dict(locs=set(), where=where, parent=None, name=o['name'],
                                                   kind=o['kind'], subkind=o['subkind']))
                if where == 'inside':
                    d['where'] = 'inside'
                    d['locs'].add((rel, o['line']))
                if parents:
                    d['parent'] = parents[0]['kind']
            elif where == 'inside' and o['roles'] & REFERENCE and not o['roles'] & IMPLICIT:
                refs.append((rel, o))

    texts = {}

    def token_at(rel, line, col):
        if rel not in texts:
            texts[rel] = (src/rel).read_bytes().split(b'\n')
        lines = texts[rel]
        if line < 1 or line > len(lines):
            return None
        m = IDENT.match(lines[line - 1][col - 1:].decode('utf-8', 'replace'))
        return m.group(0) if m else None

    out = []
    for usr, d in sorted(defs.items()):
        name = base(d['name'])
        if d['where'] != 'inside' or not name or d['subkind'] != 0:
            continue
        group = 'type' if d['kind'] in TYPE else 'callable' if d['kind'] in CALLABLE else \
            'member' if d['kind'] in MEMBER else None
        if group:
            for f, ln in sorted(d['locs']):
                out.append(dict(t='decl', repo=repo, name=name, key=usr, file=f, line=ln, decl=group))
                break
    seen = set()
    for rel, o in refs:
        name = base(o['name'])
        if not name or o['subkind'] != 0 or not (o['kind'] in CALLABLE | MEMBER | TYPE):
            continue
        if (rel, o['line'], o['col'], o['usr']) in seen or token_at(rel, o['line'], o['col']) != name:
            continue
        seen.add((rel, o['line'], o['col'], o['usr']))
        d = defs.get(o['usr'])
        parent = d and d['parent']
        if parent == EXTENSION:
            kind = 'extension'
        elif parent == PROTOCOL:
            kind = 'dispatch'
        elif o['kind'] in CALLABLE:
            kind = 'call'
        elif o['kind'] in MEMBER:
            kind = 'member'
        else:
            kind = 'type'
        row = dict(t='ref', repo=repo, file=rel, line=o['line'], col=o['col'], name=name, kind=kind, key=o['usr'],
                   symbol=o['name'], called=bool(o['roles'] & CALL))
        if d and d['where'] == 'inside':
            row.update(inside=True, locs=sorted([list(l) for l in d['locs']]), origin=None)
        elif d:
            row.update(inside=False, locs=[], origin='package')
        else:
            row.update(inside=False, locs=[], origin='stdlib' if module_of(o['usr']) in STDLIB else 'sdk')
        out.append(row)
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--repo', required=True)
    ap.add_argument('--src', required=True)
    ap.add_argument('dump')
    args = ap.parse_args()
    with open(args.dump, encoding='utf-8', errors='replace') as f:
        records = parse(f)
    for row in enumerate_repo(args.repo, args.src, records):
        sys.stdout.write(json.dumps(row, sort_keys=True) + '\n')


if __name__ == '__main__':
    main()
