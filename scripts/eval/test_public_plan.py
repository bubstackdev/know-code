"""Paired-study preflight regressions: source integrity, arm parity and evidence separation."""
import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location('public_plan', Path(__file__).resolve().parents[1]/'plan-public-eval.py')
planner = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(planner)


class PublicPlanTests(unittest.TestCase):
    def fixture(self, base):
        source = base/'source'
        view = source/'view'/'example-client'
        view.mkdir(parents=True)
        data = b'export function example() { return 42; }\n'
        file = view/'api.ts'
        file.write_bytes(data)
        file.chmod(0o444)
        metadata = {'name': 'example-client', 'commit': 'a'*40,
                    'url': 'https://github.com/example/client.git', 'exclude': []}
        manifest = base/'manifest.json'
        manifest.write_text(json.dumps({'format': 1, 'families': [{'name': 'example', 'repos': [metadata]}]}))
        receipt = {'manifest_sha256': hashlib.sha256(manifest.read_bytes()).hexdigest(),
                   'stage': 'source-ready; questions pending', 'repos': [dict(metadata, family='example',
                   files=[{'path': 'api.ts', 'sha256': hashlib.sha256(data).hexdigest(), 'bytes': len(data),
                           'symlink': False}], excluded_paths=[], git_export_omissions=[])]}
        (source/'source-receipt.json').write_text(json.dumps(receipt))
        questions = base/'bank/questions.jsonl'
        questions.parent.mkdir()
        records = [dict(id=f'public-lookup-{i:03}', stratum='definition-lookup', source='structural',
                        type='locate', question=f'Where is example{i} defined?', repos=['example-client'],
                        commit={'example-client': 'a'*40}, index_state={'example-client': 'a'*40},
                        answer_kind='file_lines') for i in range(8)]
        questions.write_text(''.join(json.dumps(q)+'\n' for q in records))
        return manifest, source, questions, file

    def test_both_arms_get_every_question_once_in_reproducible_interleaved_orders(self):
        questions = [{'id': f'question-{i}'} for i in range(20)]
        order = planner.schedule(questions, 'seed-one')
        self.assertEqual(order, planner.schedule(list(reversed(questions)), 'seed-one'))
        self.assertNotEqual(order, planner.schedule(questions, 'seed-two'))
        self.assertEqual(len({row['session'] for row in order}), 20*len(planner.ARMS))
        self.assertEqual(planner.ARMS, ('plain', 'knowcode', 'knowcode-unprompted'))
        for arm in planner.ARMS:
            self.assertEqual({row['question_id'] for row in order if row['arm'] == arm}, {q['id'] for q in questions})
            self.assertEqual(sum(row['arm'] == arm for row in order), 20)
        self.assertTrue(all(a['arm'] != b['arm'] for a, b in zip(order, order[1:])))
        with self.assertRaisesRegex(ValueError, 'empty order seed'):
            planner.schedule(questions, ' ')

    def test_real_preflight_verifies_source_and_writes_only_draft_operator_artifacts(self):
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            manifest, source, questions, _ = self.fixture(base)
            result = planner.plan(manifest, source, questions, base/'out')
            self.assertFalse(result['ready_to_run'])
            self.assertEqual(result['scored_runs'], 0)
            self.assertEqual(result['verified_source_files'], 1)
            self.assertEqual(result['sessions'], 8*len(planner.ARMS))
            self.assertIsNone(result['config'])
            self.assertEqual({p.name for p in (base/'out').iterdir()}, {'plan.json', 'questions.jsonl', 'order.jsonl'})
            self.assertEqual(result['order_sha256'], hashlib.sha256((base/'out/order.jsonl').read_bytes()).hexdigest())
            with self.assertRaisesRegex(ValueError, 'existing evidence'):
                planner.plan(manifest, source, questions, base/'out')

    def test_label_fields_and_question_pin_drift_are_rejected_before_output(self):
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            manifest, source, questions, _ = self.fixture(base)
            original = [json.loads(x) for x in questions.read_text().splitlines()]
            for mutate in [lambda q: q.update(items=['an answer']),
                           lambda q: q.update(index_state={'example-client': 'b'*40}),
                           lambda q: q.update(id='../escape')]:
                records = copy.deepcopy(original)
                mutate(records[0])
                questions.write_text(''.join(json.dumps(q)+'\n' for q in records))
                with self.assertRaises(ValueError):
                    planner.plan(manifest, source, questions, base/'out')
                self.assertFalse((base/'out').exists())

    def test_modified_source_and_hidden_git_history_cannot_receive_a_plan(self):
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            manifest, source, questions, file = self.fixture(base)
            file.chmod(0o644)
            file.write_text('export function different() {}\n')
            file.chmod(0o444)
            with self.assertRaisesRegex(ValueError, 'pinned receipt'):
                planner.plan(manifest, source, questions, base/'out')
            (file.parent/'.git').mkdir()
            with self.assertRaisesRegex(ValueError, 'history-containing'):
                planner.plan(manifest, source, questions, base/'out')
            self.assertFalse((base/'out').exists())

    def test_question_edit_during_source_verification_retains_inputs_without_a_plan(self):
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            manifest, source, questions, _ = self.fixture(base)
            load = planner.load
            def changed_load(name, path):
                module = load(name, path)
                if name == 'public_plan_census':
                    census = module.census
                    def changing(*args):
                        result = census(*args)
                        questions.write_bytes(questions.read_bytes()+b'\n')
                        return result
                    module.census = changing
                return module
            with patch.object(planner, 'load', changed_load):
                with self.assertRaisesRegex(ValueError, 'input evidence changed'):
                    planner.plan(manifest, source, questions, base/'out')
            self.assertTrue(questions.read_bytes().endswith(b'\n\n'))
            self.assertFalse((base/'out').exists())

    def test_output_cannot_enter_prepared_source_or_input_bank(self):
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            manifest, source, questions, _ = self.fixture(base)
            for out in [source/'new-output', questions.parent/'new-output']:
                with self.assertRaises(ValueError):
                    planner.plan(manifest, source, questions, out)
                self.assertFalse(out.exists())

    def test_explicit_configuration_is_recorded_without_asserting_runtime_verification(self):
        config = {'model': 'example-model', 'effort': 'medium', 'client': 'example-client',
                  'client_version': '1', 'research_call_budget': 20, 'knowcode_sha256': 'b'*64,
                  'knowcode_profile': 'core'}
        planner.check_config(config)
        for change in [{'research_call_budget': True}, {'knowcode_sha256': 'not-a-hash'}, {'extra_tool': 'unrelated'}]:
            with self.assertRaises(ValueError):
                planner.check_config(config | change)
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            manifest, source, questions, _ = self.fixture(base)
            config_path = base/'config/run.json'
            config_path.parent.mkdir()
            config_path.write_text(json.dumps(config))
            result = planner.plan(manifest, source, questions, base/'out', config_path=config_path)
            self.assertEqual(result['config'], config)
            self.assertFalse(result['ready_to_run'])


if __name__ == '__main__':
    unittest.main()
