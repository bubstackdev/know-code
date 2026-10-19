---
name: knowcode-debug-stack-trace
description: Debug from a stack trace or error log with the KnowCode index - map each frame's file and line to its symbol, read the failing code with its callers, walk inward to the entry points that reach it, and check whether the files involved are known trouble spots. Use when given an exception, a stack trace, or a "fails at file:line" report.
---

# Debug from a stack trace

Use KnowCode's advertised `kc_*` tools; the client may add a plugin or server prefix to their names.

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

1. **Frame to symbol.** For each `at file:line` frame (innermost first; skip framework frames),
   `kc_symbol_at(file: "src/Billing/Invoice.cs", line: 214)`. It gives the enclosing symbols, innermost first, with
   the qualified name that the next steps take.
2. **Read the failing code.** `kc_context(symbol: "Invoice.Total", source: "full")`: the source and its callers and
   callees with `file:line`. Check the line from the trace against the arguments its callers pass.
3. **Who reaches it?** `kc_trace(symbol: "Invoice.Total", direction: "in", depth: 2)`. This walks inward to the
   callers, actions and consumers that lead to the failure, across HTTP routes and the message bus. Narrow it with
   `kinds: ["method"]` or `edges: ["calls"]`.
   - From a specific entry point: `kc_path(from: "InvoiceController.Get", to: "Invoice.Total")`.
   - If the trace stops before an entry point, ask again with `depth: 3` or from the last caller, and follow any
     cursor. No callers is a lead to check with `kc_refs` and `kc_search`, not a conclusion: the caller may be
     reached through reflection, a framework registration or a construct KnowCode doesn't model.
4. **Is this a known trouble spot?** `kc_hotspots(path: "src/Billing/Invoice.cs")`: churn, bug-fix ratio,
   complexity and co-change for the file. A recurring fix history points at a design problem, not a one-off.

Then form a hypothesis that names the frame, the input that triggers it, and the caller that supplies it. Confirm it
by reading those lines, and pick tests with `kc_tests(targets: ["src/Billing/Invoice.cs"])`.
