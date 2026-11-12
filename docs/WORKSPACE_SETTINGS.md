# Workspace analysis settings

The coordinator resolves analysis settings once for CLI, MCP and the in-process .NET/Node workspace APIs. These settings select analysis; graph queries still execute in the shared embedded Rust core. CPU/memory ceilings remain the separate personal, coordinator-wide resource policy in [RESOURCE_CONTROL.md](RESOURCE_CONTROL.md).

## Files and precedence

Optional checked-in `.know-code.json`, at the registered workspace root:

```json
{"csharp":{"configuration":"Release","solution":"Company.sln"}}
```

Personal settings live beside the central workspace index as `settings.json`; inspect their exact location through `know-code settings PATH`. Personal fields override checked-in fields independently. Relative solution paths resolve against the workspace root. Without a choice, the C# adapter evaluates repository/MSBuild defaults and discovers zero/one solution; multiple solutions require one selection. Outside projects remain indexed under their own evaluated defaults. A selected solution is not a project exclusion list.

For each C# field, omission inherits the next source; explicit `null` selects automatic/compiler-default behavior and overrides both checked-in and legacy choices. To inherit a checked-in value again, omit the field from the replacement personal settings. Nonempty strings select an explicit configuration or solution. Unknown fields and invalid values fail visibly rather than being ignored. Settings files are bounded at 64 KiB.

Older registrations have an ambiguous stored configuration value. It remains a lower-precedence `legacy_registration` fallback; Know Code does not pretend an old Debug value was an explicit user choice. New registrations default to automatic selection. New explicit requests are stored only as personal overrides, so clearing one cannot resurrect it as legacy state.

## Personal settings and package scripts

```sh
know-code settings /path/to/repository
know-code settings /path/to/repository '{"csharp":{"solution":"Company.sln"},"allow_install_scripts":true}'
know-code settings /path/to/repository '{}'
```

The mutation **replaces the whole personal object**, rather than patching it. Omitted fields inherit/default; `{}` clears personal overrides and disables scripts. `allow_install_scripts` defaults to false and is accepted only in the personal object; a checked-in file cannot grant it. The grant covers package lifecycle scripts and executable repository hooks/configuration, subject to the selected manager's own restrictions. It does not authorize Know Code to run a separate full build or external code-generation command.

Personal changes are validated and written atomically with file/directory synchronization. Coordinator mutations serialize trust changes and explicit configuration requests with workspace identity checks. A rejected replacement workspace cannot alter the retained workspace's settings. Script revocation cannot be overwritten by a stale concurrent configuration read/modify/write.

Explicit workspace removal (`unwatch`) revokes its install-script grant before removing the registration, while retaining other settings. A different repository registered later at that path cannot inherit the grant. Removing a consumer, stopping the coordinator or uninstalling the runtime does not remove the workspace registration and does not revoke its permission.

The shared package-tool cache is `package-tools` beneath the central per-user data directory. Node 24 and the .NET SDK remain separately installed prerequisites. The accepted automatic restoration families are stable npm 10–11 and pnpm 10–12; exact repository pins remain authoritative. Unsupported families fail actionably for restoration, without substituting another manager. See [first-wave adapter contract](FIRST_WAVE_CONTRACT.md) for acquisition, offline reuse and evidence obligations.

With `allow_install_scripts` absent or false, npm 10 cannot fetch Git dependencies or restore with effective `install-links=true`. Ordinary linked directories remain supported with `install-links=false`, including executable links. Select npm 11, restore affected dependencies separately, or grant scripts explicitly in personal settings; see [npm/pnpm preparation policy](TYPESCRIPT_PROVIDER.md#npmpnpm-preparation-policy).

## SDKs, freshness and compatibility

Node `Workspace.settings()` reads effective settings; `Workspace.configure(object)` replaces personal settings. .NET `Workspace.SettingsAsync()` reads them; `Workspace.ConfigureJsonAsync(json)` replaces them using the same JSON shape, preserving explicit nulls. Configuration arguments on open/refresh remain supported and become personal configuration overrides.

`WorkspaceStatus.requested_settings` reports the desired scope, source origins and file paths. Compilation coverage describes the generation actually served. Changing settings invalidates analysis and makes prior results explicitly stale until the new scope publishes; a new desired scope does not relabel prior facts. Invalid settings preserve the prior coherent generation with an error, or leave a first index not ready.

Both settings paths, including absent files, enter the saved-input snapshot before first analysis and are watched. Saves, restart reconciliation and explicit settings changes use the existing update path. Internal changes queue full analysis without depending on a file event and do not become explicit dependency-restore retries. Policy/tool/input fingerprints still invalidate preparation when needed; unchanged source saves must not repeatedly restore dependencies.

Control capability `analysis_settings_version = 2` is required for registration, refresh and settings operations. Clients check before mutation and verify the response, including when an existing handle encounters a replaced coordinator. An incompatible running coordinator returns an actionable `update_required`; there is no fallback to legacy settings. Worker policy acknowledgment remains separately required before accepting any facts/checkpoints. The development login job is not automatically restarted by these code changes.

Managed queries also require resource-control capability `2` in RPC responses and live publisher certificates, along with analysis-settings capability `2`, so an existing client cannot certify an older coordinator's different scope as fresh. Status and cancellation operations enforce resource-control capability `2`, including existing handles connected to a replaced coordinator. Runtime information reports both capabilities; the installer rejects a target lacking it while allowing an old compatible store/runtime to upgrade to a supporting target. A newly registered workspace without a published generation returns not-ready even if an index from a removed workspace remains at that disk path.

## Verification

Resolver tests exercise precedence, persistent explicit clearing, script-grant isolation and invalid replacement preservation. The existing workspace API harness adds .NET/Node/CLI settings parity, no-source-save invalidation, clearing a newly chosen configuration, and malformed first-run/personal-file recovery with coherent stale fallback. Adapter direct-worker suites cover actual package tools and mapped compiler contexts; those remain distinct from coordinator integration evidence. Full Debug validation belongs at the settled integration checkpoint.


## 0.0.3 language settings in development

Checked-in and personal objects also accept `rust` and `swift` settings:

```json
{"rust":{"targets":["aarch64-apple-darwin"],"features":["extra"],"default_features":false},"swift":{"scheme":"App.xcodeproj/xcshareddata/xcschemes/App.xcscheme"}}
```

Each field uses the same precedence: omission inherits; personal `null` resets to the compiler/repository default. Rust's default target list preserves Cargo's configured targets, or host selection when unset. Features add to the repository defaults; `default_features: false` requests their exclusion. Swift scheme paths resolve against the workspace root; absent selection requests the sole shared scheme and does not select arbitrarily among several. These settings do not mean the corresponding adapter or Xcode bridge has passed production acceptance.

Rust execution permission is derived from the existing personal, workspace-identity-bound project trust grant and checked again before worker execution. Neither checked-in nor personal language fields can forge it. Package-script permission remains separate. Compiler preparation outputs use a central worker directory. Each language policy participates in preparation fingerprints and requires its matching worker acknowledgment before publication. Control/settings capability 2 prevents an older coordinator from silently ignoring these new choices; worker policy remains version 1 with distinct typed branches. The existing development installation is not changed by this source work.
