"""Detect re-checks: plain reads, searches or repeated KnowCode calls that cover what a KnowCode answer stated.

The rule is pre-registered in eval/public-v1/protocol.md ("Re-check detector"). It reads one stream-json
transcript and never executes a tool, reads source or opens labels.

A KnowCode answer is the result of one successful `mcp__knowcode__*` call. Its claims are the source locations it
names (`file` + `line` fields, `path:line` text) and the symbol names it names (`name`, `qualified`, `symbol`,
`container` fields, and the symbol or query the call asked about). A later call re-checks that answer when it:

- reads a claimed file (`Read`, or `cat`/`head`/`tail`/`sed`/`awk`/`less`/`more` in Bash) over a range that
  includes a claimed line of that file, within RANGE_SLACK lines, or reads it whole, or the claim has no line;
- searches for a claimed symbol (`grep`/`egrep`/`fgrep`/`rg`/`ag`: an identifier of the pattern equals a claimed
  symbol of at least MIN_SYMBOL characters; `find -name`/`-path`/`-iname`: the glob matches a claimed file's name
  or contains a claimed symbol), or searches inside a claimed file;
- repeats the KnowCode call: the same tool with the same arguments.

The re-check rate is the share of KnowCode answers with at least one claim that are re-checked by any later call
in the same session. It counts coverage, not intent: a read of a claimed file for another reason still counts.
"""
from __future__ import annotations

import argparse
import fnmatch
import json
from pathlib import Path
import posixpath
import re
import shlex

RANGE_SLACK = 3
MIN_SYMBOL = 4
READ_DEFAULT_LIMIT = 2000
KC_PREFIX = 'mcp__knowcode__'
LOCATION_KEYS = ('file', 'path')
SYMBOL_KEYS = ('name', 'qualified', 'symbol', 'container')
INPUT_SYMBOL_KEYS = ('symbol', 'symbols', 'name', 'names', 'query', 'queries', 'pattern', 'from', 'to', 'target', 'entry')
# Words a search pattern shares with many answers; never treated as a claimed symbol.
STOP = {'class', 'interface', 'public', 'private', 'protected', 'internal', 'static', 'function', 'const', 'void',
        'return', 'async', 'await', 'export', 'import', 'string', 'number', 'boolean', 'true', 'false', 'null',
        'this', 'base', 'override', 'virtual', 'abstract', 'sealed', 'record', 'struct', 'enum', 'readonly',
        'method', 'field', 'property', 'type', 'file', 'line', 'items', 'kind', 'name', 'from', 'with', 'task',
        'value', 'values', 'object', 'list', 'test', 'tests', 'index', 'default', 'new', 'get', 'set'}
# Source file names: a known extension, so `Program.Main` or `config.Register` is not taken for a file.
EXTENSIONS = ('cs|csx|cshtml|razor|csproj|props|targets|sln|ts|tsx|js|jsx|mjs|cjs|mts|cts|vue|svelte|json|jsonc|'
              'html|htm|scss|sass|less|css|xml|xaml|yml|yaml|toml|ini|config|md|txt|sql|sh|ps1|py|go|java|kt|kts|'
              'swift|rs|rb|php|c|h|cc|cpp|hpp|m|mm|gradle|proto|graphql|gql|resx|cfm|cfc|tf|hcl|j2|env')
FILE_TEXT = re.compile(r'(?<![\w@./-])((?:[\w@.+-]+/)*[\w@+-][\w@.+-]*\.(?:' + EXTENSIONS + r'))(?::(\d+))?(?![\w/.])')
IDENT = re.compile(r'[A-Za-z_$][A-Za-z0-9_$]*')
# Tree answers (kc_trace, kc_impact, kc_path, kc_flow) name symbols after their kind: `method Foo.Bar  path:line`.
KIND_NAME = re.compile(r'\b(?:class|interface|method|function|property|field|constructor|record|struct|enum|type|'
                       r'variable|const|module|controller|action|component|service|protocol|extension)\s+'
                       r'([A-Za-z_$][\w$]*(?:\.[A-Za-z_$][\w$]*)*)')
