// Process-local safety boundary for the selected npm 10 CLI only. Never loaded
// into Know Code's compiler worker or an embedding application through NODE_OPTIONS.
import fs from 'node:fs';
import path from 'node:path';
import {createRequire, Module} from 'node:module';
import {readSetupInput} from './package-tools.mjs';

function reject(message) {
  const error = new Error(`Know Code cannot safely start npm 10 restoration: ${message}`);
  error.code = 'EKNOWCODE_NPM10_LAYOUT';
  throw error;
}
function manifest(filename) {
  const bytes = readSetupInput(filename, 1024 * 1024);
  if (!bytes) reject('missing package metadata');
  return JSON.parse(bytes.toString('utf8'));
}
const cli = fs.realpathSync(process.argv[1]);
const npmRoot = path.dirname(path.dirname(cli));
const npm = manifest(path.join(npmRoot, 'package.json'));
if (npm.name !== 'npm' || !/^10\./.test(npm.version)) reject('selected CLI identity does not match npm 10');
const selectedRequire = createRequire(cli);
const primary = path.dirname(selectedRequire.resolve('pacote/package.json'));
const pending = [path.join(npmRoot, 'node_modules')], gitModules = [], scriptModules = [];
let packages = 0;
function packageDirectory(directory) {
  const stat = fs.lstatSync(directory);
  if (stat.isSymbolicLink() || !stat.isDirectory()) reject('symlinked or non-directory package tree');
  if (++packages > 4096) reject('package tree exceeds the bounded safety scan');
  const pkg = manifest(path.join(directory, 'package.json'));
  if (pkg.name === 'pacote') {
    if (pkg.type === 'module') reject('unsupported ESM Pacote layout');
    const files = ['git.js', 'fetcher.js', 'index.js'].map(name => selectedRequire.resolve(path.join(directory, 'lib', name)));
    for (const filename of files) {
      if (!filename.startsWith(npmRoot + path.sep) || requireCache(filename)) reject('Pacote escaped the selected tool or was loaded before its guard');
    }
    gitModules.push(files[0]);
  }
  if (pkg.name === '@npmcli/run-script') {
    if (pkg.type === 'module' || pkg.main !== 'lib/run-script.js') reject('unsupported npm script runner layout');
    const filename = selectedRequire.resolve(directory);
    const helper = selectedRequire.resolve(path.join(directory, 'lib', 'is-server-package.js'));
    if (filename !== path.join(directory, 'lib', 'run-script.js') || requireCache(filename) || requireCache(helper)) reject('npm script runner was loaded before its guard');
    scriptModules.push({filename, helper});
  }
  const nested = path.join(directory, 'node_modules');
  if (fs.lstatSync(nested, {throwIfNoEntry: false})) pending.push(nested);
}
function requireCache(filename) { return selectedRequire.cache[filename]; }
while (pending.length) {
  const directory = pending.pop(), stat = fs.lstatSync(directory);
  if (stat.isSymbolicLink() || !stat.isDirectory()) reject('symlinked node_modules tree');
  for (const entry of fs.readdirSync(directory, {withFileTypes: true})) {
    if (entry.name.startsWith('.')) continue; // Includes ordinary .bin links.
    const filename = path.join(directory, entry.name);
    if (entry.isSymbolicLink()) reject('symlinked package tree');
    if (!entry.isDirectory()) continue;
    if (entry.name.startsWith('@')) {
      for (const member of fs.readdirSync(filename, {withFileTypes: true})) {
        if (member.name.startsWith('.')) continue;
        if (member.isSymbolicLink()) reject('symlinked scoped package tree');
        if (member.isDirectory()) packageDirectory(path.join(filename, member.name));
      }
    } else packageDirectory(filename);
  }
}
if (!gitModules.includes(path.join(primary, 'lib', 'git.js'))) reject('primary Pacote was not covered by the tool-local scan');
if (!scriptModules.length) reject('npm script runner was not covered by the tool-local scan');
class DisabledGitFetcher {
  constructor() {
    const error = new Error('Know Code blocks npm 10 Git dependency fetching while package scripts are disabled. Select npm 11, restore the Git dependency separately, or explicitly grant package scripts for this workspace.');
    error.code = 'EKNOWCODE_NPM10_GIT_DISABLED';
    throw error;
  }
}
// Populate the cache BEFORE loading Git/Pacote/factory modules. Requiring the
// original class first would enter its cyclic imports and defeat this guard.
for (const filename of gitModules) {
  const guarded = new Module(filename);
  guarded.filename = filename;
  guarded.loaded = true;
  guarded.exports = DisabledGitFetcher;
  selectedRequire.cache[filename] = guarded;
}
// npm 10's Arborist runs linked-package prepare even with ignoreScripts=true;
// old Pacote also invokes prepare while packing directories. Their shared
// runner entry is the narrow boundary that covers both paths, including private
// methods that cannot be replaced on DirFetcher.prototype. This preload is
// selected ONLY for scripts-disabled restoration, before any consumer loads it.
for (const {filename, helper} of scriptModules) {
  const isServerPackage = selectedRequire(helper);
  if (typeof isServerPackage !== 'function') reject('unsupported npm server-package helper');
  async function DisabledPackageScripts() {
    return {args: [], code: 0, signal: null, stdout: '', stderr: ''};
  }
  DisabledPackageScripts.isServerPackage = isServerPackage;
  const guarded = new Module(filename);
  guarded.filename = filename;
  guarded.loaded = true;
  guarded.exports = DisabledPackageScripts;
  selectedRequire.cache[filename] = guarded;
}
