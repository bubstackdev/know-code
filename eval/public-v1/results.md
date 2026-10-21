# Public agent study: results (public-v1)

This page reports the agent study pre-registered in [protocol.md](protocol.md). Every number comes from
[score_public_study.py](../../scripts/eval/score_public_study.py) applied to the frozen inputs. Misses are stated as
misses. The study is small, and it compares KnowCode only with the plain tools tested here (grep, find, read). It
says nothing about other tools, other models or other codebases.

**Release status.** 0.2.0 is not yet released. The final run measured the release candidate at 00aae5b. After its
results were published, a fix round landed, and the study was run once more as a **rerun** on main at b0a3564 (still
version 0.2.0). The rerun is a third measurement, reported next to the others; it does not replace the final run. If
the released 0.2.0 differs from a measured build, these numbers describe the build measured, not the release.

**Summary.**

| Criterion | Final run (00aae5b) | Rerun (b0a3564) |
| --- | --- | --- |
| 1. Re-check rate at least halved | **Missed.** 0.27 [0.16, 0.42]; required ≤ 0.167 | **Missed.** 0.38 [0.25, 0.59]; required ≤ 0.167 |
| 2. Answer F1 and recall at least equal | **Met.** F1 +0.02, recall +0.13 | **Missed.** F1 +0.06, but recall −0.09 [−0.32, +0.03] |
| 3. Fewer research tokens | **Missed.** +28.4k [+19.2k, +81.7k] per session | **Missed.** +7.5k [+1.2k, +17.7k] per session |
| 4. Unprompted use | **Met.** 24 of 24 sessions | **Missed.** 18 of 24 sessions, 0.75 [0.55, 0.88]; required ≥ 20 |

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
- **An earlier rerun attempt was stopped.** The user stopped a rerun on the 702da21 build after 6 of 72 sessions.
  Those sessions are discarded and not scored, and the rerun was started again from scratch on b0a3564.
