# Public agent study: results (public-v1)

This page reports the agent study pre-registered in [protocol.md](protocol.md). Every number comes from
[score_public_study.py](../../scripts/eval/score_public_study.py) applied to the frozen inputs. Misses are stated as
misses. The study is small, and it compares KnowCode only with the plain tools tested here (grep, find, read). It
says nothing about other tools, other models or other codebases.

**Release status.** 0.2.0 was released on October 22, 2026. Its code is the 6b8e4b5 build that the fifth-freeze run
measured; the commits after it before the release changed only this evaluation and the changelog. The final run measured the release candidate at 00aae5b. After its
results were published, a fix round landed, and the study was run once more as a **rerun** on main at b0a3564 (still
version 0.2.0). Two more fix rounds followed, and the study was run a fourth time under a **fifth freeze**, on main at
6b8e4b5 (still version 0.2.0) with the `core` tool profile KnowCode serves by default. Each later run is reported next
to the earlier ones and does not replace them. Each run's numbers describe the build it measured.

**Summary.**

| Criterion | Final run (00aae5b) | Rerun (b0a3564) | Fifth freeze (6b8e4b5, `core`) |
| --- | --- | --- | --- |
| 1. Re-check rate at least halved | **Missed.** 0.27 [0.16, 0.42]; required ≤ 0.167 | **Missed.** 0.38 [0.25, 0.59]; required ≤ 0.167 | **Missed.** 0.52 [0.39, 0.70]; required ≤ 0.167 |
| 2. Answer F1 and recall at least equal | **Met.** F1 +0.02, recall +0.13 | **Missed.** F1 +0.06, but recall −0.09 [−0.32, +0.03] | **Met.** F1 0.00, recall 0.00 (every arm 1.00) |
| 3. Fewer research tokens | **Missed.** +28.4k [+19.2k, +81.7k] per session | **Missed.** +7.5k [+1.2k, +17.7k] per session | **Missed.** +3.7k [−0.2k, +10.8k] per session |
| 4. Unprompted use | **Met.** 24 of 24 sessions | **Missed.** 18 of 24 sessions, 0.75 [0.55, 0.88]; required ≥ 20 | **Met.** 24 of 24 sessions, 1.00 [0.86, 1.00] |

The fifth-freeze run met criteria 2 and 4 and missed criteria 1 and 3:

- **Criterion 1.** Its named-arm re-check rate, 0.52, is the highest of the four runs.
- **Criterion 3.** KnowCode sessions used about as many tokens as plain ones: the paired median is +3.7k and its
  interval crosses zero. That is not fewer tokens, and this page doesn't present it as an improvement.
