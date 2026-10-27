"""`kc` calls (0.4's one-tool profile) are scored as the tool they run, the same way as 0.3.0's separate tools.

kc_fixtures/ holds one full-client session twice: with separate tools, and with `kc {op: …}` calls. The audit, the
detector form, the re-check detector, re-check cost and the per-tool tallies must agree on both. The hand-labelled
re-check fixtures (recheck_fixtures/), rewritten as `kc` calls, must give the detector the same answers.
"""
import json
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest

import public_transcript
import public_v3
import recheck
import recheck_cost
import session_runner

HERE = Path(__file__).parent
FIXTURES = HERE / 'kc_fixtures'
PLUGIN = 'mcp__plugin_knowcode_knowcode__'


def events(name):
    return recheck.read_events(FIXTURES / name)


def audit(evs):
    return public_transcript.audit(evs, question_id='public3-example-001', arm='knowcode-full', research_call_budget=30,
                                   source_root='/study/control/index')


def as_kc(name, inp):
    """A separate-tool call written as the `kc` call that runs it: the inverse of public_transcript.knowcode_tool."""
    op = name.split('__', 2)[2].removeprefix('kc_')
    main, many = public_transcript.ONE_OPS[op]
    rest = dict(inp)
    out = {'op': op}
    if main in rest and main != many:
        value = rest.pop(main)
        out['q'] = value[0] if main in public_transcript.ONE_LISTS else value
    if many and many in rest:
        value = rest.pop(many)
        if main == many and len(value) == 1:
            out['q'] = value[0]
        else:
            out['qs'] = value
    if 'depth' in rest:
        out['depth'] = rest.pop('depth')
    return name.split('__', 2)[0] + '__' + name.split('__', 2)[1] + '__kc', {**out, **rest}


def rewrite(evs):
    """The events with every KnowCode call written as a `kc` call, and how many there were."""
    count = 0
    for event in evs:
        message = event.get('message') if isinstance(event, dict) else None
        for block in (message or {}).get('content') or ():
            if isinstance(block, dict) and block.get('type') == 'tool_use' and block['name'].startswith(recheck.KC_PREFIX):
                block['name'], block['input'] = as_kc(block['name'], block['input'])
                count += 1
    return evs, count


