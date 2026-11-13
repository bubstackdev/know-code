// Focused compiler-tool transport checks; no test framework or external server.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import {once} from 'node:events';
import {JsonRpcProcess} from '../providers/shared/jsonrpc.mjs';

const root = fileURLToPath(new URL('..', import.meta.url));
const directory = path.join(root, '.artifacts/jsonrpc');
fs.mkdirSync(directory, {recursive: true});
const fixture = path.join(directory, 'child.mjs');
fs.writeFileSync(fixture, String.raw`
import {once} from 'node:events';
const mode = process.argv[2];
const frame = value => {
  const body = Buffer.from(JSON.stringify({jsonrpc:'2.0', ...value}));
  return Buffer.concat([Buffer.from('Content-Length: ' + body.length + '\r\n\r\n'), body]);
};
let buffer = Buffer.alloc(0), original;
if (mode === 'slow-reader') { process.stdin.pause(); setTimeout(() => process.stdin.resume(), 100); }
process.stdin.on('data', chunk => {
  buffer = Buffer.concat([buffer, chunk]);
  for (;;) {
    const end = buffer.indexOf('\r\n\r\n');
    if (end < 0) return;
    const size = Number(/Content-Length: (\d+)/i.exec(buffer.subarray(0,end).toString())[1]);
    if (buffer.length < end+4+size) return;
    const message = JSON.parse(buffer.subarray(end+4,end+4+size));
    buffer = buffer.subarray(end+4+size);
    if (!message.method) {
      process.stdout.write(frame({id:original.id, result:{hasResult:Object.hasOwn(message,'result'), value:message.result ?? null}}));
      continue;
    }
    if (mode === 'hang') continue;
    if (mode === 'slow-reader') { process.stdout.write(frame({id:message.id,result:message.params.number})); continue; }
    if (mode === 'exit') process.exit(7);
    if (mode === 'rpc-error') { process.stdout.write(frame({id:message.id,error:{code:-32601,message:'Method not found',data:{method:message.method}}})); continue; }
    if (mode === 'zero') { process.stdout.write('Content-Length: 0\r\n\r\n'); continue; }
    if (mode === 'oversized') { process.stdout.write('Content-Length: 8388609\r\n\r\n'); continue; }
    if (mode === 'large-header') { process.stdout.write('X-Padding: ' + 'a'.repeat(8200) + '\r\nContent-Length: 2\r\n\r\n{}'); continue; }
    if (mode === 'bad-json') { process.stdout.write('Content-Length: 1\r\n\r\n{'); continue; }
    if (mode === 'truncated') { process.stdout.write('Content-Length: 100\r\n\r\n{'); process.exit(0); }
    if (mode === 'server-request') {
      original = message;
      process.stdout.write(frame({id:'server-1',method:'configuration',params:{enabled:true}}));
      continue;
    }
    const response = frame({id:message.id,result:message.params});
    if (mode === 'large-fragmented') {
      (async () => {
        for (let start=0;start<response.length;start+=257) {
          if (!process.stdout.write(response.subarray(start,start+257))) await once(process.stdout,'drain');
          await new Promise(resolve=>setImmediate(resolve));
        }
      })().catch(error=>{console.error(error);process.exit(1);});
      continue;
    }
    const header = response.indexOf('\r\n\r\n');
    const unicode = response.indexOf(Buffer.from('🦀'));
    const cuts = [0, 7, header+2, unicode+1, response.length];
    let index = 0;
    const send = () => {
      process.stdout.write(response.subarray(cuts[index],cuts[index+1]));
      if (++index < cuts.length-1) setTimeout(send, 5);
      else process.stdout.write(Buffer.concat([frame({method:'progress',params:{part:1}}),frame({method:'progress',params:{part:2}})]));
    };
    send();
  }
});
`);

const checks = [];
async function withChild(mode, action, options = {}) {
  const rpc = new JsonRpcProcess(process.execPath, [fixture, mode], {cwd:directory, timeout:1000, ...options});
  const exit = once(rpc.child, 'exit');
  try { await action(rpc, exit); }
  finally {
    rpc.terminate();
    await Promise.race([exit, new Promise((_, reject) => {
      const timer = setTimeout(() => reject(new Error(`fixture failed to terminate: ${mode}`)), 2000);
      timer.unref();
    })]);
  }
}

