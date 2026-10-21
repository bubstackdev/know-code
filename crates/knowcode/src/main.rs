//! `knowcode`: index local repos and answer code-graph questions about them.

mod bench;
mod cite;
mod contract;
mod compact;
mod cursor;
#[cfg(test)]
mod completeness;
mod fetch;
mod flow;
mod graph;
mod help;
mod history;
mod hook;
mod mcp;
mod ops;
mod refactor;
mod refresh;
mod removed;
mod render;
mod settings;
mod setup;
mod uninstall;
mod update;
mod tf;
mod walk;

use std::path::PathBuf;
use std::time::Instant;

use anyhow::Result;
use clap::{Parser, Subcommand};
use kc_index::{SyncOptions, search};
use ops::{repo_name, round1};
use serde_json::json;

#[derive(Parser)]
#[command(name = "knowcode", version, about = "A lightweight, local code graph for coding agents")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(clap::Args, Clone)]
struct RepoArgs {
    /// Repo to query (repeatable). Defaults to the repo containing the current directory, else every indexed repo in
    /// the workspace.
    #[arg(long = "repo", short = 'r')]
    repos: Vec<PathBuf>,
    /// Skip the freshness check and answer from the index as it is.
    #[arg(long)]
    no_sync: bool,
    /// Emit the JSON envelope instead of text.
    #[arg(long)]
    json: bool,
}

/// Whether an answer's rows quote the source lines they rest on.
#[derive(clap::Args, Clone)]
struct CiteArgs {
    /// `brief`: the first 12 rows quote the line they rest on (an edge's site, else the line naming a declaration), up
    /// to 120 characters, and the rest give where it is; `lines`: every row quotes its lines (a declaration's first lines
    /// through its name), each up to 160 characters, saying what a cap left out; `check`: no lines. Each cite says
    /// whether its lines match the index (`verified`) or how they drifted. `none`: locations only.
    #[arg(long, default_value = "brief")]
    cite: kc_store::answer::CiteMode,
}

impl CiteArgs {
    /// Sets the mode for the query this command runs.
    fn enter(&self) {
        kc_store::limits::current().set_cite(self.cite);
    }
}

/// Which edges and symbol kinds a walk keeps.
#[derive(clap::Args, Clone)]
struct WalkArgs {
    /// Edge kinds to list and follow, comma-separated or repeated: calls (with via_di, via_protocol, dispatch),
    /// instantiates, uses, reads, writes, renders, conforms_to, conformer, witness, via_di, http, bus, sql.
    #[arg(long, value_delimiter = ',')]
    edges: Vec<String>,
    /// Symbol kinds to list, comma-separated or repeated: method, function, class, property, …, or type and func.
    /// The walk goes through the others; a listed symbol hangs off its nearest listed ancestor.
    #[arg(long, value_delimiter = ',')]
    kinds: Vec<String>,
    /// Which edges to follow by what they rest on: `default` (as always), `confirmed` (only a compiler's or an
    /// explicit link: no name match, convention or ambiguous edge), `all` (ambiguous ones too), mechanisms
    /// (`scip,receiver_type`), or a preset changed (`confirmed,name_unique`, `default,-same_deployment`). The answer's
    /// `policy` counts what was left out.
    #[arg(long, visible_alias = "min-evidence", default_value = "default")]
    evidence: String,
}

