# Separately installed Xcode bridge

This is the versioned source recipe for Know Code's adopted Xcode-only tool.
SwiftPM and in-process Rust/.NET/Node queries do not require this bundle. The
common runtime does not install it, change `xcode-select`, provision Xcode or
fetch/build dependencies during ordinary queries.

`recipe.json` pins upstream sourcekit-xcode-bsp, all eight Swift dependencies,
SwiftBuild, the dependency lock and the seven-file patch. `know-code.patch`
contains the reviewed wired08 behavior, per-file modification notices and the
`.7` bound metadata-command rejection boundary. `MODIFICATIONS.md` identifies those changes. The earlier tested
binary remains a reference, not a substitute for building this source.

The bridge's runtime layout is intentionally flat: `sourcekit-xcode-bsp`,
`SWBBuildServiceBundle`, the eight `SwiftBuild_*.bundle` resource directories,
licenses and the adjacent `know-code-xcode-bridge.json` provenance inventory.
A stable PATH symlink resolves to that real directory. Do not copy just the
executable or substitute resources from a different SwiftBuild revision.

## Prepare local source and dependencies

Use durable locations. The builder needs a local checkout at the exact
`upstream.revision` and a SwiftPM scratch directory whose eight dependency
checkouts already match `Package.resolved`. It never clones, fetches or
installs anything itself. Missing or changed pins fail with the required path.

For a new development setup, the explicit source preparation is:

```sh
git clone https://github.com/slime-studio/sourcekit-xcode-bsp.git .research/repos/sourcekit-xcode-bsp
git -C .research/repos/sourcekit-xcode-bsp checkout --detach 726ce706cf18ffb7ed402bb44b2e47c0960076eb
cp tooling/xcode-bridge/Package.resolved .research/repos/sourcekit-xcode-bsp/Package.resolved
```

Provision the locked SwiftPM dependencies into a durable scratch directory as
a separate setup step.
Use the committed `Package.resolved`, `--force-resolved-versions` and
`--skip-update`; do not regenerate pins from today's branch heads. Existing
qualification uses `.artifacts/xcode-bridge/build` for the scratch directory.
Any required network access belongs to that explicit setup, not the builder or
production analysis. The source checkout is read-only to the builder.

Preview the exact patched source without invoking Swift:

```sh
python3 tooling/xcode-bridge/build.py \
  --source-checkout .research/repos/sourcekit-xcode-bsp \
  --scratch-path .artifacts/xcode-bridge/build \
  --developer-dir /Applications/Xcode.app/Contents/Developer \
  --output .artifacts/xcode-tooling/prepare-01 \
  --prepare-only
```

Use a new output directory for every attempt. To build, omit `--prepare-only`
and use a new output name. The recipe runs a release build with two jobs,
retains logs, checks dependency/source pins again and stages `runtime/` with an
inventory. It leaves build source/cache and logs outside the installable tool.
It does not activate or install anything. Existing wider validation tools own
resource/process observations and relevant provider/native checks.

## User-scoped installation layout

After a separately reviewed successful build and relevant validation, copy the
entire `runtime/` directory to:

```text
~/.local/share/know-code/tools/xcode-bridge/index-only.7-notice1/
```

Place a symlink named `sourcekit-xcode-bsp` in `~/.local/bin` pointing directly
to that directory's executable, and put `~/.local/bin` on PATH. Use a symlink,
not a shell wrapper, so executable realpath identifies the co-located service,
resources and manifest. The actual install and symlink switch are separate user
actions; this document/build recipe does not perform them. Keep prior versions
until no running analysis needs them. This tool's removal does not remove Know
Code indexes, workspace registrations or the common runtime.

Know Code must require the exact CLI identity from `recipe.json` and the
matching adjacent provenance/runtime files. An arbitrary upstream executable
with the same basename is insufficient. Missing or incompatible tooling should
explain this separate prerequisite. No new checked-in workspace setting grants
execution trust or selects an arbitrary compiler.

## Qualification boundary

The wired08 research runtime used macOS 27.2/Xcode 27.2 on Apple Silicon. Its
185 files occupied about 90.44 MiB before this recipe's additional source/license
provenance files. Source preparation, signing/notarization, installation, older
macOS/Xcode compatibility and whole-workspace performance are distinct checks.
The deployment target `15.0` does not certify operation on macOS 15. The `.7` source has no new binary/platform receipt until built and tested. See [the adoption decision](../../docs/XCODE_BRIDGE_DECISION.md),
[Atelier qualification](../../docs/XCODE_ATELIER_QUALIFICATION.md) and
[package-cache qualification](../../docs/XCODE_PACKAGE_CACHE_QUALIFICATION.md).
