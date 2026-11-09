# First agent-use feedback

2026-11-09. The user asked what Know Code should do more or differently after connecting it to this conversation. The user subsequently accepted priorities 1 and 2 together as the compact-symbol-context slice. The other improvements remain proposals. The existing 0.0.1/0.0.2/0.0.3 commitments remain in [RELEASE_PLAN.md](RELEASE_PLAN.md).

## What I tried

Through the actual model-visible MCP tools, inspect the current workspace, find `query_json` and `attach_workspace`, read the latter's declaration range, ask for its callers, and search for freshness-related code. All successful graph/source responses were current generation 92. The root workspace was idle and available. Full local evidence is retained under `.artifacts/dogfooding/`.

This is a short usability probe of this Rust-heavy mixed-language checkout. It is not a representative language-quality evaluation, throughput benchmark, real-encoding gate, or measurement of model input tokens.

## Ranked improvements

| Priority | Friction observed | Desired behavior |
| --- | --- | --- |
| 1 | Symbol lookup supplies a signature and identifier location; reading that exact range gives only `attach_workspace`, not enough code to explain registration. Getting the body still requires another retrieval/window decision. | A bounded symbol-context operation: opaque symbol ID to signature, compiler-backed declaration/definition extent and optional source body, with useful human locations and supporting context. Keep it in the common Rust query surface, available equally to CLI/MCP/.NET/Node. |
| 2 | A 16-byte identifier excerpt produced 29,179 bytes of MCP JSON. Its core JSON was 13,806 bytes; coverage alone was 13,094 bytes. Full workspace reports repeat even for one source token. | Compact relevant coverage/freshness/target evidence by default, with bounded diagnostic detail on demand. Retain essential incompleteness and resolution warnings; a small reply must still be trustworthy. Combine this with symbol context so getting usable evidence does not multiply calls. |
| 3 | A batch of four ordinary read requests produced a `busy` error for the lexical search. The existing two-operation cap is effective, but immediate retries become agent work. | Bounded waiting/admission and responsive status reads, within caller deadlines and shared resource budgets. Evaluate a small bounded queue or status headroom before changing concurrency. Keep permits attached to actual running work after cancellation. |
| 4 | Search has name/text/target filters. A `ReadView` lookup returned a useful struct plus a `selfTypeKeyword` without source locations; freshness search returned a mixed set of properties/types/functions and truncated at five. | Language, path, project and symbol-kind filters, useful enclosing-owner labels and line/column locations. Keep locals/parameters available while default overviews focus on useful types/members/callables. These should be shared filters, not MCP-only postprocessing. |
| 5 | Rust callers correctly returned `capability_unavailable` with zero examined edges. The agent learned this after a graph call and another large coverage envelope. | A compact capability overview that makes supported/partial/unavailable operations clear for the selected language/context before exploration. Explain the boundary and useful fallback. This does not require promoting Rust call-graph implementation into 0.0.1. |

## Payload evidence

Counts below are UTF-8 bytes of serialized objects returned to the tool orchestration layer, not token counts or necessarily the representation an agent client places in model context. MCP compatibility includes both text and structured content; the core envelope is also counted separately to expose the underlying metadata cost.

| Request | MCP JSON bytes | Core JSON bytes | Coverage JSON bytes | Source bytes |
| --- | ---: | ---: | ---: | ---: |
| `query_json` symbol lookup, two matches | 30,947 | 14,653 | 13,094 | — |
| `attach_workspace` lookup, one match | 29,895 | 14,151 | 13,094 | — |
| Exact identifier source excerpt | 29,179 | 13,806 | 13,094 | 16 |
| Rust callers with explicit unavailable boundary | 30,203 | 14,297 | 13,094 | — |

Do not remove required freshness, generation, target/compilation provenance or capability evidence merely to reduce bytes. Evaluate a compact per-query summary plus explicitly expandable, generation-bound details through the same operators. Account for required metadata separately from requested source and graph budgets, and measure actual agent-visible tokens/tool calls before claiming savings.

## Proposed next slice

Prioritize **compact symbol context**: make one bounded request sufficient to identify and inspect a symbol, with concise but adequate coverage. This addresses the two largest observed costs together and fits the existing shared projection/source requirements. Exact API name/schema, body-range encoding and metadata-detail mechanics are not selected yet.

Use compiler-provided extents where available, preserving UTF-8/source-version ties and distinct declaration/use occurrences. Expose unsupported or multiple-context cases rather than guessing a body from an identifier or merging incompatible targets. Human locations must map to the indexed source; line numbers must not imply current disk text matches a stale result. Keep the existing source-hash guard and partial-language boundaries.

Acceptance should use real navigation questions, ordinary and generated source, small/large/Unicode bodies, multiple declarations/targets and saved edits between lookup/context reads. Compare tool calls and response bytes/tokens, including coverage, against this baseline. Numeric payload/latency targets remain proposals to be chosen; no new universal 4 KiB response cap or performance guarantee is accepted by this feedback.

## Accepted follow-on

The user answered “Yep” to implementing compact symbol context and smaller replies together. See [the shared contract and verification](SYMBOL_CONTEXT.md). This does not authorize admission-queue changes, richer search filters or a Rust call-graph expansion.

The accepted slice is now verified through the actual conversation tools: same-generation lookup/source replies shrink substantially and a single context call returns a full Rust function. Real TS/C# function contexts match both native bindings after corpus refresh. [Measurements and remaining alias/search boundaries](SYMBOL_CONTEXT.md#verification-and-measurements) preserve the evidence; other ranked items are still proposals.
