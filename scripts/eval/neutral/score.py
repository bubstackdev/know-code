#!/usr/bin/env python3
"""Scores one tool's answers to the neutral benchmark against its labels.

    score.py --labels labels.jsonl --answers answers.jsonl [--questions questions.jsonl] [--census census.json]
             [--json out.json]

An answers file holds one JSON object per question:

    {"id": "C-cs-callers-003", "status": "answered", "items": ["repo/src/A.cs:42", "repo/src/B.cs"]}

`status` is "answered", "unsupported" (the tool has no way to answer this kind of question) or "error" (it tried
and failed). Unsupported and errors are counted apart from wrong answers, never as zero recall. A question with no
answer row is "missing".

Matching follows the label's `match`: `file` compares paths and ignores lines, `file_line` needs the same line,
`line_within:N` a line within N, `symbol` exact text and `ci` case-insensitive text; an HTTP endpoint (`VERB /path`)
is compared with leading and trailing slashes dropped, case ignored, and `{param}` and `:param` taken as the same.
Each label item is matched by at most one answer item. When the question names one repo, answer paths without a
repo prefix get it.

Labels may mark items `optional` (a hit counts, a miss costs nothing) and `via` (a call reached through dispatch;
reported with and without them), list `negatives` (same-named different targets: always a false positive), and set
`dynamic_name` (a runtime-built name; reported with and without). Answer items matching a label's `tests` are
neither hits nor false positives.

Precision counts only for `exhaustive` labels (plus negatives, on any label). `lower_bound` and `uncertain` labels
score recall only, and `uncertain` ones are reported apart. With --census, the structural headline is the mean of
per-stratum means weighted by the census stratum weights, so a stratum's floor of questions doesn't buy it weight.
"""

import argparse
import json
import re
import sys
from collections import defaultdict
from pathlib import Path


def read_jsonl(p):
    return [json.loads(x) for x in Path(p).read_text().splitlines() if x.strip()]


LINE = re.compile(r"^(.*?):(\d+)(?::\d+)?$")
VERB = re.compile(r"^(GET|POST|PUT|DELETE|PATCH|HEAD|OPTIONS)\s+(\S*)$", re.I)


def split_item(s):
    """`repo/path:line` -> (path, line or None); anything else -> (text, None)."""
    s = s.strip().replace("\\", "/")
    s = s[2:] if s.startswith("./") else s
    m = LINE.match(s)
    if m and not VERB.match(s):
        return m.group(1), int(m.group(2))
    return s, None


def route_key(s):
    """`GET /api/Orders/{id}/` and `get api/orders/:id` -> the same key; None when `s` is not an endpoint."""
    m = VERB.match(s.strip())
    if not m:
        return None
    path = re.sub(r"\{[^}]*\}|:[A-Za-z_]\w*", "{}", m.group(2).split("?", 1)[0]).strip("/").lower()
    return f"{m.group(1).upper()} {path}"


def forms(item, repos=(), paths=True):
    """The ways an answer item may be read, as (path or text, line) pairs: as written, and, for file match rules on
    a one-repo question, also with that repo prefixed (a tool may give repo-relative paths)."""
    path, line = split_item(item)
    out = [(path, line)]
    if paths and len(repos) == 1 and not path.startswith(repos[0] + "/") and not VERB.match(path):
        out.append((f"{repos[0]}/{path}", line))
    return out


def matches(answer, truth, rule):
    (ap, al), (tp, tl) = answer, truth
    ra, rt = route_key(ap), route_key(tp)
    if ra or rt:
        return ra == rt
    if rule == "ci":
        return ap.lower() == tp.lower() and (tl is None or al == tl)
    if rule == "symbol":
        return ap == tp and (tl is None or al == tl)
    if ap != tp:
        return False
    if rule == "file":
        return True
    if al is None or tl is None:
        return False
    if rule == "file_line":
        return al == tl
    if rule.startswith("line_within:"):
        return abs(al - tl) <= int(rule.split(":", 1)[1])
    raise ValueError(f"unknown match rule {rule}")


