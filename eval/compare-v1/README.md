# compare-v1: answers from three code tools, scored against a compiler and git

KnowCode, codebase-memory-mcp and Serena are asked the same questions about pinned public repositories, and their
answers are scored against labels that none of them made: each language's own toolchain for references, callers and
impact, and git for the task questions. No model is called, and no agent takes part. compare-v1 compares answers,
not agents, so it supports only the figures it publishes, and it makes no claim about how an agent would do with
any of the tools.

This file and [mapping.toml](mapping.toml) are the protocol. They were committed before any tool was run on the
set, and they don't change to fit the results. A change to the questions, the mapping or the scoring makes a new
set, compare-v2. The commit that adds them is the one the results cite as the mapping's commit.

| Release | What is published |
| --- | --- |
| 0.7 | References and impact on C#, TypeScript and Swift (identity-v1's repositories). Go and Java are included only if identity-v2's Go and Java labels are frozen by 0.7's third week. |
| 0.8 | The whole mapping: references, callers, impact, find, locate, task, co-change and cross-repository HTTP, on every language that has labels. |

## The tools

| Tool | What it is (from its own documentation) | How it is run |
| --- | --- | --- |
| KnowCode | This repository's binary | `knowcode serve --tools all`, over stdio |
| codebase-memory-mcp | One binary: tree-sitter extraction, its own type resolution ("Hybrid LSP") for ten languages, a SQLite graph | `codebase-memory-mcp` (its MCP server), over stdio |
| Serena | An MCP server over each language's language server | `serena start-mcp-server --agent-interface tools`, over stdio, with the LSP backend |

Each tool runs twice: at the version pinned in [tools.toml](tools.toml), and at its latest release when the run
starts. Both are pinned by digest, and both results are published. Only the pinned run is compared across KnowCode
releases. If the latest release is still the pinned one, that run is published once and marked as both. This is the
case for the first run (0.7): on 2026-10-29 the latest releases were codebase-memory-mcp v0.12.1 and Serena v2.1.0,
the pinned versions, so that run counts as both the pinned and the latest run.

### The container

Every tool, KnowCode included, runs in one throwaway Linux container built from [Dockerfile](Dockerfile), with its
base images pinned by digest. All three tools see the same files and run under the same CPU and memory limits. The
container gets the network while the image is built and while each tool indexes, because Serena's language
servers and codebase-memory-mcp may fetch what they need at that point. The network is cut before the first
question is asked, and the run checks that names no longer resolve. Third-party code never runs on the host. The
container and its volumes are removed when the run ends.

### Indexing

Each tool indexes each repository as its own documentation says, with its defaults:

- **KnowCode** indexes on the first query, into its own cache directory in the container.
- **codebase-memory-mcp:** `index_repository` with the repository's absolute path, which waits for the whole index.
  Its cache directory is set with `CBM_CACHE_DIR`. Its watcher is turned off (`watcher_enabled false`), because
  the tree doesn't change during a run.
- **Serena:** `serena project create --index` on the repository, with the languages it detects. The language
  servers are downloaded ahead of time (`serena download-ls-dependencies`), as Serena's documentation advises for
  restricted networks. A repository's dependencies are restored with the network up, as identity-v1's labeller
  restores them, so that a language server can resolve what the compiler resolves.

Each repository's tracked files at its pin are committed once, as one commit, into a fresh git repository in the
container, before its dependencies are restored. KnowCode indexes git repositories, and every tool reads that
same tree. No tool reads another's index or any label. Each repository is its own project, as in identity-v1.

## Questions

Every question is drawn from the toolchain enumerations alone, with a recorded seed, before any tool answers it.
The question types:

