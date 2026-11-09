# Investigating dependency fan-out

This investigation follows the [invocation lookup optimization](INDEXING_PERFORMANCE.md#c-invocation-reference-lookup). It explains the observed Polly 53-target update and tests whether unchanged declarations justify skipping consumers. The investigation itself left product code unchanged. The subsequent [job-scoped compiler reuse](INDEXING_PERFORMANCE.md#c-compiler-reuse-within-one-job) implements the selected next experiment; invalidation scope, protocol, schema, and performance guarantees remain unchanged.

## What selects the 53 contexts

The shared Rust [update planner](../crates/core/src/workspace.rs) finds source-artifact and explicit compiler-input owners, reduces them to project paths, and expands recorded transitive project dependents. A source-byte change, including a comment, enters that path. The worker then evaluates every declared target of each selected C# project. Structural/configuration changes and unknown source ownership use broader fallback paths.

For the retained Polly index and `src/Polly.Core/ResiliencePipeline.cs`:

| Scope | Count |
| --- | --- |
| Direct owning projects | 1: Polly.Core |
| Direct owning target contexts | 6 |
| Projects in the dependent closure | 15 |
| Contexts in that closure | 53 |
| Whole-workspace contexts | 59, all complete |

The six direct targets are net10.0, net8.0, net6.0, netstandard2.0, net472, and net462. Every one owns the edited file. The other contexts belong to referencing libraries, tests, benchmarks, snippets, and the AOT sample. Tests and benchmarks remain required indexed code; deleting them from the plan would change coverage.

The [scope report](../.artifacts/invalidation/polly-scope.json) records each actual project/target and its compiler-reported reference contexts. For example, Polly.Specs net9.0 consumes Polly.Core net8.0, not a fabricated Core net9.0 context. Exact target edges would help avoid excess work for some target-specific source changes, but **would not shrink this particular closure**: this file changes all six owning Core targets.

Current Rust selection and storage replacement operate at project scope. The worker's recorded options include actual reference-context names, but that provider-specific JSON is not a shared typed dependency contract. Safe target-specific planning would need explicit compilation-context edges and source/input ownership, target-scoped worker requests and publication, compatibility handling, and broad fallback for missing evidence. That is useful future work, not a reason to parse Roslyn naming conventions inside the common planner or skip targets now.

## Reproduced limit of semantic/API equality

Roslyn's [pinned project-version contract](https://github.com/dotnet/roslyn/blob/35d9211b841e7613c1d2f8f5af6d628ace696c4c/src/Workspaces/Core/Portable/Workspace/Solution/Project.cs#L565) describes dependent semantic versions in terms of consumable declarations. That is useful compiler evidence, but it does not cover every input relevant to Know Code's generated-source and diagnostic requirements.

The private [probe](../.artifacts/invalidation/probe/Program.cs) uses the same Roslyn 5.9 C# workspace APIs as the provider. Core and Consumer are connected by a normal project reference. Consumer has an incremental source generator that observes syntax trees through a `CompilationReference` using public compiler APIs. The only edit appends a comment to Core; its declarations and ordinary syntax remain equivalent.

The [observed result](../.artifacts/invalidation/counterexample.log):

1. Core syntax is equivalent when trivia is ignored.
2. Consumer's `GetDependentSemanticVersionAsync()` stays unchanged.
3. The generator observes the new comment and changes its emitted method from `BeforeComment` to `AfterComment`.
4. Unchanged Consumer source calling `BeforeComment` now reports CS0117. It had no compiler errors before the save.

This is a legal synthetic counterexample, not a claim that Polly's generators behave this way. It establishes that ignoring comments, hashing only declarations/public signatures, or trusting the dependent semantic version alone is insufficient for a general reuse rule. Skipping Consumer would retain wrong generated source and hide a required fresh partial graph. The probe asserts all four observations and retains a private dependency lockfile. No generator or collector is added to the product.

Other relevant boundaries include effective compiler/SDK options, assembly identities, target selection, linked/additional/analyzer inputs, generators, and SDK evaluation hooks. A contract fingerprint would have to account for what the consumer can observe, not only method/type names. The existing [SDK identity/import tests](POLLY_RESOLUTION.md) already demonstrate why guessed identities or stale evaluation results are unacceptable.

## Repeated compiler work is a separate opportunity

The preceding [ProjectCatalog implementation](../.artifacts/compiler-reuse/control/ProjectCatalog.cs) retained lazy project inputs but created a fresh workspace/compilation closure for each requested root target. This bounded residency, while offering little source-compilation sharing across overlapping roots within the same job. The current adapter carries shared contexts between active closures; the structural model below describes the original independent-root approach.

Walking the **actual target-context edges** stored in the Polly index gives:

| Structural model | Count |
| --- | --- |
| Requested root contexts | 53 |
| Context memberships summed over their independent closures | 157 |
| Distinct contexts across those closures | 53 |
| Largest individual closure | 6 contexts |
| Polly.Core net8.0 memberships | 21 |
| Polly.Core net10.0 memberships | 15 |

These are static graph counts, **not measured compilation executions, allocations, or a speedup estimate**. Roslyn can defer work and share metadata internally. The counts nevertheless identify a concrete candidate: avoid reconstructing equivalent shared dependency compilations for many roots while still refreshing all required facts, generated outputs, and diagnostics.

Simply retaining one workspace containing every target would expand compiler residency. Replacing source project references with ordinary emitted DLLs would also change the current analysis boundary and require builds that the product does not authorize automatically. A prototype must preserve the selected compiler context, aliases, embed-interop properties, original dependency provenance, and source-generation behavior. It must not confuse reused compiler work with skipping required fact extraction.

## Selected prototype and remaining decisions

Start with **job-scoped compiler reuse**: immutable compiler inputs/compilations can be shared among compatible roots during one indexing job and released at completion. Scope any retained cache narrowly; one entry or one closure is not itself a byte cap. Keep the existing Rust scheduling/publication path and the single provider contract. Do not add a second graph engine, a language-specific update planner in Rust, or one resident compiler workspace for every repository.

The user selected the job-scoped lifetime for this prototype. Retaining a compiler session across saves is a different lifecycle decision. It could make compiler-native incremental updates possible, but adds idle memory, eviction, worker/toolchain isolation, input reconciliation, cancellation, and resource-accounting obligations. No cross-save retention interval or numeric memory budget is accepted here.

Required checks for that experiment:

- Preserve all 59 complete Polly contexts, and distinguish the 53 refreshed roots from unchanged contexts. All target-specific identities and compiler diagnostics remain gates.
- Compare facts and retained source against fresh compilation for comment, implementation, declaration/constant, generator/additional-input, linked-source, and partial-error changes. Include the source-observing generator counterexample.
- Preserve original project/target dependency evidence even if compiler references are internally reused differently.
- Measure ordinary-build latency, CPU, active peak memory, and post-job release. Separate compiler work reuse from fact/publication reuse, and retain failed/noisy results.
- Check SDK hooks, target-property differences, aliases, generated documents, and saves during analysis through the existing suites. Failed or resource-limited analysis must preserve explicitly stale prior results rather than publishing an interrupted stream.

Longer-term narrowing of dependent analysis needs a versioned, provider-produced evidence contract with honest unknown/fallback behavior, handled by the common Rust update path. This investigation does not accept a particular hash, store schema, protocol revision, cache policy, or skipped-target rule.

## Validation

All work is retained under `.artifacts/invalidation/` and the existing licensed `.research/sources/roslyn/` directory. The [read-only scope script](../.artifacts/invalidation/trace-scope.py) reproduces the report against the retained index. Run the private generator probe with `dotnet run --project .artifacts/invalidation/probe/Probe.csproj -c Release`; its lockfile pins the existing Roslyn 5.9 dependency set. The original [investigation validation](../.artifacts/invalidation/validation.json) checked the counterexample, scope/closure counts, unchanged invocation-lookup artifacts, clean corpus, and installed state at that time. The later [compiler-reuse validation](../.artifacts/compiler-reuse/validation.json) records the implemented prototype and its complete product-suite reruns. Existing performance measurements remain diagnostic observations, not new guarantees.
