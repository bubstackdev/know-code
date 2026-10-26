# Public agent study: results (public-v2)

This page reports the agent study pre-registered in [protocol.md](protocol.md). It compares the 0.3.0 release
candidate with the 0.2.0 release on the public-v2 bank: 40 questions over ten public repositories, three arms, and two
runs per build. Every number comes from [public_v2.py](../../scripts/eval/public_v2.py), applied to the frozen inputs.
Misses come first and are stated as misses.

The study compares KnowCode only with the plain tools tested here (grep, find, read). It says nothing about other
tools, other models or other codebases.

## Summary: the release candidate missed all four decided criteria

| Criterion | 0.2.0 baseline | 0.3.0 release candidate | Result |
| --- | --- | --- | --- |
| 1. Re-check cost halved (mean re-check tokens per named-arm session) | 4,473 [2,201, 7,556] | 6,328 [2,401, 12,841]; 1.41× the baseline, required ≤ 0.5× | **Missed** |
| 3. F1 and recall at least equal (named − plain) | F1 +0.012, recall −0.333 | F1 +0.035 [−0.022, +0.100]; recall −0.167 | **Missed** (recall) |
| 4. Research tokens at most equal (median paired, named − plain) | +3.4k [+1.7k, +5.3k] | +3.5k [+1.5k, +4.6k] | **Missed** |
| 5. Unprompted use (sessions with a KnowCode call; required ≥ 67 of 80) | 44 of 80, 0.55 [0.44, 0.65] | 53 of 80, 0.66 [0.55, 0.76] | **Missed** |
| 2. Re-checks that changed the answer (descriptive; each is a KnowCode failure) | named 11, unprompted 6 | named 7, unprompted 4 | Listed below |

The baseline sets criterion 1's reference value; it isn't decided for 0.2.0. Measured against the protocol's own
thresholds, 0.2.0 also misses criteria 3, 4 and 5.

- **Re-check cost rose instead of halving.** Named-arm sessions spent more tokens re-checking KnowCode's answers than
  under 0.2.0: 1.17× in run b1 and 1.62× in run b2. The interval is wide, but neither run comes near a halving.
- **Correctness is about level.** F1 is a little higher than plain in both builds. The recall part rests on a single
  lower-bound question, and both builds miss it there (see "Disclosures first").
- **Tokens are unchanged.** Named sessions still process about 3.5k more tokens per session than plain, in both builds.
- **Unprompted use rose.** It went from 44 to 53 of 80 sessions, mostly on HTTP and impact questions, but it stays
  below the 67 required.

## Why criterion 1 rose (descriptive, not a criterion)

These are named-arm sessions only, and both runs pooled. The numbers describe where the re-check tokens were spent; they
don't establish a cause.

**One question carries much of it.**

| | 0.2.0 | RC |
| --- | ---: | ---: |
| Mean re-check tokens per session | 4,473 | 6,328 |
| Median re-check tokens per session | 1,251 | 1,437 |
| Sessions with any re-check tokens | 54 of 80 | 63 of 80 |
| Share of all re-check tokens in the top 5 sessions | 48% | 60% |
| two-hop-impact-007, run b1 | 48,418 | 72,511 |
| two-hop-impact-007, run b2 | 52,306 | 155,741 |
| Mean without two-hop-impact-007 (78 sessions) | 3,296 | 3,564 |

- In the RC, the single session of run b2 on two-hop-impact-007 is 31% of all named-arm re-check tokens. Without that
  question, the RC is 1.08× the baseline instead of 1.41×.
- Two-hop impact questions carry 69% of the RC's re-check tokens, against 50% for 0.2.0.
- HTTP questions fell from 28% to 11%.

**More calls, and more tokens per call.**

| | 0.2.0 | RC |
| --- | ---: | ---: |
| KnowCode calls | 99 | 121 |
| Answers re-checked | 65 | 83 |
| Re-checking calls | 97 | 107 |
| Re-checking calls per session | 1.21 | 1.34 |
| Re-check tokens per KnowCode call | 3,615 | 4,184 |
| Re-check tokens per re-checking call | 3,689 | 4,732 |

