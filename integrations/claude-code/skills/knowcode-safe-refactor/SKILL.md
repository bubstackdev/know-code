---
name: knowcode-safe-refactor
description: Rename a type, method or property, or delete unused code, with the KnowCode index - preview every site a rename touches (C# and TypeScript) and find dead code worth deleting, before editing.
---

# Safe refactor: dead code and renames

Use KnowCode's advertised `kc_*` tools; the client may add a plugin or server prefix to their names.
Both tools preview only; you make the edits. An incomplete answer never proves that a deletion or rename is safe.

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

## Dead code

1. Run `kc_unused(tier: "unused")` for confident findings only. Narrow with `kinds: ["method", "property"]`; raise
   `limit` for more.
2. Treat `possibly_unused` findings (`tier: "possibly"`) as leads, not verdicts. Each lists `reasons`: reflection
   strings, config, templates, tests only, public API of a library other repos may use. Check those reasons before
   you delete.
3. Before deleting, confirm with `kc_refs(name: "Type.Member")`. Search strings with `kc_search(pattern: "Member")`
   when the reasons mention them.

## Rename

1. Run `kc_rename_preview(symbol: "OrderService.Place", new_name: "Submit")`.
   - For an overload, pass `symbol: "src/Orders/OrderService.cs:42"`.
2. Check `refused` and its reason (ambiguous sites, a name collision). Don't force it with `allow_ambiguous: true`
   until you have read the ambiguous sites.
3. Edit exactly the listed sites: definition, interface members, overrides, call sites, `nameof`, strings it flags.
   Then rebuild.
4. The preview covers indexed repos that depend on this one. A public member used outside the index can still
   break, so say so when the type is a published package.
