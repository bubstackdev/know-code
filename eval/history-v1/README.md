# History questions

Questions about a symbol's or a file's git history: which commits changed this method, when it last changed, who
wrote its current lines, what changes with it, and which of its callers changed with it. A development set for 0.5,
and 0.5's history gate: [history_gate.py](../../scripts/eval/history_gate.py) scores KnowCode and the plain-git
approaches against the same labels, with no model calls. No agent has run on it.

## Repositories

public-v2's C# repositories at public-v2's pins, cloned with their full history (`git clone`, then
`git checkout <pin>`): Autofac, Serilog, Serilog.Extensions.Hosting and Jellyfin server. public-v1 and public-v2
export these without history, so their studies hold no history questions.

## Labels

[history_labels.py](../../scripts/eval/history_labels.py) computes every label from the clone with git alone; no
KnowCode answer is read. The same clones and git version give the same labels.

| Family | Question | Label |
| --- | --- | --- |
| `symbol-commits` | Which commits changed method M? | `git log -L START,END:FILE`, M's span at the pin |
| `symbol-last` | When did M last change, and who changed it? | the newest of those |
| `symbol-blame` | Who wrote M's current lines? | `git blame -w -L START,END`, authors by lines |
| `symbol-cochange` | What else changes when M changes? | files in M's commits (at most 50 files each), at least two shared |
| `callers-history` | Which of M's production callers changed with M? | hop-1 callers' files from a published public-v2 label, in M's commits |
| `file-cochange` | What usually changes with file F? | files in F's non-merge commits (`--follow`; at most 50 files each), by shared commits, each under its current name |

A member's span runs from its declaration line to its closing brace, matched outside strings, characters and
comments. Each label records the span's first and last lines and the file's sha256. `git log -L` follows lines
by git's own diff, so a different git version can move a label; the gate recomputes every label and stops when one
differs from labels.jsonl.

## How plain tools answer

An agent with grep, find, read and git would reach for these:

| Approach | What it gives here |
| --- | --- |
| `git log -L :M:FILE` | Fails with "no match" in Serilog and Jellyfin: git's default function-name rule only matches unindented lines, and C# members are indented. In Autofac it matches, but the range runs on to the next match. |
| `git log -S M -- FILE` | Commits that changed how many times the name appears: misses body edits, and counts calls and overloads. |
| `git log --follow -- FILE` | Every commit to the file: full recall, low precision (729 commits for a 4-line method in BaseItem.cs). |
| `git blame -L :M: FILE` | Same matching as `-L`: no lines, or more lines than the method. |
| `git log --format= --name-only -- FILE \| sort \| uniq -c` | The file co-change label, short of the renames. |

So an exact answer to a method-level question needs the method's line range first: a search, then a read to find
the closing brace, then `git log -L` or `git blame -L` over that range. A file-level question is one pipeline.

## The gate

```sh
python3 scripts/eval/history_gate.py fetch --work WORK
python3 scripts/eval/history_gate.py run --knowcode BIN --work WORK --out summary.json
```

`fetch` clones the four repositories into `WORK/clones` and checks out their pins: about 250 MB, so it runs locally,
not in CI. `--repos serilog,serilog-extensions-hosting` keeps five questions and about 75 MB, small enough for CI if
the clones are cached by pin. `run` exits 1 when KnowCode doesn't beat each plain approach on the method-level and
caller questions (recall and precision at least equal, one higher), or falls more than 0.05 short of the best plain
approach's recall on the two file co-change controls.