impl WalkArgs {
    fn filter(&self) -> ops::WalkFilter {
        ops::WalkFilter { edges: self.edges.clone(), kinds: self.kinds.clone(), evidence: self.evidence.clone() }
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// Index repos (cold or incremental).
    Index {
        /// Repo paths. With none, indexes the repo containing the current directory.
        paths: Vec<PathBuf>,
        /// Index every git repo directly under this directory.
        #[arg(long)]
        workspace: Option<PathBuf>,
        /// Parser threads (default: half the CPU cores).
        #[arg(long)]
        threads: Option<usize>,
        /// Drop each index and rebuild it from scratch.
        #[arg(long)]
        rebuild: bool,
        /// Read precise references from this SCIP index (written by `scip-typescript`, `scip-dotnet`, …), and keep
        /// reading it whenever it changes. Without it, `index.scip` at the repo root is read when present.
        #[arg(long, value_name = "FILE")]
        scip: Option<PathBuf>,
        /// Read and hash every file, rather than trusting an unchanged mtime and size, which some edits keep. Also
        /// set by `KNOWCODE_VERIFY=1` (for every command).
        #[arg(long)]
        verify: bool,
        #[arg(long)]
        json: bool,
    },
    /// Remove the index of a checkout (a worktree about to be deleted, say).
    Forget {
        /// The checkout's path.
        path: PathBuf,
    },
    /// Remove the indexes of checkouts that no longer exist.
    Prune,
    /// HTTP routes the selected repos request (request contracts, gateway and HTTP client calls), with the actions serving them across indexed repos.
    Routes {
        #[command(flatten)]
        repo: RepoArgs,
    },
    /// Entity Framework tables per DbContext: entity, DbSet, file:line and how many places read and write each;
    /// tables named in SQL text (CFML queries, C# SQL strings); stored procedure calls; each with the SQL defining it
    /// (`CREATE TABLE`, `CREATE PROCEDURE`) in any indexed repo and the database it is in (`shop.invoice`, and other
    /// databases with a table of that name). With a name (table, entity or DbSet), that table's accessors.
    Tables {
        name: Option<String>,
        /// List every caller writing only through a callee; by default test callers are counted, not listed, and
        /// others are kept to 5 per callee.
        #[arg(long)]
        all_callers: bool,
        #[command(flatten)]
        repo: RepoArgs,
    },
    /// Messages the selected repos publish and consume on the message bus, with the consumers and publishers on the
    /// other side across indexed repos.
    Events {
        #[command(flatten)]
        repo: RepoArgs,
    },
    /// The symbols enclosing a line, innermost first (`src/app/filter.service.ts:419`).
    At {
        /// `file:line`; the file is repo-relative, or absolute to pick its repo.
        location: String,
        #[command(flatten)]
        repo: RepoArgs,
    },
    /// Find definitions by name, prefix, or camelCase words (`set view`, `Store.load`).
    Find {
        query: String,
        /// Kind filter: type, func, class, struct, enum, actor, protocol (or interface, trait), extension, method,
        /// function, init (or constructor), property, case, typealias.
        #[arg(long, short = 'k', visible_alias = "kinds")]
        kind: Option<String>,
        #[arg(long, short = 'n', default_value_t = 20)]
        limit: usize,
        #[command(flatten)]
        repo: RepoArgs,
    },
    /// Find the files for a bug report, ticket or feature: files for a plain-language query ("refund approval", "lock
    /// domains for transfer", a ticket title), ranked by BM25 over the words of their path, symbol names, doc comments
    /// and referenced names, with the matching symbols in each. Use it first when you know what the code does but
    /// not what it is called.
    Locate {
        query: String,
        #[arg(long, short = 'n', default_value_t = 20)]
        limit: usize,
        #[command(flatten)]
        repo: RepoArgs,
    },
    /// References to a name (calls, instantiations, type uses), resolved with confidence tags.
    Refs {
        /// A name, or `Type.member` to keep only references that can point at that member.
        name: String,
        /// Reference kind filter (repeatable, or comma-separated): call, instantiate, type_ref, member; in SQL
        /// files read, write. `--edges` is the same filter, named as trace and impact name it.
        #[arg(long, short = 'k', visible_alias = "edges", value_delimiter = ',')]
        kind: Vec<String>,
        #[arg(long, short = 'n', default_value_t = 50)]
        limit: usize,
        #[command(flatten)]
        cite: CiteArgs,
        #[command(flatten)]
        repo: RepoArgs,
    },
    /// One bundle about a symbol: source, members, extensions, conformances, callers, callees. The symbol may be
    /// `Type.member`, a name, `path/to/file#name` or `file:line` (to pick one of several same-named definitions), a
    /// table or stored procedure (defined in any indexed repo), or a repo's own name: how is this repo deployed (its
    /// CI deploy chains, playbooks and roles).
    Context {
        /// A name, or `Type.member`.
        symbol: String,
        /// Approximate token budget for the whole bundle.
        #[arg(long, default_value_t = 2000)]
        budget: usize,
        /// The symbol's source: `full`, `skeleton` (signatures, types and doc comments with bodies elided, plus the
        /// lines with the references it makes and its return and throw lines), `none`, or `auto` (skeleton when the
        /// full source would take more than a fifth of the budget).
        #[arg(long, default_value = "auto")]
        source: String,
        /// For a repo's own name (its deploy answer): rows per list; each list cut continues by its cursor.
        #[arg(long)]
        limit: Option<usize>,
        #[command(flatten)]
        cite: CiteArgs,
        #[command(flatten)]
        repo: RepoArgs,
    },
    /// Call and relation tree around a symbol: callers, callees, conformers, protocol implementations. Inward, a
    /// controller action continues to the code, in any indexed repo, requesting it over HTTP.
    Trace {
        /// A name, or `Type.member`.
        symbol: String,
        /// `in` (what reaches it), `out` (what it reaches) or `both`.
        #[arg(long, short = 'd', default_value = "both")]
        direction: String,
        /// Hops to follow, 1 to 3.
        #[arg(long, default_value_t = 2)]
        depth: u8,
        /// Node cap.
        #[arg(long, short = 'n', default_value_t = 80)]
        limit: usize,
        #[command(flatten)]
        filter: WalkArgs,
        #[command(flatten)]
        cite: CiteArgs,
        #[command(flatten)]
        repo: RepoArgs,
    },
    /// How one symbol reaches another, possibly across repos: the best few paths over calls, DI bindings, HTTP
    /// routes, the message bus and SQL, each hop with its edge, confidence and site.
    Path {
        /// Where the path starts: a name, or `Type.member`, in the queried repo.
        from: String,
        /// Where it ends: a name, or `Type.member`, in `--to-repo`.
        to: String,
        /// The repo `to` is in (default: the queried repo, else the other indexed repos defining it, up to 8).
        #[arg(long)]
        to_repo: Option<PathBuf>,
        /// Hops a path may have, 1 to 12.
        #[arg(long, default_value_t = 8)]
        max_hops: u8,
        /// Paths to show, 1 to 5.
        #[arg(long, short = 'n', default_value_t = 3)]
        paths: usize,
        /// Symbols to explore at most, both directions together.
        #[arg(long, default_value_t = 2000)]
        max_nodes: usize,
        /// Repos to open at most.
        #[arg(long, default_value_t = 16)]
        max_repos: usize,
        /// Search forward only (to compare with the bidirectional search).
        #[arg(long, hide = true)]
        one_way: bool,
        /// Follow ambiguous edges too. By default a path uses only exact and unique_name edges.
        #[arg(long)]
        allow_ambiguous: bool,
        /// Edge kinds to follow, comma-separated or repeated: calls (with via_di, dispatch), instantiates, uses, reads,
        /// writes, http, bus, sql, ngrx.
        #[arg(long, value_delimiter = ',')]
        edges: Vec<String>,
        /// Which edges to follow by what they rest on: `default` (as always), `confirmed` (only a compiler's or an
        /// explicit link: no name match, convention or ambiguous edge), `all` (ambiguous ones too), mechanisms
        /// (`scip,receiver_type`), or a preset changed (`confirmed,name_unique`, `default,-same_deployment`). The answer's
        /// `policy` counts what was left out.
        #[arg(long, visible_alias = "min-evidence", default_value = "default")]
        evidence: String,
        #[command(flatten)]
        cite: CiteArgs,
        #[command(flatten)]
        repo: RepoArgs,
    },
    /// What happens end to end from an entry point (an Angular route path, a component by name, selector or file, an
    /// action, a method, or a `file:line`), across repos: service calls, DI bindings, dialogs and child components,
    /// HTTP routes, the message bus and data access, as a tree of numbered steps grouped by repo. Logging, mapping and
    /// utility calls are left out.
    Flow {
        entry: String,
        /// Steps deep a branch goes, 1 to 16.
        #[arg(long, default_value_t = 10)]
        depth: u8,
        /// Steps to list at most.
        #[arg(long, short = 'n', default_value_t = 80)]
        limit: usize,
        /// Repos to open at most.
        #[arg(long, default_value_t = 16)]
        max_repos: usize,
        /// Edge kinds to follow, comma-separated or repeated: calls (with via_di, dispatch), instantiates, uses, reads,
        /// writes, http, bus, sql, ngrx, dialog, child.
        #[arg(long, value_delimiter = ',')]
        edges: Vec<String>,
        /// Levels of child components (used in a component's template) to follow; 0 for none. Dialogs a component
        /// opens are always followed.
        #[arg(long, default_value_t = flow::DEFAULT_CHILDREN)]
        children: u8,
        /// Which edges to follow by what they rest on: `default` (as always), `confirmed` (only a compiler's or an
        /// explicit link: no name match, convention or ambiguous edge), `all` (ambiguous ones too), mechanisms
        /// (`scip,receiver_type`), or a preset changed (`confirmed,name_unique`, `default,-same_deployment`). The answer's
        /// `policy` counts what was left out.
        #[arg(long, visible_alias = "min-evidence", default_value = "default")]
        evidence: String,
        #[command(flatten)]
        cite: CiteArgs,
        #[command(flatten)]
        repo: RepoArgs,
    },
    /// What depends on a symbol, a file, or the changes in a diff: dependents classed by proximity (direct: hop 1,
    /// near: 2, far: 3+; graph distance, not risk), across HTTP routes from a controller action to the code, in any
    /// indexed repo, requesting it.
    Impact {
        /// A symbol (name or `Type.member`).
        symbol: Option<String>,
        /// A file (relative to the repo root, or absolute) instead of a symbol.
        #[arg(long, conflicts_with = "symbol")]
        file: Option<String>,
        /// Changed lines instead of a symbol: `working` (uncommitted changes) or a git revision.
        #[arg(long, conflicts_with_all = ["symbol", "file"])]
        diff: Option<String>,
        /// Hops to follow, 1 to 16. Where the walk stops with dependents left (at the depth, or an ambiguous edge), the
        /// answer names them (`frontier`) and says how to go on.
        #[arg(long, default_value_t = 3)]
        depth: u8,
        #[arg(long, short = 'n', default_value_t = 200)]
        limit: usize,
        /// Order within a hop: `rank` (PageRank of each file, personalized toward the changed files) or `hop` (as
        /// the walk found them).
        #[arg(long, default_value = "rank")]
        order: ops::ImpactOrder,
        /// The `coupled` class reads history only up to this commit (with its ancestors) or date.
        #[arg(long)]
        as_of: Option<String>,
        /// How far back the `coupled` class reads: `<n>d`, a date or `all` (default 365 days).
        #[arg(long)]
        since: Option<String>,
        #[command(flatten)]
        filter: WalkArgs,
        #[command(flatten)]
        cite: CiteArgs,
        #[command(flatten)]
        repo: RepoArgs,
    },
    /// Dead code: symbols nothing reaches. `unused` when every reference to the name, here and in indexed repos
    /// depending on this one, resolves elsewhere and nothing else names it; `possibly unused`, with the reasons,
    /// when only ambiguous references, tests, strings, nameof, templates or config do, or it is a library's public
    /// API. Entry points, overrides, interface implementations and framework conventions are never reported.
    Unused {
        /// Kind filter (repeatable): type, class, struct, enum, interface, method, function, func, property.
        #[arg(long, short = 'k')]
        kind: Vec<String>,
        /// Which findings to list: unused, possibly or all.
        #[arg(long, default_value = "all")]
        tier: String,
        #[arg(long, short = 'n', default_value_t = 200)]
        limit: usize,
        #[command(flatten)]
        repo: RepoArgs,
    },
    /// Preview a rename without editing: every site (definition, references, overrides and implementations,
    /// interface members, crefs, nameof, strings, templates, routes) with its confidence, sites in repos depending
    /// on this one, and collisions with the new name. Exits 2, with the reasons, when a site is ambiguous or the
    /// name collides, unless --allow-ambiguous.
    Rename {
        /// `Type.member`, a name, or `file:line`.
        symbol: String,
        new_name: String,
        /// Preview even when a site is ambiguous or the new name collides (exit 0).
        #[arg(long)]
        allow_ambiguous: bool,
        #[command(flatten)]
        repo: RepoArgs,
    },
    /// Files that historically change in the same commits as a file (git history, the last 365 days), with how often:
    /// support (shared commits) and confidence (the share of the file's commits). Source files and the templates,
    /// stylesheets and scripts beside them; renames followed; automated commits left out.
    Coupled {
        /// The file (relative to the repo root, or absolute).
        file: String,
        #[arg(long, short = 'n', default_value_t = 10)]
        limit: usize,
        /// Read history only up to this commit (with its ancestors) or date (YYYY-MM-DD): what was known then.
        #[arg(long)]
        as_of: Option<String>,
        /// How far back from the end: `<n>d`, a date or `all` (default 365 days; a file unchanged in the last 365
        /// days of HEAD is read over its own last 50 commits).
        #[arg(long)]
        since: Option<String>,
        #[command(flatten)]
        repo: RepoArgs,
    },
    /// The tests to run for a change: test files ranked by reverse edges from the changed code, naming
    /// (`FooTests` for `Foo`) and co-change, each with its reasons and a confidence. A tight list first; what was
    /// left out is counted and listed after.
    Tests {
        /// Changed files (relative to the repo root, or absolute) or symbols (name or `Type.member`).
        targets: Vec<String>,
        /// Changed lines instead of targets: `working` (uncommitted changes) or a git revision.
        #[arg(long, conflicts_with = "targets")]
        diff: Option<String>,
        /// Tests to list at most in the tight list (most likely relevant), and in the left-out list (at least 20).
        #[arg(long, short = 'n', default_value_t = 10)]
        limit: usize,
        /// Which edges the reverse walk follows by what they rest on: `all` (ambiguous ones too, ranked low), or
        /// `default`, `confirmed`, mechanisms, or a preset changed (as for `impact`). The answer's `policy` counts what
        /// was left out.
        #[arg(long, visible_alias = "min-evidence", default_value = "all")]
        evidence: String,
        #[command(flatten)]
        repo: RepoArgs,
    },
    /// Files that keep hurting: churn, bug-fix ratio, authors, complexity, nesting, function length and coupling,
    /// ranked. Across repos, repos ranked. With a file, its vitals and code-health biomarkers.
    Hotspots {
        /// One file's vitals and biomarkers instead of the ranking.
        path: Option<String>,
        #[arg(long, short = 'n', default_value_t = 15)]
        top: usize,
        /// End the 30/90/365-day windows here instead of now: unix seconds, YYYY-MM-DD, or `head` (the HEAD
        /// commit's time). `KNOWCODE_NOW` does the same.
        #[arg(long)]
        as_of: Option<String>,
        #[command(flatten)]
        repo: RepoArgs,
    },
    /// Historical contribution, from git history: the top contributor (by lines added), truck factor (the fewest
    /// authors covering more than half the lines added) and the share of lines added by authors who have left, for a
    /// file, a directory or the repo, with its areas; for a file, also its surviving lines by author (git blame).
    /// History, not who knows the code now. `--without` simulates authors leaving.
    Ownership {
        /// A file or directory (relative to the repo root, or absolute); the whole repo when left out.
        path: Option<String>,
        /// Treat this author (name or email) as gone: the files they are top contributor of, and the share of lines
        /// added by authors gone. Repeat for several.
        #[arg(long)]
        without: Vec<String>,
        /// Directory depth of the areas listed.
        #[arg(long, default_value_t = 2)]
        depth: usize,
        #[arg(long, short = 'n', default_value_t = 15)]
        top: usize,
        /// Anchor for "possibly former" and recency: unix seconds, YYYY-MM-DD or `head`.
        #[arg(long)]
        as_of: Option<String>,
        #[command(flatten)]
        repo: RepoArgs,
    },
    /// Who should review a change: authors ranked by recent ownership (lines added, halved every 180 days) of the
    /// changed files, leaving out former authors and the change's authors: you (the repo's user.email) for files or
    /// `working`; for a revision, the authors and committers of its commits, ranked on history up to the diff's base.
    Reviewers {
        /// Changed files (relative to the repo root, or absolute).
        files: Vec<String>,
        /// The changed files of a diff instead: `working` (uncommitted changes) or a git revision.
        #[arg(long, conflicts_with = "files")]
        diff: Option<String>,
        #[arg(long, short = 'n', default_value_t = 5)]
        limit: usize,
        #[arg(long)]
        as_of: Option<String>,
        #[command(flatten)]
        repo: RepoArgs,
    },
    /// What a repo is: languages, packages, dependencies, entry points, key types, top hotspots, ranked map.
    Overview {
        /// Bias the ranked map toward a name or a path fragment.
        #[arg(long)]
        focus: Option<String>,
        /// Token budget for the ranked map (0 to leave it out).
        #[arg(long, default_value_t = 1000)]
        map_budget: usize,
        #[command(flatten)]
        repo: RepoArgs,
    },
    /// Functional areas: the repo's files clustered (Louvain with refinement) over resolved references, co-change
    /// from history and the directory tree. Each area has a name, cohesion, key files, entry points and the areas it
    /// depends on. With --area, one area and every file in it.
    Areas {
        /// One area: its id, name or directory (or a fragment of one name).
        #[arg(long)]
        area: Option<String>,
        #[command(flatten)]
        repo: RepoArgs,
    },
    /// Notes anchored to symbols, kept in the user data directory (never in the repo or the cache; `notes.path` in
    /// config.toml moves them). A note goes stale when the symbol's body changes and orphaned when no symbol has its
    /// name.
    Note {
        #[command(subcommand)]
        action: NoteCmd,
    },
    /// A deterministic Markdown summary of a repo (facts only), printed. Nothing is written to the repo: agents read
    /// the same facts on demand through `kc_overview`.
    Docs {
        /// Token budget for the map section.
        #[arg(long, default_value_t = 1500)]
        map_budget: usize,
        #[command(flatten)]
        repo: RepoArgs,
    },
    /// Search file contents (literal or regex) across repos, ranked: definitions, declarations, code, then tests,
    /// comments and docs. Matches are per line, like `git grep`.
    Search {
        pattern: String,
        /// Treat the pattern as a regular expression (default: a literal string).
        #[arg(long, short = 'e')]
        regex: bool,
        /// Ignore case.
        #[arg(long, short = 'i', conflicts_with = "smart_case")]
        ignore_case: bool,
        /// Ignore case unless the pattern has an uppercase letter.
        #[arg(long, short = 'S')]
        smart_case: bool,
        /// Whole words only.
        #[arg(long, short = 'w')]
        word: bool,
        /// Language filter (repeatable): swift, python, rust, markdown, json, yaml, toml, shell, ...
        #[arg(long, short = 'l')]
        lang: Vec<String>,
        /// Path glob (repeatable); `!` excludes. Without a `/` it matches file names: `-g '*.swift' -g '!*Tests*'`.
        #[arg(long, short = 'g')]
        glob: Vec<String>,
        /// Lines of context around each hit.
        #[arg(long, short = 'C', default_value_t = 0)]
        context: usize,
        #[arg(long, short = 'n', default_value_t = 50)]
        limit: usize,
        /// Skip this many ranked results (for paging; see next_offset).
        #[arg(long, default_value_t = 0)]
        offset: usize,
        /// One line per matching file instead of per matching line.
        #[arg(long)]
        files: bool,
        /// Search every indexed repo in the workspace.
        #[arg(long, short = 'a', conflicts_with = "repos")]
        all: bool,
        /// Stop after this many matching lines.
        #[arg(long, default_value_t = 50_000, hide = true)]
        max_lines: usize,
        #[command(flatten)]
        repo: RepoArgs,
    },
    /// Structural search with an ast-grep pattern: code with metavariables (`$X` one node, `$$$ARGS` any number,
    /// `$_` one node, uncaptured), matched against syntax trees: `knowcode grep-ast 'new HttpClient($$$)' -l cs`.
    GrepAst {
        /// The pattern; or `kind:<node kind>` for every node of a kind, `kind:<node kind>~<regex>` for those whose
        /// text matches the regex.
        pattern: String,
        /// Language: csharp (cs), typescript (ts; .tsx files too), javascript (js), python (py), rust (rs), swift.
        #[arg(long, short = 'l')]
        lang: String,
        /// Node kind to match within the pattern parsed as context, for code that doesn't parse alone:
        /// `'class C { async void $M() { $$$ } }' --selector method_declaration`.
        #[arg(long)]
        selector: Option<String>,
        /// Keep matches inside a node matching this: a pattern, `kind:<node kind>`, `kind:<node kind>~<regex>`, or
        /// `kind:<node kind> <context pattern>` (the node of that kind within the pattern parsed as context).
        #[arg(long)]
        inside: Option<String>,
        /// Keep matches containing a node matching this (same forms as --inside).
        #[arg(long)]
        has: Option<String>,
        /// Drop matches inside a node matching this.
        #[arg(long)]
        not_inside: Option<String>,
        /// Drop matches containing a node matching this.
        #[arg(long)]
        not_has: Option<String>,
        /// Keep matches whose text matches this regular expression.
        #[arg(long)]
        text: Option<String>,
        /// Path glob (repeatable); `!` excludes.
        #[arg(long, short = 'g')]
        glob: Vec<String>,
        #[arg(long, short = 'n', default_value_t = 50)]
        limit: usize,
        /// Skip this many results (for paging; see next_offset).
        #[arg(long, default_value_t = 0)]
        offset: usize,
        /// Search every indexed repo in the workspace.
        #[arg(long, short = 'a', conflicts_with = "repos")]
        all: bool,
        /// Stop after this many matches.
        #[arg(long, default_value_t = 50_000, hide = true)]
        max_matches: usize,
        #[command(flatten)]
        repo: RepoArgs,
    },
    /// Files the content index covers and why any are skipped (for checking search coverage).
    #[command(hide = true)]
    Files {
        #[command(flatten)]
        repo: RepoArgs,
    },
    /// The next page of a list an answer cut: pass the answer's `cursor` (or a list's `<list>_cursor`) as it is. A
    /// cursor from before the index changed is refused: start again with the original call.
    More {
        cursor: String,
        #[arg(long)]
        json: bool,
    },
    /// Serve the tools over MCP (stdio).
    Serve {
        /// Default repo for tools called without one (default: the current directory's repo).
        #[arg(long = "repo", short = 'r')]
        repos: Vec<PathBuf>,
        /// The tools to advertise: `all` (default), `core`, and tool names, comma-separated (`core,kc_tests`).
        /// `KNOWCODE_TOOLS` does the same.
        #[arg(long)]
        tools: Option<String>,
    },
    /// Packages, targets and dependencies from manifests, and repo-to-repo edges across indexed repos.
    Deps {
        #[command(flatten)]
        repo: RepoArgs,
    },
    /// Clone the git repos this repo's manifests depend on into DIR, so cross-repo answers can reach them: dependencies
    /// written as git URLs (SwiftPM, XcodeGen, Cargo, npm), Terraform git module sources, GitLab CI includes of other
    /// projects, and dependencies on repos whose checkout is gone. Shallow, one directory each; only https and ssh
    /// remotes; DIR must be outside every repo. Uses your git credentials; the MCP server never clones.
    #[command(name = "fetch-deps")]
    FetchDeps(fetch::Options),
    /// Git history for the history-based answers (hotspots, coupled, ownership, reviewers).
    History {
        #[command(subcommand)]
        action: HistoryCmd,
    },
    /// Index coverage, freshness and schema.
    Status {
        /// Repo path, the same as `--repo`. Defaults to the repo containing the current directory, else every indexed
        /// repo.
        path: Option<PathBuf>,
        /// Repo to report on (repeatable).
        #[arg(long = "repo", short = 'r')]
        repos: Vec<PathBuf>,
        /// Accepted like the other commands' flag; `status` only checks freshness and never syncs.
        #[arg(long, hide = true)]
        no_sync: bool,
        /// Hash every file to find stale ones, rather than trusting an unchanged mtime and size.
        #[arg(long)]
        verify: bool,
        #[arg(long)]
        json: bool,
    },
    /// Delete every index in the cache directory. Notes on symbols live elsewhere and are never touched.
    Purge,
    /// Settings in config.toml, in the user config directory: show, get, set and unset them, or fold the legacy
    /// `cache_dir` and `ignore` files into it.
    Config {
        #[command(subcommand)]
        action: ConfigCmd,
    },
    /// Workspace roots: directories whose git repos (checkouts and worktrees) make up the workspace that cross-repo
    /// answers draw on. With none, the workspace is every indexed repo.
    Workspace {
        #[command(subcommand)]
        action: WorkspaceCmd,
    },
    /// Connect an MCP client. Claude installs the bundled plugin; Codex and Cursor register the local server.
    Setup(setup::Options),
    /// Remove recorded client registrations and cached indexes; offer to remove configuration and notes.
    Uninstall(uninstall::Options),
    /// Check and install the latest stable release for a shell-installer installation.
    SelfUpdate {
        /// Report availability without installing (also works for package-manager installs).
        #[arg(long)]
        check: bool,
    },
    /// One-shot release metadata helper, scheduled at most daily by normal commands.
    #[command(name = "_check-update", hide = true)]
    CheckUpdate,
    /// Run by `self-update` on the binary it installed: refresh a Claude Code plugin bundle an older version wrote.
    #[command(name = "_refresh-plugin", hide = true)]
    RefreshPlugin,
    /// Run as an agent hook: `claude-code` reads a Claude Code PreToolUse event (Grep, Glob or Bash) on stdin and, when
    /// the pattern names a symbol the index knows, prints context for the agent. Never fails: on any error it prints
    /// nothing and exits 0. See integrations/claude-code.
    #[command(hide = true)]
    Hook {
        /// The agent: `claude-code`.
        agent: String,
    },
    /// Measure index and query latency on indexed repos (for development measurements).
    #[command(hide = true)]
    Bench {
        #[command(flatten)]
        repo: RepoArgs,
        /// Names sampled per repo.
        #[arg(long, default_value_t = 50)]
        samples: usize,
    },
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Print the config file's path.
    Path,
    /// Every setting as it takes effect, with where it comes from (environment, config.toml, legacy file, default).
    List,
    /// A setting's value as it takes effect: one of the keys `config list` shows, or ignore.<repo>.
    Get { key: String },
    /// Set a key in config.toml. VALUE is TOML (`4`, `["~/Code", "~/work"]`) or else a string; a single path for
    /// workspace.roots or a pattern for ignore.<repo> becomes a list of one.
    Set {
        key: String,
        #[arg(allow_hyphen_values = true)]
        value: String,
    },
    /// Remove a key from config.toml.
    Unset { key: String },
    /// Fold the legacy `cache_dir` and `ignore` files into config.toml and rename them to `*.migrated`.
    Migrate,
}

#[derive(Subcommand)]
enum HistoryCmd {
    /// Fetch more history into a shallow clone (`git fetch --deepen=N`, 1000 by default, or `--shallow-since`), and
    /// report how deep it is before and after and what each history-based answer now has to read. A clone that isn't
    /// shallow is left alone.
    Deepen(history::DeepenOptions),
}

#[derive(Subcommand)]
enum WorkspaceCmd {
    /// Add a directory as a workspace root: every git repo under it joins the workspace.
    Add { dir: PathBuf },
    /// Remove a workspace root.
    Remove { dir: PathBuf },
    /// Each root, how many git repos it holds and how many of those are indexed.
    List,
}

#[derive(Subcommand)]
enum NoteCmd {
    /// Attach a note to a symbol: records its qualified name, kind, file, a hash of its body, HEAD and your
    /// git user.name.
    Add {
        /// `Type.member` or a name (the first definition, as `context` picks it).
        symbol: String,
        text: String,
        /// decision, gotcha or todo.
        #[arg(long)]
        tag: Option<String>,
        #[command(flatten)]
        repo: RepoArgs,
    },
    /// Notes on a symbol, or every note in the repo, each `current`, `stale` (since which commit, and by how many
    /// lines) or `orphaned`.
    List {
        symbol: Option<String>,
        #[command(flatten)]
        repo: RepoArgs,
    },
    /// Remove a note by id.
    Rm {
        id: i64,
        #[command(flatten)]
        repo: RepoArgs,
    },
    /// Every note, or one repo's, as JSON on stdout: every stored field (identity, body hash, commit, author), so
    /// `note import` on another machine restores them.
    Export {
        /// Only the notes of the repo at this path.
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// Add the notes of a `note export` file; notes already present are skipped, so importing twice adds nothing.
    Import {
        file: PathBuf,
        /// Put the notes (of one repo) under the repo at this path: the checkout moved.
        #[arg(long)]
        repo: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
}

/// A repo's root as notes key it: the git root above `path` when there is one, else `path` made absolute (the
/// checkout may not exist on this machine).
fn notes_repo(path: &std::path::Path) -> Result<PathBuf> {
    match kc_index::repo_root(path) {
        Ok(root) => Ok(root),
        Err(_) => Ok(std::path::absolute(path)?),
    }
}

fn main() {
    let cli = Cli::parse();
    let notify = !matches!(&cli.command, Cmd::Config { .. } | Cmd::Workspace { .. } | Cmd::Setup(_) | Cmd::Uninstall(_) | Cmd::SelfUpdate { .. } | Cmd::CheckUpdate | Cmd::RefreshPlugin | Cmd::Hook { .. });
    // Once per command, before it runs: a long-lived `serve` would otherwise check only when it exits.
    if notify { update::start(); }
    if let Err(e) = guard_privacy().and_then(|_| run(cli)) {
        eprintln!("knowcode: {e:#}");
        std::process::exit(1);
    }
    kc_index::cache::after_command();
}

/// Refuses a question whose arguments name a path an AI privacy file blocks (see [`ops::guard_inputs`]), before
/// anything is opened. Commands that index or manage the cache and config take no question.
fn guard_privacy() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_none_or(|c| ["index", "forget", "prune", "serve", "purge", "config", "workspace", "setup", "uninstall", "self-update", "_check-update", "_refresh-plugin", "help", "status"].contains(&c.as_str())) {
        return Ok(());
    }
    let mut repos = Vec::new();
    let mut it = args.iter().peekable();
    while let Some(a) = it.next() {
        if a == "--repo" || a == "-r" {
            if let Some(v) = it.peek() {
                repos.push(PathBuf::from(v.as_str()));
            }
        } else if let Some(v) = a.strip_prefix("--repo=") {
            repos.push(PathBuf::from(v));
        }
    }
    ops::guard_inputs(&repos, &args[1..])
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Cmd::Index { paths, workspace, threads, rebuild, scip, verify, json } => index(paths, workspace, threads, rebuild, scip, verify, json),
        Cmd::Forget { path } => {
            let root = std::fs::canonicalize(&path).unwrap_or(path);
            let removed = kc_index::forget(&root)?;
            // Its workspace summary goes with it; so do its notes in what its AI privacy file blocks.
            kc_index::workspace::forget_summary(&kc_index::db_path(&root)?)?;
            let privacy = kc_index::privacy::Privacy::load(&root);
            if privacy.blocks_anything() {
                kc_index::notes::purge(&root, |f| privacy.blocks(f))?;
            }
            println!("{} {}", if removed { "forgot" } else { "no index for" }, root.display());
            Ok(())
        }
        Cmd::Prune => {
            let report = kc_index::cache::prune()?;
            for entry in report.removed {
                println!("pruned {}", entry.root.display());
            }
            for entry in report.kept {
                eprintln!("retained {} ({})", entry.root.display(), entry.reason);
            }
            if report.deferred {
                eprintln!("cache maintenance is already running; retry prune later");
            }
            Ok(())
        }
        Cmd::Routes { repo } => {
            let out = ops::routes(&repo.scope())?;
            emit(&repo, &out.env, || {
                for (_, h) in &out.data {
                    render::http(std::slice::from_ref(h));
                }
            })
        }
        Cmd::Tables { name, all_callers, repo } => {
            let out = ops::tables_with(name.as_deref(), &repo.scope(), all_callers)?;
            emit(&repo, &out.env, || render::tables(&out.data, name.is_some(), repo.scope().repos.len() > 1))
        }
        Cmd::Events { repo } => {
            let out = ops::events(&repo.scope())?;
            emit(&repo, &out.env, || {
                if out.data.is_empty() {
                    println!("no message-bus publishes or consumers");
                }
                let entries: Vec<&ops::BusOut> = out.data.iter().map(|(_, b)| b).collect();
                render::bus_list(&entries);
            })
        }
        Cmd::At { location, repo } => {
            let (file, line) = location
                .rsplit_once(':')
                .and_then(|(f, l)| Some((f, l.parse::<u32>().ok()?)))
                .ok_or_else(|| anyhow::anyhow!("expected file:line, got `{location}`"))?;
            let out = ops::at(file, line, &repo.scope())?;
            emit(&repo, &out.env, || render::at(&out.data, out.env["file_indexed"].as_bool().unwrap_or(false)))
        }
        Cmd::Find { query, kind, limit, repo } => {
            let out = ops::find(&query, kind.as_deref(), limit, &repo.scope())?;
            emit(&repo, &out.env, || {
                let d = &out.data;
                if !d.items.is_empty() || d.manifests.is_empty() {
                    render::find(&d.items, d.total, d.multi, d.capped);
                }
                render::manifest_hits(&d.manifests);
            })
        }
        Cmd::Locate { query, limit, repo } => {
            let out = ops::locate(&query, limit, &repo.scope())?;
            emit(&repo, &out.env, || render::locate(&out.data))
        }
        Cmd::Refs { name, kind, limit, cite, repo } => {
            cite.enter();
            let out = ops::refs(&name, &kind, limit, &repo.scope())?;
            let d = &out.data;
            emit(&repo, &out.env, || render::refs(&name, &d.items, d.total, &d.by_confidence, d.multi))
        }
        Cmd::Context { symbol, budget, source, limit, cite, repo } => {
            cite.enter();
            let out = ops::context_limited(&symbol, budget, source.parse()?, limit, &repo.scope())?;
            emit(&repo, &out.env, || {
                if out.data.found.is_empty() && out.data.table_hits.is_empty() && out.data.repo_hits.is_empty() && out.data.file_hits.is_empty() {
                    println!("no definition named {symbol}");
                }
                for (_, r) in &out.data.repo_hits {
                    render::repo_ansible(r);
                }
                for f in &out.data.file_hits {
                    render::file_hit(f, out.data.multi);
                }
                for (i, ((((((root, ctx, http, angular, bus), tables), sql), health), sites), ansible)) in out
                    .data
                    .found
                    .iter()
                    .zip(&out.data.tables)
                    .zip(&out.data.sql)
                    .zip(&out.data.health)
                    .zip(&out.data.named_like)
                    .zip(&out.data.ansible)
                    .enumerate()
                {
                    render::context(root, ctx, out.data.multi);
                    render::area_line(out.data.areas[i].as_ref());
                    render::note_lines(&out.data.notes[i]);
                    render::named_like(ctx, *sites);
                    render::findings(health);
                    render::http(http);
                    render::bus(bus);
                    if let Some(a) = ansible {
                        render::ansible(a);
                    }
                    if let Some(t) = &out.data.terraform[i] {
                        render::terraform(t);
                    }
                    if let Some(a) = angular {
                        render::angular(a);
                    }
                    render::sql_facts(sql);
                    render::symbol_tables(tables, sql);
                }
                if !out.data.table_hits.is_empty() {
                    let models: Vec<_> = out
                        .data
                        .table_hits
                        .iter()
                        .map(|(root, t, links)| {
                            let model = kc_store::db::DbModel { tables: t.clone(), ..Default::default() };
                            (root.clone(), model, ops::ModelSql { tables: links.clone(), procedures: Vec::new() })
                        })
                        .collect();
                    render::tables(&ops::TablesData { models, sql: Vec::new() }, true, out.data.multi);
                }
            })
        }
        Cmd::Trace { symbol, direction, depth, limit, filter, cite, repo } => {
            cite.enter();
            let out = ops::trace(&symbol, &direction, depth, limit, &filter.filter(), &repo.scope())?;
            emit(&repo, &out.env, || {
                if out.data.found.is_empty() {
                    println!("no definition named {symbol}");
                }
                for (root, t) in &out.data.found {
                    render::trace(root, t, out.data.multi);
                }
            })
        }
        Cmd::Path { from, to, to_repo, max_hops, paths, max_nodes, max_repos, one_way, allow_ambiguous, edges, evidence, cite, repo } => {
            cite.enter();
            let edges = ops::edge_filter(&edges, &evidence)?;
            let opts = walk::PathOptions { max_hops, max_nodes, max_repos, paths, one_way, allow_ambiguous, edges, from_skip: 0, to_skip: 0 };
            let out = walk::path(&from, &to, to_repo.as_deref(), &opts, &repo.scope())?;
            emit(&repo, &out.env, || render::path(&out.data))
        }
        Cmd::Flow { entry, depth, limit, max_repos, edges, children, evidence, cite, repo } => {
            cite.enter();
            let opts = flow::FlowOptions { depth: depth.clamp(1, 16), max_steps: limit, max_repos, edges: ops::edge_filter(&edges, &evidence)?, children };
            let out = flow::flow(&entry, &opts, &repo.scope())?;
            emit(&repo, &out.env, || render::flow(&entry, out.data.as_ref()))
        }
        Cmd::Impact { symbol, file, diff, depth, limit, order, as_of, since, filter, cite, repo } => {
            cite.enter();
            let cochange = kc_index::vitals::Span { as_of, since };
            let p = ops::ImpactParams { symbol, file, diff, depth, limit, filter: filter.filter(), order, cochange, coupled_max: None };
            let out = ops::impact(&p, &repo.scope())?;
            emit(&repo, &out.env, || render::impact(&out.data, out.data.len() > 1))
        }
        Cmd::Unused { kind, tier, limit, repo } => {
            let p = refactor::UnusedParams { kinds: kind, tier, limit };
            let out = refactor::unused(&p, &repo.scope())?;
            emit(&repo, &out.env, || refactor::render_unused(&out.data))
        }
        Cmd::Rename { symbol, new_name, allow_ambiguous, repo } => {
            let p = refactor::RenameParams { symbol, new_name, allow_ambiguous };
            let out = refactor::rename(&p, &repo.scope())?;
            emit(&repo, &out.env, || refactor::render_rename(&out.data))?;
            if out.data.refused {
                std::process::exit(2);
            }
            Ok(())
        }
        Cmd::Coupled { file, limit, as_of, since, repo } => {
            let out = ops::coupled(&file, limit, &kc_index::vitals::Span { as_of, since }, &repo.scope())?;
            emit(&repo, &out.env, || render::coupled(&out.data, &file))
        }
        Cmd::Tests { targets, diff, limit, evidence, repo } => {
            let p = ops::TestsParams { targets, diff, limit, evidence };
            let out = ops::tests(&p, &repo.scope())?;
            emit(&repo, &out.env, || render::tests(&out.data, out.data.len() > 1))
        }
        Cmd::Hotspots { path, top, as_of, repo } => {
            let out = ops::hotspots(path.as_deref(), top, as_of.as_deref(), &repo.scope())?;
            emit(&repo, &out.env, || render::hotspots(&out.data, &out.env))
        }
        Cmd::Ownership { path, without, depth, top, as_of, repo } => {
            let p = ops::OwnershipParams { path, without, depth, top, as_of };
            let out = ops::ownership(&p, &repo.scope())?;
            emit(&repo, &out.env, || render::ownership(&out.data))
        }
        Cmd::Reviewers { files, diff, limit, as_of, repo } => {
            let p = ops::ReviewersParams { files, diff, limit, as_of };
            let out = ops::reviewers(&p, &repo.scope())?;
            emit(&repo, &out.env, || render::reviewers(&out.data))
        }
        Cmd::Overview { focus, map_budget, repo } => {
            let out = ops::overview(&repo.scope(), focus.as_deref(), map_budget)?;
            emit(&repo, &out.env, || render::overview(&out.env))
        }
        Cmd::Areas { area, repo } => {
            let out = ops::areas(area.as_deref(), &repo.scope())?;
            emit(&repo, &out.env, || render::areas(&out.data, area.as_deref(), &out.env))
        }
        Cmd::Note { action: NoteCmd::Add { symbol, text, tag, repo } } => {
            let out = ops::note_add(&symbol, &text, tag.as_deref(), &repo.scope())?;
            emit(&repo, &out.env, || render::notes(&out.data))
        }
        Cmd::Note { action: NoteCmd::List { symbol, repo } } => {
            let out = ops::note_list(symbol.as_deref(), &repo.scope())?;
            emit(&repo, &out.env, || {
                if out.data.is_empty() {
                    println!("{}", out.env["note"].as_str().unwrap_or("no notes"));
                }
                render::notes(&out.data)
            })
        }
        Cmd::Note { action: NoteCmd::Rm { id, repo } } => {
            let removed = ops::note_rm(id, &repo.scope())?;
            if repo.json {
                println!("{}", json!({ "removed": !removed.is_empty(), "id": id, "repos": removed }));
            } else if removed.is_empty() {
                println!("no note #{id} in the repos searched");
            } else {
                println!("removed note #{id}");
            }
            Ok(())
        }
        Cmd::Note { action: NoteCmd::Export { repo } } => {
            let repo = repo.as_deref().map(notes_repo).transpose()?;
            println!("{}", serde_json::to_string_pretty(&kc_index::notes::export(repo.as_deref())?)?);
            Ok(())
        }
        Cmd::Note { action: NoteCmd::Import { file, repo, json } } => {
            let text = std::fs::read_to_string(&file).map_err(|e| anyhow::anyhow!("reading {}: {e}", file.display()))?;
            let export: kc_index::notes::Export =
                serde_json::from_str(&text).map_err(|e| anyhow::anyhow!("{}: not a knowcode notes export: {e}", file.display()))?;
            let repo = repo.as_deref().map(notes_repo).transpose()?;
            let done = kc_index::notes::import(&export, repo.as_deref())?;
            let path = kc_index::notes::db_path()?;
            if json {
                println!("{}", json!({ "added": done.added, "present": done.present, "renumbered": done.renumbered, "path": path }));
            } else {
                let renumbered = if done.renumbered > 0 { format!(" ({} under new ids: theirs were taken)", done.renumbered) } else { String::new() };
                println!("added {} notes{renumbered}, {} already present, to {}", done.added, done.present, path.display());
            }
            Ok(())
        }
        Cmd::Docs { map_budget, repo } => {
            for (_, md) in ops::docs(&repo.scope(), map_budget)? {
                print!("{md}");
            }
            Ok(())
        }
        Cmd::Search {
            pattern,
            regex,
            ignore_case,
            smart_case,
            word,
            lang,
            glob,
            context,
            limit,
            offset,
            files,
            all,
            max_lines,
            mut repo,
        } => {
            if all {
                repo.repos = kc_index::indexed_roots()?;
            }
            let case = if ignore_case {
                search::Case::Insensitive
            } else if smart_case {
                search::Case::Smart
            } else {
                search::Case::Sensitive
            };
            let opts = search::SearchOptions { pattern, regex, case, word, langs: lang, globs: glob, max_lines };
            let p = ops::SearchParams { opts, limit, offset, context, files_only: files };
            let out = ops::search(&p, &repo.scope())?;
            emit(&repo, &out.env, || render::search(&out.data))
        }
        Cmd::GrepAst {
            pattern,
            lang,
            selector,
            inside,
            has,
            not_inside,
            not_has,
            text,
            glob,
            limit,
            offset,
            all,
            max_matches,
            mut repo,
        } => {
            if all {
                repo.repos = kc_index::indexed_roots()?;
            }
            let opts = kc_index::structural::AstOptions {
                pattern,
                lang,
                selector,
                inside,
                has,
                not_inside,
                not_has,
                regex: text,
                globs: glob,
                max_matches,
            };
            let out = ops::grep_ast(&ops::AstParams { opts, limit, offset }, &repo.scope())?;
            emit(&repo, &out.env, || render::grep_ast(&out.data))
        }
        Cmd::Files { repo } => {
            for (root, files) in ops::content_files(&repo.scope())? {
                for f in files {
                    let state = match f.state {
                        kc_store::content::INDEXED => "indexed".to_string(),
                        kc_store::content::PENDING => "pending".to_string(),
                        _ => format!("skipped:{}", f.skip.as_deref().unwrap_or("?")),
                    };
                    println!("{}\t{}\t{state}", root.display(), f.path);
                }
            }
            Ok(())
        }
        Cmd::More { cursor, json } => {
            let mut env = cursor::more(&cursor)?;
            if std::env::var_os(KEEP_RESUME).is_none() {
                cursor::seal(&mut env);
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&env)?);
            } else {
                render::page(&env);
                for note in ops::freshness_notes(&env) {
                    println!("{note}");
                }
            }
            Ok(())
        }
        Cmd::Serve { repos, tools } => {
            let tools = tools.or_else(|| std::env::var("KNOWCODE_TOOLS").ok()).unwrap_or_else(|| "all".into());
            // A plugin bundle an older version wrote is refreshed beside the server, never in its way.
            let refresh = std::thread::spawn(setup::refresh_claude_plugin);
            let served = mcp::serve(ops::Scope { repos, no_sync: false }, &tools);
            let _ = refresh.join();
            served
        }
        Cmd::FetchDeps(options) => fetch::run(options),
        Cmd::History { action: HistoryCmd::Deepen(options) } => history::deepen(options),
        Cmd::Deps { repo } => {
            let out = ops::deps(&repo.scope(), true)?;
            emit(&repo, &out.env, || render::deps(&out.data.packages, &out.data.edges))
        }
        Cmd::Status { path, repos, no_sync: _, verify, json } => {
            let out = ops::status_with(&ops::Scope { repos: path.into_iter().chain(repos).collect(), no_sync: false }, verify)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&out.env)?);
            } else {
                if let Some(config) = out.data.first().map(|v| &v["config"]) {
                    render::config_status(config);
                }
                out.data.iter().for_each(render::status);
            }
            Ok(())
        }
        Cmd::Purge => purge(),
        Cmd::Config { action } => match action {
            ConfigCmd::Path => {
                println!("{}", kc_index::config::path()?.display());
                Ok(())
            }
            ConfigCmd::List => settings::list(),
            ConfigCmd::Get { key } => settings::get(&key),
            ConfigCmd::Set { key, value } => settings::set(&key, &value),
            ConfigCmd::Unset { key } => settings::unset(&key),
            ConfigCmd::Migrate => settings::migrate(),
        },
        Cmd::Workspace { action } => match action {
            WorkspaceCmd::Add { dir } => settings::add(&dir),
            WorkspaceCmd::Remove { dir } => settings::remove(&dir),
            WorkspaceCmd::List => settings::list_roots(),
        },
        Cmd::Setup(options) => setup::run(options),
        Cmd::Uninstall(options) => uninstall::run(options),
        Cmd::SelfUpdate { check } => update::self_update(check),
        Cmd::CheckUpdate => update::probe(),
        Cmd::RefreshPlugin => {
            setup::refresh_claude_plugin();
            Ok(())
        }
        // Any agent but a known one: nothing to add, and the tool runs.
        Cmd::Hook { agent } if agent == "claude-code" => hook::run(),
        Cmd::Hook { .. } => Ok(()),
        Cmd::Bench { repo, samples } => bench::run(&ops::target_repos(&repo.repos)?, samples, repo.json),
    }
}

