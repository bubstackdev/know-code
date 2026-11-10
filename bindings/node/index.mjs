import { createRequire } from 'node:module';
const native = createRequire(import.meta.url)('./know_code.node');
function query(database, request) {
  const start = performance.now();
  if (typeof native.queryJsonElapsed !== 'function' || typeof native.queryContractVersion !== 'function' || native.queryContractVersion() !== 2)
    throw new KnowCodeError('update_required', 'Update the Know Code JavaScript and native packages together; bounded cursor query support is missing or incompatible.');
  const encoded = JSON.stringify(request);
  const elapsed = Math.min(Math.floor(performance.now() - start), 0xffffffff);
  return native.queryJsonElapsed(database, encoded, elapsed);
}

export class Index {
  constructor(database) { this.database = database; }
  async query(request) {
    return JSON.parse(await query(this.database, request));
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
  async query(request) {
    this.#checkOpen();
    return checked(JSON.parse(await query(this.#descriptor.status.database, request)));
  }
  close() { this.#closed = true; }
}
function createWorkspace(descriptor) { return new Workspace(createWorkspace,descriptor); }
