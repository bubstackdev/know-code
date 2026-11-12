// Exercise selection against installed Apple tools; no compiler/package builds.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import {createToolchainResolver} from '../providers/swift/toolchain.mjs';

const root=fileURLToPath(new URL('..',import.meta.url));
const run=path.join(root,'.artifacts/swift-toolchain',String(Date.now()));
fs.mkdirSync(run,{recursive:true});
const environment={...process.env};
for (const name of ['DEVELOPER_DIR','TOOLCHAINS','SDKROOT','SWIFT_EXEC','SWIFT_EXEC_MANIFEST']) delete environment[name];
const original={DEVELOPER_DIR:process.env.DEVELOPER_DIR,TOOLCHAINS:process.env.TOOLCHAINS};
const resolve=createToolchainResolver({environment,workspaceRoot:run});
const selected=resolve(run);
const checks=[];
if (selected.xcodeVersion) {
  assert.equal(selected.selection.origin,'preferred_xcode');
  assert(selected.compiler.includes('.app/Contents/Developer/'));
  assert.equal(selected.env.DEVELOPER_DIR,selected.selection.developerDirectory);
  checks.push('unpinned Xcode preference');
}
assert.deepEqual({DEVELOPER_DIR:process.env.DEVELOPER_DIR,TOOLCHAINS:process.env.TOOLCHAINS},original);
checks.push('parent environment unchanged');

const clt='/Library/Developer/CommandLineTools';
if (fs.existsSync(clt)) {
  const explicit=createToolchainResolver({environment:{...environment,DEVELOPER_DIR:clt}})(run);
  assert.equal(explicit.selection.origin,'explicit_override');
  assert(explicit.compiler.startsWith(clt+'/'));
  const sdkOverride=createToolchainResolver({environment:{...environment,SDKROOT:explicit.sdk}})(run);
  assert.equal(sdkOverride.selection.origin,'explicit_override');
  assert.equal(sdkOverride.sdk,explicit.sdk);
  assert.equal(sdkOverride.selection.sdkOverride,explicit.sdk);
  const fallback=createToolchainResolver({environment,applicationDirectories:[],toolchainDirectories:[]})(run);
  // This case uses the system's actual active developer selection.
  if (fallback.compiler.startsWith(clt+'/')) assert.equal(fallback.selection.origin,'command_line_tools');
  checks.push('explicit CLT override and absent-Xcode discovery fallback');
}
assert.throws(()=>createToolchainResolver({environment:{...environment,TOOLCHAINS:'org.knowcode.missing'}})(run),/requested installed toolchain|failed|unable/i);
assert.throws(()=>createToolchainResolver({environment:{...environment,DEVELOPER_DIR:path.join(run,'missing')}})(run),/unavailable/);
checks.push('invalid explicit choices never fall back');

const pin=path.join(run,'.swift-version');
const version=/Swift version (\d+\.\d+\.\d+)/.exec(selected.version)[1];
fs.writeFileSync(pin,version+'\n');
const pinned=resolve(run);
assert.equal(pinned.selection.origin,'repository_pin');
assert.equal(pinned.selection.swiftPin.path,pin);
assert(pinned.inputPaths.includes(pin));
assert.equal(pinned.compiler,selected.compiler);
fs.writeFileSync(pin,'99.99.99\n');
assert.throws(()=>resolve(run),/no compatible installed.*\.swift-version/s);
assert.throws(()=>createToolchainResolver({environment:{...environment,DEVELOPER_DIR:selected.env.DEVELOPER_DIR}})(run),/conflicts with selected pin/);
checks.push('matching pin, missing pin, and explicit-pin conflict');

const child=path.join(run,'child');fs.mkdirSync(child);
const childPin=path.join(child,'.swift-version');fs.writeFileSync(childPin,version+'\n');
assert.equal(resolve(child).selection.swiftPin.path,childPin);
fs.writeFileSync(path.join(child,'.tool-versions'),`swift 99.99.99\n`);
assert.throws(()=>resolve(child),/conflicting swift pins/);
fs.unlinkSync(childPin);
fs.writeFileSync(path.join(child,'.tool-versions'),`python 3.14\nswift ${version} # selected\n`);
assert.equal(resolve(child).selection.swiftPin.path,path.join(child,'.tool-versions'));
checks.push('nearest scoped pin and conflicting/selected manager entries');

const nestedWorkspace=path.join(run,'nested-workspace');fs.mkdirSync(nestedWorkspace);
fs.writeFileSync(pin,version+'\n');
const inherited=createToolchainResolver({environment,workspaceRoot:nestedWorkspace})(nestedWorkspace);
assert.equal(inherited.selection.origin,'inherited_pin');
assert.equal(inherited.selection.swiftPin.scope,'ancestor');
assert.equal(inherited.selection.swiftPin.path,pin);
assert(inherited.inputPaths.includes(pin));
fs.writeFileSync(path.join(nestedWorkspace,'.swift-version'),version+'\n');
const local=createToolchainResolver({environment,workspaceRoot:nestedWorkspace})(nestedWorkspace);
assert.equal(local.selection.origin,'repository_pin');
assert.equal(local.selection.swiftPin.scope,'workspace');
checks.push('ancestor pin remains honored and tracked with distinct non-repository provenance');

fs.unlinkSync(pin);
fs.writeFileSync(path.join(run,'.tool-versions'),'swift system\n');
assert.equal(resolve(run).selection.origin,'repository_pin');
fs.unlinkSync(path.join(run,'.tool-versions'));
if (selected.xcodeVersion) {
  const xcodePin=path.join(run,'.xcode-version');
  fs.writeFileSync(xcodePin,selected.xcodeVersion+'\n');
  assert.equal(resolve(run).selection.xcodePin.value,selected.xcodeVersion);
  fs.writeFileSync(xcodePin,'99.99\n');
  assert.throws(()=>resolve(run),/no compatible installed.*\.xcode-version/s);
  fs.unlinkSync(xcodePin);
  checks.push('Xcode version pin, no CLT substitution');
}
const summary={passed:true,checks,compiler:selected.compiler,sdk:selected.sdk,version:selected.version,
  xcode_default_exercised:Boolean(selected.xcodeVersion),global_selection_changed:false};
fs.writeFileSync(path.join(run,'result.json'),JSON.stringify(summary,null,2)+'\n');
fs.writeFileSync(path.join(root,'.artifacts/swift-toolchain/latest.json'),JSON.stringify(summary,null,2)+'\n');
console.log(JSON.stringify(summary,null,2));
