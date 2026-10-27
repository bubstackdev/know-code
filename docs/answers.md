# Reading an answer

Every answer, from every CLI command and every MCP tool, says how complete it is, which repos it was read from, and
what each edge rests on. The fields are the same everywhere. A field that is absent means there is nothing to report.
The types live in `crates/kc-store/src/answer.rs` and are applied in `crates/knowcode/src/contract.rs`; a new
member of any closed set below is a contract change.

Agents get the same material through `kc_help contract`, `kc_help evidence` and `kc_help cursors`.

## complete, incomplete and more

- `complete` is `true` only when nothing makes the answer partial for what was asked, and it is always the same as
  `incomplete` being absent. Every answer says it, the CLI's `--json` and MCP answers alike; an MCP answer said as a text tree ends its
  first line with `; complete` when it is (see [MCP answers](#mcp-answers)).
- `incomplete` lists why the answer is partial, one entry per reason: `{reason, detail, hint}`. `detail` names what
  is missing (for a cut list, how many of how many were shown). `hint`, when present, says how to get the rest:
  usually the exact `kc_more` call, or a limit to raise.

| reason | meaning |
| --- | --- |
| `capped` | a limit cut a list (`--limit`, a node limit, a context section, a list's own cap) |
| `budget` | the answer was cut to fit a size budget (the MCP server's `max_chars`, or a batch that left inputs out) |
| `deadline` | a time or work budget ran out before the answer was finished (a git command or search child past its deadline, or a file ranking still being built in the background when a long list it would order was answered: the list is in index order) |
| `stale` | answered from data that doesn't match the working tree: a repo's workspace summary when the repo changed since its last sync, a pending first index, or derived data (areas, a file ranking) from an earlier index generation |
| `failed` | files or includes could not be read; what comes from them is as last indexed. Also a repo, of several the answer spans, that could not be read at all (a `git` call failing): the answer is the other repos', and the entry names the repo and the error |
| `missing_repo` | a repo the answer needed isn't cloned or indexed, or its checkout was deleted; it is named where it would have joined a cross-repo link, or where it was one of several repos the answer spans (deleted while the answer was worked out) |
| `unsupported` | a construct the analysis doesn't model, where it is known to be present |
| `policy` | the evidence policy asked for left edges out (see [Evidence policy](#evidence-policy)) |
| `withheld` | a repo the answer read has content its privacy file blocks, which the answer may have drawn on (see [configuration](configuration.md#privacy-file)) |
| `depth` | a walk (`impact`, `trace`, `path`) stopped at its depth, the one asked for or the deepest it goes, where symbols lead further; the item's `frontier` names them (see [Where a walk stops](#where-a-walk-stops)) |

`unsupported` also marks constructs an extractor recognises but doesn't read (a DI assembly scan, SQL naming its
table at run time, a publish of a message of unknown type, a Jinja-named include, a Terraform `moved` block, a CI
include or Angular lazy route in a form not read, an HTTP endpoint registration whose route can't be placed, such
as a `MapGet` template that isn't a constant or `MapControllerRoute`). The entry names them with `file:line` and says what the answer
may lack because of them; [languages.md](languages.md#constructs-recognised-but-not-read) lists them all.

Files KnowCode doesn't read as code (Razor markup, vendored or built JavaScript) are checked as text before an
answer says uses there may be missing. `refs` names the lines there that name the symbol, as an `unsupported` entry;
when no Razor file names a C# symbol, `checked` says so (`"none of 59 Razor files (.razor, .cshtml), checked as text,
names IPushRegistrationService"`) and the answer stays `complete`. `find`'s `declared` speaks for the
whole workspace, whichever repo was asked: where the name is declared, and how many references name it in every repo;
when files not read as code name it too, the count is "at least" and those lines follow. An assembly scan of an
assembly the code doesn't name is an `unsupported` entry only for a type the container knows (registered or
injected); for any other type it is said in the "DI registrations: none found" line of `none_found`. A DI construct
that can't register the type is left out of the entry and counted in that line instead ("DI constructs not read that
can't register IComponentContext: 12 in test code (a test's own container); 1 assembly scan exposing only IModule
(src/Autofac/ModuleRegistrationExtensions.cs:173)"): one in test code, for a type outside it, and, for an interface,
an assembly scan exposing only other services.

The text rendering ends with one line per entry, for example
`incomplete (capped): items: 3 of 7 shown; continue: kc_more {cursor: <`cursor`>}`.

### Codes

Every refusal and every partial answer carries a machine code, so a client never reads the English to tell them apart.
The English stays, for people.

- **An `incomplete` or `more` entry:** its code is its `reason`, above. Where the reason alone doesn't say what to do,
  the entry also has `code`: `indexing` (a `stale` repo still being indexed: ask again after it finishes),
  `not_indexed` (a `missing_repo` not indexed yet) or `gone` (a `missing_repo` whose checkout is gone).
- **A refused call** (an MCP tool error): the text is the English, and the result's `structuredContent` is
  `{"error": {"code", "message"}}`.
- **A rename preview refused:** `refused: true` with `reasons` in English and `codes` beside them, one each.
- **Privacy:** a `withheld` entry, a call refused for naming a blocked path, and a repo refused whole all carry
  `privacy`: `{repo, file, block_at, blocks}`, where `blocks` is `repo` or `paths`. It never names a blocked path.

| code | meaning |
| --- | --- |
| `invalid_argument` | a parameter is missing, of the wrong type, outside its set, or contradicts another; or the op is unknown |
| `not_found` | nothing the call names exists: no definition, file, symbol or commit by that name |
| `ambiguous` | the call names several things and nothing decides between them; the message lists them, to pick one (a rename's ambiguous sites too) |
| `no_files` | the paths or globs given match no file, so nothing was searched |
| `not_indexed` | a repo the call needs isn't indexed, or isn't a repo |
| `indexing` | a repo is being indexed: the answer is from its last index, or there is none yet; ask again after it finishes |
| `gone` | a repo's checkout is gone |
| `withheld` | an AI privacy file blocks what was asked (with `privacy`) |
| `stale_cursor` | the cursor is from before the index or the list changed, or the server no longer holds it; start again with the original call |
| `unsupported` | a construct or language the analysis doesn't model (a rename of a member the index can't see too) |
| `collision` | a rename's new name is taken where the symbol is in scope |
| `cut` | a rename preview cut to fit `max_chars`, which can't be applied as it is |
| `cancelled` | the client cancelled the call |
| `failed` | files, the index or git could not be read |
| `internal` | anything else: a fault in KnowCode, worth reporting |

The capabilities (`kc {op: capabilities}`) list the codes and the reasons.

### More is not incomplete

`complete` speaks about what was asked. A list on the side of it that a limit or `max_chars` cut is a `more` entry
instead, with the same `{reason, detail, hint}` (reason `capped` or `budget`), and the answer stays `complete`:

- a symbol's `callers`, `callees` and `members`, the tables its calls reach (`touches_tables`), the candidate
  `targets` of one reference, its `source`, the `repos` list, and strings the size cap shortened;
- cites past the first few that give their place without their line (`cite text: 12 of 30 cites quote their line`);
- a page of the answer's own `items` whose `rest` says the rows cut are past what was asked: `find`'s when every match
  with the name asked for (in any case) is listed and the rest match by prefix or word; `search`'s when every file with
  a match is named, on the page or in `other_files`, and the rest are their other lines (`rest` says when they are
  all in tests, comments and prose: a first page at the default limit shows every hit in code when there are at most
  60 hits and they fit, and leaves those a page away). Cut further to fit
  `max_chars`, the page is incomplete.

The lists an answer is about stay `incomplete` when cut: the answer's `items` (references, impact rows, search hits
beyond the files named), implementations and conformers, DI registrations, HTTP, bus and SQL links, a walk's nodes,
path candidates and deploy chains. The size cap cuts those last (see [MCP answers](#mcp-answers)), so a `budget` entry
for one of them means the others were already down to nothing. The `hint` of a `more` entry is the call for the rest,
usually `kc_more` with the list's cursor.

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

A list that answers the question (references, conformers, impact rows, trace nodes, path candidates, deploy chains)
is `capped` when a limit cuts it; a list on the side is in `more` (see [More is not incomplete](#more-is-not-incomplete)).

### Older fields

These remain and agree with `incomplete` and `more`: `truncated` and `total`, `capped` (`find`, `search`), `freshness.failed`
with `freshness.status`, a list's `<list>_total` and `<list>_truncated`, and the MCP server's `left_out` and `hint`.

## Cursors

A list that was cut and can be continued carries a cursor: `cursor` for the answer's own `items`, `<list>_cursor`
beside a nested list (`items[0].callers_cursor`). Pass it unchanged to `knowcode more <cursor>` or `kc_more`. Each
page is `{list, items, offset, total, cursor}` and has its own cursor until the list ends; its rows are the list's
own (over MCP, a search's hits are text rows there too).

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

### The provenance record

For a durable record of an answer, ask for its provenance record: `--provenance` on the command line (the JSON
envelope gains `record`), or `provenance: true` on any MCP tool call (the answer is unchanged and a second content
block holds `{"record": …}`). Compact answers in a conversation don't carry it. The record (`format` 1) holds:

- `knowcode`: the version, the commit the binary was built from, the index schema, the resolver, and every
  extractor's version;
- `call`: the call as given (the command's arguments; the tool and its arguments) and the hash of its canonical JSON;
- `answer`: the hash of the bytes returned without the record, their length, and `evidence`: the hash of the answer's
  content without what names or measures one index or one moment (row ids, cursors, generations, timings, how much a
  sync reparsed, a search's work counters, how long ago a cached value was computed). The same question asked again of
  the same code gives the same `evidence`, from another index, process or worktree;
- `repos`: for each repo the answer read, its `repo`, the full `commit` and the `generation`, `dirty` and `fresh` the
  answer read; its `worktree` (git directory, common directory and identity: the normalized origin, else the common
  directory); `uncommitted`, the number of files that differ from the commit (changed, deleted, untracked) and a digest
  of their paths, statuses and contents, leaving out the paths the privacy file blocks; `base`, the
  shared base the index was cloned from, with its `commit` and whether any file differs from it yet (`unchanged`;
  see [architecture](architecture.md#the-cache-lifecycle)); and `privacy`, the
  digests of the policy's key (what it blocks, absent when nothing is) and of the privacy file, and `block_at`.

Hashes are `blake3:<hex>`.

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
| `operation_name` | a generated API client's operation is the controller action of that name (under the controller named like the client); the API description it was generated from wasn't read |
| `server_unverified` | nothing says which server a generated client's repo calls (no base address, configuration, client package or other request of its names one): every repo with an action of the operation's name is a candidate |
| `url_local` | an HTTP request's URL is a `const` local of the function making it (`fetch(url)`, `{ url }`, `getUrl(url)`), read from its declaration (the request's `local` says which, `url at src/a.js:44`): that the name in the call is that local was read from the syntax, without a type checker |
| `client_unread` | an operation is a method of an external client the repo only declares (a `.d.ts`, `declare module 'media-apiclient'`): the client's code, where the request is made, wasn't read, so the route is matched by the method's name and the action's `…Legacy` twin is as likely (links are `ambiguous`) |
| `package_code` | a declared external client's method requests the route its package's code shows, read from the client repo's `node_modules` at the version its lockfile pins (the link's `package` names the package, version, method, where it is and the request): that the installed copy is the code that runs, and that the request read is the one made |
| `same_destination` | a message reaches the consumer on its virtual host or queue |
| `same_database` | a SQL object matched by name is in the database the code uses |
| `revision_unverified` | the dependency's checkout is the revision asked for (a Terraform `?ref=`, a CI include `ref:`, a registry version) |

`exact` is a statement about how a name was resolved, not a measured probability. A syntactic `exact` still rests on
the assumptions listed beside it.

## Cited source

Every row that gives a location quotes the source lines it rests on, read from the working tree when the answer is
built, in `cite`. That covers `trace` nodes, `impact` rows (and its changed symbols and cross-repo references),
`path` and `flow` steps, `refs` items, a `context`'s callers and callees, and the `find` items that declare the name
asked for (`matched: exact`, the first 12): the one line naming the declaration, past its doc comment and
attributes, under `cite: lines` too.

- For a row reached by an edge, `cite` quotes the edge's site: the call, the request, the publish. When that line is
  part of a statement over several lines, the rest of the statement comes with it, at most 2 lines each side: the
  arguments a call's line opens, the call a `.SingleInstance()` line continues, the condition a `? :` branch belongs
  to, the `case` label a line runs under. `at` is then the range quoted, and `cut` says where a longer statement
  starts (`the statement starts at line 33`) or that it goes on (`the statement goes on past line 70`). A line that
  stands alone, a member of an initializer in braces, and a cite that drifted or quotes no text give the one line.
  A `context`'s callees give their line alone when its `source` quotes them already. Over MCP, the size cap drops
  these statement lines, from the last row up, before it cuts any row.
- For a row reached without one (a walk's start, a path's ends, an impact's changed symbols), it quotes the
  declaration: the line naming it, or under `cite: lines` from its first line (an attribute or decorator) through
  that line (at most 3 lines).
- `cite` is `{at, check, drift, was, text, cut, repo}`. `at` is `file:line` or `file:start-end` for the lines quoted.
  `text` is those lines with their common indentation removed. `repo` is present when the lines are in another repo
  than the row's, such as a bus hop's publish in the publisher's repo. `check`, `drift` and `was` are described under
  [Checked against the index](#checked-against-the-index).
- By default (`cite: brief`) the first 12 cites, in the answer's order, quote one line each, up to 120 characters.
  The rest give `at` only, said in `more` with reason `capped`: `cite text: 12 of 30 cites quote their line; the other
  18 give only where it is`. `cite: lines` quotes every row, each line up to 160 characters.
- When a cap leaves something out, `cut` says what: `line 12 cut at 120 of 412 characters`, or (under `cite: lines`)
  `the declaration goes on past line 9`. When nothing could be quoted, `text` is absent and `cut`
  says why: the file is gone from the working tree, the line is past its end, it couldn't be read, or an [AI privacy
  file](configuration.md#privacy-file) blocks it.

### Checked against the index

Every cite is checked when the answer is built, whether or not it quotes text. The cited file is read from the working
tree and compared with the index the answer read. `check` says the result, and for anything but `verified`, `drift`
says what moved.

| check | meaning |
| --- | --- |
| `verified` | the file is byte for byte what the index read: the quoted lines are the ones the answer's facts come from |
| `changed` | the file changed since it was indexed, and the cited line still names what the row cites (the symbol a declaration declares, the name a call or reference uses), or the edge's site names nothing to look for (a request, a publish, a mapping): the place holds, but what the index drew from the file may not |
| `moved` | the file changed, and what the row cites is on another line now: `at` and `text` give the nearest line naming it, `was` where the index had it |
| `gone` | the file is gone from the working tree, or no longer names what the row cites |
| `unchecked` | the lines could not be checked: the file isn't in the index the answer read, can't be read, or an AI privacy file blocks it |

Any `changed`, `moved` or `gone` cite makes the answer incomplete with reason `stale`. The entry names up to five of
them with their check and counts the rest. A query that syncs first, as every CLI query does unless `--no-sync`,
answers with every cite `verified` unless a file changes while it runs. Drift shows up under `--no-sync`, in an MCP
answer given from the committed snapshot while indexing finishes, and in repos read from their last sync.

The text trees of `kc_trace`, `kc_impact`, `kc_path` and `kc_flow`, and the CLI's text, print the quoted lines under
each row, numbered (`   4 | this.log.info('placing');`), the rest of a site's statement marked `-` (`   3 - await
this.http`), and a `! moved: …` line under any that drifted. A `context`'s callers used to carry their line as `code`;
they now quote it in `cite`, and `code` remains only under `cite: none`.

Over MCP, a cite leaves out `at` when it is the place the row already gives (its `site`, else its own `file` and
`line`). `--cite lines` (MCP `cite: "lines"`) quotes every row in full. `--cite check` (MCP `cite: "check"`) keeps `at`,
`check`, `drift` and `was` without the text. A `context`'s own symbol gets a check-only `cite`, since `source` quotes
it. `--cite none` (MCP `cite: "none"`) leaves `cite`
out: locations only. A cursor's pages quote their rows the way the
first answer did.

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
list, is on the item's `frontier`: `{qualified, file, line, hop, reason, beyond, next}`. `reason` is `depth` (the walk
reached its depth) or `ambiguous` (only ambiguous edges reach it, and the policy lists those without walking past);
`beyond` counts the neighbours one hop past it, at least. `next` is the exact call going on from it: the same tool,
on that symbol (as `file#Name` when other definitions share its name), in its repo, with the walk's depth, direction
and filters, such as `kc_impact {"depth":3,"repo":"web","symbol":"OrderService.place"}`. Each kind makes the answer
incomplete, reason `depth` or `policy`, naming the first frontier symbols and how to go on: a deeper depth,
`--evidence all`, or the first stop's `next`. Past 200 stops the rest are counted, not checked (reason `deadline`); a walk the symbol limit cut has its
stops counted, not checked (reason `depth`), beside the `capped` entry and cursor the cut gives.

`flow` says the same of a step at its depth that leads on: the step's `stop` is `{reason, beyond, next}` (`next` a
`kc_flow` call from the step's `file:line`), and the answer is incomplete with reason `depth`. A flow its step or repo
limit cut is incomplete with reason `capped`, the hint being the call with the limits raised. A path search its node
or repo budget stopped is incomplete with reason `capped`, likewise; one that found no path within its hops has `next`,
the call looking further (up to 12 hops, then ambiguous edges too).

`trace` and `impact` read every edge through the same symbol graph as `path` and `flow` (see
[architecture](architecture.md#walks)), so a hop across a link matched by name is `unique_name` in every walk, even
from code found `exact`: an SQL object matched by its name (evidence `sql_name`), or one of several HTTP handlers or
SQL schemas preferred by a heuristic (evidence `name_dominant`).

`impact` also lists `registrations`: the DI registrations of the types on its path (`{type, role, service,
implementation, lifetime, file, line, hop}`), where the container makes or hands out a type no reference reaches.

## MCP answers

MCP answers are compact, so that rows rather than bookkeeping fill the size cap:

- repos are named by their short name (the full path only when two indexed repos share a name);
- `complete` is always said, `true` included, so a complete answer says it is complete; a text tree (kc_trace,
  kc_impact, kc_path, kc_flow) says it at the end of its first line (`repo shop @ 0123456; complete`);
- what goes without saying is left out: `false` flags (`dirty`, `truncated`, `capped`), null fields, a first page's
  `offset: 0`, a search's `stats`, and its `matches` when `total` says the same;
- a clean, current answer leaves out `repos` and `provenance`: no repo it read is dirty or was answered from its last
  sync, and nothing is `stale`, `failed` or `missing_repo`. Otherwise `provenance` stays: commits are 12 hex digits,
  the queried repo's own entry is left out (the envelope's `repo`, `commit` and `dirty` say it), and the others are
  one string each (`billing @ 0123456789ab (dirty), from its last sync`); `repos` keeps only the repos whose
  `freshness` says files could not be read;
- `stale` is one `incomplete` entry, repos named by their short name, entries saying the same of several repos joined
  (`shop, billing: indexing is queued or running; …`);
- `kc_search` and `kc_grep_ast` hits, in the answer and in a `kc_more` page of them, are text rows rather than
  objects. The location comes first (`repo/` before the file when the answer spans repos) and the line's text last,
  after the first ` | `:
  - a search hit: `repo/file:line kind symbol | text`, where kind is `def`, `decl`, `code`, `test`, `comment` or `doc`,
    and the symbol is the one a `def` or `decl` declares, or `in X` for the one containing the line, with its
    parameter names on the page's first code hit inside a method or function (`in Orders.Place(order, user)`, from
    the index; a test's, and those the hit's line already shows, are left out); `[stale]` before the ` | ` marks a file
    that could not be read. The lines the hit's statement goes on over follow it, one per line (`  41- text`), at most
    two each side and only when there are any: before a line continuing a chained call (`.fetch(…)`), the lines
    back to the call; after a line a chained call continues, the continuation; before the first line under a `case`
    or `default` label, the label. With `context`, the lines around the hit instead;
  - with `files_only`: `repo/file:line best (n matches)`, the line and kind of the file's best-ranked hit;
  - a structural match: `repo/file:line:column[-end_line] [$X=captured, …] | text`.
- `evidence` is a string, cursors are short handles, and row ids, the build stamp and timings (`ms`, `*_ms`) are left
  out;
- a reference's `from` is the enclosing symbol's qualified name (`Checkout.Run`, what `kc_context` takes), and its
  column is left out; what every row of a list of references shares (the same `targets`, `from` or `name`) is said
  once beside the list, as `<list>_targets`, `<list>_from` or `<list>_name` (plain `targets`, `from`, `name` for
  `items`);
- a test file's ranking `signals` are left out (its `confidence` and `reasons` say them);
- in a text tree, a use site in the listed symbol's own file is `at line N`, and is left out on the symbol's own line;
- a repo's deploy answer is given once, at `items[0]` (the CLI's `--json` also repeats it as `deploy`). The size cap
  cuts its long tails (`ci_includes`, `ci_skipped`, `ci_templates`, `own`) before the deploy chains, and cuts a chain
  or an include whole, never inside.

Answers are capped at `max_chars` characters (default 8000, at most 40000). A list cut to fit gets a `budget`
entry with its `kc_more` call, in `more` for a list on the side. The lists an answer is about (implementations and conformers, DI registrations, HTTP,
bus and SQL links, a search's `other_files`) are cut last: every other list (callers, callees, members) goes down to
none, the source is shortened and those lists' rows lose their detail (signature, doc) first. Links are text rows
(`file:line kind symbol`), with what all of them share said once beside the list (`links_repo`, `links_confidence`,
`links_assumptions`). `kc_trace`, `kc_impact`, `kc_path` and `kc_flow` answer as indented text trees
unless `format` is `json`. `kc_context`, `kc_refs` and `kc_locate` take several inputs (`symbols`, `names`,
`queries`, at most 20) and answer `{"results": [...]}`; inputs that don't fit are listed in `left_out` with a hint.

Default list sizes are small, so an answer leaves room for the next one: `kc_refs` 20 references, `kc_trace` 25
nodes, `kc_impact` 30 dependents, `kc_flow` 40 steps, `kc_unused` 25 findings; `kc_context` a 1200-token bundle
(capped at 6000 characters); `kc_overview` a 400-token map. A list cut at its default says so in `incomplete`, with
its cursor or the parameter to raise.

The CLI's `--json` output is the full envelope.

## How to use an answer

- Check `complete`. If it is `false`, read each `incomplete` entry before drawing a conclusion; if it is `true`, the
  answer is the whole of what was asked within the stated scope. `more` names lists beside it that were cut, with the
  call for the rest: follow it when the question is about one of them.
- Follow `hint`s and cursors when the rest of a list matters to the question. A first page is not the whole list.
- Treat `ambiguous` and `unique_name` edges as leads to confirm in the source, and read the assumptions on `exact`
  ones that cross repos.
- `stale` means the answer may lag the working tree; ask again after indexing finishes.
- `withheld` means content was blocked on purpose. Don't try to read it another way.
