import {spawn} from 'node:child_process';

// Compiler-tool transport shared by Rust LSP and SwiftPM BSP. The worker
// boundary remains Protobuf; storage and queries remain in the Rust core.
export class JsonRpcProcess {
  constructor(program, args, {cwd, env, timeout = 120000, onNotification, onRequest, onFailure} = {}) {
    Object.assign(this,{program,timeout,onNotification,onRequest,onFailure});
    this.nextId = 1;
    this.pending = new Map();
    this.header = Buffer.allocUnsafe(8196);
    this.headerSize = 0;
    this.body = undefined;
    this.bodySize = 0;
    this.writes = [];
    this.writeHead = 0;
    this.queuedBytes = 0;
    this.stderr = '';
    this.receivedBytes = 0;
    this.child = spawn(program,args,{cwd,env,stdio:['pipe','pipe','pipe']});
    this.child.stderr.on('data', b => { this.stderr = (this.stderr + b.toString()).slice(-16384); });
    this.child.on('error', e => this.fail(new Error(`cannot start ${program}: ${e.message}; install the selected language toolchain`)));
    this.child.on('exit', (code, signal) => this.fail(Object.assign(
      new Error(`${program} exited (${signal ?? code}): ${this.stderr}`), {processExit: {code, signal}})));
    this.child.stdin.on('error', e => this.fail(e));
    this.child.stdin.on('drain', () => {
      try { this.flush(); } catch (error) { this.fail(error); this.terminate(); }
    });
    this.child.stdout.on('data', b => {
      this.receivedBytes += b.length;
      try {
        let offset = 0;
        while (offset < b.length) {
          if (!this.body) {
            this.header[this.headerSize++] = b[offset++];
            if (this.headerSize < 4 || this.header.readUInt32BE(this.headerSize-4) !== 0x0d0a0d0a) {
              if (this.headerSize === this.header.length) throw new Error('oversized JSON-RPC header');
              continue;
            }
            const match = /^Content-Length: (\d+)$/im.exec(this.header.subarray(0,this.headerSize-4).toString('ascii'));
            const size = Number(match?.[1]);
            if (!Number.isSafeInteger(size) || !size || size > 8*1024*1024) throw new Error('invalid/oversized JSON-RPC frame');
            this.body = Buffer.allocUnsafe(size);
            this.bodySize = this.headerSize = 0;
          }
          const size = Math.min(this.body.length-this.bodySize,b.length-offset);
          b.copy(this.body,this.bodySize,offset,offset+size);
          offset += size; this.bodySize += size;
          if (this.bodySize === this.body.length) {
            const message = JSON.parse(this.body);
            this.body = undefined; this.bodySize = 0;
            this.receive(message);
          }
        }
      } catch (e) { this.fail(e); this.terminate(); }
    });
  }
  fail(error) {
    this.failure ??= error;
    for (const p of this.pending.values()) { clearTimeout(p.timer); p.reject(error); }
    this.pending.clear();
    this.writes = []; this.writeHead = this.queuedBytes = 0;
    this.body = undefined; this.bodySize = this.headerSize = 0;
    this.onFailure?.(error);
  }
  receive(message) {
    if (message.method) {
      if (message.id !== undefined) {
        Promise.resolve().then(() => this.onRequest?.(message.method,message.params) ?? null)
          .then(result => this.write({id:message.id,result:result ?? null}))
          .catch(error => { this.fail(error); this.terminate(); });
      } else this.onNotification?.(message.method,message.params);
      return;
    }
    const pending = this.pending.get(message.id);
    if (!pending) return;
    this.pending.delete(message.id); clearTimeout(pending.timer);
    if (message.error) {
      const error = new Error(`${this.program} ${pending.method}: ${JSON.stringify(message.error)}`);
      error.rpcCode = message.error.code;
      error.rpcData = message.error.data;
      pending.reject(error);
    } else pending.resolve(message.result);
  }
  write(value) {
    if (this.failure) throw this.failure;
    const body = Buffer.from(JSON.stringify({jsonrpc:'2.0',...value}));
    if (body.length > 8*1024*1024) throw new Error('resource-limited: outgoing JSON-RPC frame exceeds 8 MiB');
    const header = Buffer.from(`Content-Length: ${body.length}\r\n\r\n`);
    // Include Node's submitted-but-undrained bytes in the same finite bound.
    // Never keep submitting frames after write(false), or queue without limit.
    if (this.queuedBytes + this.child.stdin.writableLength + header.length + body.length > 16*1024*1024+16384)
      throw new Error('resource-limited: outgoing JSON-RPC queue exceeds 16 MiB');
    const frame = Buffer.concat([header,body]);
    this.writes.push(frame); this.queuedBytes += frame.length;
    this.flush();
  }
  flush() {
    if (this.failure || this.child.stdin.writableNeedDrain) return;
    while (this.writeHead < this.writes.length) {
      const frame = this.writes[this.writeHead];
      this.writes[this.writeHead++] = undefined;
      this.queuedBytes -= frame.length;
      if (!this.child.stdin.write(frame)) break;
    }
    if (this.writeHead === this.writes.length) { this.writes = []; this.writeHead = 0; }
    else if (this.writeHead >= 1024 && this.writeHead >= this.writes.length/2) {
      this.writes = this.writes.slice(this.writeHead); this.writeHead = 0;
    }
  }
  notify(method, params) { this.write({method,params}); }
  request(method, params) {
    const id = this.nextId++;
    return new Promise((resolve,reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`${this.program} timed out: ${method}`)); this.terminate();
      },this.timeout);
      this.pending.set(id,{resolve,reject,timer,method});
      try { this.write({id,method,params}); }
      catch (e) { clearTimeout(timer); this.pending.delete(id); reject(e); }
    });
  }
  terminate() { this.child.kill(); }
}
