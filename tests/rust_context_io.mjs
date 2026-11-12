// Bounded I/O guards only; no compiler/analyzer/toolchain execution.
import fs from 'node:fs';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import {spawnSync} from 'node:child_process';
import assert from 'node:assert/strict';
import {writeDerivedFile} from '../providers/rust/preparation.mjs';

const repository = fileURLToPath(new URL('..', import.meta.url));
const directory = path.join(repository, '.artifacts/rust-context-io', `${Date.now()}-${process.pid}`);
fs.mkdirSync(directory, {recursive: true});
const fifo = path.join(directory, 'input.fifo');
assert.equal(spawnSync('mkfifo', [fifo]).status, 0);
for (const source of [
  `import {boundedRead} from './providers/rust/preparation.mjs'; boundedRead(${JSON.stringify(fifo)}, 64);`,
  `import {fileEvidence} from './providers/rust/contexts.mjs'; fileEvidence([${JSON.stringify(fifo)}]);`]) {
  const result = spawnSync(process.execPath, ['--input-type=module', '-e', source],
    {cwd: repository, encoding: 'utf8', timeout: 2000});
  assert(!result.error, `regular-file guard blocked: ${result.error?.message}`);
  assert.notEqual(result.status, 0);
}
writeDerivedFile(fifo, 'recovered derived data');
assert(fs.statSync(fifo).isFile());
assert.equal(fs.readFileSync(fifo, 'utf8'), 'recovered derived data');
assert(!fs.readdirSync(directory).some(name => name.endsWith('.next')));

const configuration = path.join(directory, 'configuration.json'), marker = path.join(directory, 'compiler-must-not-run');
fs.writeFileSync(configuration, JSON.stringify({directory, inputs: [], toolRoots: []}));
const rejected = spawnSync(process.execPath, [path.join(repository, 'providers/rust/capture.mjs'), '/usr/bin/touch', marker], {
  cwd: repository, encoding: 'utf8', timeout: 2000,
  env: {...process.env, KNOW_CODE_RUST_CAPTURE_CONFIG: configuration, CARGO_MAKEFLAGS: '--jobserver-auth=fifo:/unsupported'}});
assert(!rejected.error);
assert.notEqual(rejected.status, 0);
assert.match(rejected.stderr, /unsupported Cargo jobserver coordination format/);
assert(!fs.existsSync(marker));
fs.writeFileSync(path.join(directory, 'summary.json'), JSON.stringify({passed: true, assertions: [
  'FIFO setup/source/artifact reads fail without blocking', 'derived FIFO target replaced atomically',
  'unsupported jobserver format refused before tool execution']}, null, 2));
process.stdout.write(`${directory}\n`);
