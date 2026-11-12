// Keep compiler-selected semantic arguments; redirect only action/output slots
// for the existing per-file IndexStore invocation. Shared by Swift planners.
export function semanticArguments(args) {
  const result = [];
  const outputOptions = new Set(['-index-store-path', '-index-unit-output-path', '-o', '-emit-module-path',
    '-emit-module-doc-path', '-emit-module-source-info-path', '-emit-dependencies-path', '-emit-reference-dependencies-path', '-serialize-diagnostics-path']);
  for (let i = 0; i < args.length; i++) {
    if (outputOptions.has(args[i])) { i++; continue; }
    if (['-c', '-emit-module', '-emit-object', '-typecheck', '-index-file'].includes(args[i])) continue;
    result.push(args[i]);
  }
  return result;
}
