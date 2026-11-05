# Many worktrees, one index

0.6 section 4. Worktrees of one repository share an immutable base index at a commit; each worktree indexes only
its own changes on top of it, and keeps every freshness guarantee a separate index has.

## What exists in 0.5

- One SQLite index per checkout (`<tree>/repos/<name>-<hash>.sqlite`, in the build's own part of the cache). References are stored unresolved and
  resolved at query time against the whole index, so an edit to one file never re-extracts another.
- A new checkout of an indexed repo is seeded with `VACUUM INTO` from the most recently indexed checkout of the same
  origin (`kc_index::seed`). The first sync then hashes every file and extracts only those whose content differs.
  The copy is a full copy: N worktrees take N times the disk. The seed is a live index, at whatever commit and with
  whatever uncommitted edits its checkout had.
- Derived data (file ranking, areas) is cached per generation and recomputed on demand. History tables (churn,
  ownership) are keyed by HEAD.

## Design

### The overlay is physical: a copy-on-write clone of the base

A worktree's index is a copy-on-write clone of its base file (`clonefile` on APFS, a reflink on Linux btrfs and
XFS; `std::fs::copy` does either). The clone shares every page of the base until the worktree's first sync writes
one. What the worktree changed, added or deleted is written into its own pages; the unchanged files' rows,
full-text entries and content postings stay physically the base's.

Queries read one SQLite file, as today. Nothing in `kc-store`, the walks or the ops changes. The considered
alternative, a logical overlay (the base attached read-only, the worktree's files in a second database, a deletion
mask, every query reading a union), would touch every query in `query.rs`, `trace.rs`, the FTS5 and trigram
searches and the derived caches; file ids would have to be shared across two databases; and an answer could differ
from a fresh index wherever one query forgot the mask. The gate asks for byte-for-byte equality with a fresh index,
which the clone gets by construction from the existing incremental sync.

### The base

- **Key:** the repository's identity (its normalized origin; without one, its git common directory, which every
  worktree of a checkout shares), the full commit, the build (schema, resolver and extractor versions, as the
  freshness snapshot records them) and the privacy policy's key.
- **Where:** `<tree>/bases/<name>-<identity hash>-<commit>.sqlite`, with a `.used` access record.
- **Made from** a checkout whose tree is clean at that commit (`git status --porcelain` empty, untracked files
  included, since they are indexed) right after a sync with no failed reads, by `VACUUM INTO` a temporary file
  renamed into place. `VACUUM INTO` writes a consistent copy while other processes use the source, and leaves no
  free pages or WAL behind.
- **Stripped of** what belongs to a checkout and not to a commit: `root`, `head.*`, `disc.*`, `last_sync`,
  `sync.failed`, the SDK and SCIP stamps.
- **Immutable:** never opened for writing after the rename.

### A new worktree