/// Set by the MCP server on its search child: keep what cursors are made from in the envelope it prints.
pub(crate) const KEEP_RESUME: &str = "KNOWCODE_KEEP_RESUME";

impl RepoArgs {
    fn scope(&self) -> ops::Scope {
        ops::Scope { repos: self.repos.clone(), no_sync: self.no_sync }
    }
}

/// Prints the JSON envelope with `--json`, else the text rendering, followed by what the envelope says is stale or
/// missing (files that could not be read).
fn emit(args: &RepoArgs, env: &serde_json::Value, text: impl FnOnce()) -> Result<()> {
    let mut env = env.clone();
    ops::add_cuts(&mut env);
    contract::finish(&mut env);
    // What cursors are made from stays in the envelope only for the MCP server's search child, whose size cap renews
    // them (see `mcp::search_in_child`).
    if std::env::var_os(KEEP_RESUME).is_none() {
        cursor::seal(&mut env);
    }
    let env = &env;
    if args.json {
        println!("{}", serde_json::to_string_pretty(env)?);
    } else {
        text();
        for note in ops::freshness_notes(env) {
            println!("{note}");
        }
    }
    Ok(())
}

fn index(
    paths: Vec<PathBuf>,
    workspace: Option<PathBuf>,
    threads: Option<usize>,
    rebuild: bool,
    scip: Option<PathBuf>,
    verify: bool,
    json: bool,
) -> Result<()> {
    let mut roots = Vec::new();
    if let Some(ws) = workspace {
        roots.extend(kc_index::discover_repos(&ws)?);
    }
    for p in &paths {
        roots.push(kc_index::repo_root(p)?);
    }
    if roots.is_empty() {
        roots.push(kc_index::repo_root(&std::env::current_dir()?)?);
    }
    if scip.is_some() && roots.len() > 1 {
        anyhow::bail!("--scip names one repo's SCIP index: index one repo with it");
    }
    let mut opts = SyncOptions { scip_compare: true, ..Default::default() };
    opts.verify |= verify;
    if let Some(t) = threads {
        opts.threads = t.max(1);
    }
    let started = Instant::now();
    let mut rows = Vec::new();
    let mut seeds = kc_index::Seeds::default();
    let mut changed = false;
    for root in &roots {
        let _writer = kc_index::writer::WriterLock::acquire(root)?;
        if rebuild {
            kc_index::forget(root)?;
        }
        changed |= !kc_index::db_path(root)?.exists();
        let mut store = seeds.open(root)?;
        if let Some(file) = &scip {
            kc_index::scip::set_source(&store, file)?;
        }
        let report = kc_index::sync(root, &mut store, &opts)?;
        changed |= report.parsed + report.removed > 0;
        let content = kc_index::content::sync_content(root, &mut store, opts.threads)?;
        drop(store);
        let bytes = kc_index::index_bytes(root)?;
        let privacy = kc_index::privacy::Privacy::load(root);
        if !json && privacy.whole() {
            println!("{:<16} blocked: {} (its {}); not indexed", repo_name(root), privacy.detail(&repo_name(root)), kc_index::privacy::file_name());
        } else if !json {
            if privacy.blocks_anything() {
                println!("{:<16} {} blocked path(s) left out ({})", repo_name(root), privacy.blocked().paths.len(), privacy.detail(&repo_name(root)));
            }
            println!(
                "{:<16} {:>5} files  {:>5} parsed  {:>4} removed  {:>8.1} ms  {:>7.1} MB index",
                repo_name(root),
                report.files,
                report.parsed,
                report.removed,
                report.total_ms + content.total_ms,
                bytes as f64 / 1e6
            );
            for f in &report.failed {
                println!("  failed: {f}");
            }
            if let Some(s) = &report.scip {
                print_scip(s);
            }
        }
        rows.push(json!({ "repo": root, "index_bytes": bytes, "report": report, "content": content }));
    }
    if roots.len() > 1 && changed {
        // Brings the workspace summary up to date once, so the first query doesn't reread every index.
        kc_index::workspace::all_summaries()?;
    }
    let total_ms = started.elapsed().as_secs_f64() * 1000.0;
    if json {
        println!("{}", serde_json::to_string_pretty(&json!({ "repos": rows, "total_ms": round1(total_ms) }))?);
    } else if roots.len() > 1 {
        println!("{} repos in {:.1} s", roots.len(), total_ms / 1000.0);
    }
    Ok(())
}

