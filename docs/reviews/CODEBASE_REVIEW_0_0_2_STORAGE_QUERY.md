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
