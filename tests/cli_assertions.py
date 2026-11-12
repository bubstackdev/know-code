"""Shared subprocess assertion for CLI JSON failure status and native clients."""
import json

def check_exit(result, argv, check=True):
    if not check:
        return
    args = list(map(str, argv))
    if len(args) > 1 and args[1] in ['query', 'queries']:
        value = json.loads(result.stdout)
        expected = 1 if 'error' in value else 0
        assert result.returncode == expected, (args, result.returncode, result.stdout, result.stderr)
    else:
        assert result.returncode == 0, (args, result.returncode, result.stdout, result.stderr)
