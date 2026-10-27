# Changelog

KnowCode follows [Semantic Versioning](https://semver.org/) and stays on 0.x while its surface settles.

## Unreleased

- Swift code in `extension A.B` reads unqualified names as Swift does: B's members and the file's declarations, not
  A's. In `extension Box.Keyed { func sup() -> Decoder }`, `Decoder` is Swift's protocol, not `Box.Decoder`, and
  `extension Box.Decoder: Decoder` reads its conformance at the file's scope. Code written inside `A`, and an
  extension of a top-level type, read as before. `X.Y.init(…)` keeps `X` as its qualifier, as `X.Y(…)` does. Swift
  files are re-extracted once.
- `context` on a file lists the file's top-level declarations in line order (`declarations`: kind, name, line,
  end_line, and `members` for a type), at most 40 with `declarations_total` and a cursor for the rest, so the next call
  can pick one by name or `file:line`. Before, it gave only the file's area and notes, which sent a client to read the
  file. A file the AI privacy file blocks still answers nothing.
- Task context: `kc {op: task, q: "refunds fail for partial captures"}` (and `knowcode task`) answers a goal in words in
  one bounded answer, where a client joined `locate`, `context`, `impact` and `tests` itself: the files most likely
  involved with the symbols that match and the lines declaring the best of them; what depends on them and what usually
  changes with them; the tests to run, advisory (a test not listed is never said to be safe to skip); and what the
  index can't settle (goal words nothing matches, ambiguous edges, constructs not read). Each section says whether it
  is whole and the call that continues it. Rows are short text: the answer costs about what `locate` alone does.
- **Worktrees share an index base.** Checkouts of one repo (worktrees, clones) share an immutable base of its index
  at a commit. A checkout clean at a commit publishes one; a new checkout's index starts as a copy-on-write clone of
  the nearest base (APFS, btrfs, XFS; a copy elsewhere) and indexes only what differs from it. Answers are those of a
  cold index of the same tree: `scripts/worktree-gate.py` checks it on public repos, with the freshness cases (a new
  untracked file, a same-size edit with its mtime restored) in every worktree. Worktrees that start at once on a
  commit with no base index it once: one builds the base, the others wait for it (bounded) and clone it.
  `cache.shared_bases` turns it off.
- A shared base a worktree still uses is never evicted; bases nothing uses are evicted with the other idle indexes,
  least recently used first. A removed worktree's index goes at the next query, `status` or `prune`.
- **Provenance records.** `--provenance` on the command line, or `provenance: true` on any MCP tool call, adds a record
  for durable records: the build and every extractor's version; the call's hash; the answer's hash and an `evidence`
  hash that is the same for the same answer from any index or worktree; and per repo read its worktree identity, full
  commit, a digest of its uncommitted content, index generation, the base it was cloned from and its privacy policy's
  digests. Compact answers are unchanged.
- `impact` and `trace` list the stops past their frontier in the order of their places, not of the index's row ids.
- An answer naming a file the extractor couldn't read whole says so: `coverage` lists each such file with its count of
  parse errors, the first error's line and column, and the answer's own symbol when the error lies in its span, with an
  `unsupported` entry saying declarations, members, calls and references there may be missing. `status` still lists
  them all.
- Capabilities: `kc {op: capabilities}` (also the resource `knowcode://capabilities` and `knowcode capabilities`)
  describes every op as JSON, for a client building its own tool surface instead of copying the help: the full input
  schema with each number's bounds and each closed set's words, what `q` and `qs` fill and which ops take no query,
  whether an op reads or writes (notes are the only writes), the evidence presets, the answer's format and the keys
  it always has, and the server, MCP protocol and index schema versions. It is read from the tables help and the
  advertised schemas come from, and a test checks each op's entry against the running server. It costs nothing up
  front: `kc`'s schema is unchanged, and `all` leaves the `kc_capabilities` tool out unless named. The `kc` help
  topic's list of what `q` fills is now made from the same table.
- `kc_unused` refuses a `tier` other than unused, possibly or all, where it listed everything.
- Restricted mode: `knowcode serve --restricted --repo DIR --ops find,refs,… --cache-dir DIR --config-dir DIR`, for an
  automated pipeline. The server is bound to the repos given: nothing else is discovered or indexed, and another repo,
  or a path outside them, is refused with the new code `not_allowed`, as is any op not in the allowlist. It needs
  explicit cache and config directories and writes nothing outside the cache. It runs no update check or plugin
  refresh, and keeps git off the network. It states its scope in the `initialize` reply
  (`capabilities.experimental["knowcode/scope"]`), on stderr and in the capabilities. docs/usage.md has a headless
  recipe, with the real git for macOS's `/usr/bin/git` shim. An acceptance test runs it under a macOS sandbox
  profile that kills it on any write outside the cache or any network connection.
- Completeness said apart: a partial JSON answer adds `completeness` (`graph`: the graph it read was whole; `source`:
  its quoted source is whole, not a skeleton or cut; `lists`: no list cut or continuing) and `size` (`max_chars` asked,
  `chars` returned, the source mode in effect, and each continuation's cursor, rows left and about how many
  characters they take), in the room the size cap leaves, never a row's. A whole answer, and every text answer, is as
  it was.
- Stable codes: every refused call carries a machine code beside its English, in the tool result's
  `structuredContent.error` (`invalid_argument`, `not_found`, `ambiguous`, `no_files`, `not_indexed`, `stale_cursor`,
  `withheld`, …), so a client never tells refusals apart by their wording. An `incomplete` entry's reason stays its
  code, with `code` where it says more: `indexing` (ask again after it finishes), `not_indexed`, `gone`. A refused
  input of a batch, and the batch's entry for it, carry the code too, and a refused rename preview has `codes` beside
  its `reasons`. Privacy now says the same policy (`privacy: {repo, file, block_at, blocks}`) on a call refused for a
  blocked path, a repo refused whole and a `withheld` entry. The capabilities list the codes and reasons; the
  replay gate fails a build that codes its refusals if any refusal it replays has none.
- An `exact` reference names one declaration. A call of an overloaded name, or of same-named members of one type,
  was `exact` with every candidate as its target. Now a Swift call's argument labels pick its overload (`put(_:)`,
  `put(into:)`, a trailing closure's parameter), a type's bare name or a value picks a static or an instance member
  (Swift, TypeScript), and of one name in two Swift modules the caller's own module's wins. Otherwise the reference is
  `ambiguous`, every candidate listed, and walks no longer follow it as if certain. TypeScript's merged declarations (a
  module's `const` and `type` of one name, overload signatures) and a C# `partial` member stay one. On the identity
  gate's compiler labels, `exact` precision rises from 0.675 to 0.924 (Swift), 0.839 to 0.972 (C#) and 0.878 to 0.963
  (TypeScript), with no label made newly wrong and no recall lost. Swift files are read again (call labels).

## 0.5.1 (2026-10-27)

0.5.1 meets the two checks 0.5.0 missed, and moves the repository to bubstackdev/know-code. Rows of `impact` and `trace`
lead with their call site as `file:line`: on the recorded calls of the public studies, two-hop impact recall is 0.833
read as `file:line` alone, where 0.5.0 had 0.607. `history` reaches a caller that calls through a library's interface,
and history-v1's caller label now carries the `optional` flag it dropped; KnowCode meets plain git on the caller
questions either way. `impact` also stops calling some answers complete that weren't (an override leaving out a default
parameter), and keeps every way a row was reached; constructors are found by the names their source gives them; Swift
files with constructs the parser misses lose less code, and a Swift member added to a standard-library type no longer
captures others' uses. Shell installs of 0.3.0 through 0.5.0 must re-run the installer once to update. The [release
gate](eval/release-gate/0.5.1.md) makes no model calls; `impact` answers grew 10.4% there, past its 10% tolerance, most
of it one answer that 0.5.0 left wrongly empty.

- A query opening a repo whose AI privacy file had just changed, while another query or a background link step was
  still reading that repo's index, failed with "index is in use: …; close its readers and retry". Removing the index
  made under the old policy now waits up to 30 seconds for those readers to finish. `knowcode forget` still refuses
  at once while the index is in use.
