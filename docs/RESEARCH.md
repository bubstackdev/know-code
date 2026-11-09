# Research baseline

Research date: 2026-11-05.

Three research agents examined OSS capabilities, performance/evaluation evidence, and commercial/mixed product lessons. Primary documentation and selected source declarations were reviewed. No products were installed or independently benchmarked.

A subsequent implementation crawl cloned six reference repositories into the durable `.research/repos/` directory and reviewed selected source paths. Exact origins and commits are in [.research/repositories.json](../.research/repositories.json); findings and corrected source-specific observations are in [source review](SOURCE_REVIEW.md). Reference project code was not executed or incorporated into Know Code.

Features below are documented capabilities, not independent verification. Publisher benchmarks are claims under their stated workloads. Product recommendations are inferences, not proof of universal competitor gaps. Web documentation and repository main branches change; pin a revision before depending on a specific implementation or reusing code.

## Product and architecture references

| Reference | Durable lesson | Primary sources |
| --- | --- | --- |
| CodeGraphContext | Structural graph, optional SCIP, CLI/MCP, indexing lifecycle, watching, coverage and fallback concerns. | [Repository](https://github.com/CodeGraphContext/CodeGraphContext), [MCP tools](https://github.com/CodeGraphContext/CodeGraphContext/blob/main/docs/MCP_TOOLS.md) |
| codebase-memory-mcp | Persistent local graph, multiple retrieval modes, incremental operations, docs/IaC context, shared artifacts; broad features are already available in OSS. | [Repository](https://github.com/DeusData/codebase-memory-mcp), [Configuration](https://github.com/DeusData/codebase-memory-mcp/blob/main/docs/CONFIGURATION.md) |
| Joern | Code property graphs and deeper analysis are useful extensions; general interpreter interfaces have a different trust boundary from bounded agent queries. | [Overview](https://docs.joern.io/), [CPG](https://docs.joern.io/code-property-graph/), [Server](https://docs.joern.io/server/) |
| Serena | IDE-style navigation is a relevant baseline; capability varies by language/backend. | [Repository and feature matrix](https://github.com/oraios/serena) |
| Glean | Typed, deduplicated immutable facts, language-specific detail, shared symbol API, and incremental database layers. | [Introduction](https://glean.software/docs/introduction/), [Incrementality](https://glean.software/blog/incremental/) |
| Sourcegraph | Combine navigation/search primitives with task-oriented retrieval; SCIP enables reusable code-navigation indexes. | [MCP](https://sourcegraph.com/mcp), [Precise navigation](https://sourcegraph.com/docs/code-navigation/precise-code-navigation), [MCP administration](https://sourcegraph.com/docs/api/mcp) |
| Augment | Working-directory and remote default-branch retrieval need distinct freshness/revision semantics. | [Context Engine MCP](https://docs.augmentcode.com/context-services/mcp/overview) |
| CodeGPT / DeepGraph | Direct connections, usage links, source and semantic retrieval; an MIT connector does not establish an OSS indexing engine. | [MCP connector](https://github.com/diffbot/mcp-code-graph), [Code Graph documentation](https://help.codegpt.co/code-graphs) |
| Potpie | CLI-first context graph, readiness/diagnostics, and broader engineering context; keep extracted facts distinct from durable claims. | [Repository](https://github.com/potpie-ai/potpie), [Commercial platform](https://potpie.ai/) |
| Graphify | Local extraction, explained/inferred relationships, inspectable graph artifacts, optional cloud capabilities. | [Repository](https://github.com/Graphify-Labs/graphify) |
| GitLab Orbit | Shared graph authorization and explicit separation of default-branch remote code from unmerged local code. | [Architecture](https://docs.gitlab.com/orbit/how-it-works/), [Security](https://docs.gitlab.com/orbit/security/), [Overview](https://docs.gitlab.com/orbit/) |

## Standards and reusable ideas

- [Tree-sitter](https://tree-sitter.github.io/tree-sitter/) builds and incrementally updates concrete syntax trees, including in the presence of syntax errors. Syntax extraction is not full type/name resolution.
- [SCIP](https://scip-code.org/) standardizes language-independent code-navigation indexes. Its [schema](https://github.com/scip-code/scip/blob/main/scip.proto) describes occurrences and symbol relationships; do not assume it contains every graph edge Know Code needs.
- [Glean incrementality](https://glean.software/blog/incremental/) illustrates shared immutable bases and replacement layers, including the difficulty of derived-fact ownership.
- [Stack graphs](https://github.com/github/stack-graphs) are a useful file-incremental name-resolution reference. The upstream repository states it is no longer supported or updated by GitHub; it is not an automatic dependency recommendation.

## Performance evidence

| Evidence | Reported result | Limitation / lesson |
| --- | --- | --- |
| [Codebase-Memory preprint](https://arxiv.org/abs/2603.27277) | Across 31 repositories: 83% answer quality vs 92% for file exploration; ten times fewer tokens and 2.1 times fewer tool calls. | Author evaluation and a specific agent setup; token reduction is not sufficient evidence of better outcomes. |
| [codebase-memory project performance](https://github.com/DeusData/codebase-memory-mcp#performance) | Project reports Linux kernel indexing around three minutes on M3 Pro and very fast bounded graph queries. | Not independently reproduced; structural indexing and internal query times cannot become semantic or end-to-end guarantees. |
| [SCIP announcement](https://sourcegraph.com/blog/announcing-scip) | Historical report of roughly 10x TypeScript indexing speedup and 4x compressed payload reduction against an older LSIF workflow. | A 2022 comparison; the indexer implementation contributed to speedup. Useful architectural evidence, not a current target. |
| [Glean incrementality](https://glean.software/blog/incremental/) | Reports modest typical storage/indexing overhead, with some query patterns significantly more expensive. | Correct incrementality has costs; measure representative workloads and affected dependencies. |
| [GitLab Orbit architecture](https://docs.gitlab.com/orbit/how-it-works/) | Documents initial indexing in minutes to hours and subsequent updates in seconds to minutes. | Hosted product behavior is not directly comparable to local structural indexing. |

For benchmark design, also consult the [Codebase-Memory measurement guide](https://github.com/DeusData/codebase-memory-mcp/blob/main/docs/MEASURING_SAVINGS.md).

## Licensing and classification cautions

- OSS and commercial are overlapping categories. Classify the indexer, query engine, storage format, and connectors individually.
- Research observed MIT for CodeGraphContext and codebase-memory-mcp; Apache-2.0 for Joern and Potpie; Apache-2.0/MIT components in Graphify. Recheck component licenses at the revision used.
- Sourcegraph's open SCIP tooling does not make its commercial service an OSS distribution.
- GitLab's open-core model does not make Orbit free; current overview lists Premium/Ultimate.
- The DeepGraph MCP connector is MIT, but it connects to hosted graph services.
- Subsequent project decision: Know Code uses Apache-2.0; see [LICENSE](../LICENSE). The user had no strong license preference. Apache-2.0 was chosen as a permissive OSS license with an explicit contributor patent grant under its stated terms. This does not change upstream reference/component licenses. Primary references: [official license](https://www.apache.org/licenses/LICENSE-2.0), [applying it to your own work](https://www.apache.org/foundation/license-faq.html#Apply-My-Software).

## Product synthesis

The proposed opportunity is a common, inspectable engine with reliable checkout identity, updates, evidence, and bounded retrieval across embedding, local tools, and shared services. The research does not establish that any one of these capabilities is unique.

Prioritize a coherent fact/query contract and real language capability over large feature or parser counts. Keep broader SDLC ingestion, agent orchestration, code editing, and mandatory LLM extraction outside the initial core.
