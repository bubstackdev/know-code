# User-scoped runtime lifecycle

The accepted layout keeps one shared runtime separate from the npm/NuGet SDKs. This is a local candidate installer for macOS 15+ on Apple Silicon; signing, notarization and clean/older-OS verification remain release gates. Install Node 24 and the .NET 10 SDK separately and put them on PATH. Know Code checks them and never installs toolchains.

## Commands

Extract the runtime archive into a durable directory, then run its executable:

```sh
/path/to/extracted-runtime/bin/know-code install
# Add ~/.local/bin to your shell PATH if needed.
~/.local/bin/know-code install-info
~/.local/bin/know-code service status
~/.local/bin/know-code trust /absolute/repository
~/.local/bin/know-code watch /absolute/repository
~/.local/bin/know-code mcp --workspace /absolute/repository
```

Grant project execution only after reviewing the repository: compiler loading and MSBuild evaluation may execute repository-selected code. `watch`, MCP attachment and SDK opening do not grant it. `know-code trust /absolute/repository --revoke` revokes the grant. Package installation scripts stay disabled unless separately enabled in [personal settings](WORKSPACE_SETTINGS.md).

Install defaults to login startup. `install --no-login` creates an on-demand installation; `service enable` and `service disable` change that policy afterward. Disable stops the managed coordinator and removes its login plist; a later CLI/SDK attachment may start it on demand. Commands return JSON; failures go to stderr with a nonzero exit status.

```sh
know-code update --from /path/to/new/extracted-runtime
know-code rollback
know-code uninstall
```

All commands accept `--prefix DIR` (default `~/.local`). Initial install accepts `--label LABEL` for isolated installations. `HOME` determines the login-plist directory; `KNOW_CODE_DATA_DIR` selects central data for a new installation. Subsequent operations use the saved data location and PATH. Changing these environment variables does not relocate an existing installation. Reinstall with the desired PATH to a new prefix, or adjust the toolchain links it already contains; there is no runtime toolchain installer.

## Files and ownership

- `~/.local/share/know-code/versions/<version>-<content hash>/`: staged runtime trees.
- `~/.local/share/know-code/current`: atomically selected runtime.
- `~/.local/share/know-code/previous`: rollback selection.
- `~/.local/bin/know-code`: stable relative launcher through `current`.
- `~/.local/share/know-code/installation.json`: installer-owned paths, PATH and login policy.
- `~/Library/LaunchAgents/dev.bubstack.know-code.plist`: managed login job, executing the stable launcher.
- `~/Library/Application Support/Know Code/`: retained registry, settings and workspace indexes; outside the runtime tree.

Existing foreign launchers, changed login plists, occupied service labels and coordinators outside this installation are refused. In particular, the checkout-based development LaunchAgent in [LOCAL_INSTALL.md](LOCAL_INSTALL.md) is not adopted or replaced. Stop/remove that development installation explicitly before installing the default login job. An isolated prefix must also use a unique service label and separate data directory.

## Update and rollback

The installer validates executable-relative resources, copies regular files into a staged tree and verifies its content hash before changing selection. Version directories include content identity, allowing distinct local candidates with the same package version. Prefix operations are serialized. Switches use the target runtime’s explicit directional store/lifecycle catalog, plus matching worker, settings and resource contracts. The current catalog rebuilds managed formats 1–2 alongside the old index, migrates 3–7 additively and writes format 8. Unknown/newer formats and unsafe runtime rollback fail before stopping or switching. Actual registered workspace receipts and store headers are checked under maintenance. See [bounded upgrade policy](INDEX_UPGRADES.md).

A transient, owned maintenance lease pauses new analysis starts. Existing queries keep coherent data. Active analysis has up to 30 seconds to finish; if it stays busy, the lease is released, indexing resumes and `current` stays unchanged. Installer crashes cannot leave a permanent pause: leases expire within 30 seconds. The coordinator reports active work separately from visible workspace phases. Automatic CLI/SDK coordinator startup takes a shared lifecycle lock; switching holds it exclusively. Explicit coordinator-run commands must not be used concurrently to bypass that managed lifecycle.

After quiescence, the managed job is unloaded or the on-demand coordinator is asked to stop. The installer waits for the writer fence, changes `current` atomically and restarts according to the existing policy. Startup failure attempts to restore the old selection and prior running/login state only while the old runtime can still read actual durable workspace state. If a newer lifecycle receipt prevents safe restoration, retain the new selection and report that a compatible runtime is required; never reset an index selector to make rollback appear safe. The installer alone can quiesce the known lifecycle-1 peer using bounded status/maintenance/resume/shutdown messages before activating the current runtime. Ordinary clients require lifecycle 4 and receive an actionable update-required error from older peers. No forced analysis termination is used to meet the busy timeout. Identical-content updates preserve the rollback target. Rollback performs the same compatibility/quiescence path and exchanges current/previous selections.

Runtime versions are retained until uninstall; automatic version garbage collection is not implemented. A failed/busy update can leave a fully staged unselected version. Interrupted `.staging-*` directories require inspection and explicit removal. Runtime selection is atomic, but crash recovery of an interrupted whole install/service-policy operation is not yet automated. Inspect `install-info` and `service status`, then retry; do not infer a full crash-safe lifecycle guarantee from the atomic link.

## Uninstall and reinstall

Uninstall removes the managed login job/plist, launcher, runtime versions and installer settings. It **retains central index databases, workspace registrations and product settings**. Small empty installer directories and their operation-lock file remain to avoid replacing a lock inode during concurrent operations. There is no implicit purge-data option. Reinstalling against the retained data restores registrations through the existing coordinator startup reconciliation path; saved inputs are checked before freshness is claimed.

The login job captures PATH at initial install, sets the data directory and disables .NET CLI telemetry. Product builds still contain no usage collector. Required lifecycle lock files and install settings are control state, not telemetry. Runtime resources and SDKs retain the common pinned SQLite policy.

## Verification

`tests/installation.py` exercises an extracted candidate in a durable isolated prefix, including an actual uniquely labeled launchd job. Its artifacts record lifecycle outcomes; it must not target the live development prefix or label. Core/host tests cover startup exclusion and owned, expiring maintenance leases. See [PACKAGING.md](PACKAGING.md) for installed native SDK/CLI/MCP tests and current evidence.

Final integration evidence (2026-11-09): `.artifacts/installer/validation.json` records 15 real-launchd lifecycle checks, 18 exact-archive/native package checks with checkout access denied, the complete Debug suite, optimized Rust/live/Workspace/MCP checks, formatting and all-target Clippy. The corrected runtime archives are in `.artifacts/installer/candidate-retirement/`; source and archive hashes were rechecked against its manifest. Both login and on-demand failed-start recovery passed; uninstall/reinstall retained the index/registry and the development job stayed unchanged. The shared-core lease-retirement fix and deterministic pre-fix failure are documented in [LIVE_FRESHNESS.md](LIVE_FRESHNESS.md). This verifies a local unsigned candidate, not release readiness or an external publish.