- The repository moved to [bubstackdev/know-code](https://github.com/bubstackdev/know-code); the binary, crate and
  plugin are still `knowcode`, and old links redirect. **Shell installs of 0.3.0 through 0.5.0 get no update notice,
  and their `self-update` fails** ("installer URL is outside the expected KnowCode release"): they accept releases
  only under the old name. Re-run the installer once:

  ```sh
  curl --proto '=https' --tlsv1.2 -LsSf https://github.com/bubstackdev/know-code/releases/latest/download/knowcode-installer.sh | sh
  ```

  From then on `self-update` works, accepting installer receipts that name either repository. The Claude Code plugin
  stays at 0.5.0 until you run `knowcode setup claude` again. Homebrew installs are unaffected.
- A member a Swift repo adds to a standard-library struct (`extension Int32 { var isFinite … }`) no longer captures
  uses of the same name on other library types. On a value of another library type (`v: Float`), or of a generic
  parameter, it is never linked: the fallback that links a framework type's member to the repo's extension of a
  framework protocol skips library structs, which nothing conforms to. On an untyped value (`for (x, y) in zip(a, b)`,
  `x.isFinite`), a use reached in a dependency is `external_likely`, not `unique_name`, when the name is a common
  library member's or the member is there to satisfy one of the dependency's protocol requirements, as it is within a
  repo. `isFinite`, `isNaN`, `magnitude`, `rounded` and other number members join the common library members, and
  more sized number types count as standard-library types (`Int8` to `UInt64`, `Float16` to `Float80`, `Decimal`).
- A member reached on a library type through a protocol it conforms to (`v.isFinite`, `v: Float`, resolving to
  `GridScalar.isFinite`) says so: the reference's `basis` is `via protocol`, since the member that runs is the
  type's own, outside the repo. When the conformance (`extension Float: GridScalar {}`) is in a SwiftPM target the
  code neither is in nor imports, the use is the library member's alone and stays unresolved.
- A new worktree or clone seeded from another checkout's index kept that checkout's facts for a file whose size and
  mtime matched what the other checkout recorded (a copy that kept mtimes, `cp -p`), whatever its content: the first
  sync now hashes every file. A repo that is one Ansible role (`tasks/main.yml` at its root) named its role after the
  other checkout's directory; its Ansible files are extracted again.
- `unused` counted another worktree (or clone) of the same repo as a user of the repo's packages, so its answer
  changed with how many checkouts of the repo were indexed: their public API turned `possibly unused` and
  `published` named the packages. Checkouts of one origin are one repo here too.
- Swift files with constructs tree-sitter-swift doesn't read lose less of their code. When a file has parse errors,
  it is parsed again with them blanked (positions kept), and the tree with less of the file in errors is used: the
  underscored ownership modifiers (`__shared`, `__owned`, `__consuming`, `_const`), `@_lifetime(…)`, module
  selectors (`Mod::Name`, `#Mod::macro`), `nonisolated(unsafe)`, `unsafe` and `sending` before an operand, and then
  `#if` blocks whose branches are whole (around attributes, between a switch's cases). An empty tuple (`f(())`) and a
  member named by a keyword (`case .try:`) no longer count as parse errors: the grammar reads them with a zero-width
  `!` and the tree is whole. Over 4,845 files of 15 public Swift repos, files with parse errors fall from 605 to 160,
  and 256 declarations are read that weren't, none lost. Swift files are re-extracted once.
- `status` says where each of the worst files' first parse errors are (`first`: `at` line:col and the text there).
- `context` on a type lists the members and extensions of the declaration it picks, not those of every type with its
  name. Two Swift modules each declaring `Tile`, or a file's `private struct Tile` beside a module's public one, used
  to share one member list. An extension goes to the declaration it can see: the one in its file, else its module's,
  else that of a module its file imports; a `private` or `fileprivate` type is seen from its own file only. An
  extension nothing narrows to one (its file imports two modules declaring the type) is listed under each, and each
  context says so (`extensions_ambiguous`).
- A C# call written on a type reached a same-named extension method of another type: `string.IsNullOrEmpty(s)`
  resolved to a repo's `IsNullOrEmpty(this Guid? g)`, and `string.Compare(a, b, c)` to repo `Compare` methods, so
  `impact`, `refs` and `trace` listed those calls as callers. A predefined type keyword (`string`, `int`, …) as
  receiver is now the type, and a call on a type reaches only that type's members and extensions on it.
- `impact` listed each dependent once, by its best way, and dropped the others with their lines: a method calling
  the change on one line (hop 1) and, on others, a method that calls it (hop 2) named only the first line. A row now
  keeps its other ways in `ways` (`{via, hop, confidence, site, also}`), and in text last on its
  row (`(also via Store.Drop [exact] at Bin.cs:8, Bin.cs:9)`). A capped text answer leaves them out first, from the last row up,
  and says how many, so no row is cut for them.
- A C# override or implementation that leaves out its base member's default parameter value
  (`virtual M(User u, bool deep, Query q = null)`, `override M(User u, bool deep, Query q)`) wasn't matched to the base
  member, so calls through the base (`folder.M(user, true)`) never reached it: `impact` on the override answered
  "nothing affected" and called that complete. Members now match by how many parameters they take, whichever have
  defaults, and then by parameter types as before.
- In `impact` and `trace` (callers) text, a row now leads with its use site, the line to open (`Till.cs:8`), then
  its other sites as `file:line` and the line its symbol is declared at (`(declared at line 6)`). Before, a row led
  with the declaration and gave the site as `at line 8`, which an agent or a script reading `file:line` places
  missed: since rows across a library boundary joined the walk, their call sites too. The JSON is unchanged.
- A constructor is found by the name its source gives it: `Factory.__init__` (Python), `Cart.constructor`
  (TypeScript, JavaScript) and `Ledger.Ledger` (C#), as well as by `init`, the name the index stores. Where a class also
  has a method named `init`, `Factory.init` is that method, with the constructor listed after it to pick by place.
  Before, `Factory.__init__` found no definition. A call written `init` (`self.init()`, `this.init()`) now resolves
  to that method alone, no longer to the constructor too.
- `kc_impact`'s help said its walk goes up to 16 hops while its `depth` parameter said 1 to 5; it walks up to 16.
  Every bounded parameter (`depth` of impact, trace and flow; path's `max_hops` and `paths`; search's `limit` and
  `context`; grep_ast's `limit`; `max_chars`) now takes its schema line, its help text and the clamp its answers apply
  from one table, so they can't disagree. `max_chars` now says its maximum doubles for several inputs, as it always has.
- `kc {op: status, q: ""}` (and `unused`, `hotspots`): an op taking no query takes an empty `q` as none, where it
  was an error. The `kc` topic says which ops take no `q`.
- `impact`, `trace` (callers) and `history` reach an implementation of a library's interface member from the calls
  in the repo through that interface (`_cached.ForContext<T>()` on Serilog's `ILogger`, in a class implementing
  `ILogger`) when no indexed repo defines the interface. Before, such a call resolved to nothing and reached no
  implementation. It is `ambiguous` (the library's own implementation may run instead), so listed, not walked past;
  a call of another overload (by argument count and the argument types the call site gives) isn't listed. With the
  library indexed, the call is listed once, in the walk, not again in `cross_repo`. `history`'s
  `callers_changed_with` marks such a caller `ambiguous`.

## 0.5.0 (2026-10-27)

0.5 answers questions about a method's history and walks across library boundaries. `history` reads one definition's git
history over the lines it spans: the commits that changed it, who wrote its current lines, and the files and direct
callers that changed with it. On history-v1's public questions, labelled from git alone, it answers the 10 method-level
questions exactly, at least as high in recall and precision as each plain-git approach, and higher in one. On the two
caller questions it falls short of `git log -S` (1 of 2, against 2 of 2 with one wrong commit): it misses a caller that
calls through a dependency's interface (Serilog's `ILogger`), which `impact` doesn't reach; fixing that is 0.5.1 work.
`coupled` lists more of the files that change with a file than git's own log does. `impact` and `trace` walk from a
library's method on to its callers in the repos using it, where they stopped one hop across. Calls through a declared
client package link through the package's own code, C# actions serve every route they declare, and recorded `search`
calls no longer fail. The [release gate](eval/release-gate/0.5.0.md) makes no model calls; it measures what KnowCode
returns, and makes no claim about how agents use it.

- `impact` and `trace` (callers) walk on across a library boundary: from a method or property, to its calls in the repos
  depending on its repo (resolved into it) and on to their callers there; from an implementation of an interface
  member a dependency declares, to the calls through that member in the dependency and the repos using it. A row
  across the boundary has its `repo` and `library` (`{library, through}`). Calls through an interface are
  `ambiguous` (another implementation may run): listed, not walked past under the default policy, with the frontier's
  call going on from them; `evidence: all` walks past them. Before, these were one hop deep, in `cross_repo` only.
  **`cross_repo` changes:** it no longer lists a site the walk lists, so its count drops where the walk reaches
  across. Library crossings are looked up for at most 64 walked symbols an answer (then `incomplete` says how many
  were left). A continue call names a symbol by `file:line` when overloads share its file.
- `history SYMBOL` (`kc_history`; `kc {op: history, q: …}`): one definition's git history, read over the lines the
  index says it spans. It lists the commits that changed those lines (`git log -L`, through edits and renames of the
  file), the newest and, when the whole history was read, the oldest, who last changed its current lines (`git blame
  -w` over the definition), and the files changed in the same commits (commits of more than 50 files left out). A
  method's history is its own, not its file's. Uncommitted edits that move it are allowed for. It reads at most 200
  commits, and the MCP server stops git at the call's time budget, keeping what it read; the answer says when
  either happened. `history deepen` is unchanged. On the history-v1 development questions (public repositories at
  full history, labels from git alone), it answers the 10 method-level questions exactly.
- `history` lists a symbol's direct callers whose files changed in its commits (`callers_changed_with`), each with
  how many of its commits and the newest few, tests marked; `callers_total` counts every direct caller. Callers come
  from `impact`'s first hop in the repo; those in the symbol's own file aren't listed, since each of its commits
  changes that file. On history-v1's two caller questions it names the one production caller of each label that
  `impact` reaches; the other, a call through Serilog's `ILogger` inside Serilog.Extensions.Hosting, isn't reached.
- `coupled` ranks partners by shared commits, the most recent first among equals, where it ranked them by shared
  commits weighted by age (half per 90 days). Over a long span the weighting buried the strongest partners: with
  `--since all`, Autofac's `ResolveOperation.cs` listed one of the ten files sharing the most commits with it in its
  top ten, and a rule file touched by tooling sweeps first. Now its top ten holds nine of them, and the tenth is next.
  The age weighting is still in each partner (`weight`, shown as `recency`). A partner that isn't code and shared under
  a fifth of its own commits with the file (a rule or settings file that sweeps touched) comes after the others.
  Against history-v1's two file co-change questions (labels from git alone, renames followed), `coupled --since all`
  lists 51 of 55 and 14 of 14 of the labelled files at the label's size.
- The default `kc` tool names the history questions: its description lists `history`, and the server's instructions
  say which op answers a symbol's commits and authors (`history`) and the files changing with a file (`coupled`).
  Before, agents learned of them only from `help`. To stay within the 1,300-byte budget, the instructions no longer
  say answers are read from the working tree. The tool and instructions are 1,290 bytes and 501 tokens
  (count_tokens), from 1,283 and 502.
- `search` takes a look-ahead at the end of a regex or a look-behind at its start, which agents write to find uses
  that aren't calls: `\bSetParent\b(?!\()`, `(?<!\.)Save`. The rest of the pattern finds each match and the
  look-around is checked on its line. Such patterns were refused as invalid; look-around anywhere else still is.
- A call to an external client the repo declares in a `.d.ts` (`declare module 'jellyfin-apiclient'`) links
  through the client package's own code when it is installed in the repo's `node_modules` at the version the
  lockfile pins: to the action serving the request the package's method makes, assuming `package_code`, with
  `package` naming the package, version, method and its file and line (a minified bundle is read from its source
  map's original sources). Only declared packages are read, the repo's privacy file applies, links aren't followed,
  nothing is fetched or written into the repo; `index.node_modules = false` turns it off. On the public-v1
  workspace, with jellyfin-web's lockfile-pinned packages installed, 194 calls link through the package (87 that
  linked nothing, 43 to other actions than the name guess, among them 11 to the `…Legacy` twin), and public-v1's
  http-cross-repo-005 finds its one missed file.
- A call to an external client the repo only declares in a `.d.ts` (`declare module 'jellyfin-apiclient'`), with
  no tag (`apiClient.updateUserConfiguration(…)`), no longer links `exact` by the method's name: the client's code,
  where the request is made, isn't read, and it may request the action's `…Legacy` twin instead (on public-v1's
  workspace, at least 10 of 179 such links went to the wrong one of the two). These links are now `ambiguous`,
  include the `…Legacy` twin, and assume `client_unread`.
- C#: an action with several `[Http*]` attributes serves each of their routes, where only the first was read:
  `LibraryController.GetSimilarItems`'s `[HttpGet("Items/{itemId}/Similar")]` and four more, and the
  `[HttpHead]` of actions answering GET and HEAD. A named argument (`[HttpGet("Css.css", Name = "…")]`,
  `[HttpPost(Name = "…")]`) is no longer taken for the template when it comes first. On the public-v2 workspace this
  adds 42 server routes (34 in jellyfin-server, 8 in bitwarden-server) and links one more request
  (`LiveTv/Tuners/Discover`).
- TypeScript and JavaScript: a request or generated-client call in code no named function or class encloses (an
  anonymous `export default function`, a callback inside it, a function assigned to a property) is kept as the
  file's, as top-level statements are, where it was dropped: jellyfin-web's `ApiClient.getLiveTvSeriesTimers(query)`
  in `self.preRender = function () {…}`. On the public-v1 workspace this adds 25 request rows (21 in jellyfin-web,
  4 in bitwarden-clients' specs), 9 of them linked.
- `knowcode setup codex` says whether Codex runs KnowCode's session-start hook yet, read from Codex's own record of
  the hooks you trusted: if not, the one step left ("Hooks need review" → "Review hooks" when Codex starts, or
  `/hooks`), and if it changed since you trusted it, why. `knowcode status` gives the same line for each Codex
  profile set up (`codex_hooks` in `--json`). KnowCode only reads that record; trusting the hook stays yours to do in
  Codex. `knowcode uninstall` also removes Codex's record of trusting KnowCode's hook.
- For a Homebrew install, setup names `opt/knowcode/bin/knowcode` when it would otherwise name the versioned keg,
  which `brew upgrade` removes: the client configuration, and the Codex hook you trusted, keep working across
  upgrades.

## 0.4.1 (2026-10-26)

0.4.1 fixes two faults in 0.4.0, found by its own checks: one that could make an answer claim to be complete while
missing a just-made edit, and one that made some `impact`, `trace` and `context` queries scan every symbol.

- An answer no longer says it is complete while it misses an edit. A query that found indexing already under way
  waited for that job, and took its report as fresh even when the job had read the tree before the edit, so under
  load an answer could say `complete` without the edit's symbol. It now waits once more on a job that reads the
  current tree, within the same time budget, or says the repository is still indexing.
- Faster `impact`, `trace` and `context` on code implementing interfaces: four lookups of a symbol by its name (the
  supertype walk run for every implementation member reached among them) compared names case-sensitively in a way
  SQLite couldn't serve from the name index, so each scanned every symbol. They now use the index, as the other
  lookups do; answers are unchanged. On the public-v1/v2 workspace, `impact` at depth 2 retires a median 19% fewer
  instructions, and up to 93% fewer on large C# repos (jellyfin-server, bitwarden-server).

## 0.4.0 (2026-10-26)

0.4 makes KnowCode cheaper to keep in a session and more exact across repositories. By default a client now loads one
tool, `kc`, instead of eight: 502 tokens on every turn instead of 1,276, measured with the provider's token counter.
Claude Code sessions, and Codex sessions once their hook is trusted, open with a line saying what KnowCode indexes there
and which call answers callers and impact. HTTP links that pointed at the wrong server are gone, and the impact walk
reaches further: replaying every call recorded in the published studies, two-hop impact answers find more of the
labelled places (recall 0.587 to 0.628) with fewer wrong ones, and no question type got worse. From 0.4 on, releases ship on [checks that make no model calls](eval/release-gate/0.4.0.md); they
measure what KnowCode returns, and make no claim about how agents use it.

- C#: a value of a type parameter constrained to one type (`where T : BaseItem, new()`) has that type's members:
  `new T().SetParent(…)` is `BaseItem.SetParent`, which `impact` and `trace` now reach, where it was an unresolved
  call. Several type constraints, or one naming another type parameter, leave it unresolved. Calls through an
  interface member reach only an implementation taking the same arguments: a private `Find(query, parent)` overload
  is no longer reached by calls of the interface's `Find(query)` (on public-v2's two-hop-impact-007 this removed 7
  false hop-2 rows). In an extension method an unqualified call is the declaring class's (its overload, or an
  extension called in static form), never the extended type's member; an extension whose `this` parameter starts a
  line is recognised as one.
- A TypeScript or JavaScript request whose URL is a `const` of the function making it links as if the URL were
  written in the call: `const url = apiClient.getUrl('Collections', query)` passed as `$.ajax({ url })`, and
  `const url = 'Videos/' + itemId + '/Subtitles/' + index` passed to `apiClient.getUrl(url)`, which weren't read
  before. The request names the local (`local: url at src/a.js:44`) and its links assume `url_local`; requests
  passing such a `const` straight to `fetch(url)` or `send("GET", path)`, linked before, now say so too. A URL
  builder or `{ url }` option given a `let`, `var`, parameter, a `const` of an enclosing function or the module, a
  name declared twice in the function, or a `const` built from another local holding URL text, isn't read.
- `impact` on an implementation of an interface member declared in a dependency (Serilog.Extensions.Hosting's
  `ReloadableLogger.ForContext<T>()`, implementing Serilog's `ILogger.ForContext<T>()`) lists, in `cross_repo`, the
  calls through that member in the dependency and in the other repos using it (`Log.ForContext<T>()` in serilog,
  `RequestLoggingMiddleware` in serilog-aspnetcore), `through ILogger`. Another implementation may run for each, so
  they are `ambiguous`: listed, not walked past. Before, an implementation's interface members were looked up only
  in its own repo, and these calls were missing.
- C#: a conditional that splits a declaration (`ILogger ForContext(Type t)`, then `#if X` a body, `#else` a `;`,
  `#endif`) no longer loses it. tree-sitter reads a directive only between declarations or statements, and its
  error recovery lost the declaration and often its type's other members: Serilog's `ILogger` had no members, and
  `PropertyBinder` no class. A C# file that parses with errors is read again with each conditional's first branch
  (the others blanked, so every line and column still points at the file), and that reading is kept when it parses
  better. Files that parse are read as before. On the public-v1/v2 workspaces this changes 11 serilog files and
  one bitwarden-server file, and no other repo's index. An interface's static member (`private static readonly
  object[] NoPropertyValues`, now read in `ILogger`) has no implementations: a class's field of the same name is its
  own.
- An impact row whose caller reaches the changed symbol on several lines lists them all: `at line 526 (also 532, 592,
  602)` in text, `also` in JSON. Before, one line per caller was named; agents in 0.3's study grepped for the rest,
  and a two-hop question's label counts each call line. Replaying that study's KnowCode calls, impact answers grow
  by 0.1% in bytes (4 of 27 change).
- **Changed default: one MCP tool.** `knowcode serve`, and so `setup claude`, `setup codex` and `setup cursor`,
  now advertise a single tool, `kc`, instead of the eight core tools. Its `op` names the tool to run, without `kc_`
  (`impact`, `find`, `refs`, `context`, `search`, `locate`, `more`, `help`, and every other tool by name); `q` fills
  the op's main parameter and any other parameter is passed by its own name: `kc {op: impact, q: Orders.Place,
  depth: 2}`. `kc` and its instructions cost 502 tokens on every turn (`count_tokens`), against 1,276 for the core
  tools in a client that loads every schema and 1,325 for the plugin's tools in Claude Code. Answers' hints and
  cursors, the server's instructions, the Claude Code plugin's skills, its search note and its session-start line
  name calls as ops (`kc {op: more, cursor: …}`). On a small development set in Claude Code, sessions with KnowCode
  named in the prompt used `kc` in 12 of 12; differences in whole-session tokens there were within run-to-run
  spread, so no saving per session is claimed before the 0.4 study. **To keep the separate tools**, set
  `KNOWCODE_TOOLS=core` or `all` (`KNOWCODE_TOOLS=all knowcode setup claude` for the plugin, or in the environment
  `setup codex` and `setup cursor` run in); everything then reads as before. A plugin updated from 0.3, which wrote
  `KNOWCODE_TOOLS=all` as its default, takes `one`; one you set by hand is kept.
- `knowcode setup codex` adds a user-level `[[hooks.SessionStart]]` entry to Codex's `config.toml`, beside the MCP
  server, that runs KnowCode's session-start hook (the line Claude Code's plugin adds). Codex runs it only once you
  trust it in `/hooks`, and setup says so. The entry names only the binary and KnowCode's directory overrides, so the
  line can change without trusting it again. Codex keys that trust to the entry's place among the `SessionStart`
  entries, so setup adds the entry last and, run again, updates it where it is: neither it nor your own hooks move,
  and Codex keeps trusting them. `knowcode uninstall` removes it and keeps your own hooks. When your hooks follow it,
  uninstall leaves an empty entry in its place, with a comment saying why, so that they keep their place and their
  trust.
- The Claude Code plugin adds a `SessionStart` hook. At the start of a session, and after `/clear` or a compaction, it
  adds one line of about 40 tokens where KnowCode has an index: `KnowCode indexes shop. Callers, impact, HTTP:
  kc {op: impact}; definitions: op find.` (with `KNOWCODE_TOOLS=core` or `all`: `kc_impact` and `kc_find`) It says nothing in a directory with no index, in a repo whose privacy file blocks
  KnowCode, or in a resumed session. It reads no index and makes no network call. `knowcode setup claude` installs it
  with the plugin, `knowcode uninstall` removes it, and `knowcode config set hooks.session_start false` turns it off.
- Claude Code loads five tools up front instead of eight: `kc_find`, `kc_context`, `kc_refs`, `kc_impact` and
  `kc_search`. `kc_locate`, `kc_more` and `kc_help` are still advertised, and deferred to tool search: in 0.3's study
  `kc_locate` and `kc_help` were never called and `kc_more` 3 times in 320 sessions, while every schema loaded up front
  is paid on every turn. The always-loaded schemas go from 1,031 to 743 tokens (`count_tokens`); with the plugin's
  `KNOWCODE_TOOLS=all`, KnowCode's upfront cost with tool search goes from 1,570 to 1,325 tokens, instructions and
  deferred names included. A cut answer's `hint` already names the `kc_more` call that continues it, now checked by a
  test for every core tool that pages. Clients that don't defer tools, such as Claude Code's bare mode, still
  load all eight core tools.
- Conventional routes are per app. A repo with several ASP.NET apps (an admin app and an API, each with its own
  project and `MapControllerRoute`) placed every controller with no `[Route]` under every app's templates. A
  controller now takes the templates of its own project (the nearest `.csproj` above it) and of the projects
  referencing it, never another app's; an action with no template that no app's templates reach is listed as
  unsupported, saying so. On the public-v1 workspace, 18 routes of controllers declared in bitwarden-server's test
  projects, which no app maps, are gone; nothing else changed.
- A call through a typed field is judged by the field's type, not its name. `this.chromeStorageApi.remove(key)`,
  with `chromeStorageApi: chrome.storage.StorageArea`, was read as a generated API client's operation because of
  the field's name, and linked `ambiguous` to two server actions named `Remove`. A receiver's type, from a field, a
  constructor parameter property or a local, now decides: a class of the repo's with that method is the repo's own
  code (as for `new Client(…)` since 0.3.0), and a type named for no API client is no client. A generated client's
  type (`ItemsApi`) and an external client the repo declares in a `.d.ts` still are. On the public-v1 workspace, 15
  operation rows are gone, 2 of which had links (both the `remove` above); no other link changed.
- Default actions are served at their controller's route. A conventional template's defaults
  (`{controller=Home}/{action=Index}/{id?}`, or `defaults: new { action = "Index" }`) let a URL leave a trailing
  segment out when it holds the default: `OrdersController.Index` is served at `Orders` as well as `Orders/Index`, and
  the default controller's `Index` at the root. Where an action constrained to the request's verb serves the path,
  an action answering every verb at the same path is no longer a candidate, as ASP.NET Core prefers the constrained
  one. On the public-v1 workspace, bitwarden-server's admin app gained 7 routes (`Organizations`, `Providers`, `Users`,
  `Login` ×2, `Home` and the root), and the admin test fixture's `GET /login` and `POST /login` now link, `exact`, to
  `LoginController.Index`. C# files are re-extracted.
- Requests whose path is given to a function building the URL are read when the function is known to add only a
  query string. `this.send("GET", addEventParameters("/ciphers/" + id + "/events", start, end, token))`, with
  `addEventParameters` (in another file) appending `?start=…&end=…` to its first parameter, requests
  `ciphers/{}/events`. A function that may change the path leaves the request unread, and the answer names it
  ("its URL is built by withVersion(…), which KnowCode doesn't read"), never a guessed path. A URL chosen by a
  condition (`all ? "/sync?all=true" : "/sync"`) is read when both choices have one path. On the public-v1
  workspace, bitwarden-clients gained 11 requests, all linked `exact`: `ApiService.getSync` and the nine
  `ApiService.getEvents*` that #89 left unread, and `EventLogApiService.getEventsSend`. TypeScript files are
  re-extracted.
- `knowcode self-update` and the daily update check work when GitHub's API refuses them. The API allows 60
  unauthenticated requests an hour per network, and on a shared address self-update failed with `curl: (56) The
  requested URL returned error: 403`. When the API refuses, KnowCode reads the latest tag from the release page
  (`releases/latest` redirects to it) and installs this platform's release archive, checked against the SHA-256
  published beside it; nothing is installed without that check. The installer's own checksum is only in the API's
  answer, so the archive is used instead. It installs as the installer does under self-update: the binary replaces
  the old one by a rename at the same path, mode 0755, the receipt is left as it is (the installer leaves it too), a
  binary that does not report the new version is rolled back, and the plugin is refreshed. Neither path checks
  GitHub's build attestations; both trust a SHA-256 GitHub serves for the release's own files. A platform with no
  release archive (an Intel Mac) has no fallback and gets the message below. The API is not asked again until its
  limit resets.
  When the release page fails too, self-update says so: `GitHub's API rate limit for this network is used up; try
  again after 14:05, or set KNOWCODE_GITHUB_TOKEN`. With `KNOWCODE_GITHUB_TOKEN` set, the API request uses it (passed
  to curl in a private file, not on its command line); it is never required. The daily check never mentions a
  refusal.

## 0.3.0 (2026-10-26)

0.3 makes KnowCode keep up with a whole workspace on its own and measures what agents spend re-checking its
answers. Repositories under the workspace roots are found and indexed in the background, several at once and sized to
the machine, and answers say which ones aren't indexed yet. The [public, pre-registered study](eval/public-v2/results.md)
on a new question bank missed all four of its decided criteria: re-check cost rose instead of halving (most of the
rise is one session), sessions used about 3.5k more tokens than plain tools as in 0.2.0, recall on the bank's one
lower-bound question stayed below plain, and agents not told about KnowCode used it in 53 of 80 sessions against the
67 required, up from 44. The results page reports every run, miss and failure.

- `index --phases` says instructions and cycles weren't counted on a machine that offers the counters but doesn't
  count (a virtual Mac reads 0), and shares phases by CPU time there, instead of reporting no work.
- A call to an external client whose method the repo only declares (a `.d.ts` for a package such as
  `jellyfin-apiclient`), and which links to no action, now says so: its note names where the method is declared and
  that the client's code, and so its route, isn't in the workspace. Such calls (`apiClient.markUnplayed(…)`) were
  listed with no link and no note in `routes`. On the public-v1 workspace, 400 of jellyfin-web's unlinked calls carry
  the note; links are unchanged. The client's own method-to-route mapping isn't in the workspace, so they stay
  unlinked.
- An API wrapper's own requests are read: `this.send("DELETE", "/ciphers/" + id + "/attachment/" + …)` in a class
  named for an API, client or HTTP (`ApiService`) is a request of its method, as `this.apiService.send(…)` from
  another service already was. On the public-v1 workspace, bitwarden-clients' `ApiService` gained 98 requests: 94
  link, `exact`, to the action serving them, and 4 are left unlinked (no action serves their path). TypeScript files
  are re-extracted.
- A call through a class the client repo implements itself is no longer read as a generated API client's operation.
  `const client = new Client(options); client.login(user)` in an importer, `Client` a class of the repo's, was
  linked by name, `exact`, to the server's `SsoController.Login`. A call whose receiver's type the repo implements
  with a method of that name now reaches that code only; a type the repo only declares (a `.d.ts` for an external
  client) still counts as a client. A one-word operation (`update`, `approve`) not under the controller its client's
  tag names now links `ambiguous` at most: many APIs' methods share such a name. On the public-v1 workspace, 346
  operation rows that were calls into the repo's own code are gone (one had a link: the false `login` one), and six
  one-word operations of clients naming no controller went from `exact` to `ambiguous` (two of them, `update` and
  `policies`, were linked to the wrong controller).
- An ASP.NET controller with no `[Route]`, routed by `MapDefaultControllerRoute` or `MapControllerRoute`, no longer
  takes the `[Route]` of another controller of its name. Classes were matched by name only, so in a repo with an
  admin app and an API each declaring `OrganizationsController`, the admin app's `[HttpPost]` actions were placed at
  the API's `POST organizations` and linked, `exact`, to the client calling the API. They are now placed by the
  conventional template (`Organizations/UnlinkOrganizationFromProvider`, ASP.NET Core dropping `Async` from action
  names). An action without a template that no conventional route places is now unplaced and listed as such, where
  it was served at the root path; a prefix same-named base classes disagree on places nothing. On the public-v1
  workspace, 19 wrongly placed admin actions moved to their conventional routes, two requests to the API's
  `POST organizations` lost 6 false `exact` links each, and two tests posting to an admin route now link to its
  action. C# files are re-extracted.
- A controller action's `context` now says first who calls it over HTTP: `called_over_http` gives each route it
  serves and its first five callers as `repo/file:line Symbol`, ahead of its callees and code callers. The links were
  already in `http`, but after the callers, which for an action are mostly its tests; agents asking who calls an
  endpoint read those and went to grep the client repo. The callers hint for an action now points to `kc_impact`,
  which follows the routes, instead of `kc_refs`, which matches the name.
- The server starts indexing the next new repo under the workspace roots as soon as one finishes, instead of on its
  next 2-second tick. Most repos of a workspace index in well under that, so a free slot sat idle: on a 111-repo
  public workspace, the background averaged 0.3 repos at once against a plan of 1, and 1.1 to 1.75 against a plan of
  4. It now averages 0.93 and 3.4, and indexed the workspace in about 100 s instead of about 260 s at one at a time
  (41 s instead of 65 to 69 s at four), for the same instructions.
- A workspace-wide answer holds at most 254 MiB of SQLite page cache over 500 repos. It keeps every index it
  reads open until it answers, and each held up to 4 MiB of page cache (about 2 GiB over 500 repos). Past the first
  index a call opens, each one's connections now hold 256 KiB. Answers are the same: ten workspace-wide `search`,
  `locate`, `find`, `refs`, `hotspots` and `deps` calls over 111 public repos gave identical output. They took 0 to 8%
  more instructions (`search` most).
- Indexing several repos at once no longer spends most of its time in the kernel. SQLite was built with one page
  cache and one allocation counter for the whole process, each behind a global lock, and every connection's page
  fetches and allocations took them. Each connection now has its own page cache, and allocations aren't counted. On
  a workspace of 111 public repos, `index --workspace` used 114 s of user and 355 s of system CPU and 1,236 G
  instructions before, and 75 s user, 36 s system and 949 G instructions after (one run each on a loaded machine).
  The server no longer sets SQLite's 4 MiB process-wide soft memory limit: it needs that counting, and each
  connection's page cache is still bounded by its own size.
- A query that reads a repo while the server indexes it in the background no longer fails with "database is
  locked". The query wrote caches (the history walk, derived values) that waited on the indexer's write, and the
  indexer, at background priority on a loaded machine, could hold it past SQLite's 5 s busy timeout. Such a cache
  write is now skipped: the query answers from what it computed, and the cache is written next time. The background
  file ranking no longer starts on a repo that is still being indexed (a query could queue it between the index's
  sync and its contents), where the two writers each waited for the other; it is built once the indexing is done. On
  main, the workspace gate failed this way in 1 of 4 runs.
- `kc_impact`'s description now says what it answers: who calls a method or an HTTP endpoint, from any repo, hop by
  hop (`depth` 2 for callers' callers). `kc_refs` and the server's instructions point there too. Some clients leave
  the instructions out (Claude Code's bare mode), so the descriptions carry the map on their own. The core tools
  cost fewer tokens on every turn: parameter lines whose names say enough are gone (`symbols`, `names`, `queries`,
  `kc_more`'s `cursor`), `kc_impact` no longer advertises `file` (still taken, and documented in `kc_help`), and the
  instructions are shorter. By the token-counting endpoint, the eight core schemas went from 1,151 to 1,031 tokens and
  the instructions from 256 to 227.
- A workspace-wide answer says once which repos it left out because they aren't indexed yet. The server noted them
  twice, once as the answer finished and again as it sent it; when the background finished indexing a repo in
  between, the answer carried two "not yet indexed" entries with different counts. The workspace gate read them
  together and, in one run, counted nine repos as left out silently.
- `index` walks the history for the areas beside the file ranking instead of after it. The walk is a `git log` child
  that needs nothing from the ranking; on a repo with a long history it ran alone for most of a cold `index`. On a
  public TypeScript password manager client (8,177 files), a cold `index` took 19.4 to 20.7 s instead of 26.9 to
  34.6 s (three runs each on a loaded machine, so indicative only), for the same instructions (207 G) and CPU time.
  The areas are byte-identical. The background, which ranks on one thread, still walks after ranking.
- `coupled`'s `static_link` and `tests`' reverse walk now run on the symbol graph that `trace`, `impact`, `path` and
  `flow` use, with the same gate, and the old in-repo walker is gone. Their answers don't change: on the 60 most-changed
  source files of each of four public repos with history, 240 `coupled` and 240 `tests` answers are the same before
  and after.
- `context`'s `touches_tables` gives the same answer however the repo was indexed. When a method reaches a table
  several ways at one hop, the row now names the first by file and line, not the first the index happened to store.
  A walk cut at its limit visits symbols in file and line order, so it stops at the same place every time. Before,
  indexing with several parser threads (the default) could change the row's `via` and `site`, and which tables a cut
  walk listed. On the public study's password manager server, all 884 controller methods now answer the same with
  one parser thread and with the default. Against 0.2's single-thread answers, 94 of them name another `via` for
  245 rows, each one the same table at the same hop, and no table is added or dropped.

- The file ranking (the reference graph behind `overview`'s map and entry points, `hotspots`' coupling, `areas`, and
  the order of long `refs` and `impact` lists) costs a seventh to a fortieth of what it did, with the same rankings.
  Resolving every name in turn read again, for each name, what doesn't depend on it: a C# repo's project files and
  `using`s, a TypeScript repo's whole file list, imports and Angular root providers. That is now read once per graph,
  and two queries finding an Angular provider's root injector and `InjectionToken`s start from the few sites by name
  instead of every import or top-level symbol. Building the graph from a cold index, in instructions on one thread:
  a small Python web framework 0.80 to 0.48 G, a .NET media server 156 to 22 G, a .NET password manager's server
  664 to 93 G and its TypeScript clients 3,401 to 81 G. A name's references resolve a few thousand at a time,
  and no longer sorted: the server's peak resident memory building it went from 1,177 MiB to 182 MiB. Several
  threads may share the work (two at most for now: on a loaded machine more cost more instructions than they
  saved), each within the memory a repo's indexing is planned for. The graphs, ranks and areas are byte-identical
  before and after on those four repos.
- `knowcode index` builds the file ranking and the areas when they are missing, not only when an earlier query had
  computed them, and a server builds them in the background, at background priority and one repo at a time, for each
  repo it indexes for the first time and each indexed repo it finds without them: the first `overview`, `hotspots`
  or `areas` call no longer computes them. A cold `index` costs that much more: on the four repos above, 1.8 to 2.4,
  73 to 100, 514 to 624 and 116 to 209 G instructions, with peak resident memory up to 626 MiB (the clients). A
  query needing the graph while the background builds it waits for that build rather than building it again. One
  that only orders a long list by it (`refs`, `impact`, `overview`'s entry points) waits until its freshness
  deadline, then answers in index order and says so (`deadline`).
- A cold `knowcode index` takes less memory at its peak, with the same index, rankings and answers. Most of the peak
  was the history walk behind `hotspots` and `areas`, a `git log` that mapped the repo's pack files and cached delta
  bases with git's defaults: it now runs (as do the ownership walks) within 64 MiB of mapped pack and a 32 MiB delta
  cache. The content index's trigram pairs, freed before the file ranking is built, stayed counted against the
  process; they are now mapped and unmapped on their own. Peak resident memory of a cold index on the four repos
  above, over two runs each: about the same on the smallest, a tenth less on the media server, a fifth less on the
  password manager's server and less than half on its clients. The walk alone on the clients went from 631 to 121
  MiB, for 9% more of git's instructions.

- `context`'s DI flag `conflicting` counts plain registrations only. A `TryAdd*` registration stands back when the
  service has one already, `TryAddEnumerable` adds one of several implementations meant to be resolved together, and
  `Replace` takes the place of the one before: none of them is a second implementation competing for the service, and
  each was counted as one. On the public study's workspace 7 services lose a false `conflicting` flag (a password
  manager's push engines, event writers and integration services registered in exclusive branches through `TryAdd*`
  or `TryAddEnumerable`); no other answer of `context` on its 1,565 DI services changes. Each index is rebuilt once on
  the first query after updating (index schema 21); endpoint registrations whose routes can't be placed are now kept
  with the other constructs KnowCode doesn't read, and answers on them are unchanged.
- `kc_search`'s first page shown whole stops after the last hit in code. Since 0.2.0 a first page at the default limit
  showed every hit when there were at most 60 and they fit; hits in tests, comments and prose past the first 20 now
  stay a page away, and `rest` says that is what they are (all of them still show when a search finds nothing else).
  In the public study's sessions agents used 47 rows past the first 20 of such answers, every one a line of code
  outside tests; none of the 55 test or 30 comment rows there. On the 497 distinct `kc_search` calls of
  the public-v1 runs and a held-out A/B run, answers are 1.0% smaller (901,956 to 893,090 bytes): 13.8% of what
  showing every hit added (64,455 bytes over a 20-row page), with every row agents used still shown.

- A generated API client's operation links only to actions in the servers its repo is known to call: the repo itself,
  a repo it depends on, the one repo serving another of its requests or a host its settings name, or the one repo a
  tagged operation of its links to. Before, an untagged operation (`this.apiClient.getUser()`) linked to a same-named
  action in any indexed repo. With no server known, the links stay, `ambiguous`, with the new assumption
  `server_unverified`. On a public workspace of a media server, a password manager and their web clients, 16 of the
  media client's operations linked into the password manager's server; none do now, and its 349 links into its own
  server are unchanged.
- Microsoft DI service descriptors are registrations: `services.Add(new ServiceDescriptor(typeof(IFoo), typeof(Foo),
  ServiceLifetime.Scoped))`, `ServiceDescriptor.Singleton/Scoped/Transient(…)` and `ServiceDescriptor.Describe(…)`
  handed to `Add`, `TryAdd`, `TryAddEnumerable`, `Replace` or `Insert`, with the lifetime they state and the
  implementation, factory or instance they name. A keyed descriptor is listed as unread. On the public study's
  workspace this adds the 23 production registrations written this way (authorization handlers, push engines,
  hosted services, an OpenAPI provider replaced through `Replace`); none were read before.
- `kc_search` checks repos for freshness on worker threads; those workers now run as the call: a cancelled call stops
  them, the call's freshness wait applies there, and what they note (gone checkouts, repos left out) reaches the
  answer. The repos an answer read from a cached summary, and the indexes it used, are kept per query instead of per
  thread.
- A cited site carries its statement, not just its line. When the call, request or reference a row cites is part of a
  statement over several lines, `cite` quotes the rest of it, at most 2 lines each side: the arguments a call's line
  opens, the builder call a `.SingleInstance()` line continues, the condition of a `? :` branch, the `case` label a
  line runs under. `at` is the range quoted; `cut` says where a longer statement starts or that it goes on. This
  covers `kc_refs` items, `kc_context` callers (and callees its `source` doesn't quote), `kc_impact` rows,
  `kc_trace` nodes and `kc_path` and `kc_flow` steps; text trees mark the statement's other lines with `-`. A line
  that stands alone, or a member of an initializer in braces, still quotes one line. Over MCP the size cap drops
  statement lines, from the last row up, before it cuts a row. On the 67 distinct `kc_refs`, `kc_context`,
  `kc_impact` and `kc_trace` calls of the public study's runs, answers grow 1.7% (295,622 to 300,592 bytes) and show
  the same 720 rows.
- `index --phases` reports what each indexing phase cost: opening the index, discovery, stat, parse, writing rows,
  committing, SCIP, the text content index (read and write) and derived data (file ranking and areas, which it computes
  so they are counted), each with wall and CPU time, the CPU time of child processes (git), and on macOS the
  instructions retired and the phase's share of them. `scripts/index-phases.py` runs it cold and warm over repos and
  prints the medians.
- Repos under the workspace roots are indexed by a running server without an `index` command or a query touching
  them ([#40](https://github.com/bubstackdev/knowcode/issues/40)). The server checks the roots when it starts and as
  workspace queries arrive, reads a new repo's AI privacy file first (a repo blocked whole is never indexed), and
  indexes new repos in the background, smallest first, never ahead of a query's own indexing, and only while the cache
  is below nine tenths of `cache.max_size`. Until then, a workspace-wide answer says "N repositories found, not yet
  indexed" in one `incomplete` entry naming them, instead of one entry per repo or none, and `status` marks each
  `background: "queued"` (or `cache_full`). A repo removed from a root drops out of the workspace.
- Indexing adapts to the machine ([#41](https://github.com/bubstackdev/knowcode/issues/41)). `index` of several repos
  (`--workspace`, or several paths) indexes them at once, as many as half the memory and every core but one allow,
  with at most 8 parser threads a repo; `--jobs` sets how many. Background indexing of the workspace's new repos reads
  the cores, memory, load, memory pressure and power source every few seconds: idle and on power it indexes a repo
  per four cores, under load, on battery or under memory pressure one at a time on one thread, and under critical
  memory pressure it starts none. Queries still come first.
- Workspace-wide MCP answers (`workspace: true`) read the repos with a full index and name the rest in their "not
  yet indexed" entry, whatever form the answer takes, instead of opening every repo of the workspace: a `kc_search`
  over a workspace of new repos indexed each one in its search child, which has no freshness budget, until the child's
  deadline cut the answer to nothing. When no repo has a full index yet, the answer reads the smallest.
- `scripts/workspace-gate.py` is the workspace gate: it clones a pinned public workspace (`scripts/workspace-gate/repos.txt`,
  111 repos), runs `serve` with no `index` command, and checks that every repo becomes indexed and current, that a
  repo added under the root is noticed and indexed, and that workspace answers name every repo not indexed yet.

## 0.2.0 (2026-10-22)

0.2 set out to make answers that agents use without re-checking them, at a lower cost than reading files, and to
show it in a [public, pre-registered study](eval/public-v1/results.md). On the released build the study met two of
its four criteria: answers were as accurate as the plain-tools arm (F1 and recall equal), and agents used KnowCode
without being told to in 24 of 24 sessions. It missed the other two: agents still re-checked about half of
KnowCode's answers (the bar was a halving of the baseline rate), and sessions used about as many tokens as plain
ones, not fewer. The results page reports every run and failure.

- `serve` advertises the `core` tools by default: `kc_find`, `kc_locate`, `kc_context`, `kc_refs`, `kc_impact`,
  `kc_search`, `kc_more` and `kc_help`. A client that doesn't defer tools loads every advertised schema and the
  server's instructions on every turn; for the default that is now 3,462 characters, down from 7,186 (all 25 tools).
  `serve --tools all` (or `KNOWCODE_TOOLS=all`) still advertises every tool, and a comma list adds tools by name
  (`core,kc_trace`). `kc_trace` left the core set (kc_impact answers callers by hop). The core tools are the ones
  marked always loaded (`anthropic/alwaysLoad`), so under `all` Claude Code loads the same eight up front and defers
  the rest. The Claude Code plugin's server sets `KNOWCODE_TOOLS=all` unless the user set it, since its skills use
  the other tools. The server instructions map questions to the core tools again (where X is defined, every use of X, what X
  is, where a concept lives, text, what calls X or breaks if it changes), leading with "use it instead of grep and
  reading files", and say every answer states `complete`.
- The size cap cuts the lists an answer is about last: implementations and conformers, DI registrations, HTTP, bus and
  SQL links. Callers, callees and members go down to none, the source is shortened and those lists' rows drop their
  signature and doc first; links are text rows with what they share said once. On a public DI library, `kc_context` of
  an interface cut 7 of its 11 conformers to keep callers; it now shows all 11. On a public media server, a controller
  action's context showed 3 of its 39 HTTP callers; it now shows all 39 in the same 6,000 characters.
- An ASP.NET controller marked `[Route("")]` serves its actions at the root: the empty prefix replaces a base
  controller's `[Route("[controller]")]`, as ASP.NET takes it, where KnowCode placed them under the base's prefix
  (`Widgets/Items/{id}` for `Items/{id}`). On a public media server 23 controllers do this; 16 requests from its web
  client and 16 from its own tests now link to their actions, and none lost a link.
- A `kc_search` page cut short names the other files with a match (`other_files`, by directory, with counts, up to 60
  files), so the whole set is seen without paging or searching again.
- `complete` speaks for what was asked. A list on the side of it that a limit or `max_chars` cut (a symbol's callers,
  callees and members, the tables its calls reach, one reference's candidate targets, its source, cites past the first
  few without their line) is a new `more` entry, with the call for the rest, and the answer stays `complete`; the lists
  an answer is about (its items, implementations, DI registrations, links, a walk's nodes) are still `incomplete` when
  cut. `kc_find` cut only past every match with the name asked for, and `kc_search` cut with every file with a match
  named, say what the rest are (`rest`) and are `complete`. In a public A/B study most re-checks after a KnowCode answer
  followed `complete: false` from a cut side list; on the 14 such calls of one arm, all 14 now answer `complete: true`
  (77,966 to 81,170 characters).
- `kc_search` shows every hit on a first page at the default limit when there are at most 60 and they fit `max_chars`.
  On the study's 64 `kc_search` calls, the 29 that were cut are now `complete`, 17 of them showing every hit; answers
  grow from 178,576 to 206,676 characters, 26,934 of it on those 17 (the 35 uncut answers are unchanged).
- A container registering itself is a DI registration: a registration built from its parts (`new
  ComponentRegistration(…)` or `base(…)` in a class deriving it) registers each `TypedService(typeof(X))` it names as
  its activator's type. On a public DI library, `kc_context` of its resolving interface now lists the registration.
- A `file:line` or `path#name` written after its repo's name (`shop/src/a.cs:12`, the way questions and cross-repo
  answers name places) resolves in `kc_context`, `kc_impact`, `kc_trace` and `kc_refs` (which answers for the
  definition there); `kc_search` takes `**/shop/src/**` as it takes `shop/src/**`. A place naming no definition, and a
  search filter matching no file, are refused saying what didn't resolve (no such file, or the definitions nearest the
  line), where they answered `complete: true` with no items.
