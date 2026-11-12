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
code asks for the declaration line, not a separate extent line.

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

## Current-source baseline

| IDs | Released-source disposition |
| --- | --- |
| H6, M14, M15 | Already fixed: `store::ensure_cleanup_indexes` installs the three cleanup/FK probe indexes; the existing regression checks access paths, retained data and FK coherence. |
| H10, H11 | Already fixed: a bounded frontier probe precedes expansion at maximum depth; adjacency predicates precede the SQL row cap. Existing dense-frontier and rejected-kind regressions cover these paths. |
| L19 | Already fixed with H10: `GraphResult::limit` deduplicates limit labels before insertion. |
| L21 | Already fixed: compact symbol projection uses 17-location lookahead, keeps at most 16 and labels limited declarations for concrete supports. |
| M16 | Already fixed and release-validated: explicit staging PASSIVE checkpoints and active-job maintenance reservation supplement the shared disk-pressure pause/resume path. No resource cap is raised here. |
| M22 | Accepted independent graph/result/page budgets. No change to make their limits equal. |

The remaining assigned IDs are still being checked; final dispositions and any
additional focused regressions will be added before this lane is closed.
