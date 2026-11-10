# Test classification and finding filters

Status: active implementation contract. Root owns the common facts/store/query/API/scheduler path; two narrow adapters extract evidence. This does not change the five logical fact families or add a test execution engine.

## Accepted product decisions

- Index all code by default. Query `tests` is `all` (default), `exclude` or `only`.
- Excluding tests retains unknown code. Tests-only returns findings with positive test evidence; it never converts unknown to production/test.
- Filter findings, not graph traversal. Impact may cross production/unknown helpers to reach tests. Preserve compilation/path compatibility, uncertainty, boundaries and whole-cursor work limits.
- A semantic symbol with both test and other declarations is mixed. Keep it in a filtered view when a declaration matches; preserve evidence instead of splitting semantic identity or removing underlying facts.
- Initial TypeScript evidence is inferred `.test`/`.spec` script filenames or a `__tests__` directory component, applied to the normalized root-relative owned source path. Unrecognized paths remain unknown. No execution/import of runner configuration, runner-global/symbol-name guesses or package-source indexing is added.
- Initial C# evidence is evaluated per-target project metadata. Generated/linked documents inherit their owning compilation context; identical text does not establish one global test label.

## Frozen provider contract

Worker/control framing remains protocol7. Add TestClassification(policy_version, kind, evidence[]) to Compilation and optional SourceArtifact origin. Source absence inherits compilation metadata; explicit unknown remains unknown. Kinds from providers are unknown/test/non_test; mixed is a core aggregate only. TestEvidence carries basis, value, path and inferred. Policy version is1. Evidence limits at the ingestion boundary: at most8 records, basis64UTF-8bytes, value512bytes, path4096bytes; adapters must keep invalid oversized property evidence explicitly abbreviated/unknown rather than fabricate a valid boolean. Existing content bytes/hash/artifact IDs are unchanged.

AnalyzeRequest.test_classification_policy requests exact acknowledgement through ProviderInfo.test_classification_policy before facts. Zero retains the legacy contract. This is independent of existing analysis/preparation policy1 so adding classifications does not unnecessarily restore unchanged dependencies. Root requests policy1 for TS/C#; experimental other languages remain unknown and do not expand this release scope. Effective classification/policy must join each provider's project snapshot witness, including referenced contexts.

TypeScript: compilation default unknown/policy1; matching sources test/policy1 with inferred path_convention rule/path evidence; no match unknown. All first-party sources still emit. Use case-sensitive conservative script extensions recognized by the compiler, including configured JS sources if already consumed; `.test.d.ts` does not match the filename rule by accident. A matching `__tests__` component still applies. Ensure synthetic module/initializer symbols have real owned declaration anchors when needed for classification, as C# synthetic nodes already do; identities, call extraction and reference resolution must not change.

C#: read final per-target SDK metadata AFTER existing identity metadata hooks/restoration, not outer/pre-restore planning. Keep classification separate from SdkTargetMetadata.Identity (that map becomes Roslyn globals and controls batching). Empty/missing/nonboolean signals are unknown. Valid IsTestProject=true => test. Valid IsTestProject=false => non_test unless IsTestingPlatformApplication=true conflicts, in which case unknown. With IsTestProject unknown, MTP=true => test; MTP=false/unknown alone => unknown. Retain available raw property/value/project/target-stage evidence; false MTP does not contradict a classic test project. No package-name or directory guesses for C#. Emit classification on Compilation, inherit it for ordinary/generated/linked docs, and expose test_classification report capability (complete/partial/unknown as appropriate to the supported source-context evidence). Saved flag/import/config/target changes must update snapshot witnesses.

## Core ownership and planned representation

Storage format7 will add classification metadata to compilations and compilation/artifact ownership (unit_artifacts), never contents/global shared text. A private derived integer mask on symbols records declaration-origin states once at report completion; unknown1/test2/non_test4 bits compose without losing mixed/unknown declarations. External symbols stay unknown; source reference sites use their own origin, never the callee's label. Indexed masks support pre-limit search filtering without parsing every source or silently applying a filter after the result cap. Preserve old format6 readers/snapshots during compatible migration; legacy missing evidence reads unknown.

Root alone owns migration, validation, logical retention accounting, masks/query plans, public metadata, paged immutable scope and native/SDK/MCP contracts. Legacy or older-policy current compilations require bounded background backfill through the existing coordinator analysis path; missing evidence must not claim confident completeness or force dependency restoration on every save. Unchanged failed work must not become a query/rescan retry loop. This is ordinary generation publication, not a new index backend or searchable history.

Symbols/graph findings classify semantic declaration origins; references classify each use origin. Source/coverage retrieval is explicit expansion, not a filtered finding search; reject non-default tests there instead of silently pretending a filter applied. Symbol context may be filtered while retaining matching mixed symbols and declaration evidence. Search filters run before the total limit. Filtered graph results preserve nonmatching intermediate paths and all uncertainty/work-limit evidence. Coverage must expose unknown/partial classification even when a filtered result is empty. Valid cursor continuation cannot change its original tests scope; separate pinned source/context reads retain their established contract.

## Parallel ownership and checks

- TS adapter: analysis.mjs, project-links.mjs, small classifier helper, worker acknowledgement, owned focused adapter tests/docs. No storage/query/scheduler/SDK edits.
- C# adapter: SdkMetadata.cs, ProjectCatalog.cs, Program.cs, ProjectLinks.cs, narrow classification helper if needed, owned metadata/direct-worker fixtures/docs. No common schema/query/SDK edits.
- Root: proto, common core, coordinator invalidation/backfill, shared API/bindings/MCP and final assembly. Only root changes the contract.
- Reviewer: read-only review of adapter evidence, core semantics and assembled boundaries. No duplicate broad builds.

Focused fixtures must preserve bytes/IDs/UTF-8 references, linked/generated context differences, all targets, inferred TS and fresh partial compiler graphs. Verify unknown-inclusive exclusion, tests-only positive/mixed behavior, references by use site, production-helper traversal, pre-limit filtering, retained generation/cursor replay, migration/old unknown and policy-only backfill. Use existing harnesses; one coordinated assembled checkpoint follows small independent checks. No whole-release/performance certification is inferred.
