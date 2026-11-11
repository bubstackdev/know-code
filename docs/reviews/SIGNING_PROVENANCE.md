# Signing transformation and build provenance

Read-only design review, 2026-11-11. Inspected source: `1652712` plus documentation/suite-only commits `c7da268` and `470d91c`. This is a recommendation, not an implemented signing flow or release certification. No credentials, keychains, native artifacts or installed runtimes were accessed or changed; no build or integration tests ran. Only the installed `security cms` usage text and manual were inspected, alongside Apple documentation/source.

## Current behavior

`scripts/build_record.py::verify` compares the completed producer record to current source, configuration, dependencies, generated inputs, toolchains and outputs. `require_output` accepts only an exact recorded digest. The completed producer remains immutable.

`scripts/package.py` verifies that record before and after assembly under the existing exclusive producer fence. It stages copies of the CLI, native SDKs and experimental Swift helper. The Node archive and NuGet native/managed payloads are checked against their unsigned producer entries. The runtime includes the unchanged build record; `manifest.json` references its digest and records archive digests; `SHA256SUMS` covers those artifacts, record and manifest. The report explicitly says `unsigned_candidate: true`.

Consequently, signing the original producer outputs would invalidate verification, while signing staged Node/NuGet native files would fail the existing archive equality checks. Signing or stapling after the final inventory would invalidate that inventory. Neither behavior should be bypassed by replacing producer hashes or accepting arbitrary signed files.

## Smallest recommended extension

Keep the producer and its outputs unchanged. Add one packaging-stage `signing-transform.json`, produced only after staging copies and verifying their exact unsigned hashes. Reuse the existing package script/fence and manifest rather than adding another build system or provenance framework.

The receipt needs these fields:

| Field | Purpose |
| --- | --- |
| Schema, profile, build ID, source revision and exact producer-record SHA-256 | Bind the transformation to one completed build; the full producer digest also binds uncommitted content inventories. |
| Explicit staged path, corresponding producer output key, unsigned SHA-256 and signed SHA-256 for every native target | Permit only the intended byte changes; reject missing, extra and duplicate targets. |
| Signing identity certificate fingerprint, Team ID, code identifier, entitlement-content digest, signing options and tool version | Record the policy actually applied and independently verified, rather than trusting a requested identity string. |
| Verification result/evidence digest for each final native payload | Retain the exact post-sign verification evidence. |

Use a fixed target list: runtime CLI, Node add-on, NuGet FFI library, and the Swift helper if that experimental helper remains distributed. Sign staged copies at their final native names. The current NuGet `KnowCodeNativeLibrary` property can select the staged signed FFI file; it already accepts a packaging input path. JS, managed assemblies and dependency files still require their original producer/package-input hashes. A signed native file is not permission to mutate its neighboring payload.

Archive inspection must resolve an entry through either its unchanged producer digest or its single authorized transformation row. Validate that row's unsigned digest against the immutable producer, then require the extracted archived bytes to match the row's signed digest. Do not add a generic “signed binaries may differ” exception. Verify the runtime's staged native copies as well as both SDK archive payloads. Repeat producer and packaging-input checks at completion.

## Authenticate the source-to-artifact claim

An unsigned JSON receipt plus its SHA-256 establishes internal consistency only. An attacker able to replace both receipt and manifest can rewrite that claim. Developer ID validation of a native payload establishes its signing identity; it does not by itself authenticate a separate neighboring JSON statement about its source build.

The minimal complete chain is: authenticate the exact final release-manifest bytes with one detached release signature; that manifest hashes the unchanged producer, signing receipt, final archives and retained notarization evidence. Pin the expected verification key/certificate policy independently of the mutable files. Verify the signature before trusting the receipt, then follow the hashes. Signing exact serialized bytes avoids introducing JSON canonicalization machinery. Use the installed keychain identity without exporting its private key. A candidate built-in signing command is shown below; its success with the selected identity still needs verification. A digest-only local receipt must remain labeled unauthenticated.

