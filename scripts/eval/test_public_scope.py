"""Scope checks use synthetic source only; no builds, network or tool answers."""
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1]/'scope-public-eval.py'
SPEC = importlib.util.spec_from_file_location('public_scope', SCRIPT)
module = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(module)


class PublicScopeTests(unittest.TestCase):
    def fixture(self, base):
        source = base/'source'
        root = source/'view'/'example'
        files = {'libs/core/api.ts': b'export class Api {}\n',
                 'libs/core/api.spec.ts': b'// test\n',
                 'libs/core/model.ts': b'// @generated\nexport class Model {}\n',
                 'package.json': b'{"workspaces":["libs/*"]}\n'}
        items = []
        for name, data in files.items():
            path = root/name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
            path.chmod(0o444)
            items.append({'path': name, 'bytes': len(data), 'sha256': hashlib.sha256(data).hexdigest(), 'symlink': False})
        repo = {'name': 'example', 'url': 'https://github.com/example/example.git', 'commit': 'a'*40, 'exclude': []}
        manifest = base/'manifest.json'
        manifest.write_text(json.dumps({'format': 1, 'families': [{'name': 'example', 'repos': [repo]}]}))
        receipt = {'manifest_sha256': hashlib.sha256(manifest.read_bytes()).hexdigest(), 'stage': 'source-ready; questions pending',
                   'repos': [dict(repo, family='example', files=items, excluded_paths=[], git_export_omissions=[])]}
        (source/'source-receipt.json').write_text(json.dumps(receipt))
        census = base/'census'
        module.CENSUS.census(manifest, source, census)
        policy = base/'policy.json'
        policy.write_text(json.dumps({'format': 1, 'languages': ['typescript'],
                                     'exclude_flags': ['test-path', 'generated-header'],
                                     'repos': {'example': {'include': ['libs/'], 'evidence': ['package.json']}}}))
        return manifest, source, census, policy

    def test_records_all_files_and_excludes_subjects_without_removing_evidence(self):
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            args = self.fixture(base)
            result = module.scope(*args, base/'out')
            self.assertEqual(result['files'], 4)
            self.assertEqual(result['eligible_by_repo'], {'example': 1})
            rows = {r['path']: r for r in map(json.loads, (base/'out/files.jsonl').read_text().splitlines())}
            self.assertTrue(rows['libs/core/api.ts']['subject_eligible'])
            self.assertEqual(rows['libs/core/api.spec.ts']['subject_exclusion_reasons'], ['flag:test-path'])
            self.assertEqual(rows['libs/core/model.ts']['subject_exclusion_reasons'], ['flag:generated-header'])
            self.assertEqual(result['project_evidence'][0]['path'], 'package.json')
            with self.assertRaisesRegex(ValueError, 'existing evidence'):
                module.scope(*args, base/'out')

    def test_changed_source_is_not_authorized_by_old_census(self):
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            args = self.fixture(base)
            file = args[1]/'view/example/libs/core/api.ts'
            file.chmod(0o644)
            file.write_bytes(b'changed')
            file.chmod(0o444)
            with self.assertRaisesRegex(ValueError, 'pinned receipt'):
                module.scope(*args, base/'out')
            self.assertFalse((base/'out').exists())

    def test_missing_project_evidence_and_changed_rows_fail_before_output(self):
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            args = self.fixture(base)
            policy = json.loads(args[3].read_text())
            policy['repos']['example']['evidence'] = ['missing.json']
            args[3].write_text(json.dumps(policy))
            with self.assertRaisesRegex(ValueError, 'missing regular project evidence'):
                module.scope(*args, base/'out')
            (args[2]/'files.jsonl').write_text('[]\n')
            with self.assertRaisesRegex(ValueError, 'changed census rows'):
                module.scope(*args, base/'out')
            self.assertFalse((base/'out').exists())

    def test_scope_cannot_omit_a_repository(self):
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            args = self.fixture(base)
            policy = json.loads(args[3].read_text())
            policy['repos'] = {}
            args[3].write_text(json.dumps(policy))
            with self.assertRaisesRegex(ValueError, 'repository set'):
                module.scope(*args, base/'out')
