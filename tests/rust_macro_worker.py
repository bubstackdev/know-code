#!/usr/bin/env python3
"""Owned actual procedural-macro replay/binding proof; shared build owner runs."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
ARTIFACTS = ROOT / '.artifacts/rust-macro-worker' / str(time.time_ns())
WORKSPACE, MACROS = ARTIFACTS / 'fixture', ARTIFACTS / 'macro-api'
(WORKSPACE / 'src').mkdir(parents=True)
(MACROS / 'src').mkdir(parents=True)
(MACROS / 'Cargo.toml').write_text('[package]\nname="owned-macro-api"\nversion="0.1.0"\nedition="2024"\n'
                                    '[lib]\nproc-macro=true\n[workspace]\n')
(MACROS / 'src/lib.rs').write_text('extern crate proc_macro;\n// 🦀 owned compiler tool source\n'
    '#[proc_macro] pub fn choose(_: proc_macro::TokenStream) -> proc_macro::TokenStream {\n'
    ' "use crate::macro_selected as selected; fn generated_one() -> usize { 3 } fn generated_two() -> usize { 4 }".parse().unwrap()\n}\n'
    '#[proc_macro_attribute] pub fn passthrough(_: proc_macro::TokenStream, item: proc_macro::TokenStream) -> proc_macro::TokenStream { item }\n'
    '#[proc_macro_derive(Made)] pub fn made(_: proc_macro::TokenStream) -> proc_macro::TokenStream {\n'
    ' "impl Derived { pub fn generated_method(&self) -> usize { 9 } }".parse().unwrap()\n}\n')
(WORKSPACE / 'Cargo.toml').write_text('[package]\nname="owned-macro-consumer"\nversion="0.1.0"\nedition="2024"\n[workspace]\n'
    '[dependencies]\nmacro-api={package="owned-macro-api",path=' + json.dumps(str(MACROS)) + '}\n')
(WORKSPACE / 'src/lib.rs').write_text(
    'pub fn selected() -> usize { 1 }\npub fn macro_selected() -> usize { 2 }\npub fn ordinary_leaf() -> usize { 5 }\n'
    '#[macro_api::passthrough] pub fn ordinary() -> usize { selected() }\n'
    'pub fn entry() -> usize { macro_api::choose!(); selected() + ordinary_leaf() + generated_one() + generated_two() }\n'
    '#[derive(macro_api::Made)] pub struct Derived;\npub fn derived_use() -> usize { Derived.generated_method() }\n')
NODE = shutil.which('node')
assert NODE
sysroot = subprocess.run(['rustc', '--print', 'sysroot'], cwd=WORKSPACE, text=True, capture_output=True, check=True).stdout.strip()
compiler = Path(sysroot) / 'bin/rustc'
assert compiler.is_file()
(WORKSPACE / '.cargo').mkdir()
(WORKSPACE / '.cargo/config.toml').write_text('[build]\nrustc=' + json.dumps(str(compiler)) + '\n')


def analyze(label, env=None, previous=()):
    request = {'workspaceRoot': str(WORKSPACE), 'projects': [str(WORKSPACE / 'Cargo.toml')],
               'configuration': 'Debug', 'preparationFingerprint': 'owned-macro-proof-v1', 'preparations': list(previous),
               'analysisPolicy': {'version': 1, 'rust': {'executionAllowed': True, 'cacheDirectory': str(ARTIFACTS / 'cache')}}}
    result = subprocess.run([NODE, str(ROOT / 'tests/worker_harness.mjs'), str(ROOT / 'providers/rust/worker.mjs')],
                            input=json.dumps(request), cwd=ROOT, text=True, capture_output=True, timeout=190, check=True, env=env)
    payload = json.loads(result.stdout)
    (ARTIFACTS / f'{label}.json').write_text(json.dumps(payload, indent=2))
    assert payload['code'] == 0, payload
    assert any(frame.get('complete', {}).get('success') for frame in payload['messages'])
    return payload


def validate(payload):
    contexts = {frame['compilation']['id']: json.loads(frame['compilation']['optionsJson'])
                for frame in payload['messages'] if 'compilation' in frame
                and isinstance(json.loads(frame['compilation']['optionsJson']).get('context'), dict)}
    assert len(contexts) == 2, contexts
    for identity, options in contexts.items():
        assert options['rustContextPolicy'] == 1 and options['analyzerReplay']['procMacros'] is True
        dylibs = options['analyzerReplay']['macroDylibs']
        assert len(dylibs) == 1 and len(dylibs[0]['sha256']) == 64 and Path(dylibs[0]['path']).is_file()
        symbols = {symbol['id']: symbol for frame in payload['messages'] if frame.get('facts', {}).get('compilationId') == identity
                   for symbol in frame['facts']['symbols']}
        edges = [edge for frame in payload['messages'] if frame.get('facts', {}).get('compilationId') == identity
                 for edge in frame['facts']['relationships']]
        assert symbols, [frame for frame in payload['messages'] if 'report' in frame]
        def authored(name):
            matches = [key for key, symbol in symbols.items() if symbol['name'] == name and not symbol.get('external')]
            assert len(matches) == 1, (name, matches)
            return matches[0]
        def calls(source, target):
            return any(edge['kind'] == 'call' and edge['sourceId'] == authored(source) and edge['targetId'] == authored(target) for edge in edges)
        assert calls('entry', 'macro_selected'), 'macro-induced import/shadowing did not change the binding'
        assert calls('entry', 'ordinary_leaf'), 'ordinary authored body call was lost'
        assert not calls('entry', 'selected'), 'macro-disabled fallback binding was published'
        assert calls('ordinary', 'selected'), 'attribute replay lost the unchanged authored body'
        assert not {'generated_one', 'generated_two', 'generated_method'} & {symbol['name'] for symbol in symbols.values()}, 'unproven expansion identity was published'
        assert not any(symbol['name'] in ('choose', 'made') and symbol['kind'] in ('function', 'method') for symbol in symbols.values()), 'macro implementation function became a runtime call target'
        owned_sources = [frame['source']['path'] for frame in payload['messages'] if frame.get('source', {}).get('compilationId') == identity]
        assert owned_sources == ['src/lib.rs'], owned_sources
        report = next(frame['report'] for frame in payload['messages'] if frame.get('report', {}).get('compilationId') == identity)
        assert any(item['code'] == 'rust_unproven_definition_origin' for item in report['diagnostics']), report


cold = analyze('cold')
validate(cold)
preparations = [frame['preparation'] for frame in cold['messages'] if 'preparation' in frame]

# The selected Cargo compiler remains valid even if an unrelated PATH rustc
# cannot supply a sysroot. No toolchain/compiler installation is performed.
bin_directory = ARTIFACTS / 'bad-path'
bin_directory.mkdir()
marker = ARTIFACTS / 'path-rustc-used'
shim = bin_directory / 'rustc'
shim.write_text(f'#!{sys.executable}\nfrom pathlib import Path\nPath({str(marker)!r}).write_text("wrong PATH compiler")\nraise SystemExit(91)\n')
shim.chmod(0o755)
selected = analyze('cargo-selected-compiler', dict(os.environ, PATH=str(bin_directory) + os.pathsep + os.environ['PATH']), preparations)
validate(selected)
assert not marker.exists(), 'PATH rustc overrode the actual Cargo compiler/sysroot'
(ARTIFACTS / 'summary.json').write_text(json.dumps({'passed': True, 'assertions': [
    'exact captured macro dylib replay in both library contexts', 'macro-induced binding/shadowing change',
    'ordinary and attribute-preserved body calls', 'unproven generated declarations omitted',
    'macro implementation is not a runtime call target', 'dependency source remains lookup-only',
    'Cargo-selected compiler/sysroot independent of unrelated PATH rustc'],
    'limits': ['no expanded-source identity promises', 'arbitrary macro runtime file/environment input tracking remains partial']}, indent=2))
print(ARTIFACTS)
