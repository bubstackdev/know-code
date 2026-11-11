# Compact symbol context

The user accepted this slice after the first [agent-use probe](DOGFOODING.md): make one shared query sufficient to inspect a known symbol and stop repeating full workspace diagnostics/options on ordinary reads. Implementation uses the existing Rust operators, SQLite facts and native JSON contract. No second MCP query engine, source parser, database schema or worker protocol version is introduced.

## Reading a symbol

After `search_symbols`, use MCP `symbol_context` or a native/CLI request:

```json
{"op":"symbol_context","symbol_id":"opaque-id-from-search","max_bytes":16384}
```

`contexts` retains separate compilation/target variants. Each entry contains the symbol/signature and bounded `declarations`. Each declaration includes its name location, 1-based indexed-source `line`, optional compiler declaration `extent`, optional `source`, and `source_status`. Extents include executable bodies where the compiler supplies them; an overload signature, field initializer, interface or type declaration is still a declaration, not a claim of runtime execution. Multiple overloads/partial declarations remain separate. Symbol locations move into `declarations` in this projection rather than repeating the same locations twice.

Optional `target` and `compilation_id` select a context. `include_source:false` returns extents without source payloads. `limit` is 1–1,000 and bounds selected logical contexts and logical declaration slots across them. Equivalent supporting targets each retain the same first declaration slots; their concrete declaration evidence does not spend extra logical slots. The existing shared symbol projection also bounds locations to 16 per context. Omitted rows set `truncated`. `max_bytes` defaults to 16 KiB and is capped at 256 KiB; it bounds **aggregate returned source bytes across declarations**, including representative and supporting-target copies, not each declaration independently or the complete JSON envelope. Required metadata is additional. There is no promise to include the whole body inside a small budget.

`source_status` distinguishes `included`, `truncated`, `omitted`, `budget_exhausted`, `extent_unavailable` and `extent_ambiguous`. A budget can be too small for the next complete UTF-8 character; context then returns an empty excerpt with `budget_exhausted` rather than invalid text. Unknown symbols/absent target scopes return an error. External symbols without local declaration occurrences report `external_or_other_compilation_body`; this operation does not yet follow the graph engine's verified cross-project definition pointers. No source is guessed or fetched from current disk as fallback.

Coordinates remain UTF-8 bytes, end-exclusive, tied to the returned indexed hash. Source excerpts include actual start/end bytes, full artifact byte length and 1-based `start_line`. Line counting uses LF, CRLF or CR line breaks, streams indexed content through an 8 KiB buffer and handles embedded NULs; it neither loads the entire source into Rust nor chooses a column encoding. Line numbers describe the served generation, including stale fallback, not unverified current disk text.

## Compiler extent evidence

Adapters add occurrence role `definition` for the full compiler declaration extent, retaining role `declaration` for the existing name anchor. This is an additive fact value in the existing logical occurrence family; store v6, worker/control v7 and native ABI v1 are unchanged. Matching requires the same symbol, compilation, artifact/hash and an extent containing that declaration. More than one containing extent is explicitly ambiguous; there is no arbitrary branch selection.

TypeScript uses the selected declaration AST node; Roslyn uses the declared syntax node (including existing synthetic initializer/lambda/accessor nodes). Rust's experimental adapter requests the analyzer's hierarchical document outline once per file and matches a unique selection range to its full range. It adds no handwritten Rust parser or call-graph claim. Swift's current IndexStore adapter has no full extents and reports `extent_unavailable`. These remain partial provider capabilities. Availability is reported per declaration, not through a new v7 report capability key: the current writer validates a fixed set of report capability names, so adding a name would require an explicit protocol/coexistence change.

Existing facts stay readable without reindexing, with explicit unavailable extents. Reanalysis is needed to obtain the new rows; a coordinator binary update alone does not manufacture compiler evidence. Historical lookup/refactoring continuity and expansion of Swift/Rust release scope are separate work.

## Compact coverage and expansion

Ordinary queries default to `coverage_detail:"summary"`. They preserve freshness, generation, coordination, target/compilation identity, compiler identity, inferred/configuration status, analysis status, capabilities and diagnostic severity counts. These counts describe retained report records, not an exhaustive compiler diagnostic total; provider truncation/partial analysis still applies. They omit diagnostic messages/options and mark `details_omitted:true`. Symbol/context/reference/source results return relevant compilation reports, with `coverage_scope:"relevant"`. Even a zero-hit search retains `workspace_coverage`: workspace compilation count, status counts and diagnostic severity counts. This prevents an empty result from implying a complete workspace. Graph reads retain compact reports for all examined workspace capability contexts; reducing that evidence further is separate work.

