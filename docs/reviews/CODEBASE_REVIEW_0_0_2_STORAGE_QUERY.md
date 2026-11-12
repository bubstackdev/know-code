# v0.0.2 storage and query review

The Snow Leopard stabilization lane starts from released `e20da5d`. The original
`CODEBASE-REVIEW.md` predates several release fixes; its recommendations are
rechecked against that source before making changes. New execution evidence is
root-owned. This lane runs syntax/static checks only and does not claim that an
unrun regression passes.

## Confirmed changes

**L18 — incremental project replacement.** The released replacement subquery
selects compilation IDs by `project_path`, but its indexes cannot seek that
column. Shared idempotent write-schema setup now adds the covering
`compilation_project(project_path,id)` index for new and retained compatible
stores. The existing cleanup-index regression adds the exact replacement
subquery and requires an indexed search without a scan. This does not change
the store format, facts, compiler identity or retention semantics.

Root validation requested:

```sh
cargo test -p know-code-core --locked --lib store::cleanup_index_tests::cleanup_indexes_cover_owner_and_foreign_key_probes_and_preserve_retained_facts -- --exact
```

**M24 — repeated point-query preparation and reference classification.** Shared
symbol headers, finding keys, test masks and definition-boundary lookups now use
the connection's bounded prepared-statement cache. Reference projection consumes
the classification JSON already joined by its bounded SQL stream instead of
issuing another query for each occurrence/support. The regression uses three
compilation-owned origins with different classifications and rejects a direct
origin-table point query while allowing the joined classification view. This
preserves each origin's evidence, rather than synthesizing a symbol-wide class.

Root validation requested:

```sh
cargo test -p know-code-core --locked --lib query::tests::references_project_joined_origin_evidence_without_point_queries -- --exact
cargo test -p know-code-core --locked --test testing
```

**M25 — repeated indexed-source prefix scans.** Declaration line projection now
keeps at most 128 hash/byte-offset checkpoints during one query. Later offsets
resume from the nearest earlier checkpoint, repeated offsets reuse their exact
line, and earlier offsets still scan from a valid earlier position. Checkpoints
contain line/CR state rather than source payloads and are cleared at both request
boundaries. Cancellation and index-health checks also run on cache hits. This
does not add a source-history cache or use current disk text. The regression
covers CRLF across the blob chunk boundary, embedded NUL, multibyte text,
backward offsets, bounded hints and request cleanup. The original report's
suggestion of two unconditional scans per declaration is inaccurate: released
code asks for the declaration line, not a separate extent line. An uncached
offset zero still resolves/opens indexed content, preserving the prior failure
behavior for missing content; only an existing exact checkpoint avoids that read.

Root validation requested:

```sh
cargo test -p know-code-core --locked --lib query::tests::indexed_line_checkpoints_preserve_crlf_nul_utf8_and_request_bounds -- --exact
cargo test -p know-code-core --locked --lib query::tests::declaration_source_selects_exact_owner_origin_and_hash_before_limit -- --exact
```

**M23 — graph coverage projection.** Summary graph queries now use the shared
relevant-coverage projection instead of returning every compilation's metadata
and capability entry. Workspace aggregate status/diagnostic counts remain
workspace-wide; explicit `coverage_detail: full` still returns workspace detail.
Relevance includes roots with no findings, selected compatibility references,
attempted unavailable/conflicting contexts, body-definition links, each path's
source/target, dispatch proof owners/links and selected consumer witnesses.
Boundary evidence and conflicting context IDs are retained; public IDs are not
parsed. This narrows unrelated metadata without treating missing capability or
an unresolved boundary as successful analysis.

Root validation requested:

```sh
cargo test -p know-code-core --locked --test graph project_links_are_exact_snapshot_bound_and_keep_pinned_readers -- --exact
cargo test -p know-code-core --locked --test graph dispatch_uses_specializations_and_snapshot_bound_composition_witnesses -- --exact
cargo test -p know-code-core --locked --test graph graph_coverage_keeps_empty_roots_and_unavailable_contexts -- --exact
cargo test -p know-code-core --locked --test graph
```

## Per-ID disposition

“Fixed before this lane” refers to released source, not a newly executed test.
“Changed here” requires root's matching build/regression result before claiming
validation. Outstanding measurements below remain outstanding.

