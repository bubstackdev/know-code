"""Probes each tool on the made-up fixture (fixture/README.md), before compare-v1's mapping is frozen: what each tool
lists, and the raw answer of each call the mapping names, so the adapters can be written against real output.

    compare-python -m compare.probe LANGUAGE OUT        # in the container, with scripts/eval on PYTHONPATH

Writes OUT/probe-LANGUAGE-TOOL.json. Nothing here is scored, and no repository of the set is read.
"""
from __future__ import annotations

import json
from pathlib import Path
import shutil
import sys
import traceback

from compare import tools

HERE = Path(__file__).resolve().parent
# Per language: the fixture's `weigh` and `gather` declarations, as a question names them (file, line, name).
TARGETS = {
    'typescript': {'weigh': ('src/basket.ts', 4, 'weigh'), 'gather': ('src/harvest.ts', 4, 'gather'),
                   'Basket': ('src/basket.ts', 1, 'Basket')},
    'csharp': {'weigh': ('Orchard/Basket.cs', 7, 'Weigh'), 'gather': ('Orchard/Harvest.cs', 5, 'Gather'),
               'Basket': ('Orchard/Basket.cs', 3, 'Basket')},
    'swift': {'weigh': ('Sources/Orchard/Basket.swift', 6, 'weigh'),
              'gather': ('Sources/Orchard/Harvest.swift', 1, 'gather'),
              'Basket': ('Sources/Orchard/Basket.swift', 1, 'Basket')},
}


def calls(tool, session, language):
    """The probe's calls for one tool: every call the mapping names, on the fixture's targets."""
    t = TARGETS[language]
    if tool == 'knowcode':
        out = []
        for name, (f, line, _) in t.items():
            out += [('kc_refs', {'name': f'{f}:{line}'}), ('kc_impact', {'symbol': f'{f}:{line}', 'depth': 1}),
                    ('kc_impact', {'symbol': f'{f}:{line}', 'depth': 2})]
        return out + [('kc_find', {'query': t['weigh'][2]}), ('kc_locate', {'query': 'weigh the basket'}),
                      ('kc_task', {'goal': 'weigh the basket wrongly'}), ('kc_coupled', {'file': t['weigh'][0]})]
    if tool == 'codebase-memory-mcp':
        p = session.project
        out = [('list_projects', {})]
        for name, (f, line, n) in t.items():
            out += [('search_graph', {'project': p, 'name_pattern': f'^{n}$', 'format': 'json', 'limit': 50}),
                    ('search_graph', {'project': p, 'name_pattern': f'^{n}$', 'file_pattern': f, 'format': 'json'})]
        for n in ('weigh', 'gather', 'Basket'):
            name = t[n][2]
            out += [('trace_path', {'project': p, 'function_name': name, 'direction': 'inbound', 'depth': 1,
                                    'edge_types': ['CALLS', 'CALL_REFERENCE', 'USAGE'], 'include_tests': True,
                                    'include_evidence': True, 'format': 'json'}),
                    ('trace_path', {'project': p, 'function_name': name, 'direction': 'inbound', 'depth': 2,
                                    'include_tests': True, 'format': 'json'})]
        return out + [('search_graph', {'project': p, 'semantic_query': ['weigh the basket'], 'format': 'json'}),
                      ('query_graph', {'project': p, 'query': 'MATCH (a:File)-[r:FILE_CHANGES_WITH]-(b:File) '
                                       f"WHERE a.file_path = '{t['weigh'][0]}' RETURN b.file_path", 'format': 'json'})]
    out = []
    for name, (f, line, n) in t.items():
        out += [('find_symbol', {'name_path_pattern': n, 'relative_path': f, 'depth': 0}),
                ('find_symbol', {'name_path_pattern': n})]
    return out + [('find_referencing_symbols', {'name_path': 'Basket/weigh' if language != 'csharp' else 'Basket/Weigh',
                                                'relative_path': t['weigh'][0]}),
                  ('find_referencing_symbols', {'name_path': t['gather'][2], 'relative_path': t['gather'][0]}),
                  ('find_referencing_symbols', {'name_path': 'Basket', 'relative_path': t['Basket'][0]})]


def main(argv=None):
    language, out = (argv or sys.argv[1:])[:2]
    out = Path(out)
    out.mkdir(parents=True, exist_ok=True)
    for tool in tools.TOOLS:
        repo = Path('/work/probe')/tool/language
        if not repo.exists():  # container.sh copies and restores it first
            shutil.copytree(HERE/'fixture'/language, repo)
        record = {'tool': tool, 'language': language, 'calls': []}
        try:
            session = tools.Session(tool, repo, language, '/work/state', out/'logs')
            record.update(server=session.client.server, instructions=session.client.instructions,
                          tools=session.client.tools, project=session.project)
            for name, args in calls(tool, session, language):
                try:
                    text, structured = session.call(name, args, timeout=300)
                    record['calls'].append({'tool': name, 'args': args, 'text': text, 'structured': structured})
                except Exception as e:  # noqa: BLE001 - every failure is the probe's finding
                    record['calls'].append({'tool': name, 'args': args, 'error': str(e)})
            session.close()
        except Exception:  # noqa: BLE001
            record['setup_error'] = traceback.format_exc()
        (out/f'probe-{language}-{tool}.json').write_text(json.dumps(record, indent=1))
        print(tool, language, 'setup_error' in record, len(record['calls']))


if __name__ == '__main__':
    main()
