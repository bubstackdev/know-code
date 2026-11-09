import fs from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';
import { createHash } from 'node:crypto';
import { spawn, execFileSync } from 'node:child_process';

export function context(root, directory) {
  let nearest;
  for (let current = directory;; current = path.dirname(current)) {
    const manifest = path.join(current, 'package.json');
    if (fs.existsSync(manifest)) {
      const pkg = JSON.parse(fs.readFileSync(manifest, 'utf8'));
      if (!nearest) nearest = {directory: current, pkg};
      const patterns = Array.isArray(pkg.workspaces) ? pkg.workspaces : pkg.workspaces?.packages ?? [];
      if (current === nearest.directory || patterns.some(pattern => path.matchesGlob(path.relative(current, nearest.directory), pattern))) {
        nearest.install = current;
        nearest.manager = pkg.packageManager;
      }
    }
    if (current === root || path.dirname(current) === current || !current.startsWith(root + path.sep)) break;
  }
  return nearest;
}

async function command(directory, args, send) {
  return new Promise(resolve => {
    const child = spawn('npm', args, {cwd: directory, stdio: ['ignore', 'pipe', 'pipe'],
      env: {...process.env, npm_config_update_notifier: 'false'}});
    let tail = '';
    let last = Date.now();
    for (const stream of [child.stdout, child.stderr]) stream.on('data', bytes => {
      tail = (tail + bytes.toString('utf8')).slice(-4096);
      if (Date.now() - last >= 500) {
        last = Date.now();
        send('progress', {stage: 'restoring', projectPath: directory, message: tail.slice(-512)});
      }
    });
    child.on('error', error => resolve(`Cannot launch npm: ${error.message}; install the repository's package tooling and put it on PATH`));
    child.on('close', code => resolve(code === 0 ? '' : `npm ${args.join(' ')} failed (${code}) in ${directory}:\n${tail}`));
  });
}

// The core owns checkpoints/publication; this adapter owns package boundaries and commands.
export async function preparePackages(root, directories, request, send) {
  const groups = new Map();
  const contexts = new Map();
  for (const directory of new Set(directories)) {
    const item = context(root, directory);
    if (!item) continue;
    contexts.set(directory, item.install);
    if (!groups.has(item.install)) groups.set(item.install, {manager: item.manager, packages: new Map()});
    groups.get(item.install).packages.set(item.directory, item.pkg);
  }
  const errors = new Map();
  for (const [directory, group] of groups) {
    const key = `npm:${directory}`;
    const fingerprint = createHash('sha256').update(`${request.preparationFingerprint}:${process.version}:npm-scripts-disabled-v1`).digest('hex');
    const previous = request.preparations.find(p => p.key === key);
    let outputs = [];
    function missingDependencies() {
      outputs = [];
      const missing = [];
      for (const [member, pkg] of group.packages) {
        const require = createRequire(path.join(member, '__know_code__.cjs'));
        for (const name of Object.keys({...pkg.dependencies, ...pkg.devDependencies})) {
          const found = require.resolve.paths(name)?.map(p => path.join(p, name)).find(p => p.startsWith(root + path.sep) && fs.existsSync(p));
          if (found) outputs.push(found); else missing.push(name);
        }
      }
      return missing;
    }
    const missing = missingDependencies().length > 0;
    let error = '';
    if (previous?.fingerprint === fingerprint && !request.retryPreparation && (!previous.succeeded || !missing)) error = previous.error;
    else {
      const locked = ['npm-shrinkwrap.json', 'package-lock.json'].some(name => fs.existsSync(path.join(directory, name)));
      const otherLock = ['pnpm-lock.yaml', 'yarn.lock', 'bun.lock', 'bun.lockb'].find(name => fs.existsSync(path.join(directory, name)));
      const needed = missing || previous && previous.fingerprint !== fingerprint || request.retryPreparation;
      if (needed && (group.manager && !group.manager.startsWith('npm@') || !group.manager && otherLock))
        error = `Automatic restoration for ${group.manager ?? otherLock} is not supported by this provider yet; restore with the selected package manager and refresh. No npm substitution was made.`;
      else if (needed) {
        send('progress', {stage: 'restoring', projectPath: directory, message: 'Restoring npm dependencies with install scripts disabled'});
        // Do not create a lockfile or rewrite dependency constraints for an unpinned project.
        const args = locked ? ['ci'] : ['install', '--package-lock=false'];
        args.push('--ignore-scripts', '--no-audit', '--no-fund', '--include=dev');
        if (group.manager) {
          try {
            const actual = execFileSync('npm', ['--version'], {cwd: directory, encoding: 'utf8', maxBuffer: 8192}).trim();
            const selected = group.manager.slice(4).split('+')[0];
            if (actual !== selected) error = `Repository selects ${group.manager}, but npm ${actual} is on PATH; install the selected package manager version and refresh. No substitution was made.`;
          } catch (cause) { error = `Cannot check selected package manager ${group.manager}: ${cause.message}`; }
        }
        if (!error) error = await command(directory, args, send);
        if (!error) {
          const absent = missingDependencies();
          if (absent.length) error = `Dependency restoration finished but these direct dependencies remain unavailable: ${absent.join(", ")}`;
        }
      }
      error = error.slice(-4096);
      send('preparation', {key, fingerprint, succeeded: !error, error, outputs});
    }
    if (error) errors.set(directory, {code: 'dependency_restore_failed', severity: 'error', message: error, path: directory});
  }
  return directory => {
    const error = errors.get(contexts.get(directory) ?? context(root, directory)?.install);
    return error ? [error] : [];
  };
}
