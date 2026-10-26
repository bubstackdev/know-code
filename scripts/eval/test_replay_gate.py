"""Replay gate: corpus extraction, a replay against a stand-in knowcode binary, and the comparison. No model calls."""
import copy
import hashlib
import json
from pathlib import Path
import sys
import tempfile
import types
import unittest
from unittest.mock import patch

import replay_gate

FAKE = '''#!{python}
import json, sys
if sys.argv[1] == '--version':
    print('knowcode 9.9.9'); sys.exit(0)
if sys.argv[1] == 'index':
    sys.exit(0)
assert sys.argv[1] == 'serve'
for line in sys.stdin:
    m = json.loads(line)
    if 'id' not in m:
        continue
    if m['method'] == 'initialize':
        result = {{'instructions': 'Use kc_refs.'}}
    elif m['method'] == 'tools/list':
        result = {{'tools': [{{'name': 'kc_refs', 'inputSchema': {{'type': 'object'}}, '_meta': {{'anthropic/alwaysLoad': True}}}},
                            {{'name': 'kc_search', 'inputSchema': {{'type': 'object'}}}}]}}
    else:
        p = m['params']
        if p['name'] == 'kc_search' and p['arguments'].get('pattern') == 'boom':
            result = {{'isError': True, 'content': [{{'type': 'text', 'text': 'bad pattern'}}]}}
        else:
            text = 'method Api.Save  src/api.cs:3\\n  called by request  web:src/client.ts:7\\n    7 | fetch(url)'
            result = {{'content': [{{'type': 'text', 'text': text}}]}}
    print(json.dumps({{'jsonrpc': '2.0', 'id': m['id'], 'result': result}}), flush=True)
'''


def event(block):
    return dict(type='assistant', message=dict(id='m' + block['id'], content=[block]))


