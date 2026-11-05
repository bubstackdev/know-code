# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

KnowCode is a local code graph for coding agents: one Rust binary (`knowcode`), no services, that indexes the git
repos in a workspace into per-checkout SQLite indexes and answers graph questions over MCP (`knowcode serve`) and the
CLI. User-facing docs are in `README.md` and `docs/`; `docs/architecture.md` is the authoritative design reference.

## Commands

Toolchain is pinned in `rust-toolchain.toml` (1.98.0, with clippy and rustfmt). Always pass `--locked`.

```sh
cargo build --locked                                  # target/debug/knowcode
cargo test --workspace --locked                       # everything
cargo test --workspace --locked -- invariant          # the invariant suite (tests named invariant_…)
cargo test -p kc-index --test freshness               # one integration test file
cargo test -p knowcode walk_answers_match             # one test by name filter
cargo clippy --workspace --all-targets --locked -- -D warnings
```

Merge gates (from `docs/evaluation.md` "Gates per change"; CI in `.github/workflows/ci.yml` runs them):

```sh
python3 -m unittest discover -s scripts/eval -p 'test_*.py'          # eval harness tests
python3 -m unittest discover -s scripts/eval/neutral -p 'test_*.py'
python3 -m unittest discover -s scripts/acceptance -p 'test_*.py'
python3 scripts/check-doc-paths.py                    # docs name only crate/script files that exist
python3 scripts/check-docs.py target/debug/knowcode   # docs match the binary's commands, flags, config keys, MCP tools
python3 scripts/check-first-run.py target/debug/knowcode
python3 scripts/check-workspace-first-run.py target/debug/knowcode
python3 scripts/check-updates.py target/debug/knowcode
scripts/check-size.sh                                 # dist-profile binary vs size-budget.txt (slow: fat LTO)
```

On pull requests CI also runs the release replay gate's fast subset: recorded KnowCode calls on five public repos,
replayed against the latest release and this build with no model calls (`scripts/eval/replay_gate.py`; see
`eval/replay-gate/README.md`).

The Claude Code plugin's source is `integrations/claude-code` (bundled into the binary by `knowcode setup claude`):
`claude plugin validate integrations/claude-code --strict` and `integrations/claude-code/test-hook.sh REPO SYMBOL`.

Cargo runs every test binary through `scripts/test-runner` (`.cargo/config.toml`): each gets a throwaway HOME with a
git identity, no inherited `KNOWCODE_*_DIR` and the update check off, so tests never touch your config or index cache
(`crates/kc-index/tests/isolation.rs` checks it). The Python checks run git with your real HOME and need a global git
identity (`git config --global user.name/user.email`). Set `KNOWCODE_UPDATE_CHECK=0` when running the binary in scripts.

Answer diffing (required for changes that can affect answers): snapshot `eval/queries.toml` with the base build and
the change, each on a fresh cache, then `scripts/eval/run.py diff`; explain every difference in the commit/PR
message. Exact commands are in `docs/evaluation.md`. `KNOWCODE_INSERT_REVERSED=1` checks answers don't depend on file
storage order.

## Architecture

Four crates in `crates/`, layered bottom-up:

- **`kc-extract`**: per-file, context-free extraction (tree-sitter per language, SQL scanner, Ansible/GitLab CI YAML,
  manifests). Changing one file never forces another to be re-extracted. Each extractor has a version; bump it when
  extraction output changes so that language's files are re-extracted on the next query.
- **`kc-store`**: SQLite schema, FTS5/trigram search, query-time name resolution and graph queries (`query.rs`,
  `trace.rs`), the answer contract types (`answer.rs`), and the per-query context/limits (`limits.rs`,
  `kc_store::limits::Operation`: cancellation, indexing wait, and side-collected `incomplete` reasons).
- **`kc-index`**: discovery, ignore rules, freshness/incremental sync, the content index, git history, cross-repo
  linking (HTTP, bus, SQL, Terraform, Ansible, CI), the workspace summary, config, privacy file, notes, the writer
  lock and the cache lifecycle.
- **`knowcode`**: the binary. `main.rs` (CLI), `ops.rs` (the operation behind each command; very large), `mcp.rs`
  (MCP server; one `kc` tool with an `op` field by default, separate `kc_*` tools under `KNOWCODE_TOOLS=all`),
  `help.rs` (help topics), `contract.rs`/`render.rs`/`compact.rs` (answer shaping), setup/uninstall/update.

Key cross-cutting ideas that span files:

- **References are stored unresolved** and resolved at query time against the whole index (and dependency repos),
  which is why extraction can stay per-file.
- **Freshness**: every query syncs the repos it reads first (discovery → mtime/size check → hash → re-extract →
  one transaction moving the generation). Queries read one committed snapshot per repo. Writers take a per-repo lock
  file; readers never do.
- **The answer contract**: every answer carries `complete`, `incomplete` (reasons `capped`, `budget`, `deadline`,
  `stale`, `failed`, `missing_repo`, `unsupported`, `policy`, `withheld`, `depth`), per-edge `confidence` and
  `evidence`, `provenance`, and a `cursor` when cut. New code paths must report what they miss rather than return a
  confidently complete partial answer. Spec: `docs/answers.md`.
- **Walks** (`trace`, `impact`, `path`, `flow`) decide edges in one place (`knowcode/src/graph.rs`) and read every
  edge through one symbol graph (`walk.rs`; tree edges in `tree.rs`). Their output is pinned byte-for-byte by
  `crates/knowcode/tests/snapshots/walks.txt`; after a deliberate change regenerate with `KNOWCODE_TEST_BLESS=1` and
  review the diff.
- **Across repos**: the workspace summary (per-repo manifests, routes, messages, tables, Bloom filters of names)
  decides which repos a workspace-wide query opens. Checkouts of one origin count as one repo.

## Rules specific to this repo

- **Nothing is written into indexed repositories.** Indexes live in the user cache dir, notes in the user data dir.
- **Privacy file**: content a repo's privacy file blocks is never indexed, shown or searched; answers that may lack it
  say `withheld`. Don't add paths that bypass this.
- **No network** during indexing or queries; the daily update check is the only automatic network call.
- **Binary size budget**: `size-budget.txt` holds the dist binary size. Raise it only in the commit that grows the
  binary, appending a comment line explaining why; lower it when `check-size.sh` says it shrank.
- **Docs are checked against the binary**: adding or renaming a CLI command, flag, config key or MCP tool requires
  updating `docs/usage.md`/`docs/configuration.md` and the `kc_help` topic in `crates/knowcode/src/help.rs` (or
  `check-docs.py` fails). Docs may only name files that exist.
- Add user-visible changes to the "Unreleased" section of `CHANGELOG.md`.
- Use only public repositories in examples, tests and fixtures, and made-up names for anything invented. Claims
  about accuracy or cost need a measurement on public repos, published with its counts.
- Comments and docs are written as plain, precise prose explaining *why*; match the surrounding density.
