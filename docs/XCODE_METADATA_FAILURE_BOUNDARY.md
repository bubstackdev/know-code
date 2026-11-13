# Xcode metadata failure boundary

Read-only audit, 2026-11-13. The pinned `0.1.0-know-code-index-only.6`
bridge remains the qualified baseline for this audit. The narrow follow-up is
now implemented in frozen `.7` source (`8e37434`), independently reviewed and
awaiting root-owned build/runtime qualification. No `.7` runtime pass is claimed
here.

## Historical .6 failure boundary

The `.6` bridge has **no typed terminal metadata-failure receipt**. An ordinary
broken project can terminate initialization in the same observable way as a
bridge/process failure. The provider therefore preserves the prior coherent
publication rather than turning an unproven process exit into fresh unavailable
project data. This can prevent healthy sibling project results in the affected
update from publishing. It is a remaining reliability limitation, not proof
that the broken project has a usable partial compiler graph.

The [source recipe](../tooling/xcode-bridge/recipe.json) retains the `.6`
reference hashes and the current [seven-file patch](../tooling/xcode-bridge/know-code.patch)
implements the `.7` follow-up. The audited `.6` packaged-source copy remains
unchanged under
`.artifacts/xcode-tooling/build-01/source/Sources/`:

| Source boundary | What it proves |
| --- | --- |
| `SourceKitXcodeBSP/Xcode/PIFExport.swift:28–39` | Writes `know-code-project-export-request.json` **before** launching metadata. It proves the attempted argv/context, not completion or failure cause. |
| Same file, 40–71 | Writes `know-code-selected-scheme.json` only after process success, bounded PIF parsing, saved selection and coherence checks. Its absence does not identify the failure. |
| `SourceKitXcodeBSP/Bootstrap/BuildServerBootstrap.swift:62–101` | Metadata/selection occurs before BSP service handling starts. Bootstrap catches the error, closes the session/service and rethrows. No typed metadata RPC response is emitted. |
| `SourceKitXcodeBSP/Xcode/ProcessRunner.swift:12–22,112–114` | Has Swift-local launch/nonzero/resource/capture error cases. They are not serialized; nonzero exit retains status but not Foundation termination reason. |
| `SourceKitXcodeBSP/Bootstrap/KnowCodeBuildServer.swift:105–124` | Later typed preparation writes terminal result, completion count, operation state, exact PIF/configuration/target evidence and success before returning a preparation RPC error. This is a separate boundary. |

[The shared JSON-RPC transport](../providers/shared/jsonrpc.mjs) treats child
exit, pipe/framing failure and request expiry as failures. The current
[`openBridge` boundary](../providers/swift/xcode.mjs) correctly marks transport
failure fatal. A request sidecar, stderr text or exit status alone must not
reverse that decision. Known preflight configuration/tool/unsupported-manifest
errors remain independently supported project gaps. A matching later typed
preparation failure can support a scoped preparation gap while transport is
healthy; it cannot establish what happened during initial metadata export.

## Minimal follow-up outline

Frozen `.7` source reuses the current attempt/context sidecars and
`ProcessRunner`, adding one bounded terminal receipt under CLI
`0.1.0-know-code-index-only.7` and recipe `index-only.7-notice1`. No
common-core, worker Protobuf or query-engine redesign is required.

1. **Observe the actual metadata process.** Resolve the selected Xcode's real
   `xcodebuild` executable through a bounded prerequisite lookup, then launch it
   directly. Record Foundation termination reason, status and complete capture
   of both output EOFs. Only a completed natural nonzero exit enters a distinct
   typed `MetadataCommandRejected` path. Signal, cancellation, deadline,
   launch/capture/size failure, invalid PIF and changed inputs remain fatal.
   Observing only a proxy `xcrun` exit must not hide a child signal.
2. **Bind the attempt to this open.** The adapter supplies a fresh nonce plus
   its existing known saved-input hash witnesses: project/workspace, selected
   scheme, configuration/toolchain pins and known package/config inputs. The
   bridge checks those bytes before metadata and after a rejection. Bind the
   receipt to exact argv, selected context/platform, executable identity and
   the attempted-request sidecar hash. This is known-input coherence, not
   exhaustive discovery of every input consumed by Xcode.
3. **Finalize only a cleanly handled rejection.** Bootstrap catches only the
   typed command-rejection case, retains it in memory, successfully closes the
   session/service, then atomically writes `know-code-metadata-outcome.json`.
   Its small versioned payload includes nonce, phase, outcome, attempt/input
   hashes, termination reason/status, both EOF confirmations, bounded diagnostic
   output and a deliberate expected bridge exit status. Cleanup or receipt
   failure produces no project-gap proof. Existing success/PIF and preparation
   proofs keep their distinct meanings.
