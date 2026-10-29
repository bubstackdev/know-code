"""compare-v1's adapters (eval/compare-v1/README.md, "Asking"): for each tool, the lookup of a question's target, the
calls mapping.toml names, and the normalisation of the answers into score.py's items.

Every parser is a pure function of a tool's answer text, so the tests pin each format. The `ask_*` functions drive a
tools.Session and return (outcome, items, raw), where outcome is 'judged', 'not_found' or 'error', items are
score.py's tuples, and raw is every answer's text in order, kept whole for the results.
"""
from __future__ import annotations

import json
import re

from compare import mcp

PAGES = 10          # continuations followed per question (README, "Asking")
CALL_TIMEOUT = 60   # seconds per call


class NotFound(Exception):
    """The tool's own lookup found no symbol at the question's file and line."""


# ---------------------------------------------------------------------------------------------------------------------
# KnowCode

# Impact rows separate the name, the place and the rest by two spaces; a path may hold single spaces
# (`Sources/ArgumentParser/Parsable Types/ParsableCommand.swift`).
KC_ROW = re.compile(r'^\s*(?:[a-z]+(?: [a-z]+)*) by \S+ (?P<name>\S+)  (?P<file>\S(?:.*?\S)?):(?P<line>\d+)(?P<rest>(?:  .*)?)$')
KC_DECLARED = re.compile(r'\(declared at line (\d+)\)')
# The continuation, as an answer names it: `kc_more {cursor: "kc~1"}` (and, in JSON, `{"cursor": "kc~1"}`).
KC_MORE = re.compile(r'kc_more \{"?cursor"?:\s*"([^"]+)"\}')


def plain(name):
    """A declaration's simple name as the question gives it: without type parameters (`Write<T0, T1>`), an
    overload index (`run[1]`) or a parameter list (`pick(_:)`), which tools add to it."""
    name = re.sub(r'\[\d+\]$', '', name)
    name = re.sub(r'\(.*\)$', '', name)
    return re.sub(r'<.*>$', '', name).strip()


def kc_refs_sites(text):
    """Reference sites in a kc_refs JSON answer, and its cursor when cut."""
    data = json.loads(text)
    sites = [('site', i['file'], i['line']) for i in data.get('items', []) if 'file' in i and 'line' in i]
    return sites, data.get('cursor')


def kc_impact_decls(text):
    """The dependents an impact answer lists, as declarations: each row's file and the line it was declared at
    (the row's own line when the answer gives no declaration line). The frontier (`stopped at`) repeats a listed row."""
    out = []
    for line in text.splitlines():
        m = KC_ROW.match(line)
        if not m:
            continue
        declared = KC_DECLARED.search(m['rest'])
        out.append(('decl', m['file'], int(declared[1]) if declared else int(m['line'])))
    return out, (KC_MORE.search(text) or [None, None])[1]


def kc_find_decls(text):
    data = json.loads(text)
    return [('decl', i['file'], i['line']) for i in data.get('items', []) if 'file' in i and 'line' in i], data.get('cursor')


KC_HEAD = re.compile(r'^[a-z]+(?: [a-z]+)* (?P<name>\S+)  (?P<file>\S(?:.*?\S)?):(?P<line>\d+)$', re.M)


def kc_target_name(qtype, text):
    """The simple name of the declaration KnowCode answered for: refs' `name`, or impact's first row. KnowCode takes
    `file:line` as the innermost definition spanning the line, so the adapter checks the name, as the other tools'
    lookups do (README, "Lookup")."""
    if qtype == 'refs':
        name = json.loads(text).get('name') or ''
    else:
        m = KC_HEAD.search(text)
        name = m['name'] if m else ''
    return plain(name.rsplit('.', 1)[-1])


def kc_not_found(message):
    return bool(re.search(r'no (definition|symbol|declaration)|names no definition|not_found|unknown symbol', message, re.I))