- MCP answers say `complete: true` again when they are complete (0.2.0's leaner answers said it only when false). A
  text tree ends its first line with `; complete`.
- `find`'s `declared` speaks for the workspace: "the only declaration of X in the workspace (6 repos) is in autofac",
  or each repo when several declare it, whichever repo the lookup was scoped to (the other repos are ruled out by their
  summaries, or opened when they may declare or name it; a repo without a full index is never indexed for it and is
  said not checked, and other checkouts of the repo asked are left out). Its reference count covers every repo. When files KnowCode
  doesn't read as code name the symbol too, the count is "at least N" and those lines are named.
- JavaScript under an app's own `src/.../plugins/` is indexed. It was taken for vendored jQuery plugins and left out,
  so uses through named imports and namespace imports (`import * as m`, `m.f()`) there were missing from `refs` and
  `declared` counted too few (2 references instead of 8 on a public web client). A `plugins/` directory outside `src`,
  or under a web root within it, is still vendored.
- Notes about files and constructs KnowCode doesn't read are checked before they are made. Razor files (`.razor`,
  `.cshtml`) are searched as text for the symbol: `refs` and `impact` name the lines that name it, and when none does,
  `refs` says so in `checked` and stays `complete`, instead of a note that the files exist. `refs` also names lines in
  other files not read as code (vendored JavaScript) that name it. An assembly scan bears on a type only when its
  argument's assembly (`typeof(T).Assembly`, `GetExecutingAssembly()`) is a project holding the type or an
  implementation of it; a scan of an assembly the code doesn't name is an `unsupported` entry only for a type the
  container knows (registered or injected), and otherwise a clause of its "DI registrations: none found" line.
