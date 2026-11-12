// Dedicated test-hooks add-on required. Never run against or replace a live install.
import assert from 'node:assert/strict';
import {createRequire} from 'node:module';
import {readFile, mkdir, writeFile} from 'node:fs/promises';
import {createHash, pbkdf2} from 'node:crypto';
import {promisify} from 'node:util';
import {resolve, dirname} from 'node:path';
import {fileURLToPath} from 'node:url';
import {Worker, isMainThread, parentPort, workerData} from 'node:worker_threads';
const require = createRequire(import.meta.url);
const delay = ms => new Promise(resolve => setTimeout(resolve,ms));
const decode = value => JSON.parse(value);
const internal = value => {
  assert(Buffer.byteLength(value,'utf8') < 512);
  const response=decode(value);assert.equal(response.error?.code,'internal_error');
  assert(!response.error.message.includes('owned'));return response;
};
async function until(check, message, milliseconds=5000) {
  const end=performance.now()+milliseconds;
  while(!check()) {assert(performance.now()<end,message);await delay(5);}
}
if(!isMainThread) {
  const native=require(workerData.addon);
  const gate=native.testCreateGate();
  native.testQuery(workerData.scope,gate,false).catch(()=>{});
  parentPort.postMessage({gate});
} else {
  const [diagnosticPath,defaultPath,scopePath,evidencePath]=process.argv.slice(2);
  assert(diagnosticPath&&defaultPath&&scopePath&&evidencePath,'pass diagnostic addon, default addon, owned scope, evidence directory');
  assert(!process.env.UV_THREADPOOL_SIZE||process.env.UV_THREADPOOL_SIZE==='4','run the default four-thread pool; the fixture does not change the host setting');
  assert.equal(Number(process.versions.node.split('.')[0]),24);
  const addon=resolve(diagnosticPath),production=resolve(defaultPath),scope=resolve(scopePath),evidence=resolve(evidencePath);
  assert.notEqual(addon,production,'diagnostic artifact must not replace default output');
  await mkdir(scope,{recursive:true});await mkdir(evidence,{recursive:true});
  const defaultNative=require(production);
  assert.equal(defaultNative.queryContractVersion(),9);
  assert.deepEqual(Object.keys(defaultNative).filter(name=>/^test[A-Z]/.test(name)),[],'default native artifact exposes diagnostic hooks');
  const native=require(addon);assert.equal(native.queryContractVersion(),9);
  assert.equal(typeof native.testQuery,'function');
  const checks=[];const details={};const gates=new Set();
  const gate=()=>{const id=native.testCreateGate();gates.add(id);return id;};
  const release=id=>native.testReleaseGate(id);
  const drop=id=>{native.testDropGate(id);gates.delete(id);};
  const allFree=()=>{
    const leases=[];
    try {
      for(let i=0;i<4;i++) {const lease=new native.QueryReservation(scope);leases.push(lease);if(lease.errorJson)return false;}
      const extra=new native.QueryReservation(scope);leases.push(extra);
      assert.equal(decode(extra.errorJson).error.code,'busy');return true;
    } finally {for(const lease of leases)lease.release();}
  };
  try {
    internal(native.testSyncPanic());checks.push('synchronous_core_panic_returns_bounded_internal_error');
    for(const mode of ['query','workspace']) {
      const id=gate();
      const result=mode==='query'?native.testQuery(scope,id,true):native.testControl(scope,id,true,false);
      await until(()=>native.testGateEntered(id)===1,`${mode} panic task did not enter`);
      release(id);internal(await result);drop(id);
      await until(allFree,'panic retained query admission');
      await until(()=>Object.values(decode(native.testControlCounts())).every(value=>value===0),'panic retained control admission');
      checks.push(`${mode}_compute_panic_keeps_Node_alive_and_releases_ownership`);
    }
    // Both runs use the real NativeTask::compute method. Only the dispatch is
    // different. Four held queries establish default libuv worker saturation.
    for(const legacy of [true,false]) {
      const id=gate();let pending=[];let fsDone=false,cryptoDone=false;const start=performance.now();
      try {
        for(let i=0;i<4;i++) pending.push(legacy?native.testLegacyQuery(scope,id):native.testQuery(scope,id,false));
        pending.push(legacy?native.testLegacyControl(scope,id):native.testControl(scope,id,false,false));
        await until(()=>native.testGateEntered(id)===(legacy?4:5),'native tasks did not enter bounded gate');
        const fs=readFile(fileURLToPath(import.meta.url)).then(()=>{fsDone=true;});
        const crypto=promisify(pbkdf2)('owned native gate','salt',1000,32,'sha256').then(()=>{cryptoDone=true;});
        if(legacy) {
          await delay(100);assert(!fsDone&&!cryptoDone,'legacy pool did not demonstrate fs/crypto starvation');
        } else {
          await until(()=>fsDone&&cryptoDone,'admitted execution starved host fs/crypto');
          assert.equal(native.testGateEntered(id),5);
        }
        details[legacy?'legacy_libuv':'admitted_threads']={held_queries:4,workspace_wait:1,entered:native.testGateEntered(id),fs_progress_before_release:fsDone,crypto_progress_before_release:cryptoDone,elapsed_ms:performance.now()-start};
        release(id);await Promise.all([...pending,fs,crypto]);pending=[];drop(id);
      } finally {if(gates.has(id))release(id);await Promise.allSettled(pending);if(gates.has(id))drop(id);}
      await until(allFree,'dispatch case retained query ownership');
    }
    checks.push('legacy_default_libuv_starvation_reproduced_and_admitted_execution_allows_fs_crypto_progress');
    {
      const id=gate();const pending=[];
      try {
        pending.push(native.testControl(scope,id,false,false),native.testControl(scope,id,false,false));
        await until(()=>native.testGateEntered(id)===2,'ordinary controls did not enter');
        let result=native.workspaceJson(JSON.stringify({op:'wait',data_directory:scope}));
        assert.equal(typeof result,'string');assert.equal(decode(result).error.code,'busy');
        pending.push(native.testControl(scope,id,false,true),native.testControl(scope,id,false,true));
        await until(()=>native.testGateEntered(id)===4,'urgent capacity was consumed by waits');
        result=native.workspaceJson(JSON.stringify({op:'status',data_directory:scope}));
        assert.equal(typeof result,'string');assert.equal(decode(result).error.code,'busy');
        assert.deepEqual(decode(native.testControlCounts()),{normal:2,urgent:2});
        assert(allFree(),'control work consumed graph slots');
        release(id);await Promise.all(pending);drop(id);
      } finally {if(gates.has(id))release(id);await Promise.allSettled(pending);if(gates.has(id))drop(id);}
      await until(()=>Object.values(decode(native.testControlCounts())).every(value=>value===0),'control ownership did not drain');
      checks.push('bounded_ordinary_control_and_reserved_status_cancel_capacity_no_queue');
    }
    {
      const oversized=' '.repeat(native.queryMaxRequestBytes()+1);
      const response=native.workspaceJson(oversized);
      assert.equal(typeof response,'string');assert.equal(decode(response).error.code,'invalid_request');
      assert.deepEqual(decode(native.testControlCounts()),{normal:0,urgent:0});
      checks.push('control_byte_ceiling_precedes_parse_and_dispatch');
    }
    {
      const worker=new Worker(new URL(import.meta.url),{workerData:{addon,scope}});
      let id;
      try {
        id=await new Promise((resolve,reject)=>{worker.once('message',message=>resolve(message.gate));worker.once('error',reject);});
        gates.add(id);
        await until(()=>native.testGateEntered(id)===1,'worker native query did not start');
        assert(!allFree(),'worker did not retain its graph slot');
        await worker.terminate();
        await until(allFree,'environment shutdown did not cancel/release native ownership');
        drop(id);
      } finally {await worker.terminate();if(id&&gates.has(id)){release(id);drop(id);}}
      checks.push('worker_environment_shutdown_cancels_owned_query_and_fences_late_settlement');
    }
    const hash=async path=>createHash('sha256').update(await readFile(path)).digest('hex');
    const result={status:'passed',contract:9,node:process.versions.node,scope,checks,details,
      artifacts:{[addon]:await hash(addon),[production]:await hash(production)},
      harness_sha256:await hash(fileURLToPath(import.meta.url)),
      limits:['finite test-only native gate; not a query latency or encoding-contention SLO','separate diagnostic add-on; default artifact verified without test exports','no coordinator, live workspace, install or account admission scope used']};
    await writeFile(resolve(evidence,'validation.json'),JSON.stringify(result,null,2)+'\n');
    process.stdout.write(JSON.stringify(result)+'\n');
  } finally {for(const id of gates){release(id);drop(id);}}
}
