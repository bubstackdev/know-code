import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import {checked} from './index.mjs';

const pinNames = ['.swift-version', '.tool-versions', '.xcode-version'];
const exists = file => fs.existsSync(file);
const normalizeDeveloper = directory => {
  directory=path.resolve(directory);
  return directory.endsWith('.app') ? path.join(directory,'Contents/Developer') : directory;
};

function pinsAt(directory,ignoreSwift=false,ignoreXcode=false) {
  let swift, xcode;
  const inputs=[];
  for (const name of pinNames) {
    const filename=path.join(directory,name);
    if (!exists(filename)) continue;
    inputs.push(filename);
    const text=fs.readFileSync(filename,'utf8').trim();
    const entries=name === '.tool-versions'
      ? text.split(/\r?\n/).map(line=>line.split('#')[0].trim().split(/\s+/)).filter(([tool])=>tool==='swift'||tool==='xcode')
      : [[name === '.swift-version' ? 'swift' : 'xcode',...text.split('#')[0].trim().split(/\s+/)]];
    for (const [kind,...values] of entries) {
      if ((kind==='swift' && ignoreSwift) || (kind==='xcode' && ignoreXcode)) continue;
      if (values.length!==1 || !values[0]) throw new Error(`invalid ${kind} pin in ${filename}; specify one installed version/toolchain, not a fallback list`);
      const pin={value:values[0],path:filename};
      const previous=kind==='swift'?swift:xcode;
      if (previous && previous.value!==pin.value) throw new Error(`conflicting ${kind} pins in ${previous.path} and ${filename}; select one toolchain explicitly and align the pins`);
      if (kind==='swift') swift=pin; else xcode=pin;
    }
  }
  return {swift,xcode,inputs};
}

function repositoryPins(directory) {
  let swift,xcode;
  const inputs=[];
  for (let current=directory;;current=path.dirname(current)) {
    const pins=pinsAt(current,Boolean(swift),Boolean(xcode));
    swift??=pins.swift; xcode??=pins.xcode;
    inputs.push(...pins.inputs);
    if (path.dirname(current)===current) break;
  }
  return {swift,xcode,inputs};
}

function plist(file,key,directory) {
  if (!exists(file)) return '';
  try { return checked('/usr/libexec/PlistBuddy',['-c',`Print:${key}`,file],directory); }
  catch { return ''; }
}

function identifierFor(bundle,directory) {
  return plist(path.join(bundle,'ToolchainInfo.plist'),'Identifier',directory)
    || plist(path.join(bundle,'Info.plist'),'CFBundleIdentifier',directory);
}

