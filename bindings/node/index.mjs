import { createRequire } from 'node:module';
const native = createRequire(import.meta.url)('./know_code.node');
function abortError(signal) {
  const error = new Error('The operation was aborted.', signal?.reason === undefined ? undefined : {cause:signal.reason});
  error.name = 'AbortError';
  error.code = 'ABORT_ERR';
  return error;
}
function checkAborted(signal) { if (signal?.aborted) throw abortError(signal); }
async function query(database, request, {signal} = {}) {
  checkAborted(signal);
  const start = performance.now();
  if (typeof native.queryJsonElapsed !== 'function' || typeof native.queryContractVersion !== 'function' || native.queryContractVersion() !== 3)
    throw new KnowCodeError('update_required', 'Update the Know Code JavaScript and native packages together; cancellable query support is missing or incompatible.');
  const encoded = JSON.stringify(request);
  const elapsed = () => Math.min(Math.floor(performance.now() - start), 0xffffffff);
  if (!signal) return JSON.parse(await native.queryJsonElapsed(database, encoded, elapsed()));
  checkAborted(signal);
  if (typeof native.QueryCancellation !== 'function' || typeof native.queryJsonCancellable !== 'function')
    throw new KnowCodeError('update_required', 'Update the Know Code JavaScript and native packages together; cancellable query exports are missing.');
  const cancellation = new native.QueryCancellation();
  const cancel = () => cancellation.cancel();
  signal.addEventListener('abort', cancel, {once:true});
  try {
    checkAborted(signal);
    // Await the native task itself: cancelling the caller never detaches work.
    const response = JSON.parse(await native.queryJsonCancellable(database, encoded, elapsed(), cancellation));
    if (response.error?.code === 'cancelled') throw abortError(signal);
    return response;
  } finally { signal.removeEventListener('abort', cancel); }
}

export class Index {
  constructor(database) { this.database = database; }
  async query(request, options) {
    return query(this.database, request, options);
  }
}

export class KnowCodeError extends Error {
  constructor(code, message) { super(message); this.name = 'KnowCodeError'; this.code = code; }
}
function checked(response) {
  if (response.error) throw new KnowCodeError(response.error.code, response.error.message);
  return response;
}
async function control(request) {
  if (typeof native.workspaceJson !== 'function')
    throw new KnowCodeError('update_required', 'Update the Know Code JavaScript and native packages together; workspace operations are missing.');
  return checked(JSON.parse(await native.workspaceJson(JSON.stringify(request)))).result;
}
export class Workspace {
  #descriptor;
  #executable;
  #closed = false;
  constructor(token, descriptor) {
    if (token !== createWorkspace) throw new TypeError('Use Workspace.open(root, options).');
    if (descriptor.status.progress) Object.freeze(descriptor.status.progress);
    if (descriptor.status.requested_settings) {
      const settings = descriptor.status.requested_settings;
      if (settings.csharp) Object.freeze(settings.csharp);
      if (settings.node) Object.freeze(settings.node);
      Object.freeze(settings);
    }
    Object.freeze(descriptor.status);
    this.#descriptor = descriptor;
    this.#executable = descriptor.coordinator_executable;
  }
  static async open(root, options = {}) {
    const {configuration, dataDirectory, coordinatorExecutable} = options;
    const descriptor = await control({op:'open',workspace_root:root,data_directory:dataDirectory,
      coordinator_executable:coordinatorExecutable,configuration});
    return createWorkspace(descriptor);
  }
  get root() { return this.#descriptor.status.workspace_root; }
  get initialStatus() { return this.#descriptor.status; }
  #checkOpen() { if (this.#closed) throw new KnowCodeError('closed','Workspace is closed; open a new client.'); }
  async #operation(op, options = {}) {
    this.#checkOpen();
    const result = await control({op,database:this.#descriptor.status.database,
      data_directory:this.#descriptor.data_directory,coordinator_executable:this.#executable,...options});
    return result.status;
  }
  status() { return this.#operation('status'); }
  cancelIndexing() { return this.#operation('cancel'); }
  settings() { return this.#operation('settings'); }
  configure(personalSettings) { return this.#operation('configure',{personal_settings:personalSettings}); }
  refresh(options = {}) { return this.#operation('refresh',{configuration:options.configuration}); }
  waitForIndex(options = {}) { return this.#operation('wait',{wait_ms:options.waitMs}); }
  async query(request, options) {
    checkAborted(options?.signal);
    this.#checkOpen();
    return checked(await query(this.#descriptor.status.database, request, options));
  }
  close() { this.#closed = true; }
}
function createWorkspace(descriptor) { return new Workspace(createWorkspace,descriptor); }
