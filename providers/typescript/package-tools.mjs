import fs from 'node:fs';
import path from 'node:path';
import {createRequire} from 'node:module';
import {createHash, randomUUID} from 'node:crypto';
import {spawn} from 'node:child_process';
import {setTimeout as delay} from 'node:timers/promises';
import {ProjectSetupError} from './project-errors.mjs';

export const DEFAULT_TOOLS = {npm: '11.21.2', pnpm: '12.18.0'};
export const MAX_SETUP_INPUT_BYTES = 64 * 1024 * 1024;
const ownRequire = createRequire(import.meta.url);

// Resolve the logical repository installation, allowing a pnpm entry symlink
// to point into an outside store. Never silently climb above the workspace.
// A broken origin is returned so setup can repair it while compiler selection
// can report its precise unavailable boundary without selecting a substitute.
export function typescriptInstallation(directory, root) {
  directory = path.resolve(directory); root = path.resolve(root);
  const inside = directory === root || directory.startsWith(root + path.sep);
  for (let current = directory;; current = path.dirname(current)) {
    const entry = path.join(current, 'node_modules', 'typescript');
    let present = false;
    try { fs.lstatSync(entry); present = true; }
    catch (error) {
      if (error.code !== 'ENOENT')
        throw new ProjectSetupError('typescript_load_failed', `Cannot inspect selected TypeScript installation ${entry}: ${error.message}`);
    }
    if (present) {
      try {
        // Resolve the current filesystem origin before Node's module resolver:
        // its path cache can retain a previous target of this lexical symlink.
        const target = fs.realpathSync(entry);
        const manifestPath = path.join(target, 'package.json');
        const content = readSetupInput(manifestPath);
        const manifest = content === undefined ? undefined : JSON.parse(content.toString('utf8'));
        let filename;
        if (manifest && Object.hasOwn(manifest, 'exports')) {
          // Absolute-directory resolution ignores package exports. Resolve the
          // selected package's self-reference instead (native TS7 has no main).
          // A differently named package must not fall through to an ancestor.
          if (manifest.name !== 'typescript') throw new Error('Selected compiler exports belong to a package not named typescript');
          filename = createRequire(manifestPath).resolve('typescript');
        } else filename = ownRequire.resolve(target);
        filename = fs.realpathSync(filename);
        const within = path.relative(target, filename);
        if (within === '..' || within.startsWith(`..${path.sep}`) || path.isAbsolute(within))
          throw new Error('Selected compiler entry resolves outside its package origin; no ancestor compiler was substituted');
        return {entry, filename};
      }
      catch (error) {
        return {entry, error: new ProjectSetupError('compiler_unavailable', `Cannot resolve repository TypeScript installation ${entry}: ${error.message}`)};
      }
    }
    if (current === root || !inside) return undefined;
  }
}

function inputFailure(filename, message) {
  const error = new ProjectSetupError('invalid_package_input', `${filename}: ${message}`);
  error.path = filename;
  return error;
}
function inputChunks(filename, visit, maximum = MAX_SETUP_INPUT_BYTES) {
  let fd;
  try {
    // Follow legitimate config/hook symlinks, but do not block opening a FIFO.
    // Validate the opened target, not a racy path stat, before reading any byte.
    fd = fs.openSync(filename, fs.constants.O_RDONLY | fs.constants.O_NONBLOCK);
  } catch (error) {
    if (error.code === 'ENOENT') return false;
    throw inputFailure(filename, `cannot read package setup input (${error.code ?? 'I/O failure'})`);
  }
  try {
    const stat = fs.fstatSync(fd);
    if (!stat.isFile()) throw inputFailure(filename, 'package setup inputs must be regular files');
    if (stat.size > maximum) throw inputFailure(filename, `package setup input exceeds ${maximum} bytes`);
    const buffer = Buffer.allocUnsafe(64 * 1024);
    let total = 0;
    for (;;) {
      // One extra byte detects a file that grows after fstat, without ever
      // allowing readFileSync to allocate/read an unbounded selected target.
      const count = fs.readSync(fd, buffer, 0, Math.min(buffer.length, maximum + 1 - total));
      if (!count) return true;
      total += count;
      if (total > maximum) throw inputFailure(filename, `package setup input exceeds ${maximum} bytes`);
      visit(buffer.subarray(0, count));
    }
  } catch (error) {
    if (error instanceof ProjectSetupError) throw error;
    if (typeof error.code === 'string') throw inputFailure(filename, `cannot read package setup input (${error.code})`);
    throw error;
  } finally { fs.closeSync(fd); }
}
export function readSetupInput(filename, maximum = MAX_SETUP_INPUT_BYTES) {
  const chunks = [];
  return inputChunks(filename, bytes => chunks.push(Buffer.from(bytes)), maximum) ? Buffer.concat(chunks) : undefined;
}
export function hashSetupInput(filename, hash) {
  // Probe existence through the same opened regular-file check. A missing
  // target remains a tracked missing input and never becomes an empty file.
  const content = createHash('sha256');
  const present = inputChunks(filename, bytes => content.update(bytes));
  if (present) hash.update('present\0').update(content.digest());
  else hash.update('missing');
}