Both rose: more KnowCode answers were re-checked, and each re-checking call cost more.

**Re-check classes.** These are the 0.2 diagnosis classes, applied by its rule-based classifier (AI-made, hand-checked
on samples only). The count is re-checked answers; the tokens are summed per answer, so a call that re-checks two
answers counts for each.

| Class | 0.2.0 answers | RC answers | 0.2.0 tokens | RC tokens |
| --- | ---: | ---: | ---: | ---: |
| A complete answer verified anyway | 21 | 36 | 137,794 | 325,121 |
| Reading source around a claimed line | 4 | 17 | 5,427 | 55,782 |
| Same model turn as the answer | 1 | 9 | 918 | 22,473 |
| The answer said it was cut or incomplete | 31 | 16 | 216,091 | 111,010 |
| A search hit searched again | 4 | 4 | 57,978 | 55,277 |
| Empty or mismatched answer | 4 | 1 | 52,747 | 14,609 |

- Re-checks after answers that said they were cut or incomplete halved.
- Re-checks of complete answers, and reads around a claimed line, grew.

## Disclosures first

- **Labels were made by AI, and no human reviewed them.** Separate AI agent sessions (Claude Opus 5.5) labelled the
  bank. A seeded subset of 8 was labelled again blind, and it agreed on every required item, completeness and hop
  ([review.json](study/review.json)). Correctness numbers rest on these labels.
- **Criterion 3's recall rests on one question.** The bank has one lower-bound label, `public2-two-hop-impact-006`.
  Its recall, per arm and run:

  | Arm | 0.2.0 b1 | 0.2.0 b2 | RC b1 | RC b2 |
  | --- | ---: | ---: | ---: | ---: |
  | plain | 1.00 | 1.00 | 0.33 | 0.33 |
  | knowcode | 1.00 | 0.33 | 0.00 | 0.33 |
  | knowcode-unprompted | 1.00 | 1.00 | 1.00 | 0.33 |

- **The study client doesn't pass on MCP server instructions.** In bare mode, the model sees KnowCode's tool names,
  descriptions and schemas, but not the server's instructions. Unprompted use measured here is therefore a lower bound
  of what an instructions-receiving client would see.
- **The bank is small.** Each build has 80 sessions per arm. A repeated run moved criterion 1 by 6% in the pilot and by
  19% between the baseline's two runs. That is well within what criterion 1 is built to tell apart, but the intervals
  are wide.
- **There was a pilot, and a discarded attempt.** The pilot ran 0.2.0 twice on the bank before the protocol was fixed
  ($24.58). Its numbers chose two runs per build and aren't part of these results. A first pilot attempt was stopped
  after 42 sessions ($4.88), because a deny rule refused Bash commands naming source paths. It was discarded. See
  "Runs and order" in protocol.md.
- **Agent behaviour is not tool correctness.** These numbers describe what one model did with each arm's tools.

## Runs

| | 0.2.0 baseline | 0.3.0 release candidate |
| --- | --- | --- |
| KnowCode | 0.2.0, built from the v0.2.0 tag (2402604), sha256 `9a5d773c…` | 0.3.0, cargo-dist build of `release/0.3.0` at dd67050, sha256 `4ae7e237…` |
| Freeze | `1ba58b1b…` at e71ad46 | second freeze, `4cf2dfef…` at d80cfce |
| Sessions | 240 of 240 (b1, b2) | 240 of 240 (b1, b2) |
| Model and client | `claude-opus-5-5` at effort medium, Claude Code 2.1.304 bare, profile `core`, sandbox shell | same |
| Spend (client-reported) | $23.80 | $23.71 |

0.3's study spend was $94.71 in total:

| Item | Spend |
| --- | ---: |
| Discarded attempt | $4.88 |
| Pilot | $24.58 |
| Baseline | $23.80 |
| Release candidate | $23.71 |
| Development runs outside the study | $17.74 |

## Criteria per run

