import {spawn} from 'node:child_process';

// Compiler-tool transport shared by Rust LSP and SwiftPM BSP. The worker
// boundary remains Protobuf; storage and queries remain in the Rust core.
export class JsonRpcProcess {
  constructor(program, args, {cwd, env, timeout = 120000, onNotification, onRequest, onFailure} = {}) {
    Object.assign(this,{program,timeout,onNotification,onRequest,onFailure});
    this.nextId = 1;
    this.pending = new Map();
    this.buffer = Buffer.alloc(0);
    this.stderr = '';
    this.child = spawn(program,args,{cwd,env,stdio:['pipe','pipe','pipe']});
    this.child.stderr.on('data', b => { this.stderr = (this.stderr + b.toString()).slice(-16384); });
    this.child.on('error', e => this.fail(new Error(`cannot start ${program}: ${e.message}; install the selected language toolchain`)));
    this.child.on('exit', (code, signal) => this.fail(new Error(`${program} exited (${signal ?? code}): ${this.stderr}`)));
    this.child.stdin.on('error', e => this.fail(e));
    this.child.stdout.on('data', b => {
      try {
        this.buffer = Buffer.concat([this.buffer,b]);
        for (;;) {
          const end = this.buffer.indexOf('\r\n\r\n');
          if (end < 0) {
            if (this.buffer.length > 8192) throw new Error('oversized JSON-RPC header');
            break;
          }
          if (end > 8192) throw new Error('oversized JSON-RPC header');
          const match = /^Content-Length: (\d+)$/im.exec(this.buffer.subarray(0,end).toString('ascii'));
          const size = Number(match?.[1]);
          if (!Number.isSafeInteger(size) || !size || size > 8*1024*1024) throw new Error('invalid/oversized JSON-RPC frame');
          if (this.buffer.length < end+4+size) break;
          const message = JSON.parse(this.buffer.subarray(end+4,end+4+size));
          this.buffer = this.buffer.subarray(end+4+size);
          this.receive(message);
        }
      } catch (e) { this.fail(e); this.terminate(); }
    });
  }
  fail(error) {
    this.failure ??= error;
    for (const p of this.pending.values()) { clearTimeout(p.timer); p.reject(error); }
    this.pending.clear();
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
    this.child.stdin.write(Buffer.concat([Buffer.from(`Content-Length: ${body.length}\r\n\r\n`),body]));
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