| Type | The question | Labels |
| --- | --- | --- |
| `refs` | Every reference to declaration D | The toolchain's references bound to D |
| `callers` | The declarations that call or use D directly | The declarations enclosing those references |
| `impact` | The declarations that depend on D, up to two hops | The same, joined transitively once more |
| `find` | Where is a declaration named N? | Every declaration named N that the toolchain saw |
| `locate` | Which files does this task touch? | tasks-v1: the files the fix commit changed |
| `task` | Which files and tests does this task touch? | tasks-v1: the files and tests the fix commit changed |
| `cochange` | Which files usually change with file F? | git: history-v1's `file-cochange` rule, on full clones |
| `cross_http` | Which client call sites in other repositories request route R? | 0.7's hand-labelled client-link pairs ([LABELING.md](../../scripts/eval/neutral/LABELING.md)) |

`cochange` and `cross_http` are asked from 0.8. They are in the set because codebase-memory-mcp documents a
counterpart for each: `FILE_CHANGES_WITH` edges, and cross-repository `CROSS_HTTP_CALLS` edges. KnowCode features
that no other tool documents are listed in [mapping.toml](mapping.toml) as questions compare-v1 asks of no tool.

Each question names its target the way a person reading the code would: by file, line and name for a declaration
D, and by name alone for `find`.

**Lookup.** Each tool is given D's file, line and name, and addresses D by the most precise handle its own
documentation offers. KnowCode takes `file:line` directly, and answers for the innermost definition spanning that
line, so its adapter checks that the answer's target has D's name. Serena's `find_symbol` is called with the file as
`relative_path`. codebase-memory-mcp's `search_graph` is called with the file as `file_pattern` and with the name. In both cases
the adapter takes the candidate whose range holds D's line, then uses that candidate's own name path (Serena) or
qualified name (codebase-memory-mcp).
A tool that doesn't hand back such a candidate, or whose answer is for a declaration of another name, gets
`not_found` for that question. Lookup failures are reported
per tool, apart from wrong answers. Each tool is scored twice: on every question, with `not_found` counted
apart, and on only the questions where its own lookup succeeded. Taking `file:line` directly could favour
KnowCode, and the second score shows how far it does.

### Labels

**References, callers and impact** come from identity-v1's labellers ([eval/identity-v1](../identity-v1/README.md)):
the Swift index store, Roslyn and the TypeScript compiler. Go and Java come from identity-v2's labellers, once
they exist. What counts as a reference, what is left out, and how a target is identified are identity-v1's rules,
unchanged.

identity-v1's enumerations record each reference and its target, but not the declaration the reference sits in.
Callers and impact need that. compare-v1 adds one field, `in`, to each reference row. It holds the innermost
enclosing declaration that identity-v1 counts as a declaration: a function, method, property, field, event or
enum case, or a type for a reference at type level. Initialisers count as their type, and accessors as their
property. A reference at file level has `in` null. The field is written by the same labeller run, in the same
container. Every other field of every row must equal identity-v1's: each of identity-v1's 4,943 labels must
appear unchanged in the new enumerations. The run stops if one doesn't.

- **`refs` labels:** every reference row bound to D, as `file:line`.
- **`callers` labels:** the `in` declarations of D's reference rows of kind `call`, `member`, `dispatch` or
  `extension`, as the declaration's file and the line of its name. References of kind `type` (D named in a
  signature or a type position) aren't callers.
- **`impact` labels:** hop 1 is D's callers. Hop 2 is the callers of each hop-1 declaration, by the same rule.
  The label is their union, without D itself. An edge is only what the toolchain binds. A call through an
  interface binds to the interface member, so it reaches an implementation's callers only when the interface
  member is the target. This holds for every tool alike.
- **`find` labels:** every declaration row named N.

**Task and locate labels** are tasks-v1's ([eval/tasks-v1](../tasks-v1/README.md)): the files a fix commit
changed, and its tests, at the commit's parent.

### The draw

[questions.py](../../scripts/eval/compare/questions.py) draws the questions from the enumerations. For each
repository:

- **`refs`:** targets are inside declarations with between 1 and 200 reference rows. The bound keeps one widely
  used type from outweighing the rest of a language. The number of declarations excluded by it is published. Strata
  are the declaration's kind (`type`; `callable` for a function or method; `member` for a property, field, event
  or enum case; `dispatch` for a protocol, interface or abstract member), crossed with its reference count
  (`1-4`, `5-29`, `30-200`). Each stratum takes up to 3 targets, so each repository has at most 36.