/// One ingest of a SCIP index, for `index`.
fn print_scip(s: &kc_index::scip::Report) {
    if let Some(e) = &s.error {
        println!("  scip: {} unreadable: {e}", s.source);
        return;
    }
    println!(
        "  scip: {} ({}): {} of {} documents covered ({} stale, {} not indexed); {} reference sites, {} implementations",
        s.source, s.tool, s.covered, s.documents, s.stale, s.unindexed, s.refs, s.impls
    );
    let Some(a) = &s.agreement else { return };
    println!("  scip: of {} references in covered files, {} answered: {} upgraded to exact, {} corrected", a.refs, a.matched, a.upgraded, a.corrected);
    println!(
        "  scip: {} member implementations and overrides, {} of them also found by name",
        a.member_impls, a.member_impls_declared
    );
    if !a.constructors.is_empty() {
        let n = |k: &str| a.constructors.get(k).copied().unwrap_or(0);
        println!(
            "  scip: constructor overloads of {} creations: {} the one SCIP names, {} ambiguous among it, {} another, {} the implicit one",
            a.constructors.values().sum::<usize>(),
            n("same"),
            n("among"),
            n("other"),
            n("implicit")
        );
    }
}

fn purge() -> Result<()> {
    let report = kc_index::cache::purge()?;
    if report.deferred { println!("cache maintenance is already running; try again"); return Ok(()); }
    println!("removed {} cached indexes", report.removed.len());
    for kept in report.kept { println!("kept {} ({})", kept.root.display(), kept.reason); }
    for notes in [kc_index::config::notes_path()?.0, kc_index::notes::legacy_path()?] {
        if notes.exists() { println!("kept {} (durable notes)", notes.display()); }
    }
    println!("coordination locks and unrelated cache files are retained");
    Ok(())
}
