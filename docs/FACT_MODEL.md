# Shared fact model

Status: five shared logical fact families and internal UTF-8 byte-range source coordinates are accepted. The [first implementation](IMPLEMENTATION.md) has an initial SQL schema and Protobuf contract; those are provisional development formats. The broader fields and policies below remain design directions unless explicitly accepted; graph relationship extraction is not implemented yet.

## Intent

One compact model must serve TS/C# in v0.0.1, Angular in v0.0.2, and Swift/Rust in v0.0.3. Providers resolve language semantics; Rust stores evidence and implements shared search/traversal/impact. Separate semantic identity, source occurrences, and relationships.

The records below describe responsibilities, not separate packages, validators, database files, or mandatory one-to-one SQL tables. Prefer typed records and indexed relations over a universal entity/property framework.

## Context

| Context | Meaning | Essential information |
| --- | --- | --- |
| Workspace | Registered checkout/worktree and its index. | Opaque identity, repository/worktree origin, current location. |
| Compilation | Project-specific semantics. | Provider/language, project identity, target/options/toolchain fingerprints, dependencies. |
| Generation | Coherent published view of saved inputs. | Workspace, selected input/analysis-unit versions, freshness and coverage summary. |

One source file can belong to multiple compilations. Share its source bytes while keeping binding context distinct. A C# target framework or TS configuration must not disappear because the pathname matches.

Accepted for v0.0.1: analyze every declared C# target framework by default. Preserve each target's compilation context, conditional source/bindings, generated documents, and diagnostics. Do not treat a single target's bindings as evidence for every target. Unavailable/failed targets remain explicit coverage gaps rather than disappearing from the expected scope.

Share identical source content where practical and bound compiler-worker residency/concurrency across targets. All-target coverage does not require all compilations to remain resident simultaneously.

Accepted configuration scope: one selected C# build context per workspace, applied across all target frameworks. Default to repository/MSBuild evaluation unless explicitly overridden, preserving solution-to-project mappings; project configuration labels may differ within that one context. Preserve the selected context and effective project configuration in compilation identity/input fingerprints and expose them in coverage. Code active only in other configurations is outside the active analysis scope. Changing the workspace setting invalidates affected compilations through the existing update path; prior-configuration facts must not be silently reported as current under the new setting. Existing coherent readers retain their original scope and may receive stale results under the accepted freshness policy.

Default selection follows the repository/MSBuild rather than a Know Code-wide Debug setting. Exact root/solution selection and project evaluation mechanics, including ambiguous multiple-solution workspaces, and runtime-identifier handling remain open. This is not a request to enumerate all combinations of build properties or create a second configuration-specific index engine.

Accepted TypeScript scope: standalone files without a tsconfig.json use an inferred compiler configuration. Represent this using the same compilation context, with explicit inferred origin/effective options and coverage limits. Use compiler binding evidence normally; assumed options do not establish the actual intended build semantics. Preserve configuration ownership/input fingerprints so an explicit configuration appearing later triggers affected reanalysis without mixing prior inferred bindings into the new context. Exact inference defaults/grouping remain open; no second fact model or repository config writes are needed.

Accepted query presentation: combine equivalent findings across targets, identify their supporting target frameworks, preserve target-specific differences, and provide explicit target filters. Grouping is a result presentation operation, not permission to merge incompatible semantic identities or discard underlying compilation ownership. A combined finding's provenance describes the evidence actually included, not all expected targets; missing-target coverage remains visible.

Traversal must preserve compatible compilation contexts along a path. An edge present only for one target and another present only for an incompatible target must not produce a claimed end-to-end path. Share this behavior across focused and structured query operators. Exact grouping keys, compact provenance representation, and pagination ordering remain implementation decisions.

Accepted test scope: include test projects/code in the default index, with shared query filters to include or exclude them. Keep classification evidence tied to the relevant project/source context; a source file can participate in more than one project. Provider/framework detection rules and user override configuration remain design work. Unknown classification must not silently become a confident production/test label. Query filtering does not remove underlying indexed facts. Static relationships can identify candidate affected tests but do not prove execution coverage.

A generation is not a Git commit and does not create a historical-query archive. It can cover saved uncommitted inputs and compiler-generated documents. Current queries pin their served view; a future revision selector should use this context model rather than a second fact engine.

## Five fact families

Accepted across all supported languages: the following five logical families, keeping semantic symbols separate from declaration/use occurrences. Their roles are shared; the suggested fields below are not yet an accepted physical schema. This does not require five SQL tables, separate packages, per-language fact engines, or a generalized property framework.

| Family | Meaning | Proposed information |
| --- | --- | --- |
| Source artifact | A version of readable source. | Identity, content fingerprint/content reference, encoding, physical/generated origin, optional mappings. |
| Symbol | Semantic entity independent of a source occurrence. | Opaque/provider identity, kind, name, containing scope, signature/docs, provenance. |
| Occurrence | Where an entity is declared or used. | Artifact/span, compilation owner, role, enclosing symbol, zero or more bindings/resolution states. |
| Relationship | Supported connection between entities. | Typed endpoints/kind, evidence/callsite, analysis-unit ownership, resolution/derivation category. |
| Analysis report | What analysis established or could not establish. | Input/unit identity, status, diagnostics, unresolved/unsupported sites, coverage boundaries. |

