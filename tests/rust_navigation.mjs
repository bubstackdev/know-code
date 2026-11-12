import assert from 'node:assert/strict';
import {setImmediate as tick} from 'node:timers/promises';
import {orderedDefinitions,ExternalSources} from '../providers/rust/navigation.mjs';

let active=0,peak=0,completed=0;
const pending=new Map(),output=[];
const ordered=(async()=>{
  for await(const result of orderedDefinitions([0,1,2,3,4,5,6,7,8],item=>new Promise(resolve=>{
    active++;peak=Math.max(peak,active);
    pending.set(item,()=>{active--;completed++;resolve(`definition-${item}`);});
  })))output.push(result);
})();
for(const window of [[3,2,1,0],[7,6,5,4],[8]]) {
  await tick();
  assert.deepEqual([...pending.keys()],window.toReversed());
  for(const id of window){pending.get(id)();pending.delete(id);}
}
await ordered;
assert.equal(peak,4);assert.equal(active,0);assert.equal(completed,9);
assert.deepEqual(output,Array.from({length:9},(_,item)=>({item,definitions:`definition-${item}`})));

for(const cancellation of [false,true]) {
  const controller=new AbortController(),unhandled=[],started=[];let settled=0;
  const listener=error=>unhandled.push(error);process.on('unhandledRejection',listener);
  try {
    const result=(async()=>{
      for await(const value of orderedDefinitions([0,1,2,3,4],item=>{
        started.push(item);
        return new Promise((resolve,reject)=>{
          if(cancellation)controller.signal.addEventListener('abort',()=>{settled++;reject(new Error('cancelled'));},{once:true});
          else setImmediate(()=>{settled++;item===1?reject(new Error('definition failed')):resolve(item);});
        });
      }))throw new Error(`unexpected result ${value.item}`);
    })();
    // Observe the iterator immediately, before any deferred rejection.
    const rejected=assert.rejects(result,cancellation?/cancelled/:/definition failed/);
    await tick();if(cancellation)controller.abort();
    await rejected;await tick();
    assert.deepEqual(started,[0,1,2,3]);assert.equal(settled,4);assert.deepEqual(unhandled,[]);
  } finally {process.off('unhandledRejection',listener);}
}
let started=0;
function* broken(){yield 0;throw new Error('input failed');}
await assert.rejects(async()=>{for await(const unused of orderedDefinitions(broken(),()=>{started++;})){}},/input failed/);
assert.equal(started,0);
let finished=0;
for await(const unused of orderedDefinitions([0,1,2,3,4],async()=>{finished++;return 1;}))break;
assert.equal(finished,4,'early return left no in-flight window or extra requests');

const source=text=>({filename:'external.rs',artifact:{utf8:Buffer.from(text)},text,offsets:new Uint32Array(text.length+1),lines:[0]});
const cache=new ExternalSources(1500);
cache.set('a',source('one'));cache.set('b',source('two'));
assert(cache.get('a'));cache.set('c',source('three'));
assert.equal(cache.get('b'),undefined);assert(cache.get('a'));assert(cache.get('c'));
const transient=source('x'.repeat(2000));cache.set('huge',transient);
assert.equal(cache.get('huge'),undefined);assert(cache.bytes<=1500);
const countBound=new ExternalSources();for(let i=0;i<70;i++)countBound.set(String(i),source(''));
assert.equal(countBound.sources.size,64);assert.equal(countBound.get('0'),undefined);
console.log('Rust navigation: ordered finite windows, observed failures/cancellation and bounded external-source retention passed.');
