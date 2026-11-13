import fs from 'node:fs';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import {createHash} from 'node:crypto';
import {spawnSync} from 'node:child_process';
import {send, MAX_FRAME} from '../shared/protocol.mjs';
export {relative} from '../shared/source.mjs';
import {relative} from '../shared/source.mjs';
import {createGraph, graphCapabilities} from './graph.mjs';
const repo = fileURLToPath(new URL('../..', import.meta.url));
export const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const kinds = ['unknown', 'module', 'namespace', 'namespace_alias', 'macro', 'enum', 'struct', 'class',
  'protocol', 'extension', 'union', 'typealias', 'function', 'variable', 'field', 'enum_constant',
  'instance_method', 'class_method', 'static_method', 'instance_property', 'class_property',
  'static_property', 'constructor', 'destructor', 'conversion_function', 'parameter'];
function toolStderr(stderr) {
  const bytes = Buffer.from(String(stderr ?? ''));
  return bytes.length <= 8000 ? bytes.toString('utf8') : `${bytes.subarray(0, 4000).toString('utf8')}\n[Swift tool stderr truncated]\n${bytes.subarray(-4000).toString('utf8')}`;
}
export function run(command, args, cwd, env = process.env) {
  const result = spawnSync(command, args, {cwd, env, encoding: 'utf8', maxBuffer: MAX_FRAME, timeout: 120_000});
  if (result.error) throw new Error(`Swift tool failed: ${String(result.error.message).slice(0, 4096)}; stderr: ${toolStderr(result.stderr)}`);
  if (result.signal) throw new Error(`Swift tool interrupted by ${result.signal}; stderr: ${toolStderr(result.stderr)}`);
  return result;
}
export function checked(command, args, cwd, env = process.env) {
  const result = run(command, args, cwd, env);
  if (result.status !== 0) throw new Error(`${command} failed: ${result.stderr.trim()}`);
  return result.stdout.trim();
}
// SwiftPM and Xcode use the same full-body primary compiler/diagnostic path.
export function compileSources({compiler, options, store, root, env}) {
  const diagnostics = [];
  let partial = false;
  for (const option of options) {
    try {
      const compile = run(compiler, ['-index-file', '-index-file-path', option.file,
        '-index-store-path', store, '-index-include-locals', '-index-ignore-system-modules',
        '-index-unit-output-path', path.join(store, `${hash(option.file)}.o`), ...option.args], option.workingDirectory, env);
      const items = compilerDiagnostics(compile.stderr, root);
      if (/Stack dump:|Please submit a bug report|signal [0-9]+|LLVM ERROR|Assertion failed/i.test(compile.stderr)
          || (compile.status !== 0 && !items.some(item => item.severity === 'error' && item.path)))
        throw new Error(`Swift compiler/driver failed: ${compile.stderr.slice(0, 16000)}`);
      partial ||= compile.status !== 0 || items.some(item => item.severity === 'error');
      for (const item of items) if (diagnostics.length < 100 && !diagnostics.some(existing => JSON.stringify(existing) === JSON.stringify(item))) diagnostics.push(item);
    } catch (error) {
      const moduleCaches = option.args.flatMap((value, index) => value === '-module-cache-path' ? [option.args[index + 1] ?? '<missing>']
        : value.startsWith('-module-cache-path=') ? [value.slice('-module-cache-path='.length)] : []);
      const bounded = value => Buffer.from(String(value)).subarray(0, 1024).toString('utf8');
      // The shared worker retains only a bounded stderr tail. Put the actual
      // operation last so a large compiler stack cannot erase its provenance.
      throw new Error(`Swift compiler/driver failed: ${String(error.message).slice(0, 12000)}\nstage=index-file; source=${bounded(option.file)}; compiler=${bounded(compiler)}; module_cache_path=${bounded(moduleCaches.at(-1) ?? '<not specified>')}; failure=${String(error.message).slice(0, 128).replaceAll('\n', ' ')}`, {cause: error});
    }
  }
  return {partial, diagnostics};
}