Reports can use compact status/diagnostic rows rather than monolithic serialized objects. Share repeated source/provenance/configuration references instead of copying them into every edge.

## Symbol versus occurrence

- A C# partial type has an appropriate symbol identity per modeled semantic context and multiple declaration occurrences. A symbol is not one file/range.
- Overloaded display names are not identifiers. Preserve declaration/signature distinctions and compiler binding evidence.
- A token can participate in multiple bindings; do not force every span to exactly one target. This matters for declaration merging, shorthand/destructuring, and multi-role tokens.
- Locals/parameters have scopes and identities; same-named locals in different scopes must not merge. Local functions/lambdas participate in call graphs.
- External symbols can be resolved without a local source body. Their origin must not pretend to be a source location.

Public symbol identity should be opaque, versioned, and scoped, based on provider semantic identity. Do not use compiler pointers, process-local counters alone, display names alone, or SQLite row IDs as durable public identities. Compact database-local IDs remain useful internally.

**Accepted:** renames/moves may create new symbol identities in v0.0.1. Refactoring continuity is required in v0.0.2 as a fast follow; provider key design remains open. Deterministic reanalysis is distinct from continuity tracking across refactors.

Proposed continuity extension: record explicit predecessor/successor identity links with workspace/generation context, change kind, and supporting evidence. Keep semantic identity independent from the matching decision. Ambiguous matches remain ambiguous; do not silently merge entities because names or bodies look similar. This is identity tracking, not a new code-editing/refactoring engine.

Leave an additive representation/API path for continuity links and preserve the scoped identity/input evidence needed by future matching. Do not require name-independent IDs or build a retained historical graph archive to enable the fast follow. Detailed matching strategy, coverage for locals/generated symbols, and API behavior for old IDs remain design work.

## Relationships and ownership

The initial vocabulary should represent containment, imports/dependencies, references where needed for graph navigation, invocations, inheritance, implementations/overrides, and possible dispatch. Exact names/encoding remain open.

Accepted impact defaults include compiler-backed calls, value references (including field/constant reads), type references, inheritance and imports, with labeled/filterable kinds. Their common relationship representation must preserve the evidence appropriate to each kind; a dependency is not automatically an invocation, and a lexical match is not a compiler-backed dependency.

Occurrences are authoritative for source bindings. Relationship records express connections required by graph operators. Derived adjacency/search indexes must not become a second independent truth with different semantics.

A call preserves enclosing callable, invocation occurrence, compiler-bound declaration/target where available, and possible dispatch candidates. A variable/type reference is not automatically a call. Every impact path preserves evidence, possible edges, and external/unresolved boundaries.

Unresolved invocations still have callsite/report evidence. Zero known targets does not prove no calls or impact. Include possible dispatch by default; exact-target-only filters inferred expansion without implying runtime certainty.

For default impact explanations, prefer a compiler-bound supporting path over a shorter possible-dispatch path, within shared query budgets. Keep uncertainty and compilation provenance intact. Limited search must not be presented as proof that no stronger alternative exists; returning one path does not imply uniqueness.

Accepted call-query default: include compiler-implied property accessor, overloaded operator and conversion calls, explicitly labeled as implicit and filterable. This does not imply full recovery of every lowered runtime operation. See [implementation plan](CALL_GRAPH.md) for the still-unimplemented evidence/query path.

Accepted initializer representation: explicit, labeled execution nodes for file loading/type initialization, separate from later field/function-value use. Their final identity/physical encoding remains an implementation decision.

Facts belong to explicit analysis units, such as compilation/document versions. Replacing a target definition must not indiscriminately erase unrelated caller evidence. Changed bindings must still be invalidated/reanalyzed; never blindly resurrect obsolete incoming relationships.

## Source artifacts and spans

Accepted canonical internal coordinates: UTF-8 byte ranges over the exact indexed source version. Providers convert compiler-native positions at ingestion; API boundaries convert line/column representations with an explicit encoding. .NET/Node/Rust coordinates are not assumed interchangeable. Tie ranges to their artifact/version and retained source content so stale and generated-source evidence remains reproducible.

The first slice uses half-open `[start, end)` spans with SHA-256 content identity for retained logical UTF-8 text. These initial storage/wire fields are not a stable public format; public line/column conventions remain open. Preserve line endings and define broader non-UTF-8 physical file, Unicode/surrogate, generated, and mapped-source behavior. Physical offsets in an original differently encoded file may differ from canonical UTF-8 offsets; decoding to canonical text must not rewrite the source file.

Keep physical/generated/mapped identities and ranges distinct. Compiler-only source needs retrievable content for the served generation. A stale result must not fetch current disk text at old offsets and present it as matching evidence.

