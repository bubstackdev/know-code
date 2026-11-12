import {JsonRpcProcess} from '../shared/jsonrpc.mjs';

// Dependency analysis may use installed rustup shims, but must never provision
// a missing repository-selected compiler as a side effect of invoking them.
export const compilerEnvironment = ({restore = false} = {}) => ({...process.env,
  // Only deliberate dependency fetching drops our forced offline setting;
  // user-selected Cargo offline settings remain authoritative during fetch.
  ...(!restore ? {CARGO_NET_OFFLINE: 'true'} : {}), RUSTUP_AUTO_INSTALL: '0', CARGO_BUILD_JOBS: '1'});

// Only Rust readiness/diagnostic policy lives here; SwiftPM shares transport.
export class Analyzer extends JsonRpcProcess {
  constructor(cwd, environment = {}) {
    super('rust-analyzer', [], {cwd, env: {...compilerEnvironment(), ...environment}});
    this.diagnostics = new Map();
    this.onRequest = (method, params) => method === 'workspace/configuration' ? params.items.map(() => null) : null;
    this.onFailure = error => this.readyReject?.(error);
    this.onNotification = (method, params) => {
      // Preserve saved diagnostics when didClose clears client publication.
      if (method === 'textDocument/publishDiagnostics' && params.diagnostics.length)
        this.diagnostics.set(params.uri, params.diagnostics);
      if (method === 'experimental/serverStatus' && params.quiescent) {
        this.status = params;
        this.readyResolve?.(params);
      }
    };
  }
  async ready() {
    if (this.status) return this.status;
    let timer;
    try {
      return await new Promise((resolve, reject) => {
        timer = setTimeout(() => reject(new Error('rust-analyzer project loading timed out')), 120000);
        this.readyResolve = resolve;
        this.readyReject = reject;
        if (this.failure) reject(this.failure);
      });
    } finally { clearTimeout(timer); this.readyResolve = this.readyReject = undefined; }
  }
  async close() {
    try { if (!this.failure) { await this.request('shutdown', null); this.notify('exit'); } }
    finally { this.terminate(); }
  }
}
