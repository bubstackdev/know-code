# Review answers against git

Commits that a later fix repaired, from pinned public repositories. Each is labelled with the files and tests that
fix changed. Neither KnowCode nor a model labels anything. The set is 0.8's gate item "Review answers"
([plan](../../docs/plans/0.8.md#gate)): recall of those files within a byte budget, for `review --diff` against the
commit, reported against `impact --diff` alone. The review answer must be higher at equal or smaller median answer
bytes.

This README, the labeller, the scorer and [reviews.jsonl](reviews.jsonl) were committed before any KnowCode answer on
the set was read. Nothing here changes after a run. A change makes a new version, review-v2.

## Reviews

[review_labels.py](../../scripts/eval/review_labels.py) builds [reviews.jsonl](reviews.jsonl) from git, at tasks-v1's
repositories and pins ([repos.txt](../../scripts/eval/identity/repos.txt)), with their full history:

- **Fix commits:** the ones [tasks-v1](../tasks-v1/README.md) would take. They are non-merge commits since 2020-01-01
  whose subject says they fix something and has at least four words, minus the build, docs and tooling subjects
  tasks-v1 leaves out. Each has one parent, changes at most 12 files, and modifies between one and six existing source
  files.
- **The introducing commit:** the fix's rewritten or deleted source lines are blamed at its parent with
  `git blame -w`, apart from blank lines and lines of only brackets and separators. Test files aren't blamed. The
  commit blamed for the most of those lines is taken, among those that landed (committer date) at most 90 days before
  the fix. It must have one parent, change at most 30 files and at least one source file, and its subject must not be a
  merge, a revert, a reformat, a rename or a move. A fix that only adds lines has nothing to blame and gives no review.
- **One review per introducing commit,** labelled by the earliest fix that names it.
- **The labels:** the files the fix modified or deleted that exist at the introducing commit. They are split into its
  source files (`files`) and its tests (`tests`), by tasks-v1's test paths. Files the fix added aren't labels, since no
  answer at the introducing commit can name them. `in_diff` lists the labels the introducing commit itself changed.
- **The draw:** 25 reviews per language (C#, TypeScript, Swift). A language's repositories are taken in turn, in name
  order, one review at a time, so a small repository gives all it has and the rest come from the others. Each
  repository's reviews are taken in SHA-256 order of `review-v1:<repo>:<commit>`, the recorded seed.

Each row also records the fix, the days between the two commits, how many of the fix's blamed lines the introducing
commit holds (`blamed`, of `removed`), and the files it changed (`changed`). `review_labels.py check` recomputes every
review from git and exits 1 on a difference. `census` counts each repository's candidates and why the others were
dropped.

Blame finds the commit that last wrote a line, not always the one that made the mistake. A fix can also change files
the mistake never reached. The labels are what a later fix needed, measured the same way for every op scored.

## Parts

The set is frozen in parts, and the gate is decided for each part on its own as well as combined, so a part added
later can't move an earlier part's result. Each row's `part` says which it is in.

- **Part 1:** tasks-v1's eight repositories (C#, TypeScript, Swift), 75 reviews. Frozen in the commit that adds [reviews.jsonl](reviews.jsonl).
- **Part 2:** the Go and JVM repositories 0.7 pins, frozen on their own before any answer on them is read. Not yet
  built.

## Part 1

75 reviews, 25 per language. There are 141 file labels and 39 test labels (28 reviews' fixes changed tests), and 54
labels lie outside the introducing commit's own diff. The median review lands 6.4 days before its fix and changes 6
files.

| Repository | Language | Fix commits | Reviews possible | Drawn |
| --- | --- | ---: | ---: | ---: |
| autofac | C# | 80 | 8 | 8 |
| jellyfin-server | C# | 982 | 225 | 14 |
| serilog | C# | 24 | 3 | 3 |
| hono | TypeScript | 763 | 187 | 13 |
| jellyfin-web | TypeScript | 1001 | 202 | 12 |
| swift-argument-parser | Swift | 49 | 4 | 4 |
| swift-collections | Swift | 137 | 33 | 11 |
| swift-nio | Swift | 100 | 12 | 10 |

| Language | Reviews | File labels | Test labels | Reviews with tests | Labels outside the diff |
| --- | ---: | ---: | ---: | ---: | ---: |
| C# | 25 | 50 | 9 | 7 | 17 |
| TypeScript | 25 | 38 | 14 | 12 | 8 |
| Swift | 25 | 53 | 16 | 9 | 29 |

Most fix commits give no review. Of the 3,136 across the eight repositories, 1,119 had no introducing commit within
90 days, 568 had no line to blame (they only added lines, or rewrote only blank and bracket lines), 549 weren't one fix
in tasks-v1's sense, 109 named an introducing commit larger than 30 files, and 25 had no label left at it. The other 766
name 674 introducing commits (`census` gives the counts per repository).

## Scoring

[review_gate.py](../../scripts/eval/review_gate.py) checks out each review's introducing commit as a worktree of its
clone and indexes it alone, with the build's own cache, config and data. It then asks one tool over MCP
(`serve --tools all`) with the diff from the commit's parent (`{"diff": <parent>}`). Per review:

- **Bytes:** the answer's text, as a client reads it.
- **Named:** a label is named when its repository-relative path appears in the answer. Within a budget of B bytes, it
  counts when its first mention ends within the first B bytes. This is tasks-v1's rule.
- **Recall:** of the source files, of the tests (over the reviews whose fix changed tests), and of the labels outside
  the commit's own diff (`outside`). Each is taken on the whole answer and within 2,000, 4,000 and 8,000 bytes.

`compare` pairs two runs review by review. For each set of labels and each budget, it gives the pooled recall of each
run and the mean paired difference of per-review recall (candidate minus base), with wins, ties and losses. It also
gives the two median byte counts.

**The gate:** the candidate's pooled file recall within 4,000 bytes is higher than `impact --diff`'s, and its median
answer bytes are no larger. Every other figure is reported beside it and decides nothing.

```sh
python3 scripts/eval/task_gate.py fetch --work WORK
python3 scripts/eval/review_gate.py run --knowcode BIN --work WORK --tool kc_impact --out impact.json   # the baseline
python3 scripts/eval/review_gate.py run --knowcode BIN --work WORK --tool kc_review --out review.json
python3 scripts/eval/review_gate.py compare impact.json review.json
```