| Criterion | 0.2.0 b1 | 0.2.0 b2 | RC b1 | RC b2 |
| --- | ---: | ---: | ---: | ---: |
| 1. Re-check tokens per named session | 4,043 | 4,903 | 4,733 | 7,924 |
| 2. Changed answers (named, unprompted) | 5, 6 | 6, 0 | 3, 4 | 4, 0 |
| 3. F1 difference | +0.016 | +0.007 | +0.051 | +0.019 |
| 3. Recall difference (one question) | 0.000 | −0.667 | −0.333 | 0.000 |
| 4. Session tokens difference | +3,779 | +3,264 | +1,949 | +6,198 |
| 5. Unprompted sessions with a call | 24 of 40 | 20 of 40 | 26 of 40 | 27 of 40 |

Criteria are decided on the two runs pooled ([Analysis](protocol.md#analysis)). The per-run values are descriptive.

## Other measures

Medians unless noted:

| Arm | Build | Answered / error / unsupported | Session tokens | Fixed overhead | KnowCode calls (mean) | Re-check rate (public-v1 detector) |
| --- | --- | --- | ---: | ---: | ---: | ---: |
| plain | 0.2.0 | 77 / 1 / 2 | 27.4k | 3.1k | 0 | n/a |
| plain | RC | 75 / 2 / 3 | 26.9k | 3.1k | 0 | n/a |
| knowcode | 0.2.0 | 77 / 1 / 2 | 29.3k | 4.3k | 1.24 | 0.66 |
| knowcode | RC | 80 / 0 / 0 | 28.7k | 4.1k | 1.51 | 0.69 |
| knowcode-unprompted | 0.2.0 | 78 / 0 / 2 | 31.4k | 4.3k | 0.89 | 0.77 |
| knowcode-unprompted | RC | 80 / 0 / 0 | 30.2k | 4.2k | 0.90 | 0.76 |

KnowCode use by task type, as sessions with a KnowCode call out of 20:

| Task type | Named, 0.2.0 | Named, RC | Unprompted, 0.2.0 | Unprompted, RC |
| --- | ---: | ---: | ---: | ---: |
| Definition lookup | 20 | 20 | 16 | 14 |
| DI implementations | 20 | 20 | 19 | 19 |
| HTTP cross-repo | 17 | 19 | 3 | 10 |
| Two-hop impact | 13 | 20 | 6 | 10 |

## Every failure

**Re-checks that changed the answer (criterion 2).** Each is a KnowCode failure: a re-check found a location no
KnowCode answer in the session had named, and the final answer kept it.

- **Release candidate, 11 calls:**
  - two-hop-impact-007: 6 calls in three sessions (named in both runs, unprompted in b1), adding
    `jellyfin-server/MediaBrowser.Providers/Manager/ProviderManager.cs`;
  - http-cross-repo-010: 2 calls, adding `jellyfin-web/.../SplitVersionsButton.tsx`;
  - http-cross-repo-007, http-cross-repo-009 and two-hop-impact-009: 1 call each.
- **0.2.0, 17 calls:** these fall on http-cross-repo-007, 008 and 010, and two-hop-impact-001, 005 and 007.

The calls and the locations they added are listed per session in the summaries' `2_rechecks_that_changed_the_answer`.

**Format.** Four sessions failed the answer contract ("not one complete JSON object"), with transport clean. They are
scored as errors:

- 0.2.0: b2 `plain` DI-006, and b2 `knowcode` DI-010;
- RC: b1 `plain` DI-006 and DI-008.

**Transport.** None.

**Refused Bash calls.** None in any arm, in either build: no command naming the KnowCode binary and no other. The
runner's tripwires never fired.

## Reproducing

The inputs are in [study/](study/), bound by [freeze.json](study/freeze.json):

- **Release candidate:** ran from this freeze (the second).
- **Baseline:** ran from the first freeze, `1ba58b1b…`, at e71ad46, in git history.

The scorer's output is in [results/](results/), each file with per-session rows:

- `baseline/summary.json`: the 0.2.0 baseline pooled over b1 and b2, scored without a baseline. It records
  criterion 1's reference value.
- `rc/summary.json`: the release candidate pooled, scored against `baseline/summary.json`.
- `*/run-b1.json` and `*/run-b2.json`: each run alone. The RC's per-run files are scored against the baseline's
  matching run.

Raw transcripts aren't published, because they contain build-machine paths.