class KcCallTests(unittest.TestCase):
    def test_a_kc_call_is_the_tool_its_op_runs(self):
        tool = public_transcript.knowcode_tool
        self.assertEqual(tool(PLUGIN + 'kc', {'op': 'refs', 'q': 'Save', 'repo': 'app'}), ('kc_refs', {'name': 'Save', 'repo': 'app'}))
        self.assertEqual(tool('mcp__knowcode__kc', {'op': 'context', 'qs': ['A', 'B']}), ('kc_context', {'symbols': ['A', 'B']}))
        self.assertEqual(tool(PLUGIN + 'kc', {'op': 'impact', 'q': 'X', 'depth': 2}), ('kc_impact', {'symbol': 'X', 'depth': 2}))
        self.assertEqual(tool(PLUGIN + 'kc', {'op': 'kc_tests', 'q': 'a.cs'}), ('kc_tests', {'targets': ['a.cs']}))
        self.assertEqual(tool(PLUGIN + 'kc', {'op': 'more', 'q': 'c1'}), ('kc_more', {'cursor': 'c1'}))
        self.assertEqual(tool(PLUGIN + 'kc_refs', {'name': 'Save'}), ('kc_refs', {'name': 'Save'}))
        # No known op: still a KnowCode call, tallied as `kc` (the server answers it with an error).
        self.assertEqual(tool(PLUGIN + 'kc', {'op': 'nope'}), ('kc', {'op': 'nope'}))
        self.assertEqual(tool(PLUGIN + 'kc', {}), ('kc', {}))
        self.assertIsNone(tool('Grep', {'pattern': 'Save'}))

    def test_the_op_table_matches_the_server(self):
        source = (HERE.parent.parent / 'crates/knowcode/src/mcp.rs').read_text()
        table = re.search(r'pub const ONE_OPS[^=]*= &\[(.*?)\];', source, re.S)
        self.assertIsNotNone(table, 'mcp.rs has no ONE_OPS')
        ops = {op: (main, many) for op, main, many in re.findall(r'\("(\w+)", "(\w*)", "(\w*)"\)', table.group(1))}
        self.assertEqual(ops, public_transcript.ONE_OPS)
        lists = re.search(r'const ONE_LISTS[^=]*= &\[(.*?)\];', source, re.S)
        self.assertEqual(tuple(re.findall(r'"(\w+)"', lists.group(1))), public_transcript.ONE_LISTS)

    def test_both_builds_audit_and_tally_the_same(self):
        separate, one = audit(events('separate-tools.jsonl')), audit(events('one-tool.jsonl'))
        for out in (separate, one):
            self.assertTrue(out['transcript_integrity'], out['transcript_issues'])
            self.assertEqual(out['policy_flags'], [])
        self.assertEqual(one['knowcode_calls'], 6)
        self.assertEqual(one['knowcode_calls'], separate['knowcode_calls'])
        self.assertEqual(one['knowcode_tools'], {'kc_refs': 2, 'kc_context': 1, 'kc_impact': 1, 'kc_more': 1, 'kc_tests': 1})
        self.assertEqual(one['knowcode_tools'], separate['knowcode_tools'])
        self.assertEqual(one['plain_calls'], separate['plain_calls'])
        self.assertEqual(one['processed_context_tokens'], separate['processed_context_tokens'])
        self.assertEqual(public_v3.knowcode_tools(public_v3.detector_form(events('one-tool.jsonl'))), one['knowcode_tools'])

    def test_both_builds_give_the_detector_and_cost_the_same_reading(self):
        separate = public_v3.detector_form(events('separate-tools.jsonl'))
        one = public_v3.detector_form(events('one-tool.jsonl'))
        self.assertEqual(one, separate)
        found = recheck.detect(one)
        rechecked = {a['id'] for a in found['answers'] if a['rechecked']}
        # The Read re-checks t1's line, t6 repeats t1, and the Grep searches for a symbol t4 named.
        self.assertTrue({'t1', 't4'} <= rechecked, rechecked)
        items = ['app/src/Startup.cs:22', 'app/src/Program.cs:9']
        self.assertGreater(recheck_cost.cost(one, items)['recheck_tokens'], 0)
        raw_one = recheck.detect(events('one-tool.jsonl'))
        self.assertEqual(raw_one['answers'], [])  # unconverted, the plugin's `kc` is not a KnowCode answer to recheck.py

    def test_a_note_written_through_kc_is_flagged(self):
        evs = events('one-tool.jsonl')
        evs[1]['message']['content'][0]['input'] = {'op': 'note_add', 'q': 'BuildProvider', 'text': 'checked'}
        self.assertIn('persistent note tool forbidden', audit(evs)['policy_flags'])

    def test_a_plain_arm_calling_kc_is_outside_its_tools(self):
        out = public_transcript.audit(events('one-tool.jsonl'), question_id='public3-example-001', arm='plain-full',
                                      research_call_budget=30, source_root='/study/control/index')
        self.assertIn('tool outside arm: ' + PLUGIN + 'kc', out['policy_flags'])

    def test_labelled_recheck_fixtures_read_the_same_as_kc_calls(self):
        cases = sorted((HERE / 'recheck_fixtures').glob('*/*.jsonl'))
        self.assertGreaterEqual(len(cases), 50)
        rewritten = 0
        for case in cases:
            original = recheck.read_events(case)
            kc_events, count = rewrite(recheck.read_events(case))
            rewritten += count
            as_one = public_v3.detector_form(kc_events)
            self.assertEqual(as_one, original, case.name)
            self.assertEqual(recheck.detect(as_one), recheck.detect(original), case.name)
        self.assertEqual(rewritten, 60)


