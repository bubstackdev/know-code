# KnowCode

KnowCode is a local code graph for coding agents. It indexes the git repositories on your machine and answers
questions about them over MCP and on the command line: what calls this, what breaks if I change it, how does this
request reach the database. Every answer says whether it is complete, and why not when it isn't. Every edge in it
carries a confidence and the evidence it rests on.

It is one binary with no services. It reads your working tree, makes no network calls while indexing or answering,
and writes nothing into your repositories.

## When it helps

Text search finds where a name is written. KnowCode answers questions that need the structure behind the text:

- **Who calls this, and which implementation runs?** References resolved through imports, receiver types and
  dependency-injection registrations, with ambiguous ones listed rather than guessed.
- **What depends on this change?** Dependents of a symbol, file or diff, hop by hop; the tests to run; files that
  usually change with it; who could review it.
- **How does a request get from here to there?** HTTP calls to the controller actions that serve them, message-bus
  publishers to consumers, code to the SQL tables it reads and writes, across repositories.
- **Where is the code for this?** Definitions by name, the files behind a bug report or ticket title, the symbol at
  a stack frame's `file:line`, ranked text search and ast-grep patterns.
- **What happened to this method?** The commits that changed its lines, who wrote them, and what changed with it.

It reads code syntactically, without a compiler, for the [languages listed below](#languages). It does not run or
build your code, and it doesn't cover Go, Java or Kotlin.

## Quick start

You need macOS (Apple silicon) or Linux (x86_64 or arm64), `git`, and the client you want to connect: Claude Code
(its `claude` command), Codex or Cursor.

1. Install:

   ```sh
   curl --proto '=https' --tlsv1.2 -LsSf https://github.com/bubstackdev/know-code/releases/latest/download/knowcode-installer.sh | sh
   ```

   or `brew install bubstackdev/tap/knowcode`. The shell installer puts `knowcode` in `~/.local/bin`; open a new
   terminal if your shell doesn't find it yet.

   Installed 0.3.0 to 0.5.0 with the shell installer? The repository moved to `bubstackdev/know-code`, so those
   versions show no update notice and `self-update` fails. Run the installer above once; Homebrew is unaffected.

2. Connect your agent:

   ```sh
   knowcode setup claude     # or: knowcode setup codex, knowcode setup cursor
   ```

   `setup claude` installs a Claude Code plugin: the MCP server, a session-start line, a hint on Grep, Glob and Bash
   searches, and four workflow skills. `setup codex` and `setup cursor` register the MCP server; Codex also gets a
   session-start hook, which runs once you trust it in Codex (setup says how). `--print` shows the configuration
   without changing anything.

3. Start a new session of your agent inside a repository and ask about the code, for example:

   > Where is `IRequestProvider.GetAsync` called, and which implementation runs?

   No index command or configuration is needed. The first question indexes the repository. If that takes longer
   than about two seconds, the answer comes from what is indexed so far, says it is stale, and indexing finishes in
   the background.

4. Check what KnowCode sees: `knowcode status` in the repository shows the index, its freshness, and the settings in
   effect.

For answers across repositories, tell KnowCode where your clones live. It then indexes them in the background while
your agent runs:

```sh
knowcode workspace add ~/Code
```

Update with `knowcode self-update` (`brew upgrade bubstackdev/tap/knowcode` for Homebrew). `knowcode uninstall`
removes the client registrations and indexes. [Install](docs/install.md) has the
details, and building from source is in [CONTRIBUTING.md](CONTRIBUTING.md). Windows is not supported yet.

## What your agent sees

The server advertises one tool, `kc`, whose `op` names what to run (`impact`, `find`, `refs`, `context`, `search`,
`history`, …). One small schema is cheaper to carry on every turn than many. `KNOWCODE_TOOLS=all` (or `core`), set
when you run `setup`, gives the separate `kc_*` tools instead; see [usage](docs/usage.md).

The same questions work on the command line. Here is the question above, asked of
[dotnet/eShop](https://github.com/dotnet/eShop) at commit `913acea`:

```sh
knowcode refs IRequestProvider.GetAsync -n 3
```

```text
7 references to IRequestProvider.GetAsync (exact 7)
src/ClientApp/Services/Order/OrderService.cs:54:36  call _requestProvider.GetAsync  in OrderService.GetOrdersAsync  [exact → IRequestProvider.GetAsync (src/ClientApp/Services/RequestProvider/IRequestProvider.cs:5) ⇒ RequestProvider.GetAsync via DI (src/ClientApp/Services/RequestProvider/RequestProvider.cs:21)]
src/ClientApp/Services/Order/OrderService.cs:73:40  call _requestProvider.GetAsync  in OrderService.GetOrderAsync  [exact → IRequestProvider.GetAsync (src/ClientApp/Services/RequestProvider/IRequestProvider.cs:5) ⇒ RequestProvider.GetAsync via DI (src/ClientApp/Services/RequestProvider/RequestProvider.cs:21)]
src/ClientApp/Services/Catalog/CatalogService.cs:31:46  call _requestProvider.GetAsync  in CatalogService.FilterAsync  [exact → IRequestProvider.GetAsync (src/ClientApp/Services/RequestProvider/IRequestProvider.cs:5) ⇒ RequestProvider.GetAsync via DI (src/ClientApp/Services/RequestProvider/RequestProvider.cs:21)]
… 4 more (raise --limit)
incomplete (capped): items: 3 of 7 shown; continue: kc_more {cursor: <`cursor`>}; CLI: knowcode more <`cursor`>
```

Each line is one call site: file, line and column, the method it is in, and how it resolved. Here it is `exact`, to
the interface method, and through the dependency-injection registration to the implementation that runs. The last
line says the list was cut and how to get the rest. The command line names the separate tool, `kc_more`; your agent,
on the default `kc` tool, is told `kc {op: more, cursor: …}` instead.

## Reading an answer

Every answer, from every command and tool, carries the same fields:

- **`complete`**: `true` only when nothing is missing from what KnowCode models.
- **`incomplete`**: when `complete` is false, one entry per reason (`capped`, `stale`, `missing_repo`, `withheld`,
  …), each saying what is missing and how to get it. A cut list carries a `cursor` for the next page.
- **`confidence`** on every edge: `exact`, `unique_name`, `ambiguous` (all candidates listed, none picked),
  `external_likely` or `unresolved`.
- **`evidence`** on every edge: how the target was found (an import, the receiver's type, a DI registration, an HTTP
  route match, …) and what that did not check (that a matched route is served by the same deployment, …).
- **`provenance`**: the repositories the answer read, at which commit.

An empty answer says what was searched. "Complete" means complete for what KnowCode models; [languages](docs/languages.md)
lists what it doesn't. The full contract is in [docs/answers.md](docs/answers.md), and agents read it with
`kc_help contract`.

## What has been measured

KnowCode is checked on public repositories, and every result is published with its misses.

- **Cost per turn.** The default `kc` tool and the server's instructions take 501 tokens of every turn's context,
  where 0.3's eight tools took 1,276 ([0.6.0 release gate](eval/release-gate/0.6.0.md),
  [0.4's](eval/release-gate/0.4.0.md)).
- **Identity, against a compiler.** On [identity-v1](eval/identity-v1/README.md)'s references, labelled by each
  language's toolchain, links KnowCode marks `exact` are right 0.993 of the time in C#, 0.995 in Swift and 0.998 in
  TypeScript. Some names the toolchain resolves outside the workspace are still linked inside it (C# 6, Swift 22,
  TypeScript 37), which the gate counts as missed ([0.6.0 release gate](eval/release-gate/0.6.0.md)).
- **History questions.** On [history-v1](eval/history-v1/README.md)'s 10 method-level questions, labelled from git
  alone, `history` has recall 1.0 at precision 1.0; of the plain-git approaches, `git log -L :name:` has 0.526 at 1.0
  and `git log --follow` 0.965 at 0.062. On its caller questions it matches `git log -S`'s recall at higher precision
  ([0.6.0 release gate](eval/release-gate/0.6.0.md); 0.5.0 missed one, [stated](eval/release-gate/0.5.0.md)).
- **Answers on recorded questions.** The release gate replays the 1,092 KnowCode calls agents made in the public studies
  and scores the answers against the studies' labels, with no model calls. The gate reports recall per question type
  and compares builds; it says nothing about how agents use the answers
  ([replay gate](eval/replay-gate/README.md)).
- **Agent studies.** Two pre-registered studies compared agents with and without KnowCode on pinned public
  repositories, against plain grep, find and read. In both, answers with KnowCode were about as correct as without
  it, and sessions with it used more tokens, not fewer. The most recent, [public-v2](eval/public-v2/results.md)
  (0.3.0), missed all four of its decided criteria; [public-v1](eval/public-v1/results.md) (0.2.0) met two of four.

[Evaluation](docs/evaluation.md) describes how KnowCode is checked.

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

Extraction is syntactic, so some references stay `unique_name`, `ambiguous` or `unresolved`. A SCIP index, when a
repository has one, supplies the compiler's answer. Known gaps include C# minimal-API routes. Go, Java, Kotlin and
other languages not listed are covered by text search and history only. Details are in
[docs/languages.md](docs/languages.md).

## Documentation

Using KnowCode:

- [Install](docs/install.md): installing, connecting a client, updating and uninstalling.
- [Usage](docs/usage.md): CLI commands and MCP tools, by task.
- [Answers](docs/answers.md): complete and incomplete, confidence, evidence, cursors, provenance.
- [Configuration](docs/configuration.md): `config.toml`, environment variables, workspace roots, ignore rules, the
  privacy file, the update check.
- [Languages](docs/languages.md): what is extracted and resolved per language and framework, and the known gaps.
- [Claude Code plugin](integrations/claude-code/README.md): the plugin, its hooks and skills.
- In an agent session, `kc {op: help}` (or `kc_help`) lists the reference topics.

The project:

- [Changelog](CHANGELOG.md).
- [Contributing](CONTRIBUTING.md) and [security](SECURITY.md).
- [Architecture](docs/architecture.md): crates, the index, freshness, the writer lock, the cache lifecycle.
- [Evaluation](docs/evaluation.md): gates, ground truth, and the published studies.

## License

Licensed under either [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
