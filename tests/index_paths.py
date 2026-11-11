"""Resolve private index files for test/benchmark inspection.

Public/native queries must continue to receive the logical database handle.
This helper takes no reader lease: use it only with owned, settled fixtures.
"""
import json
from pathlib import Path


def physical_database(database: str | Path) -> Path:
    logical = Path(database)
    selector = logical.with_suffix('.lifecycle.json')
    if selector.exists():
        state = json.loads(selector.read_text())
        if state['version'] != 1 or state.get('initializing', False):
            raise ValueError(f'index lifecycle is not ready for inspection: {selector}')
        selected = state['selected']
        incarnation = selected['id']
        if len(incarnation) != 32 or any(c not in '0123456789abcdef' for c in incarnation):
            raise ValueError(f'invalid selected index incarnation: {selector}')
        physical = logical if selected['legacy'] else logical.parent / 'indexes' / incarnation / 'index.db'
    else:
        physical = logical
    if not physical.is_file():
        raise FileNotFoundError(f'index database is not ready for inspection: {physical}')
    return physical
