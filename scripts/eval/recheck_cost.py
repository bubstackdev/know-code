#!/usr/bin/env python3
"""Re-check cost: the tokens a session spends re-checking KnowCode answers, and whether each re-check changed the answer.

    recheck_cost.py transcript.jsonl answer.json [...]

The 0.2 detector (recheck.py) decides which calls re-check a KnowCode answer; this module doesn't change that rule.
For each re-checking call it reports:

- **tokens**: what the call adds to the session's research tokens. Research tokens count every context token each
  model turn processes, so a call costs the context it adds (its tool_use block and its result), once for every later
  turn that carries it. Per-call sizes aren't in the transcript; they are estimated from usage. The context growth
  between one turn and the next is what that turn's calls added (with the turn's text and thinking), and it is split
  among those calls by the length of their input and result text. The call's own output tokens, a command string, are
  not counted. A call counted against several answers is counted once.
- **changed**: whether the final answer names a location the call's result (or its read target) shows and no KnowCode
  answer in the session names: the re-check found something KnowCode didn't say. A re-check that only confirms, or
  removes, isn't marked changed.

A session whose turns lack context usage, or whose context shrank (compaction), gets tokens None.
"""
from __future__ import annotations

import json
from pathlib import Path
import sys

import recheck

CONTEXT_FIELDS = ('input_tokens', 'cache_creation_input_tokens', 'cache_read_input_tokens')


def turns_of(events):
    """Model turns in order: message id, latest context size, and the tool calls each issued."""
    order, context, calls = [], {}, {}
    for event in events:
        message = event.get('message') if isinstance(event, dict) else None
        if event.get('type') != 'assistant' or not isinstance(message, dict) or not message.get('id'):
            continue
        mid = message['id']
        if mid not in context:
            order.append(mid)
            context[mid] = None
            calls[mid] = []
        usage = message.get('usage')
        if isinstance(usage, dict) and all(isinstance(usage.get(k, 0), int) for k in CONTEXT_FIELDS) and 'input_tokens' in usage:
            context[mid] = sum(usage.get(k, 0) for k in CONTEXT_FIELDS)
        for block in message.get('content') or []:
            if isinstance(block, dict) and block.get('type') == 'tool_use' and block.get('id') not in calls[mid]:
                calls[mid].append(block.get('id'))
    return [dict(id=m, context=context[m], calls=calls[m]) for m in order]


def location_path(item):
    text = item if isinstance(item, str) else (item.get('location') if isinstance(item, dict) else None)
    if not isinstance(text, str):
        return None
    path = text.split(':', 1)[0].strip()
    return path or None


def tail(path):
    """A location's path without its repository: answers and tool output don't always carry the repo prefix."""
    return path.split('/', 1)[1] if '/' in path else path


def call_tokens(turns, by_id):
    """Estimated research tokens each tool call adds, keyed by call id (None when usage doesn't allow it)."""
    contexts = [t['context'] for t in turns]
    if not contexts or any(c is None for c in contexts) or any(b < a for a, b in zip(contexts, contexts[1:])):
        return None
    costs = {}
    last = len(turns) - 1
    for index, turn in enumerate(turns):
        if index == last:
            for cid in turn['calls']:
                costs[cid] = 0
            continue
        added = contexts[index + 1] - contexts[index]
        later = last - index  # turns index+1 .. last each read what this turn's calls added
        sizes = {}
        for cid in turn['calls']:
            call = by_id.get(cid)
            text = recheck.result_text(call['result']) if call and call['result'] else ''
            sizes[cid] = len(json.dumps(call['input'] if call else {})) + len(text)
        total = sum(sizes.values())
        for cid, size in sizes.items():
            costs[cid] = round(added * size / total * later) if total else 0
    return costs


def cost(events, final_items):
    """Per-session re-check cost: every call that re-checks a KnowCode answer, its tokens and whether it changed the
    final answer, and the session total."""
    found = recheck.detect(events)
    calls = recheck.calls_in_order(events)
    by_id = {c['id']: c for c in calls}
    tokens = call_tokens(turns_of(events), by_id)
    kc_text = ' '.join(recheck.result_text(c['result']) for c in calls
                       if c['name'].startswith(recheck.KC_PREFIX) and c['result'] and c['result'].get('is_error') is not True)
    final_paths = [p for p in (location_path(i) for i in final_items or []) if p]
    rechecks = {}
    for answer in found['answers']:
        if not (answer['claimed_locations'] or answer['claimed_symbols']):
            continue
        for row in answer['by']:
            entry = rechecks.setdefault(row['call'], dict(call=row['call'], tool=row['tool'], answers=[]))
            entry['answers'].append(answer['id'])
    rows = []
    for cid, entry in rechecks.items():
        call = by_id[cid]
        shown = recheck.result_text(call['result']) if call['result'] else ''
        target = call['input'].get('file_path') or call['input'].get('path') or ''
        added = [p for p in final_paths if tail(p) not in kc_text
                 and (tail(p) in shown or (isinstance(target, str) and target.endswith(tail(p))))]
        rows.append(dict(entry, tokens=tokens.get(cid) if tokens is not None else None, changed=bool(added),
                         added=added))
    rows.sort(key=lambda r: [c['id'] for c in calls].index(r['call']))
    return dict(format=1, rule='recheck_cost.py docstring', recheck_calls=len(rows),
                recheck_tokens=sum(r['tokens'] for r in rows) if tokens is not None else None,
                changed_calls=sum(r['changed'] for r in rows), calls=rows)


def main():
    args = sys.argv[1:]
    for transcript, answer in zip(args[::2], args[1::2]):
        items = json.loads(Path(answer).read_text()).get('items', [])
        print(json.dumps(dict(cost(recheck.read_events(transcript), items), transcript=Path(transcript).name),
                         sort_keys=True))


if __name__ == '__main__':
    main()
