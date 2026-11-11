import fs from 'node:fs';
import path from 'node:path';
import {createRequire} from 'node:module';
import {createHash} from 'node:crypto';
import YAML from 'yaml';
import ini from 'ini';
import {packageTool, runCommand, selection, readSetupInput, hashSetupInput, restorationSafety, typescriptInstallation} from './package-tools.mjs';
import {ProjectSetupError} from './project-errors.mjs';

function invalidInput(filename, message) {
  const error = new ProjectSetupError('invalid_package_configuration', `${filename}: ${message}`);
  error.path = filename;
  return error;
}
function parsedInput(filename, parser) {
  const bytes = readSetupInput(filename);
  if (bytes === undefined) return undefined;
  try { return parser(bytes.toString('utf8')); }
  catch (error) {
    if (error instanceof SyntaxError || error.name === 'YAMLParseError')
      throw invalidInput(filename, error.message);
    throw error;
  }
}
function packageInput(filename) {
  const value = parsedInput(filename, JSON.parse);
  if (value !== undefined && (!value || typeof value !== 'object' || Array.isArray(value)))
    throw invalidInput(filename, 'package.json must contain an object');
  return value;
}


const dependencyInputs = ['package.json', 'package-lock.json', 'npm-shrinkwrap.json', 'pnpm-lock.yaml',
  'pnpm-workspace.yaml', '.npmrc', '.pnpmfile.cjs', 'pnpmfile.cjs', '.pnpmfile.mjs', 'pnpmfile.mjs'];

function configuredHookPaths(config, directory) {
  const result = [];
  for (const [key, raw] of Object.entries(config ?? {})) {
    if (!['pnpmfile', 'globalpnpmfile'].includes(key.replaceAll('-', '').toLowerCase()) || raw === undefined || raw === null || raw === false) continue;
    for (const value of Array.isArray(raw) ? raw : [raw]) {
      if (typeof value !== 'string') throw invalidInput(directory, `Unsupported ${key} selector; use a declarative hook path`);
      const expanded = value.replace(/^~(?=\/)/, process.env.HOME ?? '~').replace(/\$\{([^}]+)\}/g, (_, name) => {
        if (!(name in process.env)) throw invalidInput(directory, `Missing environment variable ${name} in ${key} path`);
        return process.env[name];
      });
      result.push(path.resolve(directory, expanded));
    }
  }
  return result;
}

function included(relative, patterns) {
  return patterns.some(p => !p.startsWith('!') && path.matchesGlob(relative, p)) &&
    !patterns.some(p => p.startsWith('!') && path.matchesGlob(relative, p.slice(1)));
}
function workspace(current, pkg) {
  const filename = path.join(current, 'pnpm-workspace.yaml');
  const pnpm = !managerFor(pkg, path.join(current, 'package.json'))?.startsWith('npm@') && fs.existsSync(filename) ? parsedInput(filename, YAML.parse) ?? {} : undefined;
  if (pnpm !== undefined && (typeof pnpm !== 'object' || Array.isArray(pnpm)))
    throw invalidInput(filename, 'pnpm-workspace.yaml must contain an object');
  const patterns = pnpm ? pnpm.packages ?? ['**'] : Array.isArray(pkg?.workspaces) ? pkg.workspaces : pkg?.workspaces?.packages ?? [];
  if (!Array.isArray(patterns) || patterns.some(p => typeof p !== 'string' || path.isAbsolute(p) || p.split('/').includes('..')))
    throw invalidInput(filename, `Invalid workspace package patterns`);
  return {pnpm, patterns};
}
function managerFor(pkg, filename = 'package.json') {
  const manager = pkg?.devEngines?.packageManager;
  const declared = pkg?.packageManager;
  if (declared !== undefined && typeof declared !== 'string')
    throw invalidInput(filename, 'packageManager must be an exact supported version string');
  if (!manager) return declared;
  if (Array.isArray(manager) || typeof manager.name !== 'string' || typeof manager.version !== 'string')
    throw invalidInput(filename, 'Unsupported devEngines.packageManager selection; specify one exact supported npm/pnpm version');
  const pin = `${manager.name}@${manager.version}`;
  if (declared && declared.split('+')[0] !== pin) throw invalidInput(filename, `Conflicting packageManager and devEngines.packageManager selections: ${declared} versus ${pin}`);
  return declared ?? pin;
}
export function context(root, directory) {
  let nearest;
  const typescriptDeclarations = [];
  for (let current = directory;; current = path.dirname(current)) {
    const filename = path.join(current, 'package.json');
    const pkg = packageInput(filename);
    if ([pkg?.dependencies, pkg?.devDependencies, pkg?.optionalDependencies]
      .some(dependencies => dependencies && typeof dependencies === 'object' && Object.hasOwn(dependencies, 'typescript')))
      typescriptDeclarations.push(current);
    if (!nearest && pkg) nearest = {directory: current, pkg};
    if (nearest) {
      const config = workspace(current, pkg);
      if (current === nearest.directory || included(path.relative(current, nearest.directory), config.patterns)) {
        nearest.install = current;
        nearest.manager = managerFor(pkg, filename) ?? nearest.manager;
        nearest.rootPackage = pkg;
        nearest.workspace = config;
      }
    }
    if (current === root || path.dirname(current) === current || !current.startsWith(root + path.sep)) break;
  }
  if (nearest) nearest.typescriptDeclarations = typescriptDeclarations;
  return nearest;
}

