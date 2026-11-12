#!/usr/bin/env node
// Invoked only by the managed Cargo-check planner after its trust/tool guard.
import fs from 'node:fs';
import path from 'node:path';
import {spawnSync} from 'node:child_process';
import {boundedRead} from './preparation.mjs';
import {contextArguments, fileEvidence, flagValues} from './contexts.mjs';

const LIMIT = 8 * 1024 * 1024;
const [compiler, ...args] = process.argv.slice(2);
const record = {compiler, args, cwd: process.cwd(), environment: {}, absentEnvironment: [], artifacts: [], status: null};
let configuration;
try {
  configuration = JSON.parse(boundedRead(process.env.KNOW_CODE_RUST_CAPTURE_CONFIG, LIMIT).toString('utf8'));
  if (!path.isAbsolute(configuration.directory ?? '') || !Array.isArray(configuration.inputs) || configuration.inputs.length > 16000 ||
      !configuration.inputs.every(item => typeof item === 'string' && path.isAbsolute(item)) ||
      !Array.isArray(configuration.toolRoots) || !configuration.toolRoots.every(item => typeof item === 'string' && path.isAbsolute(item)))
    throw new Error('Rust context unavailable: invalid bounded compiler capture configuration');
  if (args.some(item => !item.startsWith('-') && item.endsWith('.rs'))) {
    const parsed = contextArguments(record);
    const emit = flagValues(args, '--emit').flatMap(item => item.split(',').map(value => value.split('=', 1)[0]));
    const tooling = configuration.toolRoots.includes(parsed.root);
    if (!emit.length || emit.some(item => !['dep-info', 'metadata', ...(tooling ? ['link'] : [])].includes(item)))
      throw new Error('Rust context unavailable: final-product/codegen output is outside compiler preparation');
    record.beforeInputs = fileEvidence(configuration.inputs);
    record.beforeArtifacts = fileEvidence(flagValues(args, '--extern').map(value => {
      const separator = value.indexOf('=');
      if (separator < 1) throw new Error('Rust context unavailable: pathless extern artifact');
      return path.resolve(record.cwd, value.slice(separator + 1));
    }));
  }
  const result = spawnSync(compiler, args, {cwd: record.cwd, env: process.env, encoding: 'utf8',
    timeout: 120000, maxBuffer: LIMIT});
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
  fs.writeFileSync(path.join(configuration.directory, `${process.pid}.json`), encoded);
}
process.exitCode = record.status === 0 && !record.error ? 0 : record.status || 1;
