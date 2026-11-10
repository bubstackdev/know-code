# Deterministic saved-result pages

`crates/core/src/cursor/pages.rs` is a crate-local codec over one already bounded result from the existing query operators. It performs no SQL, graph traversal, indexing, source-file reads, coordinator control or token-lifetime management. The integrator owns those boundaries in [CURSOR_INTEGRATION.md](CURSOR_INTEGRATION.md).

## Interface

- `encode(Value, EncodeOptions, Execution) -> Result<Vec<u8>>` consumes a bare result, without its outer `{ "result": ... }` envelope. Options contain `op: &str`, `total_limit: u32`, `page_size: u32`, `cursor_prefix: &str` and `response_max_bytes: usize`.
- `read(impl Read + Seek, ReadOptions, Execution) -> Result<Value>` returns a bare result with `page` metadata. Options contain `position: u32`, `cursor_prefix: &str`, `expected_generation: u64`, `response_max_bytes: usize`, `freshness: &str` and `coordination: Option<&WorkspaceStatus>`.
- `validate(impl Read + Seek, position, cursor_prefix, expected_generation, Execution) -> Result<()>` checks the selected token's indexed position/snapshot binding without parsing a large response header or applying an unrelated source/context response cap. It reads the fixed preamble and at most three small neighboring index entries (up to 388 bytes).

The cursor spelling appends eight lowercase hexadecimal position digits directly to the supplied prefix. The integrator includes its delimiter in that prefix and validates raw token/query scope before this codec runs. Its current `kc1` + 32-hex owner + 32-hex cursor ID + `.` prefix yields 76-byte complete tokens.

Supported initial operations are symbols, references, symbol_context, coverage, callers, callees and impact. `limit` remains the logical total cap, and `page_size` is 1–1000 whole items per page. This codec rejects a materialized primary array exceeding its declared cap rather than silently trimming it again. For graph results the cap applies to findings; all materialized boundaries remain available. Source excerpts are separate bounded reads through the pinned view, not a paginated source-string format.

## Stable projection and budgets

Symbols, occurrences, contexts and explicit coverage each have one primary array. Graph findings and boundaries are two ordered sections: findings first, then boundaries, preserving each section's original order. A finding with all its supporting paths, a complete symbol context and each boundary are atomic items. Other required fields remain shared header data, including coverage, workspace summary, root definitions, capability evidence, traversal counts, limits and the original `truncated` value.

Every returned page includes:

```json
{
  "page": {
    "cursor": "<prefix><8-hex-position>",
    "next_cursor": "<next-token-or-null>",
    "position": 0,
    "page_count": 3,
    "total_limit": 100,
    "page_size": 20,
    "sections": {
      "findings": { "total": 25, "start": 0, "count": 20 },
      "boundaries": { "total": 3, "start": 0, "count": 0 }
    }
  }
}
```

Section totals describe the materialized bounded result, not exhaustive workspace coverage. Zero boundaries on an early page do not assert that no boundaries exist. Reaching `next_cursor: null` does not clear an original logical/traversal limit or make a partial graph complete. An empty result has one valid page.

The existing bounded/deadline-aware serializer encodes each atomic item once. Prefix sums provide row-byte costs; page selection uses binary search, avoiding repeated serialization of growing row arrays. A bounded fixed-point pass accounts for decimal `page_count` width. The complete terminal tail is tested separately because a null next token can fit where a smaller nonterminal prefix cannot. Header bytes are measured once; envelope, tokens, metadata, commas and full atomic items all count toward the original response budget.

Partitions are frozen in the saved file. A later larger response cap does not add items; a smaller cap can reject the same page with `response_limit_exceeded`. Read-time freshness and coordination replace their original values, and the shared complete-response encoder checks the actual final envelope after replacement. The caller must provide accurate current/stale metadata and initial metadata suitable for the first response; the codec does not implement a second freshness engine. An oversized required header or item fails explicitly. Files exceeding the shared 16 MiB implementation guard fail as resource-limited; no evidence is silently discarded.

## Private indexed format

Version 1 uses a 136-byte little-endian preamble, one JSON header, fixed 84-byte page index entries and flat JSON arrays containing only each selected page's items. The header is stored once; large metadata is not duplicated per item or page. The preamble binds version, total bytes, generation, prefix digest, header length/digest, page/item counts and page size. Each indexed entry binds its position, byte range, logical range and payload digest to the header digest. SHA-256 checksums detect corruption; this is private local integrity, not an authentication mechanism against a user who can rewrite every checksum.

Reads bound and validate all lengths/ranges before seeking or allocating, then check adjacent entry continuity, header integrity and the selected payload. Only the header, nearby index entries and selected page are read/parsed; the graph is not reparsed in full. Cheap `validate` checks preamble/index integrity and scope, not unread header or payload bytes. File upload completion and the full page reader provide those other checks. Stored malformed/truncated/version-mismatched data or snapshot/prefix mismatch returns `cursor_invalidated`; operational I/O errors remain I/O errors, with no current-data fallback.

The shared deadline is checked during serialization, hashing, packing, chunked copies, every actual `Read` call, seeking and JSON work. A trickling reader cannot hide thousands of short reads inside one unchecked `read_exact`. A single OS call or JSON parser invocation still has cooperative, not hard real-time, interruption semantics.

## Focused validation

Inline tests exercise all primary layouts, replay with changed caps, graph finding/path/boundary preservation through terminal partial pages, exact terminal byte boundaries and short terminal tails, dynamic metadata growth, indivisible contexts/paths/headers, empty pages, thousand-page count widths, shared-header storage and bounded selected reads, checksum-valid malformed ranges/JSON/header fields, and expiry before/between reads. These are codec tests, not compiler, multiple-process, coordinator, installed-package or release certification. Evidence and exact source identity remain under the isolated worktree's `.artifacts/cursor-pages/` until integration.
