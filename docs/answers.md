# Reading an answer

Every answer, from every CLI command and every MCP tool, says how complete it is, which repos it was read from, and
what each edge rests on. The fields are the same everywhere. A field that is absent means there is nothing to report.
The types live in `crates/kc-store/src/answer.rs` and are applied in `crates/knowcode/src/contract.rs`; a new
member of any closed set below is a contract change.

Agents get the same material through `kc_help contract`, `kc_help evidence` and `kc_help cursors`.

## complete and incomplete

- `complete` is always present. It is `true` only when nothing makes the answer partial, and it is always the
  same as `incomplete` being absent.
- `incomplete` lists why the answer is partial, one entry per reason: `{reason, detail, hint}`. `detail` names what
  is missing (for a cut list, how many of how many were shown). `hint`, when present, says how to get the rest:
  usually the exact `kc_more` call, or a limit to raise.

| reason | meaning |
| --- | --- |
| `capped` | a limit cut a list (`--limit`, a node limit, a context section, a list's own cap) |
| `budget` | the answer was cut to fit a size budget (the MCP server's `max_chars`, or a batch that left inputs out) |
| `deadline` | a time or work budget ran out before the answer was finished (a git command or search child past its deadline) |
| `stale` | answered from data that doesn't match the working tree: a repo's workspace summary when the repo changed since its last sync, a pending first index, or derived data (areas, a file ranking) from an earlier index generation |
| `failed` | files or includes could not be read; what comes from them is as last indexed |
| `missing_repo` | a repo the answer needed isn't cloned or indexed, or its checkout was deleted; it is named where it would have joined a cross-repo link |
| `unsupported` | a construct the analysis doesn't model, where it is known to be present |
| `policy` | the evidence policy asked for left edges out (see [Evidence policy](#evidence-policy)) |
| `withheld` | a repo the answer read has content its privacy file blocks, which the answer may have drawn on (see [configuration](configuration.md#privacy-file)) |
| `depth` | a walk (`impact`, `trace`, `path`) stopped at its depth, the one asked for or the deepest it goes, where symbols lead further; the item's `frontier` names them (see [Where a walk stops](#where-a-walk-stops)) |

`unsupported` also marks constructs an extractor recognises but doesn't read (a DI assembly scan, SQL naming its
table at run time, a publish of a message of unknown type, a Jinja-named include, a Terraform `moved` block, a CI
include or Angular lazy route in a form not read, an HTTP endpoint registration whose route can't be placed, such
as a `MapGet` template that isn't a constant or `MapControllerRoute`). The entry names them with `file:line` and says what the answer
may lack because of them; [languages.md](languages.md#constructs-recognised-but-not-read) lists them all.

The text rendering ends with one line per entry, for example
`incomplete (capped): items: 3 of 7 shown; continue: kc_more {cursor: <`cursor`>}`.

An empty answer is not a claim of absence on its own: it carries a `note` saying what was searched. Read
`incomplete` before concluding that nothing exists. A `complete` answer is complete with respect to what KnowCode
models; [languages.md](languages.md) lists what it does not model.

### Sampled is not incomplete

Some lists are summaries by design: overview's entry points, key types, hotspots and tables; areas' key files,
entry points, exposed files and dependencies; locate's symbols per file; contributors; the ranked map. They show the
top few with a `<list>_total`, and the answer names them in `sampled` (`["key_types", "top_hotspots"]`). Such an
answer stays `complete`. The same holds for `tests`: its tight list is the most likely relevant of `tests_total`
candidates and the rest are in `left_out`, which may also fail. Nothing in a `tests` answer says a test is safe to
skip.

A list that answers the question (references, callers, conformers, impact rows, trace nodes, path candidates, deploy
chains) is `capped` when a limit cuts it.

### Older fields

These remain and agree with `incomplete`: `truncated` and `total`, `capped` (`find`, `search`), `freshness.failed`
with `freshness.status`, a list's `<list>_total` and `<list>_truncated`, and the MCP server's `left_out` and `hint`.

## Cursors

A list that was cut and can be continued carries a cursor: `cursor` for the answer's own `items`, `<list>_cursor`
beside a nested list (`items[0].callers_cursor`). Pass it unchanged to `knowcode more <cursor>` or `kc_more`. Each
page is `{list, items, offset, total, cursor}` and has its own cursor until the list ends.

- Following every cursor gives each row of the full list exactly once. For a walk's ranked list (impact's
  `affected`, a path search's `paths`), the rows shown first come first, then the rest of the walk, ranked the
  same way.
- A cursor records the request, the index generation and HEAD of every repo the answer read, and a hash of the rows
  delivered. If any of these changed, it is refused with `stale cursor: …; start again with the original call`.
- Over MCP a cursor is a short handle (`kc~1f`) that the server maps to the full cursor, for its last 4096 cursors.
- Lists that continue: references, a context's callers, callees, members, conformers and table accessors, a
  trace's nodes, impact's `affected`, `coupled` and `cross_repo`, every list of a repo's deploy answer
  (`deploy_chains`, `deployed_by`, `own`, `ci_templates`, `ci_includes`, `ci_skipped`), a path search's paths and
  candidate ends, rename sites, and `search` and `grep-ast` hits.
- `search` and `grep-ast` also page with `--offset` and `next_offset`.
- A rename preview cut by the size cap is `refused`: a partial preview can't be applied as it is.

## provenance

`provenance` lists every repo the answer consulted: the ones it answers for first, then dependencies and dependents
it opened, repos crossed over HTTP, the message bus, SQL or Terraform, and repos whose workspace summary supplied an
item. Each entry is `{repo, commit, generation, dirty, fresh}`:

- `generation` is the index generation read (`summary:<generation>` for a repo answered from its summary);
- `dirty` says whether tracked files differ from `commit` (`null` for a repo consulted on the side);
- `fresh` says whether the index was checked against the working tree for this answer and matched it. Under
  `--no-sync` nothing is fresh.

The text rendering prints an `also consulted:` line for other repos, marking those read `from its last sync`.

Every JSON answer also carries `knowcode`: the version, the commit the binary was built from, and the index schema.

## Confidence and evidence

Every reference, trace or impact row, and path or flow step carries a `confidence`:

| confidence | meaning |
| --- | --- |
| `exact` | resolved through scope, type, an import or a compiler index |
| `unique_name` | the one definition of the name in scope; a heuristic preference among several HTTP handlers or SQL schemas is also labelled `unique_name`, with evidence `name_dominant` |
| `ambiguous` | several candidates, all listed, none picked |
| `external_likely` | probably outside the indexed code (a standard-library member, an unindexed module) |
| `unresolved` | nothing indexed is reachable |

Beside it, `evidence` says how the target was found (`mechanism`) and what that did not check (`assumptions`). Over
MCP it is one string: `"receiver_type [project_unverified]"`.

| mechanism | the target was found by |
| --- | --- |
| `scip` | a SCIP index (the compiler's answer) |
| `import` | the import naming its module (TypeScript, Python, a dependency's package, an Ansible requirement, a Terraform module source, a CFML include) |
| `receiver_type` | the receiver's static type, the caller's own type (`this`, `self`, `base`), or a type named as the receiver |
| `lexical_scope` | the caller's own type or file declaring the name |
| `namespace` | the namespaces the file sees narrowing same-named types to one (C#) |
| `project_scope` | the caller's project references ruling the others out (C#) |
| `name_unique` | the one definition of the name in scope |
| `name_dominant` | one of several definitions preferred by a heuristic |
| `name_ambiguous` | several definitions, nothing deciding between them |
| `di_binding` | a dependency-injection registration, or an injected factory |
| `http_route` | the route a client requests matching the one an action serves |
| `bus_message` | a message type published on one side and consumed on the other |
| `sql_name` | a SQL object matched by name |
| `template` | a template naming the component (Angular selectors, dialogs) |
| `convention` | a layout convention (Ansible role directories, Terraform configuration directories, NgRx actions by name) |
| `filename_guess` | a module placed by its file name, with no config mapping the import |
| `declaration` | a relation written in a declaration: a base list, an interface member's implementation, containment |

| assumption | the edge assumes, unchecked |
| --- | --- |
| `project_unverified` | C#: the caller or target is in no project file, or past a project reference the index can't place |
| `directory_convention` | a directory layout stands in for configuration (an Ansible role or Terraform configuration by its directory, an Angular provider by the caller's directory) |
| `same_deployment` | an HTTP route matched by verb and path reaches the repo serving it |
| `configured_host` | an HTTP client reaches the repo through a host its URL, base address or settings name, which deploy or proxy configuration maps to that repo (the link's `host` says where); which environment's configuration applies, and for settings which key the request reads |
| `same_destination` | a message reaches the consumer on its virtual host or queue |
| `same_database` | a SQL object matched by name is in the database the code uses |
| `revision_unverified` | the dependency's checkout is the revision asked for (a Terraform `?ref=`, a CI include `ref:`, a registry version) |

`exact` is a statement about how a name was resolved, not a measured probability. A syntactic `exact` still rests on
the assumptions listed beside it.

## Evidence policy

`trace`, `impact`, `path`, `flow` and `tests` take `--evidence` (MCP `evidence`): which edges to follow, by what
they rest on.

- `default`: `exact` and `unique_name` edges are followed; `ambiguous` ones are listed, not followed.
- `confirmed`: only a compiler's or an explicit link: `scip`, `import`, `receiver_type`, `lexical_scope`,
  `namespace`, `project_scope`, `di_binding`, `http_route`, `bus_message`, `template`, `declaration`. No name match,
  convention, file-name guess or ambiguous edge.
- `all`: every edge, ambiguous ones followed too. This is the default for `tests`.
- A list of mechanisms (`scip,receiver_type`) follows only those. A preset can be adjusted
  (`confirmed,name_unique`, `all,-filename_guess`), and `-<assumption>` drops the edges making it
  (`default,-same_deployment`).

A stricter policy never lists an edge a looser one doesn't. Under any policy but `default` the answer has
`policy: {evidence, excluded, excluded_by}`, counting the edges the walk met and left out. The count is a lower
bound, since nothing past them was read, and any exclusion makes the answer incomplete with reason `policy`.

Ambiguous edges stay leaves under `default` because walking past them mostly walks wrong edges: against a compiler's
index (scip-dotnet), an ambiguous C# reference usually lists the right target among its candidates, but lists many,
so per candidate edge only a few percent are the call the compiler binds, and that compounds at every hop. The walk
lists them and names what lies past them instead (see below); `all` walks past them, marked `ambiguous`.

## Where a walk stops

A walk never ends short without saying so. `impact` goes up to 16 hops, its budget being the symbol limit
(`--limit`); `trace` goes up to 3 and `path` up to 12. Asked deeper, the answer has `depth: {asked, walked}`.

A symbol the walk listed but didn't walk past, that has dependents (for `trace`, neighbours) the answer doesn't
list, is on the item's `frontier`: `{qualified, file, line, hop, reason, beyond}`. `reason` is `depth` (the walk
reached its depth) or `ambiguous` (only ambiguous edges reach it, and the policy lists those without walking past);
`beyond` counts the neighbours one hop past it, at least. Each kind makes the answer incomplete, reason `depth` or
`policy`, naming the first frontier symbols and how to go on: a deeper depth, `--evidence all`, or a walk from one
of them. Past 200 stops the rest are counted, not checked (reason `deadline`); a walk the symbol limit cut has its
stops counted, not checked (reason `depth`), beside the `capped` entry and cursor the cut gives.

`impact` also lists `registrations`: the DI registrations of the types on its path (`{type, role, service,
implementation, lifetime, file, line, hop}`), where the container makes or hands out a type no reference reaches.

## MCP answers

MCP answers are compact, so that rows rather than bookkeeping fill the size cap:

- repos are named by their short name (the full path only when two indexed repos share a name);
- commits are 12 hex digits; the queried repo's own `provenance` entry is left out (the envelope's `repo`, `commit`
  and `dirty` say it), and the others are one string each (`billing @ 0123456789ab, from its last sync`);
- `evidence` is a string, cursors are short handles, and row ids, the build stamp and timings (`ms`, `*_ms`, a
  search's `stats` timings) are left out;
- a repo's deploy answer is given once, at `items[0]` (the CLI's `--json` also repeats it as `deploy`). The size cap
  cuts its long tails (`ci_includes`, `ci_skipped`, `ci_templates`, `own`) before the deploy chains, and cuts a chain
  or an include whole, never inside.

Answers are capped at `max_chars` characters (default 8000, at most 40000). A list cut to fit gets a `budget`
entry with its `kc_more` call. `kc_trace`, `kc_impact`, `kc_path` and `kc_flow` answer as indented text trees
unless `format` is `json`. `kc_context`, `kc_refs` and `kc_locate` take several inputs (`symbols`, `names`,
`queries`, at most 20) and answer `{"results": [...]}`; inputs that don't fit are listed in `left_out` with a hint.

The CLI's `--json` output is the full envelope.

## How to use an answer

- Check `complete`. If it is `false`, read each `incomplete` entry before drawing a conclusion.
- Follow `hint`s and cursors when the rest of a list matters to the question. A first page is not the whole list.
- Treat `ambiguous` and `unique_name` edges as leads to confirm in the source, and read the assumptions on `exact`
  ones that cross repos.
- `stale` means the answer may lag the working tree; ask again after indexing finishes.
- `withheld` means content was blocked on purpose. Don't try to read it another way.
