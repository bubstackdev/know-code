---
name: knowcode-explore
description: Where is X, how does X work, who calls or uses X - answer from the KnowCode index - what a repo is, where a concept lives, what a symbol does and who calls it, and what happens end to end from an entry point. Use in code you haven't read yet.
---

# Explore unfamiliar code

Use KnowCode's advertised `kc_*` tools; the client may add a plugin or server prefix to their names.
Each step narrows the next. Stop when you can answer and every answer you rely on is complete, or you can say
what is missing. Read the returned `file:line` evidence.

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

1. **What is this repo?** `kc_overview` with no arguments: languages, packages, the repos it depends on and that
   depend on it, entry points, key types, hotspots and a ranked map of the central files.
   - Interested in one area: `kc_overview(focus: "payment")` biases the map toward it.
   - Many repos: `kc_overview(workspace: true)` gives one line per repo.
2. **Where does the concept live?** `kc_locate(query: "refund approval")` ranks files for plain words or a ticket
   title. Know the name already: `kc_find(query: "RefundService")` (camelCase words work: `"refund svc"`).
3. **What is it, and who uses it?** `kc_context(symbol: "RefundService.Approve")`: source (skeleton when long),
   members, callers and callees with confidence, in one call.
   - Several at once: `kc_context(symbols: ["A", "B"])`.
   - Need more room: `budget: 4000`.
   - `context` shows the top callers. When its `hints` say a list was cut, get the rest with `kc_refs` or the
     list's cursor before you describe who uses the symbol.
4. **What happens end to end?** `kc_flow(entry: "RefundController.Approve")` (or an Angular route path such as
   `"account/refunds"`, or a component) follows route → component → effect → service → HTTP → action → DI → table,
   across repos.
   - For one path between two symbols: `kc_path(from: "A", to: "B")`.
   - A flow lists only confident steps and counts what it left out (`left_out`, `hidden`). When a step you expect
     is missing, check those counts and the `incomplete` entries, then continue from the last step with
     `kc_trace(direction: "out")`.

Confidence tags describe how a reference was resolved: a name match still carries the assumptions in its evidence.
`ambiguous` lists candidates. For text, config keys, and strings, use `kc_search(pattern: "...")`.