export function nodePreparationPolicy(request) {
  if (!request.analysisPolicy) return undefined;
  const policy = request.analysisPolicy;
  if (policy.version !== 1 || !policy.node || policy.csharp || !path.isAbsolute(policy.node.cacheDirectory ?? ''))
    throw new Error('Update required: TypeScript worker requires Node analysis policy 1 with an absolute user cache');
  return policy.node;
}

function packageMembers(directory, group) {
  const members = new Map(group.packages);
  if (group.rootPackage) members.set(directory, group.rootPackage);
  for (const pattern of group.workspace.patterns.filter(p => !p.startsWith('!'))) {
    for (const filename of fs.globSync(`${pattern.replace(/\/$/, '')}/package.json`, {cwd: directory,
      exclude: ['**/node_modules/**', '**/.git/**', '**/.artifacts/**', '**/.research/**']})) {
      const relative = path.dirname(filename);
      if (included(relative, group.workspace.patterns)) {
        const selected = path.join(directory, filename), pkg = packageInput(selected);
        if (!pkg) throw invalidInput(selected, 'package.json disappeared during setup planning');
        members.set(path.join(directory, relative), pkg);
      }
    }
  }
  return members;
}

// Shared core owns checkpoint lifetime/publication. This adapter owns package semantics.
export async function preparePackages(root, directories, request, send) {
  const policy = nodePreparationPolicy(request);
  const groups = new Map(), contexts = new Map(), packageDirectories = new Map(), typescriptDeclarations = new Map(), failures = new Map(), metadata = new Map(), errors = new Map();
  for (const directory of new Set(directories)) {
    try {
      const item = context(root, directory);
      packageDirectories.set(directory, item?.directory ?? root);
      if (!item) continue;
      for (const alias of [directory, item.directory, item.install])
        typescriptDeclarations.set(alias, item.typescriptDeclarations.some(owner => alias === owner || alias.startsWith(owner + path.sep)));
      contexts.set(directory, item.install);
      contexts.set(item.directory, item.install);
      contexts.set(item.install, item.install);
      packageDirectories.set(item.directory, item.directory);
      packageDirectories.set(item.install, item.install);
      if (!groups.has(item.install)) groups.set(item.install, {...item, packages: new Map()});
      groups.get(item.install).packages.set(item.directory, item.pkg);
    } catch (error) {
      if (!(error instanceof ProjectSetupError)) throw error;
      failures.set(directory, error);
      packageDirectories.set(directory, directory);
      metadata.set(directory, {inputPaths: error.path && path.isAbsolute(error.path) ? [error.path] : []});
    }
  }
  for (const [directory, group] of groups) {
    const inputs = new Set();
    try {
      const members = packageMembers(directory, group);
      for (const member of members.keys()) for (const name of dependencyInputs) {
        const filename = path.join(member, name);
        inputs.add(filename);
      }
      for (const name of dependencyInputs) inputs.add(path.join(directory, name));
      const configHome = process.env.XDG_CONFIG_HOME ?? (process.env.HOME && path.join(process.env.HOME, '.config'));
      const userIniPaths = [process.env.npm_config_userconfig, process.env.NPM_CONFIG_USERCONFIG,
        process.env.HOME && path.join(process.env.HOME, '.npmrc'),
        configHome && path.join(configHome, 'pnpm/rc'), configHome && path.join(configHome, 'pnpm/auth.ini')]
        .filter(Boolean)
        .map(filename => path.resolve(directory, filename));
      for (const filename of userIniPaths) inputs.add(filename);
      const userYamlPath = configHome && path.resolve(directory, configHome, 'pnpm/config.yaml');
      if (userYamlPath) inputs.add(userYamlPath);
      const hookConfigs = [group.workspace.pnpm, {
        pnpmfile: process.env.pnpm_config_pnpmfile ?? process.env.PNPM_CONFIG_PNPMFILE ?? process.env.npm_config_pnpmfile ?? process.env.NPM_CONFIG_PNPMFILE,
        globalPnpmfile: process.env.pnpm_config_global_pnpmfile ?? process.env.PNPM_CONFIG_GLOBAL_PNPMFILE ?? process.env.npm_config_global_pnpmfile ?? process.env.NPM_CONFIG_GLOBAL_PNPMFILE,
      }];
      for (const filename of [...inputs]) if (fs.existsSync(filename)) {
        if (path.basename(filename) === '.npmrc' || userIniPaths.includes(filename))
          hookConfigs.push(parsedInput(filename, ini.parse));
        else if (filename === userYamlPath) hookConfigs.push(parsedInput(filename, YAML.parse));
      }
      for (const config of hookConfigs) for (const filename of configuredHookPaths(config, directory)) inputs.add(filename);
      const inputHash = createHash('sha256');
      for (const filename of [...inputs].sort()) {
        inputHash.update(filename).update('\0');
        hashSetupInput(filename, inputHash);
        inputHash.update('\0');
      }
      let selected, tool, error = '', outputs = [];
      const allowScripts = policy?.allowInstallScripts === true;
      try {
        const lock = ['pnpm-lock.yaml', 'yarn.lock', 'bun.lock', 'bun.lockb'].find(name => fs.existsSync(path.join(directory, name)));
        const fallback = group.workspace.pnpm || lock === 'pnpm-lock.yaml' ? 'pnpm' : 'npm';
        if (!group.manager && fallback === 'pnpm' && ['package-lock.json', 'npm-shrinkwrap.json'].some(n => fs.existsSync(path.join(directory, n))))
          throw new Error('Ambiguous npm/pnpm inputs; select one exact packageManager version in package.json. No substitution was made.');
        if (!group.manager && lock && lock !== 'pnpm-lock.yaml') throw new Error(`Automatic restoration for ${lock} is unsupported; no npm substitution was made`);
        if (!policy) {
          // Legacy requests retain installed npm and scripts-off until shared settings land.
          if (group.manager && !group.manager.startsWith('npm@') || fallback !== 'npm')
            throw new Error(`Automatic restoration for ${group.manager ?? lock ?? 'pnpm'} requires Node analysis policy 1; no npm substitution was made`);
          const actual = await runCommand('npm', ['--version'], root);
          selected = selection(group.manager ?? `npm@${actual}`);
          if (selected.version !== actual) throw new Error(`Repository selects ${selected.pin}, but npm ${actual} is on PATH; no substitution was made`);
          tool = {...selected, program: 'npm', prefix: [], source: 'PATH'};
        } else selected = selection(group.manager, fallback);
      } catch (cause) { error = cause.message; }
      const key = `node:${directory}`;
      const fingerprint = createHash('sha256').update(JSON.stringify({inputs: inputHash.digest('hex'),
        policy: policy ?? 'legacy-npm-scripts-disabled', manager: selected?.pin ?? group.manager, runtime: process.version,
        implementation: 'npm-pnpm-v2-safe-inputs'})).digest('hex');
      const previous = request.preparations.find(p => p.key === key);
      let missingOptional = false;
      function missingDependencies() {
        outputs = [];
        missingOptional = false;
        const missing = [];
        for (const [member, pkg] of members) {
          const require = createRequire(path.join(member, '__know_code__.cjs'));
          for (const name of Object.keys({...pkg.dependencies, ...pkg.devDependencies, ...pkg.optionalDependencies})) {
            const compiler = name === 'typescript' ? typescriptInstallation(member, root) : undefined;
            const found = name === 'typescript' ? compiler?.filename && compiler.entry :
              require.resolve.paths(name)?.map(p => path.join(p, name)).find(p => p.startsWith(root + path.sep) && fs.existsSync(p));
            if (found) outputs.push(found);
            else if (Object.hasOwn(pkg.optionalDependencies ?? {}, name)) missingOptional = true;
            else missing.push(`${path.relative(directory, member) || '.'}:${name}`);
          }
        }
        return missing;
      }
      const missing = missingDependencies().length > 0 || previous?.outputs.some(p => !fs.existsSync(p));
      const known = previous?.fingerprint === fingerprint && !request.retryPreparation;
      if (known && (!previous.succeeded || !missing)) error = previous.error;
      else {
        const needed = missing || !previous && missingOptional || previous && previous.fingerprint !== fingerprint || request.retryPreparation;
        if (!error && needed) {
          try {
            if (policy) tool = await packageTool(selected, policy.cacheDirectory, send);
            const pnpm = tool.name === 'pnpm';
            if (pnpm && ([...members.values()].some(pkg => pkg.devEngines?.runtime || pkg.engines?.runtime || pkg.pnpm?.executionEnv?.nodeVersion ||
                Object.values({...pkg.dependencies, ...pkg.devDependencies, ...pkg.optionalDependencies}).some(v => typeof v === 'string' && v.startsWith('runtime:'))) ||
                group.workspace.pnpm?.useNodeVersion || group.workspace.pnpm?.executionEnv?.nodeVersion ||
                [...inputs].some(f => f.endsWith('/.npmrc') && fs.existsSync(f) && /^\s*use-node-version\s*=\s*\S+/mi.test(readSetupInput(f)?.toString('utf8') ?? ''))))
              throw new Error('Repository requests managed Node runtime installation; install the required Node runtime separately. Know Code will not install Node.');
            const locked = (pnpm ? ['pnpm-lock.yaml'] : ['npm-shrinkwrap.json', 'package-lock.json']).some(n => fs.existsSync(path.join(directory, n)));
            const args = pnpm ? ['install', locked ? '--frozen-lockfile' : '--config.lockfile=false', '--config.production=false', '--reporter=append-only'] :
              [...(locked ? ['ci'] : ['install', '--package-lock=false']), '--no-audit', '--no-fund', '--no-update-notifier', '--logs-max=0', '--include=dev'];
            const safety = restorationSafety(tool, allowScripts);
            args.push(...safety.args);
            if (pnpm) {
              if (tool.major === 10) args.push('--config.manage-package-manager-versions=false');
              args.push('--config.runtime=false', '--config.updateNotifier=false');
              if (tool.major === 12) args.push('--no-runtime');
              if (tool.major === 12) args.push('--config.cargo.enabled=false', '--config.python.enabled=false');
            }
            send('progress', {stage: 'restoring', projectPath: directory,
              message: `Restoring with ${tool.name}@${tool.version} (${tool.source}); repository scripts/hooks ${allowScripts ? 'personally authorized' : 'disabled'}`});
            await runCommand(tool.program, [...tool.prefix, ...args], directory, send,
              {COREPACK_ENABLE_PROJECT_SPEC: '0', npm_config_update_notifier: 'false', ...safety.env});
            const absent = missingDependencies();
            if (absent.length) throw new Error(`Dependency restoration finished but direct dependencies remain unavailable: ${absent.join(', ')}`);
          } catch (cause) { error = cause.message; }
        }
        error = error.slice(-4096);
        send('preparation', {key, fingerprint, succeeded: !error, error, outputs});
      }
      metadata.set(directory, {packageManager: {name: selected?.name, version: selected?.version, origin: selected?.origin,
        source: tool?.source ?? 'selected-not-invoked', installRoot: path.relative(root, directory) || '.',
        scriptsAuthorization: allowScripts ? 'personal' : 'disabled', repositoryHooksForcedOff: !allowScripts}, inputPaths: [...inputs]});
      if (error) errors.set(directory, {code: 'dependency_restore_failed', severity: 'error', message: error, path: directory});
    } catch (error) {
      if (!(error instanceof ProjectSetupError)) throw error;
      failures.set(directory, error);
      if (error.path && path.isAbsolute(error.path)) inputs.add(error.path);
      metadata.set(directory, {inputPaths: [...inputs]});
    }
  }
  const installFor = directory => contexts.get(directory);
  const diagnostics = directory => {
    const error = errors.get(installFor(directory));
    return error ? [error] : [];
  };
  diagnostics.metadata = directory => metadata.get(directory) ?? metadata.get(installFor(directory));
  diagnostics.failure = directory => failures.get(directory) ?? failures.get(installFor(directory));
  diagnostics.directory = directory => packageDirectories.get(directory) ?? root;
  diagnostics.typescriptDeclared = directory => typescriptDeclarations.get(directory) ?? false;
  return diagnostics;
}
