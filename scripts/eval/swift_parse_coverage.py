#!/usr/bin/env python3
"""Ranks the constructs behind Swift parse errors (eval/swift-parse-coverage.md).

    cargo run --release -p kc-extract --example parse_errors -- DIR [DIR ...] > errors.jsonl   (from the directory
    holding the clones)
    swift_parse_coverage.py errors.jsonl --clones DIR

Each file with parse errors has its first error sites (the example prints up to 20). A site is put down to the first
construct in RULES its surrounding lines (three before and after) show, else to the first construct in FILE_RULES the
file shows (an error at a file's first line can come from far down it), else to `other`. A file's errors are split
over its sites' constructs. Per construct: the files it is in, and the errors put down to it.
"""
from __future__ import annotations

import argparse
from collections import Counter, defaultdict
import json
from pathlib import Path
import re

RULES = [
    ('documentation snippets (`.docc` tutorial code)', lambda f, ctx: '.docc/' in f),
    ('`@_specialize(where …)` attributes', lambda f, ctx: re.search(r'@_specialize\(', ctx)),
    ('`borrow`, `mutate`, `unsafeAddress` accessors, `borrowing get`',
     lambda f, ctx: re.search(r'^\s*(borrow|mutate|unsafeAddress|unsafeMutableAddress|borrowing get)\b|\{\s*borrow\s+mutate\s*\}', ctx, re.M)),
    ('`unsafe` before `!`, a number or a string', lambda f, ctx: re.search(r'\bunsafe\s+[!0-9"$]', ctx)),
    ('identifiers with `$` inside (`_$id`)', lambda f, ctx: re.search(r'\w\$\w', ctx)),
    ('`where` on a generic typealias', lambda f, ctx: re.search(r'typealias\s+\w+<[^=]*=.*\bwhere\b', ctx)),
    ('`#error` and `#warning`', lambda f, ctx: re.search(r'#(error|warning)\(', ctx)),
    ('regex literals', lambda f, ctx: re.search(r'(=|^)\s*/\^', ctx, re.M)),
    ('`nonisolated(nonsending)`', lambda f, ctx: 'nonisolated(nonsending)' in ctx),
    ('partial ranges in `case` patterns (`x...:`)', lambda f, ctx: re.search(r'case .*\.\.\.(:|\s*\w+:)|case \.min\.\.<', ctx)),
    ('macros attached to members (`@Controller`, `@Suite`)', lambda f, ctx: re.search(r'^\s*@(Controller|Suite)\b', ctx, re.M)),
    ('non-ASCII identifiers (`y²`)', lambda f, ctx: re.search(r'[^\x00-\x7f]', ctx)),
    ('`consume` before an operand', lambda f, ctx: re.search(r'\bconsume\s+\w', ctx)),
    ('`@unsafe` in a conformance list', lambda f, ctx: ': @unsafe' in ctx),
    ('closures passed as `get:` and `set:`', lambda f, ctx: re.search(r'^\s*(get|set):\s*\{', ctx, re.M)),
]
FILE_RULES = [
    ('`@Sendable func` in a body', lambda text: re.search(r'^\s+@Sendable func ', text, re.M)),
    ('`()` as a statement', lambda text: re.search(r'^\s*\(\)\s*$', text, re.M)),
    ('`consume` before an operand', lambda text: re.search(r'\bswitch consume ', text)),
]


def rank(rows, clones):
    files, errors = defaultdict(set), Counter()
    for r in rows:
        if not r['errors']:
            continue
        text = (Path(clones)/r['root']/r['file']).read_text(errors='replace')
        lines = text.split('\n')
        sites = Counter()
        for line, _col, _text in r['sites']:
            ctx = '\n'.join(lines[max(0, line - 3):line + 3])
            sites[next((n for n, f in RULES if f(r['file'], ctx)), None) or
                  next((n for n, f in FILE_RULES if f(text)), 'other')] += 1
        total = sum(sites.values())
        for construct, n in sites.items():
            files[construct].add(r['root'] + r['file'])
            errors[construct] += r['errors'] * n / total
    return sorted(((c, len(files[c]), round(errors[c], 1)) for c in files), key=lambda x: (-x[2], -x[1]))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('errors')
    ap.add_argument('--clones', required=True)
    args = ap.parse_args()
    rows = [json.loads(l) for l in Path(args.errors).read_text().splitlines() if l.strip()]
    bad = [r for r in rows if r['errors']]
    print(f"{len(rows)} files, {len(bad)} with parse errors, {sum(r['errors'] for r in rows)} errors")
    print('| Construct | Files | Errors |\n| --- | ---: | ---: |')
    for construct, n, e in rank(rows, args.clones):
        print(f'| {construct} | {n} | {e} |')


if __name__ == '__main__':
    main()
