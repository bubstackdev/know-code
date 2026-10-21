# Public agent study: results (public-v1)

This page reports the agent study pre-registered in [protocol.md](protocol.md). Every number comes from
[score_public_study.py](../../scripts/eval/score_public_study.py) applied to the frozen inputs. Misses are stated as
misses. The study is small, and it compares KnowCode only with the plain tools tested here (grep, find, read). It
says nothing about other tools, other models or other codebases.

**Release status.** 0.2.0 is not yet released. The final run measured the release candidate: the build of main at
commit 00aae5b, sha256 `f7a15757…`. If the released 0.2.0 differs from that build, these numbers describe the build
measured, not the release.

**Summary.** The candidate missed 2 of the 4 pre-registered criteria and met 2:

| Criterion | Final run (0.2.0 candidate) |
| --- | --- |
| 1. Re-check rate at least halved | **Missed.** 0.27 [0.16, 0.42] against a required ≤ 0.167; the baseline was 0.33. |
| 3. Fewer research tokens | **Missed.** Paired median +28.4k [+19.2k, +81.7k] tokens per session for the named arm over the plain arm (72.4k against 39.5k). |
| 2. Answer F1 and recall at least equal | **Met.** F1 +0.02, recall +0.13. Both arms are at or near the ceiling. |
| 4. Unprompted use | **Met.** 24 of 24 unprompted sessions called KnowCode. |

## Disclosures first

- **Labels were made by AI, and no human reviewed them.** Every label, the independent blind review and the
  adjudication were done by separate AI agent sessions (Claude Opus 5.5). On the 8 seeded double-labelled questions,
  required items agreed on 8 of 8 and completeness on 4 of 8 ([review.json](study/review.json)). Correctness numbers
  rest on these labels.
- **The study is small.** F1 rests on 10 questions, lower-bound recall on 8 and correct-empty on 5, so the intervals
  are wide. A criterion that is "met" on a point estimate can be met by chance.
- **Correctness is near the ceiling in both runs.** In the final run, both KnowCode arms scored 1.00 on F1 and recall,
  and so did the plain arm on most questions. The study can't detect correctness differences smaller than those few
  questions allow.
- **Agent behavior is not tool correctness.** These numbers describe what one model did with each arm's tools.
  KnowCode's own answer accuracy is reported separately, in the accuracy bank.