- A repo indexed under `GIT_CONFIG_GLOBAL=/dev/null`, as sandboxes and CI set it, is no longer said to have "changed
  since its last index, and indexing is queued or running" on every answer. The freshness check stamped the global
  git config by its mtime, and the mtime of `/dev/null` moves on every write to it anywhere on the machine; a device
  or pipe is now stamped by its kind alone.
- DI constructs KnowCode doesn't read that can't register the type asked about are left out of the `unsupported`
  entry and counted in its "DI registrations: none found" line: those in test code (a test's own container), for a
  type outside it, and, for an interface, an assembly scan exposing only other services (`.As<IModule>()`). On a
  public DI library, `context IComponentContext` listed 13 assembly scans (12 in tests, 1 registering only modules);
  it now lists none and says why. Razor files are checked as text for any number of names up to 200, in batches,
  where more than 20 used to give the note that the files exist; a Razor file search can't read is named on its own.
  `checked` says "none of 59 Razor files (.razor, .cshtml), checked as text, names X".
- Lower memory in search and indexing, with answers unchanged. Trigram lists are intersected one at a time, in place, starting from the smallest; indexing workers allocate the trigram set only for files they index; a file read reserves its own size instead of growing twice. Path globs can no longer take exponential time: a bounded matcher takes over after a fixed amount of backtracking. The revision and manifest-version caches keep at most 1,024 entries each, so a long-running server no longer grows with every checkout state it has seen.
- Leaner MCP answers and tool list. A clean, current answer no longer lists its repos: `repos` and `provenance` are
  left out unless a repo is dirty or was answered from its last sync, or something is `stale`, `failed` or
  `missing_repo`. `complete` is said only when it is `false` (no `incomplete` means complete), and false flags, nulls,
  `offset: 0` and a search's `stats` are left out. `stale` is one `incomplete` entry naming repos by name, not path.
  `kc_search` and `kc_grep_ast` hits, and `kc_more` pages of them, are text rows (`repo/file:line kind symbol |
  text`) instead of objects; docs/answers.md has the format. On a three-repo sample, search answers are 40-60%
  smaller. The tool schemas and server instructions under profile `all` are 7,186 characters, down from 10,641:
  shorter descriptions and parameter lines, fewer advertised parameters (the rest are still accepted and in
  `kc_help <tool>`), and instructions that no longer repeat the descriptions. The instructions and the `kc_refs` and
  `kc_find` descriptions now send "what calls X" and "what breaks if X changes" to `kc_impact` (or `kc_trace`, depth
  2) with a `file:line` selector. The CLI's output and `--json` are unchanged.
- Answers whole enough to stop on. `find` on an exact name adds `declared`: one line per repo saying it has every
  declaration of the name there, with a C# `partial` type's parts (file:line each), a type's constructors (a record's
  primary one too), a method's overloads, and how many references in code name it (the other mentions are uses, for
  `refs`). `refs` groups references by caller (code before tests), marks rows in test files `test`, tallies callers
  and sites in `callers`, numbers the overloads the references resolve to (`overloads`, with signatures and the sites
  reaching each) and says which one each row calls (`overload`, in place of its `targets`). For an interface or base
  class, `refs` adds `hierarchy` (implementers or subclasses, those reaching it through another type, derived
  interfaces, DI registrations of it and of them) and `context` adds `conformers_indirect` and `di_implementers`; both
  say each empty list as "none found" with what was searched (`none_found`). A `context` on a partial type lists its
  `parts` instead of calling them other definitions, and lists constructors first among members.
