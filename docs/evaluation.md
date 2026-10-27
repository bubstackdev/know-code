# Evaluation

How KnowCode is checked: the gates every change passes, where ground truth comes from, how results are reported,
and the evaluations published so far. The documentation makes no comparative or performance claim beyond what
those evaluations show.

KnowCode is developed and evaluated against public repositories.

The tools live in [`scripts/eval/`](../scripts/eval), behind one entry point, `scripts/eval/run.py <command>`
(Python 3.12, standard library only). Data they produce (query sets beyond the public example, snapshots, results,
labels) is kept outside the repository, under `$KNOWCODE_EVAL_DIR`.

## Gates per change

CI ([`.github/workflows/ci.yml`](../.github/workflows/ci.yml)) runs these on every pull request. Run them yourself
when it can't:

```sh
cargo test --workspace --locked -- invariant      # the invariant suite, reported on its own
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
python3 -m unittest discover -s scripts/eval -p 'test_*.py'
python3 -m unittest discover -s scripts/eval/neutral -p 'test_*.py'
python3 scripts/check-doc-paths.py                # docs name only crate and script files that exist
python3 scripts/check-docs.py target/debug/knowcode   # docs match the binary's commands, flags, keys and tools
scripts/check-size.sh                             # the dist binary against size-budget.txt
```

CI also replays the recorded KnowCode calls on five small public repositories against the latest release and the
change, with no model calls, and fails on a regression beyond the replay gate's tolerances; the comparison is in the
job summary ([eval/replay-gate/README.md](../eval/replay-gate/README.md)). The full replay, all ten repositories, runs
before a release.

Then **diff the answers**: snapshot the query set with the base build and with the change, each on its own fresh
cache, and explain every difference in the commit or merge message. A difference nobody can explain is a regression
until shown otherwise.

```sh
scripts/eval/run.py snapshot --queries eval/queries.toml --bin $BASE_BIN --cache /tmp/kc-base --index --out /tmp/snap-base
scripts/eval/run.py snapshot --queries eval/queries.toml --bin target/release/knowcode --cache /tmp/kc-new --index --out /tmp/snap-new
scripts/eval/run.py diff /tmp/snap-base /tmp/snap-new      # 0 same, 1 any answer differs, 2 refused
```

[`eval/queries.toml`](../eval/queries.toml) is the public query set, over repos `scripts/corpus.py fetch` clones.
Each query runs with `--json --no-sync` and an empty config directory of its own. Answers are normalised before
they are stored (build info, freshness, timings and row ids dropped; paths under the root made relative). Every list
keeps its order, so a changed ranking is a difference; only commands whose lists carry no ranking are sorted, named
in `UNORDERED` in `scripts/eval/snapshot.py`. A snapshot in which any query failed is invalid, and `diff` refuses to
compare it.

Answers must not depend on the order files were stored in. With `KNOWCODE_INSERT_REVERSED=1`, indexing stores files
in reverse path order; a snapshot taken that way must equal one taken normally.

### The invariant suite

Tests that check a property over a class of inputs are named `invariant_…`, so `cargo test -- invariant` runs them
all. They cover, among others:

| Invariant | Where |
| --- | --- |
| Index order and parser thread count never change any command's answer | `crates/knowcode/tests/invariants.rs` |
| Every answer of every command says whether it is complete, with closed-set reasons and its repo in `provenance` | `crates/knowcode/tests/invariants.rs` |
| Every list a limit cuts makes the answer incomplete; finishing an envelope is idempotent | `crates/knowcode/src/contract.rs` |
| Every input an answer depends on (ignore files, config, tsconfig, `.csproj`, `package.json`, history settings, `.mailmap`) changes it when edited, to what a cold index answers | `crates/knowcode/tests/invariants.rs` |
| Formatting never changes references, in every extracted language | `crates/kc-index/tests/occurrences.rs` |
| An added edge never removes a dependent from a trace or an impact | `crates/knowcode/src/completeness.rs` |
| A rename preview never edits an occurrence resolved to another symbol | `crates/knowcode/src/refactor.rs` |
| Names never resolve across language families; without a project file a C# binding is never exact | `crates/kc-index/tests/scope.rs` |
| A query reads one generation while another process reindexes | `crates/kc-index/tests/snapshot.rs` |
| A child process past its deadline is killed and the cut recorded | `crates/kc-store/src/limits.rs` |

Every edge of confidence `exact`, `unique_name` or `ambiguous` carries evidence, checked by a debug assertion at each
place a confidence is decided (`kc_store::answer::debug_check`).

## Ground truth

A feature is measured against truth that doesn't come from its own implementation. In order of preference:

1. **Git history.** Replay past commits (`run.py replay`): run KnowCode at a commit's parent and ask a question the
   commit then answers. Given one changed file, does `impact --file` reach the others? Does co-change pair them?
   Given the commit's non-test files, are the test files it touched selected? Given the commit subject, are the
   changed files in `locate`'s top k? Do the files `hotspots` ranks worst get fix commits in the next 90 days
   (`run.py defects`)? Each parent is a scratch clone with its own cache, so no answer can read the change back.
   History is a noisy oracle; replay recall is a lower bound, best read as a comparison between builds.
