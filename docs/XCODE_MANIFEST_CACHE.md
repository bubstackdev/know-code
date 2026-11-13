# Xcode manifest cache: environment alignment investigation

Read-only source investigation, 2026-11-12. Wired08 is still a research
candidate; no production bridge/dependency adoption or installed-Xcode cache
behavior is certified here. Root owns every native/compiler/cache experiment.

## Upstream evidence

The retained compiler identity is Apple Swift 6.4.2
(`swiftlang-6.4.2.1.9`), from the existing `swift-relations-01/result.json`.
Official SwiftPM `swift-6.4.2-RELEASE`
[ManifestLoader.swift](https://github.com/swiftlang/swift-package-manager/blob/swift-6.4.2-RELEASE/Sources/PackageLoading/ManifestLoader.swift#L445)
constructs its key with `Environment.current.cachable` (445–453). Its
[checksum implementation](https://github.com/swiftlang/swift-package-manager/blob/swift-6.4.2-RELEASE/Sources/PackageLoading/ManifestLoader.swift#L973)
includes package identity/location, saved manifest bytes, tools version, every
remaining environment key/value in sorted order, SwiftPM version and extra
manifest flags (973–994). A database hit returns parsed data; a miss evaluates
the manifest (485–555). Evaluation applies the normal sandbox when enabled
(805–834). This establishes upstream behavior, not a byte-for-byte mapping of
Apple's installed SwiftPM.framework to that tag.

Official [Environment.swift](https://github.com/swiftlang/swift-package-manager/blob/swift-6.4.2-RELEASE/Sources/Basics/Environment/Environment.swift#L269)
filters only keys in the non-cacheable set (269–280), using a cached copy of the
current process environment (122–131). The
[explicit exclusions](https://github.com/swiftlang/swift-package-manager/blob/swift-6.4.2-RELEASE/Sources/Basics/Environment/EnvironmentKey.swift#L34)
cover terminal/session/color/SSH-agent variables and union the registered
[non-cacheable configuration variables](https://github.com/swiftlang/swift-package-manager/blob/swift-6.4.2-RELEASE/Sources/Basics/Environment/ConfigurableEnvVar.swift#L39)
for SBOM and registry/source credentials. They do **not** exclude `TMPDIR`,
`GIT_CONFIG_*`, `GIT_ALLOW_PROTOCOL`, `GIT_TERMINAL_PROMPT`,
`SWBBUILDSERVICE_PATH`, `UseSynchronousBuildDescriptionSerialization`, `PWD`
or `OLDPWD`. Latest main has changed PWD exclusions; do not substitute main's
rules for the inspected release tag or assume Apple's fork matches either.

Apple's [Process.environment documentation](https://developer.apple.com/documentation/foundation/process/environment)
says an unset environment is inherited from the creating process. Therefore
the bridge's current launch sequence supplies a concrete environment-difference
hypothesis. Actual xcrun/installed-framework behavior still requires the root's
matching replay; these sources do not establish which difference caused the
retained failure.

## What frozen wired08 does

The source packet is retained at
`.artifacts/implementation-0.0.3/research/xcode-wired-08/` with its manifest.
These references are to that candidate's `source/Sources/` directory:

| Source | Observed behavior |
| --- | --- |
| `sourcekit-xcode-bsp/SourcekitXcodeBspCommand.swift:39` | Calls the service factory before entering bootstrap. |
| `SourceKitXcodeBSP/Service/BuildServiceProviding.swift:69` | Configures process environment before launching the service. |
| Same file, 102–120 and 135–140 | Sets synchronous-description serialization and the chosen service executable in process globals. |
| `SourceKitXcodeBSP/Environment/EnvironmentRepository.swift:26` | Implements assignments with `setenv`. |
| `SourceKitXcodeBSP/Bootstrap/BuildServerBootstrap.swift:51` | Creates the service session before calling PIF export at61. |
| `SourceKitXcodeBSP/Xcode/ProcessRunner.swift:30` | Creates Foundation Process, sets executable/args/stdin/stdout/stderr, but sets no explicit environment or cwd before launch. |
| Packet `package-metadata.mjs:37` | Copies the driver environment and removes SDKROOT; starts the bridge in its owned config directory. |

The separately retained exact preparation helper sets file-only Git overrides,
disables prompts/hooks and uses its own output's `t` directory as TMPDIR
(`xcode-exact-preparation-01/prepare.py:80–89`). Its command has the selected
scheme/Test configuration/owned DerivedData/cache paths, but matching argv does
not establish matching environment. Cwd and the inherited PWD value are also
separate: changing a subprocess cwd need not update a supplied environment's
PWD string.

Upstream cache semantics make all these differing values relevant candidates.
They are not proof that only the two service keys explain the actual miss.
Service-path variables may also affect service selection, so treat their
functional effects separately from their participation in an upstream hash.

## Narrow root-owned isolation plan

Keep wired08, its failed runs, source/dependency pins and the OS analysis profile
unchanged. Root has retained exact resolve success followed by strict
restored-device failure; global DerivedData errors are gone, while nested
manifest failure remains. Do not call that a passing preparation-plus-analysis
run. No private preference, cache purge or broader fallback access is justified.

1. Replay the **actual recorded metadata argv**, including its exact PIF and
   metadata DerivedData paths, outside the bridge but under the same OS profile.
   Record cwd, HOME/CODEX_HOME preservation, explicit DEVELOPER_DIR and the known
   Git/TMPDIR/service-variable differences. Do not silently change output
   context while comparing stage results.
2. Compare otherwise-equal child environments rather than only equal commands.
   Record sorted keys plus per-key value fingerprints and the small known
   nonsecret operational overrides; arbitrary environment values need not be
   printed in console/log messages. Include inherited variables from the root
   resource wrapper, not just the manually listed suspects. The authoritative
   observation point is immediately before launching xcrun.
3. Hold the preparation-stage Git controls and TMPDIR/cwd/PWD values identical
   for a controlled setup/analysis pair. Then test the two service-factory
   assignments as an isolated group. Preserve typed metadata/service evidence
   if those variables alter routing as well as cache behavior. If the standalone
   exact replay fails too, avoid blaming bridge semantics alone.
4. If a clean cache-key witness is needed without deleting any global entries,
   root may use one owned diagnostic environment key with distinct experiment
   values on this reviewed constant manifest. Warm one value through normal
   trusted preparation, then attempt that same value and an unwarmed value
   under unchanged denial. Reverse/warm the second value and retain all stages.
   This tests environment sensitivity in the installed toolchain; it is not a
   product setting, an authorization bypass or a semantic guarantee for
   manifests that actually read that key.
5. Repeat the matching strict positive and empty-cache negative only after the
   cause is identified. Module readiness remains a separate typed public
   prepare gate; body graphs remain separate compiler/IndexStore evidence.

If environment differences are confirmed, the narrow implementation candidate
is an explicit, coherent metadata child environment shared with existing common
preparation, captured before service-specific global assignments. Preserve
repository environment semantics and real HOME/CODEX_HOME. Service settings
should reach their intended service without silently changing metadata's
manifest inputs. This is a proposal for the parent to assess after isolation,
not an implemented cache-key clone, a new setup engine or permission to discard
arbitrary environment variables for cache hits. Private cache contents and
format are not an API to reimplement.
