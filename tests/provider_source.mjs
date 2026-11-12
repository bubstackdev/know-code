import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import {byteOffsets,relative,sourceBoundary} from '../providers/shared/source.mjs';

for (const text of ['', 'ascii\r\nnext', 'π🦀é\n', '\uD800x\uDC00']) {
  const offsets=byteOffsets(text);
  for (let i=0;i<=text.length;i++) {
    const boundary=i>0 && i<text.length && /[\uD800-\uDBFF]/.test(text[i-1]) && /[\uDC00-\uDFFF]/.test(text[i]) ? i-1 : i;
    assert.equal(offsets[i],Buffer.byteLength(text.slice(0,boundary)));
  }
}
const root=fileURLToPath(new URL('..',import.meta.url));
const run=path.join(root,'.artifacts/provider-source',String(process.pid));
fs.mkdirSync(path.join(run,'workspace'),{recursive:true});
const workspace=path.join(run,'workspace'), outside=path.join(run,'outside.ts');
fs.writeFileSync(outside,'export const linked = 1;\n');
const alias=path.join(workspace,'alias.ts');fs.symlinkSync(outside,alias);
assert.equal(relative(workspace,outside),'../outside.ts');
assert.match(sourceBoundary(workspace,outside).message,/lexical path/);
assert.match(sourceBoundary(workspace,alias).message,/canonical symlink target/);
assert.equal(sourceBoundary(workspace,workspace),undefined);
assert.equal(sourceBoundary(workspace,path.join(workspace,'missing.ts')).code,'source_origin_unverified');
console.log('Shared source offsets and linked-source origin checks passed.');
