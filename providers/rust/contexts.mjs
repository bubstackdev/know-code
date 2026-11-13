// Rust-specific compiler/context evidence. No publication, scheduling or query engine.
import fs from 'node:fs';
import path from 'node:path';
import {createHash} from 'node:crypto';
import {boundedRead} from './preparation.mjs';

const gap = reason => { throw new Error(`Rust context unavailable: ${reason}`); };
const MAX_INPUT = 8 * 1024 * 1024;
const MAX_TOTAL = 64 * 1024 * 1024;
const MAX_ARTIFACT = 256 * 1024 * 1024;
const MAX_ARTIFACT_TOTAL = 512 * 1024 * 1024;
const compilerImages = new Map();
export const flagValues = (args, name) => args.flatMap((value, index) =>
  value === name ? [args[index + 1]] : value.startsWith(`${name}=`) ? [value.slice(name.length + 1)] : []);
const flag = (args, name) => flagValues(args, name)[0];

// These exist only to coordinate this analysis process; replaying descriptors,
// wrappers or private capture paths into a macro would be misleading.
const captureEnvironmentKeys = new Set(['KNOW_CODE_RUST_CAPTURE_CONFIG', 'RUSTC_WRAPPER',
  'CARGO_MAKEFLAGS', 'MAKEFLAGS', 'MFLAGS', 'CARGO_CACHE_RUSTC_INFO', 'CARGO_BUILD_BUILD_DIR', 'CARGO_BUILD_JOBS']);
const environmentHash = value => createHash('sha256').update(value).digest('hex');
const ownHash = (values, name) => Object.hasOwn(values, name) ? values[name] : null;
const baselineDigest = values => environmentHash(JSON.stringify(Object.keys(values).sort().map(name => [name, values[name]])));
export function environmentBaseline(environment) {
  const values = Object.fromEntries(Object.entries(environment).filter(([name]) => !captureEnvironmentKeys.has(name))
    .map(([name, value]) => [name, environmentHash(value)]));
  if (Object.keys(values).length > 4096 || Buffer.byteLength(JSON.stringify(values)) > 512 * 1024)
    gap('compiler environment baseline exceeds its bounded allowance');
  return values;
}
export function capturedEnvironment(environment, baseline, required = []) {
  if (!baseline || typeof baseline !== 'object' || Array.isArray(baseline) || Object.keys(baseline).length > 4096 ||
      Buffer.byteLength(JSON.stringify(baseline)) > 512 * 1024 ||
      Object.values(baseline).some(value => typeof value !== 'string' || !/^[a-f0-9]{64}$/.test(value)))
    gap('invalid hashed compiler environment baseline');
  const current = environmentBaseline(environment), values = {}, origins = {}, absent = [];
  const names = new Set([...Object.keys(current).filter(name => ownHash(current, name) !== ownHash(baseline, name)),
    ...Object.keys(environment).filter(name => name.startsWith('CARGO_PKG_') ||
      ['CARGO_CRATE_NAME', 'CARGO_MANIFEST_DIR', 'CARGO_MANIFEST_PATH', 'OUT_DIR'].includes(name)), ...required]);
  const removed = Object.keys(baseline).filter(name => !Object.hasOwn(current, name));
  for (const name of new Set([...names, ...removed])) {
    if (captureEnvironmentKeys.has(name)) continue;
    const value = ownHash(current, name);
    Object.defineProperty(origins, name, {value: {baseline: ownHash(baseline, name), value}, enumerable: true});
    if (value !== null) Object.defineProperty(values, name, {value: environment[name], enumerable: true});
    else absent.push(name);
  }
  return {environment: values, environmentOrigins: origins, environmentBaselineSha256: baselineDigest(baseline),
    absentEnvironment: absent, removedEnvironment: removed};
}
export function capturedEnvironmentCurrent(record, baseline) {
  if (!record.environmentOrigins || !baseline || record.environmentBaselineSha256 !== baselineDigest(baseline)) return false;
  return Object.entries(record.environmentOrigins).every(([name, origin]) => origin &&
    (origin.baseline === null || /^[a-f0-9]{64}$/.test(origin.baseline)) &&
    ownHash(baseline, name) === origin.baseline &&
    (Object.hasOwn(record.environment, name) ? environmentHash(record.environment[name]) : null) === origin.value);
}