// One resolver per worker caches installed-tool probes; pin files are read for
// each project. It never changes xcode-select, installs tools, or uses a shim.
export function createToolchainResolver({environment=process.env,
  applicationDirectories=['/Applications',path.join(os.homedir(),'Applications')],
  toolchainDirectories=['/Library/Developer/Toolchains',path.join(os.homedir(),'Library/Developer/Toolchains')]}={}) {
  const base={...environment};
  const probes=new Map();
  let installations;
  function installed(directory) {
    if (installations) return installations;
    const developers=[];
    try { developers.push(checked('/usr/bin/xcode-select',['--print-path'],directory,base)); } catch { /* Required only for the CLT fallback. */ }
    for (const folder of applicationDirectories) {
      if (!exists(folder)) continue;
      for (const name of fs.readdirSync(folder).sort()) {
        if (!/^Xcode.*\.app$/i.test(name)) continue;
        const candidate=path.join(folder,name,'Contents/Developer');
        if (exists(candidate)) developers.push(candidate);
      }
    }
    const xcodes=[...new Set(developers.map(normalizeDeveloper))].filter(d=>exists(path.join(d,'..','Info.plist'))
      && plist(path.join(d,'..','Info.plist'),'CFBundleIdentifier',directory)==='com.apple.dt.Xcode');
    // Keep active Xcode first, then conventional Xcode.app, then other installs.
    const active=developers[0] && normalizeDeveloper(developers[0]);
    xcodes.sort((a,b)=>Number(a!==active)-Number(b!==active)
      || Number(!a.includes('/Xcode.app/'))-Number(!b.includes('/Xcode.app/')));
    const bundles=[];
    for (const folder of [...toolchainDirectories,...xcodes.map(developer=>path.join(developer,'Toolchains'))]) {
      if (!exists(folder)) continue;
      for (const name of fs.readdirSync(folder).sort()) if (name.endsWith('.xctoolchain')) bundles.push(path.join(folder,name));
    }
    return installations={xcodes,bundles};
  }
  function probe(env,directory) {
    const key=JSON.stringify([env.DEVELOPER_DIR,env.TOOLCHAINS,env.SDKROOT,env.SWIFT_EXEC,env.SWIFT_EXEC_MANIFEST]);
    if (probes.has(key)) {
      const value=probes.get(key);
      if (value instanceof Error) throw value;
      return value;
    }
    try {
      const developerDirectory=normalizeDeveloper(env.DEVELOPER_DIR || checked('/usr/bin/xcode-select',['--print-path'],directory,env));
      env={...env,DEVELOPER_DIR:developerDirectory};
      if (env.DEVELOPER_DIR && !exists(normalizeDeveloper(env.DEVELOPER_DIR))) throw new Error(`selected DEVELOPER_DIR is unavailable: ${env.DEVELOPER_DIR}`);
      const compiler=checked('/usr/bin/xcrun',['--find','swiftc'],directory,env);
      const swift=checked('/usr/bin/xcrun',['--find','swift'],directory,env);
      const relative=path.relative(fs.realpathSync(developerDirectory),fs.realpathSync(compiler));
      if (!env.TOOLCHAINS && (relative==='..' || relative.startsWith('../') || path.isAbsolute(relative)))
        throw new Error(`DEVELOPER_DIR=${developerDirectory} resolved a compiler outside that developer directory: ${compiler}`);
      if (fs.realpathSync(path.dirname(swift))!==fs.realpathSync(path.dirname(compiler))) throw new Error('Swift and swiftc resolve to different toolchains');
      for (const name of ['SWIFT_EXEC','SWIFT_EXEC_MANIFEST']) {
        if (env[name] && (!path.isAbsolute(env[name]) || !exists(env[name]) || fs.realpathSync(env[name])!==fs.realpathSync(compiler)))
          throw new Error(`${name}=${env[name]} does not match ${compiler}; select its matching installed toolchain with TOOLCHAINS/DEVELOPER_DIR or point ${name} at that compiler. No compiler was substituted`);
      }
      const version=checked(compiler,['--version'],directory,env);
      if (!/Swift version 6\./.test(version)) throw new Error(`unsupported Swift compiler: ${version}; this provider requires Swift 6.x`);
      const sdk=checked('/usr/bin/xcrun',['--show-sdk-path'],directory,env);
      if (!exists(sdk)) throw new Error(`selected Apple SDK is unavailable: ${sdk}`);
      const library=path.resolve(path.dirname(compiler),'..','lib','libIndexStore.dylib');
      if (!exists(library)) throw new Error(`selected Swift toolchain is missing ${library}`);
      const bundle=path.resolve(path.dirname(compiler),'../..');
      const identifier=identifierFor(bundle,directory);
      const aliases=['default',identifier,path.basename(bundle,'.xctoolchain')];
      if (!identifier || identifier==='com.apple.dt.toolchain.XcodeDefault') aliases.push('XcodeDefault','com.apple.dt.toolchain.XcodeDefault');
      if (env.TOOLCHAINS && !env.TOOLCHAINS.split(',').some(value=>aliases.filter(Boolean).includes(value.trim())))
        throw new Error(`TOOLCHAINS=${env.TOOLCHAINS} did not resolve to the requested installed toolchain (selected ${compiler})`);
      const xcodeVersion=developerDirectory && plist(path.join(developerDirectory,'..','Info.plist'),'CFBundleShortVersionString',directory);
      const result={compiler,version,sdk,library,env,identifier,bundle,xcodeVersion,
        identity:JSON.stringify([compiler,version,sdk,env.TOOLCHAINS??'',env.SWIFT_EXEC_MANIFEST??''])};
      probes.set(key,result);return result;
    } catch (error) { probes.set(key,error);throw error; }
  }
  return directory => {
    const pins=repositoryPins(directory);
    const explicit=Boolean(base.DEVELOPER_DIR || base.TOOLCHAINS || base.SDKROOT || base.SWIFT_EXEC || base.SWIFT_EXEC_MANIFEST);
    const matches=tool=>{
      if (pins.xcode && tool.xcodeVersion!==pins.xcode.value) return false;
      const pin=pins.swift?.value;
      if (!pin || pin==='system') return true;
      if (pin==='xcode') return Boolean(tool.xcodeVersion);
      if (/^\d+\.\d+(?:\.\d+)?$/.test(pin)) {
        const version=/Swift version (\d+\.\d+(?:\.\d+)?)(?=\s|$)/.exec(tool.version)?.[1];
        return version===pin || (pin.split('.').length===2 && version?.startsWith(pin+'.'));
      }
      return tool.identifier===pin || path.basename(tool.bundle,'.xctoolchain')===pin || tool.version.includes(`(${pin})`);
    };
    const finish=(tool,origin)=>({...tool,inputPaths:pins.inputs,selection:{origin,
      developerDirectory:tool.env.DEVELOPER_DIR??null,toolchainIdentifier:tool.identifier||null,
      xcodeVersion:tool.xcodeVersion||null,
      sdkOverride:base.SDKROOT??null,
      swiftPin:pins.swift??null,xcodePin:pins.xcode??null}});
    if (explicit) {
      const tool=probe(base,directory);
      if (!matches(tool)) throw new Error(`explicit Swift toolchain conflicts with repository pin ${pins.swift?.path??pins.xcode?.path}; align the override and pin. Selected: ${tool.version}`);
      return finish(tool,'explicit_override');
    }
    if (pins.swift?.value==='system') {
      const tool=probe(base,directory);
      if (!matches(tool)) throw new Error(`the system Swift selection conflicts with ${pins.xcode.path}; align the repository pins or select a matching developer directory explicitly`);
      return finish(tool,'repository_pin');
    }
    const {xcodes,bundles}=installed(directory);
    const candidates=xcodes.map(developer=>({...base,DEVELOPER_DIR:developer}));
    if (pins.swift && pins.swift.value!=='xcode') {
      for (const bundle of bundles) {
        const identifier=identifierFor(bundle,directory);
        const owner=xcodes.find(developer=>bundle.startsWith(developer+'/'))??xcodes[0];
        if (identifier) candidates.push({...base,TOOLCHAINS:identifier, ...(owner?{DEVELOPER_DIR:owner}:{})});
      }
    }
    if (pins.swift || (!xcodes.length && !pins.xcode)) candidates.push(base);
    const errors=[];
    for (const candidate of candidates) {
      try {
        const tool=probe(candidate,directory);
        if (matches(tool)) return finish(tool,pins.swift||pins.xcode?'repository_pin':candidate.DEVELOPER_DIR?'preferred_xcode':'command_line_tools');
      } catch (error) { errors.push(error.message); }
    }
    throw new Error(`no compatible installed Swift toolchain${pins.swift?` matching ${pins.swift.path} (${pins.swift.value})`:''}${pins.xcode?` matching ${pins.xcode.path} (${pins.xcode.value})`:''}. Install/select the required toolchain separately and refresh. Know Code does not install compilers. ${errors.join('; ')}`);
  };
}
