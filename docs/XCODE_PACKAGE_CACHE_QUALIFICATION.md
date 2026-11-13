# Xcode package-cache qualification

Status: the finite **generic CLI metadata** gate passes. The actual wired
bridge, dependency-module preparation, real Atelier snapshot and production
adoption still need their own evidence. The bridge remains optional research.

The passing receipt is
`.artifacts/implementation-0.0.3/xcode-package-cache-10/result.json`, with command,
policy, lock, PIF and source hashes beside it. The retained driver is
`research/xcode-package-cache-07/` under that implementation artifact root.
It uses an owned local Git package and preserves the real HOME/CODEX_HOME,
authentication and global configuration. Process-local Git configuration
disables hooks and permits only file transport for this fixture.

## Preparation and analysis boundaries

Normal explicit dependency preparation uses SwiftPM's own manifest sandbox.
It creates the tool-written saved lock and central clone/cache outputs.
After preparation, metadata analysis runs under OS network denial, with the
original repositories, local source Git repository, global dependency fallback
caches and saved project/source/lock writes denied. The missing-cache phase
also denies the prepared seed. Independent TCP/UDP IPv4/IPv6, source, cache,
Foundation and symlink-alias controls qualify those restrictions.

The empty owned clone/cache directories fail metadata analysis. The restored
directories succeed, and the actual exported PIF identifies the package in the
central clones directory. Saved inputs remain byte-identical; scheme actions,
script phases and final object/product outputs are absent. These are finite
metadata results, not proof that CLI flags prohibit every fetch attempt or that
another service cannot delegate work outside the observed process boundary.

Normal SwiftPM **compiler manifest-cache** operations remain allowed. Global
repository/checkouts/artifact cache contents remain denied, including access
through aliases. This is dependency-source independence rather than complete
compiler-cache isolation.

## Findings to preserve

| Finding | Evidence and consequence |
| --- | --- |
| `-packageCachePath` does not isolate the compiler manifest cache. | Earlier restores still used `~/Library/Caches/org.swift.swiftpm/manifests/ManifestLoading`. Keep that operational cache allowance explicit. |
| `~/.swiftpm/cache` can be a symlink to that same canonical root. | A later blanket denial through the alias overrode the manifest exception. Deduplicate canonical roots before emitting policies, and test allowed manifest operations plus denied fallback access through an owned alias. |
| SwiftPM's manifest sandbox cannot initialize inside the outer test sandbox on this machine. | After the alias correction, restoration failed with `sandbox_apply: Operation not permitted`. Normal preparation precedes the strict post-restore analysis policy, matching the product contract. Do not disable SwiftPM's manifest sandbox or relabel setup as OS-network-denied. |
| Foundation controls can pass while a separate canonical-path rule still denies Xcode. | The synthetic mirror originally omitted the real alias. A matching API call alone did not qualify the whole policy. |
| The attempted private manifest-cache CLI option was rejected. | `-IDEDisablePackageManifestCaching` returned option error 64; it is not adopted. |
| Sandbox trace syntax matters. | Installed Apple profiles use `(debug deny)`. `(with report)` is invalid on a deny rule here. The bounded live trace exposed exact `file-read-metadata` denials; historical log lookup alone returned none. |

All failed attempts remain under `xcode-package-cache-*` and
`wave-01/xcode-package-cache-*`. In particular, trace08 preserves the alias
denials, run09 preserves the nested-manifest-sandbox failure, and run10 is the
passing stage-corrected result. A passing suffix does not make earlier attempts
successful.

## Actual bridge qualification

The separately frozen wired07 candidate accepts explicit central clone/cache
paths and strict saved-lock/no-update flags, retaining their effective paths in
the exact-PIF proof. It must pass the same missing/restored-cache controls on
both platforms through its real service path. Neither directory existence nor
the flags themselves certify readiness or offline execution. Preserve the
existing Test-action selection, script guards, typed outcomes and full-body
compiler graph checks when wiring that path.

The actual wired07 pair subsequently failed: the restored device case attempted
global Xcode DerivedData and re-evaluated manifests in a context different from
the generic preparation. Its expected-unavailable result alone does not qualify
the missing-cache case; the positive counterpart failed, and the batch stopped
before simulator. Those receipts remain inside the generic10 run directory.
Wired08 uses owned metadata DerivedData plus the same saved scheme and Test
configuration before export, with retained pre/post-selection hashes. Its
actual protected metadata and typed module checks now pass on both platforms:

| Check | Retained receipt beneath `.artifacts/implementation-0.0.3/` |
| --- | --- |
| Restored device and simulator metadata | `xcode-controlled-replay-metadata-{iphoneos,iphonesimulator}-02/result.json` |
| Missing-cache device and simulator boundaries | `xcode-controlled-negative-{iphoneos,iphonesimulator}-01/result.json` |
| Device and simulator typed module readiness | `xcode-controlled-module-{iphoneos,iphonesimulator}-02/result.json` |