- HTTP routes across repos find more of their callers. A controller with no `[Route]` of its own takes the nearest
  one up its base classes, in any file of the repo, with `[controller]` its own name, as ASP.NET does; `[Area]`,
  `[ActionName]`, `[NonAction]`, `[RoutePrefix]` and a method's lone `[Route]` (every verb) are read. Conventional
  routes with a literal template (`MapControllerRoute`, `MapDefaultControllerRoute`, `MapRoute`, `MapHttpRoute`) now
  place the actions they serve instead of making answers incomplete. Requests are read from `fetch`, axios (and
  instances of it), jQuery's `$.ajax`, `$.get` and `$.getJSON`, superagent, and wrappers named for HTTP or API
  clients, with URLs built from a base and a path (`getUrl('Items')`, `new URL(path, base)`, C#
  `new Uri(base, path)`) and RestSharp's `RestRequest`. A call to a generated API client
  (`getItemsApi(api).getItems()`, `new OrdersClient(base).list()`, `this.ordersApi.getOrder(id)`) links to the
  action of that name, under the controller named like the client, assuming `operation_name`. Each extractor's
  version is bumped, so C# and TypeScript files are read again on the next query.
- An HTTP fact with no links says what is left to check (`note`): for an action, the repos making requests that
  were searched and how, the generated clients seen, the requests whose URL is known only at run time (with where),
  and the requests nearest by path (another prefix); for a request, the repos serving routes and the nearest actions.
  `routes` lists, per unlinked request, the nearest actions only.
