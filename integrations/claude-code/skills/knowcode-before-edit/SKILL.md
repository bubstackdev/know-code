---
name: knowcode-before-edit
description: Assess a change before making it, with the KnowCode index - what depends on the code, which tests cover it, which files historically change with it, and who should review it. Use before editing a shared method, type, table or contract, or before opening a merge request.
---

# Assess a change before editing

Use KnowCode's advertised `kc_*` tools; the client may add a plugin or server prefix to their names.
Run them on the symbol or file you plan to change, or on `diff: "working"` once you have edits.

## Reading every answer

- Check `complete` first. When it is false, read each `incomplete` entry: its `reason` says why the answer is
  partial (`capped`, `budget`, `stale`, `deadline`, `failed`, `missing_repo`, `unsupported`, `policy`, `withheld`)
  and its `hint` says how to get the rest.
- Follow the `hint`s and cursors (`kc_more {cursor}`) whenever the rest of a list could change your conclusion. A
  first page is not the whole list, and a trace one hop deep is not the whole call graph: keep tracing until the
  answer is complete or you can say what is left.
- An empty or short answer is not proof of absence. It may be stale, sampled, withheld, or outside what KnowCode
  models (see its `note`); confirm in the source before you say something has no callers or no uses.
- Read `confidence` and `evidence` together. `ambiguous` lists candidates, none picked; `unique_name` is a name
  match; even `exact` can rest on `assumptions` (`same_deployment`, `same_database`, …) that you should state.
- Report what was incomplete and what you checked by hand.

## Steps

1. **What depends on it?** `kc_impact(symbol: "OrderService.Place")`, or `file: "src/Orders/OrderService.cs"`, or
   `diff: "working"`.
   - One hop by default. That is where the walk starts, not where it ends: ask again with `depth: 2` to `5`
     until the dependents you report reach entry points, tests or other repos, and follow the cursor when
     `affected` was cut.
   - Dependents are classed by proximity: `direct` (hop 1), `near` (2), `far` (3+), `test`. That is graph
     distance, not risk: a `far` dependent can matter more than a `direct` one, so read what they are. Watch the
     cross-repo dependents (HTTP routes, message bus, SQL), the `low_confidence` count, and `coupled`: files that
     change with this one but nothing references.
2. **Which tests?** `kc_tests(targets: ["src/Orders/OrderService.cs"])` or `kc_tests(diff: "working")`. It ranks
   test files by references, co-change history and names. Run the `tests` list; `left_out` ranks the other
   candidates, which may fail too, so nothing in the answer says a test is safe to skip.
3. **What else usually changes with it?** `kc_coupled(file: "src/Orders/OrderService.cs")`. A partner with high
   confidence that your change leaves alone is worth a look. With a diff, `kc_impact` also reports
   `missed_partners`.
4. **Who should review?** `kc_reviewers(files: ["src/Orders/OrderService.cs"])` or `kc_reviewers(diff: "working")`.
   It ranks the authors who recently added lines to the changed files (history, not current expertise) and leaves
   you out.

Report the direct and cross-repo dependents, the tests to run, any missed partner, and the suggested reviewers before you edit.
