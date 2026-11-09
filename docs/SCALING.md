# Scaling and upgrade discussion

Status: local-workstation deployment accepted for v0.0.2 and SQLite accepted as the initial fact/text-index store subject to performance gates; partitioning and detailed resource policy remain proposals.

## Workload and constraints

- Hundreds of repositories, C# service architecture, Angular frontend.
- One Rust core shared by embedded .NET/Node.js bindings, CLI/MCP, and server hosts.
- Storage and queries stay in-process for embedded use. Compiler analysis may use workers.
- Avoid separate local/enterprise engines and a speculative multi-backend framework.

Accepted release scope: whole-estate cross-repository queries target v0.0.2, not v0.0.1. Keep repository/package/compilation identity, generations, and artifacts composable in v0.0.1; do not implement distributed infrastructure solely to anticipate later demand.

Accepted primary v0.0.2 deployment: a local Apple M5 Max workstation with 64 GB unified memory here and 128 GB at work. Available disk, Know Code's memory budget, repository sizes, semantic project counts, update rates, concurrent clients, freshness, and retention are not yet known. Total machine RAM is not a resource allocation to Know Code.

## Workstation-first design implications

Resource efficiency is an explicit product priority. The user's high-end machines are evaluation environments, not required compute. Design for light baseline operation and make large-workload costs visible; do not treat spare workstation memory as a reason to retain all graph or compiler state.

The proposals below are directed at one-machine estate indexing first. Shared/distributed stages remain future possibilities, not prerequisites for v0.0.2.

- Keep a persistent disk-backed estate index with bounded caches; do not require every repository's graph to remain in RAM.
- Independently refresh repository/project analysis and reuse compatible artifacts; avoid full-estate reanalysis for a local edit.
- Budget compiler-worker concurrency and residency. Loading hundreds of Roslyn solutions or TypeScript programs simultaneously is not the intended execution model.
- A persistent local coordinator continues indexing with no consumers connected. Share resource scheduling across registered repositories; monitoring an unchanged repository does not require retaining its compiler workspace in memory.
- Measure compiler memory, graph/search caches, file cache behavior, and storage growth alongside the applications running on the workstation.
- Coordinate cross-repository lookup locally, with dependency/version-aware identities and bounded fanout. Cross-repository queries do not require network-distributed query execution.
- Establish memory/disk/freshness targets before approving a storage engine. The chip name alone is not a performance guarantee.

## Storage selection implications

SQLite is now accepted as the initial durable store, with one index database per repository/worktree plus a small central registry and bounded graph traversal in Rust, conditional on validation against performance goals. This is not evidence that hundreds-of-repository workloads have already passed. Detailed schema, cross-repository catalog layout, and publication mechanics remain open.

Indexes and registry/coordinator state are centrally stored per user, outside repositories. Independent workspace databases are accepted; cross-repository catalog/link placement still needs a concrete design. Consumers and language providers must reuse registered indexes rather than multiply storage per client or language.

SQLite supports embedded and application-server use, but permits one writer per database file. WAL permits concurrent readers and a writer, with checkpoint/lifetime considerations; WAL is not a way to share the database directly across machines on a network filesystem. These limits motivate evaluating partitioning and workload, not declaring that SQLite cannot hold a large graph.

