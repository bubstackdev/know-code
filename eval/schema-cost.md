# Schema cost

What KnowCode's MCP tools cost a session's context before the agent makes any call: the tool schemas the server
advertises and its instructions. Schemas and instructions are in every turn's context, so a session pays this on
every turn.

Measured by [`scripts/eval/schema_cost.py`](../scripts/eval/schema_cost.py) with the Anthropic token-counting
endpoint (`/v1/messages/count_tokens`, which runs no model), from the schemas `knowcode serve` emits. To reproduce:

```sh
cargo build --release
scripts/eval/schema_cost.py --bin target/release/knowcode --out table.md   # reads ANTHROPIC_API_KEY
```

How the cases are modelled:

- **Tool search off:** every advertised tool's schema is in the request, plus the server instructions.
- **Tool search on:** as Claude Code defers MCP tools, only the always-loaded tools (`_meta` `anthropic/alwaysLoad`)
  are sent as schemas; the deferred tools are named in one message, one `mcp__knowcode__<tool>` per line; plus the
  instructions. The client's own tool-search tool is the client's cost and isn't counted. Loading a deferred tool
  later adds that tool's schema (the per-tool table) from then on.
- Each figure is the difference between two counts of the same request, with and without KnowCode's part. The base
  request includes one client tool, so the fixed preamble the API adds whenever tools are present isn't charged to
  KnowCode.

Counts are for one model's tokenizer and move with any schema or instruction edit; rerun the script after either.

## Results

Each tool's schema advertises only its common parameters (`ADVERTISED` in `crates/knowcode/src/mcp.rs`), with
short lines; `repo` and `max_chars`, which the server instructions state once, and the rarer parameters are accepted
all the same and documented in `kc_help <tool>`. Tool descriptions are unchanged. Before is main at cbe7d4e, after is
the same code with that change.

Model `claude-opus-5-5`. Tokens are added to a base request of 398 tokens (short system prompt, one user message,
one client tool).

| Profile | Tools | Schema bytes, before → after | Tool search off, before → after | Tool search on, before → after |
| --- | ---: | ---: | ---: | ---: |
| `all` | 25 | 20,750 → 10,233 | 8,578 → 4,500 | 2,464 → 1,511 |
| `core` | 9 | 8,722 → 4,289 | 3,773 → 2,062 | 2,210 → 1,257 |

Of which: server instructions 333 tokens (unchanged); the always-loaded tools' schemas
(`kc_context`, `kc_impact`, `kc_locate`, `kc_refs`) 1,781 → 828 tokens.

| Tool | Always loaded | Schema bytes, before → after | Tokens on its own, before → after |
| --- | --- | ---: | ---: |
| `kc_grep_ast` |  | 1,705 → 568 | 660 → 250 |
| `kc_impact` | yes | 1,696 → 652 | 651 → 246 |
| `kc_path` |  | 1,550 → 427 | 630 → 177 |
| `kc_search` |  | 1,505 → 466 | 579 → 195 |
| `kc_trace` |  | 1,210 → 518 | 498 → 225 |
| `kc_flow` |  | 1,262 → 481 | 491 → 195 |
| `kc_context` | yes | 1,118 → 582 | 438 → 228 |
| `kc_refs` | yes | 1,094 → 460 | 419 → 177 |
| `kc_tests` |  | 792 → 456 | 313 → 180 |
| `kc_coupled` |  | 761 → 389 | 286 → 146 |
| `kc_ownership` |  | 757 → 394 | 283 → 144 |
| `kc_locate` | yes | 739 → 492 | 273 → 177 |
| `kc_hotspots` |  | 637 → 304 | 268 → 130 |
| `kc_rename_preview` |  | 660 → 429 | 267 → 178 |
| `kc_note_add` |  | 587 → 420 | 251 → 186 |
| `kc_unused` |  | 571 → 230 | 239 → 103 |
| `kc_find` |  | 573 → 398 | 235 → 164 |
| `kc_overview` |  | 580 → 239 | 235 → 103 |
| `kc_reviewers` |  | 561 → 419 | 223 → 160 |
| `kc_symbol_at` |  | 487 → 386 | 200 → 160 |
| `kc_help` |  | 444 → 412 | 193 → 179 |
| `kc_notes` |  | 382 → 294 | 165 → 127 |
| `kc_more` |  | 325 → 291 | 154 → 138 |
| `kc_status` |  | 342 → 195 | 149 → 88 |
| `kc_areas` |  | 362 → 281 | 145 → 111 |

Always-loaded sets, profile `all`, tool search on (instructions and deferred listing included):

| Loaded up front | Tools | Tokens, before → after |
| --- | ---: | ---: |
| none | 0 | 742 → 742 |
| kc_context | 1 | 1,166 → 956 |
| kc_context, kc_locate | 2 | 1,424 → 1,118 |
| kc_context, kc_refs, kc_locate | 3 | 1,828 → 1,280 |
| kc_context, kc_refs, kc_impact, kc_locate, kc_find | 5 | 2,685 → 1,661 |
| current: kc_context, kc_impact, kc_locate, kc_refs | 4 | 2,464 → 1,511 |

These are costs only. Whether they change what agents do is for the study to measure.
