#!/usr/bin/env python3
"""Cross-checks the Go labeller's targets against scip-go's (eval/identity-v2/README.md, Go).

    crosscheck.py ENUMERATION.jsonl.gz SCIPDIR [--labels labels.jsonl] [--list]

label.go is our own code, so a bug in it would become a gate. scip-go reads the same type checker through different
code, and run.sh writes its index of each module to SCIPDIR (N.scip, with modules.txt mapping N to the module's
directory). For every reference both report at the same place, the targets agree when:

- inside: scip-go's symbol is defined in the repository, at the file and line the labeller gives (scip-go calls some
  declarations local, such as a package's unexported test helpers; the place still has to match);
- outside: scip-go's symbol isn't defined in the repository, and names the same package and name.

Prints the agreement over every reference in the enumeration and, with --labels, over the drawn labels; --list prints
each disagreement.
"""
from __future__ import annotations

import argparse
import gzip
import json
import re
import sys
from collections import Counter
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from scip import DEFINITION, read_index  # noqa: E402


def load_scip(scipdir):
    """(occurrences by (file, line0, col0), definitions by symbol) over every module's index."""
    occ, defs = {}, {}
    for line in (Path(scipdir)/'modules.txt').read_text().splitlines():
        n, d = line.split(' ', 1)
        prefix = '' if d == '.' else d.rstrip('/') + '/'
        index = read_index(Path(scipdir)/f'{n}.scip')
        for doc in index['documents']:
            assert doc['position_encoding'] in (0, 1), doc['position_encoding']  # scip-go counts UTF-8 bytes
            path = prefix + doc['relative_path']
            for o in doc['occurrences']:
                start = (path, o['range'][0], o['range'][1])
                # A `local N` symbol is numbered per document, so it is qualified by the document's path.
                symbol = f"{path} {o['symbol']}" if o['symbol'].startswith('local ') else o['symbol']
                if o['symbol_roles'] & DEFINITION:
                    defs.setdefault(symbol, (path, o['range'][0] + 1))
                else:
                    occ.setdefault(start, set()).add(symbol)
    return occ, defs


DESCRIPTOR = re.compile(r'^scip-go \S+ \S+ \S+ (.*)$')


def package_and_name(symbol):
    """scip-go's package path and simple name: `io/Writer#Write().` is ('io', 'Write')."""
    m = DESCRIPTOR.match(symbol)
    if not m:
        return None, None
    desc = m.group(1)
    if desc.startswith('`'):
        end = desc.index('`', 1)
        pkg, rest = desc[1:end], desc[end + 2:]
    else:
        pkg, _, rest = desc.partition('/')
        # The package path is everything before the first descriptor: the last '/' before a name's suffix.
        parts = desc.split('/')
        for i in range(len(parts) - 1, 0, -1):
            if parts[i] and re.search(r'[#.()\[\]:]', parts[i]):
                pkg, rest = '/'.join(parts[:i]), '/'.join(parts[i:])
                break
    names = re.findall(r'([A-Za-z_][A-Za-z0-9_]*)(?:\(\)\.|#|\.|:)', rest)
    return pkg, names[-1] if names else None


def judge(row, symbols, defs):
    """'agree', or the kind of disagreement."""
    inside = [s for s in symbols if s in defs]
    if row['inside']:
        want = tuple(row['locs'][0])
        if any(defs[s] == want for s in inside):
            return 'agree'
        return 'scip-go: other inside target' if inside else 'scip-go: outside or local'
    if inside:
        return 'scip-go: inside target'
    for s in symbols:
        pkg, name = package_and_name(s)
        if pkg and name == row['name'] and row['symbol'].startswith(pkg + '.'):
            return 'agree'
    return 'scip-go: other outside target'


def read(path):
    opener = gzip.open if str(path).endswith('.gz') else open
    with opener(path, 'rt', encoding='utf-8') as f:
        return [json.loads(line) for line in f if line.strip()]


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('enumeration')
    ap.add_argument('scipdir')
    ap.add_argument('--labels')
    ap.add_argument('--list', action='store_true')
    args = ap.parse_args()
    occ, defs = load_scip(args.scipdir)
    refs = [r for r in read(args.enumeration) if r['t'] == 'ref']
    drawn = None
    if args.labels:
        drawn = {r['id'] for r in read(args.labels)}
    for scope, rows in [('enumeration', refs)] + ([('labels', [r for r in refs if f"{r['repo']}:{r['file']}:{r['line']}:{r['col']}" in drawn])] if drawn is not None else []):
        verdicts = Counter()
        for r in rows:
            symbols = occ.get((r['file'], r['line'] - 1, r['col'] - 1))
            v = judge(r, symbols, defs) if symbols else 'scip-go: no reference here'
            verdicts[v] += 1
            if args.list and v != 'agree':
                print(f"  {v}: {r['repo']}:{r['file']}:{r['line']}:{r['col']} {r['name']} -> {r['symbol']} "
                      f"{r['locs'] or r['origin']} | scip-go {sorted(symbols or [])}")
        both = sum(n for v, n in verdicts.items() if v != 'scip-go: no reference here')
        print(f"{scope}: {len(rows)} references, {both} also reported by scip-go, {verdicts['agree']} agree "
              f"({100 * verdicts['agree'] / both:.2f}%); " + ', '.join(f'{v} {n}' for v, n in sorted(verdicts.items())
                                                                         if v != 'agree'))


if __name__ == '__main__':
    main()
