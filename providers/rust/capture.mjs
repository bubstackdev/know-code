#!/usr/bin/env node
// Invoked only by the managed Cargo-check planner after its trust/tool guard.
import fs from 'node:fs';
import path from 'node:path';
import {createHash} from 'node:crypto';
import {spawnSync} from 'node:child_process';
import {boundedRead, writeDerivedFile} from './preparation.mjs';
import {contextArguments, fileEvidence, flagValues} from './contexts.mjs';

const LIMIT = 8 * 1024 * 1024;
const [compiler, ...args] = process.argv.slice(2);
const record = {compiler, args, cwd: process.cwd(), environment: {}, absentEnvironment: [], artifacts: [], status: null};
function compilerStdio() {
  const tokens = (process.env.CARGO_MAKEFLAGS ?? '').match(/--jobserver-(?:auth|fds)(?:=[^\s]*)?/g) ?? [];
  if (tokens.some(token => !/^--jobserver-(?:auth|fds)=\d+,\d+$/.test(token)))
    throw new Error('Rust context unavailable: unsupported Cargo jobserver coordination format');
  const descriptors = [...tokens.join(' ').matchAll(/--jobserver-(?:auth|fds)=(\d+),(\d+)(?=\s|$)/g)]
    .flatMap(match => [Number(match[1]), Number(match[2])]);
  const stdio = ['pipe', 'pipe', 'pipe'];
  for (const descriptor of new Set(descriptors)) {
    if (descriptor < 3 || descriptor > 255 || !fs.fstatSync(descriptor).isFIFO())
      throw new Error('Rust context unavailable: Cargo jobserver descriptors cannot be preserved');
    while (stdio.length <= descriptor) stdio.push('ignore');
    stdio[descriptor] = descriptor;
  }
  return stdio;
}
let configuration;
try {
  configuration = JSON.parse(boundedRead(process.env.KNOW_CODE_RUST_CAPTURE_CONFIG, LIMIT).toString('utf8'));
  if (!path.isAbsolute(configuration.directory ?? '') || !(configuration.snapshots || Array.isArray(configuration.inputs)) ||
      configuration.inputs && (configuration.inputs.length > 16000 || !configuration.inputs.every(item => typeof item === 'string' && path.isAbsolute(item))) ||
      !Array.isArray(configuration.toolRoots) || !configuration.toolRoots.every(item => typeof item === 'string' && path.isAbsolute(item)))
    throw new Error('Rust context unavailable: invalid bounded compiler capture configuration');
  if (args.some(item => !item.startsWith('-') && item.endsWith('.rs'))) {
    const parsed = contextArguments(record);
    const emit = flagValues(args, '--emit').flatMap(item => item.split(',').map(value => value.split('=', 1)[0]));
    const tooling = configuration.toolRoots.includes(parsed.root);
    if (!emit.length || emit.some(item => !['dep-info', 'metadata', ...(tooling ? ['link'] : [])].includes(item)))
      throw new Error('Rust context unavailable: final-product/codegen output is outside compiler preparation');
    if (configuration.snapshots) {
      const snapshot = configuration.snapshots[process.env.CARGO_MANIFEST_PATH];
      if (!snapshot || !path.isAbsolute(snapshot.filename ?? '')) throw new Error('Rust context unavailable: no package input snapshot');
      const bytes = boundedRead(snapshot.filename, LIMIT);
      if (createHash('sha256').update(bytes).digest('hex') !== snapshot.sha256) throw new Error('Rust context unavailable: package snapshot changed');
      record.beforeSnapshot = snapshot;
    } else record.beforeInputs = fileEvidence(configuration.inputs);
    record.beforeArtifacts = fileEvidence(flagValues(args, '--extern').map(value => {
      const separator = value.indexOf('=');
      if (separator < 1) throw new Error('Rust context unavailable: pathless extern artifact');
      return path.resolve(record.cwd, value.slice(separator + 1));
    }), {artifact: true});
  }
  const result = spawnSync(compiler, args, {cwd: record.cwd, env: process.env, encoding: 'utf8',
    timeout: 120000, maxBuffer: LIMIT, stdio: compilerStdio()});
  if (result.stdout) process.stdout.write(result.stdout);
  if (result.stderr) process.stderr.write(result.stderr);
  for (const line of (result.stderr ?? '').split('\n')) {
    try { const item = JSON.parse(line); if (item.$message_type === 'artifact') record.artifacts.push(item); }
    catch { /* Preserve original compiler output; non-JSON is not graph evidence. */ }
  }
  Object.assign(record, {status: result.status, signal: result.signal, error: result.error?.message});
  record.environment = Object.fromEntries(Object.entries(process.env).filter(([name]) =>
    name.startsWith('CARGO_PKG_') || ['CARGO_CRATE_NAME', 'CARGO_MANIFEST_DIR', 'CARGO_MANIFEST_PATH', 'OUT_DIR'].includes(name)));
  // Retain only environment values actually named by compiler dep-info, in
  // addition to Cargo's required project environment. Never collect all env.
  for (const item of record.artifacts.filter(item => item.emit === 'dep-info')) {
    const text = boundedRead(path.resolve(record.cwd, item.artifact), LIMIT).toString('utf8');
    for (const line of text.split(/\r?\n/)) {
      const match = /^# env-dep:([A-Za-z_][A-Za-z_0-9]*)(?:=.*)?$/.exec(line);
      if (!match) continue;
      if (Object.hasOwn(process.env, match[1])) record.environment[match[1]] = process.env[match[1]];
      else record.absentEnvironment.push(match[1]);
    }
  }
} catch (error) {
  record.error = error.message;
  process.stderr.write(`${error.message}\n`);
}
if (configuration?.directory) {
  let encoded = JSON.stringify(record);
  if (Buffer.byteLength(encoded) > LIMIT) encoded = JSON.stringify({compiler, args, cwd: record.cwd, status: null,
    error: 'resource-limited: compiler capture exceeds 8 MiB'});
  writeDerivedFile(path.join(configuration.directory, `${process.pid}.json`), encoded);
}
process.exitCode = record.status === 0 && !record.error ? 0 : record.status || 1;