MCP `coverage_details` maps to the same core operation:

```json
{"op":"coverage","snapshot":"opaque-token-from-result","compilation_id":"optional-compilation-from-result","max_bytes":16384}
```

`snapshot` guards managed-index expansion. Optional `generation` must agree with it; managed generation-only requests fail explicitly. If a different generation or physical index incarnation is now served, restart the original lookup; the operation does not silently expand a newer publication or promise retained history. With no compilation selector, reports are ordered by compilation ID and bounded by `limit`. Unknown compilation IDs fail explicitly. `max_bytes` bounds the serialized option/diagnostic payloads together (16 KiB default, 256 KiB maximum); required report metadata/counts and JSON framing are additional. Oversized options or diagnostics are omitted with `details_omitted` and `truncated`; counts survive. Increase the budget or narrow the compilation when needed. A single diagnostic larger than the maximum remains unavailable through this projection.

Native and MCP queries may also request `coverage_detail:"full"` on the original operation, which expands workspace reports under the separate `detail_max_bytes` budget, using the same defaults/maximum. This keeps graph/source budgets independent of detail payloads. Forward the original result’s opaque `snapshot` for guarded follow-up queries, including context reads. It detects publication or index-incarnation changes, not every unobserved concurrent disk save; ordinary freshness semantics remain in effect. Separate requests do not pin a generation; a portable `cursor` separately retains its generation for bounded reads until expiry or invalidation. See [recovery and snapshot guards](INDEX_RECOVERY.md#snapshot-guards).

## Verification and measurements

Core regressions cover bounded aggregate Unicode source, exact hashes/coordinates and line counting with NULs, target selection, partial reports, generated origin, missing/ambiguous extents, external boundaries, zero-hit workspace coverage, bounded diagnostics/options and pinned readers across publication. Real MCP/native parity covers TypeScript bodies/overloads, C# methods/partial types, saved-source invalidation and generation-bound detail expansion. The Roslyn generator integration checks context from compiler-generated source too.

Debug and Release builds pass. Both profiles pass 42 Rust workspace tests and the expanded MCP/native parity suite; formatting and warning-free Clippy pass. The required Debug suite passed through SwiftPM; its Xcode inventory assertion was updated to explicitly request full coverage, then Xcode and the final four-language integration passed. Final Rust/MCP checks were rerun after the selected-compilation and source-line fixes. Evidence, including the original stopped run and successful final checks, is under `.artifacts/context/1794240471405543000/` and `.artifacts/context/verify-final.sh`.

Actual model-visible tools were reloaded in this conversation. Matched requests before/after used the same current generation 95. Counts are UTF-8 serialized payload bytes, not measured model tokens:

| Request | Core before | Core after | MCP before | MCP after |
| --- | ---: | ---: | ---: | ---: |
| `attach_workspace` lookup | 14,635 | 1,720 | 30,927 | 3,676 |
| Same 16-byte identifier excerpt | 14,290 | 1,392 | 30,211 | 2,996 |
| Rust callers with the same explicit unavailable boundary | 14,781 | 5,734 | 31,235 | 12,171 |

Relevant coverage alone fell from 13,574 to 498 bytes on lookup/source. New symbol context returned the complete 796-byte Rust declaration at indexed line 907 in one request: 3,259 core bytes / 6,857 MCP bytes. The compiler/index remains honestly partial for Rust; this is not a new call-graph capability. Core and MCP serialization are counted separately because MCP carries compatible text and structured content.

The development LaunchAgent was reloaded with its plist unchanged. Registered reference corpora were reanalyzed for the additive extents: ts-pattern generation 34 and Polly generation 12. Actual function bodies (`src/match.ts`, 157 bytes; `RetryEngine.Implementation`, 2,985 bytes) match across CLI, Node and .NET with current freshness and no truncation. The earlier Polly CLI refresh wait expired while the coordinator continued; verification resumed against that same job rather than forcing another refresh. All three installed workspaces were idle/error-free at closeout.

One corpus lookup also exposed a useful boundary: the first `match` search hit was an import alias (`AliasExcludes` in the compiler's existing flag display), whose compiler declaration extent was only five bytes. That is valid declaration context, not a function body. Verification selected the actual function by source path. Better scoped search/stable symbol kinds and optional verified alias/definition following remain follow-on work; never substitute a guessed body.

Full envelope byte/time limits, SQL cancellation and cursor lifetimes remain release work; a source/detail budget is not a total response cap. The accepted real-encoding gate is still pending, and this slice introduces no latency/token guarantee.
