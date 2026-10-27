# Source sampling plan

This plan precedes question generation and tool runs. The cohort and subject-file scope are recorded in
[manifest.json](manifest.json) and [scope.json](scope.json). It is a deliberately chosen cohort, not a population
estimate for all software. Record deviations before scoring and retain the original plan.

The primary bank contains 24 questions:

| Task | Jellyfin | Bitwarden core | Autofac | Total |
| --- | ---: | ---: | ---: | ---: |
| HTTP callers of a C# controller action | 3 | 3 | 0 | 6 |
| C# interface implementations and DI registrations | 1 | 2 | 3 | 6 |
| Callers at one and two hops from a C# method | 2 | 2 | 2 | 6 |
| Single-repository definition lookup | 2 | 2 | 2 | 6 |

Each repository contributes one lookup control. HTTP subjects come from the server repository. DI and impact
subjects are pooled across a family's eligible C# files, with repository identity retained. These quotas are fixed
before outcomes. Do not transfer an unfillable quota to a more successful task or family; report the shortfall.

Use seed `knowcode-public-v1-source-2026-10-16` for subject selection. Enumerate source declaration candidates
without KnowCode or either agent arm. Reuse the neutral bank's C# method, interface and HTTP-action patterns with
the explicit public subject-file scope, rather than its old directory exclusions. The HTTP population consists of
attributed controller actions, not every way of declaring an HTTP endpoint. Impact subjects use ordinary C# methods;
the neutral bank's framework-entry and constructor exclusions apply. For lookup controls use named C# type declarations
or named exported JavaScript/TypeScript class/function declarations. Record extraction rules and their limitations.

Generate the ranked population and provisional picks outside the checkout, using Python 3.11 or newer
(required by the neutral bank):

```sh
python3.11 scripts/sample-public-eval.py --source /tmp/knowcode-public-eval-source --census /tmp/knowcode-public-eval-census --scope /tmp/knowcode-public-eval-scope --out /tmp/knowcode-public-eval-candidates
```

The helper checks the recorded scope against pinned source before and after declaration extraction. Its
`provisional.json` contains review candidates, not accepted questions or labels. Retain `candidates.jsonl` and
`sampling.json`; they bind the full population, seed, producer versions, quota shortfalls and provisional picks.

Give each candidate a stable identity containing task, family, repository, path, declaration line and symbol.
Rank by the hexadecimal SHA-256 of the seed, a newline and that identity; use identity as a tie-breaker. Process strata
in the table's order, families in name order, and lookup repositories in name order. Keep the ranked population and
record every rejected candidate. Select at most one subject per file within each stratum; this prevents a large
controller or interface file from filling a quota. The same file may contribute to a different task type.

Review candidates in rank order using pinned source only. Reject source-extraction mistakes, declarations outside
the stated population and duplicates, documenting the exact reason and evidence. A valid method with no callers,
an interface with no implementation, a package boundary, an unsupported pattern, or an ambiguous relationship is
not a rejection reason. Keep the first valid subjects satisfying each quota. Do not inspect tool answers or index
availability when validating candidates. Freeze the ranked candidates, exclusions and selected questions together.

For HTTP, ask for production caller files in the other repository of the pair, using both repositories as evidence,
and disclose external SDK boundaries. Resolve routes from source before labelling callers. For DI, ask for implementations and registration sites separately;
preserve the distinction between inheritance and runtime resolution. For impact, ask for direct call sites and call
sites of those callers, grouped by shortest distance one or two; stop traversal after two hops and handle recursion
explicitly. This two-hop requirement overrides the bank's default treatment of indirect callers as optional. Score
the union of required source locations and report hop assignment separately; union recall cannot establish correct
hop assignment. Do not infer runtime reachability solely from a syntactic call or DI registration.

Hop counting uses named source members as graph nodes. An anonymous lambda's body belongs to its nearest named
method, local function or constructor; a named local function is its own node. Constructors count as members, including
explicit object-creation calls and `base(...)`/`this(...)` initializers. Include method-group and delegate references
as source dependencies and flag possible dispatch. Do not insert implicit runtime activation or framework callback
nodes into this graph. A conditional source edge does not establish that a call executes. This rule was clarified
during source-only label review, before either arm ran; it defines a source impact task rather than a runtime trace.
Match impact locations at the exact callee-expression line. The bank's usual two-line tolerance can accept a
different, adjacent invocation; labels record their match rule explicitly. Score distinct file/line locations and
disclose that multiple call expressions on one line are not distinguished by that score.

Lookup questions identify repository, name and declaration kind, without giving the answer's file or line. Labels
include all matching production definitions, including partial declarations, and document same-name negatives.
Generated source and tests remain available as evidence for every task even when they are not sampling subjects;
separate test-only findings from required production items. Independently review a seeded subset under the main
[method](method.md) before scoring. No questions, labels, run order or agent configuration are frozen yet.