// Bare proc_macro is Cargo's sysroot prelude injection, not an artifact join.
// Direct extern lookup uses crate/all search paths; Cargo's typed dependency
// paths cannot replace this sysroot crate. Refuse broader paths rather than
// guessing which same-named binary the compiler selected.
export function externArguments(record) {
  const values = flagValues(record.args, '--extern'), dependencies = [], sysroot = [];
  const search = record.args.flatMap((value, index) => value === '-L' ? [record.args[index + 1]] :
    value.startsWith('-L') && value.length > 2 ? [value.slice(2)] : []);
  for (const value of values) {
    const separator = value.indexOf('=');
    if (separator < 1) {
      if (value !== 'proc_macro' || !flagValues(record.args, '--crate-type').some(value => value.split(',').includes('proc-macro')) ||
          values.some(other => other.startsWith('proc_macro=')) ||
          search.some(value => typeof value !== 'string' || !/^(?:dependency|native|framework)=.+/.test(value)))
        gap('pathless extern dependency lacks an unambiguous sysroot selection');
      sysroot.push(value);
    } else {
      const name = value.slice(0, separator), filename = value.slice(separator + 1);
      if (!/^[_\p{ID_Start}][_\p{ID_Continue}]*$/u.test(name) || !filename) gap('unsupported extern alias/artifact');
      dependencies.push({name, filename: path.resolve(record.cwd, filename)});
    }
  }
  return {dependencies, sysroot: [...new Set(sysroot)]};
}

export function cargoArtifactIdentity(artifact) {
  return JSON.stringify([artifact.package_id, artifact.manifest_path, artifact.target,
    artifact.profile, artifact.features, artifact.filenames]);
}

export function contextArguments(record) {
  if (!path.isAbsolute(record.compiler ?? '') || !Array.isArray(record.args) ||
      record.args.some(item => typeof item !== 'string' || item.startsWith('@')))
    gap('compiler identity/response-file arguments cannot be replayed');
  const args = record.args, query = [], roots = [];
  const semantic = new Set(['--crate-name', '--edition', '--crate-type', '--target', '--cfg', '--sysroot']);
  const ignored = new Set(['--emit', '--out-dir', '--error-format', '--json', '--check-cfg', '--extern', '--cap-lints']);
  for (let index = 0; index < args.length; index++) {
    const value = args[index], name = value.split('=', 1)[0];
    if (semantic.has(name)) {
      query.push(value);
      if (name === value) { if (!args[index + 1]) gap(`missing ${name} argument`); query.push(args[++index]); }
    } else if (value === '--test' || value === '-g' || value === '-O') query.push(value);
    else if (value === '-C' || value.startsWith('-C')) {
      query.push(value);
      if (value === '-C') { if (!args[index + 1]) gap('missing -C argument'); query.push(args[++index]); }
    } else if (ignored.has(name)) {
      if (name === value && !args[++index]) gap(`missing ${name} argument`);
    } else if (['-L', '-A', '-W', '-D', '-F'].includes(value)) {
      if (!args[++index]) gap(`missing ${value} argument`);
    } else if (/^-[LAWDF].+/.test(value)) { /* Non-semantic search/lint flags. */ }
    else if (!value.startsWith('-') && value.endsWith('.rs')) roots.push(path.resolve(record.cwd, value));
    else gap(`unsupported compiler argument ${value}`);
  }
  if (roots.length !== 1 || !flag(args, '--crate-name') || !flag(args, '--edition')) gap('no unique Cargo-selected Rust crate root/edition');
  if (flag(args, '--sysroot')) gap('custom sysroot argument requires equivalent analyzer compiler selection');
  return {root: roots[0], query, target: flag(args, '--target'), test: args.includes('--test'),
    name: flag(args, '--crate-name'), edition: flag(args, '--edition')};
}