- **Criteria 3 and 4 measure a different setup.** They were measured on the shipped `core` profile, where earlier
  runs used `all`; see the note under [Criteria](#criteria).

The rerun missed all four criteria. Its recall miss comes from one named-arm session whose answer failed the format
contract (see "Every failure"). Run-to-run differences between the final run and the rerun are within what one run
of 24 questions per arm can show, so they are not evidence that the fix round helped or hurt.

## Disclosures first

- **Labels were made by AI, and no human reviewed them.** Every label, the independent blind review and the
  adjudication were done by separate AI agent sessions (Claude Opus 5.5). On the 8 seeded double-labelled questions,
  required items agreed on 8 of 8 and completeness on 4 of 8 ([review.json](study/review.json)). Correctness numbers
  rest on these labels.
- **The study is small.** F1 rests on 10 questions, lower-bound recall on 8 and correct-empty on 5, so the intervals
  are wide. A criterion that is "met" or "missed" on a point estimate can turn on a single session, as the rerun's
  recall shows.
- **Correctness is near the ceiling.** In the final run and the rerun, the KnowCode arms scored 1.00 F1 and the plain
  arm 0.94–0.98. The study can't detect correctness differences smaller than those few questions allow.
- **Agent behavior is not tool correctness.** These numbers describe what one model did with each arm's tools.
  KnowCode's own answer accuracy is reported separately, in the accuracy bank.
- **There was a pilot.** The first freeze's run was stopped after 23 of 72 sessions because 12 answers failed the
  answer format, 4 in each arm. Those sessions were declared a pilot, and no criterion was computed from them. See
  [Pilot](#pilot).
- **There was a rule gap.** After the baseline, a rule for broken relay streams was adopted (see "Transport retries"
  in protocol.md). The final run and the rerun use it. The baseline is shown both under its frozen strict check
  (primary) and under the adopted rule.
- **The rerun's detector reads 0.2's compact answers.** Before the rerun, the re-check detector was adapted to text
  forms the newer build uses. Re-scored, the published baseline and final runs give identical criterion-1 counts. See
  "Re-check detector" in protocol.md.
- **The fifth freeze changed the tool profile.** Earlier runs served all 25 tools (`all`). The fifth freeze serves
  the 8 tools of the `core` profile, which KnowCode uses by default. Criteria 3 and 4 therefore compare builds and
  profiles together across runs; see the note under [Criteria](#criteria).
- **Runs of one build vary.** The size of the differences between runs here is within what repeated runs of one
  build produce. An exploratory check before the fifth freeze ran the same binary on two days, on 12 held-out
  questions outside this study. The unprompted arm's re-check rate moved by about 0.2, and its median session tokens
  by about 6k. The plain arm's median session also moved between runs: 40.5k in the rerun, 33.6k in the fifth-freeze
  run.
- **An earlier rerun attempt was stopped.** The user stopped a rerun on the 702da21 build after 6 of 72 sessions.
  Those sessions are discarded and not scored, and the rerun was started again from scratch on b0a3564.
- **The final run's per-session rows are not published**, a deviation from protocol.md. See
  [Reproducing](#reproducing).

## Runs

| | Baseline | Final | Rerun | Fifth freeze |
| --- | --- | --- | --- | --- |
| KnowCode | 0.1.1 code (main at 36eec45), sha256 `42c8fd09…` | 0.2.0 candidate (main at 00aae5b), sha256 `f7a15757…` | 0.2.0 (main at b0a3564), sha256 `e55fdc31…` | 0.2.0 (main at 6b8e4b5), sha256 `1bda7bbe…` |
| Tool profile | `all` | `all` | `all` | `core` |
| Freeze | second freeze, `9bc06141…` | third freeze, `0141cee5…` | fourth freeze, `6298a569…` | fifth freeze, `cb371bf5…` |
| Sessions | 72 of 72 | 72 of 72 | 72 of 72 | 72 of 72 |
| Model and client | `claude-opus-5-5` at effort medium, Claude Code 2.1.304 | same | same | same |
| Spend (client-reported) | $12.74, plus $2.96 pilot and $0.55 cost pilot | $10.73 | $9.36, plus $0.58 for the stopped attempt | $7.87 |

## Criteria

Intervals are 95%: a cluster bootstrap over questions (10,000 resamples), or a Wilson interval for criterion 4.
"Named" is the `knowcode` arm, whose brief mentions KnowCode. "Unprompted" is the `knowcode-unprompted` arm, which
has the same tools and the plain arm's brief. Criterion 1 compares each run with the baseline's rate.

| Criterion | Baseline (primary, strict) | Baseline, adopted rule | Final | Rerun | Fifth freeze |
| --- | --- | --- | --- | --- | --- |
| 1. Re-check rate at least halved (named arm, pooled) | 27 of 81, 0.33 [0.18, 0.55] | 0.33 [0.18, 0.55] | 33 of 123, 0.27 [0.16, 0.42]: **missed** | 33 of 87, **0.38** [0.25, 0.59]: **missed** | 31 of 60, **0.52** [0.39, 0.70]: **missed** |
| 3. Session tokens (named − plain, paired median, n=24) | +27.6k [+23.0k, +46.2k]: 82.8k against 39.1k | same | +28.4k [+19.2k, +81.7k]: 72.4k against 39.5k: **missed** | **+7.5k** [+1.2k, +17.7k]: 61.9k against 40.5k: **missed** | **+3.7k** [−0.2k, +10.8k]: 33.3k against 33.6k: **missed** |
| 2a. F1 (named − plain, exhaustive labels, n=10) | +0.23 [0.00, 0.50] | +0.14 [0.00, 0.36] | +0.02 [0.00, 0.07]: met | **+0.06** [0.00, 0.17]: 1.00 against 0.94 | **0.00** [0.00, 0.00]: 1.00 against 1.00 |
| 2b. Recall (named − plain, lower-bound labels, n=8) | +0.25 [0.00, 0.60] | 0.00 [0.00, 0.00] | +0.13 [0.00, 0.40]: met | **−0.09** [−0.32, +0.03]: 0.88 against 0.96: **missed** | **0.00** [0.00, 0.00]: 1.00 against 1.00: met |
| 4. Unprompted use (sessions with a KnowCode call) | 22 of 24, 0.92 [0.74, 0.98] | same | 24 of 24, 1.00 [0.86, 1.00]: met | **18 of 24**, 0.75 [0.55, 0.88]: **missed** | **24 of 24**, 1.00 [0.86, 1.00]: met |

**Criteria 3 and 4 in the fifth freeze use the shipped `core` profile.** The earlier runs used `all`, so the setup
differs in three ways:

- The KnowCode arms are offered 8 tools, all loaded up front, instead of 25.
- Fixed overhead, the first turn's context, falls from 5.5k tokens in the rerun to 3.7k, and criterion 3 counts it on
  every turn.
- Criterion 4 counts calls to that smaller tool set.

A change in these two criteria between the rerun and the fifth-freeze run reflects the build and the profile
together. Differences of this size between runs are also within what repeated runs of one build produce (see
"Disclosures first").

What the fifth-freeze run shows:

- **Re-checks did not fall.** The named arm re-checked 31 of 60 KnowCode answers (0.52), at 2.6 KnowCode calls per
  session. The interval overlaps the rerun's.
- **Tokens are about level with plain, not lower.** The named arm's median session was 33.3k against plain's 33.6k.
  The paired median difference is +3.7k, with an interval from −0.2k to +10.8k that crosses zero. The unprompted arm's
  paired difference is +9.1k [+6.2k, +13.4k].
- **Correctness was at the ceiling in every arm.** F1 and recall were 1.00 in all three arms. Both KnowCode arms
  answered all 5 empty questions correctly. Plain answered 3, and declined the other 2 (HTTP 001 and 006) as
  `unsupported`.
- **Every unprompted session used KnowCode.** All 24 called it, at 3.0 calls per session.

What the rerun shows:

- **Re-checks did not fall.** The named arm re-checked 33 of 87 KnowCode answers (0.38). It made fewer KnowCode calls
  per session than in the final run (3.8 against 5.5), and a larger share of them was re-checked. The intervals of
  the baseline, final and rerun rates overlap.
- **The token gap narrowed but did not close.** Fixed overhead per turn fell to 5.5k tokens (from 6.5k in the final
  run and 10.2k in the baseline). The named arm's median session fell to 61.9k, and the paired gap over the plain arm
  shrank to +7.5k. KnowCode sessions still processed more tokens than plain ones.
- **Correctness: F1 higher, recall lower because of one session.** The named arm scored 1.00 F1 against the plain arm's 0.94.
  Its recall was 0.88 against 0.96, because one named-arm DI session (005, a lower-bound question) failed the answer
  format and scores zero. Without that session the named arm's recall would be 1.00, but the frozen rule counts it.
- **Unprompted use fell.** 18 of 24 unprompted sessions called KnowCode, at 3.0 calls per session, against 24 of 24
  in the final run.

## Sensitivity views

The pre-registered view leaves out Bitwarden HTTP questions 001 and 003, whose labels depend on the separately
licensed tree left out of the export.

| View | 1. Re-check rate | 2a. F1 diff | 2b. Recall diff | 3. Tokens diff | 4. Unprompted use |
| --- | --- | --- | --- | --- | --- |
| Baseline without Bitwarden HTTP 001 and 003 (pre-registered) | 0.45 [0.28, 0.65], n=60 | +0.23 [0.00, 0.50] | +0.25 [0.00, 0.60] | +27.6k [+23.2k, +46.2k], n=22 | 21 of 22, 0.95 [0.78, 0.99] |
| Final without Bitwarden HTTP 001 and 003 (pre-registered) | 0.35 [0.24, 0.51], n=94; required ≤ 0.225: **missed** | +0.02 [0.00, 0.07]: met | +0.13 [0.00, 0.40]: met | +25.4k [+17.5k, +67.5k], n=22: **missed** | 22 of 22, 1.00 [0.85, 1.00]: met |
| Rerun without Bitwarden HTTP 001 and 003 (pre-registered) | 0.42 [0.25, 0.66], n=69; required ≤ 0.225: **missed** | +0.06 [0.00, 0.16] | −0.09 [−0.32, +0.03]: **missed** | +7.5k [+4.5k, +17.7k], n=22: **missed** | 17 of 22, 0.77 [0.57, 0.90]: **missed** |
| Fifth freeze without Bitwarden HTTP 001 and 003 (pre-registered) | 0.50 [0.37, 0.68], n=58; required ≤ 0.225: **missed** | 0.00 [0.00, 0.00]: met | 0.00 [0.00, 0.00]: met | +3.7k [−0.6k, +10.8k], n=22: **missed** | 22 of 22, 1.00 [0.85, 1.00]: met |
| Baseline counting all 7 retried answers as valid (requested after the run) | 0.33 | +0.14 [0.00, 0.36] | 0.00 | +27.6k | 22 of 24 |
| Final counting retried answers as valid | no session changes, so identical to the primary | | | | |
| Rerun counting retried answers as valid | no transport failures, so identical to the primary | | | | |
| Fifth freeze counting retried answers as valid | no transport failures, so identical to the primary | | | | |

## Other measures

| Arm | Run | Answered / error / unsupported | Correct-empty (of 5) | Research calls (mean) | KnowCode calls (mean) | Re-check rate | Fixed overhead (median) |
| --- | --- | --- | --- | ---: | ---: | --- | ---: |
| plain | baseline | 17 / 4 / 3 | 2 | 8.3 | 0 | n/a | 2.5k |
| plain | final | 22 / 1 / 1 | 4 | 6.7 | 0 | n/a | 2.5k |
| plain | rerun | 21 / 0 / 3 | 1 | 8.3 | 0 | n/a | 2.5k |
| plain | fifth freeze | 22 / 0 / 2 | 3 | 5.6 | 0 | n/a | 2.5k |
| knowcode | baseline | 22 / 2 / 0 | 4 | 6.3 | 3.7 | 0.33 [0.18, 0.55] | 10.2k |
| knowcode | final | 24 / 0 / 0 | 5 | 8.0 | 5.5 | 0.27 [0.16, 0.42] | 6.5k |
| knowcode | rerun | 23 / 1 / 0 | 4 | 6.8 | 3.8 | 0.38 [0.25, 0.59] | 5.5k |
| knowcode | fifth freeze | 24 / 0 / 0 | 5 | 5.0 | 2.6 | 0.52 [0.39, 0.70] | 3.7k |
| knowcode-unprompted | baseline | 21 / 2 / 1 | 4 | 6.3 | 4.2 | 0.24 [0.12, 0.42] | 10.2k |
| knowcode-unprompted | final | 24 / 0 / 0 | 5 | 7.3 | 4.6 | 0.28 [0.17, 0.43] | 6.5k |
| knowcode-unprompted | rerun | 23 / 0 / 1 | 4 | 5.9 | 3.0 | 0.32 [0.17, 0.53] | 5.5k |
| knowcode-unprompted | fifth freeze | 24 / 0 / 0 | 5 | 5.2 | 3.0 | 0.41 [0.26, 0.59] | 3.7k |

**Stale-index answers** (descriptive, pre-registered before the rerun; not a criterion): successful KnowCode answers
that said the index was still being built.

| Run | Answers | Sessions |
| --- | --- | ---: |
| Baseline | 9 of 186 | 8 |
| Final | 41 of 238 | 33 |
| Rerun | 7 of 162 | 7 |
| Fifth freeze | 0 of 134 | 0 |

- No session went over the 40-call research budget in any run.
- Hop assignment agreed with the labels wherever an impact answer found a labelled item.
- The baseline rows use the strict check.
- An `unsupported` answer is not counted as a correct empty answer.

## Descriptive diagnostics (fifth freeze; not criteria)

These five measures were pre-registered in protocol.md ("Fifth-freeze diagnostics") before the fifth-freeze run, as
descriptive only. None is a criterion, and none changes a verdict above. A separate script computes them from the
same transcripts, using the frozen re-check detector. They are in `results/fifth/diagnostics.json`.

| Measure | plain | knowcode | knowcode-unprompted |
| --- | ---: | ---: | ---: |
| Sessions with at least one re-checked answer (of sessions with a claiming answer) | n/a | 20 of 24 | 18 of 24 |
| Mean re-checked share per session | n/a | 0.66 | 0.56 |
| Pooled re-check rate, same-turn calls excluded | n/a | 0.50 (30 of 60) | 0.36 (25 of 69) |
| Session tokens, median | 33.6k | 33.3k | 30.7k |
| Cache reads, median | 21.8k | 24.2k | 21.2k |
| Uncached input (input plus cache creation), median | 6.9k | 7.2k | 7.4k |
| Bytes per successful KnowCode answer, mean / median | n/a | 2,760 / 1,988 | 2,729 / 1,879 |
| KnowCode answer bytes per session, median | n/a | 5.5k | 6.2k |

**Why answers were re-checked.** Each re-checked answer is classed by what it said just before the check.

| Trigger (first matching rule) | knowcode | knowcode-unprompted |
| --- | ---: | ---: |
| The check was issued in the same model turn as the answer | 1 | 3 |
| The answer carried a stale-index note | 0 | 0 |
| The answer marked something unsupported | 1 | 1 |
| The answer said it was incomplete or cut | 6 | 4 |
| The answer was empty, or named nothing for what was asked | 2 | 1 |
| A search hit was searched for again | 1 | 1 |
| The agent read source around a claimed line | 10 | 10 |
| A complete answer was verified anyway | 10 | 8 |
| **Total** | **31** | **28** |

The classes come from ordered, rule-based checks written by an AI agent and checked by hand on samples only. Read
them as an approximate breakdown. The "empty" class over-counts on HTTP questions: it also fires when an answer's
nested caller list is empty, and on the questions whose correct answer is empty, that list is the right answer. By
producing tool, the re-checked answers came from `kc_context` (13 and 8), `kc_search` (8 and 9), `kc_refs` (4 and 4),
`kc_impact` (4 and 2) and `kc_find` (2 and 5).

## Every failure

**Baseline: format.** 1 contract failure, in `plain` DI 002: its JSON block has an extra closing brace.

**Baseline: transport.** 7 sessions were scored as errors under the strict check: `plain` impact 001, 003 and 005;
`knowcode` HTTP 004 and impact 006; `knowcode-unprompted` impact 005 and 006. Under the adopted rule, 6 of them are
valid; `knowcode` HTTP 004 still fails.

**Baseline: declined.** 4 sessions answered `unsupported`: `plain` HTTP 001, 003 and 006, and `knowcode-unprompted`
HTTP 001.

**Final: format.** No contract failures.

**Final: transport.** `plain` DI 005 failed: the client used a response the relay recorded as cut off
(IncompleteRead). `knowcode` HTTP 006 had one abandoned attempt, which the adopted rule accepts, and was scored
normally.

**Final: declined.** 1 session answered `unsupported`: `plain` HTTP 003.

**Rerun: format.** 1 contract failure, in `knowcode` DI 005: its JSON block has an extra closing brace. It is a
lower-bound question, so this one session sets the named arm's recall to 0.88 and causes the criterion 2 miss.

**Rerun: transport.** None.

**Rerun: declined.** 4 sessions answered `unsupported`: `plain` HTTP 001, 003 and 006, and `knowcode-unprompted`
HTTP 001.

**Fifth freeze: format.** No contract failures.

**Fifth freeze: transport.** None.

**Fifth freeze: declined.** 2 sessions answered `unsupported`: `plain` HTTP 001 and 006.

**All runs: refused calls.** Some agents tried `cd …; …` compound commands, which the frozen allow-list doesn't
permit. These are counted as refused calls, as designed. In the fifth-freeze run, they were 10 calls in 8 `plain`
sessions, 7 in 7 `knowcode` sessions and 11 in 11 `knowcode-unprompted` sessions. They affect every arm. When a
refused command was a check of a KnowCode answer, the detector still counts the attempt as a re-check.

**All runs: policy flags.** `tool_progress` stream events were flagged for review: 3 sessions in the baseline, 10
KnowCode-arm sessions in the final run, 3 in the rerun and 5 in the fifth-freeze run (heartbeats of long KnowCode
calls). No tool outside an arm's interface was called. These are
not integrity failures.

## Pilot

The first freeze (`0797df76`) was stopped after 23 sessions:

| Arm | Completed | Format failures |
| --- | ---: | ---: |
| `plain` | 8 | 4 |
| `knowcode` | 7 | 4 |
| `knowcode-unprompted` | 8 | 4 |

Two patterns caused the failures:

- grouped `items` on DI questions (7 sessions), whose text asked for "separate groups";
- prose before the JSON block (5 sessions).

One more session was cut off when the run was stopped. No criterion was computed from the pilot.

The second freeze changed only three things: the answer format (a final fenced JSON block, kinded items allowed), the
brief, and the six DI questions' wording. See "Pilot and correction" in protocol.md.

## Reproducing

The inputs are in [study/](study/), bound by [freeze.json](study/freeze.json). The fifth-freeze run used the fifth
freeze. The rerun's fourth freeze, the final run's third freeze and the baseline's second freeze are in git history. The scorer's output is in
[results/](results/):

- `baseline/summary.json`: the baseline under its frozen strict check (primary), with per-session rows and the
  pre-registered sensitivity view.
- `baseline/adopted-transport-rule.json`: the baseline re-scored under the adopted transport rule.
- `baseline/transport-sensitivity.json`: the baseline counting all 7 retried answers as valid.
- `final/summary.json`: the final run's arm-level totals, paired differences, criteria and pre-registered
  sensitivity view, without per-session rows (see the deviation below).
- `final/transport-sensitivity.json`: the final run counting retried answers as valid, also without per-session rows.
  It was scored without the baseline rate, so its criterion 1 carries the scorer's baseline note and no verdict; the
  rate is identical to the primary.
- `rerun/summary.json`: the rerun's totals, paired differences, criteria, pre-registered sensitivity view and
  stale-index measure, with per-session rows.
- `rerun/transport-sensitivity.json`: the rerun counting retried answers as valid. The rerun had no transport
  failures, so it matches the primary.
- `fifth/summary.json`: the fifth-freeze run's totals, paired differences, criteria, pre-registered sensitivity view
  and stale-index measure, with per-session rows.
- `fifth/transport-sensitivity.json`: the fifth-freeze run counting retried answers as valid. It had no transport
  failures, so it matches the primary.
- `fifth/diagnostics.json`: the descriptive diagnostics above, with per-session values and one row per re-checked
  answer (trigger class and checking calls). It is not scorer output.

**Deviation: the final run's per-session rows are not published.** protocol.md says the committed results include
the per-session rows. For the final run they were left out, because one measured value coincided with a term on the
project's private-vocabulary list, which every public commit is checked against. Every statistic on this page,
and every total and criterion in the two final files, was computed from the full per-session data before the rows
were removed. The baseline and rerun files keep their per-session rows.

Raw transcripts contain build-machine paths, so they aren't published.