- **There was a pilot.** The first freeze's run was stopped after 23 of 72 sessions because 12 answers failed the
  answer format, 4 in each arm. Those sessions were declared a pilot, and no criterion was computed from them. See
  [Pilot](#pilot).
- **There was a rule gap.** After the baseline, a rule for broken relay streams was adopted (see "Transport retries"
  in protocol.md). The final run uses it. The baseline is shown both under its frozen strict check (primary) and
  under the adopted rule.
- **The final run's per-session rows are not published**, a deviation from protocol.md. See
  [Reproducing](#reproducing).

## Runs

| | Baseline | Final |
| --- | --- | --- |
| KnowCode | 0.1.1 code (main at 36eec45), sha256 `42c8fd09…` | 0.2.0 candidate (main at 00aae5b), sha256 `f7a15757…` |
| Freeze | second freeze, `9bc06141…` | third freeze, `0141cee5…` |
| Sessions | 72 of 72 | 72 of 72 |
| Model and client | `claude-opus-5-5` at effort medium, Claude Code 2.1.304 | same |
| Spend (client-reported) | $12.74, plus $2.96 pilot and $0.55 cost pilot | $10.73 |

## Criteria

Intervals are 95%: a cluster bootstrap over questions (10,000 resamples), or a Wilson interval for criterion 4.
"Named" is the `knowcode` arm, whose brief mentions KnowCode. "Unprompted" is the `knowcode-unprompted` arm, which
has the same tools and the plain arm's brief.

| Criterion | Baseline (primary, strict) | Baseline, adopted rule | Final | Result |
| --- | --- | --- | --- | --- |
| 1. Re-check rate at least halved (named arm, pooled) | 27 of 81 answers, 0.33 [0.18, 0.55] | 0.33 [0.18, 0.55] | 33 of 123 answers, **0.27** [0.16, 0.42]; required ≤ 0.167 | **Missed** |
| 3. Session tokens (named − plain, paired median, n=24) | +27.6k [+23.0k, +46.2k]: 82.8k against 39.1k | same | **+28.4k** [+19.2k, +81.7k]: 72.4k against 39.5k | **Missed** |
| 2a. F1 (named − plain, exhaustive labels, n=10) | +0.23 [0.00, 0.50] | +0.14 [0.00, 0.36] | **+0.02** [0.00, 0.07]: 1.00 against 0.98 | **Met** (with 2b) |
| 2b. Recall (named − plain, lower-bound labels, n=8) | +0.25 [0.00, 0.60] | 0.00 [0.00, 0.00] | **+0.13** [0.00, 0.40]: 1.00 against 0.88 | **Met** (with 2a) |
| 4. Unprompted use (sessions with a KnowCode call) | 22 of 24, 0.92 [0.74, 0.98] | same | **24 of 24**, 1.00 [0.86, 1.00]; 4.6 calls per session | **Met** |

What the final numbers show:

- **Re-checks fell, but not by half.** The named arm's re-check rate went from 0.33 to 0.27, and the intervals
  overlap heavily. The agent also made more KnowCode calls per session (5.5 against 3.7), so more answers were made
  and more were re-checked in absolute terms (33 against 27).
- **Fixed overhead fell; the token gap didn't.** The tool-schema overhead per turn dropped from 10.2k to 6.5k tokens.
  The named arm's median session total fell from 82.8k to 72.4k. The plain arm stayed at about 39.5k, so the paired
  gap is unchanged at about +28k. KnowCode sessions still processed more tokens than plain ones.
- **Correctness is level, at the ceiling.** Both KnowCode arms answered every question and scored 1.00 on F1, on
  lower-bound recall and on correct-empty (5 of 5). The plain arm scored 0.98 F1 and 0.88 recall, got 4 of 5
  empty-answer questions right, and had one transport error.
- **Unprompted use is complete.** Every unprompted session called KnowCode, at 4.6 calls per session.

## Sensitivity views

The pre-registered view leaves out Bitwarden HTTP questions 001 and 003, whose labels depend on the separately
licensed tree left out of the export.

| View | 1. Re-check rate | 2a. F1 diff | 2b. Recall diff | 3. Tokens diff | 4. Unprompted use |
| --- | --- | --- | --- | --- | --- |
| Baseline without Bitwarden HTTP 001 and 003 (pre-registered) | 0.45 [0.28, 0.65], n=60 | +0.23 [0.00, 0.50] | +0.25 [0.00, 0.60] | +27.6k [+23.2k, +46.2k], n=22 | 21 of 22, 0.95 [0.78, 0.99] |
| Final without Bitwarden HTTP 001 and 003 (pre-registered) | 0.35 [0.24, 0.51], n=94; required ≤ 0.225: **missed** | +0.02 [0.00, 0.07]: met | +0.13 [0.00, 0.40]: met | +25.4k [+17.5k, +67.5k], n=22: **missed** | 22 of 22, 1.00 [0.85, 1.00]: met |
| Baseline counting all 7 retried answers as valid (requested after the run) | 0.33 | +0.14 [0.00, 0.36] | 0.00 | +27.6k | 22 of 24 |
| Final counting retried answers as valid | no session changes, so identical to the primary | | | | |

- Under the adopted rule, 6 of the baseline's 7 retried sessions count as valid. The 7th (named arm, HTTP 004) still
  fails, because the client's message came from a response the relay recorded as cut off.
- In the final run, one plain session (DI 005) failed the same way, with an IncompleteRead on a response the client
  used. It counts as an error under every view.

## Other measures

| Arm | Run | Answered / error / unsupported | Correct-empty (of 5) | Research calls (mean) | KnowCode calls (mean) | Re-check rate | Fixed overhead (median) |
| --- | --- | --- | --- | ---: | ---: | --- | ---: |
| plain | baseline | 17 / 4 / 3 | 2 | 8.3 | 0 | n/a | 2.5k |
| plain | final | 22 / 1 / 1 | 4 | 6.7 | 0 | n/a | 2.5k |
| knowcode | baseline | 22 / 2 / 0 | 4 | 6.3 | 3.7 | 0.33 [0.18, 0.55] | 10.2k |
| knowcode | final | 24 / 0 / 0 | 5 | 8.0 | 5.5 | 0.27 [0.16, 0.42] | 6.5k |
| knowcode-unprompted | baseline | 21 / 2 / 1 | 4 | 6.3 | 4.2 | 0.24 [0.12, 0.42] | 10.2k |
| knowcode-unprompted | final | 24 / 0 / 0 | 5 | 7.3 | 4.6 | 0.28 [0.17, 0.43] | 6.5k |

- No session went over the 40-call research budget in either run.
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

**Both runs: refused calls.** Some agents tried `cd …; …` compound commands, which the frozen allow-list doesn't
permit. These are counted as refused calls, as designed.

**Both runs: policy flags.** `tool_progress` stream events were flagged for review: 3 sessions in the baseline and
10 KnowCode-arm sessions in the final run. No tool outside an arm's interface was called. These are not integrity
failures.

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

The inputs are in [study/](study/), bound by [freeze.json](study/freeze.json). The final run used the third freeze;
the baseline's second freeze is in git history at 8cf2882. The scorer's output is in [results/](results/):

- `baseline/summary.json`: the baseline under its frozen strict check (primary), with per-session rows and the
  pre-registered sensitivity view.
- `baseline/adopted-transport-rule.json`: the baseline re-scored under the adopted transport rule.
- `baseline/transport-sensitivity.json`: the baseline counting all 7 retried answers as valid.
- `final/summary.json`: the final run's arm-level totals, paired differences, criteria and pre-registered
  sensitivity view, without per-session rows (see the deviation below).
- `final/transport-sensitivity.json`: the final run counting retried answers as valid, also without per-session rows.
  It was scored without the baseline rate, so its criterion 1 carries the scorer's baseline note and no verdict; the
  rate is identical to the primary.

**Deviation: the final run's per-session rows are not published.** protocol.md says the committed results include
the per-session rows. For the final run they were left out, because one measured value coincided with a term on the
project's private-vocabulary list, which every public commit is checked against. Every statistic on this page,
and every total and criterion in the two final files, was computed from the full per-session data before the rows
were removed. The baseline files keep their per-session rows.

Raw transcripts contain build-machine paths, so they aren't published.
