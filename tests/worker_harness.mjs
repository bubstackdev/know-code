// One focused-test codec for every Node provider; no Python Protobuf dependency.
import fs from 'node:fs';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import {spawn} from 'node:child_process';
import protobuf from 'protobufjs';

const root = fileURLToPath(new URL('..', import.meta.url));
const schema = await protobuf.load(new URL('../proto/analysis.proto', import.meta.url).pathname);
const Envelope = schema.lookupType('knowcode.Envelope');
const jobId = 'provider-test';
const request = JSON.parse(fs.readFileSync(0,'utf8'));
const bytes = Envelope.encode(Envelope.create({protocolVersion:7,jobId,analyze:request})).finish();
if (!bytes.length || bytes.length>8*1024*1024) throw new Error('invalid test request size');
const header = Buffer.alloc(4); header.writeUInt32LE(bytes.length);
const timeout = Number(process.env.KNOW_CODE_TEST_TIMEOUT_MS ?? 180000);
if (!Number.isSafeInteger(timeout) || timeout<=0 || timeout>3600000) throw new Error('invalid test worker timeout');
const sink = process.env.KNOW_CODE_TEST_SINK && path.resolve(process.env.KNOW_CODE_TEST_SINK);
let output;
if (sink) {
  fs.mkdirSync(sink,{recursive:true});
  output = fs.openSync(path.join(sink,'messages.jsonl'),'wx');
}
const messages = [];
let summaryBytes=0, summaryTruncated=false;
const counts = {frames:0,wireBytes:0,compilations:0,sources:0,sourceBytes:0,symbols:0,occurrences:0,relationships:0,reports:{}};
function accept(message) {
  if (message.protocolVersion!==7 || message.jobId!==jobId) throw new Error('worker protocol/job identity mismatch');
  counts.frames++;
  if (message.compilation) counts.compilations++;
  if (message.source) {
    counts.sources++;
    const encoded=message.source.utf8;
    counts.sourceBytes+=encoded.length*3/4-(encoded.endsWith('==')?2:encoded.endsWith('=')?1:0);
  }
  if (message.facts) for (const key of ['symbols','occurrences','relationships']) counts[key]+=message.facts[key].length;
  if (message.report) counts.reports[message.report.status]=(counts.reports[message.report.status]??0)+1;
  if (!sink) { messages.push(message); return; }
  fs.writeFileSync(output,JSON.stringify(message)+'\n');
  // Full options, diagnostics, source and graph remain in JSONL. Stdout keeps
  // bounded context/status summaries so large corpora do not fill host RAM.
  let compact;
  if (message.compilation) {
    const {optionsJson,inputPaths,...compilation}=message.compilation;
    compact={compilation:{...compilation,inputPathsCount:inputPaths.length},optionsInJsonl:true};
  } else if (message.report) {
    compact={report:{...message.report,diagnostics:message.report.diagnostics.slice(0,10).map(d=>({...d,message:d.message.slice(0,2048)}))},diagnosticsInJsonl:true};
  } else if (message.preparation) {
    compact={preparation:{...message.preparation,error:message.preparation.error.slice(0,2048)}};
  } else if (message.provider || message.complete) compact=message;
  if (compact) {
    const size=Buffer.byteLength(JSON.stringify(compact));
    if (message.complete || summaryBytes+size<=4*1024*1024) { messages.push(compact); summaryBytes+=size; }
    else summaryTruncated=true;
  }
}

const child=spawn(process.execPath,[process.argv[2]],{cwd:root,detached:true,stdio:['pipe','pipe','pipe']});
let error,stderr='',headerSize=0,body,bodySize=0;
const prefix=Buffer.alloc(4);
function terminate() { if (child.pid) { try { process.kill(-child.pid,'SIGTERM'); } catch { /* Already exited. */ } } }
function fail(cause) { error??=cause.message; terminate(); }
child.on('error',fail);
child.stdin.on('error',fail);
child.stderr.on('data',chunk=>{ stderr=(stderr+chunk.toString()).slice(-16384); });
child.stdout.on('data',chunk=>{
  if (error) return;
  try {
    counts.wireBytes+=chunk.length;
    if (!sink && counts.wireBytes>64*1024*1024) throw new Error('test capture exceeds 64 MiB; set KNOW_CODE_TEST_SINK for streaming evidence');
    let offset=0;
    while (offset<chunk.length) {
      if (!body) {
        const size=Math.min(4-headerSize,chunk.length-offset);
        chunk.copy(prefix,headerSize,offset,offset+size); headerSize+=size; offset+=size;
        if (headerSize<4) continue;
        const length=prefix.readUInt32LE();
        if (!length || length>8*1024*1024) throw new Error('invalid worker frame');
        body=Buffer.allocUnsafe(length); bodySize=0; headerSize=0;
      }
      const size=Math.min(body.length-bodySize,chunk.length-offset);
      chunk.copy(body,bodySize,offset,offset+size); bodySize+=size; offset+=size;
      if (bodySize===body.length) {
        accept(Envelope.toObject(Envelope.decode(body),{longs:Number,bytes:String,defaults:true}));
        body=undefined; bodySize=0;
      }
    }
  } catch (cause) { fail(cause); }
});
const timer=setTimeout(()=>fail(new Error('test worker timed out')),timeout);
child.stdin.end(Buffer.concat([header,bytes]));
const worker=await new Promise(resolve=>child.on('close',(code,signal)=>resolve({code,signal})));
clearTimeout(timer);
terminate(); // Clean up descendants if a failed worker left a compiler alive.
if (!error && (headerSize || body)) error='truncated worker frame';
if (output!==undefined) fs.closeSync(output);
const result={code:error?1:worker.code,error,stderr,messages};
if (sink) Object.assign(result,{sink,counts,summaryTruncated,workerCode:worker.code,signal:worker.signal});
console.log(JSON.stringify(result));
