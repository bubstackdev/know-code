# Architecture

KnowCode is one Rust binary with no services. It keeps one SQLite index per checkout in the user cache directory,
brings it up to date with the working tree before answering, and resolves names at query time.

## Crates

| Crate | Role |
| --- | --- |
| `kc-extract` | Per-file extraction with tree-sitter (Swift, Python, Rust, C#, TypeScript/TSX/JavaScript, CFML, Terraform, Angular templates), a SQL statement scanner, Ansible YAML and Jinja, GitLab CI, and manifests (SwiftPM, XcodeGen, Cargo, `.csproj`, `package.json`, `tsconfig.json`). Extraction is context-free: changing one file never forces another to be re-extracted. |
| `kc-store` | The SQLite schema, FTS5 and trigram search, name resolution and graph queries (`query.rs`, `trace.rs`), the answer contract types (`answer.rs`), and execution limits (`limits.rs`). |
| `kc-index` | File discovery, ignore rules, freshness, incremental indexing, the content index, git history (hotspots, co-change, ownership, test selection), cross-repo linking (HTTP, bus, SQL, Terraform, Ansible, CI), the workspace summary, configuration, privacy, notes, the writer lock and the cache lifecycle. |
| `knowcode` | The CLI (`main.rs`), the operations behind each command (`ops.rs`), walks, paths and flows, rendering, the answer contract (`contract.rs`), cursors, the MCP server (`mcp.rs`) and its help topics (`help.rs`), client setup, uninstall and updates. |

## The index

Each checkout gets `<cache>/repos/<name>-<hash>.sqlite`, opened in WAL mode. It holds:

- files with `(path, mtime, size, blake3)`;
- symbols, references with their site (file, line, column), relations, imports and tags;
- framework facts: routes, DI registrations, table access, bus publishes and consumers, templates, Terraform and
  Ansible facts;
- constructs an extractor recognised but doesn't read (an assembly scan, a dynamic include), in a table of their own,
  so an answer drawing on them says it is incomplete with reason `unsupported`;
- a trigram content index for `search` (one row per trigram with delta-encoded file ids; file contents are not
  stored, so every candidate is verified against the working tree);
- the words `locate` ranks by, in a contentless FTS5 table;
- a generation, moved by every sync, and caches derived from it (rankings, areas, history walks).

References are stored unresolved. Resolution happens at query time against the whole index (and against
dependency repos when the answer crosses into them), so an edit to one file never requires re-extracting others.
Each extractor has a version; bumping it re-extracts that language's files on the next query.

An index written by a newer schema is refused with an error naming both schemas, rather than rebuilt; an older one
is rebuilt.

## Freshness

Every CLI query syncs the repos it reads before answering, unless `--no-sync`:

1. **Discovery.** `git ls-files --cached --others --exclude-standard`, minus ignore rules. The file list is reused
   while the git index, the ignore files and every directory listing seen last time are unchanged, and is rebuilt at
   least every 10 minutes.
2. **Change check.** Each file's mtime and size are compared with the index. A file whose metadata moved is hashed,
   and re-extracted only if its hash changed. `--verify` (or `KNOWCODE_VERIFY=1`) hashes every file, for edits that
   keep mtime and size.
3. **Commit.** A sync writes the facts and the generation they make in one transaction.

`context` checks a symbol's file hash before showing its source, and reindexes on a mismatch. A file that can't be
read keeps its last-indexed facts; the answer lists it in `freshness.failed` and is incomplete with reason
`failed`.

A query reads one generation of each index it opens: after its own sync it holds a read snapshot, so a concurrent
reindex by another process never shows it a mix of two generations. A cross-repo answer holds one snapshot per repo.

## The writer lock

Indexing a checkout takes an exclusive per-repository lock file, kept outside the `repos/` directory. Readers
never take it: they read committed snapshots while indexing proceeds. Cache maintenance uses a non-blocking attempt
and skips a repo another process is indexing. Lock files are never removed during pruning, since unlinking a held
lock would let another process lock a different file for the same repository.

## First run and the MCP server

There is no daemon. `knowcode serve` runs for as long as the client keeps it.

- The first query indexes its repository automatically, by default the server's working directory.
- MCP graph queries share a wait of about two seconds for indexing across the repos they consult. Past it, they answer
  from the available committed snapshot with `freshness.status: "indexing"`, `complete: false` and a `stale` reason.
  A missing index never yields a confidently complete empty answer.
- One low-priority background worker, with one extraction thread, finishes indexing. While the server is alive it
  polls one queried checkout per idle tick, so edits are picked up before the next query.
- Background threads index the repos under the workspace roots that have no full index yet, smallest first, while no
  query's indexing is queued, and only while the cache is below nine tenths of `cache.max_size`; past that the worker
  reads their manifests and name summaries instead. How many run at once follows the machine
  (`kc_index::machine`): every few seconds it reads the cores, memory, load average, memory pressure and power source.
  Under load from other work, on battery, or under memory pressure, one repo on one thread; idle and on power, a repo
  per four cores, within a quarter of the memory; under critical memory pressure, none starts. A repo already started
  finishes, at background priority.
- `knowcode index` of several repos (`--workspace`, or several paths) indexes them at once: as many as half the
  memory holds (512 MiB each) and every core but one allows, the cores shared among them, at most 8 parser threads a
  repo (one thread writes each index, and bounds its parse past that). `--jobs` and `--threads` override it.
- SQLite's memory is bounded per connection, not per process. Each connection's page cache holds at most 2 MiB
  (`cache_size`). A process holds one connection per index it has open, a second for caches written outside a
  query's snapshot, and one per thread building a reference graph. A workspace-wide call keeps the indexes it reads
  open until it answers, so its page caches can reach 4 MiB per repo it reads. Sorting and temporary tables come on
  top of that, and are freed when their statement ends. SQLite is built without a process-wide page cache or
  allocation counting (`.cargo/config.toml`): with several repos indexing at once, their global locks cost more
  system time than the indexing did. That rules out a process-wide limit (`sqlite3_soft_heap_limit64`).
- Calls run one at a time, except that a call past 2 seconds lets the next one start beside it (at most two), and
  `kc_search` and `kc_grep_ast` run in a child process of their own.
- Child processes have deadlines (`git` 60 seconds, a search child 120 seconds). A child past its deadline, or in a
  cancelled call, is killed; the answer is incomplete with reason `deadline`, and nothing computed from it is cached.

## The query context

Each query (an MCP tool call's work, a CLI command) runs with one context object (`kc_store::limits::Operation`). It
holds the query's cancellation, its wait for indexing, and what the query collects on the side for its answer's
`incomplete` entries: child processes cut at their deadline, repos a workspace fan-out left out because they could
not be read, and gone checkouts that would have mattered. The code finishing the answer takes each entry once. Two
queries never share a context, so two calls running at once, or one after another on the same pooled thread, never
see each other's entries. A thread that runs no query of its own (a CLI command, a test) gets its own context on first
use.

## Across repos

The workspace summary holds, per indexed repo, its manifests, the routes, messages and tables it serves or uses, and
Bloom filters of its reference names, defined names and table names. Workspace-wide answers (`refs` or `tables`
outside a repo, dependents) open only the repos the summary says can answer, and read the others as last synced. A
summary is used only while its repo is unchanged since it was written (checked by `stat` against a snapshot each sync
leaves); a changed repo is synced first.

Checkouts of one origin (a clone, a worktree, a second clone, the remote URL written any way) are one repo. One rule
picks the checkout that answers for a repo in every cross-repo link: the queried repo's own checkout, then one that
exists, then one under the workspace roots, then the most recently synced. A deleted checkout is left out of every
cross-repo link until pruned, and an answer it would have joined names it with `missing_repo`.

## Walks

`trace`, `impact`, `path` and `flow` decide about an edge in one place (`crates/knowcode/src/graph.rs`). A walk's
gate holds the edge kinds it lists and follows, its evidence policy (an edge is followed, listed as a leaf, or left out
and counted in the answer's `policy`), and the query it runs for: a cancelled query's walk stops at its next
expansion. A hop across repos (an HTTP route, the bus, SQL, a deploy, a Terraform module) arrives with its confidence
across the link and the evidence it rests on, worked out once for every walk and the check past where a walk stopped.

All four read every edge, in a repo and across repos, through one symbol graph (`crates/knowcode/src/walk.rs`) that
opens repos as the walk reaches them, up to `max_repos`. `path` and `flow` read it forward and backward
(`Graph::out`, `Graph::into`); `trace` and `impact` read the tree edges (`Graph::tree_edges`: a type's conformances
and conformers too) and the crossings, in `crates/knowcode/src/tree.rs`. Every place a walk stops short names the
exact call going on from it. Walk answers are pinned by a snapshot test (`crates/knowcode/tests/snapshots/walks.txt`).

## The cache lifecycle

- **Seeding.** A new worktree or clone of an indexed repo starts from a copy of that repo's index and extracts only
  what differs (never when either side's privacy file blocks something).
- **Maintenance.** After queries, and periodically while the MCP server runs, deleted checkouts are pruned and idle
  indexes are evicted least recently used above `cache.max_size` (default 2 GiB). Current and in-use indexes stay,
  so the cache may exceed the budget for a while. Eviction keeps the repo's last workspace summary; an answer that
  uses it says it is stale, and the index is rebuilt when needed.
- **Explicit.** `knowcode forget PATH` removes one checkout's index and summary entry, `knowcode prune` the indexes of
  checkouts that no longer exist, `knowcode purge` all recognised indexes. Unrelated files are never touched.
- **Notes** live in the data directory, not the cache, so none of this loses them. Only `forget` and a privacy purge
  delete the notes anchored in blocked content. Notes kept in the cache by earlier builds are moved on first use and
  the old file is renamed `notes.sqlite.migrated`.

## Network

Indexing and queries make no network calls. The only automatic network operation is the daily update check, which
fetches public release metadata with `curl` and can be turned off (see
[configuration](configuration.md#update-check)). `self-update` and the installers access the network only when you run them.
