"""CLI information/argument behavior and structured failure exit status."""
from pathlib import Path
import json, os, subprocess, time

ROOT = Path(__file__).resolve().parents[1]
PROFILE = os.environ.get('KNOW_CODE_BUILD_PROFILE', 'debug')
HOST = ROOT / 'target' / PROFILE / 'know-code'
RUN = ROOT / '.artifacts' / 'cli' / format(time.time_ns(), 'x')
RUN.mkdir(parents=True)
ENV = dict(os.environ, KNOW_CODE_DATA_DIR=str(RUN / 'd'), KNOW_CODE_QUERY_SCOPE=str(RUN / 'scope'))
checks = []

def cli(*args, env=ENV):
    result = subprocess.run([str(HOST), *map(str, args)], env=env, cwd=ROOT, text=True, capture_output=True, timeout=15)
    with (RUN / 'commands.jsonl').open('a') as out:
        out.write(json.dumps({'args': args, 'returncode': result.returncode, 'stdout': result.stdout, 'stderr': result.stderr}, default=str) + '\n')
    return result

no_home = {k: v for k, v in ENV.items() if k not in ['HOME', 'KNOW_CODE_DATA_DIR', 'KNOW_CODE_QUERY_SCOPE']}
version = cli('--version', env=no_home)
assert version.returncode == 0 and version.stdout.startswith('know-code ') and not version.stderr
for flag in ['--help', '-h']:
    help_result = cli(flag, env=no_home)
    assert help_result.returncode == 0 and not help_result.stderr
    assert all(command in help_result.stdout for command in ['parents', 'queries', 'cursors', 'unwatch', 'runtime-info', 'service'])
assert cli('--version', 'extra', env=no_home).returncode != 0
checks.append('information_commands_do_not_require_HOME_or_create_account_state')

for args, flag in [
    (['queries', '--max-active', 'oops'], '--max-active'),
    (['cursors', '--max-count', '-1'], '--max-count'),
    (['cursors', '--retention-bytes', 'lots'], '--retention-bytes'),
    (['resources', '--cpu', 'many'], '--cpu'),
    (['resources', '--memory-mib', 'huge'], '--memory-mib'),
    (['resources', '--min-free-disk-mib', 'huge'], '--min-free-disk-mib'),
]:
    result = cli(*args)
    assert result.returncode != 0 and flag in result.stderr and not result.stdout, result
checks.append('numeric_parse_errors_identify_the_flag')

for args in [
    ['queries', '--max-active', '2', '--max-active', '3'],
    ['queries', '--scope', 'one', '--scope', 'two'],
    ['cursors', '--max-count', '2', '--max-count', '3'],
    ['resources', '--cpu', '1', '--cpu', '2'],
    ['resources', '--rebuild-disk-mib', '512', '--temporary-disk-mib', '1024'],
]:
    result = cli(*args)
    assert result.returncode != 0 and 'more than once' in result.stderr and not result.stdout, result
checks.append('duplicate_options_and_aliases_are_rejected_before_execution')

for args, code in [
    (['query', str(RUN / 'missing.db'), '{"op":"symbols"}'], 'not_ready'),
    (['query', str(RUN / 'missing.db'), '{'], 'invalid_request'),
    (['queries', '--max-active', '0'], 'invalid_request'),
]:
    result = cli(*args)
    assert result.returncode == 1 and json.loads(result.stdout)['error']['code'] == code and not result.stderr, result
assert not list(RUN.rglob('c.sock')), 'CLI error handling started a coordinator'
checks.append('structured_errors_keep_JSON_stdout_and_return_nonzero_without_starting_indexing')

(RUN / 'validation.json').write_text(json.dumps({'passed': True, 'checks': checks, 'profile': PROFILE}, indent=2) + '\n')
print(json.dumps({'passed': len(checks), 'artifacts': str(RUN)}))
