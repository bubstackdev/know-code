"""Audit a bare-client transcript without executing tools or reading source/labels.

Context metrics follow the source-only study definition: latest observed context
per unique assistant message (input + cache creation + cache read); fixed overhead
is the first context and research context is last minus first. These observations
are distinct from terminal usage totals. Claude stream-json documentation:
https://code.claude.com/docs/en/headless

This is an offline audit/conversion component, not a runnable or frozen study.
Tool-policy flags require review; their absence does not prove runtime confinement.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import math
from pathlib import Path
import re

CONTEXT_FIELDS = ('input_tokens', 'cache_creation_input_tokens', 'cache_read_input_tokens')
# KnowCode's tools through an explicit MCP config (bare client) or through its Claude Code plugin (full client).
KNOWCODE_PREFIXES = ('mcp__knowcode__', 'mcp__plugin_knowcode_knowcode__')
# 0.4's default profile advertises one tool, `kc`, whose `op` names the tool it runs. Each op: (the parameter `q`
# fills, the one `qs` fills), as the knowcode crate's mcp.rs ONE_OPS has them (test_public_kc.py holds them equal).
# A `kc` call is scored as the tool it runs, so a build with separate tools and a build with `kc` are scored the same
# way.
ONE_OPS = {'impact': ('symbol', ''), 'find': ('query', ''), 'refs': ('name', 'names'), 'context': ('symbol', 'symbols'),
           'search': ('pattern', ''), 'locate': ('query', 'queries'), 'more': ('cursor', ''), 'help': ('topic', ''),
           'trace': ('symbol', ''), 'path': ('from', ''), 'flow': ('entry', ''), 'symbol_at': ('file', ''),
           'unused': ('', ''), 'rename_preview': ('symbol', ''), 'coupled': ('file', ''), 'tests': ('targets', 'targets'),
           'hotspots': ('', ''), 'ownership': ('path', ''), 'reviewers': ('files', 'files'), 'overview': ('focus', ''),
           'areas': ('area', ''), 'notes': ('symbol', ''), 'note_add': ('symbol', ''), 'grep_ast': ('pattern', ''),
           'status': ('', '')}
ONE_LISTS = ('targets', 'files')


def knowcode_tool(name, inp):
    """A KnowCode call as (the tool it runs, that tool's arguments): `kc_refs` for `mcp__knowcode__kc_refs` or for
    `kc {op: refs, q: X}`, with `q` as `name: X`. None for any other call. A `kc` call with no known op stays `kc`."""
    if not isinstance(name, str) or not name.startswith(KNOWCODE_PREFIXES):
        return None
    tool = name.split('__', 2)[2]
    inp = inp if isinstance(inp, dict) else {}
    if tool != 'kc':
        return tool, inp
    op = inp.get('op').strip().removeprefix('kc_') if isinstance(inp.get('op'), str) else None
    if op not in ONE_OPS:
        return 'kc', inp
    main, many = ONE_OPS[op]
    args = {k: v for k, v in inp.items() if k not in ('op', 'q', 'qs', 'depth')}
    if inp.get('q') is not None and main:
        args[main] = [inp['q']] if main in ONE_LISTS else inp['q']
    if inp.get('qs') and many:
        args[many] = inp['qs']
    if inp.get('depth') is not None:
        args['depth'] = inp['depth']
    return 'kc_' + op, args


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError('duplicate JSON object key')
        result[key] = value
    return result


def load_json(text):
    def reject_constant(_value):
        raise ValueError('non-finite JSON number')
    return json.loads(text, object_pairs_hook=unique_object, parse_constant=reject_constant)


def read_events(data):
    """Retain malformed-line evidence rather than silently skipping damaged logs."""
    issues, events = [], []
    try:
        text = data.decode('utf-8')
    except UnicodeDecodeError:
        return [], ['transcript is not UTF-8']
    for number, line in enumerate(text.splitlines(), 1):
        if not line.strip():
            continue
        try:
            event = load_json(line)
            if not isinstance(event, dict) or not isinstance(event.get('type'), str):
                raise ValueError('event object with type required')
            events.append(event)
        except (ValueError, TypeError):
            issues.append(f'invalid event at line {number}')
    return events, issues


def count(value):
    return type(value) is int and value >= 0


def answer_from_result(result, question_id, source_root, two_hop):
    """Convert only a complete result object, with no substring extraction/repair."""
    issues = []
    failure = {'id': question_id, 'status': 'error', 'items': [], 'notes': 'Transcript has no valid completed answer.'}
    if not result or result.get('is_error') is not False or result.get('subtype') != 'success':
        return failure, ['client result missing or unsuccessful']
    raw = result.get('result')
    if not isinstance(raw, str):
        return failure, ['client result has no answer text']
    raw = raw.strip()
    # The answer is the whole text when it is one JSON object; otherwise the last fenced ```json block, with prose
    # allowed around it. Nothing else is extracted or repaired: the block itself must be one complete object.
    fences = re.findall(r'```json[ \t]*\n(.*?)\n[ \t]*```', raw, re.S)
    whole = re.fullmatch(r'```(?:json)?\s*\n(.*?)\n```', raw, re.S)
    if whole:
        raw = whole.group(1)
    elif not raw.startswith('{') and fences:
        raw = fences[-1]
    try:
        answer = load_json(raw)
    except ValueError:
        return failure, ['answer is not one complete JSON object']

    def located(item):
        # A plain location, or {"location": ..., "kind": ...} where the question asks for kinds.
        if isinstance(item, str) and item.strip():
            return item
        if (isinstance(item, dict) and isinstance(item.get('location'), str) and item['location'].strip()
                and set(item) <= {'location', 'kind', 'note'} and all(isinstance(v, str) for v in item.values())):
            return item['location']
        return None

    if (not isinstance(answer, dict) or answer.get('status') not in ('answered', 'unsupported', 'error')
            or not isinstance(answer.get('items'), list)
            or not all(located(item) for item in answer['items'])
            or ('id' in answer and answer['id'] != question_id)):
        return failure, ['invalid answer contract']
    kinds = [item.get('kind') if isinstance(item, dict) else None for item in answer['items']]
    answer = dict(answer, items=[located(item) for item in answer['items']])
    if any(kind is not None for kind in kinds):
        answer['item_kinds'] = kinds

    def location(value):
        # Only an exact granted source prefix is portable. Preserve wrong/unknown
        # paths for scoring; never repair them into a potentially correct answer.
        prefix = str(source_root).rstrip('/') + '/'
        if value.startswith(prefix):
            value = value[len(prefix):]
        if value.startswith('/') or '..' in value.split('/'):
            issues.append('answer includes an outside or traversing source location')
        return value

    converted = dict(answer, id=question_id, items=[location(v) for v in answer['items']])
    if two_hop:
        hops = answer.get('hops')
        if (not isinstance(hops, dict) or set(hops) != {'1', '2'}
                or any(not isinstance(v, list) or any(not isinstance(s, str) or not s.strip() for s in v) for v in hops.values())):
            issues.append('missing or invalid hop assignment')
        else:
            converted['hops'] = {k: [location(v) for v in values] for k, values in hops.items()}
            if set(converted['items']) != set(converted['hops']['1'] + converted['hops']['2']):
                issues.append('answer item union differs from hop assignment')
            if set(converted['hops']['1']) & set(converted['hops']['2']):
                issues.append('same source location assigned to both shortest distances')
    return converted, issues


def audit(events, *, question_id, arm, research_call_budget, source_root, two_hop=False, parse_issues=()):
    if arm not in ('plain', 'knowcode', 'knowcode-unprompted', 'plain-full', 'knowcode-full', 'knowcode-unprompted-full') or not isinstance(question_id, str) or not question_id:
        raise ValueError('explicit question id and arm required')
    if type(research_call_budget) is not int or research_call_budget <= 0:
        raise ValueError('positive soft research-call budget required')
    issues, flags = list(parse_issues), []
    calls, tool_results, turns, terminals = {}, {}, {}, []
    session_ids = set()
    compacted = False
    model_refusal = False
    for position, event in enumerate(events):
        session_id = event.get('session_id')
        if session_id is not None:
            if not isinstance(session_id, str) or not session_id:
                issues.append('invalid session id')
            else:
                session_ids.add(session_id)
        if event.get('parent_tool_use_id') is not None:
            flags.append('subagent event outside study interface')
        kind = event['type']
        if kind not in ('assistant', 'user', 'result', 'system', 'stream_event', 'tool_use_summary', 'rate_limit_event', 'auth_status'):
            flags.append('unrecognized stream event: ' + kind)
        if kind == 'result':
            terminals.append((position, event))
        if kind == 'system' and event.get('subtype') == 'compact_boundary':
            compacted = True
        message = event.get('message')
        if kind not in ('assistant', 'user'):
            continue
        if not isinstance(message, dict) or not isinstance(message.get('content'), list):
            issues.append('invalid assistant/user message')
            continue
        message_id = message.get('id')
        if kind == 'assistant':
            model_refusal = model_refusal or message.get('stop_reason') == 'refusal'
            if not isinstance(message_id, str) or not message_id:
                issues.append('assistant message lacks id')
            else:
                turn = turns.setdefault(message_id, {'id': message_id, 'contexts': [], 'observed_output_tokens': None})
                usage = message.get('usage')
                if isinstance(usage, dict):
                    if 'input_tokens' in usage:
                        values = [usage.get(k, 0) for k in CONTEXT_FIELDS]
                        if all(count(v) for v in values):
                            turn['contexts'].append(sum(values))
                        else:
                            issues.append('invalid assistant context usage')
                    if 'output_tokens' in usage:
                        value = usage['output_tokens']
                        if count(value):
                            turn['observed_output_tokens'] = max(turn['observed_output_tokens'] or 0, value)
                        else:
                            issues.append('invalid assistant output usage')
        for block in message['content']:
            if not isinstance(block, dict):
                issues.append('invalid content block')
                continue
            if kind == 'assistant' and block.get('type') == 'server_tool_use':
                issues.append('server-side tool use outside recorded research interface')
            if kind == 'assistant' and block.get('type') == 'tool_use':
                tool_id, name, inp = block.get('id'), block.get('name'), block.get('input')
                if not isinstance(tool_id, str) or not tool_id or not isinstance(name, str) or not isinstance(inp, dict):
                    issues.append('invalid tool call')
                    continue
                identity = (name, inp, message_id)
                if tool_id in calls:
                    if calls[tool_id]['identity'] != identity:
                        issues.append('conflicting repeated tool id')
                    continue
                calls[tool_id] = dict(id=tool_id, name=name, input=inp, identity=identity, event_position=position)
                is_kc = name.startswith(KNOWCODE_PREFIXES)
                # A full-client arm (public-v3) also has Grep and Glob, and Skill and ToolSearch for the plugin.
                interface = ('Bash', 'Read') + (('Grep', 'Glob', 'Skill', 'ToolSearch') if arm.endswith('-full') else ())
                if name not in interface and not (not arm.startswith('plain') and is_kc):
                    flags.append('tool outside arm: ' + name)
                if is_kc and knowcode_tool(name, inp)[0] == 'kc_note_add':
                    flags.append('persistent note tool forbidden')
            elif kind == 'user' and block.get('type') == 'tool_result':
                tool_id = block.get('tool_use_id')
                if not isinstance(tool_id, str) or not tool_id:
                    issues.append('tool result lacks call id')
                elif tool_id in tool_results and tool_results[tool_id] != block:
                    issues.append('conflicting repeated tool result')
                else:
                    tool_results[tool_id] = block
    if len(session_ids) != 1:
        issues.append('expected one recorded session id')
    if len(terminals) != 1:
        issues.append('expected one terminal result')
    elif terminals[0][0] != len(events) - 1:
        issues.append('events follow terminal result')
    terminal = terminals[0][1] if len(terminals) == 1 else None
    if terminal and terminal.get('subtype') == 'success' and not turns:
        issues.append('successful stream has no assistant messages')
    model_refusal = model_refusal or bool(terminal and terminal.get('stop_reason') == 'refusal')
    refused, refusal_candidates = set(), set()
    if terminal:
        denials = terminal.get('permission_denials', [])
        if not isinstance(denials, list):
            issues.append('invalid terminal permission denials')
        else:
            for denial in denials:
                tool_id = denial.get('tool_use_id') if isinstance(denial, dict) else None
                if tool_id in calls:
                    refused.add(tool_id)
                else:
                    issues.append('permission denial has no recorded tool call')
    call_rows = []
    for tool_id, call in calls.items():
        result = tool_results.get(tool_id)
        if result is None:
            issues.append('tool call has no recorded result')
        elif result.get('is_error') is True and re.search(r'permission|not allowed|denied|dontAsk', json.dumps(result.get('content')), re.I):
            refusal_candidates.add(tool_id)
        call_rows.append({k:v for k,v in call.items() if k != 'identity'} | {
            'result_present': result is not None, 'is_error': result.get('is_error') if result else None,
            'permission_refused': tool_id in refused,
            'permission_refusal_candidate': tool_id in refusal_candidates,
            'result_sha256': hashlib.sha256(json.dumps(result,sort_keys=True).encode()).hexdigest() if result else None})
    if set(tool_results) - set(calls):
        issues.append('tool result has no recorded call')
    contexts = [row['contexts'][-1] if row['contexts'] else None for row in turns.values()]
    complete_contexts = bool(contexts) and all(value is not None for value in contexts)
    delta = contexts[-1] - contexts[0] if complete_contexts else None
    if any(len(set(row['contexts'])) > 1 for row in turns.values()):
        flags.append('context usage changed within assistant message; latest observation used')
    aggregate_usage = terminal.get('usage') if terminal else None
    if not isinstance(aggregate_usage, dict):
        aggregate_usage = None
    elif any(not count(v) for k,v in aggregate_usage.items() if k.endswith('_tokens')):
        issues.append('invalid terminal token usage')
        aggregate_usage = None
    cost = terminal.get('total_cost_usd') if terminal else None
    if cost is not None and (type(cost) not in (int, float) or not math.isfinite(cost) or cost < 0):
        issues.append('invalid reported cost')
        cost = None
    answer, answer_issues = answer_from_result(terminal, question_id, source_root, two_hop)
    if issues:
        answer = {'id':question_id,'status':'error','items':[], 'notes':'Transcript integrity error; inspect audit and raw transcript.'}
    kc_tools = [knowcode_tool(row['name'], row['input']) for row in call_rows]
    kc_calls = sum(t is not None for t in kc_tools)
    by_tool = {}
    for t in kc_tools:
        if t is not None:
            by_tool[t[0]] = by_tool.get(t[0], 0) + 1
    return dict(format=1, id=question_id, arm=arm, answer=answer, answer_issues=answer_issues,
                transcript_integrity=not issues, transcript_issues=sorted(set(issues)), policy_flags=sorted(set(flags)),
                research_calls=len(call_rows), knowcode_calls=kc_calls, knowcode_tools=by_tool, plain_calls=sum(row['name'] in ('Bash','Read','Grep','Glob') for row in call_rows),
                research_call_budget=research_call_budget, research_call_overrun=max(0,len(call_rows)-research_call_budget),
                research_call_budget_enforced=False, refused_calls=len(refused),
                heuristic_refusal_calls=len(refusal_candidates),
                permission_refusal_review_required=sorted(refusal_candidates-refused), calls=call_rows,
                unique_assistant_messages=len(turns), observed_turns=list(turns.values()),
                fixed_overhead_context=contexts[0] if contexts else None,
                context_at_answer=contexts[-1] if contexts else None, observed_context_delta=delta,
                research_context_tokens=delta if complete_contexts and not compacted else None,
                processed_context_tokens=sum(contexts) if complete_contexts else None,
                compaction_observed=compacted, terminal_usage=aggregate_usage,
                terminal_output_tokens=aggregate_usage.get('output_tokens') if aggregate_usage else None,
                client_result_subtype=terminal.get('subtype') if terminal else None,
                client_is_error=terminal.get('is_error') if terminal else None,
                model_refusal_observed=model_refusal,
                reported_cost_usd=cost, cost_is_client_estimate=True,
                measurement_notes=[
                    'Observed per-message usage may be provisional; terminal usage totals are reported separately.',
                    'Research context is a signed last-minus-first context observation, not total consumed tokens or measured savings. Compacted runs have no research-context estimate.',
                    'Refused calls use terminal permission-denial records. Error-text candidates require review and are counted separately.',
                ],
                runtime_confinement_verified=False, ready_to_run=False)


def audit_transport(measured, events, records, *, expected_model, relay_cleanup_complete,
                    expected_permission_mode='dontAsk'):
    """Join provider message IDs to unique client messages, retaining failed retries.

    A completed HTTP error can precede a successful retry. It remains visible but
    supplies no model message. So does an abandoned attempt: a broken or statusless
    response that no client message came from (protocol.md, transport retries). A partial response, missing/extra/duplicate model
    message or changed interface requires review instead of silently being scored.
    This checks correspondence; it does not prove runtime confinement or a freeze.
    """
    if not isinstance(expected_model, str) or not expected_model.strip():
        raise ValueError('explicit expected model required')
    issues, successful, failures = [], [], []
    modes = [e.get('permissionMode') for e in events if e.get('type') == 'system' and e.get('subtype') == 'init']
    if modes != [expected_permission_mode]:
        issues.append('client permission mode missing, repeated or different from configuration')
    if relay_cleanup_complete is not True:
        issues.append('relay cleanup not verified')
    if measured.get('transcript_integrity') is not True:
        issues.append('transcript integrity failed')
    if not isinstance(records, list):
        issues.append('relay records must be a list')
        records = []
    attempts, counts = 0, 0
    client_ids = [row['id'] for row in measured.get('observed_turns', [])]
    abandoned = []
    for ordinal, row in enumerate(records, 1):
        if not isinstance(row, dict) or row.get('endpoint') not in ('/v1/messages', '/v1/messages/count_tokens'):
            issues.append('invalid relay record or endpoint')
            continue
        is_message = row['endpoint'] == '/v1/messages'
        attempts += int(is_message)
        counts += int(not is_message)
        if row.get('model') != expected_model:
            issues.append('relay request model differs from configuration')
        status = row.get('response_status')
        valid_status = type(status) is int and 100 <= status <= 599
        message_id = row.get('response_message_id')
        if row.get('completed') is not True or not valid_status:
            # An abandoned attempt: a broken or statusless response that no client message came from (the client
            # retried). It is reported, not failed. A partial response that a client message did come from fails.
            if is_message and not (isinstance(message_id, str) and message_id in client_ids):
                abandoned.append({'ordinal':ordinal, 'status':status if valid_status else None,
                                  'error_type':row.get('error_type')})
                continue
            if row.get('completed') is not True:
                issues.append('relay response interrupted or incomplete')
            if not valid_status:
                issues.append('relay response lacks a valid HTTP status')
                continue
        if not 200 <= status < 300:
            failures.append({'ordinal':ordinal, 'endpoint':row['endpoint'], 'status':status})
            continue
        if is_message:
            if not isinstance(message_id, str) or not message_id:
                issues.append('successful model response lacks its message id')
            else:
                successful.append(message_id)
    if len(set(successful)) != len(successful):
        issues.append('repeated provider message id')
    missing = sorted(set(client_ids)-set(successful))
    extra = sorted(set(successful)-set(client_ids))
    if missing:
        issues.append('client messages have no matching successful provider response')
    if extra:
        issues.append('provider responses have no matching client message')
    if not missing and not extra and successful != client_ids:
        issues.append('provider response order differs from client messages')
    answer = measured['answer']
    if issues:
        answer = {'id':measured['id'], 'status':'error', 'items':[],
                  'notes':'Transport/transcript correspondence failed; retain and review the raw evidence.'}
    return dict(format=1, transport_integrity=not issues, transport_issues=sorted(set(issues)),
                actual_permission_modes=modes, model_request_attempts=attempts, token_count_attempts=counts,
                non_success_responses=failures, abandoned_attempts=abandoned, provider_message_ids=successful, client_message_ids=client_ids,
                missing_provider_ids=missing, extra_provider_ids=extra, answer=answer,
                runtime_confinement_verified=False, ready_to_run=False)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--transcript', type=Path, required=True)
    parser.add_argument('--session', type=Path, required=True)
    parser.add_argument('--research-call-budget', type=int, required=True)
    parser.add_argument('--relay-records', type=Path, help='JSON object with records and cleanup_complete; requires --expected-model')
    parser.add_argument('--expected-model', help='Frozen model identifier; requires --relay-records')
    args = parser.parse_args()
    if (args.relay_records is None) != (args.expected_model is None):
        parser.error('--relay-records and --expected-model must be supplied together')
    transcript, receipt_bytes = args.transcript.read_bytes(), args.session.read_bytes()
    receipt = load_json(receipt_bytes.decode('utf-8'))
    question, session = receipt['question'], receipt['session']
    if question['id'] != session['question_id']:
        raise SystemExit('session and question ids differ')
    events, issues = read_events(transcript)
    result = audit(events, question_id=question['id'], arm=session['arm'],
                   research_call_budget=args.research_call_budget, source_root=args.session.resolve().parent/'source',
                   two_hop=question.get('stratum')=='two-hop-impact', parse_issues=issues)
    if args.relay_records is not None:
        if not args.expected_model:
            raise SystemExit('--expected-model is required with --relay-records')
        relay_bytes = args.relay_records.read_bytes()
        relay = load_json(relay_bytes.decode('utf-8'))
        transport = audit_transport(result, events, relay['records'], expected_model=args.expected_model,
                                    relay_cleanup_complete=relay.get('cleanup_complete'))
        result['transport'] = transport
        result['relay_records_sha256'] = hashlib.sha256(relay_bytes).hexdigest()
        if not transport['transport_integrity']:
            result['unverified_answer'] = result['answer']
        result['answer'] = transport['answer']
    result.update(transcript_sha256=hashlib.sha256(transcript).hexdigest(),
                  session_receipt_sha256=hashlib.sha256(receipt_bytes).hexdigest(),
                  producer_sha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest())
    print(json.dumps(result, sort_keys=True))


if __name__ == '__main__':
    main()