export function compilerCfg(record, command) {
  const parsed = contextArguments(record), externs = externArguments(record);
  const effective = command(record.compiler, [...parsed.query, '--print', 'cfg'], record.cwd).split(/\r?\n/).filter(Boolean).sort();
  const baselineArgs = parsed.target ? ['--target', parsed.target] : [];
  const baseline = command(record.compiler, [...baselineArgs, '--print', 'cfg'], record.cwd).split(/\r?\n/).filter(Boolean).sort();
  for (const value of [...effective, ...baseline])
    if (!/^[A-Za-z_][A-Za-z_0-9]*(?:="(?:[^"\\]|\\.)*")?$/.test(value)) gap('unsupported compiler cfg output');
  const sysroot = command(record.compiler, ['--print', 'sysroot'], record.cwd).trim();
  // The public JSON project resolves cfg with this sysroot's compiler. A custom
  // build.rustc wrapper is not equivalent merely because it prints a version.
  if (fs.realpathSync(record.compiler) !== fs.realpathSync(path.join(sysroot, 'bin/rustc')))
    gap('Cargo compiler cannot be proven equivalent to the analyzer sysroot compiler');
  const library = path.join(sysroot, 'lib/rustlib/src/rust/library');
  if (!fs.existsSync(path.join(library, 'Cargo.toml')) || !fs.existsSync(path.join(library, 'core/src/lib.rs')))
    gap(`missing rust-src prerequisite for captured Cargo compiler sysroot ${sysroot}; install rust-src separately and refresh`);
  const stat = fs.statSync(record.compiler), imageKey = JSON.stringify([record.compiler, stat.size, stat.mtimeMs, stat.ctimeMs]);
  if (!compilerImages.has(imageKey)) compilerImages.set(imageKey, fileEvidence([record.compiler], {artifact: true})[record.compiler]);
  const sysrootExternInputs = externs.sysroot.flatMap(name =>
    [path.join(library, name, 'Cargo.toml'), path.join(library, name, 'src/lib.rs')]);
  const sysrootExternHashes = fileEvidence(sysrootExternInputs);
  return {...parsed, sysrootExterns: externs.sysroot, sysrootExternHashes, effective, baseline, removals: baseline.filter(value => !effective.includes(value)), sysroot,
    compilerVersion: command(record.compiler, ['-Vv'], record.cwd), compilerSha256: compilerImages.get(imageKey)};
}

