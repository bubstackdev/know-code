# Schema cost

What KnowCode's MCP tools cost a session's context before the agent makes any call: the tool schemas the server
advertises and its instructions. Schemas and instructions are in every turn's context, so a session pays this on
every turn.

Measured by [`scripts/eval/schema_cost.py`](../scripts/eval/schema_cost.py) with the Anthropic token-counting
endpoint (`/v1/messages/count_tokens`, which runs no model), from the schemas `knowcode serve` emits at commit
e894cb8. To reproduce:

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

Model `claude-opus-5-5`. Tokens are added to a base request of 398 tokens (short system prompt, one user message,
one client tool).

| Profile | Tools | Always loaded | Deferred | Schema bytes | Tool search off | Tool search on |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `all` | 25 | 4 | 21 | 19,842 | 8,212 | 2,282 |
| `core` | 9 | 4 | 5 | 8,126 | 3,527 | 2,028 |

Of which: server instructions 325 tokens; the always-loaded tools' schemas (`kc_context`, `kc_impact`, `kc_locate`,
`kc_refs`) 1,607 tokens.

| Tool | Always loaded | Schema bytes | Tokens on its own |
| --- | --- | ---: | ---: |
| `kc_grep_ast` |  | 1,705 | 660 |
| `kc_impact` | yes | 1,544 | 595 |
| `kc_search` |  | 1,505 | 576 |
| `kc_path` |  | 1,394 | 570 |
| `kc_trace` |  | 1,058 | 438 |
| `kc_flow` |  | 1,106 | 431 |
| `kc_context` | yes | 962 | 378 |
| `kc_refs` | yes | 950 | 359 |
| `kc_tests` |  | 792 | 313 |
| `kc_coupled` |  | 761 | 286 |
| `kc_ownership` |  | 757 | 283 |
| `kc_locate` | yes | 745 | 275 |
| `kc_hotspots` |  | 637 | 268 |
| `kc_rename_preview` |  | 660 | 267 |
| `kc_note_add` |  | 587 | 251 |
| `kc_unused` |  | 571 | 239 |
| `kc_overview` |  | 580 | 235 |
| `kc_find` |  | 575 | 234 |
| `kc_reviewers` |  | 561 | 223 |
| `kc_symbol_at` |  | 487 | 200 |
| `kc_help` |  | 444 | 193 |
| `kc_notes` |  | 382 | 165 |
| `kc_more` |  | 325 | 154 |
| `kc_status` |  | 342 | 149 |
| `kc_areas` |  | 362 | 145 |

Always-loaded sets, profile `all`, tool search on (instructions and deferred listing included):

| Loaded up front | Tools | Tokens |
| --- | ---: | ---: |
| none | 0 | 734 |
| kc_context | 1 | 1,098 |
| kc_context, kc_locate | 2 | 1,358 |
| kc_context, kc_refs, kc_locate | 3 | 1,702 |
| kc_context, kc_refs, kc_impact, kc_locate, kc_find | 5 | 2,502 |
| current: kc_context, kc_impact, kc_locate, kc_refs | 4 | 2,282 |

These are costs only. Whether a smaller set changes what agents do is for the study to measure.