export function restorationSafety(tool, allowScripts) {
  if (allowScripts) return {args: [], configArgs: [], env: {}, program: tool.program, prefix: tool.prefix};
  const npm10 = tool.name === 'npm' && tool.major === 10;
  if (npm10 && (tool.program !== process.execPath || tool.prefix.length !== 1 || !path.isAbsolute(tool.prefix[0])))
    throw new Error('Safe npm 10 restoration requires the current Know Code package-tool cache; update the coordinator or restore dependencies separately. No package manager was substituted.');
  const config = {'git': npm10 ? '/usr/bin/false' : '/usr/bin/git', 'script-shell': '/bin/sh'};
  const configArgs = Object.entries(config).map(([key, value]) => `${tool.name === 'npm' ? '--' : '--config.'}${key}=${value}`);
  const args = [...configArgs];
  args.push('--ignore-scripts');
  if (tool.name === 'pnpm') args.push('--ignore-pnpmfile');
  const env = {};
  // A manager may spawn another installer while fetching a Git dependency.
  // Parent CLI options are not automatically inherited by that child. Keep
  // scripts/hooks disabled in both supported managers' environment spellings.
  for (const [key, value] of Object.entries({...config, 'ignore-scripts': 'true', 'ignore-pnpmfile': 'true'})) for (const manager of ['npm', 'pnpm']) {
    env[`${manager}_config_${key.replaceAll('-', '_')}`] = value;
    env[`${manager.toUpperCase()}_CONFIG_${key.replaceAll('-', '_').toUpperCase()}`] = value;
  }
  return {args, configArgs, env, program: tool.program,
    prefix: npm10 ? ['--import', new URL('./npm10-guard.mjs', import.meta.url).href, ...tool.prefix] : tool.prefix};
}

function fileHash(filename) {
  const hash = createHash('sha256');
  if (!inputChunks(filename, bytes => hash.update(bytes), 256 * 1024 * 1024))
    throw inputFailure(filename, 'package-tool entry is missing');
  return hash.digest('hex');
}

// Detect accidental cache corruption throughout the installed tool, including
// required modules and added files. This is relative to registry/TLS trust,
// not authentication against a user who can replace both tree and manifest.
function toolTree(directory) {
  const hash=createHash('sha256'), root=fs.realpathSync(directory);
  let entries=0, bytes=0;
  function walk(folder) {
    for (const name of fs.readdirSync(folder).sort()) {
      const filename=path.join(folder,name), relative=path.relative(directory,filename).split(path.sep).join('/');
      if (relative==='tool.json') continue;
      if (++entries>50000) throw new Error('Package-tool cache exceeds 50000 entries');
      const stat=fs.lstatSync(filename), mode=stat.mode&0o777;
      if (stat.isDirectory()) { hash.update(JSON.stringify([relative,'directory',mode])+'\n');walk(filename); }
      else if (stat.isFile()) {
        bytes+=stat.size;
        if (bytes>512*1024*1024) throw new Error('Package-tool cache exceeds 512 MiB');
        hash.update(JSON.stringify([relative,'file',mode,stat.size,fileHash(filename)])+'\n');
      } else if (stat.isSymbolicLink()) {
        const target=fs.readlinkSync(filename), resolved=path.relative(root,fs.realpathSync(filename));
        if (path.isAbsolute(target)||resolved==='..'||resolved.startsWith(`..${path.sep}`)||path.isAbsolute(resolved))
          throw new Error('Package-tool cache symlink escapes its installed tree');
        hash.update(JSON.stringify([relative,'symlink',target])+'\n');
      } else throw new Error('Package-tool cache contains a non-regular entry');
    }
  }
  walk(directory);
  return {treeSha256:hash.digest('hex'),treeEntries:entries,treeBytes:bytes};
}