def score_one(label, answer_items, repos=(), skip_via=False):
    """Hits, misses and false positives of one answer. Returns tp, fn (required items missed), fp and precision
    (None when the label is not exhaustive and no negative was returned), recall over required items, and the
    missed, extra and negative items. `skip_via` leaves items tagged `via` out of the truth (an answer naming one is
    then neither hit nor false positive).

    An empty exhaustive label is a real answer: returning nothing scores recall 1 and precision 1; returning
    anything scores precision 0."""
    rule = label["match"]
    labelled = [i for i in label["items"] if not (skip_via and i.get("via"))]
    ignored = [i for i in label["items"] if skip_via and i.get("via")] + label.get("tests", [])
    paths = rule not in {"ci", "symbol"}
    truth = [split_item(i["item"]) for i in labelled]
    required = [not i.get("optional") for i in labelled]
    neutral = [split_item(i["item"]) for i in ignored]
    negatives = [split_item(i["item"]) for i in label.get("negatives", [])]
    used = [False] * len(truth)
    extra, wrong, seen = [], [], set()

    def hits(a, t):
        return any(matches(f, t, rule) for f in a)

    for raw in answer_items:
        a = forms(raw, repos, paths)
        if a[0] in seen:
            continue
        seen.add(a[0])
        if any(hits(a, t) for t in negatives):
            wrong.append(raw)
            continue
        hit = next((k for k, t in enumerate(truth) if not used[k] and hits(a, t)), None)
        if hit is not None:
            used[hit] = True
        elif not any(hits(a, t) for t in neutral + truth):
            extra.append(raw)
    tp = sum(u for u in used)
    tp_req = sum(u for u, r in zip(used, required) if r)
    n_req = sum(required)
    fn = n_req - tp_req
    exhaustive = label["completeness"] == "exhaustive"
    fp = (len(extra) if exhaustive else 0) + len(wrong)
    counted_fp = exhaustive or bool(wrong)
    if counted_fp:
        precision = tp / (tp + fp) if tp + fp else 1.0
    else:
        fp, precision = None, None
    if n_req:
        recall = tp_req / n_req
    else:  # nothing required: right unless the tool named something where the label says there is nothing
        recall = 0.0 if wrong or (exhaustive and extra) else 1.0
    return {"tp": tp, "fn": fn, "fp": fp, "precision": precision, "recall": recall,
            "missed": [labelled[k]["item"] for k, u in enumerate(used) if not u and required[k]],
            "extra": extra, "negatives_returned": wrong}


def summarize(rows):
    ans = [r for r in rows if r["status"] == "answered"]
    s = {"questions": len(rows), "answered": len(ans),
         "unsupported": sum(r["status"] == "unsupported" for r in rows),
         "error": sum(r["status"] == "error" for r in rows), "missing": sum(r["status"] == "missing" for r in rows),
         "tp": sum(r["tp"] for r in ans), "fn": sum(r["fn"] for r in ans)}
    s["recall_pooled"] = s["tp"] / (s["tp"] + s["fn"]) if s["tp"] + s["fn"] else None
    s["recall_mean"] = sum(r["recall"] for r in ans) / len(ans) if ans else None
    prec = [r for r in ans if r["precision"] is not None]
    s["fp"] = sum(r["fp"] for r in prec)
    tpp = sum(r["tp"] for r in prec)
    s["precision_pooled"] = tpp / (tpp + s["fp"]) if tpp + s["fp"] else None
    s["precision_mean"] = sum(r["precision"] for r in prec) / len(prec) if prec else None
    s["precision_n"] = len(prec)
    return s


