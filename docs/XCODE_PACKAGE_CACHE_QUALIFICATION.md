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