def ask_knowcode(session, qtype, q):
    raw = []
    target = q.get('target')
    try:
        if qtype == 'refs':
            text, _ = session.call('kc_refs', {'name': f'{target[0]}:{target[1]}'}, CALL_TIMEOUT)
            parse = kc_refs_sites
        elif qtype in ('impact', 'callers'):
            text, _ = session.call('kc_impact', {'symbol': f'{target[0]}:{target[1]}',
                                                 'depth': 2 if qtype == 'impact' else 1}, CALL_TIMEOUT)
            parse = kc_impact_decls
        elif qtype == 'find':
            text, _ = session.call('kc_find', {'query': q['name']}, CALL_TIMEOUT)
            parse = kc_find_decls
        else:
            raise ValueError(qtype)
        raw.append(text)
        if target and kc_target_name(qtype, text) != plain(target[2]):
            return 'not_found', [], raw
        items, cursor = parse(text)
        for _ in range(PAGES - 1):
            if not cursor:
                break
            text, _ = session.call('kc_more', {'cursor': cursor}, CALL_TIMEOUT)
            raw.append(text)
            more, cursor = parse(text)
            items += more
        return 'judged', items, raw
    except mcp.Error as e:
        raw.append(str(e))
        return ('not_found' if kc_not_found(str(e)) else 'error'), [], raw


# ---------------------------------------------------------------------------------------------------------------------
# codebase-memory-mcp

def cbm_rows(text):
    """search_graph's JSON rows as dicts with qn, name, label, file, first and last line."""
    data = json.loads(text)
    out = []
    for g in data.get('groups', []):
        cols = data.get('cols') or g.get('cols') or []
        for row in g.get('rows', []):
            r = dict(zip(cols, row))
            prefix = g.get('qn_prefix') or ''
            first, _, last = str(r.get('lines', '')).partition('-')
            out.append(dict(qn=f'{prefix}.{r["name"]}' if prefix else r['name'], name=r['name'], label=r.get('label'),
                            file=g.get('file'), first=int(first) if first.isdigit() else None,
                            last=int(last) if last.isdigit() else (int(first) if first.isdigit() else None)))
    return out, bool(data.get('has_more'))


def cbm_pick(rows, file, line, name):
    """The candidate whose range holds `line`, in `file`, named `name`; the narrowest when several do."""
    hold = [r for r in rows if r['file'] == file and plain(r['name']) == plain(name) and r['first'] is not None
            and r['first'] <= line <= r['last']]
    return min(hold, key=lambda r: (r['last'] - r['first'], r['qn']), default=None)


def cbm_callers(text):
    """trace_path's callers, as (qn, hop), and its continuation cursor."""
    data = json.loads(text)
    callers = data.get('callers') or {}
    cols = callers.get('cols') or []
    out = []
    for g in callers.get('groups', []):
        prefix = g.get('qn_prefix') or ''
        for row in g.get('rows', []):
            r = dict(zip(cols, row))
            name = r.get('name') or r.get('qn')
            out.append((f'{prefix}.{name}' if prefix else name, r.get('hop')))
    return out, data.get('next_cursor') or data.get('next')


def ask_cbm(session, qtype, q):
    raw = []
    p = session.project
    try:
        if qtype == 'find':
            items, offset = [], 0
            for _ in range(PAGES):
                text, _ = session.call('search_graph', {'project': p, 'name_pattern': f'^{re.escape(q["name"])}$',
                                                        'format': 'json', 'limit': 500, 'offset': offset}, CALL_TIMEOUT)
                raw.append(text)
                rows, more = cbm_rows(text)
                items += [('decl', r['file'], r['first']) for r in rows if r['file'] and r['first']]
                offset += len(rows)
                if not more or not rows:
                    break
            return 'judged', items, raw
        file, line, name = q['target']
        text, _ = session.call('search_graph', {'project': p, 'name_pattern': f'^{re.escape(name)}$', 'file_pattern': file,
                                                'format': 'json', 'limit': 50}, CALL_TIMEOUT)
        raw.append(text)
        picked = cbm_pick(cbm_rows(text)[0], file, line, name)
        if not picked:
            return 'not_found', [], raw
        args = {'project': p, 'function_name': picked['qn'], 'direction': 'inbound', 'include_tests': True,
                'format': 'json'}
        if qtype == 'refs':
            args.update(depth=1, edge_types=['CALLS', 'CALL_REFERENCE', 'USAGE'], include_evidence=True)
        else:
            args.update(depth=2 if qtype == 'impact' else 1)
        callers, cursor = [], None
        for _ in range(PAGES):
            text, _ = session.call('trace_path', dict(args, cursor=cursor) if cursor else args, CALL_TIMEOUT)
            raw.append(text)
            page, cursor = cbm_callers(text)
            callers += page
            if not cursor:
                break
        # The answer names callers by qualified name; their files and lines come from the tool's own search.
        items = []
        for qn, _hop in dict.fromkeys(callers):
            text, _ = session.call('search_graph', {'project': p, 'qn_pattern': f'^{re.escape(qn)}$', 'format': 'json',
                                                    'limit': 5}, CALL_TIMEOUT)
            raw.append(text)
            for r in cbm_rows(text)[0]:
                if r['qn'] == qn and r['file'] and r['first']:
                    items.append(('span', r['file'], r['first'], r['last']) if qtype == 'refs'
                                 else ('decl', r['file'], r['first']))
        return 'judged', items, raw
    except mcp.Error as e:
        raw.append(str(e))
        return 'error', [], raw