| ID | Disposition | Current source and regression witness |
| --- | --- | --- |
| H6 | Fixed before this lane. | `store::ensure_cleanup_indexes`: `unit_artifact_owner(owner_generation,artifact_id)`; cleanup-index plan/retained-fact/FK regression. |
| H10 | Fixed before this lane. | `graph::query` probes the frontier before adjacency expansion at maximum depth; `depth_frontier_is_only_probed_without_expansion_or_out_of_depth_boundaries`. |
| H11 | Fixed before this lane. | `edge_predicates` applies kind/implicit/exact filters before SQL row caps; `edge_filters_apply_before_the_sql_row_cap_in_both_directions`. |
| M14 | Fixed before this lane. | `artifact_content(content_hash)` covers the contents child-FK probe; shared cleanup-index regression. |
| M15 | Fixed before this lane. | `generation_unit_owner(owner_generation,compilation_id)` covers compilation retention probes; shared cleanup-index regression. |
| M16 | Fixed before this lane; no caps changed. | `Writer::maintain_staging` runs explicit PASSIVE after 64 committed payloads. Existing pinned-reader/WAL reuse, typed-stop/storage-error and active-reservation regressions; see [WAL_PRESSURE.md](../WAL_PRESSURE.md) for retained actual Polly and failed-run history. |
| M17 | Accepted bound; scale/contention measurement remains open. | `recovery::validate_candidate` retains a 120-second integrity/checkpoint deadline under the supplied interruptible Control. [INDEX_RECOVERY.md](../INDEX_RECOVERY.md) explicitly distinguishes timeout from corruption and requires explicit retry after a failed allocated attempt. |
| M21 | Confirmed repeated work; optimization awaits measurement. | `visits` preserves `(node,possible,context)` and up to eight paths; `dispatch::neighbors` is called again on admitted revisits. SQL preparation is already cached. No general 8× slowdown is established. |
| M22 | Accepted independent budgets. | [QUERY_LIFECYCLE.md](../QUERY_LIFECYCLE.md) distinguishes logical graph/materialization, page and full response caps. `response_limit_exceeded` remains a typed whole-response outcome; limits are not forced equal. |
| M23 | Changed here; root validation requested above. | Summary coverage now retains root/path/proof/witness/compatibility/boundary contexts; explicit full workspace detail remains available. |
| M24 | Changed here; root validation requested above. | Cached point statements and joined per-reference classification replace repeated preparation/point SQL. |
| M25 | Changed here; root validation requested above. | Request-local, bounded hash/offset checkpoints reduce repeated source-prefix work while preserving byte-based newline semantics. |
| M26 | Claim overstated; SQL setup measurement remains open. | `symbols_sql` groups/orders matching finding keys before SQL LIMIT; Rust collects only `limit+1` keys. Exact-name/FTS paths have `symbol_filters_use_name_or_fts_lookups`; browsing and target-equivalent grouping remain accepted behavior. |
| L16 | Residual cleanup measurement remains open; not fixed by a new bound. | `Writer::drop` still deletes staging/manifests and reclaims without a progress handler. Missing cleanup/FK probes are repaired; `publication_checkpoint_and_failed_writer_cleanup_release_the_cursor_fence` verifies cleanup is outside the cursor mutex, not that all cleanup is fast. |
| L17 | Plan/write-cost measurement remains open. | `symbol_test_mask` cannot provide an ordinary seek for the current bitwise predicates, but “never usable” is stronger than demonstrated. A covering scan or future plan is not ruled out by static inspection. Index retained pending measured evidence. |
| L18 | Changed here; root validation requested above. | Shared idempotent `compilation_project(project_path,id)` plus exact replacement-probe plan assertion. |
| L19 | Fixed before this lane. | `GraphResult::limit` deduplicates labels on insertion; depth-frontier regression does not accumulate per-neighbor depth labels. |
| L20 | Confirmed metadata parsing; setup-cost measurement remains open. | `Contexts::load` parses current compilation references for exact consumer/fingerprint compatibility. These are compilation metadata, not all graph facts. No resident/global graph cache added. |
| L21 | Fixed before this lane. | Shared compact `graph::symbol` reads at most 17 declaration locations, retains 16, and labels limited supports; `graph::tests::compact_symbol_search_bounds_declarations_and_labels_every_exact_support`. |

The self-recursion behavior associated with the adjacent provider finding is
also present in released core: `root_self_calls_are_terminal_findings_and_mutual_recursion_stays_bounded`
tests callers/callees terminal self-findings and bounded impact recursion. This
lane does not edit provider extraction.

## Remaining measurement and design boundaries

**L16:** first measure cancelled/failed staging cleanup after the repaired probe
indexes, with current and cursor-retained facts and a competing writer. A narrow
possible follow-up is a short independent cleanup deadline/progress handler,
rather than the already-cancelled analysis Control. That needs a deterministic
interruption regression before implementation. Decide whether manifest deletion
and reclamation must share one transaction: current separate statements may
commit some cleanup before a later interruption. Preserve current/pinned owners
under either choice. Interrupted cleanup must leave discoverable, charged
reclamation debt that the existing write/maintenance path retries; deleting a
staging receipt before recording debt must not make retained bytes unaccounted
or leave pressure data without a retry path. The retiring writer must release
its fence even when cleanup is interrupted. No new sweeper or cap increase is implied.

**M21/L20:** retain separate timing and work counts for first expansion versus
revisited contexts and metadata setup on named graph fixtures. An adjacency memo
must cap retained evidence bytes, preserve context/path-specific proof selection,
charge actual uncached proof work and keep cancellation/time/state bounds. A
whole-graph cache is unwarranted. A per-ReadView metadata cache may give little
benefit to fresh-view callers and retain unnecessary state; measure that lifecycle
before changing it. Existing converging-context and compiler-witness regressions
remain mandatory for any optimization.

**M17/M26/L17:** measure sealing under accepted resource/contention conditions,
SQL grouping for broad browsing versus focused lookup, and actual mask-index
plans/write cost. Keep failed observations and distinguish setup/cold work from
the accepted warm focused lookup gate. None is closed by this static review.

Registry versioning (architecture 4) belongs to the runtime lane. Recovery/disk
module decomposition (architecture 5) is not needed for the confirmed narrow
fixes here. A typed operation rewrite (architecture 7) is not introduced without
a concrete cross-entry validation defect; root owns the current MCP/CLI boundary
changes. This avoids a second query engine or cross-lane source edits.

## Validation handoff

All owned Rust files pass standalone `rustfmt --edition 2024 --check`; all diffs
pass `git diff --check`. No cargo/build/native/runtime/VM command ran in this
lane. In addition to the focused commands above, root should run affected core
`--test context`, `--test publication`, `--test target_grouping`, the full graph
integration target and the normal workspace gates once integrated. Broader
release/performance claims require their actual resulting receipts.
