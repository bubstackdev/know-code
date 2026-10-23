"""Bare-client launch preparation is inert, source-scoped and identical in common tool privileges."""
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location('bare_client', Path(__file__).with_name('bare_client.py'))
client = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(client)


class BareClientTests(unittest.TestCase):
    def fixture(self, base, arm):
        root = base/arm
        for name in ['source/example', 'control', 'profile']:
            (root/name).mkdir(parents=True)
        question = {'id': 'public-example-001', 'question': 'Where is the example declared?',
                    'commit': {'example': 'a'*40}, 'stratum': 'definition-lookup'}
        receipt = {'source_root': 'source', 'required_client_cwd': 'control', 'empty_profile_directory': 'profile',
                   'session': {'arm': arm, 'question_id': question['id']}, 'question': question}
        (root/'session.json').write_text(json.dumps(receipt))
        binary = base/'fake-client'
        if not binary.exists():
            binary.write_text('#!/bin/sh\nexit 0\n')
            binary.chmod(0o700)
        config = {'client': 'claude-code', 'client_version': 'example-version', 'model': 'example-model',
                  'effort': 'medium', 'research_call_budget': 20, 'knowcode_profile': 'all',
                  'knowcode_sha256': client.file_hash(binary)}
        return root, binary, config

    def test_plain_preparation_disables_discovery_and_records_no_secret_or_hard_call_cap(self):
        with tempfile.TemporaryDirectory() as temp:
            root, binary, config = self.fixture(Path(temp), 'plain')
            with patch.dict(os.environ, {'ANTHROPIC_API_KEY': 'synthetic-secret-marker'}):
                result = client.prepare(root, config, binary)
            args = result['argv']
            for flag in ['--bare', '--print', '--no-session-persistence', '--disable-slash-commands', '--strict-mcp-config']:
                self.assertIn(flag, args)
            self.assertEqual(args[args.index('--setting-sources')+1], '')
            self.assertEqual(args[args.index('--tools')+1], 'Bash,Read')
            self.assertEqual(args[args.index('--permission-mode')+1], 'dontAsk')
            self.assertNotIn('--max-turns', args)
            self.assertNotIn('--max-budget-usd', args)
            self.assertFalse(result['research_call_budget_enforced'])
            self.assertFalse(result['ready_to_run'])
            self.assertNotIn('synthetic-secret-marker', json.dumps(result))
            self.assertNotIn('ANTHROPIC_API_KEY', result['env'])
            self.assertEqual(result['env']['TMPDIR'], str(root.resolve()/'control'))
            self.assertEqual(result['env']['CLAUDE_CODE_TMPDIR'], str(root.resolve()/'control/client-temp'))
            self.assertEqual(json.loads((root/'control/mcp.json').read_text()), {'mcpServers': {}})
            self.assertEqual(result['model_calls'], 0)

    def test_knowcode_receives_only_owned_mcp_paths_and_note_add_is_denied(self):
        with tempfile.TemporaryDirectory() as temp:
            root, binary, config = self.fixture(Path(temp), 'knowcode')
            index = root/'control/index/example'
            index.mkdir(parents=True)
            result = client.prepare(root, config, binary, knowcode=binary, index_repos={'example': index})
            mcp = json.loads((root/'control/mcp.json').read_text())['mcpServers']
            self.assertEqual(set(mcp), {'knowcode'})
            server = mcp['knowcode']
            self.assertEqual(server['args'], ['serve', '--tools', 'all', '--repo', str(index.resolve())])
            self.assertEqual(server['env']['KNOWCODE_UPDATE_CHECK'], '0')
            self.assertEqual(server['env']['ANTHROPIC_API_KEY'], '')
            for key in ['GIT_CONFIG_SYSTEM', 'GIT_CONFIG_GLOBAL']:
                self.assertEqual(server['env'][key], '/dev/null')
            self.assertEqual(server['env']['GIT_CONFIG_NOSYSTEM'], '1')
            self.assertEqual(server['env']['GIT_TERMINAL_PROMPT'], '0')
            for key in ['KNOWCODE_CACHE_DIR', 'KNOWCODE_CONFIG_DIR', 'KNOWCODE_DATA_DIR']:
                self.assertTrue(Path(server['env'][key]).is_relative_to(root.resolve()/'control'))
            self.assertIn('mcp__knowcode', result['argv'])
            self.assertIn('mcp__knowcode__kc_note_add', result['argv'])
            self.assertEqual(result['argv'][result['argv'].index('--tools')+1], 'Bash,Read')

    def test_unprompted_arm_gets_the_server_and_exactly_the_plain_brief(self):
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            briefs = {}
            for arm in client.ARMS:
                root, binary, config = self.fixture(base, arm)
                kwargs = {}
                if arm in client.KNOWCODE_ARMS:
                    index = root/'control/index/example'
                    index.mkdir(parents=True)
                    kwargs = {'knowcode': binary, 'index_repos': {'example': index}}
                result = client.prepare(root, config, binary, **kwargs)
                argv = result['argv']
                briefs[arm] = argv[argv.index('--print')+1].replace(str(root.resolve()), 'ROOT')
                servers = json.loads((root/'control/mcp.json').read_text())['mcpServers']
                self.assertEqual(set(servers), {'knowcode'} if arm in client.KNOWCODE_ARMS else set())
                self.assertEqual('mcp__knowcode__kc_note_add' in argv, arm in client.KNOWCODE_ARMS)
            self.assertEqual(briefs['knowcode-unprompted'], briefs['plain'])
            self.assertNotIn('KnowCode', briefs['plain'])
            self.assertIn('KnowCode MCP tools', briefs['knowcode'])

    def test_plain_arm_cannot_be_given_an_mcp_binary(self):
        with tempfile.TemporaryDirectory() as temp:
            root, binary, config = self.fixture(Path(temp), 'plain')
            with self.assertRaisesRegex(ValueError, 'plain arm'):
                client.prepare(root, config, binary, knowcode=binary)
            self.assertFalse((root/'control/mcp.json').exists())

    def test_wrong_binary_and_escaping_or_symlinked_index_roots_are_rejected(self):
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            root, binary, config = self.fixture(base, 'knowcode')
            outside = base/'outside/example'
            outside.mkdir(parents=True)
            with self.assertRaisesRegex(ValueError, 'hash differs'):
                client.prepare(root, config | {'knowcode_sha256': '0'*64}, binary, knowcode=binary, index_repos={'example': outside})
            (root/'control/index').symlink_to(outside.parent, target_is_directory=True)
            with self.assertRaisesRegex(ValueError, 'real isolated directory'):
                client.prepare(root, config, binary, knowcode=binary, index_repos={'example': outside})
            self.assertFalse((root/'control/mcp.json').exists())

    def test_existing_configuration_is_preserved_and_invalid_budget_does_not_write(self):
        with tempfile.TemporaryDirectory() as temp:
            root, binary, config = self.fixture(Path(temp), 'plain')
            with self.assertRaisesRegex(ValueError, 'positive common'):
                client.prepare(root, config | {'research_call_budget': True}, binary)
            self.assertFalse((root/'control/client-settings.json').exists())
            (root/'control/client-settings.json').write_text('retain')
            with self.assertRaisesRegex(ValueError, 'existing client configuration'):
                client.prepare(root, config, binary)
            self.assertEqual((root/'control/client-settings.json').read_text(), 'retain')

    def test_two_hop_answer_contract_is_common_to_both_arms(self):
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            for arm in ['plain', 'knowcode']:
                root, binary, config = self.fixture(base, arm)
                receipt = json.loads((root/'session.json').read_text())
                receipt['question']['stratum'] = 'two-hop-impact'
                (root/'session.json').write_text(json.dumps(receipt))
                kwargs = {}
                if arm == 'knowcode':
                    index = root/'control/index/example'
                    index.mkdir(parents=True)
                    kwargs = {'knowcode': binary, 'index_repos': {'example': index}}
                result = client.prepare(root, config, binary, **kwargs)
                prompt = result['argv'][result['argv'].index('--print')+1]
                self.assertIn('"1": [source locations], "2": [source locations]', prompt)
                self.assertIn('items is their union', prompt)


    def test_question_repository_traversal_cannot_enter_the_prompt(self):
        with tempfile.TemporaryDirectory() as temp:
            root, binary, config = self.fixture(Path(temp), 'plain')
            receipt = json.loads((root/'session.json').read_text())
            receipt['question']['commit'] = {'../labels': 'a'*40}
            (root/'session.json').write_text(json.dumps(receipt))
            with self.assertRaisesRegex(ValueError, 'unsafe question repository'):
                client.prepare(root, config, binary)
            self.assertFalse((root/'control/client-settings.json').exists())


    def test_both_arms_keep_the_full_cohort_not_only_question_repositories(self):
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            for arm in ['plain', 'knowcode']:
                root, binary, config = self.fixture(base, arm)
                (root/'source/other-client').mkdir()
                kwargs = {}
                if arm == 'knowcode':
                    index = root/'control/index/example'
                    index.mkdir(parents=True)
                    with self.assertRaisesRegex(ValueError, 'full source cohort'):
                        client.prepare(root, config, binary, knowcode=binary, index_repos={'example': index})
                    other = root/'control/index/other-client'
                    other.mkdir()
                    kwargs = {'knowcode': binary, 'index_repos': {'example': index, 'other-client': other}}
                result = client.prepare(root, config, binary, **kwargs)
                prompt = result['argv'][result['argv'].index('--print')+1]
                self.assertIn('other-client:', prompt)
                if arm == 'knowcode':
                    server = json.loads((root/'control/mcp.json').read_text())['mcpServers']['knowcode']
                    self.assertIn(str(other.resolve()), server['args'])

    def test_shell_permissions_are_the_same_in_every_arm_in_both_modes(self):
        def bash_rules(argv):
            start = argv.index('--allowedTools')+1
            end = argv.index('--disallowedTools') if '--disallowedTools' in argv else len(argv)
            return [a for a in argv[start:end] if a.startswith('Bash')]
        for mode, expected in [(None, None), ('patterns', None), ('sandbox', ['Bash'])]:
            with tempfile.TemporaryDirectory() as temp:
                base, seen = Path(temp), []
                for arm in client.ARMS:
                    root, binary, config = self.fixture(base, arm)
                    if mode:
                        config['shell'] = mode
                    if mode == 'sandbox':
                        config['knowcode_binary_name'] = binary.name
                    kwargs = {}
                    if arm != 'plain':
                        index = root/'control/index/example'
                        index.mkdir(parents=True)
                        kwargs = {'knowcode': binary, 'index_repos': {'example': index}}
                    result = client.prepare(root, config, binary, **kwargs)
                    self.assertEqual(result['shell'], mode or 'patterns')
                    self.assertIn('Read(/'+str(root.resolve()/'source')+'/**)', result['argv'])
                    seen.append(bash_rules(result['argv']))
                    argv = result['argv']
                    denied = argv[argv.index('--disallowedTools')+1:] if '--disallowedTools' in argv else []
                    # In sandbox mode every arm refuses Bash commands naming the KnowCode binary.
                    self.assertEqual('Bash(*fake-client*)' in denied, mode == 'sandbox')
                self.assertTrue(all(rules == seen[0] for rules in seen))
                if expected:
                    self.assertEqual(seen[0], expected)
                else:
                    self.assertIn('Bash(grep *)', seen[0])
                    self.assertNotIn('Bash', seen[0])
        with tempfile.TemporaryDirectory() as temp:
            root, binary, config = self.fixture(Path(temp), 'plain')
            config['shell'] = 'anything'
            with self.assertRaisesRegex(ValueError, 'shell permission mode'):
                client.prepare(root, config, binary)
            config['shell'] = 'sandbox'
            with self.assertRaisesRegex(ValueError, 'distinctive knowcode_binary_name'):
                client.prepare(root, config, binary)
            # A name that occurs in the session's own paths would make Bash refuse every source path.
            config['knowcode_binary_name'] = Path(temp).name
            with self.assertRaisesRegex(ValueError, 'session paths contain'):
                client.prepare(root, config, binary)


if __name__ == '__main__':
    unittest.main()
