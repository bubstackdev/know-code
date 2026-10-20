# Configuration

KnowCode works with no configuration. Everything here is optional.

## Where things live

| What | macOS default | Linux default | Override |
| --- | --- | --- | --- |
| `config.toml`, `bot_authors`, `former_authors` | `~/Library/Application Support/dev.bubstack.knowcode/` | `~/.config/dev.bubstack.knowcode/` | `KNOWCODE_CONFIG_DIR` |
| Indexes (one SQLite file per checkout) | `~/Library/Caches/dev.bubstack.knowcode/` | `~/.cache/dev.bubstack.knowcode/` | `cache.dir`, `KNOWCODE_CACHE_DIR` |
| Notes on symbols (`notes.sqlite`), plugin files | `~/Library/Application Support/dev.bubstack.knowcode/` | `~/.local/share/dev.bubstack.knowcode/` | `notes.path`, `KNOWCODE_DATA_DIR` |
| The shell installer's binary | `~/.local/bin/knowcode` | `~/.local/bin/knowcode` | `KNOWCODE_INSTALL_DIR` at install |
| The shell installer's receipt, `env.sh`, `env.fish` | `~/.config/knowcode/` | `~/.config/knowcode/` | `XDG_CONFIG_HOME` |

Nothing is written into the repos you index. `knowcode config path` prints the config file's path, and
`knowcode status` shows the config file, the cache directory, the notes database and the workspace roots, each with
where its setting came from. `knowcode uninstall` removes what KnowCode wrote; removing a shell-installer
installation is in [usage](usage.md#setup-settings-and-updates).

## config.toml

Edit the file by hand, or use `knowcode config get|set|unset|list`. `set` and `unset` keep the file's comments and
layout. `config list` shows every setting as it takes effect and where it came from.

```toml
[cache]
dir = "~/caches/knowcode"        # where the indexes live
max_size = "2 GiB"              # budget for idle indexes; 0 disables size eviction

[notes]
path = "~/notes/knowcode.sqlite" # the notes database

[workspace]
roots = ["~/Code", "~/work"]     # every git repo under these is in the workspace
max_depth = 4                    # how deep under a root a repo may be

[ignore]                         # paths not to index, per repo
"*" = ["Generated"]              # every repo
my-service = ["fixtures/"]       # a repo by directory name, or by absolute path

[privacy]
block_at = 1                     # content at this level or below is blocked; 0 blocks nothing
malformed = "restricted"         # a privacy file that can't be read: "restricted" or "default"
file = ".knowcode-privacy.yml"   # the privacy file's name and keys (see "Privacy file")

[conventions.csharp.bus]         # names your code base's own conventions use (see "Conventions")
message_attributes = ["BusMessage"]

[update]
check = true                     # the daily update check
```

| Key | Default | Meaning |
| --- | --- | --- |
| `cache.dir` | the user cache directory | Where indexes live. |
| `cache.max_size` | `2147483648` (2 GiB) | Idle indexes above this are evicted least recently used. Bytes or a size such as `"4 GiB"`; `0` disables size eviction. |
| `notes.path` | `notes.sqlite` in the data directory | The notes database. |
| `workspace.roots` | none | Directories whose git repos make up the workspace. |
| `workspace.max_depth` | `4` | How many directories deep under a root a repo may be. |
| `ignore.<repo>` | none | Path patterns not to index in that repo (`"*"` for every repo). |
| `privacy.block_at` | `1` | Privacy level at or below which content is blocked. |
| `privacy.malformed` | `"restricted"` | How to treat a privacy file that can't be read. |
| `privacy.file` | `".knowcode-privacy.yml"` | The privacy file's name, at a repo's root. |
| `privacy.global_key` | `"level"` | The privacy file's key for the whole repo's level. |
| `privacy.rules_key` | `"rules"` | The privacy file's key for its list of path rules. |
| `privacy.path_key` | `"path"` | A rule's key for its path. |
| `privacy.level_key` | `"level"` | A rule's key for its level. |
| `update.check` | `true` | Whether to run the daily update check. |

The `[conventions.*]` keys are in [Conventions](#conventions).

Precedence: an environment variable wins, then `config.toml`, then the default. `~/` expands in paths; other paths
must be absolute. A bad value is an error naming the file and the key. A key this build doesn't know is warned about
once and ignored. The MCP server rereads `config.toml` when it changes.

Before `config.toml` existed, the cache directory was named in a `cache_dir` file and user-level ignore rules were
in an `ignore` file, both in the config directory. They are still read while `config.toml` doesn't set `cache.dir`
or `[ignore]`. `knowcode config migrate` folds them into `config.toml` and renames them to `*.migrated`.

## Environment variables

| Variable | Meaning |
| --- | --- |
| `KNOWCODE_CONFIG_DIR` | The config directory. |
| `KNOWCODE_CACHE_DIR` | The cache directory (overrides `cache.dir`). |
| `KNOWCODE_DATA_DIR` | The data directory holding `notes.sqlite` (overrides `notes.path`). |
| `KNOWCODE_UPDATE_CHECK` | `0` turns the update check off (overrides `update.check`). |
| `KNOWCODE_TOOLS` | Which MCP tools `serve` advertises: `all` (default), `core`, or a comma list such as `core,kc_tests`. Same as `serve --tools`. See `kc_help profiles`. |
| `KNOWCODE_VERIFY` | `1` hashes every file on every command instead of trusting unchanged mtime and size. Same as `--verify`. |
| `KNOWCODE_NOW` | Where `hotspots` windows end: a `YYYY-MM-DD` date, unix seconds, or `head`. Same as `--as-of`. |
| `KNOWCODE_GIT_DEADLINE` | Seconds a `git` child process may run (default 60; `0` for none). |
| `KNOWCODE_SEARCH_DEADLINE` | Seconds a search child of the MCP server may run (default 120; `0` for none). |
| `KNOWCODE_SETUP_DEADLINE` | Seconds each client registration command in `setup` may run (default 120). |
| `KNOWCODE_UPDATE_CHECK_DEADLINE` | Seconds the update check may run (default 8). |
| `KNOWCODE_UPDATE_DEADLINE` | Seconds `self-update` may spend downloading and installing (default 180). |
| `KNOWCODE_HOOK_MS` | Milliseconds the Claude Code Grep/Glob hook may take before it gives up silently (default 1500). |
| `KNOWCODE_BIN` | The binary the Claude Code hook runs, when it isn't `knowcode` on `PATH`. |

A deadline that passes is reported in the answer as `incomplete` with reason `deadline`, naming the variable to raise.

The shell installer is generated by cargo-dist and takes its usual variables, such as `KNOWCODE_INSTALL_DIR` and
`KNOWCODE_NO_MODIFY_PATH`. `self-update` clears the installer's download overrides before it runs the installer.

## Workspace roots

The workspace is what cross-repo answers draw on (dependencies and dependents, HTTP routes, the message bus, SQL,
Ansible and Terraform links), and what a query covers outside any repo without `--repo` (and `search --all`,
`grep-ast --all`, `workspace: true` in MCP calls).

- With no roots, the workspace is every indexed repo.
- `knowcode workspace add <dir>` and `knowcode workspace remove <dir>` edit `workspace.roots`. `workspace list` shows
  each root, how many git repos it holds and how many are indexed.
- Every git repo under a root, down to `workspace.max_depth` directories, is in the workspace: clones, worktrees,
  and the worktrees git lists for them. The search doesn't enter a repo, hidden directories, or dependency and build
  directories (`node_modules`, `vendor`, `target`, `build`, …) unless they are repos themselves.
- Adding a root records the checkouts without building full indexes. While the MCP server runs, a low-priority
  worker reads their manifests and name summaries; a repo is fully indexed when you first query it or run
  `knowcode index`.
- A query about a repo outside every root still answers for that repo.

## Ignoring paths

KnowCode indexes what `git ls-files` lists (tracked and untracked, not git-ignored), minus build output and
dependency directories (`node_modules`, `vendor`, `third_party`, `Pods`, `DerivedData`, virtualenvs, …) and two kinds
of ignore rules:

- `.knowcodeignore` at a repo's root, one pattern per line, `#` for comments;
- `[ignore]` in `config.toml`, for repos you don't want to write into.

A pattern containing `/` matches a path prefix; otherwise it matches any path component, with `*` as a wildcard.
Changes to these files, `.gitignore` files, `.git/info/exclude` and git's global excludes file take effect on the
next query.

Also left out:

- Symbolic links are never followed. `status` lists those skipped (`coverage.skipped_symlinks`).
- Source files over 1 MB aren't extracted (ColdFusion and SQL files up to 8 MB). `status` and `overview` list them.
- Search skips binary files, files over 1 MB, and generated files (lockfiles, minified files, files marked
  `@generated` or `DO NOT EDIT`).
- Minified and bundled JavaScript (`*.min.js`, bundles, `dist/`, a line over 1,000 characters).

## Privacy file

A repo can declare how much of it AI tools may see, in a privacy file at its root. The file gives the whole repo a
level and can lower it for folders or files. Levels are 1 (restricted), 2 (confidential), 3 (internal) and
4 (public); a file's level is the lowest of the repo's level and every rule naming it or a folder above it, so a rule
can make a path more restrictive than its parent, never less.

The file is `.knowcode-privacy.yml`, in YAML:

```yaml
level: 3                  # the whole repo's level; 3 when absent
rules:                    # optional: folders (ending in /) or files, repo-relative
  - path: /payments/keys/
    level: 1
```

A code base that already keeps such files under another name or with other keys points KnowCode at them, with the
same levels and meaning:

```toml
[privacy]
file = ".data-policy.yml"
global_key = "repo_tier"
rules_key = "tiers"
path_key = "where"
level_key = "tier"
```

KnowCode blocks content at or below `privacy.block_at` (default 1, so levels 2 to 4 are answered from; 0 blocks
nothing). Blocked content never reaches an answer or the cache:

- Blocked files are not listed, read, extracted, searched or counted, and git history is read without their paths.
- A repo blocked whole is not indexed. A question naming it is refused with `withheld`, and workspace-wide answers
  leave it out. Another repo's dependency on it is shown only as blocked.
- A question naming a blocked path is refused without repeating the path.
- Every answer that read a repo with anything blocked is `complete: false` with an `incomplete` entry of reason
  `withheld`, naming the repo, never a blocked path.
- When the file changes so that something new is blocked, the repo's index, side caches, workspace summary entry and
  notes anchored in blocked files are deleted, and the repo is indexed again without them.
- `status` always shows the policy (`privacy`): the file's state (`read`, `empty`, `absent` or `malformed`), the file
  name it looked for, the global level, `block_at`, and how many paths are blocked (`blocked_paths`, with the rules'
  own paths in `blocked`, or `"repo"`). The CLI prints it as one line. So a `privacy.file` that names no file in the
  repo shows as `absent`, with the name it looked for.

A file that can't be read as the expected format is warned about and taken as level 1 (the whole repo blocked) until
it is fixed. `privacy.malformed = "default"` treats such a file as absent instead.

Commit subjects of commits touching both blocked and allowed files stay part of the allowed files' history. Repo
names stay visible.

## Conventions

Some linking recognises code by name: the attribute that marks a message type, the base classes of consumers, the
calls that publish, a service gateway's calls, a request contract's attribute or decorator. KnowCode knows the public
libraries' names (MassTransit's `IConsumer<T>`, NServiceBus's `IHandleMessages<T>`, `Publish` and `Send`, ASP.NET's
attributes, Angular's `HttpClient`). A code base's own conventions are configured under `[conventions.*]`, so its
messages, consumers and requests link as the libraries' do. Each key replaces its default. A change to a C# or
TypeScript key re-extracts that language's files at the next query; the CI and Ansible keys apply at once.

```toml
[conventions.csharp.bus]
message_attributes = ["BusMessage"]                       # [BusMessage(Hosts.Orders)] marks a message type
consumer_bases = ["IConsumer", "IHandleMessages", "ShopConsumer"]
consumer_registrations = ["AddConsumer", "AddShopConsumer"]
publish = ["Publish(message)", "Send(message)", "PublishToHost(message, host)", "SendToQueue(message, host, queue)"]

[conventions.csharp.http]
request_attributes = ["SendsRequest(service, path, verb)"] # [SendsRequest("BillingApi", "invoices", HttpMethods.Post)]
gateway_calls = ["CallService(service, path, verb)", "SendToApi(verb, service, path)"]
service_host_bases = ["ServiceHost"]                     # : ServiceHost<S>("BillingApi", …) hosts BillingApi

[conventions.csharp.di]
scope_lifetimes = { PerJob = "unit_of_work" }           # Scoped(Scope.PerJob)

[conventions.typescript.http]
request_decorators = ["RequestContract"]                 # @RequestContract({ endpoint: "orders", httpMethod: "POST" })
path_keys = ["endpoint", "path", "url"]
verb_keys = ["httpMethod", "method", "verb"]

[conventions.ci]
app_name_variables = ["PACKAGE_NAME", "APP_NAME"]

[conventions.ansible]
infra_names = ["edgeagent"]
name_prefixes = ["acme"]
```

A call is written `Name(role, …)`, one role per argument by position: `message`, `host` and `queue` for the bus,
`service`, `path` and `verb` for requests, `_` for any other argument. A `host` is a constant's member name
(`Hosts.Jobs` is `Jobs`) or a literal; a `queue` is a literal, on its host.

| Key | Default | Meaning |
| --- | --- | --- |
| `conventions.csharp.bus.message_attributes` | none | Attributes marking a class as a message type; the first argument, if any, is its destination. |
| `conventions.csharp.bus.consumer_bases` | `["IConsumer", "IHandleMessages"]` | Generic bases whose first type argument is the message a consumer handles. |
| `conventions.csharp.bus.consumer_registrations` | `["AddConsumer"]` | Registrations whose one type argument is a consumer class, not a message. |
| `conventions.csharp.bus.publish` | `["Publish(message)", "Send(message)"]` | Calls putting a message on the bus, on a receiver named for a bus, publisher, endpoint, session or context. |
| `conventions.csharp.http.request_attributes` | none | Attributes marking a class as a request through a service gateway. |
| `conventions.csharp.http.gateway_calls` | none | Gateway calls naming a request's service, path and verb. |
| `conventions.csharp.http.service_host_bases` | none | Base classes (a prefix of the name) of a service host, whose first constructor argument names the service. |
| `conventions.csharp.di.scope_lifetimes` | none | `Scoped(Scope.X)` values beyond the built-in ones, each to `singleton`, `scoped`, `transient` or `unit_of_work`. |
| `conventions.typescript.http.request_decorators` | none | Class decorators taking an object with the request's path and verb. |
| `conventions.typescript.http.path_keys` | `["endpoint", "path", "url"]` | The object's keys holding the path. |
| `conventions.typescript.http.verb_keys` | `["httpMethod", "method", "verb"]` | The object's keys holding the verb. |
| `conventions.ci.app_name_variables` | `["PACKAGE_NAME", "APP_NAME"]` | Pipeline variables naming the application a deploy installs, the first set winning. |
| `conventions.ansible.infra_names` | none | Infrastructure names, beyond the built-in ones (`consul`, `nginx`, `mysql`, …), that never link a service or image to an application repo. |
| `conventions.ansible.name_prefixes` | none | Prefixes dropped when service, image and repo names are compared. |

A team keeps its conventions in a config file of its own and adds it to each member's `config.toml` (or points
`KNOWCODE_CONFIG_DIR` at it), so its answers name its own messages and requests.

## Update check

At most once a day, a normal command checks public GitHub release metadata for a newer version and prints a notice
to stderr. It sends no repository paths, code, questions or identifiers, runs outside query execution, and never
installs anything.

- Turn it off with `knowcode config set update.check false` or `KNOWCODE_UPDATE_CHECK=0`.
- `knowcode self-update --check` checks on demand, even with automatic checks off.
- `knowcode self-update` installs the latest release for a shell-installer installation. Other installations
  (a source build, for example) update the way they were installed.

Indexing and queries make no network calls, and there is no telemetry.

## History settings

History answers (`hotspots`, `coupled`, `ownership`, `reviewers`, `tests`, `areas`) read local git history only.

- `bot_authors` in the config directory: one name or email per line of automated authors to leave out, in addition
  to the built-in rules (`[bot]`, `noreply@` service mailboxes, CI-style names and commit subjects). `!name` exempts
  an author the rules would catch.
- `former_authors` in the config directory: one name or email per line of authors who have left, for `ownership`
  (`contributors_gone`) and `reviewers`.
- A repo's `.mailmap` is applied.

History caches follow these files, so an edit takes effect on the next query.
