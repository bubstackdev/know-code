// Finite owned package-manager executable-selector and setup-input regressions.
// This is adapter setup validation, not compiler/index/throughput certification.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import {spawnSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import {createHash} from 'node:crypto';
import {preparePackages} from '../providers/typescript/preparation.mjs';
import {packageTool,runCommand,selection,restorationSafety,readSetupInput,MAX_SETUP_INPUT_BYTES,typescriptInstallation} from '../providers/typescript/package-tools.mjs';
import {ProjectSetupError} from '../providers/typescript/project-errors.mjs';

const root=fileURLToPath(new URL('..',import.meta.url));
const run=path.join(root,'.artifacts/preparation-safety',Date.now().toString(16));
fs.mkdirSync(run,{recursive:true});
const cache=path.resolve(process.env.KNOW_CODE_TEST_TOOL_CACHE??path.join(run,'tools'));
const project=root.split(`${path.sep}.artifacts${path.sep}`)[0];
assert(cache.startsWith(path.join(project,'.artifacts')+path.sep),'package-tool cache must be a durable fixture-owned project artifact');
const actualHome=process.env.HOME;
const canonicalFullPins=[
  'npm@10.0.0','npm@10.9.10','npm@11.0.0','npm@11.21.2',
  'pnpm@10.0.0','pnpm@10.34.9','pnpm@11.0.0','pnpm@11.28.11','pnpm@12.0.0','pnpm@12.18.0'
];
const pins=process.env.KNOW_CODE_TEST_PACKAGE_PINS?.split(',')??canonicalFullPins;
const fullCatalog=pins.length===canonicalFullPins.length&&new Set(pins).size===canonicalFullPins.length&&canonicalFullPins.every(pin=>pins.includes(pin));
const write=(filename,value)=>{fs.mkdirSync(path.dirname(filename),{recursive:true});fs.writeFileSync(filename,value);};
const hash=filename=>createHash('sha256').update(fs.readFileSync(filename)).digest('hex');
for(const [key,value] of Object.entries({XDG_CONFIG_HOME:path.join(run,'config'),XDG_CACHE_HOME:path.join(run,'cache'),XDG_DATA_HOME:path.join(run,'data'),
  npm_config_cache:path.join(run,'npm-cache'),npm_config_store_dir:path.join(run,'pnpm-store'),pnpm_config_store_dir:path.join(run,'pnpm-store'),
  npm_config_userconfig:path.join(run,'empty-userconfig'),NPM_CONFIG_USERCONFIG:path.join(run,'empty-userconfig'),
  pnpm_config_userconfig:path.join(run,'empty-userconfig'),PNPM_CONFIG_USERCONFIG:path.join(run,'empty-userconfig'),
  npm_config_globalconfig:path.join(run,'empty-globalconfig'),NPM_CONFIG_GLOBALCONFIG:path.join(run,'empty-globalconfig'),
  PNPM_HOME:path.join(run,'pnpm-home'),COREPACK_HOME:path.join(run,'corepack-cache'),GIT_CONFIG_GLOBAL:path.join(run,'empty-gitconfig'),
  PATH:`${path.dirname(process.execPath)}:/usr/bin:/bin`,GIT_CONFIG_NOSYSTEM:'1'})) process.env[key]=value;
write(process.env.npm_config_userconfig,'');
write(process.env.npm_config_globalconfig,'');write(process.env.GIT_CONFIG_GLOBAL,'');
assert.equal(process.env.HOME,actualHome,'fixture must preserve the actual account HOME');
const commands=[];
function command(program,args,cwd=run) {
  const result=spawnSync(program,args,{cwd,env:process.env,encoding:'utf8',timeout:30000,maxBuffer:1024*1024});
  commands.push({program,args,cwd,status:result.status,stdout:result.stdout,stderr:result.stderr,error:result.error?.message});
  assert.equal(result.status,0,JSON.stringify(commands.at(-1)));return result.stdout.trim();
}
const quote=value=>`'${value.replaceAll("'", "'\\''")}'`;
const git=path.join(run,'git-dependency');
write(path.join(git,'package.json'),JSON.stringify({name:'know-code-safe-git-dependency',version:'1.0.0',types:'index.d.ts',
  scripts:{prepare:`node -e ${quote(`require('fs').writeFileSync(${JSON.stringify(path.join(run,'dependency-prepare-ran'))},'bad')`)}`}}));
