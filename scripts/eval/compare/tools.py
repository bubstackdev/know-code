"""How each tool is started and made to index one repository, in compare-v1's container (eval/compare-v1/README.md,
"Indexing"). Each tool keeps its state under its own directory in STATE, and nothing of one tool is visible to another.
"""
from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess

from compare import mcp

TOOLS = ('knowcode', 'codebase-memory-mcp', 'serena')
# Serena's language for each of compare-v1's languages (its `--language`; docs/01-about/020_programming-languages.md).
SERENA_LANGUAGE = {'csharp': 'csharp', 'typescript': 'typescript', 'swift': 'swift', 'go': 'go', 'java': 'java',
                   'kotlin': 'kotlin'}
INDEX_TIMEOUT = 3600


def environment(tool, state):
    """The tool's environment: a home, cache and config of its own under `state`."""
    home = Path(state)/tool
    home.mkdir(parents=True, exist_ok=True)
    env = {k: v for k, v in os.environ.items() if k in ('PATH', 'LANG', 'LC_ALL', 'TERM')}
    env.update(HOME=str(home), XDG_CACHE_HOME=str(home/'cache'), XDG_CONFIG_HOME=str(home/'config'),
               XDG_DATA_HOME=str(home/'data'))
    if tool == 'knowcode':
        env.update(KNOWCODE_CACHE_DIR=str(home/'kc-cache'), KNOWCODE_CONFIG_DIR=str(home/'kc-config'),
                   KNOWCODE_DATA_DIR=str(home/'kc-data'), KNOWCODE_UPDATE_CHECK='0')
    elif tool == 'codebase-memory-mcp':
        env.update(CBM_CACHE_DIR=str(home/'cbm-cache'))
    return env


class Session:
    """One tool's MCP session on one repository, after the tool has indexed it as its documentation says."""

    def __init__(self, tool, repo, language, state, logs, setup=True, project=None):
        """With `setup`, the tool indexes the repository (the network may be up); without, the session reopens what an
        earlier session indexed, as a later session of a user's would."""
        self.tool, self.repo, self.language = tool, str(repo), language
        self.env = environment(tool, state)
        logs = Path(logs)
        logs.mkdir(parents=True, exist_ok=True)
        stem = f'{Path(repo).name}.{tool}'
        self.stderr = open(logs/f'{stem}.stderr', 'ab')
        self.log = open(logs/f'{stem}.mcp.jsonl', 'a')
        self.project = project  # codebase-memory-mcp's name for the repository, from the indexing session
        self.setup_log = logs/f'{stem}.setup.log'
        if not setup:
            pass
        elif tool == 'codebase-memory-mcp':
            # The tree doesn't change during a run, so the watcher has nothing to do (README.md, "Configuration").
            self._run(['codebase-memory-mcp', 'config', 'set', 'watcher_enabled', 'false'])
            self._run(['codebase-memory-mcp', 'config', 'set', 'auto_index', 'false'])
        elif tool == 'knowcode':
            # `knowcode index` indexes the repository now (docs/usage.md, "Index repos now"). A query alone waits
            # about two seconds for indexing and leaves the rest to a background worker.
            self._run(['knowcode', 'index', self.repo], timeout=INDEX_TIMEOUT)
        elif tool == 'serena':
            self._run(['serena', 'project', 'create', '--language', SERENA_LANGUAGE[language], '--index', self.repo],
                      timeout=INDEX_TIMEOUT)
        self.client = mcp.Client(self.command(), env=self.env, cwd=self.repo, stderr=self.stderr, log=self.log,
                                 timeout=INDEX_TIMEOUT if tool == 'serena' else 120)
        if setup:
            self.index()
        elif tool == 'knowcode':
            self.index()  # status: a sync that finds nothing changed

    def command(self):
        if self.tool == 'knowcode':
            return ['knowcode', 'serve', '--tools', 'all']
        if self.tool == 'codebase-memory-mcp':
            return ['codebase-memory-mcp']
        # The classical tool interface (docs/02-usage/050_configuration.md, "Agent Interfaces"): one MCP tool per
        # operation, the calls the mapping names. The REPL interface, v2's default, runs the same operations (LspApi).
        return ['serena', 'start-mcp-server', '--project', self.repo, '--agent-interface', 'tools']

    def index(self):
        if self.tool == 'knowcode':
            self.client.call('kc_status', {}, timeout=INDEX_TIMEOUT)
        elif self.tool == 'codebase-memory-mcp':
            text, structured = self.client.call('index_repository', {'repo_path': self.repo}, timeout=INDEX_TIMEOUT)
            self.project = project_name(text, structured)

    def call(self, tool, args, timeout=60):
        return self.client.call(tool, args, timeout=timeout)

    def close(self):
        self.client.close()
        self.stderr.close()
        self.log.close()

    def _run(self, command, timeout=600):
        with open(self.setup_log, 'ab') as out:
            out.write(f'$ {" ".join(command)}\n'.encode())
            out.flush()
            subprocess.run(command, env=self.env, cwd=self.repo, stdout=out, stderr=out, timeout=timeout, check=True,
                           stdin=subprocess.DEVNULL)


def project_name(text, structured):
    """The project name codebase-memory-mcp's index_repository answers with."""
    for source in (structured, _json(text)):
        if isinstance(source, dict):
            for key in ('project', 'name', 'project_name'):
                if isinstance(source.get(key), str):
                    return source[key]
    return None


def _json(text):
    try:
        return json.loads(text)
    except (TypeError, ValueError):
        return None
