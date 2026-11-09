# OSS source review for Know Code

Status: architecture reconnaissance; no implementation choice is accepted by this document.

## Scope and reproducibility

Six OSS repositories were shallow-cloned and retained in `.research/repos/`. Their exact commits and origins are recorded in [.research/repositories.json](../.research/repositories.json). Source links below point to those revisions.

Three agents initially inspected source through a web reader. The primary agent then checked selected local clone paths and reconciled important differences against the pinned revisions, including C# symbol normalization. Cached web source observations were not treated as a coherent repository snapshot.

This is a targeted source crawl, not an exhaustive audit. Reference code was not installed, built, benchmarked, or executed. Observed source behavior is distinguished below from recommendations; inferred risks have not been reproduced as bugs. No upstream code was copied into Know Code.

## TypeScript compiler extraction

Sources: [ProjectIndexer.ts](https://github.com/sourcegraph/scip-typescript/blob/cfce6003ca74e6fa7e5ba471184eb67fa8514d30/src/ProjectIndexer.ts), [FileIndexer.ts](https://github.com/sourcegraph/scip-typescript/blob/cfce6003ca74e6fa7e5ba471184eb67fa8514d30/src/FileIndexer.ts), [main.ts](https://github.com/sourcegraph/scip-typescript/blob/cfce6003ca74e6fa7e5ba471184eb67fa8514d30/src/main.ts), [Packages.ts](https://github.com/sourcegraph/scip-typescript/blob/cfce6003ca74e6fa7e5ba471184eb67fa8514d30/src/Packages.ts).

Observed:

- `ProjectIndexer` creates a TypeScript compiler host, `Program`, and `TypeChecker`, then delegates source-file extraction.
- Batch caches include source files keyed by pathname and language version, shared indexed-file sets, parsed configuration, and source mapping information.
- The orchestration recursively visits `projectReferences`; indexed files are deduplicated by pathname.
- `FileIndexer` emits definition/reference occurrences and implementation relationships, resolves aliases, and handles tokens with multiple semantic identities. Call-expression checks exist for special extraction cases; that does not mean emitted ordinary references are classified as a general CALLS graph.
- Package identities derive from package metadata, with `HEAD` or anonymous fallbacks. The optional inferred-tsconfig path writes a configuration file.

Implications for Know Code:

- Use compiler-backed resolution for semantic navigation, without inheriting batch CLI orchestration as the library API.
- Track compilation identity as well as physical file identity: one file can participate in projects with different options and references.
- A long-lived cache needs content/configuration/dependency invalidation. Batch cache reuse is not a persistent incremental algorithm.
- Preserve multiple declarations/occurrences; keep display names separate from canonical symbols.
- Derive invocation edges from syntax context plus resolved target and enclosing callable. Do not convert every reference into a call.
- Configuration inference and filesystem mutations must be explicit host policy, not hidden library behavior.

## C# / Roslyn extraction

Sources: [ScipProjectIndexer.cs](https://github.com/sourcegraph/scip-dotnet/blob/ba95b8bf3c82c3b49820eb55faf26dca6b07c75e/ScipDotnet/ScipProjectIndexer.cs), [ScipCSharpSyntaxWalker.cs](https://github.com/sourcegraph/scip-dotnet/blob/ba95b8bf3c82c3b49820eb55faf26dca6b07c75e/ScipDotnet/ScipCSharpSyntaxWalker.cs), [ScipDocumentIndexer.cs](https://github.com/sourcegraph/scip-dotnet/blob/ba95b8bf3c82c3b49820eb55faf26dca6b07c75e/ScipDotnet/ScipDocumentIndexer.cs), [Program.cs](https://github.com/sourcegraph/scip-dotnet/blob/ba95b8bf3c82c3b49820eb55faf26dca6b07c75e/ScipDotnet/Program.cs).

Observed:

- Project loading uses `MSBuildWorkspace`; restore runs unless skipped. The program registers MSBuild and creates the workspace in its application entry point.
- Projects are grouped by project-file path and one target-framework variant is selected, preferring the running framework. Extraction iterates `project.Documents` and obtains semantic models.
- The inspected C# walker emits declarations and identifier references using `GetDeclaredSymbol` / `GetSymbolInfo(...).Symbol`. It does not explicitly override generic-name or invocation/operation extraction, and does not retain candidate bindings at that callsite.
- The pinned document indexer DOES normalize reduced extension methods and original definitions in specific identity/documentation paths. An earlier cached-source observation that normalization was absent is not applicable to this revision.
- Overload disambiguation still uses same-name member ordering. Interface implementation relationships use Roslyn's `FindImplementationForInterfaceMember`.
- Ranges use mapped line spans, while project documents assign their physical relative path. The project loop does not explicitly enumerate generated documents through Roslyn's generated-document API.

Implications for Know Code:

- Treat project/target framework/compiler options and semantic inputs as part of analysis identity. Do not silently merge different compilations or hide selection of one target.
- Keep symbol identity distinct from its declarations: partials and overloads need deliberate handling. Define required identity stability before adopting upstream encoding.
- Explicitly cover generic references, constructors, extension methods, operators, conversions, and candidate/unresolved bindings. A Roslyn operation pass is a candidate for invocation extraction, not yet a selected design.
- Preserve physical and mapped source identities/ranges where they differ. Subsequent product decision: generated project source is included in v0.0.1; the inspected ordinary-document loop alone is not a sufficient coverage contract.
- Keep restore, toolchain selection, logging, workspace lifetime, and cancellation in explicit host/setup policy. Subsequent product decision: Know Code automatically restores missing project dependencies through coordinator-managed jobs; library construction must still not perform unconditional global initialization or package restoration. Toolchain installation remains separate.

## Storage, publication, and incrementality

Sources: codebase-memory [pipeline.c](https://github.com/DeusData/codebase-memory-mcp/blob/612eee2518ef84cc573e491fdee5fa5c90264cb5/src/pipeline/pipeline.c), [pipeline_incremental.c](https://github.com/DeusData/codebase-memory-mcp/blob/612eee2518ef84cc573e491fdee5fa5c90264cb5/src/pipeline/pipeline_incremental.c), [store.h](https://github.com/DeusData/codebase-memory-mcp/blob/612eee2518ef84cc573e491fdee5fa5c90264cb5/src/store/store.h), [watcher.c](https://github.com/DeusData/codebase-memory-mcp/blob/612eee2518ef84cc573e491fdee5fa5c90264cb5/src/watcher/watcher.c).

Observed:

- The pipeline stages new database generations and publishes them through replacement, with metadata/semantic surfaces participating in publication. Prepublication abort paths preserve the prior serving database.
- A process-global pipeline lock exists; the inspected frozen-build path acquires it without a database/workspace key and restores ambient language configuration afterward. This observation does not establish that every ordinary indexing path takes the lock.
- A store handle explicitly cannot be used concurrently. Read-scope APIs expose scoped query state and cancellation.
- Incremental code records semantic manifests and definition surfaces, repairs reverse consumers, distinguishes skipped files from deleted files, and captures inbound edges that replacement would otherwise drop.
- One closure-repair path relies on extraction being file-content-local, allowing a depth-one repair under its stated assumptions; uncertain cases decline to a full rebuild.
- The watcher uses adaptive polling, with a nominal 5-second base and 60-second interval cap. Successful generation acknowledgement controls advancement of observed baselines. This is not instantaneous filesystem-event freshness.

Implications for Know Code:

- Prefer instance-owned workspace state, explicit readers/writers, and coherent publication. Avoid process-global coordination across independent library instances.
- Changes arriving during indexing must remain pending after the generation commits; acknowledge only the inputs actually indexed.
- Record fact ownership and semantic inputs. Deleting and recreating a target must not silently erase unaffected inbound relationships, but blindly restoring old relationships can also preserve stale edges.
- Do not assume depth-one invalidation works with TypeScript inference or Roslyn project semantics. Dependency changes can alter the meaning of unchanged source transitively.
- Explicit updates, watching, and restart reconciliation should converge on one update path. Watchers are host facilities, not a second indexing engine.

## Core versus transport and language lifecycle

Sources: CodeGraphContext [server.py](https://github.com/CodeGraphContext/CodeGraphContext/blob/6ed71a9f03f8eefec5bf7c38d638425efdf92ffe/src/codegraphcontext/server.py), [graph_builder.py](https://github.com/CodeGraphContext/CodeGraphContext/blob/6ed71a9f03f8eefec5bf7c38d638425efdf92ffe/src/codegraphcontext/tools/graph_builder.py); Serena [mcp.py](https://github.com/oraios/serena/blob/ede329aaf0c824bea75455a2926f01420d7aa620/src/serena/mcp.py), [ls_manager.py](https://github.com/oraios/serena/blob/ede329aaf0c824bea75455a2926f01420d7aa620/src/serena/ls_manager.py), [C# adapter](https://github.com/oraios/serena/blob/ede329aaf0c824bea75455a2926f01420d7aa620/src/solidlsp/language_servers/csharp_language_server.py).

Observed:

- CodeGraphContext's server constructs database, indexing, job, and watcher objects; it can alter process environment and event-loop state. Graph construction reads CLI configuration and creates schema during construction.
- Its optional SCIP branch detects one language, indexes through that path, and returns on success. That is not a sufficient scheduling model for Know Code's mixed TS+C# workspace.
- Serena delegates through shared language-server abstractions and manages startup/cleanup, but its MCP integration contains process-global logging/Uvicorn patches.
- Serena's external-change reconciliation traverses tracked files and compares mtimes before symbolic operations. An observed greater-than mtime comparison motivates testing preserved/backdated timestamp cases; this crawl did not reproduce a defect.
- Its C# adapter explicitly reloads solution/project state when files are created; an ordinary file notification is documented as insufficient for that backend. Timeout paths can warn and proceed.
- Serena's inspected upper MCP file declares GPL-3.0-or-later; its inspected SolidLSP C# adapter declares MIT. Component licenses differ.

Implications for Know Code:

- Library construction must not install tools, rewrite configuration, start a server/watcher, mutate global logging, or own the host event loop implicitly.
- Discover and schedule language projects independently, then publish through common workspace operations. Avoid one workspace-wide language switch.
- Separate process startup from semantic readiness; report partial, failed, unsupported, and complete outcomes explicitly.
- Extract shared lifecycle/update behavior, retaining real compiler/project differences inside narrow providers. Do not hide genuine differences behind dozens of boolean flags.
- Learn from architecture without copying implementation across incompatible or unreviewed licensing boundaries.

## Fact model and identity from Glean

Sources: [code.angle](https://github.com/facebookincubator/Glean/blob/ce5a6ef435b3400239f963270c83c69535385bb1/glean/schema/source/code.angle), [Glass SymbolId.hs](https://github.com/facebookincubator/Glean/blob/ce5a6ef435b3400239f963270c83c69535385bb1/glean/glass/Glean/Glass/SymbolId.hs), [Database Ownership.hs](https://github.com/facebookincubator/Glean/blob/ce5a6ef435b3400239f963270c83c69535385bb1/glean/db/Glean/Database/Ownership.hs).

Observed:

- The code schema provides common entity/language surfaces while retaining language-specific alternatives and schemas.
- Glass defines language-specific symbol encoding behind a common API. Public symbol IDs include repository/language information; storage fact IDs and navigational identities are not the same abstraction.
- Ownership introspection traverses stacked database ownership expressions, including AND/OR combinations of owning units.

Implications for Know Code:

- A small common model can coexist with language-specific semantic detail. Do not flatten away information needed for correct identity or relationships.
- Keep database row identity, source occurrence identity, and public symbol identity distinct in the design even if some implementations share representations.
- Ownership and incremental derivation deserve a deliberate model. Do not import Glean's complete schema-generation and distributed infrastructure merely to obtain that principle.

## Lean architecture requirements suggested by the crawl

These are recommendations, not accepted implementation choices:

1. Instance-owned workspace engine: open, analyze/update, query, status, dispose.
2. A language-provider boundary exercised by TS and C# immediately, including mixed workspaces and configuration changes.
3. One shared fact/update/query implementation across .NET, Node.js, CLI/MCP, and server hosts.
4. A coherent input snapshot per generation, including compiler/project inputs, not just source timestamps.
5. Typed evidence/results with unresolved reasons and completeness; transports adapt one contract.
6. Shared scoped cancellation and explicit resource lifetime rather than repeated transport-specific checks.
7. A small invariant suite: incremental/full equivalence, coherent reads, failed-update preservation, independent workspace isolation, and equivalent bindings.
8. No generalized plugin marketplace, multi-backend graph abstraction, or separate validation framework before concrete requirements justify them.

## Runtime decision after the review

Confirmed: in-process libraries for BOTH .NET and Node.js, and TS+C# indexing in v0.0.1. The TypeScript compiler and Roslyn belong to different runtimes; their source demonstrates analysis approaches, not an automatic single-runtime solution.

Accepted after discussion: a Rust common core keeps SQLite-backed storage, queries, and bounded graph traversal in-process; compiler-backed analysis may use workers through one shared analysis contract. Workers do not duplicate storage or query logic. SQLite is the initial durable store subject to performance gates; binding mechanics and storage layout remain open. See [architecture](ARCHITECTURE.md).
