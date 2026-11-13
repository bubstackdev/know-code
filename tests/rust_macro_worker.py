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
    ' let body = if std::env::var("KNOW_CODE_OWNED_MACRO_ENV").as_deref() == Ok("cargo-selected") {\n'
    ' "use crate::macro_selected as selected; fn generated_one() -> usize { 3 } fn generated_two() -> usize { 4 }"\n'
    ' } else { "use crate::selected as selected; fn generated_one() -> usize { 3 } fn generated_two() -> usize { 4 }" };\n'
    ' body.parse().unwrap()\n}\n'
    '#[proc_macro_attribute] pub fn passthrough(_: proc_macro::TokenStream, item: proc_macro::TokenStream) -> proc_macro::TokenStream { item }\n'
    '#[proc_macro_derive(Made)] pub fn made(_: proc_macro::TokenStream) -> proc_macro::TokenStream {\n'
    ' "impl Derived { pub fn generated_method(&self) -> usize { 9 } }".parse().unwrap()\n}\n'
    '#[proc_macro_attribute] pub fn rename(_: proc_macro::TokenStream, item: proc_macro::TokenStream) -> proc_macro::TokenStream {\n'
    ' item.into_iter().map(|token| match token { proc_macro::TokenTree::Ident(name) if name.to_string() == "old_name" => '
    'proc_macro::TokenTree::Ident(proc_macro::Ident::new("new_name", name.span())), other => other }).collect()\n}\n'
    'fn rename_shape_tokens(item: proc_macro::TokenStream) -> proc_macro::TokenStream {\n'
    ' item.into_iter().map(|token| match token {\n'
    ' proc_macro::TokenTree::Ident(name) if name.to_string() == "OldShape" => proc_macro::TokenTree::Ident(proc_macro::Ident::new("NewShape", name.span())),\n'
    ' proc_macro::TokenTree::Ident(name) if name.to_string() == "old_field" => proc_macro::TokenTree::Ident(proc_macro::Ident::new("new_field", name.span())),\n'
    ' proc_macro::TokenTree::Group(group) => { let mut replacement = proc_macro::Group::new(group.delimiter(), rename_shape_tokens(group.stream())); replacement.set_span(group.span()); proc_macro::TokenTree::Group(replacement) },\n'
    ' other => other }).collect()\n}\n'
    '#[proc_macro_attribute] pub fn rename_shape(_: proc_macro::TokenStream, item: proc_macro::TokenStream) -> proc_macro::TokenStream { rename_shape_tokens(item) }\n')
(WORKSPACE / 'Cargo.toml').write_text('[package]\nname="owned-macro-consumer"\nversion="0.1.0"\nedition="2024"\n[workspace]\n'
    '[dependencies]\nmacro-api={package="owned-macro-api",path=' + json.dumps(str(MACROS)) + '}\n')
(WORKSPACE / 'src/lib.rs').write_text(
    'pub fn selected() -> usize { 1 }\npub fn macro_selected() -> usize { 2 }\npub fn ordinary_leaf() -> usize { 5 }\n'
    '#[macro_api::passthrough] pub fn ordinary() -> usize { selected() }\n'
    'pub fn entry() -> usize { macro_api::choose!(); selected() + ordinary_leaf() + generated_one() + generated_two() }\n'
    '#[derive(macro_api::Made)] pub struct Derived;\npub fn derived_use() -> usize { Derived.generated_method() }\n'
    '#[macro_api::rename] pub fn old_name() -> usize { ordinary_leaf() }\npub fn renamed_use() -> usize { new_name() }\n'
    'use crate::ordinary_leaf as alias_leaf;\npub fn aliased() -> usize { alias_leaf() }\n'
    '#[macro_api::rename_shape] pub struct OldShape { pub old_field: usize }\n'
    'pub fn renamed_shape_use() -> usize { let value = NewShape { new_field: 1 }; value.new_field }\n'
    'pub struct PlainShape { pub plain_field: usize }\n'
    'pub fn plain_shape_use() -> usize { let value = PlainShape { plain_field: 2 }; value.plain_field }\n'
    '#[macro_api::passthrough] pub mod attributed { pub struct AttributedShape { pub attributed_field: usize } pub fn nested_leaf() -> usize { crate::ordinary_leaf() } }\n'
    'pub fn nested_call() -> usize { attributed::nested_leaf() }\n'
    '#[path="risk/mod.rs"] pub mod risk;\nmod plain;\n'
    'pub fn risk_call() -> usize { risk::risk_leaf() }\n')