- **The final run's per-session rows are not published**, a deviation from protocol.md. See
  [Reproducing](#reproducing).

## Runs

| | Baseline | Final | Rerun |
| --- | --- | --- | --- |
| KnowCode | 0.1.1 code (main at 36eec45), sha256 `42c8fd09…` | 0.2.0 candidate (main at 00aae5b), sha256 `f7a15757…` | 0.2.0 (main at b0a3564), sha256 `e55fdc31…` |
| Freeze | second freeze, `9bc06141…` | third freeze, `0141cee5…` | fourth freeze, `6298a569…` |
| Sessions | 72 of 72 | 72 of 72 | 72 of 72 |
| Model and client | `claude-opus-5-5` at effort medium, Claude Code 2.1.304 | same | same |
| Spend (client-reported) | $12.74, plus $2.96 pilot and $0.55 cost pilot | $10.73 | $9.36, plus $0.58 for the stopped attempt |

## Criteria

Intervals are 95%: a cluster bootstrap over questions (10,000 resamples), or a Wilson interval for criterion 4.
"Named" is the `knowcode` arm, whose brief mentions KnowCode. "Unprompted" is the `knowcode-unprompted` arm, which
has the same tools and the plain arm's brief. Criterion 1 compares each run with the baseline's rate.

| Criterion | Baseline (primary, strict) | Baseline, adopted rule | Final | Rerun |
| --- | --- | --- | --- | --- |
| 1. Re-check rate at least halved (named arm, pooled) | 27 of 81, 0.33 [0.18, 0.55] | 0.33 [0.18, 0.55] | 33 of 123, 0.27 [0.16, 0.42]: **missed** | 33 of 87, **0.38** [0.25, 0.59]: **missed** |
| 3. Session tokens (named − plain, paired median, n=24) | +27.6k [+23.0k, +46.2k]: 82.8k against 39.1k | same | +28.4k [+19.2k, +81.7k]: 72.4k against 39.5k: **missed** | **+7.5k** [+1.2k, +17.7k]: 61.9k against 40.5k: **missed** |
| 2a. F1 (named − plain, exhaustive labels, n=10) | +0.23 [0.00, 0.50] | +0.14 [0.00, 0.36] | +0.02 [0.00, 0.07]: met | **+0.06** [0.00, 0.17]: 1.00 against 0.94 |
| 2b. Recall (named − plain, lower-bound labels, n=8) | +0.25 [0.00, 0.60] | 0.00 [0.00, 0.00] | +0.13 [0.00, 0.40]: met | **−0.09** [−0.32, +0.03]: 0.88 against 0.96: **missed** |
| 4. Unprompted use (sessions with a KnowCode call) | 22 of 24, 0.92 [0.74, 0.98] | same | 24 of 24, 1.00 [0.86, 1.00]: met | **18 of 24**, 0.75 [0.55, 0.88]: **missed** |

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
| Baseline counting all 7 retried answers as valid (requested after the run) | 0.33 | +0.14 [0.00, 0.36] | 0.00 | +27.6k | 22 of 24 |
| Final counting retried answers as valid | no session changes, so identical to the primary | | | | |
| Rerun counting retried answers as valid | no transport failures, so identical to the primary | | | | |

## Other measures

| Arm | Run | Answered / error / unsupported | Correct-empty (of 5) | Research calls (mean) | KnowCode calls (mean) | Re-check rate | Fixed overhead (median) |
| --- | --- | --- | --- | ---: | ---: | --- | ---: |
| plain | baseline | 17 / 4 / 3 | 2 | 8.3 | 0 | n/a | 2.5k |
| plain | final | 22 / 1 / 1 | 4 | 6.7 | 0 | n/a | 2.5k |
| plain | rerun | 21 / 0 / 3 | 1 | 8.3 | 0 | n/a | 2.5k |
| knowcode | baseline | 22 / 2 / 0 | 4 | 6.3 | 3.7 | 0.33 [0.18, 0.55] | 10.2k |
| knowcode | final | 24 / 0 / 0 | 5 | 8.0 | 5.5 | 0.27 [0.16, 0.42] | 6.5k |
| knowcode | rerun | 23 / 1 / 0 | 4 | 6.8 | 3.8 | 0.38 [0.25, 0.59] | 5.5k |
| knowcode-unprompted | baseline | 21 / 2 / 1 | 4 | 6.3 | 4.2 | 0.24 [0.12, 0.42] | 10.2k |
| knowcode-unprompted | final | 24 / 0 / 0 | 5 | 7.3 | 4.6 | 0.28 [0.17, 0.43] | 6.5k |
| knowcode-unprompted | rerun | 23 / 0 / 1 | 4 | 5.9 | 3.0 | 0.32 [0.17, 0.53] | 5.5k |

**Stale-index answers** (descriptive, pre-registered before the rerun; not a criterion): successful KnowCode answers
that said the index was still being built.

| Run | Answers | Sessions |
| --- | --- | ---: |
| Baseline | 9 of 186 | 8 |
| Final | 41 of 238 | 33 |
| Rerun | 7 of 162 | 7 |

- No session went over the 40-call research budget in any run.
- Hop assignment agreed with the labels wherever an impact answer found a labelled item.
- The baseline rows use the strict check.
- An `unsupported` answer is not counted as a correct empty answer.

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

**All runs: refused calls.** Some agents tried `cd …; …` compound commands, which the frozen allow-list doesn't
permit. These are counted as refused calls, as designed.

**All runs: policy flags.** `tool_progress` stream events were flagged for review: 3 sessions in the baseline, 10
KnowCode-arm sessions in the final run and 3 in the rerun. No tool outside an arm's interface was called. These are
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

The inputs are in [study/](study/), bound by [freeze.json](study/freeze.json). The rerun used the fourth freeze; the
final run's third freeze and the baseline's second freeze are in git history. The scorer's output is in
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

**Deviation: the final run's per-session rows are not published.** protocol.md says the committed results include
the per-session rows. For the final run they were left out, because one measured value coincided with a term on the
project's private-vocabulary list, which every public commit is checked against. Every statistic on this page,
and every total and criterion in the two final files, was computed from the full per-session data before the rows
were removed. The baseline and rerun files keep their per-session rows.

Raw transcripts contain build-machine paths, so they aren't published.