export function selection(value, fallback = 'npm') {
  const pin = value ?? `${fallback}@${DEFAULT_TOOLS[fallback]}`;
  const match = /^(npm|pnpm)@(\d+)\.(\d+)\.(\d+)(?:\+sha(224|512)\.([a-fA-F0-9]+))?$/.exec(pin);
  if (!match || !(match[1] === 'npm' ? [10, 11] : [10, 11, 12]).includes(Number(match[2])))
    throw new Error(`Unsupported package manager ${pin}; automatic restoration supports stable npm 10–11 and pnpm 10–12 on Node 24. No substitution was made.`);
  if (match[5] && match[6].length !== Number(match[5]) / 4)
    throw new Error(`Invalid integrity suffix in package manager ${pin}`);
  return {name: match[1], version: `${match[2]}.${match[3]}.${match[4]}`, major: Number(match[2]),
    pin, origin: value ? 'repository' : 'know-code-default', hashAlgorithm: match[5] && `sha${match[5]}`, hash: match[6]?.toLowerCase()};
}

export function runCommand(program, args, directory, send, env = {}, outputLimit = 8192) {
  return new Promise((resolve, reject) => {
    const child = spawn(program, args, {cwd: directory, stdio: ['ignore', 'pipe', 'pipe'],
      env: {...process.env, npm_config_update_notifier: 'false', npm_config_logs_max: '0', pnpm_config_update_notifier: 'false',
        PNPM_CONFIG_UPDATE_NOTIFIER: 'false', NO_UPDATE_NOTIFIER: '1', ...env}});
    let tail = '', stdout = '', last = Date.now(), overflow = false;
    child.stdout.on('data', bytes => { stdout = (stdout + bytes.toString('utf8')).slice(-outputLimit); });
    for (const stream of [child.stdout, child.stderr]) stream.on('data', bytes => {
      tail += bytes.toString('utf8');
      overflow ||= tail.length > outputLimit;
      tail = tail.slice(-outputLimit);
      if (Date.now() - last >= 500) {
        last = Date.now();
        send?.('progress', {stage: 'restoring', projectPath: directory, message: tail.slice(-512)});
      }
    });
    child.once('error', reject);
    child.once('close', code => code === 0 && !(overflow && outputLimit > 8192) ? resolve(stdout.trim()) : reject(new Error(`${path.basename(program)} ${args.join(' ')} failed (${code}${overflow && outputLimit > 8192 ? ', oversized output' : ''}): ${tail.slice(-8192)}`)));
  });
}

async function registryMetadata(name, version) {
  const url = `https://registry.npmjs.org/${encodeURIComponent(name)}/${version}`;
  let response;
  try { response = await fetch(url, {signal: AbortSignal.timeout(30000)}); }
  catch (error) { throw new Error(`Cannot acquire ${name}@${version} from the public package registry: ${error.cause?.code ?? error.message}; check network access and retry preparation`, {cause: error}); }
  if (!response.ok) throw new Error(`Package-tool metadata ${name}@${version}: HTTP ${response.status}`);
  const limit = 1024 * 1024;
  if (Number(response.headers.get('content-length')) > limit) {
    await response.body?.cancel();
    throw new Error('Oversized package-tool metadata');
  }
  const chunks = [];
  let length = 0;
  for await (const chunk of response.body) {
    length += chunk.byteLength;
    if (length > limit) throw new Error('Oversized package-tool metadata');
    chunks.push(chunk);
  }
  const value = JSON.parse(Buffer.concat(chunks, length).toString('utf8'));
  if (value.name !== name || value.version !== version || !value.dist?.integrity?.startsWith('sha512-'))
    throw new Error(`Invalid registry identity/integrity for ${name}@${version}`);
  return value;
}

