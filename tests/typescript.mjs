import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import ts from 'typescript';
import {analyze} from '../providers/typescript/analysis.mjs';
import {declarationSymbols, dispatchTypeKeys} from '../providers/typescript/symbols.mjs';

const root = path.resolve('.artifacts/typescript', String(Date.now()));
fs.mkdirSync(root, {recursive: true});
const source = `
export class Box<T> { constructor(public item: T) {} }
const stringBox = new Box('π😀');
const numberBox = new Box(42);
stringBox.item; numberBox.item;
interface Left { shared: string; }
interface Right { shared: number; }
declare const union: Left | Right;
declare const intersection: Left & Right;
union.shared; intersection.shared;
const spread = {...stringBox, ...numberBox}; spread.item;
interface Merged { first: string; }
interface Merged { second: number; }
declare const merged: Merged; merged.first; merged.second;
const local = 1; const shorthand = {local}; shorthand.local;
export {Box as Alias};
import {Alias as ImportedBox} from './example.js';
new ImportedBox(true);
`;
const file = path.join(root, 'example.ts');
fs.writeFileSync(file, source);
fs.writeFileSync(path.join(root, 'tsconfig.json'), JSON.stringify({compilerOptions: {
  strict: true, noEmit: true, skipLibCheck: true, target: 'ES2022', module: 'NodeNext', moduleResolution: 'NodeNext'
}, files: ['example.ts']}));

function run(checkFirst) {
  const messages = [];
  let checker, sourceFile;
  const compiler = {...ts, createProgram(options) {
    const program = ts.createProgram(options);
    checker = program.getTypeChecker();
    sourceFile = program.getSourceFile(file);
    if (checkFirst) ts.getPreEmitDiagnostics(program);
    return program;
  }};
  analyze(compiler, root, path.join(root, 'tsconfig.json'), [], [], root,
    (kind, value) => messages.push({kind, value}));
  const symbols = messages.filter(m => m.kind === 'facts').flatMap(m => m.value.symbols);
  const occurrences = messages.filter(m => m.kind === 'facts').flatMap(m => m.value.occurrences);
  const at = (expression, token) => {
    const position = source.indexOf(expression) + expression.lastIndexOf(token);
    const startByte = Buffer.byteLength(source.slice(0, position));
    return occurrences.filter(o => o.role !== 'definition' && o.startByte === startByte);
  };
  const property = symbols.find(s => s.name === 'item' && s.signature === 'T');
  assert(property, 'generic property retains its declaration type');
  for (const expression of ['stringBox.item', 'numberBox.item']) {
    assert.deepEqual(at(expression, 'item').map(o => o.symbolId), [property.id], expression);
  }
  assert.equal(at('{local}', 'local').length, 2, 'shorthand property and value both survive');
  assert.deepEqual(at('new ImportedBox', 'ImportedBox').map(o => o.symbolId),
    at('class Box', 'Box').map(o => o.symbolId), 'import/export aliases bind to the original class');
  for (const name of ['first', 'second']) {
    assert.deepEqual(at(`merged.${name}`, name).map(o => o.symbolId),
      symbols.filter(s => s.name === name).map(s => s.id), 'merged declarations retain both members');
  }
  const canonical = declarationSymbols(ts, checker);
  let multipleRoots = 0;
  const visit = node => {
    if (ts.isIdentifier(node) && node.text === 'shared' && ts.isPropertyAccessExpression(node.parent)) {
      const symbol = checker.getSymbolAtLocation(node);
      assert(checker.getRootSymbols(symbol).length > 1);
      assert.equal(canonical(symbol), symbol, 'a synthetic property must not choose an arbitrary declaration');
      multipleRoots++;
    }
    ts.forEachChild(node, visit);
  };
  visit(sourceFile);
  assert.equal(multipleRoots, 2);
  assert(messages.some(m => m.kind === 'report' && m.value.status === 'complete'));
  return {symbols, occurrences};
}
const after = run(false), before = run(true);
assert.deepEqual(after.occurrences, before.occurrences, 'checking order changes occurrence identity');
assert.deepEqual(after.symbols.map(({signature, ...identity}) => identity), before.symbols.map(({signature, ...identity}) => identity));
fs.writeFileSync(path.join(root, 'summary.json'), JSON.stringify({compiler: ts.version,
  symbols: after.symbols.length, occurrences: after.occurrences.length, checkOrdersMatch: true}, null, 2)+'\n');
console.log('TypeScript declaration identities, generic roots, synthetic views, aliases and shorthand passed');

// Receiver specialization must preserve semantic distinctions that declaration
// identities intentionally share, including boolean/bigint/nominal literals.
const dispatchFile=path.join(root,'dispatch-types.ts');
fs.writeFileSync(dispatchFile,`
interface Slot<T> { call(): T; }
type Alias = number;
enum First { A } enum Second { A }
declare const first: unique symbol; declare const second: unique symbol;
declare let yes: Slot<true>, no: Slot<false>, bigOne: Slot<1n>, bigTwo: Slot<2n>;
declare let number: Slot<number>, text: Slot<string>, alias: Slot<Alias>;
declare let firstEnum: Slot<First.A>, secondEnum: Slot<Second.A>;
declare let firstSymbol: Slot<typeof first>, secondSymbol: Slot<typeof second>;
declare let union1: Slot<number|string>, union2: Slot<string|number>;
declare let unknown: Slot<any>, anonymous: Slot<{a: 1}>, structural: {call():number};
`);
const dispatchProgram=ts.createProgram({rootNames:[dispatchFile],options:{target:ts.ScriptTarget.ES2022,strict:true,noEmit:true}});
const dispatchChecker=dispatchProgram.getTypeChecker(),key=dispatchTypeKeys(ts,dispatchChecker,root),keys={},receivers={};
const dispatchSource=dispatchProgram.getSourceFile(dispatchFile);
function readKeys(node) {
  if(ts.isVariableDeclaration(node)&&node.type) {
    const receiver=dispatchChecker.getTypeAtLocation(node.name);
    const method=dispatchChecker.getPropertyOfType(receiver,'call');
    if(method) {receivers[node.name.getText()]=receiver;keys[node.name.getText()]=key(receiver,method.declarations[0]);}
  }
  ts.forEachChild(node,readKeys);
}
readKeys(dispatchSource);
const distinct=['yes','no','bigOne','bigTwo','number','text','firstEnum','secondEnum','firstSymbol','secondSymbol'];
assert(distinct.every(name=>keys[name]),keys);
assert.equal(new Set(distinct.map(name=>keys[name])).size,distinct.length,'receiver specialization identities collapsed');
assert.equal(keys.alias,keys.number,'type aliases must preserve the compiler type');
assert.equal(keys.union1,keys.union2,'union order must not affect specialization');
assert.equal(keys.unknown,'');assert.equal(keys.anonymous,'');
console.log('Dispatch keys preserve boolean/bigint/enum/unique-symbol specializations, aliases and union identity');

assert.equal(key(receivers.structural,dispatchChecker.getPropertyOfType(receivers.number,'call').declarations[0]),'', 'structural receivers must not be passed to the compiler base-type API');