READERS = {'cat', 'head', 'tail', 'less', 'more', 'awk', 'nl', 'bat'}
SEARCHERS = {'grep', 'egrep', 'fgrep', 'rg', 'ag', 'ack'}
SEPARATORS = {'|', '||', '&&', ';', '&'}


def result_text(block):
    content = block.get('content')
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return '\n'.join(part.get('text', '') for part in content if isinstance(part, dict) and part.get('type') == 'text')
    return ''


def identifiers(text):
    out = set()
    for token in IDENT.findall(text or ''):
        if len(token) >= MIN_SYMBOL and token.lower() not in STOP:
            out.add(token)
    return out


def symbol_names(value):
    """A name field as claimed symbols: the whole identifier and each dotted or generic-free part."""
    if not isinstance(value, str) or not value.strip() or len(value) > 200:
        return set()
    plain = re.sub(r'[<(\[].*$', '', value.strip())
    return identifiers(plain)


def add_location(claims, path, line=None):
    path = normalize(path)
    if not path or '.' not in posixpath.basename(path) or path.startswith('http'):
        return
    claims.setdefault(path, set()).add(line if type(line) is int and line > 0 else None)


def normalize(path):
    if not isinstance(path, str):
        return ''
    path = path.strip().strip('"\'`').replace('\\', '/')
    while path.startswith('./'):
        path = path[2:]
    return posixpath.normpath(path) if path else ''


def claims_of(tool_input, text):
    """Locations {path: {line or None}} and symbols a KnowCode answer states."""
    locations, symbols = {}, set()

    def walk(value):
        if isinstance(value, dict):
            for key in LOCATION_KEYS:
                if isinstance(value.get(key), str):
                    add_location(locations, value[key], value.get('line'))
            for key in SYMBOL_KEYS:
                symbols.update(symbol_names(value.get(key)))
            for child in value.values():
                walk(child)
        elif isinstance(value, list):
            for child in value:
                walk(child)
        elif isinstance(value, str):
            scan(value)

    def scan(value):
        for match in FILE_TEXT.finditer(value):
            add_location(locations, match.group(1), int(match.group(2)) if match.group(2) else None)
        for match in KIND_NAME.finditer(value):
            symbols.update(symbol_names(match.group(1)))

    try:
        walk(json.loads(text))
    except (ValueError, TypeError):
        scan(text)
    for key in INPUT_SYMBOL_KEYS:
        value = (tool_input or {}).get(key)
        for item in value if isinstance(value, list) else [value]:
            if isinstance(item, str):
                if FILE_TEXT.fullmatch(item.strip()):
                    match = FILE_TEXT.fullmatch(item.strip())
                    add_location(locations, match.group(1), int(match.group(2)) if match.group(2) else None)
                else:
                    symbols.update(symbol_names(item.split('#')[-1]))
    return locations, symbols


def same_file(read_path, claimed):
    """A read path (absolute or relative) names a claimed repo-relative path when one ends with the other."""
    read_path, claimed = normalize(read_path), normalize(claimed)
    if not read_path or not claimed or posixpath.basename(read_path) != posixpath.basename(claimed):
        return False
    return (read_path == claimed or read_path.endswith('/' + claimed) or
            (('/' in read_path) and claimed.endswith('/' + read_path.lstrip('/'))))


def covers(span, lines):
    """A file named without a line is covered by any read of it, unless the answer also gives lines for it."""
    lines = {line for line in lines if line is not None} or {None}
    if span is None or None in lines:
        return True
    low, high = span
    return any(low - RANGE_SLACK <= line <= high + RANGE_SLACK for line in lines)


def split_commands(command):
    try:
        lexer = shlex.shlex(command, posix=True, punctuation_chars=';&|')
        lexer.whitespace_split = True
        tokens = list(lexer)
    except ValueError:
        tokens = command.replace('|', ' | ').replace(';', ' ; ').split()
    commands, current = [], []
    for token in tokens:
        if token in SEPARATORS or set(token) <= set(';&|'):
            if current:
                commands.append(current)
            current = []
        else:
            current.append(token)
    if current:
        commands.append(current)
    return commands