4. **Require receipt and transport agreement.** A thin adapter reader accepts
   only a current nonce/context/argv/input match plus the actual bridge's natural
   deliberate exit, without a signal. It rechecks saved inputs. Missing, stale,
   malformed or mismatched proof, cancellation/resource enforcement and
   unexpected transport death stay fatal. Only then emit a generic
   `xcode_metadata_unavailable` gap with no facts for that project.

Natural command rejection is **not root-cause classification**. Xcode may
internally translate a backend failure into a normal command exit. The proposed
receipt cannot prove that every such rejection was an ordinary compiler/config
error; do not parse stderr/status into that claim or call the graph complete.
Strict diagnosis of hidden backend crashes would require additional upstream
structured evidence, which `.6` does not expose.

Qualification would need real broken-project failures and healthy sibling
publication, both SDK contexts, plus crash/signal, interruption, timeout,
truncated capture, stale/wrong nonce, changed argv/input, failed cleanup and
receipt-replay negatives. A new build/provenance and relevant provider/native
checks are required; existing `.6` receipts do not certify this follow-up.

## Frozen .7 source contract

All five optional config fields must be absent for ordinary unbound BSP use,
or supplied together: `metadataJobNonce` (fresh 64 lowercase hexadecimal
characters), `metadataInputSha256`, `metadataMissingInputs`,
`metadataToolchainIdentity` (opaque provider identity hash) and
`metadataCompilerPath`. Project and saved scheme witnesses are mandatory for
a bound request. The map/missing-input collections are bounded; the encoded
binding is at most 1 MiB. Unbound successful initialization remains supported,
but an unbound failure cannot generate accepted rejection proof.

The attempt sidecar preserves logical `arguments` beginning with `xcodebuild`.
It additionally records the selected direct executable path/hash and per-open
binding. The terminal `know-code-metadata-outcome.json` is at most 2 MiB and
echoes those same flat binding fields, with `version: 1`,
`phase: "metadata_command"`, `outcome: "rejected"`, workspace/scheme/Test
configuration/platform, `attemptSha256`, direct executable identity and actual
arguments (the logical argv without its first element). It records natural
termination, nonzero command status, complete stdout/stderr EOF,
`bridgeCleanupSucceeded: true`, `expectedBridgeExit: 65` and bounded diagnostic
tails. Session-close failure replaces a pending command rejection; no terminal
receipt is finalized for that cleanup failure.

Independent read-only source review found no additional blocker at `8e37434`.
Patch/hash preparation and runtime/adversarial tests remain separate root-owned
gates. The `.7` source adds no worker/core/query engine or new dependency.


## Focused runtime checkpoint — 2026-11-13

The `.7` tool build passes with all 200 staged manifest entries verified.
Both SDKs pass typed module-success and true missing-input-failure checks.
Matching Debug producer `1d2fcd1df0d34b37bab70964c94f8b27`, built at `bea7b4f`,
passes the 14-check production Xcode fixture: healthy TypeScript sibling facts
publish while the Xcode project is malformed, and saved project repair restores
both SDK graphs automatically. The existing preparation path may report an
honest `dependency_restore_failed` gap after the bound metadata rejection;
no extra per-SDK retry machinery was introduced to force a diagnostic label.
Evidence: `wave-01/assembled-xcode-integration-05/` and
`.artifacts/xcode-integration/1794588603510642000/summary.json`.

The existing Xcode policy fixture passes one valid receipt and 23 fault cases:
nonce/replay, digest, consistently changed argv/input, saved input/executable
replacement, a newly appeared input, terminal/signal/close/stdout disagreement,
cleanup/EOF/status, malformed, oversized and missing proof. Shared JSON-RPC
framing, natural-exit, timeout and backpressure checks also pass. Evidence:
`wave-01/xcode-rejection-contract-01/` and
`.artifacts/xcode-provider/1794588727285522000/source-boundaries.json`.
These proof-reader fault cases do not claim every actual OS signal, backend
crash, interrupted cleanup or resource-limit path has been exercised end to end.

A real protected-cache regression from changing only metadata to direct launch
was found and fixed by using the same selected direct executable for normal
preparation. Both SDK plugin/cache checks now pass under the unchanged analysis
policy; see [the exact failures and repair](XCODE_PACKAGE_CACHE_QUALIFICATION.md).
Installed/optimized/platform and broader cleanup/cancellation qualification
remain separate. No release, live installation or encoding-performance pass is
claimed.
