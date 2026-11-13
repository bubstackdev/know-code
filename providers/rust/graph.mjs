// Rust syntax/semantic evidence only; traversal and storage remain in the core.
// The analyzer's syntax-tree extension supplies grammar roles and byte ranges.
// A reference is never promoted to a call without outgoing-call evidence too.
const OWNERS = new Set(['FN', 'CONST', 'STATIC', 'STRUCT', 'ENUM', 'TRAIT', 'TYPE_ALIAS', 'MOD']);
const TYPES = new Set(['PATH_TYPE', 'REF_TYPE', 'PTR_TYPE', 'ARRAY_TYPE', 'SLICE_TYPE', 'TUPLE_TYPE', 'FN_PTR_TYPE', 'IMPL_TRAIT_TYPE', 'DYN_TRAIT_TYPE']);
const POSITIONAL = new Set(['CALL_EXPR', 'METHOD_CALL_EXPR']);
const callable = node => node.kind === 'FN' || node.kind === 'CONST' || node.kind === 'STATIC';
const within = (outer, start, end) => outer.start <= start && end <= outer.end;
const DECLARATIONS = {
  function: ['FN'], method: ['FN'], const: ['CONST'], static: ['STATIC'],
  struct: ['STRUCT'], enum: ['ENUM'], interface: ['TRAIT'], union: ['UNION'],
  class: ['STRUCT', 'ENUM', 'UNION'], type: ['STRUCT', 'ENUM', 'TRAIT', 'TYPE_ALIAS', 'TYPE_PARAM', 'UNION'],
  typeParameter: ['TYPE_PARAM'], constParameter: ['CONST_PARAM'], property: ['RECORD_FIELD'], enumMember: ['VARIANT'],
  variable: ['IDENT_PAT'], parameter: ['IDENT_PAT', 'SELF_PARAM'], selfKeyword: ['SELF_PARAM'],
  selfTypeKeyword: ['STRUCT', 'ENUM', 'TRAIT', 'TYPE_ALIAS', 'TYPE_PARAM', 'UNION'],
  namespace: ['MOD', 'SOURCE_FILE'], macro: ['MACRO_RULES', 'MACRO_DEF', 'FN'], procMacro: ['FN'], derive: ['FN']
};

export const authoredDeclaration = (anchors, start, end, kind) => DECLARATIONS[kind]?.includes(anchors.get(`${start}:${end}`)) ?? false;

export class RustGraph {
  constructor(source, encoded, compilerText = source.text) {
    // The analyzer may normalize CRLF internally. Its syntax positions carry
    // UTF-8 byte columns, unlike LSP's negotiated UTF-16 columns. Translate
    // those byte columns against the retained original source's line starts.
    const compilerBytes = Buffer.from(compilerText);
    const compilerLines = [0];
    for (let i = 0; i < compilerBytes.length; i++) if (compilerBytes[i] === 10) compilerLines.push(i + 1);
    const byte = position => {
      const [offset, line, column] = position ?? [];
      if (!Number.isSafeInteger(offset) || !Number.isSafeInteger(line) || !Number.isSafeInteger(column) ||
        line < 0 || column < 0 || compilerLines[line] === undefined || compilerLines[line] + column !== offset ||
        offset < 0 || offset > compilerBytes.length || source.lines[line] === undefined)
        throw new Error('invalid rust-analyzer syntax source position');
      return source.offsets[source.lines[line]] + column;
    };
    const tree = typeof encoded === 'string' ? JSON.parse(encoded) : encoded;
    if (tree?.kind !== 'SOURCE_FILE' || byte(tree.start) !== 0 || byte(tree.end) !== source.artifact.utf8.length)
      throw new Error('rust-analyzer syntax tree does not match the saved source snapshot');
    this.source = source;
    this.owners = [];
    const stack = [{raw: tree, parent: undefined}];
    while (stack.length) {
      const {raw, parent} = stack.pop();
      // The analyzer may parse Rust examples inside string literals. Those
      // mapped inner nodes are editor presentation, not executable source.
      if (raw.istart !== undefined || raw.iend !== undefined) continue;
      const start = byte(raw.start), end = byte(raw.end);
      if (!Number.isSafeInteger(start) || !Number.isSafeInteger(end) || start < 0 || end < start || end > source.artifact.utf8.length)
        throw new Error('invalid rust-analyzer syntax tree source range');
      const node = {kind: raw.kind, start, end, children: []};
      if (typeof node.kind !== 'string' || parent && !within(parent, start, end)) throw new Error('invalid rust-analyzer syntax nesting');
      if (parent) parent.children.push(node); else this.root = node;
      if (raw.type === 'Node' && OWNERS.has(node.kind)) this.owners.push(node);
      const children = raw.type === 'Node' ? raw.children : [];
      if (children !== undefined && !Array.isArray(children)) throw new Error('invalid rust-analyzer syntax tree children');
      for (let i = (children?.length ?? 0) - 1; i >= 0; i--) stack.push({raw: children[i], parent: node});
    }
    for (const owner of this.owners) {
      const name = owner.children.find(node => node.kind === 'NAME');
      const ident = name?.children.find(node => node.kind === 'IDENT');
      if (ident) owner.selection = {start: ident.start, end: ident.end};
    }
  }
  pathAt(start, end) {
    const result = [];
    for (let node = this.root; node && within(node, start, end);) {
      result.push(node);
      node = node.children.find(child => within(child, start, end));
    }
    return result;
  }
  bind(start, end, id) {
    for (const node of this.pathAt(start, end))
      if (node.selection?.start === start && node.selection.end === end) node.id = id;
  }
  enclosing(start, end, skipId = '') {
    const nodes = this.pathAt(start, end);
    // An unnamed closure/async block needs a separate execution owner. Until
    // supported, omit its dependencies instead of attributing them to a caller.
    if (nodes.some(node => node.kind === 'CLOSURE_EXPR' || node.kind === 'ASYNC_BLOCK_EXPR' ||
      node.kind === 'BLOCK_EXPR' && node.children.some(child => child.kind === 'ASYNC_KW'))) return undefined;
    for (let index = nodes.length - 1; index >= 0; index--) {
      const node = nodes[index];
      if (node.id === skipId) continue;
      if (OWNERS.has(node.kind)) return node.id ? node : undefined;
      if (node === this.root) return node.id ? node : undefined;
    }
    return undefined;
  }
  dependency(start, end) {
    const nodes = this.pathAt(start, end);
    if (!nodes.some(node => node.kind === 'NAME_REF')) return undefined;
    if (nodes.some(node => node.kind === 'USE')) return 'imports';
    if (nodes.some(node => TYPES.has(node.kind))) return 'type_reference';
    if (nodes.some(node => node.kind === 'PATH_EXPR' || node.kind === 'FIELD_EXPR' || node.kind === 'RECORD_EXPR' || node.kind === 'MACRO_CALL')) return 'reference';
    return undefined;
  }
  callAt(start, end) {
    return this.pathAt(start, end).findLast(node => POSITIONAL.has(node.kind));
  }
  callableOwners() { return this.owners.filter(node => node.id && node.selection && callable(node)); }
  declarationAnchors() {
    const anchors = new Map([[`0:${this.source.artifact.utf8.length}`, 'SOURCE_FILE']]);
    const stack = [{node: this.root, parent: undefined}];
    while (stack.length) {
      const {node, parent} = stack.pop();
      const identifier = node.kind === 'NAME' ? node.children.find(child => child.kind === 'IDENT') :
        node.kind === 'SELF_PARAM' ? node.children.find(child => child.kind === 'SELF_KW') : undefined;
      if (identifier) anchors.set(`${identifier.start}:${identifier.end}`, node.kind === 'SELF_PARAM' ? node.kind : parent.kind);
      for (const child of node.children) stack.push({node: child, parent: node});
    }
    return anchors;
  }
}

