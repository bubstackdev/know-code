"""Explicit execution grants for owned durable integration fixtures only."""
from pathlib import Path
import json
import subprocess
import time


def grant_fixture(host, workspace, env, owned_root, log, *, runner=None):
    """Use the real CLI; never manufacture the private trust JSON/identity.

    A runner keeps package tests inside their existing network/checkout sandbox.
    This does not register the workspace or grant package installation scripts.
    """
    owned = Path(owned_root).resolve(strict=True)
    workspace = Path(workspace).resolve(strict=True)
    assert '.artifacts' in owned.parts and owned.is_dir(), ('not a durable owned fixture root', owned)
    assert workspace.is_dir() and workspace.is_relative_to(owned), ('fixture workspace escapes ownership', workspace, owned)
    data_value = env.get('KNOW_CODE_DATA_DIR')
    scope_value = env.get('KNOW_CODE_QUERY_SCOPE')
    assert data_value and scope_value, 'fixture trust needs isolated data and query scopes'
    data, scope = Path(data_value).resolve(), Path(scope_value).resolve()
    assert data != owned and data.is_relative_to(owned) and not data.is_relative_to(workspace), ('fixture data escapes ownership or overlaps source', data, owned)
    assert scope != owned and scope.is_relative_to(owned), ('fixture admission scope escapes ownership', scope, owned)
    log = Path(log).resolve()
    assert log.is_relative_to(owned), ('fixture trust log escapes ownership', log, owned)
    argv = [str(Path(host).resolve(strict=True)), 'trust', str(workspace)]
    started = time.time_ns()
    process = runner(argv) if runner else subprocess.run(argv, cwd=owned, env=env, text=True, capture_output=True, timeout=30)
    log.parent.mkdir(parents=True, exist_ok=True)
    with log.open('a') as stream:
        stream.write(json.dumps({'operation':'fixture_trust','argv':argv,'workspace':str(workspace),'owned_root':str(owned),
                                 'data_directory':str(data),'query_scope':str(scope),'started_ns':started,'finished_ns':time.time_ns(),
                                 'returncode':process.returncode,'stdout':process.stdout,'stderr':process.stderr})+'\n')
    assert process.returncode == 0, ('fixture trust failed',argv,process.returncode,process.stdout,process.stderr)
    if process.stdout.strip():
        result = json.loads(process.stdout)
        assert not result.get('error_code') and not isinstance(result.get('error'), dict), ('fixture trust rejected',result)
    return process
