import { createRequire } from 'node:module';
const native = createRequire(import.meta.url)('./know_code.node');
function abortError(signal) {
  const error = new Error('The operation was aborted.', signal?.reason === undefined ? undefined : {cause:signal.reason});
  error.name = 'AbortError';
  error.code = 'ABORT_ERR';
  return error;
}
function checkAborted(signal) { if (signal?.aborted) throw abortError(signal); }
function checkQueryContract() {
  if (typeof native.queryContractVersion !== 'function' || native.queryContractVersion() !== 7
      || typeof native.QueryReservation !== 'function' || typeof native.queryJsonReserved !== 'function'
      || typeof native.queryMaxRequestBytes !== 'function')
    throw new KnowCodeError('update_required', 'Update the Know Code JavaScript and native packages together; shared query admission is missing or incompatible.');
}
/** Select one shared scope for an embedding/server host; default queries use the account scope. */
export class QueryLimitScope {
  constructor(directory) { this.directory = directory; Object.freeze(this); }
}
async function query(database, request, {signal} = {}, queryLimitScope) {
  checkAborted(signal);
  const start = performance.now();
  checkQueryContract();
  // Acquire before invoking user serialization or allocating host-queue work.
  const reservation = new native.QueryReservation(queryLimitScope?.directory);
  let cancel;
  try {
    checkAborted(signal);
    if (reservation.errorJson) return JSON.parse(reservation.errorJson);
    const encoded = JSON.stringify(request);
    const maximum = native.queryMaxRequestBytes();
    if (Buffer.byteLength(encoded, 'utf8') > maximum)
      return {error:{code:'invalid_request',message:`serialized query request exceeds ${maximum} UTF-8 bytes`}};
    checkAborted(signal);
    const cancellation = signal ? new native.QueryCancellation() : undefined;
    if (signal) {
      cancel = () => cancellation.cancel();
      signal.addEventListener('abort', cancel, {once:true});
      checkAborted(signal);
    }
    const elapsed = Math.min(Math.floor(performance.now() - start), 0xffffffff);
    // Both task and wrapper own the charge: abort never detaches native work,
    // and native completion never releases the wrapper's JSON parsing budget.
    const response = JSON.parse(await native.queryJsonReserved(database, encoded, elapsed, reservation, cancellation));
    if (response.error?.code === 'cancelled' && signal) throw abortError(signal);
    return response;
  } finally {
    if (cancel) signal.removeEventListener('abort', cancel);
    reservation.release();
  }
}

export class Index {
  #queryLimitScope;
  constructor(database, queryLimitScope) { this.database = database; this.#queryLimitScope = queryLimitScope; }
  async query(request, options) {
    return query(this.database, request, options, this.#queryLimitScope);
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
  #queryLimitScope;
  constructor(token, descriptor, queryLimitScope) {
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
    this.#queryLimitScope = queryLimitScope;
    this.#executable = descriptor.coordinator_executable;
  }
  static async open(root, options = {}) {
    const {configuration, dataDirectory, coordinatorExecutable} = options;
    const descriptor = await control({op:'open',workspace_root:root,data_directory:dataDirectory,
      coordinator_executable:coordinatorExecutable,configuration});
    return createWorkspace(descriptor, options.queryLimitScope);
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
    return checked(await query(this.#descriptor.status.database, request, options, this.#queryLimitScope));
  }
  close() { this.#closed = true; }
}
function createWorkspace(descriptor, queryLimitScope) { return new Workspace(createWorkspace,descriptor,queryLimitScope); }
