// Keep selected language/build semantics, redirect action/output slots and
// remove module-preparation shortcuts for full-body per-file IndexStore work.
export function semanticArguments(args) {
  const result = [];
  const indexingShortcuts = new Set(['-experimental-lazy-typecheck',
    '-experimental-skip-non-exportable-decls', '-experimental-allow-module-with-compiler-errors']);
  const outputOptions = new Set(['-index-store-path', '-index-unit-output-path', '-o', '-emit-module-path',
    '-emit-module-doc-path', '-emit-module-source-info-path', '-emit-dependencies-path', '-emit-reference-dependencies-path', '-serialize-diagnostics-path']);
  for (let i = 0; i < args.length; i++) {
    if (args[i] === '-Xfrontend' && indexingShortcuts.has(args[i + 1])) { i++; continue; }
    if (outputOptions.has(args[i])) { i++; continue; }
    if (['-c', '-emit-module', '-emit-object', '-typecheck', '-index-file'].includes(args[i])) continue;
    result.push(args[i]);
  }
  return result;
}