def program(argv):
    while argv and re.fullmatch(r'[A-Za-z_][A-Za-z0-9_]*=.*', argv[0]):
        argv = argv[1:]
    while argv and posixpath.basename(argv[0]) in ('xargs', 'env', 'command', 'nice', 'time'):
        rest = argv[1:]
        while rest and rest[0].startswith('-'):
            # xargs options that take a value
            rest = rest[2:] if rest[0] in ('-n', '-I', '-P', '-L', '-d', '-E', '-s') else rest[1:]
        argv = rest
    return (posixpath.basename(argv[0]), argv[1:]) if argv else ('', [])


def bash_actions(command):
    """(kind, detail) actions of a Bash command: ('read', (path, span)), ('search', (patterns, paths)), ('find', globs)."""
    actions = []
    for argv in split_commands(command):
        name, args = program(argv)
        if name in READERS or name == 'sed':
            span, paths, skip = None, [], False
            script_taken = name != 'sed' and name != 'awk'
            for index, arg in enumerate(args):
                if skip:
                    skip = False
                    continue
                if arg in ('-n', '-c') and name in ('head', 'tail') and index + 1 < len(args):
                    if name == 'head' and args[index + 1].isdigit():
                        span = (1, int(args[index + 1]))
                    skip = True
                    continue
                if re.fullmatch(r'-\d+', arg) and name == 'head':
                    span = (1, int(arg[1:]))
                    continue
                if arg.startswith('-'):
                    if name in ('sed', 'awk') and arg in ('-e', '-f', '-F', '-v'):
                        skip = True
                    continue
                if not script_taken:
                    script_taken = True
                    if name == 'sed':
                        found = re.fullmatch(r"\s*(\d+)\s*(?:,\s*(\d+|\$))?\s*p\s*;?\s*", arg)
                        if found:
                            end = found.group(2)
                            span = (int(found.group(1)), int(end) if end and end.isdigit() else 10**9)
                    continue
                paths.append(arg)
            for path in paths:
                actions.append(('read', (path, span)))
        elif name in SEARCHERS:
            patterns, paths, skip_next, explicit = [], [], None, False
            for arg in args:
                if skip_next:
                    if skip_next == 'pattern':
                        patterns.append(arg)
                    skip_next = None
                    continue
                if arg in ('-e', '--regexp'):
                    skip_next, explicit = 'pattern', True
                    continue
                if arg in ('-f', '-m', '-A', '-B', '-C', '--include', '--exclude', '--exclude-dir', '-g', '--glob',
                           '-t', '--type', '--max-count', '--context'):
                    skip_next = 'value'
                    continue
                if arg.startswith('--regexp='):
                    patterns.append(arg.split('=', 1)[1]); explicit = True
                    continue
                if arg.startswith('-'):
                    continue
                if not explicit and not patterns:
                    patterns.append(arg)
                else:
                    paths.append(arg)
            actions.append(('search', (patterns, paths)))
        elif name == 'find':
            globs = [args[i + 1] for i, a in enumerate(args[:-1]) if a in ('-name', '-iname', '-path', '-ipath', '-regex')]
            if globs:
                actions.append(('find', globs))
    return actions


def call_covers(call, answer):
    """Reasons a later call covers one answer's claims (empty when it does not)."""
    reasons = []
    name, inp = call['name'], call['input']
    if name.startswith(KC_PREFIX):
        if name == answer['name'] and inp == answer['input']:
            reasons.append('repeated KnowCode call')
        return reasons
    if name == 'Read':
        path = inp.get('file_path') or inp.get('path')
        offset = inp.get('offset') if type(inp.get('offset')) is int else 1
        limit = inp.get('limit') if type(inp.get('limit')) is int else READ_DEFAULT_LIMIT
        span = (max(offset, 1), max(offset, 1) + max(limit, 1) - 1)
        for claimed, lines in answer['locations'].items():
            if same_file(path, claimed) and covers(span, lines):
                reasons.append('read ' + claimed)
        return reasons
    if name != 'Bash':
        return reasons
    for kind, detail in bash_actions(inp.get('command', '')):
        if kind == 'read':
            path, span = detail
            for claimed, lines in answer['locations'].items():
                if same_file(path, claimed) and covers(span, lines):
                    reasons.append('read ' + claimed)
        elif kind == 'search':
            patterns, paths = detail
            hit = set().union(*(identifiers(p) for p in patterns)) & answer['symbols'] if patterns else set()
            if hit:
                reasons.append('search ' + ','.join(sorted(hit)))
            for path in paths:
                if any(same_file(path, claimed) for claimed in answer['locations']):
                    reasons.append('search in ' + normalize(path))
        elif kind == 'find':
            for glob in detail:
                base = glob.rsplit('/', 1)[-1]
                names = {posixpath.basename(c) for c in answer['locations']}
                if any(fnmatch.fnmatchcase(n, base) for n in names) and any(ch not in '*?' for ch in base):
                    reasons.append('find ' + glob)
                elif identifiers(glob) & answer['symbols']:
                    reasons.append('find ' + glob)
    return reasons