Accepted initial source storage: retain source text in the workspace SQLite database and share identical content across compilation targets and generations. Sharing content does not merge source-artifact identities, paths, generated provenance/mappings, or compilation ownership. Source references must be ready at atomic fact publication; reclamation follows current/pinned/recovery lifetimes. External source blob storage is deferred until a measured need, not a second backend to implement now.

Exact content-key/hash strategy, SQL representation, compression, and retrieval chunking remain open; no general object-store framework or estate-wide deduplication service is required. Preserve exact canonical text rather than normalizing away line endings or Unicode distinctions. Retain current/pinned evidence, reclaim obsolete data, and avoid duplicating source/ASTs on each save or retaining a source-history archive.

## Three independent result properties

| Property | Question answered |
| --- | --- |
| Freshness | Does this publication satisfy the request's observed saved-input target? What newer work is pending? |
| Completeness | Which units/features/regions are partial, unresolved, external, unavailable, or truncated? |
| Resolution evidence | How was this particular binding/relationship established? Is dispatch only possible? |

A fresh generation may have compiler errors and partial coverage. A complete prior generation may be stale. A resolved virtual declaration does not prove the concrete runtime implementation. A single numeric confidence cannot replace these distinctions.

Reports preserve missing prerequisites/outputs, restore failures, source diagnostics, and provider failures distinctly. Source diagnostics allow valid partial publication; crashed/cancelled analysis or failed storage publication preserves prior usable state.

Accepted graph projection: return compact symbols/signatures, relationships, and source locations by default; source bodies are optional bounded inclusion. This changes response shape, not indexed semantic detail or evidence retention. Included source must match the served generation/artifact fingerprint, including stale results and generated documents. Omission, unavailable source, and budget truncation remain explicit. Required freshness/coverage/resolution/target provenance must survive compact projection; exact schema, budget units, and numeric defaults remain open.

## Future composition and languages

Workspace/compilation identity is not package identity. Preserve observed assembly/module/package/version provenance rather than guessing one from another. v0.0.2 can link compatible external identities to registered source without duplicating each graph centrally or assuming current branches match installed packages.

Preserve small typed provider-specific details where needed. Promote fields required by shared query semantics into the common model. Avoid an unbounded property bag or language-specific semantic engines in Rust.

Swift/Rust schema pressure tests include extensions/traits/implementations, associated items, generics, closures, and macro/generated origins. These are future design checks, not implemented provider claims.

## Implementation guidance and focused checks

- Use compact internal IDs, indexed forward/reverse adjacency, and shared source/provenance references. Keep public IDs separate.
- Batch provider ingestion with one decode/validation boundary and common operation invariants; do not duplicate validators in each transport.
- Focused and structured queries use the same facts, operators, evidence, and coverage policy.
- Do not persist full compiler ASTs, unbounded inferred types, or all third-party source to make the model appear universal.
- Test partial declarations/overloads, multi-role bindings/shadowing, multiple compilations for one artifact, Unicode/mappings, fresh-partial/stale-complete results, and incremental/full equivalence using one meaningful fixture suite.

Exact schema, ID normalization, source storage, SQLite plans, batch protocol, and extension fields remain design work subject to performance gates.

## Source basis

Sources were reviewed in the durable clones at commits recorded in [.research/repositories.json](../.research/repositories.json). Architectural references, not copied implementation:

- [SCIP schema](https://github.com/sourcegraph/scip-dotnet/blob/ba95b8bf3c82c3b49820eb55faf26dca6b07c75e/scip.proto): documents, symbols, occurrences, relationships, and explicit position encodings.
- [TS symbol encoding](https://github.com/sourcegraph/scip-typescript/blob/cfce6003ca74e6fa7e5ba471184eb67fa8514d30/src/ScipSymbol.ts), [C# symbol encoding](https://github.com/sourcegraph/scip-dotnet/blob/ba95b8bf3c82c3b49820eb55faf26dca6b07c75e/ScipDotnet/ScipSymbol.cs): package/local identities; per-document counters alone are not globally durable identities.
- [Glean source schema](https://github.com/facebookincubator/Glean/blob/ce5a6ef435b3400239f963270c83c69535385bb1/glean/schema/source/src.angle): file content/digest, byte spans, coordinate conventions.
- [Glean C# schema](https://github.com/facebookincubator/Glean/blob/ce5a6ef435b3400239f963270c83c69535385bb1/glean/schema/source/csharp.angle): definitions, locals/parameters, and invocation locations.

See [source review](SOURCE_REVIEW.md) for extraction and incremental gotchas. This draft adds no performance or provider-completeness guarantees.

The compact context slice records compiler declaration extents as `definition` occurrences alongside existing `declaration` name anchors. This is an implemented additive role, not a sixth fact family or a guarantee that every provider has bodies. See [source-version ties, bounds and unavailable cases](SYMBOL_CONTEXT.md).