export function compilerInputs(record) {
  const artifacts = record.artifacts.filter(item => item.emit === 'dep-info');
  if (artifacts.length !== 1) gap('no unique compiler-emitted dep-info artifact');
  const filename = path.resolve(record.cwd, artifacts[0].artifact);
  const text = boundedRead(filename, MAX_INPUT).toString('utf8');
  const files = new Set(), environment = {};
  for (const line of text.split(/\r?\n/)) {
    if (!line) continue;
    if (line.startsWith('# env-dep:')) {
      const match = /^# env-dep:([A-Za-z_][A-Za-z_0-9]*)(?:=(.*))?$/.exec(line);
      if (!match || match[2]?.includes('\\')) gap('unsupported escaped dep-info environment input');
      const name = match[1], expected = match[2];
      if (Object.hasOwn(record.environment, name) ? record.environment[name] !== expected :
          expected !== undefined || !record.absentEnvironment?.includes(name)) gap(`uncaptured compiler environment input ${name}`);
      environment[name] = expected ?? null;
      continue;
    }
    if (line.startsWith('#')) gap('unsupported compiler dep-info directive');
    const separator = line.indexOf(': ');
    if (separator < 0) { if (line.endsWith(':')) continue; gap('unsupported dep-info rule'); }
    const inputs = line.slice(separator + 2);
    // Support the unescaped makefile subset first. Unknown escaping is an
    // explicit gap, never a filename guess or silently dropped input.
    if (/[\\$#]/.test(inputs)) gap('escaped/continued dep-info paths require supported parsing');
    for (const item of inputs.split(/\s+/).filter(Boolean)) files.add(path.resolve(record.cwd, item));
    if (files.size > 16000) gap('dep-info input count exceeds 16000');
  }
  if (!files.size) gap('compiler dep-info has no consumed inputs');
  return {files: [...files].sort(), environment};
}

function fileDigest(filename, limit) {
  const descriptor = fs.openSync(filename, fs.constants.O_RDONLY | fs.constants.O_NONBLOCK);
  try {
    const initial = fs.fstatSync(descriptor);
    if (!initial.isFile() || initial.size > limit) gap(`file exceeds bounded hashing allowance: ${filename}`);
    const hash = createHash('sha256'), buffer = Buffer.alloc(64 * 1024);
    let bytes = 0;
    for (let count; (count = fs.readSync(descriptor, buffer, 0, buffer.length, null));) {
      bytes += count;
      if (bytes > limit) gap(`file grew beyond bounded hashing allowance: ${filename}`);
      hash.update(buffer.subarray(0, count));
    }
    const final = fs.fstatSync(descriptor);
    if (bytes !== initial.size || final.size !== initial.size || final.mtimeMs !== initial.mtimeMs)
      gap(`file changed during hashing: ${filename}`);
    return {digest: hash.digest('hex'), bytes};
  } finally { fs.closeSync(descriptor); }
}

export function fileEvidence(paths, {artifact = false} = {}) {
  const result = {}, names = [...new Set(paths)];
  let total = 0;
  for (const filename of names) {
    const file = fileDigest(filename, artifact ? MAX_ARTIFACT : MAX_INPUT);
    total += file.bytes;
    if (total > (artifact ? MAX_ARTIFACT_TOTAL : MAX_TOTAL)) gap('captured input/artifact hashing exceeds its shared operation allowance');
    result[filename] = file.digest;
  }
  return result;
}

export function capturedUnit(record, artifact, command, setupInputs = []) {
  if (record.status !== 0 || record.error || record.signal) gap('failed compilation cannot supply fresh context proof');
  const cfg = compilerCfg(record, command), inputs = compilerInputs(record);
  if (path.resolve(artifact.target.src_path) !== cfg.root || artifact.profile.test !== cfg.test)
    gap('Cargo target/profile differs from the captured compiler invocation');
  const outputs = record.artifacts.map(item => path.resolve(record.cwd, item.artifact));
  if (!artifact.filenames.some(item => outputs.includes(path.resolve(item)))) gap('Cargo/rustc artifacts do not have an exact join');
  if (!inputs.files.includes(cfg.root)) gap('compiler dep-info does not select its declared crate root');
  const targetInput = cfg.target?.endsWith('.json') ? [path.resolve(record.cwd, cfg.target)] : [];
  const inputHashes = fileEvidence([...inputs.files, artifact.manifest_path, ...setupInputs, ...targetInput]);
  for (const filename of inputs.files)
    if (record.beforeInputs?.[filename] !== inputHashes[filename]) gap('consumed input lacks a matching pre-compilation snapshot');
  const dependencies = externArguments(record).dependencies.map(item => item.filename);
  const dependencyHashes = fileEvidence(dependencies, {artifact: true});
  for (const filename of dependencies)
    if (record.beforeArtifacts?.[filename] !== dependencyHashes[filename]) gap('extern artifact lacks matching compiler-consumption snapshot');
  return {record, cargo: artifact, cfg, inputs, outputs, artifactIdentity: cargoArtifactIdentity(artifact),
    inputHashes, dependencyHashes, outputHashes: fileEvidence(outputs, {artifact: true})};
}

export function capturedUnitCurrent(unit, artifact, baseline) {
  try {
    return unit.record.status === 0 && unit.artifactIdentity === cargoArtifactIdentity(artifact) &&
      Object.entries(unit.inputHashes).every(([filename, expected]) => fileDigest(filename, MAX_INPUT).digest === expected) &&
      Object.entries(unit.cfg.sysrootExternHashes ?? {}).every(([filename, expected]) => fileDigest(filename, MAX_INPUT).digest === expected) &&
      Object.entries(unit.outputHashes).every(([filename, expected]) => fileDigest(filename, MAX_ARTIFACT).digest === expected) &&
      Object.entries(unit.dependencyHashes).every(([filename, expected]) => fileDigest(filename, MAX_ARTIFACT).digest === expected) &&
      (unit.record.environmentOrigins ? capturedEnvironmentCurrent(unit.record, baseline) :
        Object.entries(unit.inputs.environment).every(([name, expected]) =>
          name.startsWith('CARGO_PKG_') || ['CARGO_CRATE_NAME', 'CARGO_MANIFEST_DIR', 'CARGO_MANIFEST_PATH', 'OUT_DIR'].includes(name) ||
          (process.env[name] ?? null) === expected));
  } catch { return false; }
}

export function connectCapturedUnits(units) {
  const producers = new Map();
  for (let index = 0; index < units.length; index++) for (const filename of units[index].outputs) {
    producers.set(filename, producers.has(filename) ? null : index);
  }
  return units.map(unit => {
    try { return {...unit, dependencies: externArguments(unit.record).dependencies.map(({name, filename}) => {
    if (!/^[_\p{ID_Start}][_\p{ID_Continue}]*$/u.test(name) || !producers.has(filename) || producers.get(filename) === null) gap('extern alias/artifact producer is unavailable');
    const index = producers.get(filename);
    if (unit.dependencyHashes[filename] !== units[index].outputHashes[filename]) gap('extern producer content differs from caller consumption evidence');
    return {name, unit: index, artifact: filename};
    })}; } catch (error) { return {...unit, dependencies: [], contextError: error.message}; }
  });
}

export function selectedProject(units, selected, metadata) {
  const reachable = [];
  function visit(index) {
    if (reachable.includes(index)) return;
    reachable.push(index);
    if (units[index].contextError) gap(units[index].contextError);
    for (const dependency of units[index].dependencies) visit(dependency.unit);
  }
  visit(selected);
  if (reachable.some(index => units[index].cfg.sysrootExterns?.includes('proc_macro')) &&
      reachable.some(index => units[index].cfg.name === 'proc_macro'))
    gap('explicit reachable crate conflicts with the sysroot proc_macro injection');
  const owner = new Map(), removals = JSON.stringify(units[selected].cfg.removals);
  const sysroot = units[selected].cfg.sysroot, compilerVersion = units[selected].cfg.compilerVersion;
  for (const index of reachable) {
    const unit = units[index];
    if (unit.cfg.sysroot !== sysroot || unit.cfg.compilerVersion !== compilerVersion || unit.cfg.compilerSha256 !== units[selected].cfg.compilerSha256)
      gap('reachable units select incompatible compiler/sysroot identities');
    if (JSON.stringify(unit.cfg.removals) !== removals) gap('reachable units need different analyzer cfg removals');
    for (const filename of unit.inputs.files.filter(item => item.endsWith('.rs'))) {
      const physical = fs.realpathSync(filename);
      if (owner.has(physical) && owner.get(physical) !== index) gap('shared Rust source has multiple reachable compilation contexts');
      owner.set(physical, index);
    }
  }
  const macroDylibs = [];
  const crates = reachable.map(index => {
    const unit = units[index], member = metadata.workspace_members.includes(unit.cargo.package_id);
    const procMacro = unit.cargo.target.kind.includes('proc-macro');
    const macroArtifacts = procMacro ? unit.record.artifacts.filter(item => item.emit === 'link' &&
      unit.cargo.filenames.includes(path.resolve(unit.record.cwd, item.artifact))) : [];
    if (procMacro && macroArtifacts.length !== 1) gap('procedural macro has no unique captured emitted dylib');
    const dylib = procMacro ? path.resolve(unit.record.cwd, macroArtifacts[0].artifact) : undefined;
    if (procMacro) {
      for (const consumer of reachable) for (const edge of units[consumer].dependencies)
        if (edge.unit === index && (edge.artifact !== dylib || units[consumer].dependencyHashes[dylib] !== unit.outputHashes[dylib]))
          gap('procedural-macro dylib differs from exact caller extern consumption');
      macroDylibs.push({path: dylib, sha256: unit.outputHashes[dylib], sourceFiles: unit.inputs.files.filter(filename => filename.endsWith('.rs'))});
    }
    return {display_name: unit.cfg.name, root_module: unit.cfg.root, edition: unit.cfg.edition,
      cfg: unit.cfg.effective, env: unit.record.environment, is_workspace_member: member,
      ...(procMacro ? {is_proc_macro: true, proc_macro_dylib_path: dylib, proc_macro_cwd: unit.record.cwd} : {}),
      ...(unit.cfg.target ? {target: unit.cfg.target} : {}),
      deps: unit.dependencies.map(item => ({crate: reachable.indexOf(item.unit), name: item.name})),
      source: {include_dirs: [...new Set(unit.inputs.files.filter(filename => filename.endsWith('.rs')).map(filename => path.dirname(filename)))], exclude_dirs: []}};
  });
  if (macroDylibs.length) {
    if (reachable.some(index => units[index].record.removedEnvironment?.length))
      gap('Cargo removed inherited environment values; public per-crate macro replay cannot represent deletion');
    const foreignDirectories = crates.filter(crate => !crate.is_workspace_member).flatMap(crate => crate.source.include_dirs).map(directory => fs.realpathSync(directory));
    foreignDirectories.push(fs.realpathSync(path.join(sysroot, 'lib/rustlib/src/rust/library')));
    for (const filename of units[selected].inputs.files.filter(filename => filename.endsWith('.rs'))) {
      const physical = fs.realpathSync(filename);
      if (foreignDirectories.some(directory => { const relative = path.relative(directory, physical); return relative !== '..' && !relative.startsWith(`..${path.sep}`) && !path.isAbsolute(relative); }))
        gap('macro diagnostic source is ambiguous between local and nonlocal roots');
    }
  }
  const removeAnalyzerCfg = reachable.some(index => metadata.workspace_members.includes(units[index].cargo.package_id) &&
    !units[index].cfg.effective.includes('rust_analyzer'));
  if (removeAnalyzerCfg && reachable.some(index => units[index].cfg.effective.includes('rust_analyzer')))
    gap('reachable units need different rust_analyzer cfg replay');
  const overrides = units[selected].cfg.removals.map(value => {
    const separator = value.indexOf('=');
    return `!${separator < 0 ? value : value.slice(0, separator + 1) + JSON.parse(value.slice(separator + 1))}`;
  });
  return {reachable, macroDylibs, project: {sysroot, sysroot_src: path.join(sysroot, 'lib/rustlib/src/rust/library'), crates},
    analyzerCfg: {setTest: false}, cargoCfgs: [...(removeAnalyzerCfg ? ['!rust_analyzer'] : []), ...overrides],
    firstPartyFiles: metadata.workspace_members.includes(units[selected].cargo.package_id) ?
      units[selected].inputs.files.filter(filename => filename.endsWith('.rs')) : []};
}
