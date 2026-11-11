"""Judges compare-v1's normalised answers against the toolchain's labels (eval/compare-v1/README.md, "Scoring").

The functions here are pure: an adapter has already normalised a tool's answer into items, and the caller supplies
the labels and the enumeration's view of the lines involved. Nothing here reads a tool's raw output.

Items:
    ('site', file, line)          a reference site (refs)
    ('span', file, first, last)   an enclosing symbol named instead of a site (refs), lines inclusive
    ('decl', file, line)          a declaration (callers, impact, find), at the line the tool gives
    ('path', file)                a file (locate, task), in the order the answer first names it
"""
from __future__ import annotations

from dataclasses import dataclass, field
import math
import re

# How far below a tool's line a declaration's name may sit and still be that declaration (identity-v1's rule: a
# declaration that starts at a doc comment, attribute or decorator).
NAME_LINE_REACH = 10

# Outcomes a question can have for a tool, apart from a judged answer. Counted apart; never a hit or a miss.
NOT_JUDGED = ('no_counterpart', 'not_found', 'error')


@dataclass
class Judged:
    """One answer's judgement: labels named and missed, items right, wrong and unjudged."""
    hits: list = field(default_factory=list)       # labels the answer names
    misses: list = field(default_factory=list)     # labels it doesn't
    right: list = field(default_factory=list)      # items that are labels (or hold one)
    wrong: list = field(default_factory=list)      # items the toolchain binds elsewhere
    unjudged: list = field(default_factory=list)   # items the toolchain has no view on

    def counts(self):
        return {k: len(getattr(self, k)) for k in ('hits', 'misses', 'right', 'wrong', 'unjudged')}


def judge_refs(items, labels, bound):
    """A refs answer. `labels` is the set of (file, line) sites bound to D. `bound` maps (file, line) to the set of
    declaration keys that the toolchain binds a token of D's name to on that line; a line absent from it holds no
    such token. `labels` lines are those whose set holds D's key; any other line in `bound` is bound elsewhere."""
    labels = set(labels)
    named = set()
    j = Judged()
    for item in _unique(items):
        if item[0] == 'site':
            site = (item[1], item[2])
            if site in labels:
                named.add(site)
                j.right.append(item)
            elif site in bound:
                j.wrong.append(item)
            else:
                j.unjudged.append(item)
        elif item[0] == 'span':
            _, f, first, last = item
            held = {s for s in labels if s[0] == f and first <= s[1] <= last}
            if held:
                named |= held
                j.right.append(item)
            elif any(s[0] == f and first <= s[1] <= last for s in bound):
                j.wrong.append(item)
            else:
                j.unjudged.append(item)
        else:
            raise ValueError(f'refs answer item {item!r}')
    j.hits = sorted(named)
    j.misses = sorted(labels - named)
    return j


def name_line(lines, line, name):
    """The line, from `line` through NAME_LINE_REACH below it, holding `name` as a word; None if there is none.
    `lines` is the file's text as a list (1-based lines map to index line-1)."""
    word = re.compile(r'(?<![A-Za-z0-9_$])' + re.escape(name) + r'(?![A-Za-z0-9_$])')
    for n in range(line, min(line + NAME_LINE_REACH, len(lines)) + 1):
        if n >= 1 and word.search(lines[n - 1]):
            return n
    return None


