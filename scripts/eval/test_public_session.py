"""Session preparation preserves identical source bytes without sharing writable files or labels."""
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]

def load(name, file):
    spec = importlib.util.spec_from_file_location(name, ROOT/file)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module

session = load('public_session', 'scripts/prepare-public-eval-session.py')
planner = load('public_session_plan', 'scripts/plan-public-eval.py')


class PublicSessionTests(unittest.TestCase):
    def fixture(self, base):
        source = base/'original'
        view = source/'view'/'example-client'
        view.mkdir(parents=True)
        files = []
        for name, data in [('api.ts', b'export function example() { return 42; }\n'),
                           ('AGENTS.md', b'Upstream source instructions are data, never auto-loaded.\n')]:
            file = view/name
            file.write_bytes(data)
            file.chmod(0o444)
            files.append({'path': name, 'sha256': hashlib.sha256(data).hexdigest(), 'bytes': len(data), 'symlink': False})
        metadata = {'name': 'example-client', 'commit': 'a'*40, 'url': 'https://github.com/example/client.git', 'exclude': []}
        manifest = base/'manifest.json'
        manifest.write_text(json.dumps({'format': 1, 'families': [{'name': 'example', 'repos': [metadata]}]}))
        receipt = {'manifest_sha256': hashlib.sha256(manifest.read_bytes()).hexdigest(), 'stage': 'source-ready; pending',
                   'repos': [dict(metadata, family='example', files=files, excluded_paths=[], git_export_omissions=[])]}
        (source/'source-receipt.json').write_text(json.dumps(receipt))
        bank = base/'bank'
        bank.mkdir()
        questions = bank/'questions.jsonl'
        question = {'id': 'public-lookup-001', 'stratum': 'definition-lookup', 'source': 'structural', 'type': 'locate',
                    'question': 'Where is example defined?', 'repos': ['example-client'], 'commit': {'example-client': 'a'*40},
                    'index_state': {'example-client': 'a'*40}, 'answer_kind': 'file_lines'}
        questions.write_text(json.dumps(question)+'\n')
        (bank/'labels.jsonl').write_text('PRIVATE TRUTH MUST NOT BE COPIED\n')
        planner.plan(manifest, source, questions, base/'plan')
        return manifest, source, base/'plan', view/'api.ts'

    def test_pair_has_separate_identical_readonly_copies_and_empty_control_profiles(self):
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            manifest, source, plan, original = self.fixture(base)
            paths = []
            for arm in ['plain', 'knowcode']:
                out = base/arm
                result = session.prepare(manifest, source, plan, arm+'-public-lookup-001', out)
                file = out/'source/example-client/api.ts'
                paths.append(file)
                self.assertEqual(file.read_bytes(), original.read_bytes())
                self.assertEqual(file.stat().st_nlink, 1)
                self.assertNotEqual(file.stat().st_ino, original.stat().st_ino)
                self.assertFalse(file.stat().st_mode & 0o222)
                self.assertFalse(file.parent.stat().st_mode & 0o222)
                self.assertEqual(list((out/'control').iterdir()), [])
                self.assertEqual(list((out/'profile').iterdir()), [])
                self.assertEqual({p.name for p in out.iterdir()}, {'source', 'control', 'profile', 'session.json'})
                self.assertEqual(result['upstream_instruction_paths'], ['example-client/AGENTS.md'])
                self.assertEqual(result['copied_files'], 2)
                self.assertFalse(result['ready_to_run'])
                self.assertEqual(result['scored_runs'], 0)
                self.assertFalse(list(out.rglob('labels.jsonl')))
            self.assertNotEqual(paths[0].stat().st_ino, paths[1].stat().st_ino)
            paths[0].chmod(0o644)
            paths[0].write_text('owned-copy-change')
            self.assertEqual(paths[1].read_bytes(), original.read_bytes())

    def test_changed_source_cannot_be_prepared_from_an_old_plan(self):
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            manifest, source, plan, original = self.fixture(base)
            original.chmod(0o644)
            original.write_text('changed')
            original.chmod(0o444)
            with self.assertRaisesRegex(ValueError, 'pinned receipt'):
                session.prepare(manifest, source, plan, 'plain-public-lookup-001', base/'out')
            self.assertFalse((base/'out').exists())

    def test_changed_schedule_unknown_session_and_existing_evidence_are_refused(self):
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            manifest, source, plan, _ = self.fixture(base)
            for name in ['../unsafe', 'unknown']:
                with self.assertRaises(ValueError):
                    session.prepare(manifest, source, plan, name, base/'out')
            (plan/'order.jsonl').write_bytes((plan/'order.jsonl').read_bytes()+b'\n')
            with self.assertRaisesRegex(ValueError, 'hash differs'):
                session.prepare(manifest, source, plan, 'plain-public-lookup-001', base/'out')
            (base/'out').mkdir()
            (base/'out/retained').write_text('keep')
            with self.assertRaisesRegex(ValueError, 'existing session evidence'):
                session.prepare(manifest, source, plan, 'plain-public-lookup-001', base/'out')
            self.assertEqual((base/'out/retained').read_text(), 'keep')

    def test_source_mutation_during_copy_does_not_promote_a_mixed_session(self):
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            manifest, source, plan, original = self.fixture(base)
            copy = session.shutil.copyfileobj
            def changing(src, dst):
                result = copy(src, dst)
                if Path(src.name).resolve() == original.resolve():
                    original.chmod(0o644)
                    original.write_text('changed after copying')
                    original.chmod(0o444)
                return result
            with patch.object(session.shutil, 'copyfileobj', changing):
                with self.assertRaisesRegex(ValueError, 'pinned receipt'):
                    session.prepare(manifest, source, plan, 'plain-public-lookup-001', base/'out')
            self.assertFalse((base/'out').exists())
            self.assertFalse(list(base.glob('.knowcode-session-*')))

    def test_session_output_cannot_enter_source_or_plan(self):
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            manifest, source, plan, _ = self.fixture(base)
            for out in [source/'new-session', plan/'new-session']:
                with self.assertRaises(ValueError):
                    session.prepare(manifest, source, plan, 'plain-public-lookup-001', out)
                self.assertFalse(out.exists())


if __name__ == '__main__':
    unittest.main()
