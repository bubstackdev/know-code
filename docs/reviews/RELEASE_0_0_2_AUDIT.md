# 0.0.2 release-risk audit

Read-only source/evidence review at `9063045`, after the user's instruction to
proceed toward publication. No build, test, native load, credential operation,
signing or publication ran in this audit. Version/changelog work has a separate
owner. The [review ledger](../CODEBASE_REVIEW_0_0_2.md) and
[validation policy](../VALIDATION.md) remain authoritative.

No new demonstrated source defect was found that requires reopening a review
item before tagging. All 72 findings have explicit dispositions. Residual
M8/L8 lock coupling, M17 integrity bound, M21/M26/L16/L20 cost measurements,
M19 serialization, bounded L10/L14/L15 behavior and L1/L2 local-platform hardening
are recorded limitations, not falsely closed defects. Do not invent a global
fork framework, mmap rewrite or performance certification to close them.

The full optimized `c646bcf` checkpoint and exact Debug/Release previous-native
matrices passed. Debug coverage is segmented with its original failures retained;
under the accepted new policy, that is not a reason to demand another unrelated
full Debug run. A final-version change still needs matching producer/artifact
proof and the applicable required/triggered checks; do not relabel pre-version
receipts as certification of bytes that have not been built.

## Work still required before publishing final assets

- Freeze the actual 0.0.2 source/version, build matching Release artifacts and
  verify their producer before/after the selected validation and packaging.
  Use the required public core and relevant completed extended receipts under
  the accepted policy; unchanged npm/pnpm source/toolchain matrices do not need
  duplication solely for Debug/Release profiles.
- Bind all three final archives to actual native bytes, pinned dependencies and
  complete license/notice inventory, zero-telemetry policy and authenticated
  manifests. Sign, notarize, staple and verify the new container/native hashes.
  Historical v0.0.1 acceptance does not certify new binaries.
- Run applicable exact-archive installed/offline/native/CLI/MCP and normal-security
  platform checks. DATA owner/ACL/socket and registry/update changes make a
  supported macOS15 install/update smoke relevant; no new experimental-language
  acceptance or encoding gate is introduced.
- Exercise the real 0.0.1-to-0.0.2 managed update using owned retained registry,
  settings/trust and indexes, new SDK/native pairs and cursor restart. Preserve
  advertised rollback checks/refusals, rather than promising rollback by schema
  marker inspection alone. Use the approved real-HOME isolated-prefix workflow.

The retained `.artifacts/pretag-wave/drivers/notarization_operator.py` and
`NOTARIZATION_OPERATOR.md` are a **0.0.1-specific** operator/allowlist. Prepare a
separate concrete 0.0.2 operator configuration/copy; never overwrite the old
receipt chain or reuse its accepted image/ticket/index for new artifacts.

## Concise user-facing caveats

Update the shared runtime and matching SDK packages, restart embedding/MCP
processes and restart paginated queries. SDK/native contracts8/9 and cursor
control2/3 are incompatible at their named boundaries; this does not mean every
ordinary mixed-version call fails. Query coordination is compact; fetch full
status separately. CLI query error envelopes now exit1. Selected npm/pnpm cache
entries from format1 need priming in format2 before offline use. Registry1 marks
the compatible registry schema while worker7/store8/lifecycle4 remain unchanged.
Existing owned DATA leaves become private and lose extended ACL entries.

User instructions are in [INSTALLATION.md](../INSTALLATION.md#upgrading-from-001-to-002).
The real-encoding gate remains deferred, and Swift/Rust remain experimental.
