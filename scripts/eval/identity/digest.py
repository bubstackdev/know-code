#!/usr/bin/env python3
"""The digest an identity set's enumerations.sha256 records for each enumeration it was drawn from.

    digest.py ENUMERATION.jsonl.gz ...

prints `sha256  name` per file, in sha256sum's format. The digest covers the enumeration's rows, not its bytes: each row
is re-serialised with its keys sorted, and the lines are sorted, before hashing. Two labelling runs of one pinned
repository give the same rows, but the toolchain may write its documents in a different order (scip-java does), so
the rows come out in a different order, and gzip's bytes differ from one compressor to another; neither changes what
the labels are drawn from.
"""
import gzip
import hashlib
import json
import pathlib
import sys


def digest(path):
    with gzip.open(path, 'rt', encoding='utf-8') as f:
        rows = [json.dumps(json.loads(line), sort_keys=True, separators=(',', ':'), ensure_ascii=False)
                for line in f if line.strip()]
    h = hashlib.sha256()
    for row in sorted(r.encode('utf-8') for r in rows):
        h.update(row + b'\n')
    return h.hexdigest()


def main(paths):
    if not paths:
        sys.exit(__doc__)
    for p in paths:
        print(f'{digest(p)}  {pathlib.Path(p).name}')


if __name__ == '__main__':
    main(sys.argv[1:])