// IndexStore uses one-based UTF-8 byte columns. Its occurrence has a start only;
// recover just that token in the exact retained text, never search by symbol name.
function tokenRange(source, line, column) {
  if (!line || line > source.lines.length || !column) return null;
  const start = source.lines[line - 1] + column - 1;
  const lineEnd = source.lineEnds[line - 1];
  if (!Number.isSafeInteger(start) || start >= lineEnd || (source.bytes[start] & 0xc0) === 0x80) return null;
  // IndexStore anchors subscripts/array constructors at '[' and computed
  // accessors at '{'. These are actual source tokens, not name guesses.
  if (source.bytes[start] === 91 || source.bytes[start] === 123) return [start, start + 1];
  const point = offset => {
    const lead = source.bytes[offset];
    const width = lead < 0x80 ? 1 : lead < 0xe0 ? 2 : lead < 0xf0 ? 3 : 4;
    return [source.bytes.toString('utf8', offset, offset + width), width];
  };
  let end = start;
  if (source.bytes[start] === 96) {
    const close = source.bytes.indexOf(96, start + 1);
    return close > start + 1 && close < lineEnd ? [start, close + 1] : null;
  }
  const [first] = point(start);
  const identifier = /^[\p{ID_Start}_]$/u.test(first);
  const accepts = identifier ? /^[\p{ID_Continue}\u200c\u200d]$/u
    : first === '.' ? /^[.\/\-=+!*%<>&|^?~\p{Sm}\p{So}]$/u
    : /^[\/\-=+!*%<>&|^?~\p{Sm}\p{So}]$/u;
  while (end < lineEnd) {
    const [character, width] = point(end);
    if (!accepts.test(character)) break;
    end += width;
  }
  return end > start ? [start, end] : null;
}
export function sourceFile(filename, root) {
  const bytes = fs.readFileSync(filename);
  const text = new TextDecoder('utf-8', {fatal: true, ignoreBOM: true}).decode(bytes);
  if (!Buffer.from(text).equals(bytes)) throw new Error(`unsupported Swift source encoding: ${filename}`);
  const lines = [0];
  const lineEnds = [];
  for (let i = 0; i < bytes.length; i++) {
    if (bytes[i] !== 10 && bytes[i] !== 13) continue;
    lineEnds.push(i);
    if (bytes[i] === 13 && bytes[i + 1] === 10) i++;
    lines.push(i + 1);
  }
  lineEnds.push(bytes.length);
  return {filename, path: relative(root, filename), bytes, lines, lineEnds, id: `${relative(root, filename)}@${hash(bytes)}`};
}
export function compilerDiagnostics(stderr, root) {
  const diagnostics = [];
  const pattern = /^(?:(.*?):\d+(?::\d+)?: )?(error|warning|note|remark): (.*)$/gm;
  for (const match of stderr.matchAll(pattern)) {
    diagnostics.push({code: 'swift_compiler', severity: ['note', 'remark'].includes(match[2]) ? 'info' : match[2],
      message: match[3], path: match[1] && match[1] !== '<unknown>' ? relative(root, path.resolve(root, match[1])) : ''});
    if (diagnostics.length === 100) break;
  }
  if (!diagnostics.length && stderr.trim()) diagnostics.push({code: 'swift_compiler', severity: 'error', message: stderr.trim().slice(0, 16000), path: ''});
  return diagnostics;
}

function* records(filename) {
  const fd = fs.openSync(filename, 'r');
  const chunk = Buffer.alloc(64 * 1024);
  let pending = Buffer.alloc(0);
  try {
    for (;;) {
      const size = fs.readSync(fd, chunk);
      if (!size) break;
      const bytes = pending.length ? Buffer.concat([pending, chunk.subarray(0, size)]) : chunk.subarray(0, size);
      let start = 0;
      for (;;) {
        const end = bytes.indexOf(10, start);
        if (end < 0) break;
        if (end - start > MAX_FRAME) throw new Error('resource-limited: Swift IndexStore record exceeds 8 MiB');
        yield JSON.parse(bytes.toString('utf8', start, end));
        start = end + 1;
      }
      pending = Buffer.from(bytes.subarray(start));
      if (pending.length > MAX_FRAME) throw new Error('resource-limited: Swift IndexStore record exceeds 8 MiB');
    }
    if (pending.length) throw new Error('truncated Swift IndexStore record');
  } finally { fs.closeSync(fd); }
}