def judge_decls(items, labels, decls, source):
    """A callers, impact or find answer. `labels` is a set of (file, name line, name). `decls` holds every
    declaration the toolchain saw, as (file, name line, name). `source(file)` returns the file's lines, or None.

    An item ('decl', file, line) is matched to a declaration in `decls` whose file is the item's and whose name line
    is the item's line moved down to the name (name_line); it is right when that declaration is a label, wrong when
    it is another declaration, and unjudged when it is no declaration the toolchain saw."""
    labels = set(labels)
    by_file = {}
    for d in decls:
        by_file.setdefault(d[0], []).append(d)
    named = set()
    j = Judged()
    for item in _unique(items):
        if item[0] != 'decl':
            raise ValueError(f'declaration answer item {item!r}')
        _, f, line = item
        text = source(f)
        candidates = by_file.get(f, [])
        match = None
        if text is not None:
            # Labels first: a line holding two declarations' names (`a, b int`) matches the label if either is one.
            for d in sorted(candidates, key=lambda d: (d not in labels, d)):
                if name_line(text, line, d[2]) == d[1]:
                    match = d
                    break
        if match is None:
            j.unjudged.append(item)
        elif match in labels:
            named.add(match)
            j.right.append(item)
        else:
            j.wrong.append(item)
    j.hits = sorted(named)
    j.misses = sorted(labels - named)
    return j


def judge_files(items, labels, single):
    """A co-change answer. `items` are ('path', file) in the answer's order; `labels` the files sharing at least two
    of F's commits; `single` those sharing exactly one, which history-v1's rule doesn't call co-changed and which are
    left unjudged. Any other file named is wrong."""
    labels, single = set(labels), set(single)
    named = set()
    j = Judged()
    for item in _unique(items):
        f = item[1]
        if f in labels:
            named.add(f)
            j.right.append(item)
        elif f in single:
            j.unjudged.append(item)
        else:
            j.wrong.append(item)
    j.hits = sorted(named)
    j.misses = sorted(labels - named)
    return j


def judge_paths(paths, labels, budgets=(2000, 4000, 8000)):
    """A locate or task answer. `paths` is a list of (path, end byte of its first mention). Returns recall of
    `labels` on the whole answer and within each budget, as {budget or 'all': (named, total)}."""
    first = {}
    for p, end in paths:
        first.setdefault(p, end)
    labels = set(labels)
    out = {'all': (len(labels & first.keys()), len(labels))}
    for b in budgets:
        out[b] = (sum(1 for p in labels if p in first and first[p] <= b), len(labels))
    return out


def wilson(k, n, z=1.959964):
    """The 95% Wilson interval of k successes in n, as (low, high); (None, None) when n is 0."""
    if n == 0:
        return (None, None)
    p = k / n
    d = 1 + z * z / n
    c = (p + z * z / (2 * n)) / d
    h = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / d
    return (max(0.0, c - h), min(1.0, c + h))


def pool(judgements):
    """Pooled and per-question figures over judged answers (a list of Judged). Recall pools labels; precision pools
    judged items (right + wrong). Per-question means skip questions with no labels or no judged items."""
    hits = sum(len(j.hits) for j in judgements)
    labels = sum(len(j.hits) + len(j.misses) for j in judgements)
    right = sum(len(j.right) for j in judgements)
    judged = sum(len(j.right) + len(j.wrong) for j in judgements)
    recalls = [len(j.hits) / (len(j.hits) + len(j.misses)) for j in judgements if j.hits or j.misses]
    precisions = [len(j.right) / (len(j.right) + len(j.wrong)) for j in judgements if j.right or j.wrong]
    return {
        'questions': len(judgements),
        'recall': _rate(hits, labels),
        'precision': _rate(right, judged),
        'unjudged': sum(len(j.unjudged) for j in judgements),
        'mean_recall': _mean(recalls, len(recalls)),
        'mean_precision': _mean(precisions, len(precisions)),
    }


def tally(outcomes):
    """Counts of the outcomes that aren't judged answers, by outcome; every key of NOT_JUDGED present."""
    out = {k: 0 for k in NOT_JUDGED}
    for o in outcomes:
        if o in out:
            out[o] += 1
    return out


def _rate(k, n):
    low, high = wilson(k, n)
    return {'k': k, 'n': n, 'rate': (k / n) if n else None, 'low': low, 'high': high}


def _mean(values, n):
    return {'n': n, 'mean': (sum(values) / n) if n else None}


def _unique(items):
    seen = set()
    for i in items:
        i = tuple(i)
        if i not in seen:
            seen.add(i)
            yield i