- `knowcode fetch-deps --into DIR` clones the git repos a repo depends on (manifest dependencies written as git
  URLs, Terraform git module sources, GitLab CI includes of other projects, and repos whose checkout was deleted),
  shallow, one directory each, so cross-repo answers can reach them. `--dry-run` prints the plan. DIR must be outside
  every repo; only https and ssh remotes are cloned; a repo whose AI privacy file blocks it whole is skipped unless
  `--include-blocked`. `--add-root` adds DIR as a workspace root. CLI only: the MCP server never clones.
- `knowcode history deepen` fetches more history into a shallow clone (`--by N`, 1000 by default, or `--since DATE`)
  and reports how deep it is before and after, and whether `hotspots`, `coupled`, `ownership` and `reviewers` now
  have all the history they read.
- Each index is rebuilt once on the first query after updating (index schema 20): constructs KnowCode doesn't read
  are now kept in a table of their own. Answers are unchanged.
- Smaller MCP answers by default. `kc_refs` lists 20 references (was 25), `kc_trace` 25 nodes (was 40), `kc_impact`
  30 dependents (was 60), `kc_flow` 40 steps (was 60), `kc_unused` 25 findings (was 50). The `kc_context` budget is
  1200 tokens (was 2000), capped at 6000 characters, and the `kc_overview` map is 400 tokens (was 800). Every cut says
  what it left out, and the rest comes through `kc_more` or the parameter it names. MCP rows are leaner. A
  reference's `from` is the enclosing symbol's qualified name, with no column. What all rows of a reference list
  share (`targets`, `from`, `name`) is said once beside the list. Test files drop their ranking `signals`. Tree use
  sites in the symbol's own file are said by line. The CLI's output and `--json` are unchanged.
  `scripts/mcp_tokens.py` measures answer sizes, with `--count-tokens` and `--compare`.
