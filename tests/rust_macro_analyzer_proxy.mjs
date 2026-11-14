#!/usr/bin/env node
// Owned test-only LSP recorder/fault injector; never packaged with the product.
import fs from 'node:fs';
import {spawn} from 'node:child_process';

const [program, logFile, invalidDylib, ...args] = process.argv.slice(2);
const child = spawn(program, args, {stdio: ['pipe', 'pipe', 'pipe'], env: {...process.env, RUST_BACKTRACE: '1'}});
const pending = new Map();
let logged = 0, stderrBytes = 0, closing = false, killTimer, drainTimer;
const syntaxMethods = new Set(['rust-analyzer/viewFileText', 'rust-analyzer/viewSyntaxTree']);
function record(value) {
  const text = JSON.stringify({atNs: process.hrtime.bigint().toString(), ...value}) + '\n';
  if (logged + Buffer.byteLength(text) > 8 * 1024 * 1024) throw new Error('macro fixture LSP log exceeds 8 MiB');
  fs.appendFileSync(logFile, text); logged += Buffer.byteLength(text);
}
function stop(code, reason, immediate = false) {
  if (closing) return;
  closing = true; process.exitCode = code;
  try { record({proxyStopping: {reason, code, pending: [...pending.values()].map(item => ({method: item.method, params: item.params}))}}); } catch {}
  process.stdin.pause(); child.stdin.end();
  if (immediate) child.kill('SIGTERM');
  killTimer = setTimeout(() => child.kill('SIGKILL'), immediate ? 1000 : 1500);
  // Retain the outer harness process group: this proxy never detaches a compiler
  // subtree from the shared owner. Bound inherited-pipe drain after child exit.
}
function failed(error) {
  try { record({proxyError: error.message}); } catch {}
  process.stderr.write(`macro fixture proxy: ${error.message}\n`);
  stop(1, 'proxy error', true);
}
function connection(input, output, request) {
  let buffer = Buffer.alloc(0);
  input.on('data', chunk => {
   try {
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
        if (syntaxMethods.has(message.method)) record({request: {id: message.id, method: message.method, params: message.params}});
        if (['initialize', 'shutdown', 'textDocument/diagnostic', 'textDocument/definition', 'textDocument/prepareCallHierarchy', 'callHierarchy/outgoingCalls'].includes(message.method) || syntaxMethods.has(message.method))
          pending.set(message.id, {method: message.method, params: message.params});
      } else if (!message.method && pending.has(message.id)) {
        const original = pending.get(message.id);
        let syntaxTopLevelKinds;
        if (original.method === 'rust-analyzer/viewSyntaxTree' && typeof message.result === 'string') {
          try { syntaxTopLevelKinds = JSON.parse(message.result).children?.filter(item => item.type === 'Node').map(item => item.kind).slice(0, 128); }
          catch { /* Preserve the actual RPC result; product syntax validation owns any failure. */ }
        }
        record({...original, ...(syntaxMethods.has(original.method) ? {responseBytes: length, syntaxTopLevelKinds,
          resultType: typeof message.result, ...(typeof message.result === 'string' ? {resultUtf8Bytes: Buffer.byteLength(message.result)} : {})} :
          {result: message.result}), error: message.error});
        pending.delete(message.id);
      } else if (message.method === 'experimental/serverStatus') record({serverStatus: message.params});
      const encoded = Buffer.from(JSON.stringify(message));
      output.write(`Content-Length: ${encoded.length}\r\n\r\n`); output.write(encoded);
      if (request && message.method === 'exit') stop(0, 'protocol exit');
    }
   } catch (error) { failed(error); }
  });
  input.on('error', failed); output.on('error', failed);
  input.on('end', () => { output.end(); if (request) stop(0, 'client stdin EOF'); });
}
if (args.includes('--version')) {
  child.stdout.pipe(process.stdout); child.stdin.end();
} else {
  connection(process.stdin, child.stdin, true);
  connection(child.stdout, process.stdout, false);
}
child.stderr.on('data', bytes => {
  try {
    const available = Math.max(0, 8 * 1024 * 1024 - stderrBytes);
    if (available) fs.appendFileSync(`${logFile}.stderr.log`, bytes.subarray(0, available));
    stderrBytes += bytes.length; process.stderr.write(bytes);
  } catch (error) { failed(error); }
});
child.on('error', failed);
child.on('exit', (code, signal) => {
  clearTimeout(killTimer);
  process.exitCode = process.exitCode ?? code ?? 1;
  try { record({analyzerExit: {code, signal, stderrBytes, stderrRetainedBytes: Math.min(stderrBytes, 8 * 1024 * 1024)}}); } catch {}
  process.stdin.destroy();
  drainTimer = setTimeout(() => { child.stdout.destroy(); child.stderr.destroy(); process.stdout.end(); }, 500);
});
child.on('close', () => { clearTimeout(killTimer); clearTimeout(drainTimer); process.stdin.destroy(); });
for (const signal of ['SIGTERM', 'SIGINT']) process.on(signal, () => stop(1, signal, true));
