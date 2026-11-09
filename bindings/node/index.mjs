import { createRequire } from 'node:module';
const native = createRequire(import.meta.url)('./know_code.node');

export class Index {
  constructor(database) { this.database = database; }
  async query(request) {
    return JSON.parse(await native.queryJson(this.database, JSON.stringify(request)));
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
  refresh(options = {}) { return this.#operation('refresh',{configuration:options.configuration}); }
  waitForIndex(options = {}) { return this.#operation('wait',{wait_ms:options.waitMs}); }
  async query(request) {
    this.#checkOpen();
    return checked(JSON.parse(await native.queryJson(this.#descriptor.status.database,JSON.stringify(request))));
  }
  close() { this.#closed = true; }
}
function createWorkspace(descriptor) { return new Workspace(createWorkspace,descriptor); }
