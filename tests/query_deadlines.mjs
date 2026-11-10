// Start in an isolated process with UV_THREADPOOL_SIZE=1. Real libuv work owns
// the sole slot before the native query is submitted; no product test hooks.
import assert from 'node:assert/strict';
import {pbkdf2} from 'node:crypto';
import {pathToFileURL} from 'node:url';
const {Index} = await import(pathToFileURL(process.argv[2]));
const index = new Index(process.argv[3]);
const started = performance.now();
const blocker = new Promise((resolve, reject) => {
  pbkdf2('owned fixture', 'deadline', 1_000_000, 32, 'sha256', error => error ? reject(error) : resolve());
});
const result = await index.query({op:'symbols', deadline_ms:25});
await blocker;
assert.ok(performance.now() - started > 25, 'fixture did not hold the queue long enough');
assert.equal(result.error?.code, 'deadline_exceeded', JSON.stringify(result));
assert.equal((await index.query({op:'symbols'})).error?.code, 'not_ready');
console.log(JSON.stringify({passed:true, check:'Node_libuv_queue_consumes_overall_deadline'}));