write(path.join(git,'index.d.ts'),'export declare const value: number;\n');
command('/usr/bin/git',['init','--quiet',git]);
command('/usr/bin/git',['add','package.json','index.d.ts'],git);
command('/usr/bin/git',['-c','user.name=Know Code fixture','-c','user.email=fixture@example.invalid','-c','commit.gpgsign=false','-c','core.hooksPath=/dev/null','commit','--quiet','-m','owned dependency'],git);
const revision=command('/usr/bin/git',['rev-parse','HEAD'],git);assert(/^[0-9a-f]{40,64}$/.test(revision));
const registry='https://registry.example.invalid/';const proxy='http://127.0.0.1:9';
const checks=[],matrix=[];
function progress(status='running',error) {
  fs.writeFileSync(path.join(run,'matrix-progress.json'),JSON.stringify({status,canonical_full_pins:canonicalFullPins,selected_pins:pins,full_catalog:fullCatalog,checks,matrix,commands,error},null,2)+'\n');
  const current=matrix.at(-1);
  if(current)fs.writeFileSync(path.join(current.directory,'result.json'),JSON.stringify(current,null,2)+'\n');
}
const request={analysisPolicy:{version:1,node:{cacheDirectory:cache,allowInstallScripts:false}},preparations:[],retryPreparation:true};
try {
  for(const pin of pins) {
    const directory=path.join(run,pin.replaceAll('@','-'));
    const gitMarker=path.join(directory,'git-selector-ran'),shellMarker=path.join(directory,'shell-selector-ran');
    write(path.join(directory,'selector-git'),`#!/bin/sh\nprintf bad > ${quote(gitMarker)}\nexit 37\n`);
    write(path.join(directory,'selector-shell'),`#!/bin/sh\nprintf bad > ${quote(shellMarker)}\nexit 38\n`);
    fs.chmodSync(path.join(directory,'selector-git'),0o755);fs.chmodSync(path.join(directory,'selector-shell'),0o755);
    write(path.join(directory,'package.json'),JSON.stringify({name:'know-code-safe-app',version:'1.0.0',private:true,packageManager:pin,
      dependencies:{'know-code-safe-git-dependency':`git+file://${git}#${revision}`},
      scripts:{install:`node -e ${quote(`require('fs').writeFileSync(${JSON.stringify(path.join(run,'root-install-ran'))},'bad')`)}`}}));
    const config=`git=./selector-git\nscript-shell=./selector-shell\nnode-gyp=./selector-shell\nignore-scripts=false\nallow-git=true\nregistry=${registry}\nproxy=${proxy}\n//registry.example.invalid/:_authToken=owned-fake-token\n`;
    write(path.join(directory,'.npmrc'),config);
    if(pin.startsWith('pnpm'))write(path.join(directory,'pnpm-workspace.yaml'),`packages:\n  - .\nscriptShell: ./selector-shell\n`);
    const configHash=hash(path.join(directory,'.npmrc'));const messages=[];
    const attempt={pin,directory,status:'running',messages,config_sha256:configHash};matrix.push(attempt);progress();
    const diagnostics=await preparePackages(directory,[directory],request,(kind,value)=>{messages.push({kind,value});progress();});
    const preparation=messages.filter(message=>message.kind==='preparation').map(message=>message.value);
    Object.assign(attempt,{preparation,diagnostics:diagnostics(directory),setup_failure:diagnostics.failure(directory)?.message,
      markers:Object.fromEntries([gitMarker,shellMarker,path.join(run,'dependency-prepare-ran'),path.join(run,'root-install-ran')].map(filename=>[filename,fs.existsSync(filename)]))});progress();
    assert.equal(diagnostics.failure(directory),undefined);
    assert.deepEqual(diagnostics(directory),[]);
    assert.equal(preparation.length,1);assert(preparation[0].succeeded,JSON.stringify(preparation));
    assert(fs.existsSync(path.join(directory,'node_modules/know-code-safe-git-dependency/package.json')),'real Git dependency was not installed');
    for(const filename of [gitMarker,shellMarker,path.join(run,'dependency-prepare-ran'),path.join(run,'root-install-ran')])assert(!fs.existsSync(filename),`ungranted executable ran: ${filename}`);
    assert.equal(hash(path.join(directory,'.npmrc')),configHash,'setup rewrote repository authentication/config');
    const tool=await packageTool(selection(pin),cache,()=>{}),safety=restorationSafety(tool,false);
    const configured={};
    for(const key of ['registry','proxy']) {
      const output=await runCommand(tool.program,[...tool.prefix,'config','get',key,...safety.args],directory,undefined,safety.env);
      assert(output.includes(key==='registry'?registry:proxy),`${pin} lost ${key} configuration: ${output}`);configured[key]=output;
    }
    Object.assign(attempt,{status:'passed',configured,tool:{program:tool.program,prefix:tool.prefix,source:tool.source}});progress();
  }
  checks.push(fullCatalog?'full_catalog_git_restore_ignores_executable_selectors_without_losing_registry_proxy_or_config':'selected_pins_only_git_restore_ignores_executable_selectors_without_losing_registry_proxy_or_config');
  const inputs=path.join(run,'inputs');fs.mkdirSync(inputs);
  write(path.join(inputs,'regular'),'abcd');fs.symlinkSync(path.join(inputs,'regular'),path.join(inputs,'regular-link'));
  assert.equal(readSetupInput(path.join(inputs,'regular-link'),4).toString(),'abcd');
  assert.throws(()=>readSetupInput(path.join(inputs,'regular'),3),error=>error instanceof ProjectSetupError&&error.message.includes('exceeds'));
  write(path.join(inputs,'large'),'');fs.truncateSync(path.join(inputs,'large'),MAX_SETUP_INPUT_BYTES+1);
  assert.throws(()=>readSetupInput(path.join(inputs,'large')),ProjectSetupError);
  command('/usr/bin/mkfifo',[path.join(inputs,'fifo')]);
  fs.symlinkSync('/dev/zero',path.join(inputs,'zero-link'));
  for(const filename of [path.join(inputs,'fifo'),'/dev/zero',path.join(inputs,'zero-link'),inputs])
    assert.throws(()=>readSetupInput(filename),error=>error instanceof ProjectSetupError&&error.message.includes('regular files'));
  assert.equal(readSetupInput(path.join(inputs,'missing')),undefined);
  checks.push('regular_bounded_setup_inputs_allow_regular_symlinks_reject_fifo_device_directory_and_oversize');
  const bad=path.join(run,'scoped/bad'),device=path.join(run,'scoped/device'),good=path.join(run,'scoped/good');
  write(path.join(bad,'package.json'),'{');
  for(const directory of [device,good])write(path.join(directory,'package.json'),JSON.stringify({private:true,name:path.basename(directory),version:'1.0.0'}));
  write(path.join(device,'.npmrc'),'pnpmfile=/dev/zero\n');
  const diagnostics=await preparePackages(path.join(run,'scoped'),[bad,device,good],request,()=>{});
  assert(diagnostics.failure(bad) instanceof ProjectSetupError);
  assert(diagnostics.failure(device) instanceof ProjectSetupError);
  assert.equal(diagnostics.failure(good),undefined);assert.deepEqual(diagnostics(good),[]);
  assert(diagnostics.metadata(device).inputPaths.includes('/dev/zero'));
  // Captured planning metadata must not reparse a changed file after setup.
  write(path.join(good,'package.json'),'{');
  assert.equal(diagnostics.directory(good),good);assert.equal(diagnostics.failure(good),undefined);
  checks.push('expected_package_metadata_failures_are_scoped_and_context_is_not_reparsed');
  const ancestor=path.join(run,'declared'),child=path.join(ancestor,'child'),source=path.join(child,'src');
  write(path.join(ancestor,'package.json'),JSON.stringify({private:true,name:'workspace',version:'1.0.0',workspaces:['child']}));
  write(path.join(child,'package.json'),JSON.stringify({private:true,name:'child',version:'1.0.0',optionalDependencies:{typescript:'5.9.3'}}));
  write(path.join(child,'node_modules/typescript/package.json'),JSON.stringify({name:'typescript',version:'5.9.3'}));
  write(path.join(child,'node_modules/typescript/index.js'),'module.exports = {version:"5.9.3"};\n');
  fs.mkdirSync(source,{recursive:true});
  const declared=await preparePackages(ancestor,[source],request,()=>{});
  assert.equal(declared.typescriptDeclared(source),true);assert.equal(declared.typescriptDeclared(child),true);
  assert.equal(declared.typescriptDeclared(ancestor),false,'child declaration leaked into workspace-root alias');
  write(path.join(ancestor,'package.json'),JSON.stringify({private:true,name:'workspace',version:'1.0.0',workspaces:['child'],devDependencies:{typescript:'5.9.3'}}));
  write(path.join(ancestor,'node_modules/typescript/package.json'),JSON.stringify({name:'typescript',version:'5.9.3'}));
  write(path.join(ancestor,'node_modules/typescript/index.js'),'module.exports = {version:"5.9.3"};\n');
  const hoisted=await preparePackages(ancestor,[source],request,()=>{});
  assert.equal(hoisted.typescriptDeclared(ancestor),true);assert.equal(hoisted.typescriptDeclared(source),true);
  write(path.join(child,'package.json'),'{');
  assert.equal(hoisted.typescriptDeclared(source),true,'declaration lookup reparsed changed metadata');
  checks.push('captured_typescript_declarations_preserve_ancestor_and_alias_scope');
  const boundary=path.join(run,'origins/workspace'),outside=path.join(run,'origins/node_modules/typescript');
  fs.mkdirSync(boundary,{recursive:true});
  write(path.join(outside,'package.json'),JSON.stringify({name:'typescript',version:'5.9.3'}));
  write(path.join(outside,'index.js'),'module.exports = {version:"5.9.3"};\n');
  assert.equal(typescriptInstallation(boundary,boundary),undefined,'ambient ancestor escaped lexical workspace bound');
  fs.mkdirSync(path.join(boundary,'node_modules'),{recursive:true});
  fs.symlinkSync(outside,path.join(boundary,'node_modules/typescript'));
  const linked=typescriptInstallation(boundary,boundary);
  assert.equal(linked.entry,path.join(boundary,'node_modules/typescript'));assert.equal(linked.filename,path.join(outside,'index.js'));
  fs.unlinkSync(linked.entry);fs.symlinkSync(path.join(run,'missing-compiler-store'),linked.entry);
  const broken=typescriptInstallation(boundary,boundary);
  assert.equal(broken.entry,linked.entry);assert(broken.error instanceof ProjectSetupError);assert.equal(broken.filename,undefined);
  checks.push('one_lexical_compiler_origin_resolver_rejects_ambient_ancestors_accepts_external_store_links_and_reports_broken_origins');
  assert.equal(process.env.HOME,actualHome,'fixture changed the account HOME');
  const result={status:'passed',canonical_full_pins:canonicalFullPins,selected_pins:pins,full_catalog:fullCatalog,checks,matrix,commands,run,cache,home_unchanged:true,test_sha256:hash(fileURLToPath(import.meta.url)),
    source_sha256:Object.fromEntries(['providers/typescript/preparation.mjs','providers/typescript/package-tools.mjs','providers/typescript/project-errors.mjs'].map(name=>[name,hash(path.join(root,name))])),
    limits:['finite owned setup fixture; no compiler/index or end-to-end freshness certification','authentication config retained byte-for-byte; no real credential/network-auth exchange tested',fullCatalog?'canonical full floor/latest catalog executed; not every intermediate patch':'only the named selected pins executed; full catalog certification remains pending']};
  fs.writeFileSync(path.join(run,'validation.json'),JSON.stringify(result,null,2)+'\n');console.log(JSON.stringify(result));
} catch(error) {
  const attempt=matrix.at(-1);if(attempt&&attempt.status==='running')attempt.status='failed';
  progress('failed',String(error.stack??error));throw error;
} finally {fs.writeFileSync(path.join(run,'commands.json'),JSON.stringify(commands,null,2)+'\n');}
