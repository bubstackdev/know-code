# Know Code

Know Code is an OSS code-intelligence engine in early development, indexing source code and supplying inspectable context to AI agents and other tools.

Working slices extract compiler-backed TS/C# symbols, references and explicitly partial call/dependency graphs, publish them atomically to SQLite, and query the same index in-process from .NET and Node.js. Shared Rust callers/callees/impact operators retain source evidence, implicit/possible/unresolved distinctions and query limits. Saved-file updates run through a persistent local coordinator while native queries remain in-process. Broader graph/query capabilities, operational safeguards and distribution remain incomplete for v0.0.1. See [release readiness](docs/RELEASE_PLAN.md), [implementation](docs/IMPLEMENTATION.md) and [graph queries and limits](docs/CALL_GRAPH.md).

Experimental [Swift/SwiftPM](docs/SWIFT_PROVIDER.md) and [Rust](docs/RUST_PROVIDER.md) adapters now feed that same core. Their documented coverage limits remain explicit; Xcode project support is the next Swift step. These begin the planned v0.0.3 language work without changing v0.0.1 scope.

```sh
bash scripts/build.sh
bash scripts/test.sh
target/debug/know-code trust /absolute/repository
target/debug/know-code watch /absolute/repository
# Configure your agent to launch:
target/debug/know-code mcp --workspace /absolute/repository
```

Grant project execution only for repositories you trust: compiler loading and MSBuild evaluation can execute repository-selected code. Registration or opening a workspace does not grant this permission. Package installation scripts remain disabled unless separately enabled in [personal settings](docs/WORKSPACE_SETTINGS.md).

Local runtime candidates have a [user-scoped installer, updates, rollback and uninstall](docs/INSTALLATION.md), separate from the native SDKs. Uninstall retains index data and registrations.

For optimized artifacts, run `bash scripts/build.sh release` and `bash scripts/test.sh release`. Debug and Release artifacts coexist; this build choice does not change a workspace's analyzed C# configuration.

[Shared background resource limits and indexing cancellation](docs/RESOURCE_CONTROL.md) use accepted two-core/2 GiB defaults, personal overrides, and coherent stale fallback on interruption.

Operation is local after dependency restoration. Public builds have zero telemetry; Debug/personal builds may collect local diagnostics for agent-assisted bug investigation.

The product must support an embeddable library, local CLI/MCP operation, and shared services through a common core. Multiple languages must fit that architecture without creating a sprawl of implementations or validations.

v0.0.1 targets macOS on Apple Silicon only, indexes TypeScript and C# with compiler-resolved symbols/references using TypeScript's compiler and Roslyn, and provides in-process libraries for both .NET and Node.js.

Callers, callees, and dependency-impact queries are required in v0.0.1.

Generated project source is included and explicitly labeled; locals and parameters are queryable too.

Agents use focused tools or bounded structured graph queries, backed by the same Rust query operators.

v0.0.1 MCP runs as an agent-launched stdio process. HTTP transport is deferred.

Missing project dependencies are restored automatically, with visible setup status.

Third-party packages contribute external symbol metadata by default; full dependency-source indexing is deferred.

Node.js and the .NET SDK must be installed separately. Know Code checks prerequisites and reports actionable errors; it does not install them.

Initial retrieval uses symbol/text search and graph queries. Embedding-based semantic search is deferred beyond v0.0.1.

The common core is Rust. Compiler analysis may use workers; storage and graph queries remain in-process.

SQLite is the initial durable fact/source store, with FTS5 lexical search and bounded graph traversal in Rust, subject to quality and performance validation.

v0.0.1 must allow concurrent local processes to share a repository index while retaining in-process queries.

A persistent local coordinator starts at user login and keeps indexes current after consumers disconnect, reusing the same Rust core and language workers.

Register repositories individually or register parent folders for discovery. New repositories beneath registered parents automatically enroll; analysis requires each workspace’s explicit project-execution grant and remains subject to exclusions and resource limits.

Indexes and durable coordinator state are stored centrally per user, outside source repositories.

Use one index database per repository/worktree plus a small central registry. Language providers and consumers share those indexes.

Initial queries cover current saved state. The data model must allow historical revision support later without implementing a history archive now.

Renames/moves may produce new symbol IDs in v0.0.1; refactoring continuity is required in v0.0.2.

Queries wait up to 500 ms by default for fresh analysis (configurable per query), then return coherent prior data explicitly flagged stale if needed.

v0.0.2 adds whole-estate cross-repository queries, Angular template-to-component references, and refactoring continuity, with hundreds of repositories indexed locally on an Apple M5 Max workstation as the primary use case.

v0.0.3 adds Swift and Rust source-language support through the same core and analysis contract.

## Documentation

- [CLI/MCP workflow](docs/MCP.md): explicit-workspace startup, tools, source excerpts, protocol support and limits.
- [Release plan](docs/RELEASE_PLAN.md): evidence-based 0.0.1 readiness, proposed completion order, and accepted 0.0.2 boundaries.
- [Design](docs/DESIGN.md): product requirements, scope, priorities, and constraints.
- [Architecture discussion](docs/ARCHITECTURE.md): candidate boundaries and unresolved decisions.
- [Saved-state coordinator](docs/COORDINATOR.md): background watching, scoped publication, freshness, restart behavior, and login setup.
- [Shared fact model](docs/FACT_MODEL.md): five accepted logical record families; detailed fields, identity, source coordinates, and physical schema remain under design.
- [Update pipeline](docs/UPDATE_PIPELINE.md): proposed incremental analysis, atomic publication, and reader lifetimes.
- [Worker protocol](docs/WORKER_PROTOCOL.md): accepted Protobuf/stdio transport, with shared messages, bounded batches, and lifecycle details under design.
- [Candidate packaging](docs/PACKAGING.md): separate shared runtime and native npm/NuGet SDK archives, validated outside the checkout.
- [Workspace APIs](docs/WORKSPACE_API.md): open a repository from .NET/Node, auto-start the installed coordinator, inspect progress, refresh/wait and query in-process with typed errors and client disposal.
- [Company onboarding backlog](docs/COMPANY_ONBOARDING.md): v0.0.2 GitHub/GitLab discovery and managed clone/update scope.
- [Runtime bindings](docs/RUNTIME_BINDINGS.md): accepted .NET C ABI/PInvoke and Node-API mechanisms; detailed ownership, async, and packaging design remains open.
- [Performance and evaluation](docs/PERFORMANCE.md): proposed budgets, quality gates, and measurement methods.
- [Indexing measurements](docs/INDEXING_PERFORMANCE.md): retained corpus results, implemented optimizations, and observed limits.
- [Research](docs/RESEARCH.md): primary sources, lessons, and evidence limitations.
- [Source review](docs/SOURCE_REVIEW.md): implementation findings and gotchas from pinned OSS reference clones.
- [Scaling](docs/SCALING.md): proposed upgrade path for hundreds of repositories using the initial SQLite design, subject to performance gates.
- [Diagnostics](docs/DIAGNOSTICS.md): zero-telemetry public builds and local Debug/personal investigation records.

Status: working development slices and corpus measurements; the full v0.0.1 scope remains incomplete. See the implementation and measurement documents above.

## License

Apache-2.0. See [LICENSE](LICENSE). Local research clones retain their upstream licenses.
