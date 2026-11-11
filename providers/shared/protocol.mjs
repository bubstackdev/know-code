import fs from 'node:fs';
import {fileURLToPath} from 'node:url';
import protobuf from 'protobufjs';

export const VERSION = 7;
export const MAX_FRAME = 8 * 1024 * 1024;
const schema = await protobuf.load(fileURLToPath(new URL('../../proto/analysis.proto', import.meta.url)));
const Envelope = schema.lookupType('knowcode.Envelope');
let jobId;

function readExact(count) {
  const buffer = Buffer.alloc(count);
  for (let offset = 0; offset < count;) {
    const n = fs.readSync(0, buffer, offset, count - offset);
    if (!n) throw new Error('truncated analysis request');
    offset += n;
  }
  return buffer;
}

// Workers handle one request per process. Compiler adapters never own storage.
export function readRequest() {
  if (jobId !== undefined) throw new Error('worker already received an analysis request');
  const length = readExact(4).readUInt32LE();
  if (!length || length > MAX_FRAME) throw new Error('invalid request frame length');
  const envelope = Envelope.decode(readExact(length));
  if (envelope.protocolVersion !== VERSION || !envelope.analyze)
    throw new Error('update required: incompatible worker protocol');
  jobId = envelope.jobId;
  return envelope.analyze;
}

// Encoded size includes the same envelope/identity metadata as send(). This
// does not allocate a full encoded frame or change the transport's hard cap.
export function canSend(kind,value) {
  const size=Envelope.encode(Envelope.create({protocolVersion: VERSION,jobId:jobId??'', [kind]:value})).len;
  return size>0&&size<=MAX_FRAME;
}

export function send(kind, value) {
  if (jobId === undefined) throw new Error('worker has no analysis request');
  const bytes = Envelope.encode(Envelope.create({protocolVersion: VERSION, jobId, [kind]: value})).finish();
  if (!bytes.length || bytes.length > MAX_FRAME)
    throw new Error(`resource-limited: frame exceeds ${MAX_FRAME} bytes`);
  const header = Buffer.alloc(4);
  header.writeUInt32LE(bytes.length);
  // Pipes can accept partial writes. Keep stdout exclusively framed Protobuf.
  for (const buffer of [header, bytes]) {
    for (let offset = 0; offset < buffer.length;)
      offset += fs.writeSync(1, buffer, offset, buffer.length - offset);
  }
}