export function ingestStore({id, root, sources, store, library, scratch, onInputs}) {
  const packaged = fs.existsSync(path.join(repo, 'runtime.json'));
  const reader = packaged ? fileURLToPath(new URL('./indexstore-reader', import.meta.url)) : path.join(repo, '.artifacts', 'build', process.env.KNOW_CODE_BUILD_PROFILE || 'debug', 'swift', 'indexstore-reader');
  if (packaged && !fs.existsSync(reader)) throw new Error("Swift IndexStore helper missing; reinstall the matching Know Code runtime");
  if (!fs.existsSync(reader)) throw new Error(`Swift IndexStore helper missing; run bash providers/swift/build.sh ${process.env.KNOW_CODE_BUILD_PROFILE || 'debug'}`);
  const spool = path.join(scratch, `${hash(id)}.jsonl`);
  const sourceList = path.join(scratch, `${hash(id)}.sources`);
  fs.writeFileSync(sourceList, Buffer.concat([...sources.keys()].map(file => Buffer.from(`${file}\0`))));
  const fd = fs.openSync(spool, 'wx');
  try {
    if (fs.existsSync(store)) {
      const result = spawnSync(reader, [library, store, sourceList], {cwd: root, stdio: ['ignore', fd, 'pipe'],
        encoding: 'utf8', maxBuffer: MAX_FRAME, timeout: 120_000});
      if (result.error || result.signal || result.status !== 0)
        throw new Error(`Swift IndexStore reader failed: status=${result.status}, signal=${result.signal ?? 'none'}, error=${String(result.error?.message ?? 'none').slice(-4096)}, stderr=${String(result.stderr ?? '').slice(-16000)}`);
    }
  } finally { fs.closeSync(fd); }
  const symbols = new Map();
  const internal = new Set();
  const indexedFiles = new Set();
  const inputs = new Set();
  const register = item => {
    if (!item.usr) return;
    const symbolId = `${id}:${item.usr}`;
    if (!symbols.has(symbolId)) symbols.set(symbolId, {id: symbolId, name: item.name,
      kind: kinds[item.kind] ?? `indexstore_kind_${item.kind}`, signature: item.name});
  };
  // First pass retains symbol identities only, so declarations in later files
  // correctly classify earlier references. Occurrences remain on disk.
  for (const item of records(spool)) {
    if (item.unit) { indexedFiles.add(path.resolve(item.path)); continue; }
    if (item.input) { inputs.add(path.resolve(item.path)); continue; }
    if (item.record || !sources.has(path.resolve(item.path)) || !item.usr) continue;
    const symbolId = `${id}:${item.usr}`;
    if (item.roles & 3) internal.add(symbolId);
    if (!Array.isArray(item.relations)) throw new Error('Swift IndexStore helper lacks compiler relations; rebuild/reinstall the matching Know Code runtime');
    register(item);
    for (const relation of item.relations) register(relation);
  }
  onInputs?.(inputs);
  for (const source of sources.values()) {
    if (!fs.readFileSync(source.filename).equals(source.bytes)) throw new Error(`saved Swift source changed during analysis: ${source.path}`);
    send('source', {id: source.id, path: source.path, utf8: source.bytes, sha256: hash(source.bytes),
      compilationId: id, generated: source.generated ?? false});
  }
  let batch = [];
  for (const symbol of symbols.values()) {
    batch.push({...symbol, external: !internal.has(symbol.id)});
    if (batch.length === 128) { send('facts', {compilationId: id, symbols: batch}); batch = []; }
  }
  if (batch.length) send('facts', {compilationId: id, symbols: batch});
  batch = [];
  let unavailableRanges = 0;
  let seen = new Set();
  let relationships = [];
  const graph = createGraph({id, symbols, emit: relationship => {
    relationships.push(relationship);
    if (relationships.length === 128) { send('facts', {compilationId: id, relationships}); relationships = []; }
  }});
  for (const item of records(spool)) {
    // The native reader deduplicates records shared by units. Keep occurrence
    // deduplication scoped to one record rather than retaining a whole graph.
    if (item.record) { seen = new Set(); graph.reset(); continue; }
    if (item.unit || !item.usr) continue;
    const source = sources.get(path.resolve(item.path));
    if (!source) continue;
    const range = tokenRange(source, item.line, item.column);
    graph.occurrence(item, source, range);
    // Implicit compiler anchors are call evidence when CALL supplies a site,
    // not automatically independently written declaration/use occurrences.
    if (item.roles & 256) continue;
    if (!range) { unavailableRanges++; continue; }
    const symbolId = `${id}:${item.usr}`;
    const role = item.roles & 3 ? 'declaration' : 'reference';
    const key = `${symbolId}:${source.id}:${range[0]}:${range[1]}:${role}`;
    if (seen.has(key)) continue;
    seen.add(key);
    batch.push({symbolId, artifactId: source.id, startByte: range[0], endByte: range[1], role,
      enclosingSymbolId: item.enclosing ? `${id}:${item.enclosing}` : ''});
    if (batch.length === 128) { send('facts', {compilationId: id, occurrences: batch}); batch = []; }
  }
  if (batch.length) send('facts', {compilationId: id, occurrences: batch});
  if (relationships.length) send('facts', {compilationId: id, relationships});
  const diagnostics = graph.diagnostics();
  const missingFiles = [...sources.keys()].filter(file => !indexedFiles.has(file));
  if (missingFiles.length) diagnostics.push({code: 'swift_index_missing_sources', severity: 'warning', path: '',
    message: `The compiler produced no primary index unit for ${missingFiles.map(file => relative(root, file)).join(', ')}`});
  if (unavailableRanges) diagnostics.push({code: 'swift_token_range_unavailable', severity: 'warning', path: '',
    message: `${unavailableRanges} compiler occurrences had unsupported source token ranges and were omitted.`});
  fs.unlinkSync(spool); fs.unlinkSync(sourceList);
  return {partial: missingFiles.length > 0 || unavailableRanges > 0, diagnostics, capabilities: graphCapabilities};
}