2. **Hand labels.** Where history records nothing (is this route link right? is this DI binding the one that
   runs?), label a sample from the source, before and without looking at KnowCode's answer, and score it with
   `run.py labels` (see [`eval/labels-example.toml`](../eval/labels-example.toml)).

Samples are drawn at random with a recorded seed.

## Reporting

Every claim of accuracy carries counts and sample sizes, not only rates; 95% confidence intervals (Wilson for
proportions, a bootstrap over repos for replay means); every false positive and false negative in the sample with its
cause; and what the validation doesn't cover. Comparisons between builds replay the same commits and report paired
differences. Cost is reported as work done (instructions retired, rows read, files parsed, peak memory); wall-clock
time only from a quiet machine, with the machine and load stated.

After a feature merges, a separate agent that didn't build it re-runs the gates, re-derives a sample of the claims
from source, and checks the reported numbers against the stored results.

## Neutral comparison protocol

Comparing KnowCode with another tool needs a protocol neither side can tilt. The code is in
[`scripts/eval/neutral/`](../scripts/eval/neutral):

1. Questions are pre-registered from sources neither tool influenced (merged changes, ticket titles, structural
   subjects drawn per stratum with a recorded seed), and nobody edits them after seeing an answer.
2. Labels come from source only, under [`LABELING.md`](../scripts/eval/neutral/LABELING.md), made before and without
   either tool's output. A seeded 20% is labelled twice, and the agreement is reported.
3. Commits and index states are pinned, so no tool can read the answer back.
4. Each tool has its own expert operator and the same budget per question.
5. Answers are normalised and graded blind.
6. Each question runs at least three times per tool.
7. "This tool can't answer this" is counted apart from wrong answers, per stratum.

## Published evaluations

**Release gates**, with no model calls, compare each release candidate with the previous release:

- [Release gate, 0.5.1](../eval/release-gate/0.5.1.md): the same checks as 0.5.0's, against 0.5.0, with whether
  each of 0.5.0's misses is now met, and the update across the repository's new name.
- [Release gate, 0.5.0](../eval/release-gate/0.5.0.md): tool cost per turn in tokens, the replay of recorded calls,
  the history questions, the workspace gate, clean-machine acceptance and the update from 0.4.0.
- [Release gate, 0.4.0](../eval/release-gate/0.4.0.md): tool cost per turn in tokens, the replay of recorded calls,
  the workspace gate, clean-machine acceptance and the update from 0.3.0.
- [Replay gate](../eval/replay-gate/README.md): every KnowCode call recorded in the public studies, replayed against
  a build and scored against the studies' labels.
- [History gate](../eval/history-v1/README.md): history questions on full clones, labelled from git alone, with
  KnowCode scored against the plain-git approaches.
- [Identity gate](../eval/identity-v1/README.md): references from pinned public repositories labelled by each
  language's toolchain (the Swift index store, Roslyn, the TypeScript compiler), with KnowCode's reference links scored
  against them: `exact` precision, recall, and links to the workspace where the toolchain resolves outside.
- [Contract gate](../eval/contract/README.md): each op's capabilities entry checked against what the server accepts
  and answers, restricted mode against its scope, files and network, and refusal codes across the replay.
- [Schema cost](../eval/schema-cost.md): what the advertised tools and instructions cost a session per turn.

**Agent studies** compare agents with and without KnowCode on pinned public repositories, pre-registered, with the
misses stated first. The plain arm gets grep, find and read; the KnowCode arm gets the same plus its MCP tools.
Questions, labels, run order and configuration are frozen with content hashes before either arm runs.

- [public-v1](../eval/public-v1/results.md) (0.2.0): Jellyfin server and web client, Bitwarden server and clients,
  Autofac with its DI integration. The [method](../eval/public-v1/method.md), the
  [protocol](../eval/public-v1/protocol.md), the [sampling plan](../eval/public-v1/sampling.md), the
  [manifest](../eval/public-v1/manifest.json) and the [subject scope](../eval/public-v1/scope.json).
- [public-v2](../eval/public-v2/results.md) (0.3.0 against 0.2.0): 40 questions over ten public repositories. The
  [protocol](../eval/public-v2/protocol.md) and [sampling plan](../eval/public-v2/sampling.md).
- public-v3 is held out for the next study.

The preparation scripts (`scripts/prepare-public-eval.py`, `scripts/census-public-eval.py`,
`scripts/scope-public-eval.py`, `scripts/sample-public-eval.py`, `scripts/plan-public-eval.py`,
`scripts/prepare-public-eval-session.py`, `scripts/prepare-public-eval-index.py`) and the runner in
`scripts/eval/session_runner.py` are described step by step in the method.

## First-run acceptance

[first-run-acceptance.md](first-run-acceptance.md) checks installation, `knowcode setup claude` and the first
correct answer on pinned public repositories. These are acceptance cases, not an evaluation.

## Other tools

- `run.py mine`, `replay` and `report`: the history replay above. Scorers are defined in
  [`eval/scorers.toml`](../eval/scorers.toml).
- `scripts/search_eval.py` checks `search` against `git grep` on [`eval/search.toml`](../eval/search.toml).
- `scripts/measure.py`, `scripts/search_bench.py` and `scripts/mcp_memory.py` measure footprint, search latency and
  server memory.
- [`eval/questions.toml`](../eval/questions.toml) and [`eval/references.md`](../eval/references.md) hold an early
  agent question set with hand-checked references, kept for the record.
