import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import protobuf from 'protobufjs';

const schema = await protobuf.load(new URL('../proto/analysis.proto', import.meta.url).pathname);
const Envelope = schema.lookupType('knowcode.Envelope');
const AnalyzeRequest = schema.lookupType('knowcode.AnalyzeRequest');
const SdkMetadataRequest = schema.lookupType('knowcode.SdkMetadataRequest');
for (const platform of [undefined, 'AnyCPU']) {
  const value = platform === undefined ? {} : {platform};
  const decoded = SdkMetadataRequest.decode(SdkMetadataRequest.encode(SdkMetadataRequest.create(value)).finish());
  assert.equal(Object.hasOwn(decoded, 'platform'), platform !== undefined,
    'default project platform must survive the Node codec as absent');
  if (platform !== undefined) assert.equal(decoded.platform, platform);
}
for (const configuration of [undefined, 'Release']) {
  const context = configuration === undefined ? {} : {configuration};
  const request = AnalyzeRequest.decode(AnalyzeRequest.encode(AnalyzeRequest.create({
    analysisPolicy: {version: 1, csharp: context}
  })).finish());
  assert.equal(Object.hasOwn(request.analysisPolicy.csharp, 'configuration'), configuration !== undefined,
    'automatic configuration must survive the Node codec as absent');
  if (configuration !== undefined) assert.equal(request.analysisPolicy.csharp.configuration, configuration);
}
const worker = `import {readRequest,send} from './providers/shared/protocol.mjs';
  const request=readRequest(); send('provider',{language:request.configuration});
  send('complete',{success:true});`;
const frame = value => {
  const bytes = Envelope.encode(Envelope.create(value)).finish();
  const header = Buffer.alloc(4);
  header.writeUInt32LE(bytes.length);
  return Buffer.concat([header, bytes]);
};
const valid = frame({protocolVersion:7,jobId:'shared-codec',analyze:{configuration:'rust'}});
const run = input => spawnSync(process.execPath,['--input-type=module','-e',worker],{
  cwd:new URL('..',import.meta.url),input,timeout:5000
});
const result = run(valid);
assert.equal(result.status,0,result.stderr.toString());
let offset=0;
const messages=[];
while(offset<result.stdout.length) {
  const size=result.stdout.readUInt32LE(offset); offset+=4;
  messages.push(Envelope.decode(result.stdout.subarray(offset,offset+size))); offset+=size;
}
assert.equal(messages.length,2);
assert(messages.every(m=>m.protocolVersion===7 && m.jobId==='shared-codec'));
assert.equal(messages[0].provider.language,'rust');
assert.equal(messages[1].complete.success,true);
for(const invalid of [Buffer.alloc(4),Buffer.from([1,0,128,0]),valid.subarray(0,valid.length-1),
  frame({protocolVersion:6,analyze:{}}),frame({protocolVersion:7,complete:{success:true}})]) {
  const failed=run(invalid);
  assert.notEqual(failed.status,0);
  assert.equal(failed.stdout.length,0,'invalid request emitted worker frames');
}
console.log('Shared Node worker framing: valid handshake, bounds, truncation, protocol and payload checks passed.');
