// Package entry resolution only: no compiler import, restoration or child process.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import {typescriptInstallation} from '../providers/typescript/package-tools.mjs';

const project=fileURLToPath(new URL('..',import.meta.url));
const run=path.join(project,'.artifacts/typescript-resolution',process.hrtime.bigint().toString());
const workspace=path.join(run,'workspace');fs.mkdirSync(path.join(workspace,'node_modules'),{recursive:true});
const write=(filename,text)=>{fs.mkdirSync(path.dirname(filename),{recursive:true});fs.writeFileSync(filename,text);};
const packageAt=(root,manifest,entry)=>{
  write(path.join(root,'package.json'),JSON.stringify(manifest));
  write(path.join(root,entry),'throw new Error("Resolution must not execute this compiler fixture");\n');return root;
};
const ambient=packageAt(path.join(run,'node_modules/typescript'),{name:'typescript',main:'ambient.cjs'},'ambient.cjs');
assert.equal(typescriptInstallation(workspace,workspace),undefined,'workspace boundary must exclude the ambient compiler');
const selected=path.join(workspace,'node_modules/typescript');
function point(root){if(fs.existsSync(selected)||fs.lstatSync(selected,{throwIfNoEntry:false}))fs.unlinkSync(selected);fs.symlinkSync(root,selected,'dir');}
const legacy=packageAt(path.join(run,'legacy-store/typescript'),{name:'typescript',version:'6.0.2',main:'lib/compiler.cjs'},'lib/compiler.cjs');
point(legacy);
assert.equal(typescriptInstallation(workspace,workspace).filename,path.join(legacy,'lib/compiler.cjs'));
const native=packageAt(path.join(run,'native-store/typescript'),{name:'typescript',version:'7.0.2',type:'module',exports:{'.':'./lib/version.cjs'}},'lib/version.cjs');
point(native);
const rootExport=typescriptInstallation(workspace,workspace);
assert.equal(rootExport.entry,selected);assert.equal(rootExport.filename,path.join(native,'lib/version.cjs'));
const conditional=packageAt(path.join(run,'conditional-store/typescript'),{name:'typescript',version:'7.0.2',exports:{'.':{import:'./not-selected.mjs',require:'./version.cjs'}}},'version.cjs');
point(conditional);
assert.equal(typescriptInstallation(workspace,workspace).filename,path.join(conditional,'version.cjs'));
const unavailable=packageAt(path.join(run,'closed-store/typescript'),{name:'typescript',version:'7.0.2',exports:{'./package.json':'./package.json'}},'index.js');
point(unavailable);
assert(typescriptInstallation(workspace,workspace).error,'missing root exports must not fall back to index or an ambient compiler');
const wrongName=packageAt(path.join(run,'wrong-name-store'),{name:'another-package',exports:{'.':'./index.cjs'}},'index.cjs');
point(wrongName);
assert(typescriptInstallation(workspace,workspace).error,'self-reference identity cannot silently select ambient typescript');
const escaped=packageAt(path.join(run,'escaped-store/typescript'),{name:'typescript',main:'../../node_modules/typescript/ambient.cjs'},'unused.cjs');
point(escaped);
assert(typescriptInstallation(workspace,workspace).error,'legacy entry outside its selected origin must not select the ambient compiler');
point(path.join(run,'absent-store'));
assert(typescriptInstallation(workspace,workspace).error,'broken selected origin must remain unavailable');
const result={status:'passed_package_entry_resolution',run,checks:['workspace_boundary','legacy_main','native_root_exports','conditional_require_export','closed_exports_no_fallback','wrong_name_no_ambient_substitution','legacy_origin_escape_no_substitution','broken_origin'],compiler_execution:false};
fs.writeFileSync(path.join(run,'result.json'),JSON.stringify(result,null,2)+'\n');console.log(JSON.stringify(result));
