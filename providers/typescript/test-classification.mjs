// Saved source-origin evidence, never proof that a runner executes this file.
export const TEST_CLASSIFICATION_POLICY = 1;
export function classificationPolicy(value = 0) {
  if (value !== 0 && value !== TEST_CLASSIFICATION_POLICY)
    throw new Error(`Update required: unsupported TypeScript test classification policy ${value}; expected ${TEST_CLASSIFICATION_POLICY}`);
  return value;
}
export function compilationClassification(policy = TEST_CLASSIFICATION_POLICY) {
  return policy ? {policyVersion: policy, kind: 'unknown', evidence: []} : undefined;
}
export function sourceClassification(normalizedPath, policy = TEST_CLASSIFICATION_POLICY) {
  const unknown = compilationClassification(policy);
  if (!policy) return undefined;
  const parts = normalizedPath.split('/');
  const filename = parts.at(-1);
  // Declaration-only *.test.d.ts is deliberately not a script-suffix match.
  const suffix = /\.(test|spec)\.(?:[cm]?[jt]s|[jt]sx)$/.exec(filename);
  const rule = parts.slice(0, -1).includes('__tests__') ? 'directory_component=__tests__'
    : suffix ? `filename_suffix=.${suffix[1]}` : '';
  if (!rule) return unknown;
  if (Buffer.byteLength(normalizedPath, 'utf8') > 4096)
    return {...unknown, evidence: [{basis: 'path_convention', value: 'matching path exceeds 4096 UTF-8 bytes; classification unknown', path: '', inferred: true}]};
  return {policyVersion: policy, kind: 'test', evidence: [{basis: 'path_convention', value: rule, path: normalizedPath, inferred: true}]};
}
