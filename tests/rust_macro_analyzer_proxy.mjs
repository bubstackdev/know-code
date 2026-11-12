#!/usr/bin/env node
// Owned test-only LSP recorder/fault injector; never packaged with the product.
import fs from 'node:fs';
import {spawn} from 'node:child_process';

const [program, logFile, invalidDylib, ...args] = process.argv.slice(2);
const child = spawn(program, args, {stdio: ['pipe', 'pipe', 'pipe']});
const pending = new Map();
let logged = 0;
function record(value) {
  const text = JSON.stringify(value) + '\n';
  if (logged + Buffer.byteLength(text) > 8 * 1024 * 1024) throw new Error('macro fixture LSP log exceeds 8 MiB');
  fs.appendFileSync(logFile, text); logged += Buffer.byteLength(text);
}
function connection(input, output, request) {
  let buffer = Buffer.alloc(0);
  input.on('data', chunk => {
    buffer = Buffer.concat([buffer, chunk]);
    if (buffer.length > 16 * 1024 * 1024) throw new Error('macro fixture LSP buffer exceeds bound');
    for (;;) {
      const end = buffer.indexOf('\r\n\r\n');
      if (end < 0) break;
      const match = /(?:^|\r\n)Content-Length:\s*(\d+)/i.exec(buffer.subarray(0, end).toString('ascii'));
      if (!match) throw new Error('missing fixture LSP length');
      const length = Number(match[1]);
      if (length <= 0 || length > 8 * 1024 * 1024) throw new Error('invalid fixture LSP length');
      if (buffer.length < end + 4 + length) break;
      const message = JSON.parse(buffer.subarray(end + 4, end + 4 + length).toString('utf8'));
      buffer = buffer.subarray(end + 4 + length);
      if (request) {
        if (message.method === 'initialize' && invalidDylib) {
          const replaced = [];
          for (const project of message.params.initializationOptions.linkedProjects ?? []) for (const crate of project.crates ?? [])
            if (crate.is_proc_macro && crate.proc_macro_dylib_path) {
              replaced.push(crate.proc_macro_dylib_path); crate.proc_macro_dylib_path = invalidDylib;
            }
          record({injectedLoaderFault: {capturedDylibs: replaced, replacement: invalidDylib}});
        }
        if (['initialize', 'textDocument/diagnostic', 'textDocument/prepareCallHierarchy', 'callHierarchy/outgoingCalls'].includes(message.method))
          pending.set(message.id, {method: message.method, params: message.params});
      } else if (!message.method && pending.has(message.id)) {
        record({...pending.get(message.id), result: message.result, error: message.error}); pending.delete(message.id);
      } else if (message.method === 'experimental/serverStatus') record({serverStatus: message.params});
      const encoded = Buffer.from(JSON.stringify(message));
      output.write(`Content-Length: ${encoded.length}\r\n\r\n`); output.write(encoded);
    }
  });
  input.on('end', () => output.end());
}
if (args.includes('--version')) {
  child.stdout.pipe(process.stdout); child.stderr.pipe(process.stderr); child.stdin.end();
} else {
  connection(process.stdin, child.stdin, true);
  connection(child.stdout, process.stdout, false);
  child.stderr.pipe(process.stderr);
}
child.on('error', error => { record({proxyError: error.message}); process.exitCode = 1; });
child.on('exit', code => { process.exitCode = code ?? 1; });
for (const signal of ['SIGTERM', 'SIGINT']) process.on(signal, () => { child.kill(signal); process.exitCode = 1; });