(WORKSPACE / 'src/risk').mkdir()
(WORKSPACE / 'src/risk/mod.rs').write_text('pub mod nested;\npub struct RiskShape { pub risk_field: usize }\npub fn risk_leaf() -> usize { crate::ordinary_leaf() }\n')
(WORKSPACE / 'src/risk/nested.rs').write_text('pub struct DeepRiskShape { pub deep_risk_field: usize }\n')
(WORKSPACE / 'src/plain.rs').write_text('pub struct PlainNestedShape { pub plain_nested_field: usize }\n')
NODE = shutil.which('node')
assert NODE
sysroot = subprocess.run(['rustc', '--print', 'sysroot'], cwd=WORKSPACE, text=True, capture_output=True, check=True).stdout.strip()
compiler = Path(sysroot) / 'bin/rustc'
assert compiler.is_file()
real_analyzer = Path(sysroot) / 'bin/rust-analyzer'
assert real_analyzer.is_file(), 'repository-selected rust-analyzer component is required'
(WORKSPACE / '.cargo').mkdir()
(WORKSPACE / '.cargo/config.toml').write_text('[build]\nrustc=' + json.dumps(str(compiler)) + '\n[env]\nKNOW_CODE_OWNED_MACRO_ENV={value="cargo-selected",force=true}\n')


def analyzer_proxy(label, invalid=''):
    directory = ARTIFACTS / label
    directory.mkdir()
    log = ARTIFACTS / f'{label}-lsp.jsonl'
    executable = directory / 'rust-analyzer'
    executable.write_text(f'#!{sys.executable}\nimport os,sys\n'
        f'os.execv({NODE!r}, [{NODE!r}, {str(ROOT / "tests/rust_macro_analyzer_proxy.mjs")!r}, '
        f'{str(real_analyzer)!r}, {str(log)!r}, {str(invalid)!r}, *sys.argv[1:]])\n')
    executable.chmod(0o755)
    return dict(os.environ, PATH=str(directory) + os.pathsep + os.environ['PATH'], KNOW_CODE_OWNED_MACRO_ENV='inherited-wrong-binding'), log


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
        assert calls('aliased', 'ordinary_leaf'), 'canonical declaration validation confused an alias with a renamed target'
        assert calls('nested_leaf', 'ordinary_leaf') and calls('nested_call', 'nested_leaf'), 'attributed module suppressed proven callable identities'
        assert calls('risk_leaf', 'ordinary_leaf') and calls('risk_call', 'risk_leaf'), 'out-of-line attribute risk suppressed proven callables'
        assert not {'OldShape', 'NewShape', 'old_field', 'new_field', 'AttributedShape', 'attributed_field',
                    'RiskShape', 'risk_field', 'DeepRiskShape', 'deep_risk_field'} & {symbol['name'] for symbol in symbols.values()}, 'unproved attributed noncallable identity was published'
        for name in ('PlainShape', 'plain_field', 'PlainNestedShape', 'plain_nested_field'):
            authored(name)

        assert not {'old_name', 'new_name'} & {symbol['name'] for symbol in symbols.values()}, 'same-kind attribute rename reused an authored function identity'
        assert not {'generated_one', 'generated_two', 'generated_method'} & {symbol['name'] for symbol in symbols.values()}, 'unproven expansion identity was published'
        assert not any(symbol['name'] in ('choose', 'made') and symbol['kind'] in ('function', 'method') for symbol in symbols.values()), 'macro implementation function became a runtime call target'
        owned_sources = [frame['source']['path'] for frame in payload['messages'] if frame.get('source', {}).get('compilationId') == identity]
        assert sorted(owned_sources) == ['src/lib.rs', 'src/plain.rs', 'src/risk/mod.rs', 'src/risk/nested.rs'], owned_sources
        report = next(frame['report'] for frame in payload['messages'] if frame.get('report', {}).get('compilationId') == identity)
        assert any(item['code'] == 'rust_unproven_definition_origin' for item in report['diagnostics']), report
        assert any(item['code'] == 'rust_unproven_attribute_origin' for item in report['diagnostics']), report
        assert len([item for item in report['diagnostics'] if item['code'] == 'rust_lookup_syntax_unavailable' and 'macro-api/src/lib.rs' in item.get('path', '')]) == 1, report