async function archive(metadata, filename, selected) {
  const url = new URL(metadata.dist.tarball);
  if (url.protocol !== 'https:' || url.hostname !== 'registry.npmjs.org') throw new Error('Unexpected package-tool archive origin');
  const response = await fetch(url, {signal: AbortSignal.timeout(180000)});
  if (!response.ok) throw new Error(`Package-tool archive: HTTP ${response.status}`);
  const sha512 = createHash('sha512'), pinHash = selected.hashAlgorithm && createHash(selected.hashAlgorithm);
  let bytes = 0;
  const fd = fs.openSync(filename, 'wx');
  try {
    for await (const chunk of response.body) {
      bytes += chunk.length;
      if (bytes > 128 * 1024 * 1024) throw new Error('Package-tool archive exceeds 128 MiB');
      sha512.update(chunk); pinHash?.update(chunk);
      for (let offset = 0; offset < chunk.length;) offset += fs.writeSync(fd, chunk, offset);
    }
  } finally { fs.closeSync(fd); }
  if (`sha512-${sha512.digest('base64')}` !== metadata.dist.integrity || pinHash && pinHash.digest('hex') !== selected.hash)
    throw new Error(`Package-tool integrity mismatch for ${selected.pin}`);
}

function cached(directory, selected) {
  try {
    const manifest = JSON.parse(readSetupInput(path.join(directory, 'tool.json'), 1024 * 1024));
    if (manifest.format !== 2 || manifest.name !== selected.name || manifest.version !== selected.version ||
        manifest.pinHash !== (selected.hash ?? '') || manifest.platform !== `${process.platform}-${process.arch}` ||
        manifest.native !== (selected.name==='pnpm'&&selected.major===12) ||
        !manifest.archiveIntegrity?.startsWith('sha512-') || typeof manifest.entry !== 'string' ||
        path.isAbsolute(manifest.entry) || manifest.entry.split(path.sep).includes('..')) return;
    const entry = path.join(directory, manifest.entry);
    if (fileHash(entry) !== manifest.entrySha256) return;
    const tree=toolTree(directory);
    if (tree.treeSha256!==manifest.treeSha256 || tree.treeEntries!==manifest.treeEntries || tree.treeBytes!==manifest.treeBytes) return;
    return {...selected, program: manifest.native ? entry : process.execPath,
      // We already selected/verified the exact pin. Suppress pnpm's independent
      // bootstrap (including native v12's unconditional config-dependency fetch).
      prefix: [...(manifest.native ? [] : [entry]), ...(selected.name === 'pnpm' && selected.major >= 11 ? ['with', 'current'] : [])],
      source: 'user-cache', cacheDirectory: directory, archiveIntegrity: manifest.archiveIntegrity};
  } catch { /* Missing or incomplete cache entries are never used. */ }
}

