# Task answers against git

Tasks taken from the fix commits of pinned public repositories, each labelled with the files and tests its commit
changed. Neither KnowCode nor a model labels anything. The set is 0.6's gate item "Task answers"
([plan](../../docs/plans/0.6.md#gate)): recall of changed files and tests within an answer's budget, reported
against KnowCode's own `locate` alone; the task answer must be higher at equal or smaller answer bytes.

## Tasks

[task_labels.py](../../scripts/eval/task_labels.py) builds [tasks.jsonl](tasks.jsonl) from git, at identity-v1's
repositories and pins ([repos.txt](../../scripts/eval/identity/repos.txt)), with their full history:

- **Fix commits:** non-merge commits since 2020-01-01 whose subject says they fix something (`fix`, `bug`, `crash`,
  `regression`, `incorrect`, `wrong`) and has at least four words. Left out: subjects about the build, CI, docs,
  dependencies, formatting, warnings, analyzer rules (`CA1849`), renames, imports, merges and conflicts, flaky tests
  and reverts.
- **One task each:** the commit has one parent, changes at most 12 files, and modifies between one and six source files
  that exist at its parent (`.cs`; `.ts`, `.tsx`, `.js`, `.jsx`; `.swift`; generated files left out).
- **The draw:** per repository, a fixed quota taken in SHA-256 order of `repo:sha`: 6 serilog, 6 autofac, 8
  jellyfin-server; 9 jellyfin-web, 9 hono; 5 swift-argument-parser, 5 swift-collections, 6 swift-nio.
- **The text:** the title of the issue the commit message closes (`Fixes #12`), when GitHub has that issue, else the
  commit's subject, without a squash merge's trailing pull request number. Titles are fetched once and stored with
  the set.
- **The labels:** the files the commit modified or deleted that exist at its parent: its source files (`files`) and,
  apart, its tests (`tests`: under a `test`, `tests`, `Tests`, `__tests__` or `spec` directory, or named `*.test.ts`,
  `*.spec.ts`, `*Tests.cs`, `*Tests.swift`). Files the commit added aren't labels: no answer at its parent can name
  them.

`task_labels.py check` recomputes every task's labels from git and exits 1 on a difference.

The set: 54 tasks, 20 C#, 18 TypeScript, 16 Swift; 6 from issue titles, 48 from commit subjects; 91 file labels and
26 test labels (18 tasks changed tests). Commit subjects can name what they fix (`fix(jwt): …`, a type's name), which
makes some tasks easier than an issue would be; that holds for every op scored.

## Scoring

[task_gate.py](../../scripts/eval/task_gate.py) checks out each task's parent commit as a worktree of its clone,
indexes it alone, and asks one tool over MCP (`serve --tools all`) with the task's text. Per task:

- **Bytes:** the answer's text, as a client reads it.
- **Named:** a label is named when its repository-relative path appears in the answer; within a budget of B bytes when
  its first mention ends within the first B bytes.
- **Recall:** of the changed source files, and of the changed tests (over the tasks that changed tests), on the whole
  answer and within 2,000, 4,000 and 8,000 bytes.

```sh
python3 scripts/eval/task_gate.py fetch --work WORK
python3 scripts/eval/task_gate.py run --knowcode BIN --work WORK --out locate.json        # locate, the baseline
# another op: its tool, the argument the task's text goes in, and any fixed arguments (names here are examples)
python3 scripts/eval/task_gate.py run --knowcode BIN --work WORK --tool kc_task --query-arg goal \
  --args '{"budget": 4000}' --out task.json
python3 scripts/eval/task_gate.py compare locate.json task.json
```

A run takes about three minutes on a loaded development Mac (54 indexes and answers).

## Baseline: `locate`, integration/0.6 at f112ff2

Descriptive figures, `kc_locate` with its defaults (10 files):

| Language | Tasks | Median bytes | Files @2000 | Files @4000 | Files @8000 | Files, all | Tests @2000 | Tests @4000 | Tests, all |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| C# | 20 | 3548 | 0.379 | 0.414 | 0.414 | 0.414 | 0.200 | 0.400 | 0.400 |
| Swift | 16 | 4104 | 0.444 | 0.556 | 0.556 | 0.556 | 0.125 | 0.188 | 0.188 |
| TypeScript | 18 | 2780 | 0.600 | 0.600 | 0.600 | 0.600 | 0.000 | 0.000 | 0.000 |
| All | 54 | 3420 | 0.483 | 0.527 | 0.527 | 0.527 | 0.115 | 0.192 | 0.192 |

38 of the 54 answers name at least one changed file.