positive_environment, positive_log = analyzer_proxy('positive-proxy')
cold = analyze('cold', positive_environment)
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
selected = analyze('cargo-selected-compiler', dict(positive_environment, PATH=str(bin_directory) + os.pathsep + positive_environment['PATH']), preparations)
validate(selected)
assert not marker.exists(), 'PATH rustc overrode the actual Cargo compiler/sysroot'
recorded = [json.loads(line) for line in positive_log.read_text().splitlines()]
assert any(item.get('method') == 'initialize' and item.get('result', {}).get('capabilities', {}).get('diagnosticProvider') for item in recorded)
for item in recorded:
    if item.get('method') == 'initialize':
        options = item['params']['initializationOptions']
        assert options['procMacro']['attributes']['enable'] is True
        assert options['procMacro']['ignored'] == {}
        assert options['diagnostics']['enable'] is True and options['diagnostics']['disabled'] == []
assert any(item.get('method') == 'textDocument/diagnostic' and item.get('result', {}).get('kind') == 'full' for item in recorded)
assert all('previousResultId' not in item.get('params', {}) for item in recorded if item.get('method') == 'textDocument/diagnostic')
assert any(item.get('method') == 'textDocument/prepareCallHierarchy' for item in recorded), 'canonical declaration proof was not retained'

# Real analyzer/server, fault injected only into the public initialize payload:
# Cargo still compiled and witnessed the valid dylib; the loader is given an
# owned invalid image. Healthy server startup is insufficient to pass this case.
invalid_dylib = ARTIFACTS / 'invalid-tool.dylib'
invalid_dylib.write_bytes(b'owned invalid dynamic library\n')
negative_environment, negative_log = analyzer_proxy('loader-failure-proxy', invalid_dylib)
failed = analyze('loader-failure', negative_environment, preparations)
assert not any('source' in frame or 'facts' in frame for frame in failed['messages']), 'loader failure published fallback data'
reports = [frame['report'] for frame in failed['messages'] if 'report' in frame]
assert any(any(item['code'] == 'rust_macro_replay_unavailable' for item in report['diagnostics']) for report in reports), reports
negative = [json.loads(line) for line in negative_log.read_text().splitlines()]
assert any(item.get('injectedLoaderFault', {}).get('capturedDylibs') for item in negative)
assert any(item.get('serverStatus', {}).get('health') == 'ok' for item in negative), 'actual healthy-server/failed-dylib boundary was not exercised'
assert any(item.get('method') == 'textDocument/diagnostic' and item.get('result', {}).get('kind') == 'full'
           and any(diagnostic.get('severity') == 1 or 'macro' in str(diagnostic.get('code', ''))
                   for diagnostic in item['result'].get('items', [])) for item in negative), negative
(ARTIFACTS / 'summary.json').write_text(json.dumps({'passed': True, 'assertions': [
    'exact captured macro dylib replay in both library contexts', 'Cargo-injected macro environment selects the correct valid authored binding', 'macro-induced binding/shadowing change',
    'ordinary and attribute-preserved body calls', 'same-kind attribute rename omitted without breaking aliases',
    'same-kind type/field transformation and enclosing module risk omit unproved noncallables', 'unattributed root/module type and field identities remain', 'unproven generated declarations omitted',
    'macro implementation is not a runtime call target', 'dependency source remains lookup-only',
    'Cargo-selected compiler/sysroot independent of unrelated PATH rustc',
    'synchronous full diagnostics gate replay', 'lookup-only syntax presentation panic omits unproved targets', 'healthy server with actual dylib-load failure publishes no fallback facts'],
    'limits': ['no expanded-source identity promises', 'arbitrary macro runtime file/environment input tracking remains partial']}, indent=2))
print(ARTIFACTS)