- **`callers` and `impact`:** targets are callables with at least one caller, stratified by the number of
  hop-1 callers (`1`, `2-9`, `10+`). Each stratum takes up to 4 targets, so each repository has at most 12. The
  two question types share the same targets.
- **`find`:** names with between 1 and 20 declarations, stratified by count (`1`, `2-4`, `5-20`). Each stratum
  takes up to 4 names, so each repository has at most 12.
- **`locate` and `task`:** every task in tasks-v1.

Within a stratum, targets are taken in SHA-256 order of `compare-v1:repo:key` (or `compare-v1:repo:name` for
`find`). The draw therefore depends only on the enumerations. [questions.jsonl](questions.jsonl) holds the
questions and their labels, and `strata.json` holds, per stratum, how many targets were available and how many
were drawn.

## Asking

[run.py](../../scripts/eval/compare/run.py) runs in the container. Each tool gets one MCP session per repository,
over stdio, and each question is one or more calls, exactly as [mapping.toml](mapping.toml) lists them. Answers are
normalised as follows:

- **`refs`:** every `file:line` the answer names as a reference site. A tool that names the enclosing symbol
  instead of the line names that symbol's span, and the span is matched as below. Serena marks each reference's
  line in its snippet, and those lines are the sites. codebase-memory-mcp names callers only.
- **`callers` and `impact`:** every declaration the answer names, as its file and line. A tool's line counts as a
  declaration's name line when it is the first line, from the tool's line through the next 10, that has the
  declaration's name as a word. This is identity-v1's rule.
- **`find`:** every declaration the answer names, matched the same way.
- **`locate` and `task`:** every repository-relative path the answer names, in the order it first names them.

codebase-memory-mcp's `trace_path` names each caller by qualified name only. The adapter reads each one back with
the tool's own `search_graph` to get its file and line range. That step names nothing the answer didn't name.

Serena runs with its classical tool interface, which has one MCP tool per operation, so the mapping's calls are
the tools themselves. Its v2 default, a Python REPL tool, runs the same operations.

A question has a fixed budget: 60 seconds per call, and the tool's default page of results. A cursor or
continuation is followed up to 10 pages when the tool offers one. Each answer is stored whole, with its bytes, so
that every judgement can be checked again.

## Scoring

[score.py](../../scripts/eval/compare/score.py) judges every normalised answer against its labels.

- **Recall:** the share of labels that the answer names, per question and pooled over a stratum's labels.
- **Precision:** of the items the answer names, the share that are labels. An item the toolchain has no view on is
  `unjudged`, not wrong. For `refs`, that means a line with no reference row of D's name, such as a comment, an
  import, a constructor call, or D's own declaration. Such items are counted and published, but they are left out
  of precision. An item is wrong only when the toolchain binds it elsewhere: a reference row of the same name on
  that line bound to another declaration, or, for `callers`, `impact` and `find`, a declaration that isn't a
  label.
- **Span answers** (an enclosing symbol named instead of a line, as described above): the span counts as one item.
  It is right when it holds a label, wrong when it holds a reference row of D's name bound elsewhere and no label,
  and unjudged otherwise. It is credited with every label it holds, for recall.
- **Can't answer:** a tool that has no call for a question type gets `no_counterpart` for the whole type. A
  question whose target lookup found nothing gets `not_found`. A call that errors or passes its time limit gets
  `error`. Each of these is counted apart from wrong answers and never scored as a miss, a hit or a win. Each
  `error` is kept with its log.
- **`locate` and `task`:** recall of the changed files and, separately, of the changed tests, over the whole answer
  and within the first 2,000, 4,000 and 8,000 bytes, as in tasks-v1.

Results are reported per tool, per language and per question type, with counts and 95% Wilson intervals for
pooled proportions. Per-question means are given beside them, because labels within one question aren't
independent. Every miss of every tool, KnowCode's included, is listed in the results: every label not named,
and every item judged wrong, with the tool's answer.

