"""Neutral metadata preparation uses real local Git on new synthetic copies; no network or KnowCode."""
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]

def load(name, path):
    spec = importlib.util.spec_from_file_location(name, ROOT/path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module

index = load('public_index', 'scripts/prepare-public-eval-index.py')
census = load('public_index_fixture_census', 'scripts/census-public-eval.py')


class PublicIndexTests(unittest.TestCase):
    def fixture(self, base):
        source = base/'export'
        view = source/'view'
        metadata, repos = [], []
        for name, pin in [('example-client', 'a'*40), ('example-server', 'b'*40)]:
            root = view/name
            root.mkdir(parents=True)
            data = b'export function example() { return 42; }\n'
            file = root/'api.ts'
            file.write_bytes(data)
            file.chmod(0o444)
            repo = {'name': name, 'commit': pin, 'url': 'https://github.com/example/'+name+'.git', 'exclude': []}
            metadata.append(repo)
            repos.append(dict(repo, family='example', files=[{'path': 'api.ts', 'sha256': index.digest(data),
                                                            'bytes': len(data), 'symlink': False}],
                              excluded_paths=[], git_export_omissions=[]))
        manifest = base/'manifest.json'
        manifest.write_text(json.dumps({'format': 1, 'families': [{'name': 'example', 'repos': metadata}]}))
        receipt = source/'source-receipt.json'
        receipt.write_text(json.dumps({'stage': 'source-ready; synthetic', 'manifest_sha256': index.digest(manifest.read_bytes()), 'repos': repos}))
        verified = census.census(manifest, source, base/'census')
        session = base/'session'
        shutil.copytree(view, session/'source')
        (session/'control').mkdir()
        state = {'session': {'arm': 'knowcode'}, 'source_root': 'source', 'required_client_cwd': 'control',
                 'manifest_sha256': index.digest(manifest.read_bytes()), 'source_receipt_sha256': index.digest(receipt.read_bytes()),
                 'source_files_sha256': verified['files_sha256']}
        (session/'session.json').write_text(json.dumps(state))
        return manifest, receipt, session

    def test_full_cohort_has_identical_readonly_payload_and_fresh_history_free_metadata(self):
        with tempfile.TemporaryDirectory() as temp:
            manifest, receipt, session = self.fixture(Path(temp))
            result = index.prepare(manifest, receipt, session)
            self.assertEqual(len(result['repos']), 2)
            self.assertFalse(result['original_history_imported'])
            self.assertFalse(result['knowcode_executed'])
            self.assertFalse(result['ready_to_run'])
            for row in result['repos']:
                root = Path(result['repo_paths'][row['name']])
                original = session/'source'/row['name']/'api.ts'
                copied = root/'api.ts'
                self.assertEqual(copied.read_bytes(), original.read_bytes())
                self.assertNotEqual(copied.stat().st_ino, original.stat().st_ino)
                self.assertEqual(copied.stat().st_nlink, 1)
                self.assertFalse(copied.stat().st_mode & 0o222)
                self.assertFalse(root.stat().st_mode & 0o222)
                self.assertTrue((root/'.git').is_dir())
                self.assertFalse((original.parent/'.git').exists())
                self.assertEqual(row['refs'], 0)
                self.assertEqual(row['objects'], 0)
                self.assertTrue(row['head_unborn'])
                p = subprocess.run(['git', 'config', '--get', 'remote.origin.url'], cwd=root, env=index.GIT_ENV, capture_output=True, text=True)
                self.assertEqual(p.stdout.strip(), row['public_origin'])
            self.assertTrue((session/'control/index/neutral-index.json').is_file())

    def test_existing_workspace_is_retained_and_plain_sessions_are_refused(self):
        with tempfile.TemporaryDirectory() as temp:
            manifest, receipt, session = self.fixture(Path(temp))
            (session/'control/index').mkdir()
            marker = session/'control/index/retained'
            marker.write_text('keep')
            with self.assertRaisesRegex(ValueError, 'existing neutral'):
                index.prepare(manifest, receipt, session)
            self.assertEqual(marker.read_text(), 'keep')
            state = json.loads((session/'session.json').read_text())
            state['session']['arm'] = 'plain'
            (session/'session.json').write_text(json.dumps(state))
            with self.assertRaisesRegex(ValueError, 'only to the KnowCode'):
                index.prepare(manifest, receipt, session)

    def test_source_and_receipt_changes_are_refused_without_promoting_output(self):
        with tempfile.TemporaryDirectory() as temp:
            manifest, receipt, session = self.fixture(Path(temp))
            receipt.write_bytes(receipt.read_bytes()+b'\n')
            with self.assertRaisesRegex(ValueError, 'receipt differs'):
                index.prepare(manifest, receipt, session)
            self.assertFalse((session/'control/index').exists())
        with tempfile.TemporaryDirectory() as temp:
            manifest, receipt, session = self.fixture(Path(temp))
            file = session/'source/example-client/api.ts'
            file.chmod(0o644)
            file.write_text('changed')
            file.chmod(0o444)
            with self.assertRaisesRegex(ValueError, 'pinned receipt'):
                index.prepare(manifest, receipt, session)
            self.assertFalse((session/'control/index').exists())

    def test_ambient_git_template_and_user_configuration_do_not_enter_metadata(self):
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            manifest, receipt, session = self.fixture(base)
            template = base/'untrusted-template'
            (template/'hooks').mkdir(parents=True)
            (template/'hooks/unrelated-hook').write_text('must not be imported')
            config = base/'user-gitconfig'
            config.write_text('[user]\nname = Synthetic Private User\n[init]\ntemplateDir = '+str(template)+'\n')
            with patch.dict('os.environ', {'GIT_CONFIG_GLOBAL': str(config), 'GIT_TEMPLATE_DIR': str(template)}):
                result = index.prepare(manifest, receipt, session)
            for path in result['repo_paths'].values():
                root = Path(path)
                self.assertFalse((root/'.git/hooks/unrelated-hook').exists())
                self.assertNotIn('Synthetic Private User', (root/'.git/config').read_text())

    def test_mutation_during_copy_does_not_publish_an_index_workspace(self):
        with tempfile.TemporaryDirectory() as temp:
            manifest, receipt, session = self.fixture(Path(temp))
            original = session/'source/example-client/api.ts'
            copy = index.shutil.copyfileobj
            def changing(src, dst):
                result = copy(src, dst)
                if Path(src.name).resolve() == original.resolve():
                    original.chmod(0o644)
                    original.write_text('changed after copy')
                    original.chmod(0o444)
                return result
            with patch.object(index.shutil, 'copyfileobj', changing):
                with self.assertRaisesRegex(ValueError, 'pinned receipt'):
                    index.prepare(manifest, receipt, session)
            self.assertFalse((session/'control/index').exists())
            self.assertFalse(list((session/'control').glob('.knowcode-neutral-*')))


if __name__ == '__main__':
    unittest.main()