class HookEventTests(unittest.TestCase):
    """A full client run with --include-hook-events records each hook it runs; the audit counts them per event."""
    def test_hook_runs_responses_and_notes_are_counted_per_event(self):
        evs = events('one-tool.jsonl')
        def hook(sub, hid, name, **extra):
            return dict(type='system', subtype=sub, hook_id=hid, hook_name=name, hook_event=name.split(':')[0], session_id='s', **extra)
        evs[1:1] = [hook('hook_started', 'h0', 'SessionStart:startup'),
                    hook('hook_response', 'h0', 'SessionStart:startup', outcome='success', exit_code=0,
                         stdout='{"hookSpecificOutput":{"additionalContext":"KnowCode index: app"}}'),
                    hook('hook_started', 'h1', 'PreToolUse:Grep'),
                    hook('hook_response', 'h1', 'PreToolUse:Grep', outcome='success', exit_code=0, stdout=''),
                    hook('hook_started', 'h2', 'PreToolUse:Bash'),
                    hook('hook_response', 'h2', 'PreToolUse:Bash', outcome='success', exit_code=0,
                         stdout='{"hookSpecificOutput":{"additionalContext":"KnowCode index: Save"}}'),
                    hook('hook_started', 'h3', 'PreToolUse:Glob'),
                    hook('hook_started', 'h4', 'PreToolUse:Grep'),
                    hook('hook_response', 'h4', 'PreToolUse:Grep', outcome='error', exit_code=1, stdout='')]
        out = audit(evs)
        self.assertTrue(out['transcript_integrity'], out['transcript_issues'])
        self.assertEqual(out['policy_flags'], [])
        self.assertEqual(out['hooks'], {'SessionStart': dict(started=1, responded=1, with_note=1, not_success=0),
                                        'PreToolUse': dict(started=4, responded=3, with_note=1, not_success=1)})
        self.assertEqual(audit(events('one-tool.jsonl'))['hooks'], {})
        tally = public_v3.hook_tally([dict(hooks=out['hooks']), dict(hooks={}), dict()])
        self.assertEqual(tally['by_event']['PreToolUse'], dict(sessions_with_hook=1, started=4, responded=3, with_note=1, not_success=1))
        self.assertEqual(tally['sessions_without_hook_record'], 1)


TOOLCHAIN = Path('/Library/Developer/CommandLineTools')


@unittest.skipUnless(sys.platform == 'darwin' and Path('/usr/bin/sandbox-exec').is_file() and (TOOLCHAIN / 'usr/bin/git').is_file(),
                     'needs macOS sandbox-exec and the command line tools')
class NotesWriteTests(unittest.TestCase):
    """No note is written in a KnowCode session, whichever tool or `kc` op asks: the sandbox refuses the database."""
    def session(self, base):
        session = Path(base) / 'session'
        for name in ('source', 'control', 'profile'):
            (session / name).mkdir(parents=True)
        (session / 'session.json').write_text(json.dumps(
            {'source_root': 'source', 'required_client_cwd': 'control', 'empty_profile_directory': 'profile'}))
        return session

    def write(self, policy):
        data = policy.paths['CONTROL'] / 'kc-data'
        data.mkdir(exist_ok=True)
        results = {}
        for suffix in session_runner.NOTES_SUFFIXES + ('-other',):
            argv = ['/bin/bash', '-c', 'echo x > "$1"', 'sh', str(data / ('notes.sqlite' + suffix))]
            results[suffix] = subprocess.run(policy.command(argv), capture_output=True).returncode
        return results

    def test_a_knowcode_session_cannot_write_its_notes_database_and_a_plain_one_is_unchanged(self):
        with tempfile.TemporaryDirectory() as base:
            session = self.session(base)
            knowcode = session_runner.policy_for(session, Path('/bin/bash'), knowcode=Path('/bin/echo'), git_toolchain=TOOLCHAIN)
            written = self.write(knowcode)
            self.assertTrue(all(written[s] != 0 for s in session_runner.NOTES_SUFFIXES), written)
            self.assertEqual(written['-other'], 0)  # the rest of the data directory stays writable
            plain = session_runner.policy_for(session, Path('/bin/bash'))
            self.assertNotIn('NOTES0', plain.parameters)
            self.assertEqual(set(self.write(plain).values()), {0})


if __name__ == '__main__':
    unittest.main()
