# Monitors against history

0.8's gate item "Monitors" ([plan](../../docs/plans/0.8.md#gate)): a monitor replayed over the history of public
repositories fires on exactly the commits that are labelled. This README, the rules and the repository list were
committed before KnowCode's answers on them were read.

## The monitors

A monitor is a rule run with a base: `knowcode rules --rules <file> --new-since <base>` reports the matches the
working tree has that the base didn't. A match is known by its rule and its whole matched text, so a match that
moved (within its file or to another) or whose file was renamed is not new. With more matches of one rule and text
than the base had, the difference is new.

- [csharp.yml](csharp.yml): four C# rules (sync over async, hand-written null guards, exceptions built without a
  message, `lock (this)`), replayed on serilog and Autofac.
- [typescript.yml](typescript.yml): four TypeScript rules and one Tsx rule (`console.log`, casts to `any`,
  `@ts-ignore` and its kin, non-null assertions), replayed on hono.
- [repos.txt](repos.txt): each repository with the commit whose history is replayed (the pins of identity-v1).

## The replay

For each repository, the last 200 commits on the pinned commit's first-parent line that have one parent, oldest
first. At each commit, the monitor runs in a checkout of that commit with the commit's parent as its base; it fires
when it reports a match.

## The labels

[monitor_gate.py](../../scripts/eval/monitor_gate.py) `label` labels each commit from git and ast-grep alone: it
writes out the files the commit changes (`git diff --raw -M` against its parent), each at the commit and at the
parent, and runs the ast-grep 0.50.0 command line (as in [rules-v1](../rules-v1/README.md), in a throwaway
container with no network, with every ignore file turned off) over both sides. The commit is labelled as firing when
some (rule id, matched text) occurs more often at the commit than at the parent. Files the commit doesn't change
hold the same matches on both sides, so they aren't read.

Both sides read the same files: those git tracks, minus what KnowCode never reads (symbolic links, and the dependency
and build directories `node_modules`, `Pods`, `Carthage`, `DerivedData`, `vendor`, `third_party`, `.venv`,
`venv`).

The labels are in [labels](labels), one line per commit: whether it fires and how many new matches ast-grep counts.

`monitor_gate.py run` replays the monitor with a KnowCode build and lists every commit where it and the labels
differ. The gate passes with none.

## Results

On the build of `w4/rules-engine` (KnowCode 0.6.1 plus the rules op), 600 commits, and again, with the same results,
on `w4/b-rules-op`'s build (the vendored ast-grep-config):

| Repository | Commits | Labelled as firing | Monitor fired | Differ | New matches differ |
| --- | ---: | ---: | ---: | ---: | ---: |
| serilog | 200 | 2 | 2 | 0 | 0 |
| autofac | 200 | 5 | 5 | 0 | 0 |
| hono | 200 | 20 | 20 | 0 | 0 |

Besides firing, the number of new matches the monitor reports equals the labels' count at every commit.

Text monitors, on the same commits (labels from git, frozen before the runs):

| Repository | Monitor | Commits | Labelled as firing | Monitor fired | Differ | New lines differ |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| serilog | text-1 | 200 | 0 | 0 | 0 | 0 |
| serilog | text-2 | 200 | 10 | 10 | 0 | 0 |
| autofac | text-1 | 200 | 0 | 0 | 0 | 0 |
| autofac | text-2 | 200 | 14 | 14 | 0 | 0 |
| hono | text-1 | 200 | 4 | 4 | 0 | 0 |
| hono | text-2 | 200 | 89 | 89 | 0 | 0 | Two hono
answers are marked incomplete (`unsupported`): a file they read has a parse error, which the answer names; ast-grep
parses it with the same grammar.

## Text monitors

A text monitor is a line regex run with a base: `knowcode rules --regex <re> -g <glob> --new-since <base>`. Each line
the regex matches is a match, known by its whole text (without the line break); with more lines of one text in the
files that differ from the base than the base's versions of those files had, the difference is new. As with rules, a
line that moved (within its file or to another) or whose file was renamed is not new.

- serilog and Autofac: `TODO|FIXME|HACK` (text-1) and `nameof\(|ArgumentNullException` (text-2) in `**/*.cs`;
- hono: `console\.(log|warn|error)` (text-1) and `as any|expect\(` (text-2) in `**/*.ts`.

The text-2 monitors were added after text-1's labels showed it fires rarely (0, 0 and 4 commits of 200), so that
the set has firing commits to get right; no monitor answer had been read.

Each regex means the same as a POSIX extended regex (git's `-E`) and as a Rust regex. The replay is the structural
one's: the same 200 commits per repository, each with its parent as the base.

The labels come from git alone. The candidates are the commits `git log -E -G<re> -- ':(glob)<glob>'` lists: those
whose patch adds or removes a line the regex matches. A candidate is labelled as firing when, over the files the
commit changes (`git diff -M` against its parent, the files KnowCode reads as above), the lines it adds that the regex
matches outnumber, for some line text, the identical lines it removes. `-G` alone also lists commits that only remove
or move a matching line, which a monitor of what is new must not fire on. These labels are written before any text
monitor's answer is read: [labels](labels), `<repo>-text-1.jsonl` and `<repo>-text-2.jsonl`.

## Results

On the build of `w4/rules-engine` (KnowCode 0.6.1 plus the rules op), 600 commits, and again, with the same results,
on `w4/b-rules-op`'s build (the vendored ast-grep-config):

| Repository | Commits | Labelled as firing | Monitor fired | Differ | New matches differ |
| --- | ---: | ---: | ---: | ---: | ---: |
| serilog | 200 | 2 | 2 | 0 | 0 |
| autofac | 200 | 5 | 5 | 0 | 0 |
| hono | 200 | 20 | 20 | 0 | 0 |

Besides firing, the number of new matches the monitor reports equals the labels' count at every commit.

Text monitors, on the same commits (labels from git, frozen before the runs):

| Repository | Monitor | Commits | Labelled as firing | Monitor fired | Differ | New lines differ |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| serilog | text-1 | 200 | 0 | 0 | 0 | 0 |
| serilog | text-2 | 200 | 10 | 10 | 0 | 0 |
| autofac | text-1 | 200 | 0 | 0 | 0 | 0 |
| autofac | text-2 | 200 | 14 | 14 | 0 | 0 |
| hono | text-1 | 200 | 4 | 4 | 0 | 0 |
| hono | text-2 | 200 | 89 | 89 | 0 | 0 | Two hono
answers are marked incomplete (`unsupported`): a file they read has a parse error, which the answer names; ast-grep
parses it with the same grammar.

## Text monitors

The gate also names monitors with a text pattern, labelled with `git log -G`. They are not in this set yet: an
ast-grep rule always matches syntax nodes, so a text monitor needs its own form, to be added with its own labels.
