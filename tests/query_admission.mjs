// Actual native SDK calls; no mocked native exports or admission policy.
import assert from 'node:assert/strict';
import {pbkdf2} from 'node:crypto';
import {createRequire} from 'node:module';
import {createInterface} from 'node:readline';
import {pathToFileURL} from 'node:url';

const [addon,scope,database,mode,encoded] = process.argv.slice(2);
const native = createRequire(pathToFileURL(addon))('./know_code.node');
const {Index,Workspace,QueryLimitScope} = await import(pathToFileURL(addon));
assert.equal(native.queryContractVersion(),8);
assert.equal(native.queryMaxRequestBytes(),262144);
const index = new Index(database,new QueryLimitScope(scope));
const request = {op:'symbols',name:'admissionTarget'};
const decode = value => JSON.parse(value);
function error(response,code) { assert.equal(response.error?.code,code,JSON.stringify(response)); }
function allFree() {
  const handles=[];
  try {
    for(let i=0;i<4;i++) {
      const handle=new native.QueryReservation(scope);handles.push(handle);
      assert.equal(handle.errorJson,null,'reservation leaked: '+handle.errorJson);
    }
    const rejected=new native.QueryReservation(scope);handles.push(rejected);
    error(decode(rejected.errorJson),'busy');
  } finally { for(const handle of handles) handle.release(); }
}
let details={};
if(mode==='hold') {
  const reservation=new native.QueryReservation(scope);
  try {
    assert.equal(reservation.errorJson,null);
    console.log(JSON.stringify({ready:true,pid:process.pid,contract:8}));
    const lines=createInterface({input:process.stdin});
    for await(const line of lines) { if(line==='release') {lines.close();break;} }
  } finally {reservation.release();}
  console.log(JSON.stringify({released:true}));
} else if(mode==='busy') {
  let serialized=0;
  const hostile={toJSON(){serialized++;throw new Error('busy request serialized');}};
  error(await index.query(hostile),'busy');
  error(await new Index(database).query(hostile),'busy'); // owned env selects default local scope
  const controller=new AbortController();controller.abort('already stopped');
  await assert.rejects(index.query(hostile,{signal:controller.signal}),{name:'AbortError'});
  assert.equal(serialized,0);
  details={serialized,default_scope_busy:true,preabort:true};
} else if(mode==='workspace-busy') {
  const options=decode(encoded);
  const workspace=await Workspace.open(options.root,{dataDirectory:options.data,
    coordinatorExecutable:options.host,queryLimitScope:new QueryLimitScope(scope)});
  let serialized=0;
  try {
    await assert.rejects(workspace.query({toJSON(){serialized++;throw new Error('busy workspace serialized');}}),
      error=>error.code==='busy');
    assert.equal(serialized,0);
    const status=await workspace.status();assert.equal(status.workspace_root,options.root);
    details={serialized,status};
  } finally {workspace.close();}
} else if(mode==='failure-release') {
  const marker=new Error('owned serializer failure');
  await assert.rejects(index.query({toJSON(){throw marker;}}),error=>error===marker);
  allFree();
  const original=JSON.parse;let heldDuringParse=false;
  JSON.parse=function(value,...rest) {
    const handles=[];
    try {
      for(let i=0;i<3;i++) {
        const handle=new native.QueryReservation(scope);handles.push(handle);
        assert.equal(handle.errorJson,null);
      }
      const blocked=new native.QueryReservation(scope);handles.push(blocked);
      assert.equal(original(blocked.errorJson).error.code,'busy');heldDuringParse=true;
    } finally {for(const handle of handles) handle.release();}
    throw marker;
  };
  try {await assert.rejects(index.query(request),error=>error===marker);}
  finally {JSON.parse=original;}
  assert(heldDuringParse);allFree();
  details={serializer_released:true,parse_holds_slot:true,parse_failure_released:true};
} else if(mode==='duplicate-queue') {
  assert.equal(process.env.UV_THREADPOOL_SIZE,'1');
  let blockerDone=false,settled=false;
  const blocker=new Promise((resolve,reject)=>pbkdf2('owned admission','queue',1_000_000,32,'sha256',error=>{
    blockerDone=true;error?reject(error):resolve();
  }));
  const reservation=new native.QueryReservation(scope);
  const other=[];
  try {
    assert.equal(reservation.errorJson,null);
    const pending=native.queryJsonReserved(database,JSON.stringify(request),0,reservation);
    assert.equal(typeof pending.then,'function');
    const done=pending.then(value=>{settled=true;return decode(value);});
    const duplicate=native.queryJsonReserved(database,JSON.stringify(request),0,reservation);
    assert.equal(typeof duplicate,'string','duplicate entered native dispatch');error(decode(duplicate),'invalid_request');
    // Releasing the wrapper must not release native/completion ownership before JS delivery.
    reservation.release();
    for(let i=0;i<3;i++) {
      const handle=new native.QueryReservation(scope);other.push(handle);assert.equal(handle.errorJson,null);
    }
    const blocked=new native.QueryReservation(scope);other.push(blocked);error(decode(blocked.errorJson),'busy');
    const raw=native.queryJsonElapsed(database,JSON.stringify(request),0);
    assert.equal(typeof raw,'string','raw saturated query entered native dispatch');error(decode(raw),'busy');
    for(const handle of other) handle.release();
    const response=await done;assert(settled&&!blockerDone,'query waited for unrelated libuv work');await blocker;
    assert(response.result||response.error?.code==='not_ready',response);allFree();
    details={duplicate_immediate:true,raw_busy_immediate:true,native_completion_kept_charge:true,libuv_independent:true};
  } finally {
    reservation.release();for(const handle of other) handle.release();await blocker;
  }
} else if(mode==='bytes') {
  const maximum=native.queryMaxRequestBytes();
  const make=extra=>{
    const value={op:'symbols',name:''};const overhead=Buffer.byteLength(JSON.stringify(value));
    const available=maximum-overhead;
    value.name='😀'.repeat(Math.floor(available/4))+'x'.repeat(available%4+extra);
    assert.equal(Buffer.byteLength(JSON.stringify(value)),maximum+extra);return value;
  };
  error(await index.query(make(0)),'not_ready');
  error(await index.query(make(1)),'invalid_request');
  allFree();details={exact:maximum,over:maximum+1,non_bmp:true};
} else if(mode==='query') {
  const response=await index.query(encoded?decode(encoded):request);
  details={response};
} else {throw new Error('unknown admission fixture mode: '+mode);}
if(mode!=='hold') console.log(JSON.stringify({passed:true,contract:8,mode,...details}));