```sh
/usr/bin/security cms -S -T -H SHA256 -u 6 -N 'SELECTED KEYCHAIN IDENTITY' \
  -i manifest.json -o manifest.cms
```

`-T` detaches the exact content, `-H SHA256` avoids the documented SHA-1 default, and `-u 6` selects object-signing usage. The installed usage text and [Apple's Security tool source](https://github.com/apple-oss-distributions/Security/blob/main/SecurityTool/macOS/security.c) document these options. This is a proposed command, not one executed by this review; it may require a normal keychain authorization prompt. Do not export keys or place passwords in command arguments.

Verification needs one expected signer, cryptographic verification against the exact manifest bytes, acceptable code-signing trust and a certificate/public-key fingerprint pinned outside the mutable release directory. Merely decoding CMS is insufficient: [Apple's decoder source](https://github.com/apple-oss-distributions/Security/blob/main/SecurityTool/macOS/cmsutil.c) calls signer verification without making its result a decode failure. Therefore do not accept `security cms -D` exit status alone. Prefer a small packaging-side verifier using Security's CMSDecoder verified signer status and certificate checks, or validate an equivalent strict existing verifier with changed-content/wrong-signer negative cases before adoption. This is tooling for the one release manifest, not a runtime API or a new provenance engine.

Keep this chain acyclic: the final manifest hashes the receipt and artifacts; the detached signature covers the manifest. Neither the receipt nor the manifest hashes itself. Existing checksum files remain useful for accidental corruption, alongside the authenticated manifest.

## Notarization and final bytes

Apple requires appropriate Developer ID signing and protections for notarized software; notarization is separate from source provenance. Retain the actual submission result and log, not a receipt flag claiming success. See Apple's [notarization requirements](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution).

The current runtime `.tar.gz` and npm/NuGet archives are not themselves a chosen Apple submission/stapling strategy. Apple's workflow uses supported submission containers and can require repackaging after stapling; select and validate that workflow for the actual CLI/native-library layout before implementing it. See Apple's [custom notarization workflow](https://developer.apple.com/documentation/security/customizing-the-notarization-workflow) and [distribution packaging guidance](https://developer.apple.com/documentation/xcode/packaging-mac-software-for-distribution).

Compute the release inventory only after every byte-changing stage. If stapling changes a distributed payload, retain both the signed-input and final-output digests as explicit stages and check the final archive against the latter. Submit-container hashes and final-distribution hashes can differ and must remain separately identified. A successful upload is not the normal-security installation gate. An online ticket lookup does not establish offline acceptance, and bare CLI files must not be assumed stapleable. Preserve quarantine/default-security behavior and test the selected supported container/ticket route with networking denied. Do not clear quarantine or disable Gatekeeper to make the gate pass.

## Finite checks before accepting this extension

The smallest installed-harness addition is an explicit signed-release mode in `tests/package_install.py`. Before SDK loading, inspect the native files extracted from the exact final runtime/npm/NuGet archives: require valid Developer ID signatures, the approved Team ID/code identifiers and expected signing policy on the CLI, add-on, FFI library and any shipped native helper; compare those files to the authenticated transform receipt. Preserve unsigned-candidate mode for historical local tests without counting it toward the signing gate. Then run the existing checkout-denied/offline TS/C# and native-loading checks unchanged, plus the separate normal-security container installation/ticket check. Native SDK loading must still work with the actual host's library-validation/hardened-runtime behavior after signing changes.

Preserve the existing unsigned provenance fault suite. Add focused failures for wrong producer/profile; changed unsigned native input; missing/duplicate/unexpected target; wrong identity/entitlements; changed signed bytes; changed unchanged payload; archive extraction mismatch; receipt/manifest mutation; and an unrecorded post-sign/staple mutation. Include a real final signed/notarized package and normal-security install proof. These are proposed checks, not results from this review.