// One cross-process cache/lock path for both managers; no package installation scripts.
export async function packageTool(selected, cacheDirectory, send) {
  const directory = path.join(cacheDirectory, `${selected.name}-${selected.version}${selected.hash ? `-${selected.hashAlgorithm}-${selected.hash}` : ''}-${process.platform}-${process.arch}`);
  const existing = cached(directory, selected);
  if (existing) return existing;
  fs.mkdirSync(cacheDirectory, {recursive: true});
  const lock = `${directory}.lock`, deadline = Date.now() + 300000;
  const token = `${process.pid}-${randomUUID()}`;
  const stage = `${directory}.staging-${token}`;
  const ownerPath = path.join(lock, `owner-${token}.json`);
  for (;;) {
    try {
      fs.mkdirSync(lock);
      fs.writeFileSync(ownerPath, JSON.stringify({pid: process.pid, stage: path.basename(stage)}), {flag: 'wx'});
      break;
    } catch (error) {
      if (error.code !== 'EEXIST') throw error;
      const ready = cached(directory, selected);
      if (ready) return ready;
      try {
        const stat = fs.statSync(lock);
        const entries = fs.readdirSync(lock);
        if (entries.length === 1 && entries[0].startsWith('owner-')) {
          const stalePath = path.join(lock, entries[0]);
          const owner = JSON.parse(readSetupInput(stalePath, 1024 * 1024));
          try { process.kill(owner.pid, 0); } catch (error) {
            if (error.code === 'ESRCH') {
              // Unique owner filenames prevent a second reclaimer from unlinking
              // a newly acquired live lock after the stale directory is removed.
              fs.unlinkSync(stalePath);
              fs.rmdirSync(lock);
              if (typeof owner.stage === 'string' && owner.stage.startsWith(`${path.basename(directory)}.staging-`) && path.basename(owner.stage) === owner.stage)
                fs.rmSync(path.join(cacheDirectory, owner.stage), {recursive: true, force: true});
            }
          }
        } else if (!entries.length && Date.now() - stat.mtimeMs > 60000) fs.rmdirSync(lock);
      } catch (error) { if (error.code !== 'ENOENT') throw error; }
      if (Date.now() >= deadline) throw new Error(`Package-tool cache busy for ${selected.pin}; retry preparation later`);
      await delay(100);
    }
  }
  try {
    const ready = cached(directory, selected);
    if (ready) return ready;
    send('progress', {stage: 'preparing', projectPath: cacheDirectory, message: `Acquiring ${selected.pin} into the user package-tool cache`});
    const native = selected.name === 'pnpm' && selected.major === 12;
    if (native && (process.platform !== 'darwin' || process.arch !== 'arm64'))
      throw new Error('This Know Code candidate supports pnpm 12 tools on macOS ARM64 only');
    const metadata = await registryMetadata(native ? '@pnpm/exe.darwin-arm64' : selected.name, selected.version);
    fs.mkdirSync(stage);
    fs.writeFileSync(path.join(stage, 'package.json'), '{"private":true}');
    const filename = path.join(stage, 'archive.tgz');
    // A Corepack pin hash names the pnpm package archive, not its native optional dependency.
    if (native && selected.hash) {
      const wrapper = await registryMetadata(selected.name, selected.version);
      await archive(wrapper, path.join(stage, 'pin.tgz'), selected);
    }
    await archive(metadata, filename, native ? {...selected, hashAlgorithm: undefined, hash: undefined} : selected);
    const listing = await runCommand('/usr/bin/tar', ['-tzf', filename], stage, undefined, {}, 4 * 1024 * 1024);
    if (listing.split('\n').some(name => !name.startsWith('package/') || name.split('/').includes('..')))
      throw new Error('Invalid package-tool archive paths');
    const content = path.join(stage, 'content');
    fs.mkdirSync(content);
    await runCommand('/usr/bin/tar', ['-xzf', filename, '--strip-components=1', '--no-same-owner', '-C', content], stage);
    const pkg = JSON.parse(readSetupInput(path.join(content, 'package.json')));
    if (pkg.version !== selected.version || pkg.name !== metadata.name) throw new Error('Extracted package-tool identity mismatch');
    const bin = native ? ['pnpm', 'bin/pnpm'].find(p => fs.existsSync(path.join(content, p))) : pkg.bin?.[selected.name];
    if (!bin || path.isAbsolute(bin) || bin.split('/').includes('..')) throw new Error('Invalid package-tool executable');
    const entry = path.join(content, bin);
    if (native) fs.chmodSync(entry, 0o755);
    const version = await runCommand(native ? entry : process.execPath, [...(native ? [] : [entry]), '--version'], stage,
      undefined, {COREPACK_ENABLE_PROJECT_SPEC: '0', npm_config_manage_package_manager_versions: 'false', pnpm_config_manage_package_manager_versions: 'false'});
    if (version !== selected.version) throw new Error(`Package-tool version mismatch: expected ${selected.version}, got ${version}`);
    fs.writeFileSync(path.join(content, 'tool.json'), JSON.stringify({format: 2, name: selected.name, version: selected.version,
      pinHash: selected.hash ?? '', platform: `${process.platform}-${process.arch}`, native, entry: bin,
      archiveIntegrity: metadata.dist.integrity, registryPackage: metadata.name,
      entrySha256: fileHash(entry),...toolTree(content)}));
    fs.rmSync(directory, {recursive: true, force: true});
    fs.renameSync(content, directory);
    return cached(directory, selected);
  } finally {
    fs.rmSync(stage, {recursive: true, force: true});
    fs.unlinkSync(ownerPath);
    fs.rmdirSync(lock);
  }
}
