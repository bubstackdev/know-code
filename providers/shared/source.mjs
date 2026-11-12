import fs from 'node:fs';
import path from 'node:path';

export const relative = (root, file) => path.relative(root, file).split(path.sep).join('/');

// Provider-native UTF-16 offsets address one exact compiler text snapshot.
export function byteOffsets(text) {
  const offsets = new Uint32Array(text.length + 1);
  let bytes = 0;
  for (let i = 0; i < text.length;) {
    const point = text.codePointAt(i);
    offsets[i] = bytes;
    if (point > 0xffff) offsets[i + 1] = bytes;
    bytes += Buffer.byteLength(String.fromCodePoint(point));
    i += point > 0xffff ? 2 : 1;
  }
  offsets[text.length] = bytes;
  return offsets;
}

export function sourceBoundary(root, filename) {
  const outside = file => { const p = path.relative(root,file); return p === '..' || p.startsWith(`..${path.sep}`) || path.isAbsolute(p); };
  const lexicalOutside = outside(filename);
  let canonicalOutside = false, unverified = false;
  try {
    const canonicalRoot = fs.realpathSync(root), canonicalFile = fs.realpathSync(filename);
    const p = path.relative(canonicalRoot,canonicalFile);
    canonicalOutside = p === '..' || p.startsWith(`..${path.sep}`) || path.isAbsolute(p);
  } catch { unverified = true; }
  if (!lexicalOutside && unverified) return {code:'source_origin_unverified',severity:'info',path:relative(root,filename).slice(0,4096),
    message:'The canonical source boundary could not be verified during analysis. Retained text and ranges belong to the compiler snapshot; no inside-workspace physical origin is asserted.'};
  if (!lexicalOutside && !canonicalOutside) return undefined;
  return {code:'outside_workspace_source',severity:'info',path:relative(root,filename).slice(0,4096),
    message:`Compiler-selected linked source is outside the workspace (${lexicalOutside?'lexical path':'canonical symlink target'} observed during analysis). Retained text and ranges belong to the compiler snapshot, not a later filesystem target.`};
}