Some KnowCode features have no documented counterpart in the other tools: symbol history, test selection and
SQL table links. compare-v1 asks no such question of any tool. The results list them as such and never count
them as KnowCode wins. A question the set does ask, but one tool has no call for, is that tool's `no_counterpart`
row.

## 0.8's additions

Each addition is committed before any run asks it, and each is its own pull request.

1. **Callers and find**, on the questions already drawn ([questions.jsonl](questions.jsonl)).
   - Every impact question also stands as a callers question on the same target, with its hop-1 labels (`callers` in the impact row). That makes 96 callers questions.
   - The 96 find questions were drawn with the rest.
   - The calls are mapping.toml's.
   - Serena's find uses find_symbol's documented `substring_matching`, as its lookup does since the first run's audit. A generic declaration's name carries its type parameters, and names are compared without them.

2. **Locate and task**, on tasks-v1's 54 tasks ([eval/tasks-v1](../tasks-v1/README.md)), unchanged.
   - Each task is its own tree: the tracked files of its repository at the fix commit's parent, checked out as in tasks-v1 and committed once, as every tree is.
   - The task's text is the question. A locate question and a task question are asked on each tree.
   - KnowCode answers with `kc_locate` and `kc_task`, and codebase-memory-mcp with `search_graph`'s `semantic_query`. These are mapping.toml's calls.
   - codebase-memory-mcp has no counterpart for `task`, and Serena none for either type. Serena isn't started on these trees, because it would only answer `no_counterpart`.
   - Continuations are followed as for every question: KnowCode's cursor, and codebase-memory-mcp's `semantic_has_more` with `semantic_offset`.
   - Scoring is tasks-v1's. A changed file or test is named when its repository-relative path appears in the answer's text, with the pages joined in order. It counts within a budget of B bytes when its first mention ends within the first B bytes. Recall of the changed files, and separately of the changed tests, is given over the whole answer and within 2,000, 4,000 and 8,000 bytes, with counts and Wilson intervals.
   - Every changed file or test an answer doesn't name is listed with the misses.
   - These trees aren't restored. The restore exists only for Serena's language servers, and Serena isn't started here; KnowCode and codebase-memory-mcp ignore restored dependencies. The procedure changed before any answer was read: the first attempt stopped on 10 C# trees, whose parents pin a .NET 5 SDK the image doesn't have. Every tree is indexed as checked out, and the trees that had finished were run again under the same rule.

## Changes after the first run

The first run (2026-10-29, KnowCode at integration/0.7 b6c9bd5) was scored, and its two repetitions were compared. An
audit of the three adapters against this protocol then found the deviations below. They were fixed in one commit
(`compare-v1: adapters brought to the frozen protocol after the first run`), and both runs were repeated. The
mapping, the scoring and the questions didn't change. Both the first run and the rerun are published, side by
side, in results.md.

**Three of the four fixes favour KnowCode, and one favours Serena.** None changes codebase-memory-mcp's stored
answers: re-parsing its 116 `not_found` lookups (refs and impact) with the new name comparison finds none.
The last column of the table shows each fix's effect on the first run's stored answers. These are re-parsed
figures, not a rerun. A fix that needs calls the first run never made can't be measured that way, and the table
gives the size of what it touches instead.

