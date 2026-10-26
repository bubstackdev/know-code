# KnowCode

KnowCode is a local code graph for coding agents. It indexes the git repositories in your workspace and answers
questions about them (what calls this, what breaks if I change it, how does this request reach the database) over
MCP and on the command line. Every answer says whether it is complete, and why not when it isn't, and every edge in
it carries a confidence and the evidence it rests on.

It is one binary with no services. It runs on your machine, reads your working tree, and writes nothing into your
repositories.

## What it is for

- **Finding code**: definitions by name, the files behind a bug report or ticket title, the symbol at a stack
  frame's `file:line`, ranked text search, and structural search with ast-grep patterns.
- **Understanding a symbol**: its source, members, callers and callees in one call; every reference to it; the call
  tree around it; how one symbol reaches another; what happens end to end from a route or action.
- **Following code across repos**: HTTP requests to the controller actions that serve them, message-bus publishers to
  consumers, code to the SQL tables and procedures it uses, dependency-injection bindings, Terraform modules and
  Ansible roles, and how a repo is deployed by its CI pipeline.
- **Preparing a change**: what depends on a symbol, file or diff; the tests to run; files that usually change with
  it; who could review it; dead-code candidates and rename previews.
- **History**: files that keep changing and attracting fixes, historical contribution, and functional areas.
- **Notes**: short notes anchored to symbols, which go stale when the code changes.

## Install

