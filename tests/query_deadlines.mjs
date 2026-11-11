// Public request serialization belongs to the overall query deadline, before
// the missing database can produce its ordinary not-ready result.
import assert from 'node:assert/strict';
import {pathToFileURL} from 'node:url';
const {Index} = await import(pathToFileURL(process.argv[2]));
const index = new Index(process.argv[3]);
const waitWord = new Int32Array(new SharedArrayBuffer(4));
let serializationMs;
const result = await index.query({toJSON() {
  const started = performance.now();
  Atomics.wait(waitWord, 0, 0, 60);
  serializationMs = performance.now() - started;
  return {op:'symbols', deadline_ms:25};
}});
assert.ok(serializationMs > 25, 'fixture serialization did not consume the requested deadline');
assert.equal(result.error?.code, 'deadline_exceeded', JSON.stringify(result));
assert.equal((await index.query({op:'symbols'})).error?.code, 'not_ready');
console.log(JSON.stringify({passed:true, check:'Node_public_request_serialization_consumes_overall_deadline', serialization_ms:serializationMs}));
