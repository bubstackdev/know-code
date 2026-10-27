"""`labels`: precision and recall against hand-labelled answers, for features git history can't check.

A labels file (TOML) holds queries with the items a correct answer contains, labelled from the source (grep, reading
code, the language's rules) before looking at knowcode's answer:

    root = "~/Library/Caches/dev.bubstack.knowcode/corpus/public"   # repos are directories under it
    match = "file_line"            # default matching rule (below)

    [[label]]
    id = "flask-refs-url_for"
    repo = "flask"                 # or repos = [...]
    argv = ["refs", "url_for", "-n", "1000"]
    items = "items[]"              # JSON path to the answer's rows
    item = "{file}:{line}"         # how a row reads as an item (fields of the row; `a.b` for nested fields)
    expected = ["src/flask/helpers.py:220", "src/flask/app.py:1610"]
    match = "line_within:2"        # optional per-label rule
    filter = "^src/"               # optional: only answer items matching this regex are scored
    complete = true                # expected lists every right item (default); false: a known subset, so only
                                   # recall is scored
    notes = "how the labels were made"

Matching rules: `exact` (strings equal), `ci` (equal ignoring case), `file` (the part before the first `:` equal),
`file_line` (`file:line` equal, anything after a second `:` ignored), `line_within:N` (same file, lines at most N apart),
`suffix` (the answer item ends with the expected one, for qualified names), `regex` (the expected item is a regex the
answer item must match in full).

Every answer item matched to no expected item is a false positive and every expected item matched by none is a false
negative; both are listed, so each can be explained.
"""

import json
import re
import sys
from pathlib import Path

from common import expand, json_path, kc_json, load_toml, pct, repo_dir, wilson


def field(row, name):
    v = row
    for part in name.split("."):
        v = v.get(part) if isinstance(v, dict) else None
    return "" if v is None else str(v)


def render(row, template):
    return re.sub(r"\{([A-Za-z0-9_.]+)\}", lambda m: field(row, m.group(1)), template)


def split_fl(s):
    parts = s.split(":")
    line = int(parts[1]) if len(parts) > 1 and parts[1].isdigit() else None
    return parts[0], line


def matcher(rule):
    if rule == "exact":
        return lambda got, exp: got == exp
    if rule == "ci":
        return lambda got, exp: got.lower() == exp.lower()
    if rule == "file":
        return lambda got, exp: split_fl(got)[0] == split_fl(exp)[0]
    if rule == "file_line":
        return lambda got, exp: split_fl(got) == split_fl(exp)
    if rule.startswith("line_within:"):
        n = int(rule.split(":", 1)[1])

        def within(got, exp):
            (gf, gl), (ef, el) = split_fl(got), split_fl(exp)
            return gf == ef and gl is not None and el is not None and abs(gl - el) <= n

        return within
    if rule == "suffix":
        return lambda got, exp: got == exp or got.endswith(exp) and not got[-len(exp) - 1].isalnum()
    if rule == "regex":
        return lambda got, exp: re.fullmatch(exp, got) is not None
    raise SystemExit(f"unknown match rule {rule!r}")


def score(got, expected, rule):
    """Greedy one-to-one matching of answer items to expected items. Returns (true positives, false positives, false
    negatives) as lists of items."""
    m = matcher(rule)
    left = list(expected)
    tp, fp = [], []
    for g in got:
        hit = next((e for e in left if m(g, e)), None)
        if hit is None:
            fp.append(g)
        else:
            left.remove(hit)
            tp.append(g)
    return tp, fp, left


def labels(a):
    spec = load_toml(a.file)
    root = expand(spec.get("root", "."))
    default_rule = spec.get("match", "exact")
    out_rows = []
    totals = {"tp": 0, "fp": 0, "fn": 0}
    for lab in spec["label"]:
        if a.only and a.only not in lab["id"]:
            continue
        repos = lab.get("repos") or [lab["repo"]]
        argv = [*lab["argv"], "--json", "--no-sync"]
        for r in repos:
            argv += ["--repo", str(repo_dir(root, r))]
        ans = kc_json(a.bin, argv, a.cache)
        rows = json_path(ans, lab.get("items", "items[]"))
        got = []
        for row in rows:
            s = render(row, lab["item"]) if isinstance(row, dict) else str(row)
            if s not in got and re.search(lab.get("filter", ""), s):
                got.append(s)
        tp, fp, fn = score(got, lab["expected"], lab.get("match", default_rule))
        complete = lab.get("complete", True)
        if not complete:
            fp = []
        else:
            totals["tp_p"] = totals.get("tp_p", 0) + len(tp)
        totals["tp"] += len(tp)
        totals["fp"] += len(fp)
        totals["fn"] += len(fn)
        p = len(tp) / (len(tp) + len(fp)) if complete and (tp or fp) else None
        rc = len(tp) / (len(tp) + len(fn)) if tp or fn else None
        out_rows.append({"id": lab["id"], "precision": p, "recall": rc, "tp": len(tp), "fp": fp, "fn": fn, "error": ans.get("error")})
    print("| Label | Precision | Recall | TP | FP | FN |")
    print("|---|---:|---:|---:|---:|---:|")
    for r in out_rows:
        p = "–" if r["precision"] is None else pct(r["precision"])
        rc = "–" if r["recall"] is None else pct(r["recall"])
        print(f"| {r['id']} | {p} | {rc} | {r['tp']} | {len(r['fp'])} | {len(r['fn'])} |")
    tp, fp, fn = totals["tp"], totals["fp"], totals["fn"]
    tpp = totals.get("tp_p", 0)  # true positives in labels that list every right item
    plo, phi = wilson(tpp, tpp + fp)
    rlo, rhi = wilson(tp, tp + fn)
    print(f"\nPrecision {tpp}/{tpp + fp} = {pct(tpp / (tpp + fp)) if tpp + fp else '–'} [{pct(plo)}, {pct(phi)}]; "
          f"recall {tp}/{tp + fn} = {pct(tp / (tp + fn)) if tp + fn else '–'} [{pct(rlo)}, {pct(rhi)}] "
          f"over {len(out_rows)} labelled queries (95% Wilson).\n")
    for r in out_rows:
        if r["error"]:
            print(f"{r['id']}: error: {r['error']}")
        for x in r["fp"]:
            print(f"{r['id']}: false positive {x}")
        for x in r["fn"]:
            print(f"{r['id']}: false negative {x}")
    if a.out:
        Path(a.out).write_text(json.dumps({"totals": totals, "labels": out_rows}, indent=1) + "\n")
    sys.exit(1 if a.strict and (totals["fp"] or totals["fn"]) else 0)


def add_parsers(sub):
    p = sub.add_parser("labels", help="score answers against a hand-labelled file")
    p.add_argument("file", help="labels file (TOML)")
    p.add_argument("--bin", required=True, type=Path)
    p.add_argument("--cache", required=True, help="KNOWCODE_CACHE_DIR holding the repos' indexes")
    p.add_argument("--only", help="only labels whose id contains this")
    p.add_argument("--out", help="write per-label results as JSON here")
    p.add_argument("--strict", action="store_true", help="exit 1 on any false positive or negative")
    p.set_defaults(func=labels)
