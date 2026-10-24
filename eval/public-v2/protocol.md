# Pre-registered agent study (public-v2)

Fixed on October 24, 2026, before the baseline runs. This study measures 0.3 against 0.2.0. The study follows the
[0.3 plan](../../docs/plans/0.3.md#gate), and its five criteria are the plan's, made operational here.

This document, the [study inputs](study/) and the scripts named below are fixed by [freeze.json](study/freeze.json),
whose SHA-256 identifies the study. Any deviation is recorded here, dated, before the run it affects. Results go on a
separate page, published after the release-candidate run.

The study runs on two builds:

- **The baseline**: the 0.2.0 release, built from the v0.2.0 tag.
- **The release candidate**: the 0.3 build named in a second freeze, made before that run starts. It differs from this
  freeze only in the KnowCode binary's hash and file name.

Each build is run twice on the bank, in the order below. The release candidate's criterion 1 is judged against this
baseline's value.

## Bank

- **Questions:** the [40 questions](study/questions.jsonl) of public-v2, 10 per task type. They are sampled as
  [sampling.md](sampling.md) describes, over ten public repositories, and none overlaps public-v1.
- **Labels:** made from pinned source only, before any arm ran, by separate AI agent sessions. A seeded subset of 8 was
  labelled again blind ([review.json](study/review.json)). No human has reviewed the labels. Correctness numbers rest on
  them, and the results page says so.
- **Empty and undecided items:** seven labels are empty, and HTTP labels may list `undecided` calls. Naming an undecided
  call is neither right nor wrong.

## Arms

There are three arms, as in public-v1:

| Arm | Tools | Brief |
| --- | --- | --- |
| `plain` | Read and Bash | Neutral: "use only the granted tools" |
| `knowcode` (named) | The same, plus KnowCode's MCP server | Names the KnowCode MCP tools |
| `knowcode-unprompted` | Same as `knowcode` | Word for word the `plain` brief; KnowCode isn't mentioned |

**Common configuration** ([config.json](study/config.json)):

- Model and effort: `claude-opus-5-5`, effort `medium`.
- Client: Claude Code 2.1.304 in bare print mode, a pinned copy whose hash is in freeze.json.
- Research budget: 40 calls, stated in the brief, soft. Overruns are reported.
- Operational timeout: 1,800 s per session.
- KnowCode: the `core` tool profile, which KnowCode serves by default. `kc_note_add` is denied.
- Index preparation runs before the client starts.
- Isolation, as in public-v1: a fresh source copy per session, an isolated client profile, the API key held only by
  a local relay, and KnowCode's cache, config and data inside the session.

**Answer contract:** as in public-v1. The reply ends with one JSON object in a fenced `json` block, with `status`,
`items` and `notes`, and impact answers add `hops`.

### Shell commands: the sandbox mode

public-v1 gave the client one allow rule per program. The client then refused any command it couldn't match, in every
arm: `cd x 2>/dev/null || cd y`, `f=…; sed $f`, `pwd`, and a `cd` outside its working directory. In the fifth-freeze
run and the 0.2 A/B, 91 of 420 Bash calls were refused this way. public-v2 uses the `sandbox` shell mode of
[bare_client.py](../../scripts/eval/bare_client.py), the same in every arm:

- **The client allows any Bash command.** The session's macOS sandbox is the boundary:
  - only the listed programs (`grep`, `find`, `cat`, `sed`, `head`, `tail`, `wc`, `ls`, `sort`, `uniq`, `xargs`) and
    bash builtins can run;
  - only the source copy can be read;
  - only the session's `control/` and `profile/` directories can be written;
  - there is no network.
- **The plain arm cannot execute the KnowCode binary through Bash.** The binary isn't on its sandbox's executable list,
  so a direct path, a variable, `xargs` and `find -exec` all fail with "Operation not permitted".
- **The KnowCode arms reach KnowCode only through MCP.** Their sandbox must let the client start the binary as its MCP
  server. So in every arm the client also refuses any Bash command naming the binary's file name
  (`knowcode_binary_name`, here `knowcode-v0.2.0`). Preparation refuses a session whose own paths contain that name.
- **Not covered:** a binary name built at runtime (`k=know; ${k}code …`) would get past the client's check in the KnowCode
  arms. The scorer counts every refused Bash call in every arm, split by whether it named the binary.

The probes behind these statements ran the pinned client under the real sandbox, against a local stand-in for the
Messages API, with no model calls (`harness-probe/` in the study's evidence). Their result, in every arm, with sessions
under a path containing "knowcode":

- an absolute-path `grep` of the source ran;
- the binary's direct, variable, `xargs` and `find -exec` forms were refused;
- in the KnowCode arms, an MCP call reached the server.

**Tripwires:** the runner stops when a session's client refuses a Bash command that doesn't name the binary. In this
mode that can only be a harness fault. It also stops when more than 3 sessions fail on the answer contract or
transport.

## Runs and order

- **Runs:** each build runs `b1`, then `b2`. Each run is the full schedule: 3 arms × 40 questions = 120 sessions.
- **Order:** within a run, each arm's question order is seeded independently and the arms rotate. This is public-v1's
  rule, with seeds `knowcode-public-v2-run-b1` and `knowcode-public-v2-run-b2` ([order.jsonl](study/order.jsonl)).
- **Same schedule for both builds:** the release candidate's runs use the same session schedule.

### Why two runs: the pilot

Before this protocol, the 0.2.0 release ran twice on the bank (runs `r1`, `r2`; 240 sessions; $24.58) to measure how
much a repeated run moves each criterion. The pilot isn't part of the study's results.

| Criterion | r1 | r2 |
| --- | ---: | ---: |
| 1. Re-check tokens per named session (mean) | 5,705 | 5,360 |
| 2. Re-checks that changed the answer (named, unprompted) | 7, 6 | 7, 6 |
| 3. F1 difference, named − plain (31 questions) | +0.034 | +0.017 |
| 3. Recall difference, named − plain (1 lower-bound question) | −0.667 | −0.667 |
| 4. Session tokens, named − plain (paired median) | +3,273 | +3,722 |
| 5. Unprompted sessions with a KnowCode call | 24 of 40 | 25 of 40 |

How the two runs per build were chosen:

1. **A repeated run moved criterion 1 by 6%.** The per-question run-to-run spread gives a one-session standard
   deviation of σ ≈ 3,114 tokens, around a mean μ ≈ 5,532.
2. **The noise in comparing two builds.** Compare two builds on the same 40 questions with k runs each. The difference
   in their criterion-1 means has a standard deviation of σ·√(2/(40·k)). That is 696 tokens for k = 1 and 492 for
   k = 2.
3. **Against the effect we look for.** 1.96 times that, 1,365 and 965 tokens, is well under a halving (μ/2 ≈ 2,766).
4. **On the ratio scale criterion 1 uses.** A build that changed nothing would have to fall 5.5 standard deviations to
   look halved at k = 1, and 7.8 at k = 2.

So 40 questions and two runs per build, as the plan proposed, keep a repeated run's movement of criterion 1 well below
the halving the criterion looks for.

**Discarded attempt:** a first pilot attempt was stopped after 42 sessions ($4.88). Its deny rule matched the word
"knowcode", which also appears in the sessions' own paths, so the client refused any Bash command naming a source path
by its absolute path. That was 28 of 92 Bash calls, in every arm. The rule now matches the binary's file name, and
preparation refuses a colliding session (#69, #70). The 42 sessions were discarded and are not used anywhere.

## Metrics

Per session:

- **Session tokens:** every context token each model turn processed (input, cache creation and cache reads), plus
  output. This is public-v1's research-token measure, with tool schemas included. Cache reads and uncached input are
  also reported apart.
- **Re-check tokens:** the sum, over the calls that re-check a KnowCode answer, of the tokens each adds to the session,
  per [recheck_cost.py](../../scripts/eval/recheck_cost.py):
  - which calls re-check is decided by public-v1's detector, [recheck.py](../../scripts/eval/recheck.py), unchanged;
  - a call's tokens are the context it adds, times the later model turns that read it, estimated from per-turn usage;
  - a call counted against several answers counts once.
- **Changed answers:** a re-checking call changed the answer when the final answer names a location the call showed
  and no KnowCode answer in the session named.
- **Correctness:** F1 on exhaustive labels with items, recall on lower-bound labels, and correct-empty on empty labels,
  scored by [score.py](../../scripts/eval/neutral/score.py). An errored, refused or missing session counts as F1 0 and
  recall 0.
- **Calls:** research calls, KnowCode calls, budget overruns, and refused Bash calls split by whether they named the
  binary.
- **Descriptive:** fixed overhead, public-v1's pooled re-check rate, and use per task type.

## Criteria

These are the 0.3 plan's five criteria. For each build, they are computed over both runs pooled: every (run, question)
pair in an arm.

1. **Re-check cost halved.** The release candidate's mean re-check tokens per named-arm session is at most half the
   0.2.0 baseline's, on the same questions.
2. **Re-checks that change the answer** are reported per run (descriptive). Each one is a KnowCode failure: a
   re-check that found something KnowCode didn't say. All of them are listed.
3. **Answer F1 and recall at least equal** to the plain arm. The mean paired difference (named − plain) in F1, on
   exhaustive labels with items, is ≥ 0, and the mean paired difference in recall, on lower-bound labels, is ≥ 0.
   - The bank has one lower-bound label. That recall difference rests on one question, and the results page says so.
4. **Research tokens at most equal** to the plain arm: the median paired difference in session tokens
   (named − plain) is ≤ 0. Tool schemas are included.
5. **Unprompted use:** KnowCode is called in at least 20 of every 24 sessions where the prompt doesn't name it. That is
   a share of at least 20/24 of the unprompted sessions over both runs: 67 of 80.

Every number is published with a 95% interval:

- a percentile cluster bootstrap over questions (10,000 resamples, seed `knowcode-public-v2-bootstrap-2026-10-23`),
  which keeps both runs of a question together;
- a Wilson interval for criterion 5.

Criteria are decided on point estimates. A missed criterion is reported as missed, with every failure and loss.

## Scoring

[public_v2.py](../../scripts/eval/public_v2.py) runs the study and scores it. Scoring a build looks like this:

```sh
public_v2.py score --freeze study/freeze.json --runs-dir RUNS --runs b1,b2 [--baseline BASELINE_SUMMARY] --out summary.json
```

The baseline is scored without `--baseline`, which records its criterion-1 value. The release candidate is scored
with the baseline's summary, which decides criterion 1.

Raw transcripts aren't published, because they contain build-machine paths. Summaries and per-session rows are.

## Deviations

None yet.