- Smaller MCP tool schemas: each tool's schema lists only its common parameters, with short lines. `repo` and
  `max_chars`, which the server instructions state once, and the rarer parameters (`as_of`, `since`, `order`,
  `format`, `kinds`, `evidence`, `edges`, `cite`, …) are still accepted and documented in `kc_help <tool>`. Profile
  `all` costs about 4.5k tokens per turn for a client that loads every schema (was about 8.6k), and the four
  always-loaded tools about 0.8k (was about 1.8k); see eval/schema-cost.md.
- The MCP server's instructions, the descriptions of `kc_find`, `kc_locate`, `kc_refs`, `kc_context`, `kc_trace`,
  `kc_impact` and `kc_search`, and the plugin's skill descriptions now lead with the questions they answer ("who calls
  or uses", "where is", "what breaks if"). None is longer than before.
- The clean-machine acceptance report adds `knowcode-use.json`: KnowCode calls per case and session, by tool.
- Cited source: every row that gives a location (`trace` nodes, `impact` rows, `path` and `flow` steps, `refs` items,
  a `context`'s callers and callees) carries `cite`, the source lines it rests on, read from the working tree when
  the answer is built: an edge's site, else the line naming a declaration. By default (`cite: brief`) the first 12
  rows quote one line each, up to 120 characters, and the rest give where it is, with a `capped` entry saying so.
  `--cite lines` (MCP `cite: "lines"`) quotes every row, a declaration from its first line through its name (at most
  3), each line up to 160 characters; `cut` says what a cap left out. `--cite none` leaves them out. Text trees print
  the lines under each row. A `context`'s callers quote their line in `cite` instead of `code`, which remains under
  `cite: none`.
- Cites are checked at query time: each cited file is compared with the index the answer read, and `check` says
  `verified`, or names the drift: `changed` (the line still names what it cites), `moved` (quoted where it is now,
  with `was`), `gone`, or `unchecked`. Every cite is checked, past the brief quote cap too. Any drift makes the answer
  `stale`, naming the cites. `--cite check` (MCP `cite: "check"`) gives the check without the lines. A `context`'s own
  symbol carries a check-only `cite`.
- Walks that finish: every place a walk stops short gives the exact call going on from it. Each `frontier` stop of
  `trace` and `impact` has `next` (`kc_impact {"depth":3,"repo":"web","symbol":"OrderService.place"}`), and the
  `incomplete` hint ends with the first one. A `flow` step at the depth limit that leads on has `stop` `{reason,
  beyond, next}`, and the flow is incomplete (`depth`); a flow cut by its step or repo limit, and a path search cut by
  its node or repo budget, are `capped` with the call raising the limits; a path search finding nothing within its
  hops has `next`, the call looking further.
- `trace` and `impact` now walk the same symbol graph as `path` and `flow`, through the same gate. One visible change:
  a hop across a link matched by name, from code found `exact`, is now `unique_name` in `trace` and `impact` too, as
  `path` and `flow` always labelled it. That covers an SQL object matched by its name (evidence `sql_name`) and one of
  several HTTP handlers or SQL schemas preferred by a heuristic (`dominant`, evidence `name_dominant`). The evidence is
  as before; the hop now counts in `impact`'s `low_confidence`. Under the default evidence policy it is still followed;
  under `confirmed` it is still left out.

- When a name has definitions in code and in tests (a test helper named like the function it tests), the code's
  answers first, whatever the kinds: `context`, `trace`, `impact` and the other symbol lookups used to rank a test
  file's function above a source file's constant. The test's definition is listed among the alternatives.
- Razor files (`.razor`, `.cshtml`) aren't read. A C# `refs` or `impact` answer from a repo holding any is now
  incomplete with reason `unsupported`, naming one and saying their markup's uses aren't listed, with a `kc_search`
  hint, instead of looking complete.
- The pre-registered public agent study measured this release's candidate (commit 00aae5b): it missed 2 of its 4
  criteria (re-check rate, tokens) and met 2 (answer F1 and recall, unprompted use). See
  [eval/public-v1/results.md](eval/public-v1/results.md).
- `search` and `grep-ast` (`kc_search`, `kc_grep_ast`) take a `paths` glob written after a repo's name
  (`web/**`, `web/src/**`): it searches that repo only, relative to it. Such a glob used to match nothing, and the
  answer said "no line matches" in every repo. An absolute path searches the repo it is in. A `lang` may be an
  extension (`ts`, `cs`, `py`). When the filter matches no file, the note says `paths matched 0 files (…)` (or
  `lang matched 0 files`), naming the repos, instead of a no-match note for lines that were never read.
- `at` (`kc_symbol_at`) finds the file's repo across the workspace: a path after the repo's name (`web/src/a.ts`),
  one relative to a workspace root, a repo-relative path held by exactly one repo, an absolute path inside a repo, and
  an absolute path outside any repo holding a repo's name (a copy of the tree, a stack frame from another machine).
  It used to refuse any relative path in a workspace ("pass --repo") and any absolute path outside a repo ("not
  inside a git repo"). A path in several repos names them.
- `index` brings derived data earlier queries computed (areas, the file ranking) up to the new index. An MCP call on
  a current index no longer waits for that work: it was scheduled on the session's first call, which then could run
  out of its budget and say "indexing is queued or running" right after `index`. The note is now only for repos
  that changed since their index, and only where it bears on the answer (the repo asked about, one never indexed, or
  one the answer's items come from). It names the repo, not its local path.
- C#: an extension method is found by the static class declaring it (`RegistrationExtensions.RegisterDecorator`) as
  well as by the type it extends: `find`, `context`, `trace`, `impact` and the other lookups, and `refs`, which
  answered nothing for it. A constructor initializer on the line after the colon (`… int x) :` then `base(x)`) is
  placed at `base`/`this`, not a line early. C# files are reparsed once.
- `search` hits carry what an agent opened the file for after one matched line. The first code hit inside a method
  or function names its parameters (`in EventSink.Emit(eventName, payload)`), once per member on a
  page. Without `context`, a hit in a statement that spans lines gets those lines, at most two each side: the call a
  `.fetch(…)` line continues, the `.then(…)` continuing an `ordersApi(client)` line, the `case` label a line
  runs under. A line that stands alone gets nothing. On the 46 `kc_search` calls of a public A/B study whose answers
  were re-checked, answers grow 6.2% (104,001 to 110,450 characters): 33 of 560 rows gain statement lines, 204 gain
  parameter names.
- `find` quotes the line naming each declaration of the name asked for in `cite`, read from the file now and checked
  against the index (`check: verified`), past doc comments and attributes: `[HttpGet("{id}")]` is not the line quoted
  for an action. One line per item, the first 12 such items.

## 0.1.1 (2026-10-20)

- A workspace-wide answer no longer fails because one repo can't be read (a checkout deleted while the answer was
  worked out, a failing `git` call): it answers with the other repos and names the one left out as `missing_repo` or
  `failed`.

- Updating refreshes the Claude Code plugin: after `self-update`, and when `serve` starts with an older bundle,
  KnowCode rewrites the bundle and runs Claude Code's plugin update. If that can't finish, it prints the
  `knowcode setup claude` command that does. `setup claude` also moves an installed plugin to the current version.
- The Claude Code hook script exits without starting `knowcode` for Bash commands that can't be code searches.

- `status` and `kc_status` always show the AI privacy policy they apply, indexed or not: the file's state, the file
  name looked for, the global level, `block_at` and how many paths are blocked. The CLI text gives it one line.
- `uninstall` no longer asks Claude Code to keep the plugin's data directory, where KnowCode keeps nothing. Claude
  Code's orphaned cache copy of the plugin is left to Claude Code, for sessions still running.
- Tests: the shared `commit` test helper reports git's output when a fixture commit fails.

- Homebrew is back: `brew install bubstackdev/tap/knowcode`. For a Homebrew installation `self-update` refuses with
  "Homebrew manages this install; run `brew upgrade bubstackdev/tap/knowcode`", and the update notice and
  `self-update --check` name that command. After `brew upgrade`, the next `serve` start refreshes the Claude Code
  plugin.

### Release

- Releases publish a Homebrew formula to `bubstackdev/homebrew-tap` again. The publish job commits as "KnowCode
  release bot" with a noreply address, has read-only `GITHUB_TOKEN` permissions, and is the only job given the
  `HOMEBREW_TAP_TOKEN` secret, which reaches only its push step, through `env:`. On a tag push, CI first checks that
  the tap is public and that the token can push to it; pull requests and pushes to `main` need no secret.
- Release binaries no longer contain build-machine paths. The C code in tree-sitter and its grammars now gets the same
  path remapping as Rust code, and each release build fails if a binary contains the builder's home or checkout path.

### Release testing

- Clean-machine acceptance is one script in the repo, `scripts/acceptance/clean-machine.sh`, for macOS and Linux. It
  never starts an interactive shell, covers the installer adding `~/.local/bin` to PATH, compares client settings
  entry by entry from a snapshot taken right before uninstall, records two client sessions per case from the
  client's transcripts (recorded, not gated), and checks that a new shell starts cleanly after the removal steps.

## 0.1.0 (2026-10-19)

The first public release, for macOS on Apple silicon and Linux on x86_64 and arm64.

### Answers

- One answer contract for every command and MCP tool: `complete`, `incomplete` with closed-set reasons (`capped`,
  `budget`, `deadline`, `stale`, `failed`, `missing_repo`, `unsupported`, `policy`, `withheld`), `sampled`,
  `provenance` for every repo consulted, and cursors that continue a cut list exactly once (`knowcode more`,
  `kc_more`).
- Every edge carries a confidence (`exact`, `unique_name`, `ambiguous`, `external_likely`, `unresolved`) and evidence:
  how the target was found and what that did not check. An evidence policy (`--evidence`) chooses which edges a walk
  follows.

### Questions it answers

- Definitions by name (`find`), files for a concept (`locate`), symbols at a line (`at`), ranked text and regex search
  (`search`), and structural search with ast-grep patterns (`grep-ast`).
- One bundle per symbol (`context`), every reference (`refs`), call trees (`trace`), paths between symbols (`path`),
  and end-to-end flows from an entry point (`flow`), across repos where HTTP routes, the message bus, SQL,
  dependency injection, Terraform modules or Ansible roles link them.
- Before a change: `impact` by symbol, file or diff, with dependents by proximity and co-changed files; `tests` to
  run; `coupled` files; `reviewers`; dead-code candidates (`unused`) and rename previews (`rename`) for C# and
  TypeScript.
- History: `hotspots` with code-health biomarkers, `ownership`, and functional `areas`.
- How a repo is deployed, from its GitLab CI pipeline through shared templates to Ansible, Terraform, cloud CLIs,
  containers and releases (`context <repo name>`), with every include accounted for and every list continuing by
  cursor.
- Constructs an extractor recognises but doesn't read (DI assembly scans, run-time SQL table names, publishes of
  unknown message types, Jinja-named includes, Terraform `moved` blocks, unread CI include and Angular lazy-route
  forms) make the answers drawing on them incomplete (`unsupported`), naming each.
- Orientation: `overview`, `deps`, `routes`, `tables`, `events`, `docs`, `status`.
- Notes anchored to symbols, kept in the user data directory, with export and import.

### Languages

- C#, TypeScript, TSX and JavaScript, Swift, Python, Rust, ColdFusion, SQL, Terraform, Ansible and GitLab CI, with
  manifests from SwiftPM, XcodeGen, Cargo, MSBuild and npm. Angular, ASP.NET, Entity Framework, MassTransit,
  NServiceBus and common DI containers are modelled. Precise references are read from a SCIP index when one is
  present. See [languages](docs/languages.md) for what is and isn't covered.
- A code base's own conventions (message-type attributes, consumer bases and publish helpers, gateway calls and
  request contracts, DI scope values, CI and Ansible names) are configured under `[conventions.*]` in `config.toml`,
  so its messages and requests link as the libraries' do.

