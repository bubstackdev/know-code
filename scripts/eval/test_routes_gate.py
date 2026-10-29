"""routes-v1's scorer: path normalising, matching, per-repository scoring and the framework summaries."""
import importlib.util
from pathlib import Path
import unittest

HERE = Path(__file__).resolve().parent


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


gate = load('routes_gate', HERE/'routes_gate.py')


def label(method, path, framework='chi', repo='orchard', locs=()):
    return dict(repo=repo, framework=framework, method=method, path=path, handlers=['h'], locs=list(locs), programs=[])


def row(verb, path, file='api/crates.go', line=10):
    return dict(verb=verb, path=path, symbol='crates', kind='function', file=file, line=line, start_line=line)


class Norm(unittest.TestCase):
    def test_parameters_case_and_slashes(self):
        for written in ['/crates/{id}/', 'crates/:id', '/Crates/{id:[0-9]+}', 'crates/*']:
            self.assertEqual(gate.norm(written), 'crates/{}', written)
        self.assertEqual(gate.norm('/{$}'), '')

    def test_a_net_http_pattern_carries_its_method(self):
        self.assertEqual(gate.method_and_path(label('*', 'GET /crates/{id}', 'net/http')), ('GET', 'crates/{}'))
        self.assertEqual(gate.method_and_path(label('*', '/health', 'net/http')), ('*', 'health'))

    def test_any_method_matches(self):
        self.assertTrue(gate.matches(('*', 'a'), ('GET', 'a')))
        self.assertFalse(gate.matches(('POST', 'a'), ('GET', 'a')))


class Score(unittest.TestCase):
    def test_precision_and_recall_per_framework(self):
        labels = [label('GET', '/crates/{id}'), label('POST', '/crates'), label('GET', '/apples')]
        rows = [row('GET', 'crates/{id}'), row('POST', 'crates'), row('PUT', 'pears')]
        s = gate.score_repo(labels, rows)['chi']
        self.assertEqual((s['routes'], s['routes_matched'], s['labels'], s['labels_matched']), (3, 2, 3, 2))
        self.assertEqual(s['false'], ['PUT /pears at api/crates.go:10'])
        self.assertEqual(s['missed'], ['GET /apples (h)'])

    def test_macro_average_and_gating(self):
        per_repo = {
            'a': {'chi': dict(routes=40, routes_matched=40, labels=40, labels_matched=40, false=[], missed=[])},
            'b': {'chi': dict(routes=20, routes_matched=10, labels=20, labels_matched=10, false=[], missed=[])},
        }
        s = gate.summarize(per_repo)['chi']
        self.assertEqual((s['precision']['k'], s['precision']['n']), (50, 60))
        self.assertEqual(s['macro'], {'precision': 0.75, 'recall': 0.75})
        self.assertTrue(s['gated'])
        self.assertFalse(s['passed'])

    def test_a_small_framework_is_reported_not_gated(self):
        s = gate.summarize({'a': {'gin': dict(routes=3, routes_matched=1, labels=3, labels_matched=1, false=[], missed=[])}})
        self.assertFalse(s['gin']['gated'])
        self.assertTrue(s['gin']['passed'])


if __name__ == '__main__':
    unittest.main()
