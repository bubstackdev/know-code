# npm scripts-off boundary and accepted compatibility policy

The user accepted blocking npm 10 Git fetching and rejecting effective
`install-links=true` while scripts are disabled, preserving registry, tarball
and ordinary linked-directory restoration. The adapter implements those
boundaries; complete pin-matrix and installed-archive execution remain required.
The initial unsafe execution and source audit below are retained evidence.

The owned [setup-01 log](../../.artifacts/pretag-wave/diagnostic/setup-01/output.log)
shows `dependency-prepare-ran` created despite the outer `--ignore-scripts` flag.
The fixture is `.artifacts/preparation-safety/1a1cca02018`. Git/shell selector
traps remained off. This was an actual preparation-script execution, not an
inference from the marker's absence or a failed command.

The pinned upstream files are retained under
`.artifacts/worktrees/node-preparation/.artifacts/node-preparation/tools/`.
The [source packet](../../.artifacts/worktrees/release-setup-hardening/.artifacts/release-setup-hardening/primary-source-review.json)
binds their exact versions, paths, SHA-256 hashes and relevant line numbers.

| Selected manager | Pinned implementation evidence | Consequence |
| --- | --- | --- |
| npm 10.0.0 | Pacote 17.0.4 `lib/dir.js:48` calls `runScript` without an `ignoreScripts` guard or `scriptShell` argument. | Parent CLI/environment ignore flags cannot prevent this direct `prepare` call. |
| npm 10.9.10 | Pacote 19.0.3 `lib/dir.js:41` also lacks the ignore guard, but passes `this.opts.scriptShell`. | A non-executing shell can fail this route safely, but this does not establish safety for the older accepted pin. |
| npm 11.0.0 / 11.21.2 | Pacote 21.0.0 / 21.5.2 `lib/dir.js:35` checks `this.opts.ignoreScripts` before `runScript`. | The direct preparation path has the expected guard; inherited flags still protect installer children. Actual complete pin-matrix validation remains required. |

In npm 10.0.0, `@npmcli/run-script/lib/make-spawn-args.js:10` defaults the omitted
shell to `true`; `@npmcli/promise-spawn/lib/index.js:66–72` maps that to `sh` on
macOS. Thus forcing CLI/environment `script-shell=/usr/bin/false` does **not**
cover the oldest accepted pin. No shell-refusal prototype was pursued after
this source check. It would not justify declaring all npm 10 scripts-off safe.

The same unguarded directory fetcher has a local-directory route. npm 10.0.0's
config definitions at `@npmcli/config/lib/definitions/definitions.js:1062`
describe `install-links=true` as packing `file:` dependencies instead of linking
them. Arborist `lib/arborist/build-ideal-tree.js:1229–1232` selects that path and
`lib/arborist/reify.js:715` invokes `pacote.extract`. Therefore disabling only
Git acquisition does not establish safety for repository-enabled local-directory
packing. This second route is source-confirmed and **not separately executed**
by this review. A finite marker variant must cover it if that capability stays
supported under npm 10 with scripts disabled.

Independent fix `a01cf43` supplies scripts-off and pnpmfile-off flags through
both CLI and npm/pnpm lower/uppercase environment spellings. This is necessary
because Git fetching can spawn another installer without carrying the parent
CLI flags. It is not claimed to fix the unconditional npm 10 call. Registry,
proxy and authentication configuration files remain enabled and unchanged.

Cached pnpm 10.0.0/10.34.9 and 11.0.0/11.28.11 `preparePackage` implementations
return before executing preparation when `ignoreScripts` is true; their source
excerpts and hashes are in the packet. The pnpm 12 native implementation was
not source-certified here. Every named supported pin still needs the actual
owned marker gate. The fixture retains per-pin preparation results, marker
observations and matrix progress on failure; a failed/unsafe pin must not be
silently omitted from the matrix or relabeled successful.

## Accepted boundary implementation

The npm 10 invocation adds only an adapter-owned Node 24 `--import` preload.
Before npm/Pacote modules load, a bounded scan of the selected npm distribution
installs process-local throwing GitFetcher exports for its primary and nested
Pacote copies. No npm file or shared cache content is patched. A trusted
`git=/usr/bin/false` override remains defense in depth; it is insufficient alone
because hosted full-SHA Git specs can download archives without invoking Git.
Registry, remote/file tarball and directory-link fetchers are not replaced.

The adapter asks that same selected npm CLI for effective `install-links`, with
the original repository/user/environment precedence intact. Observed true is
rejected without installation or configuration rewriting. Only after observing
false does installation pin false in CLI/environment to close a saved-config
race. Reported package-manager options identify the checked/pinned policy.
Adapter fingerprint salt v3 invalidates older preparation checkpoints. An old
legacy PATH invocation that cannot establish the guarded CLI fails actionably;
current managed analysis uses the selected verified package-tool cache.

The finite fixture keeps the ten named default pins. npm 10 Git refusal is an
expected negative policy test; npm 11/pnpm Git restoration remains positive.
Each selected npm 10 pin also exercises hosted Git with a local denying proxy,
direct/factory guards including nested copies, effective packing refusals,
registry/tarball/link positives, and unchanged config bytes. Subset runs record
their exact selected pins and cannot claim full-catalog coverage. The final
installed archive must exercise npm 10 restoration through the relocated
preload; a checkout-only pass cannot establish that the guard was shipped.

## pnpm config-query evidence

The full setup-03 run reached pnpm 11.0 after its Git restoration and script
markers passed, then failed the fixture's `config get proxy` assertion. This
does not prove the adapter discarded proxy settings. In the exact cached
pnpm 11.0 `dist/pnpm.mjs`, `localConfig.js` accepts network INI keys including
`proxy`; `getConfig` copies them into its effective settings and derives
`httpsProxy`/`httpProxy`. However, `configGet.js::lookupConfig` exposes typed
keys only when explicitly marked or present in its auth-only INI view. The
network INI merge does not mark them, so that query can return `undefined`.

The fixture now sets pnpm 11+ `httpProxy` and `httpsProxy` explicitly in its
owned `pnpm-workspace.yaml`, and checks both actual CLI values. pnpm 11's
`addSettingsFromWorkspaceManifestToConfig` marks these YAML settings explicit.
Earlier managers retain the `.npmrc` proxy check. Both repository config files
are hashed before preparation and checked after preparation and config reads;
unchanged bytes alone are not treated as effective-config evidence. These are
fixture changes only; no user configuration or adapter policy is rewritten.
The ten-pin execution remains a separate root-owned gate, including pnpm 12's
native implementation, whose behavior is not certified by this source review.