macOS (Apple silicon) or Linux (x86_64 or arm64):

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/bubstackdev/knowcode/releases/latest/download/knowcode-installer.sh | sh
```

Or with Homebrew:

```sh
brew install bubstackdev/tap/knowcode
```

Update with `knowcode self-update`, or `brew upgrade bubstackdev/tap/knowcode` for a Homebrew install.

From source, with a Rust toolchain: `cargo build --release --locked`, then put `target/release/knowcode` on your
`PATH`. Windows is not supported yet.

## Connect your agent

```sh
knowcode setup claude     # or: knowcode setup codex, knowcode setup cursor
```

`setup claude` installs the bundled Claude Code plugin: the MCP server, a Grep/Glob hook and four workflow skills.
`setup codex` and `setup cursor` add the MCP server to the client's user settings; `setup codex` also adds a
session-start hook, which Codex runs once you trust it (setup says how, and `knowcode status` says whether you have).
`--print` shows the configuration without changing
anything. Start a new client session afterwards.

The server advertises one tool, `kc`, whose `op` names what to run (`impact`, `find`, `refs`, `context`, `search`, …,
or any other KnowCode tool), so a session pays for one short schema on every turn. `KNOWCODE_TOOLS=all` (or `core`),
set when you run `setup`, gives the separate `kc_*` tools instead; see [usage](docs/usage.md).

No index command or configuration is needed. The first query indexes the repository. If indexing takes longer than
about two seconds, the answer comes from what is indexed so far, says it is stale, and the rest is finished in the
background.

## A first query

In your agent, inside a repository, ask a question about the code:

> Where is `IRequestProvider.GetAsync` called, and which implementation runs?

The agent calls `kc {op: refs}` (`kc_refs` under `KNOWCODE_TOOLS=all`). On the command line the same question is:

```sh
knowcode refs IRequestProvider.GetAsync -n 3
```

## What an answer looks like

Here is that command run on [dotnet/eShop](https://github.com/dotnet/eShop) at commit `913acea`:

```text
7 references to IRequestProvider.GetAsync (exact 7)
src/ClientApp/Services/Order/OrderService.cs:54:36  call _requestProvider.GetAsync  in OrderService.GetOrdersAsync  [exact → IRequestProvider.GetAsync (src/ClientApp/Services/RequestProvider/IRequestProvider.cs:5) ⇒ RequestProvider.GetAsync via DI (src/ClientApp/Services/RequestProvider/RequestProvider.cs:21)]
src/ClientApp/Services/Order/OrderService.cs:73:40  call _requestProvider.GetAsync  in OrderService.GetOrderAsync  [exact → IRequestProvider.GetAsync (src/ClientApp/Services/RequestProvider/IRequestProvider.cs:5) ⇒ RequestProvider.GetAsync via DI (src/ClientApp/Services/RequestProvider/RequestProvider.cs:21)]
src/ClientApp/Services/Catalog/CatalogService.cs:31:46  call _requestProvider.GetAsync  in CatalogService.FilterAsync  [exact → IRequestProvider.GetAsync (src/ClientApp/Services/RequestProvider/IRequestProvider.cs:5) ⇒ RequestProvider.GetAsync via DI (src/ClientApp/Services/RequestProvider/RequestProvider.cs:21)]
… 4 more (raise --limit)
incomplete (capped): items: 3 of 7 shown; continue: kc_more {cursor: <`cursor`>}; CLI: knowcode more <`cursor`>
```

Each line is one call site with its file, line and column, the method it is in, and how it resolved: `exact`, to the
interface method, and through the dependency-injection registration to the implementation that runs. The last line
says the list was cut and how to get the rest. With `--json` (and over MCP) the same answer is structured; one item
of it:

```json
{
  "file": "src/ClientApp/Services/Order/OrderService.cs", "line": 54, "col": 36, "kind": "call",
  "confidence": "exact", "evidence": { "mechanism": "receiver_type" }, "receiver_type": "IRequestProvider",
  "targets": [{ "qualified": "IRequestProvider.GetAsync", "file": "src/ClientApp/Services/RequestProvider/IRequestProvider.cs", "line": 5 }],
  "via_di": { "qualified": "RequestProvider.GetAsync", "lifetime": "singleton", "registered": "src/ClientApp/MauiProgram.cs:78" }
}
```

and the envelope around it says `"complete": false`, with
`"incomplete": [{"reason": "capped", "detail": "items: 3 of 7 shown", "hint": "…"}]` and a `cursor`.

## The answer contract

Every answer, from every command and tool, carries the same fields:

- **`complete`**: `true` only when nothing is missing from what KnowCode models.
- **`incomplete`**: when `complete` is false, one entry per reason, with what is missing and how to get it. The
  reasons are `capped`, `budget`, `deadline`, `stale`, `failed`, `missing_repo`, `unsupported`, `policy`,
  `withheld` and `depth`. A cut list carries a `cursor` for the next page.
- **`confidence`** on every edge: `exact`, `unique_name`, `ambiguous` (all candidates listed, none picked),
  `external_likely` or `unresolved`.
- **`evidence`** on every edge: the mechanism that found the target (an import, the receiver's type, a DI
  registration, an HTTP route match, a name match, …) and the assumptions it did not check (that a matched route is
  served by the same deployment, that a SQL name is in the same database, …).
- **`provenance`**: every repo the answer read, at which commit, and whether it was checked against the working tree.

An empty answer says what was searched. A `complete` answer is complete with respect to what KnowCode models: read
[languages](docs/languages.md) for what it doesn't. The full contract is in [docs/answers.md](docs/answers.md), and
agents can read it with `kc_help contract`.

## Privacy and network

- Your code stays on your machine. Indexing and queries make no network calls, and there is no telemetry.
- Indexes live in your user cache directory, notes in your user data directory. Nothing is written into indexed
  repositories.
- Once a day KnowCode checks public GitHub release metadata for a newer version and prints a notice. It sends no
  paths, code or identifiers and never installs anything. Turn it off with
  `knowcode config set update.check false` or `KNOWCODE_UPDATE_CHECK=0`.
- A repository can mark content that AI tools must not see, in a privacy file at its root. KnowCode never indexes,
  shows or searches blocked content, and an answer that may lack it says `withheld`. See
  [configuration](docs/configuration.md#privacy-file).

## Languages

| Language | What is extracted and resolved |
| --- | --- |
| C# | Types, members, calls and uses, resolved by namespace, project references and receiver types; DI registrations; ASP.NET controller routes; Entity Framework tables; SQL in strings; MassTransit messages |
| TypeScript, JavaScript | The same, resolved through imports and tsconfig paths; Angular components, templates, routes, providers; NgRx actions and effects; HTTP client calls |
| Swift | Types, extensions, conformances, members and calls; SwiftUI, SwiftData and entry-point tags; SwiftPM and XcodeGen packages |
| Python, Rust | Definitions, calls and imports, syntactically |
| ColdFusion | Templates, components and functions, resolved by path; queries and HTTP calls |
| SQL | Tables, views, procedures and functions, with what each reads, writes and calls |
| Terraform, Ansible, GitLab CI | Blocks and modules; roles, plays and variables; jobs, includes and deploy chains |

Extraction is syntactic, with no compiler, so some references stay `unique_name`, `ambiguous` or `unresolved`; a
SCIP index, when present, supplies the compiler's answer. Known gaps include C# minimal-API
routes, and languages such as Go, Java and Kotlin are not extracted at all. Details and limits are in
[docs/languages.md](docs/languages.md).

## Documentation

- [Usage](docs/usage.md): CLI commands and MCP tools, by task.
- [Answers](docs/answers.md): complete and incomplete, confidence, evidence, cursors, provenance.
- [Configuration](docs/configuration.md): `config.toml`, environment variables, workspace roots, ignore rules, the
  privacy file, the update check.
- [Languages](docs/languages.md): what is extracted and resolved per language and framework, and the known gaps.
- [Architecture](docs/architecture.md): crates, the index, freshness, the writer lock, the cache lifecycle.
- [Evaluation](docs/evaluation.md): how KnowCode is checked.
- [Public agent study results, 0.3](eval/public-v2/results.md): the pre-registered public-v2 study, criterion by
  criterion; [0.2's study](eval/public-v1/results.md).
- [Claude Code integration](integrations/claude-code/README.md): the plugin, hook and skills.
- [Decisions](docs/decisions.md), [releasing](docs/releasing.md),
  [first-run acceptance](docs/first-run-acceptance.md) and the [changelog](CHANGELOG.md).
- In an agent session, `kc {op: help}` (or `kc_help`) lists the per-tool reference topics.

## License

Licensed under either [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