def score(labels, answers, questions=None, weights=None):
    """Scores every labelled question. Returns per-question rows and summaries: by completeness, by stratum (with
    its sample size), the views without `via` items and without `dynamic_name` labels, and, given stratum weights,
    the weighted structural headline."""
    qmeta = {q["id"]: q for q in questions or []}
    by_id = {a["id"]: a for a in answers}
    per = {}
    rows_novia = []
    for lab in labels:
        if lab.get("excluded"):
            continue
        q = qmeta.get(lab["id"], {})
        a = by_id.get(lab["id"])
        status = a.get("status", "answered") if a else "missing"
        base = {"status": status, "completeness": lab["completeness"], "stratum": q.get("stratum"),
                "subset": (q.get("meta") or {}).get("subset", q.get("source", "")),
                "dynamic_name": bool(lab.get("dynamic_name")),
                "tool_own_repo": bool((q.get("meta") or {}).get("tool_own_repo"))}
        row, row_nv = dict(base), dict(base)
        if status == "answered":
            row.update(score_one(lab, a.get("items", []), q.get("repos", ())))
            row_nv.update(score_one(lab, a.get("items", []), q.get("repos", ()), skip_via=True))
        per[lab["id"]] = row
        rows_novia.append(row_nv)
    rows = list(per.values())
    headline = [r for r in rows if r["completeness"] != "uncertain"]
    summary = {
        "by_completeness": {c: summarize([r for r in rows if r["completeness"] == c])
                            for c in sorted({r["completeness"] for r in rows})},
        "by_stratum": {s: summarize([r for r in headline if r["stratum"] == s])
                       for s in sorted({r["stratum"] for r in rows if r["stratum"]})},
        "by_subset": {s: summarize([r for r in headline if r["subset"] == s])
                      for s in sorted({r["subset"] for r in rows})},
        "all_certain": summarize(headline),
        "without_dispatch_items": summarize([r for r in rows_novia if r["completeness"] != "uncertain"]),
        "without_dynamic_names": summarize([r for r in headline if not r["dynamic_name"]]),
        "without_tool_own_repos": summarize([r for r in headline if not r["tool_own_repo"]]),
    }
    if weights:
        # the active-code draw only: the uniform and hard draws and history are reported on their own
        means = {s: summarize([r for r in headline if r["stratum"] == s and r["subset"] == "active"])
                 for s in weights}
        live = {s: m for s, m in means.items() if m["recall_mean"] is not None}
        tw = sum(weights[s] for s in live)
        summary["weighted_structural"] = {
            "recall": sum(weights[s] * live[s]["recall_mean"] for s in live) / tw if tw else None,
            "strata": sorted(live), "weight_covered": tw}
    return {"per_question": per, "summary": summary}


def cohen_kappa(pairs):
    """Cohen's kappa for two raters' categorical labels, given as (a, b) pairs; None when undefined."""
    n = len(pairs)
    if not n:
        return None
    cats = {x for p in pairs for x in p}
    po = sum(a == b for a, b in pairs) / n
    pe = sum((sum(a == c for a, _ in pairs) / n) * (sum(b == c for _, b in pairs) / n) for c in cats)
    return None if pe == 1 else (po - pe) / (1 - pe)


