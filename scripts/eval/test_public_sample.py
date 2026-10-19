"""Deterministic source candidate checks; no tool-under-test output or network."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest


SCRIPTS = Path(__file__).resolve().parents[1]


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


sample = load('public_sample', SCRIPTS/'sample-public-eval.py')
fixtures = load('scope_fixtures', SCRIPTS/'eval/test_public_scope.py')


class PublicSampleTests(unittest.TestCase):
    def test_explicit_scope_retains_library_paths_and_omits_unapproved_files(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root/'libs').mkdir()
            (root/'src').mkdir()
            (root/'libs/IClock.cs').write_text('public interface IClock {}\n')
            (root/'src/ISecret.cs').write_text('public interface ISecret {}\n')
            paths = ['libs/IClock.cs', 'src/ISecret.cs']
            old = sample.BANK.structural_candidates(root, paths, 'di-implementations', None, None)
            self.assertEqual([row['key'] for row in old], ['ISecret'])
            scoped = sample.BANK.structural_candidates(root, paths, 'di-implementations', None, None,
                                                       eligible_files={'libs/IClock.cs'})
            self.assertEqual([row['key'] for row in scoped], ['IClock'])

    def test_ranking_is_independent_of_enumeration_order_and_does_not_check_callers(self):
        with tempfile.TemporaryDirectory() as temp:
            views = Path(temp)
            (views/'autofac/src').mkdir(parents=True)
            rows = []
            for name in ['First', 'Second']:
                path = 'src/'+name+'.cs'
                (views/'autofac'/path).write_text('public class '+name+' {\n public void NeverCalled() {}\n}\n')
                rows.append({'repo': 'autofac', 'family': 'autofac', 'path': path,
                             'language': 'csharp', 'subject_eligible': True})
            first = sample.rank_candidates(rows, views)
            self.assertEqual(first, sample.rank_candidates(list(reversed(rows)), views))
            self.assertEqual(len([row for row in first if row['task'] == 'two-hop-impact']), 2)
            other = sample.rank_candidates(rows, views, seed='another-seed')
            self.assertEqual({row['id'] for row in first}, {row['id'] for row in other})
            self.assertNotEqual([row['rank'] for row in first], [row['rank'] for row in other])

    def test_explicit_scope_reads_past_legacy_size_limit_and_propagates_read_errors(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            file = root/'Large.cs'
            file.write_text(' '*2_000_001+'\npublic interface ILate {}\n')
            result = sample.BANK.structural_candidates(root, ['Large.cs'], 'di-implementations', None, None,
                                                       eligible_files={'Large.cs'})
            self.assertEqual(result[0]['key'], 'ILate')
            file.unlink()
            with self.assertRaises(FileNotFoundError):
                sample.BANK.structural_candidates(root, ['Large.cs'], 'di-implementations', None, None,
                                                  eligible_files={'Large.cs'})

    def test_one_file_per_stratum_and_shortfalls_are_not_reallocated(self):
        candidates = []
        for n, file in enumerate(['src/One.cs', 'src/One.cs', 'src/Two.cs']):
            candidates.append({'task': 'http-cross-repo', 'family': 'jellyfin', 'repo': 'jellyfin-server',
                               'file': file, 'rank': str(n), 'identity': str(n), 'id': str(n)})
        groups = sample.provisional_picks(candidates, {'jellyfin-server'})
        group = next(g for g in groups if g['task'] == 'http-cross-repo' and g['group'] == 'jellyfin')
        self.assertEqual(group['provisional_ids'], ['0', '2'])
        self.assertEqual(group['shortfall'], 1)
        missing = next(g for g in groups if g['task'] == 'http-cross-repo' and g['group'] == 'bitwarden-core')
        self.assertEqual(missing['shortfall'], 3)

    def test_integrated_sampling_rechecks_source_and_retains_existing_outputs(self):
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            manifest, source, census, policy = fixtures.PublicScopeTests().fixture(base)
            scoped = base/'scope'
            sample.SCOPE.scope(manifest, source, census, policy, scoped)
            args = manifest, source, census, scoped, policy
            result = sample.sample(*args, base/'out')
            self.assertEqual(result['population_by_task'], {'definition-lookup': 1})
            row = json.loads((base/'out/candidates.jsonl').read_text())
            self.assertEqual(row['key'], 'Api')
            self.assertEqual(row['file'], 'libs/core/api.ts')
            with self.assertRaisesRegex(ValueError, 'existing evidence'):
                sample.sample(*args, base/'out')
            path = source/'view/example/libs/core/api.ts'
            path.chmod(0o644)
            path.write_text('changed source')
            path.chmod(0o444)
            with self.assertRaisesRegex(ValueError, 'pinned receipt'):
                sample.sample(*args, base/'changed-out')
            self.assertFalse((base/'changed-out').exists())
