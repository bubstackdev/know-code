// Real SDK/native calls only. Queue mode runs with UV_THREADPOOL_SIZE=1.
import assert from 'node:assert/strict';
import {pbkdf2} from 'node:crypto';
import {existsSync} from 'node:fs';
import {createRequire} from 'node:module';
import {pathToFileURL} from 'node:url';

const [addon,database,mode,encoded,delay] = process.argv.slice(2);
const native = createRequire(pathToFileURL(addon))('./know_code.node');
assert.equal(native.queryContractVersion(),7,'matching cancellation-native contract required');
const {Index} = await import(pathToFileURL(addon));
const index = new Index(database);
const reason = 'owned query cancellation fixture';
function aborted(error) {
  assert(error instanceof Error,error);
  assert.equal(error.name,'AbortError');assert.equal(error.code,'ABORT_ERR');assert.equal(error.cause,reason);
  return {name:error.name,code:error.code,cause:error.cause};
}
let details;
if (mode === 'preabort') {
  assert(!existsSync(database),'preabort needs a missing index');
  const controller = new AbortController();controller.abort(reason);
  let serialized = false;
  const request = {op:'symbols',toJSON() {serialized=true;throw new Error('preabort serialized the request');}};
  let failure;
  try {await index.query(request,{signal:controller.signal});}
  catch (error) {failure=aborted(error);}
  assert(failure && !serialized && !existsSync(database),'preabort opened/serialized an index request');
  details={error:failure,index_created:false,serialized};
} else if (mode === 'queue') {
  assert.equal(process.env.UV_THREADPOOL_SIZE,'1');assert(!existsSync(database));
  let blockerDone=false,settled=false,settledBeforeBlocker=false;
  const blocker = new Promise((resolve,reject) => pbkdf2('owned fixture','cancel',1_000_000,32,'sha256',error => {
    blockerDone=true;error?reject(error):resolve();
  }));
  const controller=new AbortController();
  const pending=index.query({op:'symbols',deadline_ms:5000},{signal:controller.signal}).then(
    response => {settled=true;settledBeforeBlocker=!blockerDone;return {response};},
    error => {settled=true;settledBeforeBlocker=!blockerDone;return {error};});
  await new Promise((resolve,reject) => setTimeout(() => {
    try {assert(!blockerDone,'PBKDF2 did not hold the actual libuv queue');controller.abort(reason);}
    catch(error) {reject(error);return;}
    setImmediate(() => {
      try {assert(!settled || blockerDone,'query detached from queued native work');resolve();}
      catch(error) {reject(error);}
    });
  },5));
  const outcome=await pending;await blocker;
  assert(!settledBeforeBlocker,'cancelled query settled before the occupied libuv slot drained');
  const failure=aborted(outcome.error);assert(!existsSync(database),'cancelled queued query opened index');
  const recovery=await index.query({op:'symbols'});assert.equal(recovery.error?.code,'not_ready');
  details={error:failure,recovery,blocker_done:blockerDone};
} else if (mode === 'query') {
  const request=JSON.parse(encoded),controller=new AbortController(),started=performance.now();
  const pending=index.query(request,{signal:controller.signal});
  const timer=setTimeout(() => controller.abort(reason),Number(delay));
  let failure;
  try {const response=await pending;assert.fail('amplified SQL completed before cancellation: '+JSON.stringify(response));}
  catch(error) {failure=aborted(error);assert(controller.signal.aborted);}
  finally {clearTimeout(timer);}
  details={error:failure,elapsed_ms:performance.now()-started};
} else if (mode === 'recover') {
  const request=JSON.parse(encoded),responses=[];
  for(let i=0;i<3;i++) {
    const controller=new AbortController();
    const response=await index.query(request,{signal:controller.signal});assert(response.result,response);
    controller.abort(reason); // A completed query is not retroactively cancelled.
    const next=await index.query(request);assert.deepEqual(next,response);
    responses.push(response,next);
  }
  details={responses,late_cancels:3};
} else {throw new Error('unknown cancellation fixture mode');}
console.log(JSON.stringify({passed:true,contract:7,mode,...details}));