# ---------------------------------------------------------------------------------------------------------------------
# Serena

SERENA_MARK = re.compile(r'^\s*>\s*(\d+):', re.M)


def serena_symbols(text):
    """find_symbol's symbols: name path, kind, file and 1-based first and last line."""
    out = []
    for s in json.loads(text):
        loc = s.get('body_location') or {}
        if 'start_line' in loc:
            out.append(dict(name_path=s['name_path'], kind=s.get('kind'), file=s.get('relative_path'),
                            first=loc['start_line'] + 1, last=loc.get('end_line', loc['start_line']) + 1))
    return out


def serena_last(name_path):
    return plain(name_path.rsplit('/', 1)[-1])


def serena_pick(symbols, file, line, name):
    hold = [s for s in symbols if s['file'] == file and serena_last(s['name_path']) == plain(name)
            and s['first'] <= line <= s['last']]
    return min(hold, key=lambda s: (s['last'] - s['first'], s['name_path']), default=None)


def serena_references(text):
    """find_referencing_symbols' answer: each referencing symbol as (file, kind, name path, first, last, site lines),
    the site lines read from the `>` marks of its snippet (0-based in the answer, 1-based here)."""
    out = []
    for file, kinds in json.loads(text).items():
        for kind, symbols in kinds.items():
            for s in symbols:
                loc = s.get('body_location') or {}
                marks = [int(m) + 1 for m in SERENA_MARK.findall(s.get('content_around_reference') or '')]
                out.append(dict(file=file, kind=kind, name_path=s.get('name_path'),
                                first=loc.get('start_line', -1) + 1, last=loc.get('end_line', -1) + 1, sites=marks))
    return out


def ask_serena(session, qtype, q):
    raw = []
    try:
        if qtype == 'find':
            # substring_matching, as in the lookup: a generic declaration's name carries its type parameters.
            text, _ = session.call('find_symbol', {'name_path_pattern': q['name'], 'substring_matching': True},
                                   CALL_TIMEOUT)
            raw.append(text)
            return 'judged', [('decl', s['file'], s['first']) for s in _symbols(text)
                              if serena_last(s['name_path']) == plain(q['name'])], raw
        if qtype == 'impact':
            return 'no_counterpart', [], raw
        file, line, name = q['target']
        # substring_matching (find_symbol's documented option): a generic declaration's name path segment carries its
        # type parameters (`InboundWebSocketMessage<T>`), which a plain name doesn't match; the pick compares plain names.
        text, _ = session.call('find_symbol', {'name_path_pattern': name, 'relative_path': file, 'depth': 0,
                                               'substring_matching': True}, CALL_TIMEOUT)
        raw.append(text)
        picked = serena_pick(_symbols(text), file, line, name)
        if not picked:
            return 'not_found', [], raw
        text, _ = session.call('find_referencing_symbols', {'name_path': picked['name_path'], 'relative_path': file},
                               CALL_TIMEOUT)
        raw.append(text)
        refs = _references(text)
        if qtype == 'refs':
            items = []
            for r in refs:
                items += [('site', r['file'], s) for s in r['sites']] or [('span', r['file'], r['first'], r['last'])]
            return 'judged', items, raw
        # callers: the referencing symbols themselves; a whole file isn't a declaration.
        return 'judged', [('decl', r['file'], r['first']) for r in refs if r['kind'] != 'File'], raw
    except mcp.Error as e:
        raw.append(str(e))
        return 'error', [], raw


def _symbols(text):
    try:
        return serena_symbols(text)
    except ValueError:  # a shortened answer (its size limit) is not JSON: nothing named
        return []


def _references(text):
    try:
        return serena_references(text)
    except ValueError:
        return []


ASK = {'knowcode': ask_knowcode, 'codebase-memory-mcp': ask_cbm, 'serena': ask_serena}