| Favours | Deviation from the protocol | Effect on the first run's stored answers |
| --- | --- | --- |
| KnowCode | Its continuation, written `kc_more {cursor: "…"}` in impact answers, was never followed. The parser expected `{"cursor": "…"}`. ("Asking": follow up to 10 pages.) | 24 of 96 impact answers stopped at their first page. The pages that weren't fetched can't be re-parsed, so only the rerun measures this fix. |
| KnowCode | The impact parsers read a path as one word, so a path with a space (`Parsable Types/`) dropped its rows, and the name check failed on such a target. | Re-parsed: 9 impact answers change and 5 `not_found` become answers. KnowCode's impact recall on the questions it answered goes from 0.330 (657/1990) to 0.355 (712/2005), and on every question from 0.316 to 0.342. Precision goes from 0.707 (657/929) to 0.703 (713/1014). Swift recall goes from 0.210 to 0.262. |
| KnowCode | The index phase sent `kc_status`, which doesn't index. KnowCode then indexed during the first query, with the network cut. Its documented way to index now is `knowcode index`. ("Indexing": each tool indexes as its documentation says, with the network up.) | Not re-parseable. KnowCode's index was built in the ask phase, and its index phase took 0.0 s in every repository. 1 to 2 answers per repetition on jellyfin-server read a stale snapshot. |
| Serena | A generic declaration's name-path segment carries its type parameters (`Write<T0, T1, T2>`), which neither find_symbol's plain-name match nor the adapter's name comparison accepted. The lookup now uses find_symbol's documented `substring_matching`, and every adapter compares names without type parameters, overload indexes or parameter lists. ("Lookup": the most precise handle the documentation offers.) | 11 of Serena's 16 refs `not_found` results are generic C# declarations. The plain-name lookup never returned them, so re-parsing recovers none, and only the rerun measures this fix. |

Checked and left as they were:
- **codebase-memory-mcp:** its continuations (`next_cursor`) were followed in every answer, up to its stated totals. Its lookup misses are the tool's: it keeps one declaration per name and file, so the other overloads aren't there to find.
- **Serena's readiness:** Serena documents no readiness wait for its Swift or C# language servers. Its sourcekit-lsp integration sleeps a fixed 5 seconds before the first references request. No wait is added, because a sleep tuned to the results is not a documented mechanism. Serena's empty answers on some swift-nio references, in one repetition but not the other, are its own and are reported as they come.
- **Serena's size limit:** one answer was past Serena's default size limit. Serena offers no continuation, so that answer names nothing.

## Repeatability

Each published result is two complete runs in fresh containers, from the same image. The two must give identical
scores, or nothing is published until the difference is explained. The results give the mapping's commit, the
image digest, the tool versions and their digests, the run dates, and the host's CPU and memory limits. They make
no claim about speed.

## Files

| File | What it holds |
| --- | --- |
| [mapping.toml](mapping.toml) | Per question type, the call each tool's documentation recommends, or `no_counterpart` |
| [tools.toml](tools.toml) | The pinned versions, release assets and digests, and the latest-release rule |
| [Dockerfile](Dockerfile) | The container, with every base image pinned by digest |
| questions.jsonl, strata.json | The drawn questions and their labels (written by the draw, before any run) |
| results.md, results/*.misses.tsv | The published figures, and every miss of every tool per run |

The scripts are in [scripts/eval/compare](../../scripts/eval/compare): one adapter per tool, the draw, the runner,
the scorer and the report (report.py, which writes results.md's figures), using the standard library only, with
tests.

## Running it

```sh
# the images (once per KnowCode build under test); records each image's id in WORK/images.txt
PYTHON=python3.12 scripts/eval/compare/build.sh KNOWCODE_SOURCE WORK typescript csharp swift
# the pinned clones (identity-v1's), then one container per repository: index with the network up, ask without it
python3 scripts/eval/identity_gate.py fetch --work WORK
for REPO in serilog autofac jellyfin-server jellyfin-web hono swift-argument-parser swift-collections swift-nio; do
  LANGUAGE=$(awk -v r=$REPO '$1 == r {print $2}' scripts/eval/identity/repos.txt)
  scripts/eval/compare/container.sh $LANGUAGE $REPO WORK/clones/$REPO RUN/$REPO --types refs,impact
done
# the scores, on the host; a second run into RUN2 must give an identical summary
(cd scripts/eval && python3 -m compare.run score --questions ../../eval/compare-v1/questions.jsonl \
   --enumerations ENUMERATIONS --clones WORK/clones --out RUN/summary.json RUN/*/out)
```

`ENUMERATIONS` is the directory of the eight enumerations ([enumerations/README.md](enumerations/README.md)).
`summary.json` holds both scores per tool, question type and language. `summary.misses.jsonl` lists every label
each tool missed and every item judged wrong. Each repository's `out` keeps every raw answer and every tool's logs.