export async function syntaxGraph(analyzer, source, {onPresentationUnavailable} = {}) {
  try {
    const actual = await analyzer.request('rust-analyzer/viewFileText', {uri: source.uri});
    if (typeof actual !== 'string' || actual !== source.text && actual !== source.text.replace(/\r\n/g, '\n'))
      throw new Error('Rust analyzer source differs from the retained saved snapshot');
    let encoded;
    try { encoded = await analyzer.request('rust-analyzer/viewSyntaxTree', {textDocument: {uri: source.uri}}); }
    catch (error) {
      // The presentation extension can panic while expanding Rust-looking
      // strings. A lookup-only caller may omit identity proof for that file;
      // required owned syntax, source equality and transport failures stay fatal.
      if (error.rpcCode === -32603 && onPresentationUnavailable) { onPresentationUnavailable(error); return undefined; }
      throw error;
    }
    return new RustGraph(source, encoded, actual);
  } catch (error) {
    if (error.rpcCode === -32601) return undefined;
    throw error;
  }
}

export async function outgoingCalls(analyzer, graph, {position, symbol, relationship, initializer, recordLimit}) {
  let emitted = 0;
  const seen = new Set();
  for (const owner of graph.callableOwners()) {
    const prepared = await analyzer.request('textDocument/prepareCallHierarchy', {textDocument: {uri: graph.source.uri}, position: position(owner.selection.start)});
    // Preparation can navigate elsewhere. Require the exact compiler-backed
    // declaration/owner selected in this saved source before requesting calls.
    const items = (prepared ?? []).filter(item => item.uri === graph.source.uri &&
      graph.source.byte(item.selectionRange.start) === owner.selection.start && graph.source.byte(item.selectionRange.end) === owner.selection.end);
    if (items.length !== 1) continue;
    const calls = await analyzer.request('callHierarchy/outgoingCalls', {item: items[0]});
    const candidates = new Map();
    for (const call of calls ?? []) {
      const sites = [];
      for (const range of call.fromRanges ?? []) {
        const start = graph.source.byte(range.start), end = graph.source.byte(range.end);
        if (start < end && graph.callAt(start, end) && graph.enclosing(start, end)?.id === owner.id) sites.push({start, end});
      }
      if (!sites.length) continue;
      const target = await symbol(call.to);
      if (!target) continue;
      for (const {start, end} of sites) {
        const key = `${start}:${end}`;
        const values = candidates.get(key) ?? new Map();
        values.set(target.id, {target, start, end}); candidates.set(key, values);
      }
    }
    for (const values of candidates.values()) {
      if (values.size !== 1) continue; // Never choose one ambiguous target.
      const {target, start, end} = [...values.values()][0];
      const sourceId = owner.kind === 'FN' ? owner.id : initializer(owner);
      const key = JSON.stringify([sourceId, target.id, start, end]);
      if (seen.has(key)) continue;
      seen.add(key);
      // A declaration binding is not proof of a concrete trait-object target.
      // Method calls keep their indirect boundary; no dispatch candidates are
      // inferred from matching signatures or implementation names.
      relationship({sourceId, targetId: target.id, kind: 'call', resolution: 'compiler_bound',
        artifactId: graph.source.artifact.id, startByte: start, endByte: end, implicit: false,
        dispatch: target.method ? 'indirect' : 'direct'});
      emitted++;
      recordLimit();
    }
  }
  return emitted;
}
