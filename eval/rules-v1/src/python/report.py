from billing import Invoice


def build(rows):
    inv = Invoice([])
    for r in rows:
        print(r)
        inv.lines.append(r)
    # ast-grep-ignore: py-no-print
    print("built")
    return inv


def is_empty(rows):
    return len(rows) == 0