class ReplayGateTests(unittest.TestCase):
    def source(self, base):
        source = base/'source'
        repos = []
        for name, path, data in [('app', 'src/api.cs', b'class Api {\n  void X() {}\n  public void Save() {}\n}\n'),
                                 ('web', 'src/client.ts', b'export const a = 1;\n' * 6 + b'fetch(url)\n')]:
            file = source/'view'/name/path
            file.parent.mkdir(parents=True)
            file.write_bytes(data)
            repos.append(dict(name=name, family='example', files=[dict(path=path, sha256=hashlib.sha256(data).hexdigest(), bytes=len(data), symlink=False)]))
        (source/'source-receipt.json').write_text(json.dumps({'stage': 'source-ready; test', 'repos': repos}))
        return source

    def study(self, base):
        study = base/'study'
        study.mkdir()
        q = dict(id='public2-http-cross-repo-001', stratum='http-cross-repo', repos=['app', 'web'])
        label = dict(id=q['id'], completeness='exhaustive', match='file_line',
                     items=[{'item': 'web/src/client.ts:7'}, {'item': 'web/src/client.ts:9'}])
        (study/'questions.jsonl').write_text(json.dumps(q) + '\n')
        (study/'labels.jsonl').write_text(json.dumps(label) + '\n')
        return study

    def test_extract_reads_kc_ops_as_their_tool_strips_session_paths_and_counts_sessions(self):
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            for n, name, inp in [(1, 'mcp__knowcode__kc_refs', {'name': 'Save', 'repo': 'app'}),
                                 (2, 'mcp__plugin_knowcode_knowcode__kc', {'op': 'refs', 'q': 'Save', 'repo': 'app'}),
                                 (3, 'mcp__knowcode__kc_symbol_at', {'file': '/x/sessions/s1/control/index/app/src/api.cs', 'line': 3})]:
                d = base/'runs'/f'b{n}-knowcode-public2-http-cross-repo-001'
                d.mkdir(parents=True)
                (d/'transcript.jsonl').write_text(json.dumps(event(dict(type='tool_use', id=f't{n}', name=name, input=inp))) + '\n')
            rows = replay_gate.extract('public-v2', [base/'runs'])
        self.assertEqual(rows, [
            dict(study='public-v2', question='public2-http-cross-repo-001', tool='kc_refs', args={'name': 'Save', 'repo': 'app'}, sessions=2),
            dict(study='public-v2', question='public2-http-cross-repo-001', tool='kc_symbol_at', args={'file': 'app/src/api.cs', 'line': 3}, sessions=1)])

    def test_places_take_the_repository_that_has_the_file(self):
        with tempfile.TemporaryDirectory() as temp:
            roots = {}
            for repo in ('app', 'web'):
                (Path(temp)/repo/'src').mkdir(parents=True)
                roots[repo] = Path(temp)/repo
            (roots['app']/'src/api.cs').write_text('x')
            text = 'src/api.cs:3 and web:src/client.ts:7 and app/src/api.cs:4 and src/nowhere.cs:1'
            self.assertEqual(replay_gate.places(text, ['app', 'web'], roots),
                             ['app/src/api.cs:3', 'web/src/client.ts:7', 'app/src/api.cs:4', 'src/nowhere.cs:1'])

    def test_a_replay_against_a_build_scores_places_counts_errors_and_reads_schemas(self):
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            source, study = self.source(base), self.study(base)
            binary = base/'knowcode'
            binary.write_text(FAKE.format(python=sys.executable))
            binary.chmod(0o755)
            corpus = base/'corpus.jsonl'
            rows = [dict(study='public-v2', question='public2-http-cross-repo-001', tool='kc_refs', args={'name': 'Save'}, sessions=2),
                    dict(study='public-v2', question='public2-http-cross-repo-001', tool='kc_search', args={'pattern': 'boom'}, sessions=1),
                    dict(study='public-v2', question='public2-http-cross-repo-001', tool='kc_more', args={'cursor': 'c1'}, sessions=1)]
            corpus.write_text(''.join(json.dumps(r) + '\n' for r in rows))
            args = types.SimpleNamespace(knowcode=str(binary), label='fake', source=str(source), work=str(base/'work'),
                                         corpus=[str(corpus)], repos=None, out=None)
            with patch.dict(replay_gate.STUDIES, {'public-v2': study}), patch.dict('os.environ', {}, clear=False) as env:
                env.pop('ANTHROPIC_API_KEY', None)
                summary = replay_gate.run(args)
                again = replay_gate.run(args)
            self.assertEqual(summary['knowcode_version'], 'knowcode 9.9.9')
            self.assertEqual(summary['skipped'], {'kc_more': 1})
            self.assertEqual(summary['calls'], 2)
            self.assertEqual(summary['strata']['http-cross-repo']['recall'], 0.5)
            self.assertEqual(summary['errors'], [dict(question='public2-http-cross-repo-001', tool='kc_search', args={'pattern': 'boom'})])
            self.assertEqual(summary['tools']['kc_search']['errors'], 1)
            self.assertEqual(summary['schemas']['default']['always_loaded'], 1)
            self.assertNotIn('always_loaded_tokens', summary['schemas']['default'])
            for key in ('strata', 'tools', 'errors', 'questions', 'schemas'):
                self.assertEqual(again[key], summary[key])
            workspace = base/'work'/'workspaces'/'app'
            self.assertTrue((workspace/'.git').is_dir())
            self.assertFalse((workspace/'src/api.cs').stat().st_mode & 0o222)
            self.assertNotEqual((workspace/'src/api.cs').stat().st_ino, (source/'view/app/src/api.cs').stat().st_ino)

    def test_compare_fails_on_regressions_beyond_tolerance_only(self):
        base = dict(strata={'two-hop-impact': dict(questions=2, recall=0.80, precision=0.50)},
                    errors=[dict(question='q1', tool='kc_search', args={'pattern': 'a'})],
                    tools={'kc_refs': dict(calls=1, errors=0, bytes_total=1000, bytes_median=1000)},
                    schemas={'default': dict(always_loaded_bytes=2000, instructions_bytes=500)})
        same = copy.deepcopy(base)
        same['strata']['two-hop-impact']['recall'] = 0.79
        same['tools']['kc_refs']['bytes_total'] = 1090
        self.assertEqual(replay_gate.compare(base, same), [])
        worse = copy.deepcopy(base)
        worse['strata']['two-hop-impact'].update(recall=0.70, precision=0.40)
        worse['errors'].append(dict(question='q2', tool='kc_refs', args={'name': 'X'}))
        worse['tools']['kc_refs']['bytes_total'] = 1200
        worse['schemas']['default']['always_loaded_bytes'] = 2300
        found = replay_gate.compare(base, worse)
        self.assertEqual(len(found), 5, found)


if __name__ == '__main__':
    unittest.main()