def calls_in_order(events):
    """Tool calls with their results, in first-seen order; repeated stream blocks are counted once."""
    calls, results, order = {}, {}, []
    for event in events:
        message = event.get('message') if isinstance(event, dict) else None
        if not isinstance(message, dict) or not isinstance(message.get('content'), list):
            continue
        for block in message['content']:
            if not isinstance(block, dict):
                continue
            if event.get('type') == 'assistant' and block.get('type') == 'tool_use' and isinstance(block.get('id'), str):
                if block['id'] not in calls:
                    calls[block['id']] = {'id': block['id'], 'name': block.get('name', ''), 'input': block.get('input') or {}}
                    order.append(block['id'])
            elif event.get('type') == 'user' and block.get('type') == 'tool_result' and isinstance(block.get('tool_use_id'), str):
                results.setdefault(block['tool_use_id'], block)
    return [dict(calls[i], result=results.get(i)) for i in order]


def detect(events):
    """Per-answer re-check findings and session totals for one transcript."""
    calls = calls_in_order(events)
    answers, errors = [], 0
    for position, call in enumerate(calls):
        if not call['name'].startswith(KC_PREFIX):
            continue
        result = call['result']
        if result is None or result.get('is_error') is True:
            errors += 1
            continue
        locations, symbols = claims_of(call['input'], result_text(result))
        answer = dict(id=call['id'], name=call['name'], input=call['input'], position=position,
                      locations=locations, symbols=symbols)
        by = []
        for later in calls[position + 1:]:
            reasons = call_covers(later, answer)
            if reasons:
                by.append({'call': later['id'], 'tool': later['name'], 'reasons': reasons})
        answers.append(dict(answer, claims=bool(locations or symbols), rechecked=bool(by), by=by))
    scored = [a for a in answers if a['claims']]
    rechecking = {row['call'] for a in scored for row in a['by']}
    plain_after = [c for i, c in enumerate(calls) if not c['name'].startswith(KC_PREFIX)
                   and any(a['position'] < i for a in scored)]
    return {'format': 1, 'rule': 'protocol.md#re-check-detector', 'knowcode_answers': len(answers),
            'knowcode_errors': errors, 'answers_with_claims': len(scored),
            'rechecked_answers': sum(a['rechecked'] for a in scored),
            'recheck_rate': (sum(a['rechecked'] for a in scored) / len(scored)) if scored else None,
            'plain_calls_after_first_answer': len(plain_after),
            'rechecking_calls': len(rechecking),
            'answers': [{'id': a['id'], 'tool': a['name'][len(KC_PREFIX):], 'claimed_locations': sum(len(v) for v in a['locations'].values()),
                         'claimed_symbols': len(a['symbols']), 'rechecked': a['rechecked'], 'by': a['by']} for a in answers]}


def read_events(path):
    events = []
    for line in Path(path).read_text().splitlines():
        if line.strip():
            try:
                events.append(json.loads(line))
            except ValueError:
                continue
    return events


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('transcript', type=Path, nargs='+')
    args = parser.parse_args()
    for path in args.transcript:
        print(json.dumps(dict(detect(read_events(path)), transcript=path.name), sort_keys=True))


if __name__ == '__main__':
    main()