The CLI/service hashes, unchanged analysis profiles, saved inputs, script
markers, selected target identities and real package module outputs are bound
to those receipts. Missing-cache checks establish an unavailable outcome with
fallback denied; they do not uniquely identify every diagnostic's cause.

The first historical-environment replay stopped before launching the bridge:
the current launcher hash differed. A fresh pair then used one current
environment for normal metadata preparation and protected actual analysis.
Each later module invocation likewise performs its own separately recorded
cheap metadata preparation before entering the unchanged analysis policy.
This matters because the upstream manifest cache includes environment inputs,
and session variables can change between tool calls. See
[the source investigation and controlled comparison](XCODE_MANIFEST_CACHE.md).
Normal preparation is not OS-network-denied; protected analysis is. No private
cache options, source changes or broader analysis permissions were needed.

These owned-fixture passes do not establish real-project completeness,
production integration, native queries, installed behavior or bridge adoption.
The protected Atelier snapshot and subsequent body-graph integration remain
separate gates.

## `.7` direct-launch environment regression and repair

The `.7` metadata rejection boundary observes the selected `xcodebuild`
executable directly, allowing a natural metadata-command exit to remain distinct
from a signal. Initial normal dependency preparation still launched
`/usr/bin/xcrun xcodebuild`. These two routes do not necessarily supply the same
process environment to SwiftPM's environment-sensitive manifest cache.

Both fresh hostile-plugin04 probes passed normal dependency resolve but failed
strict metadata with `sandbox_apply: Operation not permitted` while attempting
nested SwiftPM manifest evaluation. No plugin callback/prebuild/generated-source
marker appeared. This remains a failed analysis result; marker absence does not
turn metadata failure into a safety or readiness pass. Receipts remain at
`.artifacts/implementation-0.0.3/xcode-plugin-probe-{device,simulator}-04/`.

Packet05 changes **one orchestration line**: normal dependency resolve invokes
`DEVELOPER_DIR/usr/bin/xcodebuild` directly, matching metadata. It preserves the
inherited environment and existing analysis policy, lock handling, plugin
fixture, scanners and typed module checks. Each run uses a fresh owned prefix;
its path-dependent values naturally differ. No normal metadata/dumpPIF warm-up,
SwiftPM sandbox disable, cache purge or permission expansion was introduced.

Both SDK contexts pass the resulting finite hostile-plugin absence and typed
package-module readiness gates:

| Phase | Device05 | Simulator05 |
| --- | --- | --- |
| After normal dependency resolve | No execution/compilation evidence | No execution/compilation evidence |
| After strict actual BSP metadata/dumpPIF | No execution/compilation evidence | No execution/compilation evidence |
| After typed module preparation | No execution/compilation evidence; three proven empty auxiliary bookkeeping files retained separately | Same |

Exact receipts:
`.artifacts/implementation-0.0.3/xcode-plugin-probe-{device,simulator}-05/result.json`,
per-stage `plugin-scan-*.json`, and each `actual/result.json`. The unchanged `.7`
executable SHA-256 is
`433a75674d8941d4a3b0585f283d18b72833dfb859833ec325cda716692e9758`;
the same runtime manifest binds the04 and05 probes. The actual package-module
result is `passed_wired_cache_metadata_and_typed_package_module_readiness` in
both05 cases. This is a finite fixture result, not exhaustive plugin safety,
whole-workspace performance or release certification.

Production repair `9806ec8` uses the same selected direct executable for normal
restoration and metadata, preserving repository/user environment semantics. It
advances the existing preparation fingerprint to `xcode-preparation-2`, so old
setup evidence does not silently certify the changed launch contract. No tool
rebuild was required for this adapter/setup correction; the external `.7`
source and binary remained frozen.

The exact installed `xcodebuild` child-environment delta remains **unproven**.
Light `xcrun`→owned-environment-helper probes establish changes to `CPATH`,
`LIBRARY_PATH`, `MANPATH` and, for an ordinary helper tool, `SDKROOT`; only key
hashes and selected nonsecret tool paths were recorded. The installed
`xcrun(1)` documentation explicitly exempts build tools such as `xcodebuild`
from the ordinary-tool SDKROOT injection, so the helper's SDKROOT result cannot
be applied to `xcodebuild`. An attempted direct process-environment read was
unavailable; an owned exact-tool-name fixture was rejected as an invalid
developer directory. Those attempts are not actual-build-tool environment
witnesses. Retained evidence is under the tooling worktree's
`.artifacts/xcrun-buildtool-env-01/`.

The supported conclusion is the observed launch-contract mismatch and its
repair by using the same executable/environment route for both phases. Do not
claim a particular variable caused the installed manifest-cache miss or
reimplement undocumented `xcrun` environment mutations to force a cache hit.