Sources: [appropriate SQLite uses](https://www.sqlite.org/whentouse.html), [WAL concurrency and constraints](https://www.sqlite.org/wal.html).

Repository count alone cannot select the store. Primary dimensions are stored facts/edges, relationship density, changes per unit time, cross-repository query fanout, write contention, cache locality, and retention. Compiler/project loading may dominate indexing independently of persistence.

## Candidate upgrade path

| Stage | Placement and execution | Intended continuity |
| --- | --- | --- |
| Embedded | Core and local index in the application process; explicit project/repository scope. | Same canonical facts, identities, query semantics, analysis contract, and host bindings. |
| Shared, one machine | Same Rust engine serves independently indexed partitions; bounded analysis concurrency; query coordinator routes and merges results. | Reuse index artifacts and core operators; publish a manifest of repository generations. |
| Shared, multiple machines | Place partitions and read replicas on multiple hosts, route queries to relevant owners, and distribute indexing jobs. | Same facts and query semantics inside each host; deployment layer adds placement, communication, and failure handling. |

These are proposed architectural capabilities, not implemented performance or migration guarantees. Distributed serving adds real coordination, authorization, and operational complexity. Implement stages only as required by confirmed release scope.

An index database per repository/worktree with explicit current-generation identity is the accepted starting layout. Future repartitioning is a measured upgrade option, not extra infrastructure to build now. Initial queries are current-state-only. Keep identity/artifacts extensible to historical revisions later, without building or retaining a revision archive now. Avoid full graph duplication per save; decide baseline/overlay and transient-generation reclamation mechanics separately.

Keep graph data on storage local to the serving owner; serve application requests over the network. Do not turn local embedded file access into multi-machine network-file access.

## Cross-repository graph requirements

Independent partitions alone do not provide cross-repository intelligence. A coordinator needs searchable symbol/package/service identities, dependency routing, and explicit boundary links. It must support bounded fanout, continuation, global ranking, and meaningful partial results when partitions are missing or stale.

Different repositories advance independently. A query must identify its selected generation set or consistency policy; there is no natural single Git commit for an entire estate. Package references must respect versions, not assume all repositories' latest branches compile together.

For C# services and Angular, distinguish:

- Compiler-resolved references within projects and to known package versions.
- Contract-backed links such as generated clients or API schemas, where available.
- Inferred HTTP/event/service links, carrying their evidence and uncertainty.

Angular template-to-component references are committed for v0.0.2. Template expressions have their own compiler analysis and configuration; plain TypeScript indexing does not establish template coverage. Framework-specific extraction should extend the language analysis model without contaminating the storage/query core. Detailed coverage and service-boundary extraction remain open. [Angular template type checking](https://angular.dev/tools/cli/template-typecheck).

Repository permissions must apply to routing, cross-boundary traversal, returned paths/snippets, counts, and caches, including derived impact answers.

## Accepted rebuild behavior

For upgrades requiring an index rebuild, construct a replacement beside the existing workspace index, then switch to the replacement only when ready. Compatible old data remains queryable during rebuilding with its actual freshness/coverage and served-format identity. This does not promise that a new client can read every previous format; incompatible consumers require an explicit compatibility outcome, not unsafe reads or an empty success.

Rebuilds share the normal per-user scheduling/resource policy, including active-workspace priority and progress for other queued work. Bound simultaneous replacements and temporary disk use across the estate. Do not make the upgrade depend on holding every compiler workspace resident or rebuilding hundreds of repositories simultaneously. A failed/interrupted rebuild preserves the prior usable index. Exact resumability, disk headroom policy, and supported format/version ranges remain open.

Cutover must select a coherent replacement and preserve ongoing saved-input tracking; old index/source data remains until pinned readers release it or their accepted cursors expire. Reclaim retired data safely rather than retaining an unbounded backup/history archive. Exact manifest/atomic selection, writer handover, crash recovery, and mixed-client compatibility mechanisms still require design and validation. Replacing an entire index is upgrade behavior, not the routine per-save update mechanism.

## Early-version compatibility

Accepted for v0.0.x: compatible library/coordinator versions may coexist; incompatible pairs return an actionable update-required error. Broad backward compatibility is not required while the APIs settle. This does not authorize unsafe index access, forced updates of another client's files, or automatically starting a competing coordinator.

Compatibility must account for the actual control protocol, fact semantics, native binding ABI, and readable index format at their relevant boundaries. Package-version equality is not the compatibility algorithm, and a minor-version difference need not imply incompatibility. Return the detected components/versions, incompatible boundary, and concrete update action through the shared error model. Exact supported ranges, capability negotiation, and behavior of already-open clients across cutover remain design work. The accepted preservation of pinned readers still applies; it does not promise new operations for incompatible clients.

## Data and API upgrade contract to design

1. Public APIs expose typed facts/queries and opaque identities, not SQL or a database vendor's row identifiers.
2. Version analysis facts, artifact schema, storage format, and binding/worker protocol deliberately; these are related compatibility boundaries, not separate validation frameworks.
3. Define a portable fact artifact sufficient to rebuild derived storage without rerunning compilers when compatible. Retention and completeness are explicit; an artifact missing new required facts still requires analysis.
4. Provide a migration tool that reads a known old format and writes a new destination, verifies integrity, and supports cutover/rollback. Storage and semantic changes must be distinguished.
5. Pin source revisions/configuration and preserve provenance during migration. Rebuild optional derived search structures from canonical facts where possible.
6. Test an actual migration path before promising compatibility. Artifact versions alone do not implement an upgrade story.

Keep one narrow internal persistence boundary driven by real core operations. Do not build generic adapters for several unselected databases. If a measured bottleneck requires a different store, change persistence while preserving the public semantic model; query plans and operational behavior may still need substantial work.

## Upgrade triggers to measure

- Sustained index queue growth or write-lock waits prevent freshness targets.
- Cross-partition fanout or graph expansion misses latency goals despite bounded queries and appropriate indexes.
- Working sets or artifacts exceed affordable memory, local disk, or recovery time.
- Read traffic warrants replicas, or placement needs require multiple hosts.

A faster local database does not automatically solve query fanout, semantic linking, or distributed consistency. Partitioning also does not rescue a slow individual-partition query. Diagnose and measure the relevant layer before changing storage.

## Architectural references

[Glean databases](https://glean.software/docs/databases/) identify databases by name and version/hash and support local/server access plus completed-database backup/restore. [Sourcegraph architecture](https://sourcegraph.com/docs/admin/architecture) separates repository storage/synchronization, search indexes, and precise-navigation ingestion. These support the general separation of analysis artifacts, serving, and placement; they do not prove the proposed Know Code topology or performance.