def agreement(labels_a, labels_b, questions=None):
    """Agreement between two labellers on the questions both labelled. Items are matched with labeller A's match
    rule (all items counted, optional or not); per question F1 = 2 * matched / (|A| + |B|), 1 when both are empty.
    Also Cohen's kappa on completeness and on empty versus non-empty answers."""
    qrepos = {q["id"]: q.get("repos", ()) for q in questions or []}
    b_by = {x["id"]: x for x in labels_b}
    rows = []
    for a in labels_a:
        b = b_by.get(a["id"])
        if not b:
            continue
        truth = dict(a, completeness="exhaustive", items=[dict(i, optional=False) for i in a["items"]],
                     negatives=[])
        r = score_one(truth, [i["item"] for i in b["items"]], qrepos.get(a["id"], ()))
        na, nb = len(a["items"]), len(b["items"])
        f1 = 1.0 if na + nb == 0 else 2 * r["tp"] / (na + nb)
        rows.append({"id": a["id"], "a": na, "b": nb, "matched": r["tp"], "f1": f1,
                     "completeness": (a["completeness"], b["completeness"]),
                     "only_a": r["missed"], "only_b": r["extra"]})
    tot_a, tot_b, tot_m = (sum(r[k] for r in rows) for k in ("a", "b", "matched"))
    return {"questions": len(rows),
            "f1_mean": sum(r["f1"] for r in rows) / len(rows) if rows else None,
            "f1_pooled": 2 * tot_m / (tot_a + tot_b) if tot_a + tot_b else None,
            "kappa_completeness": cohen_kappa([r["completeness"] for r in rows]),
            "kappa_empty": cohen_kappa([(r["a"] == 0, r["b"] == 0) for r in rows]),
            "per_question": rows}


def fmt(x):
    return "-" if x is None else f"{x:.3f}"


def line(name, s):
    out = (f"{name:28} n={s['questions']:3} answered {s['answered']}, unsupported {s['unsupported']}, errors "
           f"{s['error']}, missing {s['missing']}; recall {fmt(s['recall_pooled'])} pooled ({s['tp']}/"
           f"{s['tp'] + s['fn']}), {fmt(s['recall_mean'])} mean")
    if s["precision_n"]:
        out += (f"; precision {fmt(s['precision_pooled'])} pooled, {fmt(s['precision_mean'])} mean "
                f"(n={s['precision_n']})")
    return out


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--labels", required=True)
    p.add_argument("--answers", help="the tool's answers (or, with --agree, a second labeller's labels)")
    p.add_argument("--agree", action="store_true",
                   help="compare two labellers: --labels is labeller A, --answers labeller B")
    p.add_argument("--questions")
    p.add_argument("--census", help="census.json from sample.py, for the weighted structural headline")
    p.add_argument("--json", help="write the full result here")
    a = p.parse_args(argv)
    if a.agree:
        res = agreement(read_jsonl(a.labels), read_jsonl(a.answers), read_jsonl(a.questions) if a.questions else None)
        print(f"{res['questions']} double-labelled questions; item F1 {fmt(res['f1_mean'])} mean, "
              f"{fmt(res['f1_pooled'])} pooled; kappa completeness {fmt(res['kappa_completeness'])}, "
              f"empty-vs-not {fmt(res['kappa_empty'])}")
        for r in res["per_question"]:
            if r["f1"] < 1:
                print(f"  {r['id']}: F1 {r['f1']:.2f} (A {r['a']}, B {r['b']}, matched {r['matched']}); "
                      f"only A {r['only_a'][:4]}, only B {r['only_b'][:4]}")
        if a.json:
            Path(a.json).write_text(json.dumps(res, indent=1, sort_keys=True))
        return 0
    weights = None
    if a.census:
        weights = {k: v for k, v in json.loads(Path(a.census).read_text())["weights"].items()}
    res = score(read_jsonl(a.labels), read_jsonl(a.answers), read_jsonl(a.questions) if a.questions else None, weights)
    s = res["summary"]
    for c, v in s["by_completeness"].items():
        print(line(c, v))
    print(line("all (excl. uncertain)", s["all_certain"]))
    print(line("without dispatch items", s["without_dispatch_items"]))
    print(line("without dynamic names", s["without_dynamic_names"]))
    print(line("without tools' own repos", s["without_tool_own_repos"]))
    for k, v in s["by_subset"].items():
        print(line(f"subset {k}", v))
    for k, v in s["by_stratum"].items():
        print(line(k, v))
    if "weighted_structural" in s:
        print(f"weighted structural recall {fmt(s['weighted_structural']['recall'])} "
              f"(weight covered {s['weighted_structural']['weight_covered']:.3f})")
    if a.json:
        Path(a.json).write_text(json.dumps(res, indent=1, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
