// TypeScript references and declarations as identity-v1 enumerates them (eval/identity-v1/README.md), from the type
// checker of the repository's own TypeScript (its node_modules/typescript). Runs in the labelling container, after
// the repository's dependencies are installed with their scripts off.
//
//     node label.cjs --repo NAME --src DIR TSCONFIG [TSCONFIG ...] > enumeration-NAME.jsonl
'use strict';
const path = require('path');

const args = process.argv.slice(2);
const opt = (name) => { const i = args.indexOf(name); return i < 0 ? null : args.splice(i, 2)[1]; };
const repo = opt('--repo');
const src = path.resolve(opt('--src')) + '/';
const configs = args;
if (!repo || !configs.length) {
  console.error('usage: label.cjs --repo NAME --src DIR TSCONFIG [TSCONFIG ...]');
  process.exit(2);
}
const ts = require(require.resolve('typescript', { paths: [src] }));
const TS_FILE = /\.(ts|tsx|mts|cts)$/;
const out = [];
const done = new Set();
const declared = new Set();
// compare-v1 (eval/compare-v1/README.md) adds `in` to each reference row; identity-v1's output is unchanged without it.
const enclosing = process.env.LABEL_ENCLOSING === '1';

function own(fileName) {
  const full = path.resolve(fileName);
  if (!full.startsWith(src)) return null;
  const rel = full.slice(src.length);
  return rel.split('/').some((p) => p === 'node_modules' || p === 'dist') ? null : rel;
}

function program(config) {
  const read = ts.readConfigFile(config, ts.sys.readFile);
  if (read.error) throw new Error(ts.flattenDiagnosticMessageText(read.error.messageText, '\n'));
  const parsed = ts.parseJsonConfigFileContent(read.config, ts.sys, path.dirname(config), undefined, config);
  return ts.createProgram({ rootNames: parsed.fileNames, options: parsed.options, projectReferences: parsed.projectReferences });
}

function nameLine(decl) {
  const sf = decl.getSourceFile();
  const name = ts.getNameOfDeclaration(decl) || decl;
  return sf.getLineAndCharacterOfPosition(name.getStart(sf)).line + 1;
}

// A declaration local to a function body or signature: not labelled, not counted.
function local(decl) {
  if (ts.isParameter(decl) || ts.isTypeParameterDeclaration(decl) || ts.isBindingElement(decl)) return true;
  if (ts.isVariableDeclaration(decl) || ts.isFunctionDeclaration(decl) || ts.isClassDeclaration(decl)) {
    for (let n = decl.parent; n; n = n.parent) {
      if (ts.isSourceFile(n) || ts.isModuleBlock(n)) return false;
      if (ts.isFunctionLike(n) || ts.isBlock(n) && !ts.isModuleBlock(n)) return true;
    }
  }
  return false;
}

function functionValued(decl) {
  return ts.isVariableDeclaration(decl) && decl.initializer
    && (ts.isArrowFunction(decl.initializer) || ts.isFunctionExpression(decl.initializer));
}

// What a declaration counts as: `type`, `callable` or `member`; null for what isn't labelled.
function group(decl) {
  if (local(decl)) return null;
  if (ts.isClassDeclaration(decl) || ts.isInterfaceDeclaration(decl) || ts.isTypeAliasDeclaration(decl) || ts.isEnumDeclaration(decl)) return 'type';
  if (ts.isFunctionDeclaration(decl) || ts.isMethodDeclaration(decl) || ts.isMethodSignature(decl) || functionValued(decl)) return 'callable';
  if (ts.isPropertyDeclaration(decl) || ts.isPropertySignature(decl) || ts.isEnumMember(decl) || ts.isVariableDeclaration(decl)) return 'member';
  return null;
}

// A counted declaration's key, as its `decl` row gives it.
function declKey(checker, node, rel) {
  const symbol = checker.getSymbolAtLocation(ts.getNameOfDeclaration(node));
  return symbol && symbol.declarations ? key(symbol.declarations) : `${rel}:${nameLine(node)}`;
}

// The key of the innermost declaration around a reference that identity-v1 counts; null at file level.
function enclosingKey(checker, node, rel) {
  for (let n = node.parent; n; n = n.parent) {
    const name = group(n) && ts.getNameOfDeclaration(n);
    if (name && ts.isIdentifier(name)) return declKey(checker, n, rel);
  }
  return null;
}