1. Its index doesn't exist. Under its writer lock, it picks the base of its identity, build and privacy key whose
   commit is nearest to its HEAD: the same commit, else the fewest files `git diff --name-only <base> HEAD` lists
   (the base's commit must be in the repository). Without a base, the 0.5 seed from a sibling is the fallback,
   then a cold index.
2. It clones the base to its index path (to a temporary name, then renamed).
3. It invalidates what the clone can't vouch for: every file's stat (mtime 0, size -1), the extractor version of the
   files whose facts depend on the checkout's location (manifests: path dependencies; Ansible: the root role is
   named after the checkout's directory). The next discovery is a full one.
4. Its first sync is the ordinary one. Every listed file is hashed; a file whose hash matches keeps the base's facts
   and gets the worktree's stat; one that differs is extracted; one not listed is removed. That is exactly the work
   an existing index does after a checkout switches branch, so it already holds the freshness guarantees.

Resetting the stats closes a gap the 0.5 seed has: a copied checkout whose files keep their mtimes (`cp -p`,
`rsync -t`) could keep the sibling's facts for a file with the same size and mtime and different content.

### Reading base and overlay as one graph

- **Symbol identity:** identities are made of module, scope and file (section 1), never of row ids, so a symbol
  read from a base page and one written by the worktree are named the same way a fresh index names them. Answers
  must not depend on row ids or insertion order; `KNOWCODE_INSERT_REVERSED` already tests that.
- **Cross-file edges:** references are resolved at query time, so an overlay change invalidates nothing stored
  across files. What is cached across files is keyed by the generation (`tree_gen`), which a sync moves whenever
  it reparses or removes a file.
- **Derived data:** the base keeps its derived rows (ranking, areas). They stay valid while the worktree's
  generation is the base's: a clean worktree at the base's commit. Any change moves the generation, and the next
  query that needs the ranking or areas computes them again for the whole worktree, as today. This is the main
  per-worktree cost left (about a quarter of a cold index's instructions on swift-argument-parser).
- **History** (churn, ownership, co-change) is keyed by HEAD and read from the shared git directory: kept while the
  worktree's HEAD is the base's, rebuilt otherwise.

### Rebasing and rebuilding

A worktree's index never depends on its base after the clone. When its HEAD moves, it syncs incrementally like
any index. A base is published again only when it pays: a clean checkout at a commit whose nearest base is more
than a threshold away (proposed: 2% of the files, at least 50). At most three bases per identity are kept; an older
one goes when a newer one is published and nothing is cloning it. A base whose build or privacy key no longer
matches is never used and goes at the next maintenance.

### Eviction and the cache budget

- Bases are entries of the LRU like indexes, touched when cloned. Pruning removes a base whose identity has no
  existing checkout; `purge` removes all; `forget` leaves bases alone.
- A worktree's index is removed with its worktree, by the existing prune of gone checkouts.
- The budget counts a clone's private bytes, not its length: on APFS, `getattrlist` with
  `ATTR_CMNEXT_PRIVATESIZE`; on Linux, the extents `FIEMAP` doesn't mark shared. Where neither works, the length,
  which over-counts and only evicts early.

### Concurrency

- Publishing takes a per-base lock without waiting (another process publishing the same base wins) and renames a
  complete file into place. Readers never see a partial base.
- Cloning needs no lock: a clone is a snapshot of the file at that instant, and an unlink during it fails the clone,
  which falls back to the sibling seed or a cold index.
- Many processes on one worktree: unchanged (the writer lock serializes syncs; readers hold WAL snapshots).
- N jobs that start at once on a commit with no base: the first clean checkout claims the base (a lock per repository
  and commit) and builds it; the others wait until it is published or given up, at most about a cold index's time
  (by the checkout's size, between a minute and 15 minutes), then clone it or index on their own. A dirty checkout
  never claims (it couldn't publish), so no job waits on one that won't publish.

### Privacy, per worktree

Each worktree applies its own privacy file, as each checkout does now. A worktree clones a base only when its
own policy's key equals the key the base was made under. An index made under a key holds nothing that key blocks
(it is purged whole when the key changes), and `VACUUM INTO` leaves no free pages or WAL frames with older content,
so the clone holds nothing the worktree's policy blocks. A repository blocked whole never has a base. A worktree
whose policy differs from every base indexes cold.

## Provenance

An export for durable records (`--provenance` on answers, and the `provenance` op): the full commit, the worktree
identity (root, git directory, identity hash), a digest of uncommitted content (the hashes of indexed files that
differ from the commit, and of untracked indexed files), the index generation, the base the index was cloned from,
the extractor versions, the privacy policy's key and source digest, and blake3 hashes of the canonical query and of
the result. Compact answers stay as they are.

## The gate (`scripts/worktree-gate.py`)

Public repositories at pinned commits, cloned into a scratch directory; worktrees made there.

- N worktrees at different commits and with uncommitted edits, indexed in one cache on shared bases, against each
  indexed alone in a cache of its own. A fixed query set per repository; answers compared byte for byte, after
  removing only what differs between two cold indexes of the same tree (listed in the report).
- Freshness in every worktree: a new untracked file, and a same-size edit with its mtime restored, with
  `KNOWCODE_VERIFY=1` and without.
- Instructions retired (indexing and first query) and disk (private bytes) against N separate indexes.

## Risks

- **Filesystems without reflinks** (ext4, tmpfs, overlayfs): the clone is a full copy. Indexing work is still only
  the worktree's changes; disk is not shared.
- **State outside per-file facts** carried from the base (meta keys, SDK and SCIP stamps, history tables, derived
  rows): each needs resetting or a key that the worktree's state invalidates. The gate is what finds a missed one.
- **Derived data is whole-graph:** any change recomputes the ranking and areas per worktree.
- **First sync reads every file** to hash it: linear in the repository's bytes, small next to extraction. Since 0.7
  the clone keeps the checkout's stat for every file git says is clean and unchanged since the base's commit, so the
  first sync reads only the others.
- **Page churn:** the first sync rewrites every `files` row (new stats), so those pages become private; a large
  change rewrites more than its files' share of the index.
