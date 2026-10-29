# find-dev-v1: a development set for `find`'s precision

compare-v1's first published step found KnowCode's `find` at precision 0.650, against Serena at 0.820 and
codebase-memory-mcp at 0.963. `find` lists rows that match the query by token, by prefix or ignoring case next to
the exact declaration. This set is where a change to that is measured and chosen. compare-v1's frozen find questions
are never used to tune it: this set's repositories are none of compare-v1's, identity-v1's or identity-v2's, so
compare-v1 stays a held-out check.

This README, the repository list and the draw were committed before KnowCode answered any of the set's questions.

## Repositories

Two public repositories per language, each pinned at the commit below:

| Name | Language | Repository | Commit |
| --- | --- | --- | --- |
| mediatr | csharp | jbogard/MediatR | `62010baf12851770553f275ce4047e93f4471e79` |
| fluentvalidation | csharp | FluentValidation/FluentValidation | `f95dbd5c8166a10e38f7a8bca4d752abd641a453` |
| jotai | typescript | pmndrs/jotai | `e5971ba44866d070564a0a30187f6976fee0e350` |
| class-validator | typescript | typestack/class-validator | `2e1a5c27dbd65b80e27fe96b49bd6e6641fa3603` |
| pflag | go | spf13/pflag | `f8a990678a6d44385313e9b19e40737f889f9b14` |
| urfave-cli | go | urfave/cli | `293b0229b99c460fa66283418569773681df010d` |
| commons-cli | jvm | apache/commons-cli | `7dfda5bb37158b99d543f03ac8f56c68add46094` |
| commons-text | jvm | apache/commons-text | `aeab8401bbdb1a98d458c10f4ef6d77a86e5b4ba` |

Swift isn't in the set: its labeller builds each repository whole, which is slow enough to defer. A repository whose
toolchain build fails is replaced by the next candidate in the same language, and the replacement is recorded here
before any answer is read. Replaced so far: sindresorhus/ky and pmndrs/zustand, the first TypeScript candidates,
which keep no lockfile, so their dependencies can't be restored as the labeller restores them (from the lockfile).
Both were replaced before anything was labelled or answered.

## Labels

Each repository's declarations come from its own toolchain, through identity-v1's and identity-v2's labellers run in
their throwaway containers ([run.sh](../../scripts/eval/identity/run.sh), with this set's
[repos.txt](repos.txt)). These are Roslyn for C#, the TypeScript compiler, `go/packages` and `go/types` for Go, and
scip-java for Java. KnowCode labels nothing.

## The draw

From each repository's enumeration, the same way compare-v1 draws `find` (`scripts/eval/compare/questions.py`), but
with this set's seed (`find-dev-v1`) and a larger quota:

- the names of its declarations, grouped by how many declarations share the name (strata `1`, `2-4`, `5-20`; names
  with more than 20 are left out);
- per stratum, the first 8 names in SHA-256 order of `find-dev-v1:REPO:NAME`;
- the labels: every declaration with that name, as (file, line of its name, name).

Each drawn name is asked as it is (`exact`), and in up to three forms a caller who doesn't remember it exactly might
type. All of them have the same labels, the declarations of the name:

- `lower`: the name in lower case, when that differs;
- `prefix`: its first 60% of characters (at least 3), when shorter than the name;
- `typo`: the name with its second and third characters swapped, when it has at least 4 characters and they differ.

## What is measured

KnowCode answers each question with `find NAME --json -n 200` on its own index of the pinned tree. All four of
KnowCode's directories (HOME, config, data, cache) are the run's own. Each row's `matched` and name say whether it
is a case-sensitive exact match, an exact match ignoring case, a prefix match or a token match. Every option below
is then a rule over that one answer:

- **current**: the first 10 rows, as `kc_find` answers by default;
- **exact-only**: when any row's name equals the query exactly (with case), only those rows; otherwise as current;
- **similar-apart**: the same first page as exact-only, plus the other rows, up to 5, in a separate `similar`
  list;
- **cursor**: the same first page as exact-only, with the other rows behind a cursor (`more`).

Per option and question form:

- **precision**: right rows among the rows judged, on the first page, with compare-v1's judge
  (`scripts/eval/compare/score.py`, `judge_decls`);
- **recall**: labels named on the first page, and, for similar-apart, on the first page plus `similar`;
- **found**: questions where at least one label is on the first page (or in `similar`);
- the rows and bytes a client reads.

Every count is published with its totals. The precision on the `exact` form is the number compare-v1's find measures.
The other forms are the cost a rule may impose on a caller who misspells or half-remembers a name.