function inImportOrExport(node) {
  for (let n = node.parent; n; n = n.parent) {
    if (ts.isImportDeclaration(n) || ts.isExportDeclaration(n) || ts.isImportEqualsDeclaration(n) || ts.isExportAssignment(n)) return true;
    if (ts.isStatement(n) || ts.isSourceFile(n)) return false;
  }
  return false;
}

// `new Foo(…)`: the toolchain binds the construction to the constructor.
function constructed(node) {
  let n = node;
  while (n.parent && ts.isPropertyAccessExpression(n.parent) && n.parent.name === n) n = n.parent;
  return n.parent && ts.isNewExpression(n.parent) && n.parent.expression === n;
}

function byteCol(sf, pos) {
  const lc = sf.getLineAndCharacterOfPosition(pos);
  const lineStart = sf.getPositionOfLineAndCharacter(lc.line, 0);
  return [lc.line + 1, Buffer.byteLength(sf.text.slice(lineStart, lineStart + lc.character), 'utf8') + 1];
}

function key(decls) {
  const d = decls[0];
  const rel = own(d.getSourceFile().fileName) || path.basename(d.getSourceFile().fileName);
  return `${rel}:${nameLine(d)}`;
}

function origin(decls) {
  const file = decls[0].getSourceFile().fileName;
  if (/\/typescript\/lib\/lib\.[^/]*\.d\.ts$/.test(file)) return 'stdlib';
  return file.includes('/node_modules/') ? 'package' : 'sdk';
}

function reference(checker, sf, rel, node) {
  if (inImportOrExport(node) || constructed(node)) return null;
  let symbol = checker.getSymbolAtLocation(node);
  if (!symbol) return null;
  if (symbol.flags & ts.SymbolFlags.Alias) symbol = checker.getAliasedSymbol(symbol);
  const decls = symbol.declarations || [];
  if (!decls.length || decls.some((d) => (ts.getNameOfDeclaration(d) === node))) return null;
  if (node.text !== symbol.name) return null;
  const groups = decls.map(group);
  if (groups.some((g) => g === null)) return null;
  let kind = groups.includes('type') ? 'type' : groups.includes('callable') ? 'call' : 'member';
  if (kind !== 'type') {
    const d = decls[0];
    const abstract = ts.getCombinedModifierFlags(d) & ts.ModifierFlags.Abstract;
    if (d.parent && ts.isInterfaceDeclaration(d.parent) || abstract) kind = 'dispatch';
  }
  const places = decls.map((d) => own(d.getSourceFile().fileName));
  const [line, col] = byteCol(sf, node.getStart(sf));
  const row = { t: 'ref', repo, file: rel, line, col, name: node.text, kind, key: key(decls), symbol: checker.getFullyQualifiedName(symbol) };
  if (places.every((p) => p !== null)) {
    const locs = [...new Set(decls.map((d) => `${own(d.getSourceFile().fileName)}\u0000${nameLine(d)}`))].sort()
      .map((s) => { const [f, l] = s.split('\u0000'); return [f, Number(l)]; });
    return Object.assign(row, { inside: true, locs, origin: null });
  }
  if (places.every((p) => p === null)) return Object.assign(row, { inside: false, locs: [], origin: origin(decls) });
  return null;
}

for (const config of configs) {
  const p = program(path.resolve(config));
  const checker = p.getTypeChecker();
  for (const sf of p.getSourceFiles()) {
    const rel = own(sf.fileName);
    if (rel === null || done.has(rel)) continue;
    done.add(rel);
    const refs = TS_FILE.test(rel) && !rel.endsWith('.d.ts');
    const visit = (node) => {
      const g = group(node);
      const name = g && ts.getNameOfDeclaration(node);
      if (name && ts.isIdentifier(name)) {
        const k = declKey(checker, node, rel);
        if (!declared.has(k)) {
          declared.add(k);
          out.push({ t: 'decl', repo, name: name.text, key: k, file: rel, line: nameLine(node), decl: g });
        }
      }
      if (refs && ts.isIdentifier(node)) {
        const row = reference(checker, sf, rel, node);
        if (row && enclosing) row.in = enclosingKey(checker, node, rel);
        if (row) out.push(row);
      }
      ts.forEachChild(node, visit);
    };
    visit(sf);
  }
}
const keys = (o) => Object.keys(o).sort().reduce((a, k) => (a[k] = o[k], a), {});
process.stdout.write(out.map((o) => JSON.stringify(keys(o))).join('\n') + (out.length ? '\n' : ''));