const notifications = [];
await withChild('fragmented', async rpc => {
  assert.deepEqual(await rpc.request('echo', {text:'café 🦀'}), {text:'café 🦀'});
  // A second request provides a barrier after the coalesced notifications.
  assert.deepEqual(await rpc.request('echo', {text:'second 🦀'}), {text:'second 🦀'});
  assert.deepEqual(notifications.slice(0,2), [{part:1},{part:2}]);
}, {onNotification:(method,params) => { assert.equal(method,'progress'); notifications.push(params); }});
checks.push('fragmented Unicode frames and coalesced notifications');

await withChild('server-request', async rpc => {
  const result = await rpc.request('start', {});
  assert.deepEqual(result, {hasResult:true,value:{accepted:true}});
}, {onRequest:async (method,params) => {
  assert.equal(method,'configuration'); assert.deepEqual(params,{enabled:true});
  return {accepted:true};
}});
checks.push('asynchronous server request response');

await withChild('server-request', async rpc => {
  assert.deepEqual(await rpc.request('start', {}), {hasResult:true,value:null});
}, {onRequest:async () => undefined});
checks.push('undefined server result normalizes to null');

await withChild('rpc-error', async rpc => {
  await assert.rejects(rpc.request('missing-method', {}), error => {
    assert.equal(error.rpcCode,-32601);
    assert.deepEqual(error.rpcData,{method:'missing-method'});
    assert.match(error.message,/Method not found/);
    return true;
  });
  assert.equal(rpc.pending.size,0);
});
checks.push('RPC error preserves code and data');

for (const [mode,pattern] of [['exit',/exited \(7\)/], ['truncated',/exited/],
  ['zero',/invalid\/oversized/], ['oversized',/invalid\/oversized/],
  ['large-header',/oversized JSON-RPC header/], ['bad-json',/JSON|Unexpected|Expected/]]) {
  await withChild(mode, async rpc => {
    await assert.rejects(rpc.request('start', {}), error => {
      assert.match(error.message, pattern);
      if (mode === 'exit' || mode === 'truncated') {
        assert.deepEqual(error.processExit, {code: mode === 'exit' ? 7 : 0, signal: null});
      } else assert.equal(error.processExit, undefined);
      return true;
    });
    assert.equal(rpc.receivedBytes === 0, mode === 'exit');
  });
  checks.push(mode);
}

await withChild('hang', async (rpc, exit) => {
  const results = await Promise.allSettled([rpc.request('never', {}), rpc.request('concurrent', {})]);
  assert.ok(results.every(result => result.status === 'rejected'));
  const error = results[0].reason;
  assert.match(error.message, /timed out: never/);
  assert.equal(error.processExit, undefined);
  assert.equal(results[1].reason, error);
  assert.equal(rpc.pending.size, 0);
  assert.equal(rpc.failure, error);
  await exit;
  assert.equal(rpc.failure, error);
  assert.equal(error.processExit, undefined);
  await assert.rejects(rpc.request('after-exit', {}), rejected => rejected === error);
}, {timeout:100});
checks.push('request timeout fails all pending and preserves its cause after child exit');

await withChild('hang', async rpc => {
  await assert.rejects(rpc.request('too-large', {text:'x'.repeat(8*1024*1024)}), /outgoing JSON-RPC frame exceeds/);
  assert.equal(rpc.pending.size,0);
});
checks.push('outgoing frame bound and pending cleanup');

await withChild('large-fragmented', async rpc => {
  const text = 'café 🦀'.repeat(200000);
  assert.deepEqual(await rpc.request('echo', {text}), {text});
}, {timeout:10000});
checks.push('multi-megabyte fragmented response retains exact Unicode payload');

await withChild('slow-reader', async rpc => {
  const responses = await Promise.all(Array.from({length:20}, (_,number) =>
    rpc.request('ordered', {number,text:'x'.repeat(512*1024)})));
  assert.deepEqual(responses,Array.from({length:20},(_,i)=>i));
  assert.equal(rpc.pending.size,0);
}, {timeout:10000});
checks.push('backpressured multi-frame writes drain in order without loss');

await withChild('hang', async rpc => {
  // The child never consumes enough to permit unbounded producer buffering.
  rpc.child.stdin.cork();
  try {
    for (let i=0;i<16;i++) rpc.notify('queued',{text:'x'.repeat(1024*1024)});
    assert.throws(()=>rpc.notify('overflow',{text:'x'.repeat(1024*1024)}),/outgoing JSON-RPC queue exceeds/);
  } finally { rpc.child.stdin.uncork(); }
});
checks.push('outgoing aggregate queue is bounded while the pipe is blocked');

const summary = {passed:true,checks};
fs.writeFileSync(path.join(directory,'latest.json'), JSON.stringify(summary,null,2)+'\n');
console.log(JSON.stringify(summary,null,2));
