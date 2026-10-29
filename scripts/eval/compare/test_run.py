"""run.py's scoring on the host, end to end over a made-up repository."""
import gzip
import json
from pathlib import Path
import tempfile
import unittest

from compare import run

ROWS = [
    {'t': 'decl', 'repo': 'serilog', 'key': 'M:Save', 'name': 'Save', 'file': 'a.cs', 'line': 2, 'decl': 'callable'},
    {'t': 'decl', 'repo': 'serilog', 'key': 'M:Load', 'name': 'Load', 'file': 'b.cs', 'line': 1, 'decl': 'callable'},
    {'t': 'decl', 'repo': 'serilog', 'key': 'M:Other', 'name': 'Other', 'file': 'b.cs', 'line': 5, 'decl': 'callable'},
    {'t': 'ref', 'repo': 'serilog', 'key': 'M:Save', 'name': 'Save', 'file': 'b.cs', 'line': 2, 'col': 1,
     'kind': 'call', 'inside': True, 'in': 'M:Load'},
    {'t': 'ref', 'repo': 'serilog', 'key': 'M:Save2', 'name': 'Save', 'file': 'b.cs', 'line': 6, 'col': 1,
     'kind': 'call', 'inside': True, 'in': 'M:Other'},
]
SOURCES = {'a.cs': 'class A {\n  void Save() {}\n}\n', 'b.cs': 'void Load() {\n  Save();\n}\n\nvoid Other() {\n  Save();\n}\n'}
QUESTIONS = [
    {'id': 'serilog:refs:M:Save', 'repo': 'serilog', 'type': 'refs', 'target': ['a.cs', 2, 'Save'], 'labels': [['b.cs', 2]]},
    {'id': 'serilog:impact:M:Save', 'repo': 'serilog', 'type': 'impact', 'target': ['a.cs', 2, 'Save'],
     'labels': [['b.cs', 1, 'Load']]},
]
ANSWERS = [
    # One right site, one bound elsewhere (wrong).
    {'id': 'serilog:refs:M:Save', 'tool': 'knowcode', 'type': 'refs', 'outcome': 'judged',
     'items': [['site', 'b.cs', 2], ['site', 'b.cs', 6]], 'raw': []},
    {'id': 'serilog:refs:M:Save', 'tool': 'serena', 'type': 'refs', 'outcome': 'not_found', 'items': [], 'raw': []},
    {'id': 'serilog:impact:M:Save', 'tool': 'knowcode', 'type': 'impact', 'outcome': 'judged',
     'items': [['decl', 'b.cs', 1]], 'raw': []},
    {'id': 'serilog:impact:M:Save', 'tool': 'serena', 'type': 'impact', 'outcome': 'no_counterpart', 'items': [], 'raw': []},
]


class ScoreTest(unittest.TestCase):
    def test_both_scores_and_the_outcomes(self):
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            (d/'enum').mkdir()
            with gzip.open(d/'enum/enumeration-serilog.jsonl.gz', 'wt') as f:
                f.write(''.join(json.dumps(r) + '\n' for r in ROWS))
            for name, text in SOURCES.items():
                (d/'clones/serilog').mkdir(parents=True, exist_ok=True)
                (d/'clones/serilog'/name).write_text(text)
            (d/'q.jsonl').write_text(''.join(json.dumps(q) + '\n' for q in QUESTIONS))
            (d/'out').mkdir()
            (d/'out/answers.jsonl').write_text(''.join(json.dumps(a) + '\n' for a in ANSWERS))
            run.main(['score', '--questions', str(d/'q.jsonl'), '--enumerations', str(d/'enum'),
                      '--clones', str(d/'clones'), '--out', str(d/'s.json'), str(d/'out')])
            s = json.loads((d/'s.json').read_text())
            rows = {(r['tool'], r['type'], r['language']): r for r in s['answered']}
            kc = rows[('knowcode', 'refs', 'csharp')]
            self.assertEqual((kc['precision']['k'], kc['precision']['n'], kc['recall']['k']), (1, 2, 1))
            self.assertEqual(rows[('serena', 'refs', 'csharp')]['outcomes']['not_found'], 1)
            self.assertEqual(rows[('knowcode', 'impact', 'all')]['recall']['rate'], 1.0)
            every = {(r['tool'], r['type'], r['language']): r for r in s['all_questions']}
            self.assertEqual(every[('serena', 'refs', 'all')]['recall']['n'], 1)    # not_found: its label missed
            self.assertNotIn(('serena', 'impact', 'all'), every)                   # no_counterpart: left out
            misses = [json.loads(l) for l in (d/'s.misses.jsonl').read_text().splitlines()]
            self.assertEqual([m['wrong'] for m in misses if m['tool'] == 'knowcode' and 'refs' in m['id']],
                             [[['site', 'b.cs', 6]]])


if __name__ == '__main__':
    unittest.main()
