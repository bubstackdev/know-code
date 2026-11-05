"""routes-v1's label builder, on made-up listings."""
import importlib.util
from pathlib import Path
import unittest

HERE = Path(__file__).resolve().parent


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


labels = load('routes_labels', HERE/'routes/labels.py')


def program(package, status='listed', dirs='app app/api '):
    return {'t': 'program', 'repo': 'orchard', 'module': '.', 'package': package, 'status': status, 'dirs': dirs}


def route(package, method, path, handler, framework='chi', loc=None):
    r = {'t': 'route', 'repo': 'orchard', 'module': '.', 'package': package, 'framework': framework,
         'method': method, 'path': path, 'handler': handler}
    if loc:
        r['loc'] = loc
    return r


class Build(unittest.TestCase):
    def test_a_route_two_programs_list_is_one_label(self):
        rows = [program('cmd/a'), route('cmd/a', 'GET', '/crates/{id}', 'api.GetCrate'),
                program('cmd/b'), route('cmd/b', 'GET', '/crates/{id}', 'api.GetCrate')]
        got, programs = labels.build(rows)
        self.assertEqual(len(got), 1)
        self.assertEqual(got[0]['programs'], ['.:cmd/a', '.:cmd/b'])
        self.assertEqual([p['dirs'] for p in programs['orchard']], [['app', 'app/api'], ['app', 'app/api']])

    def test_a_pattern_without_a_method_answers_every_method(self):
        got, _ = labels.build([program('.'), route('.', '', '/health', 'main.health', 'net/http', 'main.go:12')])
        self.assertEqual((got[0]['method'], got[0]['locs']), ('*', ['main.go:12']))

    def test_unknown_handlers_and_unlisted_programs_give_no_labels(self):
        rows = [program('cmd/a', status='no listing'), route('cmd/b', '', '', 'http.HandlerFunc', 'unknown')]
        got, programs = labels.build(rows)
        self.assertEqual(got, [])
        self.assertEqual(programs['orchard'][0]['status'], 'no listing')

    def test_counts_by_framework_and_repository(self):
        got, _ = labels.build([program('.'), route('.', 'GET', '/a', 'h'), route('.', 'POST', '/a', 'h')])
        self.assertEqual(labels.counts(got), {'chi': {'orchard': 2}})


if __name__ == '__main__':
    unittest.main()
