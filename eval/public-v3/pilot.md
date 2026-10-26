# public-v3 pilot, closed (descriptive)

**One run of the 0.3.0 release candidate on 20 questions. Descriptive only; no criterion was decided.**

The pilot was meant to size the run count for a pre-registered public-v3 study, from two runs (`p1`, `p2`) of one
build on 20 of the 40 questions (half of each task type, by seed). On October 26, 2026 the user paused it, and then
changed the release gate: releases now gate on deterministic checks with no model cost, and the paid study runs only at
milestones, as one run on the 24-question core tier. So `p2` never ran, and no run count was sized. The study's protocol
stays an unfrozen draft, with the core tier, on the `study-v3-protocol` branch, as the starting point for the next
milestone study.

## What ran

- **Build:** the 0.3.0 release candidate (`knowcode 0.3.0`, sha256 `4ae7e237…`), not the 0.3.0 release, and not a 0.4
  build.
- **Harness:** main at 0697b5f, with all four arms. That harness didn't yet record hook events (#116) or clone the
  source (#118).
- **Sessions:** 62 of `p1`'s 80, all answered, none errored, stopped between batches when paused:

  | Arm | Sessions |
  | --- | ---: |
  | `plain-full` | 16 |
  | `knowcode-full` | 15 |
  | `knowcode-unprompted-full` | 15 |
  | `knowcode-unprompted` (bare) | 16 |

- **Spend:** $9.53 in total. That is $9.38 reported by the client for the 62 sessions (including a 3-session smoke
  batch), plus an estimated $0.15 for one session discarded when parallelism went from 3 to 6. That session's client
  was stopped mid-run, and it was run again from scratch.
- **API:** all 316 relayed responses were 200; there were no rate limits.

## Per arm, over each arm's finished sessions

Tokens are session tokens (every context token each turn processed, plus output). F1 is over exhaustive labels with
items; recall is over every label with items.

| Arm | With a KnowCode call | Tokens, median | F1, mean | Recall, mean | Correct empty | Re-check tokens, mean (median) | Re-checks that changed the answer |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `plain-full` | 0 of 16 | 80,437 | 0.81 (11) | 1.00 | 5 of 5 | — | — |
| `knowcode-full` | 15 of 15 | 75,820 | 0.78 (11) | 1.00 | 2 of 4 | 7,002 (4,286) | 2 |
| `knowcode-unprompted-full` | 11 of 15 | 80,951 | 0.87 (12) | 1.00 | 3 of 3 | 6,855 (1,238) | 3 |
| `knowcode-unprompted` (bare) | 10 of 16 | 37,294 | 0.78 (12) | 1.00 | 4 of 4 | 2,137 (691) | 2 |

**KnowCode calls by tool:**

| Arm | Calls |
| --- | --- |
| named full | `kc_impact` 13, `kc_refs` 5, `kc_search` 4, `kc_find` 2, `kc_context` 2 |
| unprompted full | `kc_impact` 8, `kc_find` 3, `kc_search` 2 |
| bare | `kc_impact` 6, `kc_find` 2, `kc_refs` 2, `kc_context` 2, `kc_search` 2 |

**Named − plain, on the 13 questions both arms finished:**

- **Tokens:** median paired difference −654.
- **F1:** mean paired difference −0.03, over 9 pairs on exhaustive labels with items.
- **Recall:** mean paired difference 0.00, over the same 9.
- **Empty labels:** named got 2 of 4 right, plain 4 of 4.

These are one run of a release candidate on a partial schedule, with no repeat run. They show no effect and decide
nothing.

## Hooks

0.3.0's plugin has one hook, before Grep, Glob and Bash searches. Fail-open timeouts aren't visible in this harness's
streams, so every search call of the 30 finished KnowCode full-arm sessions was replayed through the shipped hook,
offline. Each call ran in the session sandbox, at the shipped 1,500 ms limit and at 60 s:

- **Coverage:** 25 of the 30 sessions made a search call, 81 calls in all.
- **Notes:** 7 calls got a note, the same 7 at both limits.
- **Dropped:** none.
- **Hook time:** median 42 ms, p95 94 ms, max 554 ms, at load averages of about 7 to 21.
