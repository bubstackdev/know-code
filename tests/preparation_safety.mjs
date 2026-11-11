// Finite owned package-manager executable-selector and setup-input regressions.
// This is adapter setup validation, not compiler/index/throughput certification.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import {spawnSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import {createHash} from 'node:crypto';
import {createServer} from 'node:http';
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
write(path.join(git,'package.json'),JSON.stringify({name:'know-code-safe-git-dependency',version:'1.0.0',types:'index.d.ts',bin:{'know-code-owned-tool':'cli.cjs'},
  scripts:{prepare:`node -e ${quote(`require('fs').writeFileSync(${JSON.stringify(path.join(run,'dependency-prepare-ran'))},'bad')`)}`}}));
write(path.join(git,'index.d.ts'),'export declare const value: number;\n');
write(path.join(git,'cli.cjs'),'#!/usr/bin/env node\nconsole.log("owned fixture bin");\n');fs.chmodSync(path.join(git,'cli.cjs'),0o755);
command('/usr/bin/git',['init','--quiet',git]);
command('/usr/bin/git',['add','package.json','index.d.ts','cli.cjs'],git);
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
function installLinksEnvironment(value) {
  return Object.fromEntries(['npm_config_install_links','NPM_CONFIG_INSTALL_LINKS','pnpm_config_install_links','PNPM_CONFIG_INSTALL_LINKS'].map(key=>[key,value]));
}
async function withEnvironment(values,action) {
  const previous=Object.fromEntries(Object.keys(values).map(key=>[key,process.env[key]]));
  for(const [key,value] of Object.entries(values)) {if(value===undefined)delete process.env[key];else process.env[key]=value;}
  try {return await action();} finally {for(const [key,value] of Object.entries(previous)){if(value===undefined)delete process.env[key];else process.env[key]=value;}}
}
const tarStage=path.join(run,'tar-stage/package'),tarball=path.join(run,'dependency.tgz');
fs.mkdirSync(tarStage,{recursive:true});
for(const name of ['package.json','index.d.ts','cli.cjs'])fs.copyFileSync(path.join(git,name),path.join(tarStage,name));
command('/usr/bin/tar',['-czf',tarball,'-C',path.dirname(tarStage),'package']);
const tarBytes=fs.readFileSync(tarball),httpRequests=[];
const server=createServer((req,res)=>{
  httpRequests.push({method:req.method,url:req.url});
  if(req.url==='/dependency.tgz') {res.writeHead(200,{'content-type':'application/octet-stream','content-length':tarBytes.length});res.end(tarBytes);return;}
  if(req.url==='/npm/know-code-safe-git-dependency') {
    const pkg={name:'know-code-safe-git-dependency',version:'1.0.0',dist:{tarball:`${local}/dependency.tgz`,integrity:`sha512-${createHash('sha512').update(tarBytes).digest('base64')}`}};
    res.writeHead(200,{'content-type':'application/json'});res.end(JSON.stringify({name:pkg.name,'dist-tags':{latest:pkg.version},versions:{[pkg.version]:pkg}}));return;
  }
  res.writeHead(403);res.end('owned fixture denies outward proxy requests');
});
server.on('connect',(req,socket)=>{httpRequests.push({method:'CONNECT',url:req.url});socket.end('HTTP/1.1 403 Forbidden\r\nConnection: close\r\n\r\n');});
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
const local=`http://127.0.0.1:${server.address().port}`;
async function npm10Case(pin,label,dependency,config,environment) {
  const directory=path.join(run,pin.replaceAll('@','-'),label),messages=[];
  write(path.join(directory,'package.json'),JSON.stringify({name:'know-code-safe-app',private:true,version:'1.0.0',packageManager:pin,
    dependencies:{'know-code-safe-git-dependency':dependency},scripts:{install:`node -e ${quote(`require('fs').writeFileSync(${JSON.stringify(path.join(run,'root-install-ran'))},'bad')`)}`}}));
  write(path.join(directory,'.npmrc'),`ignore-scripts=false\n${config}\n`);
  const configHash=hash(path.join(directory,'.npmrc')),attempt={pin,case:label,directory,status:'running',messages,config_sha256:configHash};
  matrix.push(attempt);progress();
  const diagnostics=await withEnvironment(environment,()=>preparePackages(directory,[directory],request,(kind,value)=>{messages.push({kind,value});progress();}));
  const preparation=messages.filter(message=>message.kind==='preparation').map(message=>message.value);
  Object.assign(attempt,{preparation,diagnostics:diagnostics(directory),metadata:diagnostics.metadata(directory),setup_failure:diagnostics.failure(directory)?.message});progress();
  assert.equal(diagnostics.failure(directory),undefined);assert.equal(preparation.length,1);
  for(const marker of ['dependency-prepare-ran','root-install-ran'])assert(!fs.existsSync(path.join(run,marker)),`ungranted script ran in ${label}`);
  assert.equal(hash(path.join(directory,'.npmrc')),configHash,'policy rewrote repository config');
  return {directory,preparation:preparation[0],attempt,done:()=>{attempt.status='passed';progress();}};
}
try {
  for(const pin of pins) {
    const selected=selection(pin),yamlProxy=selected.name==='pnpm'&&selected.major>=11;
    const directory=path.join(run,pin.replaceAll('@','-'));
    const gitMarker=path.join(directory,'git-selector-ran'),shellMarker=path.join(directory,'shell-selector-ran');
    write(path.join(directory,'selector-git'),`#!/bin/sh\nprintf bad > ${quote(gitMarker)}\nexit 37\n`);
    write(path.join(directory,'selector-shell'),`#!/bin/sh\nprintf bad > ${quote(shellMarker)}\nexit 38\n`);
    fs.chmodSync(path.join(directory,'selector-git'),0o755);fs.chmodSync(path.join(directory,'selector-shell'),0o755);
    write(path.join(directory,'package.json'),JSON.stringify({name:'know-code-safe-app',version:'1.0.0',private:true,packageManager:pin,
      dependencies:{'know-code-safe-git-dependency':`git+file://${git}#${revision}`},
      scripts:{install:`node -e ${quote(`require('fs').writeFileSync(${JSON.stringify(path.join(run,'root-install-ran'))},'bad')`)}`}}));
    const config=`git=./selector-git\nscript-shell=./selector-shell\nnode-gyp=./selector-shell\nignore-scripts=false\ninstall-links=false\nallow-git=true\nregistry=${registry}\nproxy=${proxy}\n//registry.example.invalid/:_authToken=owned-fake-token\n`;
    write(path.join(directory,'.npmrc'),config);
    if(pin.startsWith('pnpm'))write(path.join(directory,'pnpm-workspace.yaml'),`packages:\n  - .\nscriptShell: ./selector-shell\n${yamlProxy?`httpProxy: ${JSON.stringify(proxy)}\nhttpsProxy: ${JSON.stringify(proxy)}\n`:''}`);
    const configFiles=['.npmrc',...(pin.startsWith('pnpm')?['pnpm-workspace.yaml']:[])];
    const configHashes=Object.fromEntries(configFiles.map(name=>[name,hash(path.join(directory,name))]));
    const configHash=configHashes['.npmrc'];const messages=[];
    const attempt={pin,directory,status:'running',messages,config_sha256:configHash,config_files_sha256:configHashes,
      proxy_config_source:yamlProxy?'pnpm-workspace.yaml:httpProxy,httpsProxy':'.npmrc:proxy'};matrix.push(attempt);progress();
    const diagnostics=await withEnvironment(installLinksEnvironment('false'),()=>preparePackages(directory,[directory],request,(kind,value)=>{messages.push({kind,value});progress();}));
    const preparation=messages.filter(message=>message.kind==='preparation').map(message=>message.value);
    Object.assign(attempt,{preparation,diagnostics:diagnostics(directory),setup_failure:diagnostics.failure(directory)?.message,
      markers:Object.fromEntries([gitMarker,shellMarker,path.join(run,'dependency-prepare-ran'),path.join(run,'root-install-ran')].map(filename=>[filename,fs.existsSync(filename)]))});progress();
    assert.equal(diagnostics.failure(directory),undefined);
    assert.equal(preparation.length,1);
    const denied=selected.name==='npm'&&selected.major===10;
    if(denied) {
      assert.equal(preparation[0].succeeded,false);assert(preparation[0].error.includes('EKNOWCODE_NPM10_GIT_DISABLED'),preparation[0].error);
      assert.equal(diagnostics(directory)[0]?.code,'dependency_restore_failed');attempt.outcome='npm10_git_fetch_refused';
    } else {
      assert.deepEqual(diagnostics(directory),[]);assert(preparation[0].succeeded,JSON.stringify(preparation));
      assert(fs.existsSync(path.join(directory,'node_modules/know-code-safe-git-dependency/package.json')),'real Git dependency was not installed');attempt.outcome='git_dependency_restored';
    }
    for(const filename of [gitMarker,shellMarker,path.join(run,'dependency-prepare-ran'),path.join(run,'root-install-ran')])assert(!fs.existsSync(filename),`ungranted executable ran: ${filename}`);
    for(const [name,expected] of Object.entries(configHashes))assert.equal(hash(path.join(directory,name)),expected,`setup rewrote repository ${name}`);
    const tool=await packageTool(selection(pin),cache,()=>{}),safety=restorationSafety(tool,false);
    if(denied) {
      const probe=`const assert=require('node:assert/strict'),path=require('node:path');
const files=Object.entries(require.cache).filter(([,value])=>value.exports?.name==='DisabledGitFetcher').map(([filename])=>filename);
assert(files.length>0);const result=[];
for(const filename of files){const pacote=require(path.join(path.dirname(filename),'index.js'));const codes=[];
for(const invoke of [()=>pacote.manifest('github:know-code-fixture/never-fetch#${revision}',{cache:${JSON.stringify(path.join(run,'probe-cache'))}}),()=>new pacote.GitFetcher('github:know-code-fixture/never-fetch#${revision}',{})]){let code;try{invoke();}catch(error){code=error.code;}assert.equal(code,'EKNOWCODE_NPM10_GIT_DISABLED');codes.push(code);}result.push({filename,codes});}
console.log(JSON.stringify(result));`;
      const factories=JSON.parse(command(safety.program,[...safety.prefix.slice(0,-1),'--input-type=commonjs','--eval',probe,tool.prefix[0]],directory));
      if(pin==='npm@10.9.10')assert(factories.length>=2,'nested Pacote copy escaped its process-local guard');
      const scriptProbe=`const assert=require('node:assert/strict');
(async()=>{const entries=Object.entries(require.cache).filter(([,value])=>value.exports?.name==='DisabledPackageScripts');assert(entries.length>0);const results=[];
for(const [filename,value] of entries){assert.equal(typeof value.exports.isServerPackage,'function');const result=await value.exports({event:'prepare',path:${JSON.stringify(git)}});assert.deepEqual(result,{args:[],code:0,signal:null,stdout:'',stderr:''});results.push({filename,result});}console.log(JSON.stringify(results));})().catch(error=>{console.error(error);process.exitCode=1});`;
      const scriptRunners=JSON.parse(command(safety.program,[...safety.prefix.slice(0,-1),'--input-type=commonjs','--eval',scriptProbe,tool.prefix[0]],directory));
      attempt.guarded_factories=factories;attempt.guarded_script_runners=scriptRunners;progress();
    }
    const configured={};
    for(const key of ['registry','proxy',...(yamlProxy?['httpsProxy']:[])]) {
      // pnpm 11+ config get filters non-explicit network INI keys even though
      // its reader consumes them. YAML marks the real network fields explicit.
      const queryKey=key==='proxy'&&yamlProxy?'httpProxy':key;
      const output=await runCommand(safety.program,[...safety.prefix,'config','get',queryKey,...safety.configArgs],directory,undefined,safety.env);
      assert(output.includes(key==='registry'?registry:proxy),`${pin} lost ${queryKey} configuration: ${output}`);configured[key]={query_key:queryKey,output};
    }
    for(const [name,expected] of Object.entries(configHashes))assert.equal(hash(path.join(directory,name)),expected,`config query rewrote repository ${name}`);
    Object.assign(attempt,{status:'passed',configured,tool:{program:safety.program,prefix:safety.prefix,source:tool.source}});progress();
  }
  checks.push(fullCatalog?'full_catalog_npm10_git_refusal_and_npm11_pnpm_git_restores_preserve_selectors_registry_proxy':'selected_pins_only_git_policy_preserves_selectors_registry_proxy');
  for(const pin of pins.filter(pin=>{const selected=selection(pin);return selected.name==='npm'&&selected.major===10;})) {
    const probeCount=httpRequests.length;
    const hosted=await npm10Case(pin,'hosted-full-sha',`github:know-code-fixture/never-fetch#${revision}`,
      `install-links=false\nproxy=${local}\nhttps-proxy=${local}\nfetch-retries=0`,
      {...installLinksEnvironment('false'),HTTP_PROXY:local,HTTPS_PROXY:local,NO_PROXY:'127.0.0.1,localhost',npm_config_noproxy:'127.0.0.1,localhost'});
    assert.equal(hosted.preparation.succeeded,false);assert(hosted.preparation.error.includes('EKNOWCODE_NPM10_GIT_DISABLED'),hosted.preparation.error);
    assert.equal(httpRequests.length,probeCount,'hosted Git reached the denying proxy before policy rejection');hosted.done();
    for(const [label,setting,environment] of [
      ['packing-repository-true','true',installLinksEnvironment(undefined)],
      ['packing-personal-env-true','false',installLinksEnvironment('true')]
    ]) {
      const packed=await npm10Case(pin,label,`file:${git}`,`install-links=${setting}`,environment);
      assert.equal(packed.preparation.succeeded,false);assert(packed.preparation.error.includes('effective install-links=true'),packed.preparation.error);
      assert.equal(packed.attempt.metadata.packageManager.restorationSafety.installLinks,true);
      assert(!fs.existsSync(path.join(packed.directory,'node_modules')),'unsafe packing began installation');packed.done();
    }
    const linked=await npm10Case(pin,'safe-linked-directory',`file:${git}`,'install-links=true',installLinksEnvironment('false'));
    assert(linked.preparation.succeeded,linked.preparation.error);
    assert(fs.lstatSync(path.join(linked.directory,'node_modules/know-code-safe-git-dependency')).isSymbolicLink());
    assert(fs.lstatSync(path.join(linked.directory,'node_modules/.bin/know-code-owned-tool')).isSymbolicLink(),'scripts-off linked directory lost its ordinary executable link');
    assert.equal(linked.attempt.metadata.packageManager.restorationSafety.installLinks,false);linked.done();
    const file=await npm10Case(pin,'safe-file-tarball',`file:${tarball}`,'install-links=false',installLinksEnvironment('false'));
    assert(file.preparation.succeeded,file.preparation.error);assert(fs.existsSync(path.join(file.directory,'node_modules/know-code-safe-git-dependency/package.json')));file.done();
    const registry=await npm10Case(pin,'safe-registry-package','1.0.0',`install-links=false\nregistry=${local}/npm/\nproxy=\nhttps-proxy=`,
      {...installLinksEnvironment('false'),HTTP_PROXY:'',HTTPS_PROXY:'',NO_PROXY:'127.0.0.1,localhost'});
    assert(registry.preparation.succeeded,registry.preparation.error);assert(fs.existsSync(path.join(registry.directory,'node_modules/know-code-safe-git-dependency/package.json')));registry.done();
  }
  if(pins.some(pin=>pin.startsWith('npm@10.')))checks.push('selected_npm10_hosted_git_denied_before_proxy_packing_true_rejected_and_registry_tarball_linked_directories_restore');
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
  const diagnostics=await preparePackages(path.join(run,'scoped'),[bad,device,good],{...request,retryPreparation:false},()=>{});
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
  const declared=await preparePackages(ancestor,[source],{...request,retryPreparation:false},()=>{});
  assert.equal(declared.typescriptDeclared(source),true);assert.equal(declared.typescriptDeclared(child),true);
  assert.equal(declared.typescriptDeclared(ancestor),false,'child declaration leaked into workspace-root alias');
  write(path.join(ancestor,'package.json'),JSON.stringify({private:true,name:'workspace',version:'1.0.0',workspaces:['child'],devDependencies:{typescript:'5.9.3'}}));
  write(path.join(ancestor,'node_modules/typescript/package.json'),JSON.stringify({name:'typescript',version:'5.9.3'}));
  write(path.join(ancestor,'node_modules/typescript/index.js'),'module.exports = {version:"5.9.3"};\n');
  const hoisted=await preparePackages(ancestor,[source],{...request,retryPreparation:false},()=>{});
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
    source_sha256:Object.fromEntries(['providers/typescript/preparation.mjs','providers/typescript/package-tools.mjs','providers/typescript/project-errors.mjs','providers/typescript/npm10-guard.mjs'].map(name=>[name,hash(path.join(root,name))])),
    limits:['finite owned setup fixture; no compiler/index or end-to-end freshness certification','authentication config retained byte-for-byte; no real credential/network-auth exchange tested','npm10 Git refusal is an expected negative policy test, not successful Git restoration',fullCatalog?'canonical full floor/latest catalog executed; not every intermediate patch':'only the named selected pins executed; full catalog certification remains pending']};
  fs.writeFileSync(path.join(run,'validation.json'),JSON.stringify(result,null,2)+'\n');console.log(JSON.stringify(result));
} catch(error) {
  const attempt=matrix.at(-1);if(attempt&&attempt.status==='running')attempt.status='failed';
  progress('failed',String(error.stack??error));throw error;
} finally {
  await new Promise(resolve=>server.close(resolve));
  fs.writeFileSync(path.join(run,'commands.json'),JSON.stringify(commands,null,2)+'\n');
  fs.writeFileSync(path.join(run,'http-requests.json'),JSON.stringify(httpRequests,null,2)+'\n');
}
