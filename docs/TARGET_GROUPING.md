# Cross-target finding presentation

Status: frozen implementation contract for the next parallel release slice; not yet implemented or certified.

## Accepted behavior

Combine equivalent findings across target frameworks by default, preserving all exact member identities and supporting targets. The user explicitly chose follow-up references/callers/callees/impact over all supporting targets. Keep each path's compatible concrete compilation selections; grouping never constructs a new graph or combines edges into a synthetic path. Target-specific projected forms remain separate findings. Missing-target, partial and uncertain coverage stays visible.

The returned id/compilation_id/target remains an actual deterministic representative. A follow-up symbol_id first resolves that exact member in the served generation, then expands its stored equivalent group. target and compilation_id narrow this selection; compilation_id provides explicit single-context access. An absent original member is invalid_request, with no name/ID-text fallback. Existing generation/hash guards still apply. Test filters control findings/use sites, never root expansion or intermediate traversal.

Add supporting_targets with exact symbol_id, compilation_id, target, signature, locations and test_classification. Graph supports own their complete concrete paths; representative paths never become a cross-context union. Reference/context projections retain corresponding origin/declaration support. Keep all required evidence inside response budgets; no silent support truncation advertised as complete.

## Evidence and storage

Worker framing remains7. AnalyzeRequest.target_equivalence_policy=1 requires exact ProviderInfo acknowledgement before facts;0 remains legacy and omits new fields/capability names. Initially request1 only for C#. Compilation supplies opaque target_family plus policy version; Symbol supplies declaration_key. Reuse compiler-owned identity components; never parse public IDs, strip target text or infer equivalence from names/signatures alone. Family covers normalized project identity, effective configuration/platform and selected compiler/SDK domain. Declaration key covers actual containing assembly identity and compiler declaration identity, with owned source/synthetic anchors where necessary. Include the policy/family in own and referenced snapshot witnesses; do not change preparation policy or restore unchanged dependencies.

Format8 adds compilation target_family/policy, symbol declaration_key and private grouping_key. A group key requires family/key, a nonexternal symbol and owned declaration anchors. Include visible name/kind/signature, full test mask and ALL declaration origin/hash/ranges in the deterministic key. Hash ordered declarations, not the16-location graph projection. Missing/unproven/external keys remain singletons. Generated origins require explicit compiler generator/hint evidence; do not normalize target-bearing display paths by stripping strings. Unproven origins stay separate with honest partial grouping coverage.

SQL must select limit+1 logical groups after name/text/target/tests predicates, then read bounded supporting members; never cap concrete rows and deduplicate afterward or guess limit times framework count. Graph roots/frontiers/visited/context sets remain concrete and use one shared work/byte/deadline budget. Count grouped findings toward limit; paths_per_symbol applies per concrete member. Page size counts whole groups; materialize groups before existing cursor packing. Partial traversal exposes only actually discovered support, with existing work-limit evidence.

Keep format7 current and pinned readers coherent during compatible migration. Missing policy schedules ordinary bounded backfill, preserving unchanged cancelled/failed baselines and preparation checkpoints. Native query contract advances to5; no parallel per-host query engine.

## Parallel boundary

Grouping owner: query.rs, graph.rs, grouping.rs, store.rs migration/derived grouping, C# identity/emission/snapshot adapters, thin query binding/MCP types and focused fixtures. Root owns proto and update/workspace policy acknowledgement/backfill integration; recovery owner owns coordinator/recovery destinations. Root applies the recovery Writer/open hook to store.rs after grouping handoff and wires IndexReadGuard into ReadView after query handoff. No simultaneous writes to those shared files.


## Grouping lane implementation

The common projection implements logical limits before support materialization for symbols/references/context and counts logical graph findings while traversing concrete nodes. `supporting_targets` is omitted for singletons; multi-member arrays include the representative and all actually included witnesses. Workspace and per-compilation coverage expose missing target-equivalence evidence as unknown, including empty relevant result sets. Compiler-specific bindings and paths remain on each supporting target.

Expanded graph seeds consume `max_nodes` before root/context setup. A partial root selection reports `node_budget`; pagination cannot reset that work. The cursor client must use `ReadView::query_materialized_controlled` only for initial aggregate materialization, with the existing16MiB result allowance, before normal per-page packing. Ordinary reads use `query_controlled` and their response cap. This is one engine with two bounded output phases, not a separate graph implementation.

Focused owned Rust fixtures cover logical search/reference limits, conditional projected variants, exact support/source, peer-expanded and narrowed follow-ups, concrete graph paths/shared seed budgets, singleton unknown/external/generated cases, format7 pinned migration, and aggregate materialization larger than a page. Existing context/graph/publication/classification fixtures remain part of the affected check set. Provider/compiler, in-process native, health/snapshot/cursor and installed acceptance are separate evidence scopes; root wires the recovery guard and performs assembled validation.


### Scoped owner evidence

C# peer keys for locals, parameters, range variables, lambdas and local functions include their compiler-selected containing declaration chain. Named owners use compiler documentation identities; scoped owners use unique owned syntax/location anchors. This prevents a conditional enclosing type from making same-offset locals appear equivalent. Public symbol and synthetic IDs retain their existing recipes. Field/property initializer peer keys use the declared member supplied by the semantic model. File initializers or other unproven owners remain separate with empty peer keys and partial grouping capability.