### Running it

- `knowcode setup claude`, `codex` or `cursor` connects a client; the Claude Code plugin adds a Grep/Glob/Bash hook and
  four workflow skills. `knowcode uninstall` removes what setup recorded, checks that the client no longer lists
  it, and ends with the exact steps to remove a shell-installer installation.
- In Claude Code, `kc_context`, `kc_refs`, `kc_impact` and `kc_locate` load up front (`anthropic/alwaysLoad`, about
  1k tokens); the other tools stay behind tool search.
- The first query indexes automatically. MCP queries wait about two seconds and otherwise answer from the last
  snapshot, marked stale, while a low-priority background worker finishes.
- Indexes live in the user cache directory, bounded by `cache.max_size` (2 GiB by default), with deleted checkouts
  pruned automatically. Nothing is written into indexed repos.
- One `config.toml` for settings, `knowcode config` to edit it, and workspace roots for multi-repo answers.
- Repos can block content from AI tools with a privacy file (`.knowcode-privacy.yml`, or the name and keys
  `[privacy]` configures); blocked content never reaches an answer or the cache.
- No telemetry. Indexing and queries make no network calls. A daily, notify-only update check can be turned off.
- Shell installer built with cargo-dist; `knowcode self-update` for shell installs. Homebrew follows in 0.1.1.
