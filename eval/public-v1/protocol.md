# Pre-registered agent study (public-v1)

Frozen October 20, 2026, before any scored session ran, then frozen again the same day after a pilot exposed an
answer-format flaw (see [Pilot and correction](#pilot-and-correction-2026-10-20)). This document, the [study inputs](study/) and the scripts
named below are fixed by [freeze.json](study/freeze.json), and its SHA-256 identifies the study. The general design is
in [method.md](method.md) and the sampling plan in [sampling.md](sampling.md). This page adds what those two leave
open: arms, model, metrics, the four success criteria from the 0.2 plan, the re-check detector, and the scoring
script. A deviation from it is recorded here, dated, before the run it affects. Results go on a separate page.

The study runs twice. The **baseline** ran on the 0.1.1 code (main at 36eec45), under the second freeze
(`9bc06141`). The **final** run uses the 0.2 release candidate, under a third freeze made before it starts. That freeze
differs from the second in two ways only:

- `knowcode_sha256` names the release candidate.
- The transport check follows the rule in [Transport retries](#transport-retries-adopted-2026-10-21), which was adopted
  after the baseline and is applied to both runs.

Questions, labels, arms, model, client, profile, metrics, criteria, detector and intervals are unchanged.

## Cohort and questions

- **Cohort:** the six public repositories pinned in [manifest.json](manifest.json), exported as described in
  method.md: Jellyfin server and web, Bitwarden server and clients (with the separately licensed `bitwarden_license/`
  tree left out), and Autofac with its dependency-injection integration.
- **Questions:** [24 questions](study/questions.jsonl), 6 per task type, drawn by the seeded procedure in
  sampling.md. The rank-ordered review accepted every provisional pick; no candidate was rejected and no replacement
  was drawn ([subject-review.json](study/subject-review.json)). The ranked population (`candidates.jsonl`, 23,887
  candidates) and the source receipt aren't committed because they carry build-machine paths. Both are regenerated
  by the documented prepare, census, scope and sample steps, and freeze.json binds them by hash.

## Labels and their review

Every label was made from pinned source only, before any arm ran and without any tool's output.

- **Primary labels:** 20 were drafted in an earlier source-only pass. The four HTTP questions that pass left
  unresolved were finished in a separate source-only session before this freeze.
- **Independent review:** method.md requires double labels on a seeded subset. The eight subset questions (two per
  task type, seed `knowcode-public-v1-independent-review-2026-10-16`) were labelled again by separate blind sessions
  with no access to the primary labels or their evidence. So were the three other HTTP questions that had been
  unresolved, as a diagnostic.
- **Who did the work:** all labelling, review and adjudication was done by AI agent sessions (Claude Opus 5.5 in
  Claude Code). The primary labeller, the two blind reviewers and the adjudicator were separate sessions. **No human
  has reviewed these labels.** method.md doesn't require a human reviewer, but the results page states this next to
  every correctness number.
- **Agreement** ([review.json](study/review.json)): all 8 seeded questions and all 3 diagnostic questions agree on
  every required item. On impact question 005 the hop assignment agrees for all 16 items. Completeness agreed on 4 of
  the 8 seeded questions. Every completeness disagreement had the reviewer calling a label exhaustive where the
  primary had said lower bound. These were adjudicated as follows:
  - For DI 001 and 002, HTTP 005 and impact 005, both labellers found the same items and the reviewer gave an explicit
    whole-cohort search argument, so these were raised to exhaustive.
  - DI 001: the reviewer also listed three `RegisterModule` forwarding sites as registrations. These were added as
    optional items: naming one is neither required nor a false positive.
  - Same-name negatives and test items are the union of both labels.
- **Final labels** ([labels.jsonl](study/labels.jsonl)):

  | Completeness | Questions |
  | --- | ---: |
  | Exhaustive, with items | 10 |
  | Exhaustive and empty | 5 |
  | Lower bound | 8 |
  | Uncertain | 1 |

  The 16 questions outside the review subset carry only their primary label and say so.

### Questions whose answer is empty

There are five: HTTP 001, 003, 004 and 006, and impact 002.

- Jellyfin HTTP 004 and 006: the web client never calls those routes.
- Autofac impact 002: no production caller exists within two hops.
- Bitwarden HTTP 001 and 003: the empty answer holds for the exported cohort. Their real callers most likely sit in
  the excluded `bitwarden_license/` tree, which no labeller opened. Those labels say so (`corpus_boundary`) and claim
  no count of outside callers.

We considered excluding these questions and resampling, and decided against it. sampling.md, written before any
label existed, says that "a valid method with no callers, an interface with no implementation, a package boundary
... is not a rejection reason", and forbids substituting questions after looking. Dropping questions because their
labelled answer turned out empty would be a post-hoc choice made on label content. So all 24 stay. Two measures keep
the empty answers from distorting results:

- Empty-answer questions are scored apart from F1, as a **correct-empty rate**: an answered status with no items.
- A pre-registered **sensitivity analysis** repeats every criterion without Bitwarden HTTP 001 and 003. Both views
  are published.

## Arms

Each arm runs every question once: 3 × 24 = 72 sessions, in the [frozen order](study/order.jsonl). Each arm's
question order is seeded independently, and the arms rotate.

| Arm | Tools | Brief |
| --- | --- | --- |
| `plain` | Read and Bash (`grep`, `find`, `cat`, `sed`, `head`, `tail`, `wc`, `ls`, `sort`, `uniq`, `xargs`) | Neutral: "use only the granted tools" |
| `knowcode` | The same tools, plus KnowCode's MCP server (profile `all`, `kc_note_add` denied) | Names the KnowCode MCP tools |
| `knowcode-unprompted` | Same as `knowcode` | Word-for-word the `plain` brief; KnowCode isn't mentioned |

Criteria 1–3 compare `knowcode` with `plain`, and criterion 4 is measured in `knowcode-unprompted`. Every arm also
reports every metric.

**Common configuration** ([config.json](study/config.json)):

- Model and effort: `claude-opus-5-5`, effort `medium`.
- Client: Claude Code 2.1.304 in bare print mode, as a pinned copy whose hash is in freeze.json.
- Soft research budget: 40 calls. It is stated in the brief and overruns are reported; the client can't enforce it.
- Operational timeout: 1,800 s per session.
- KnowCode binary: built from main at 36eec45 (the 0.1.1 code; it reports version `0.1.0`). Its hash is in
  config.json.
- Index preparation runs before the client starts, outside the research interval.

**Answer contract.** The brief asks the agent to end its reply with one JSON object in a ```` ```json ```` block,
with `status` (answered, unsupported or error), `items` and `notes`. Prose before the block is allowed.

- `items` is one flat list. Each entry is a `repo/path[:line]` location, or `{"location": ..., "kind": ...}` where
  the question asks for kinds. The DI questions ask for kind `declaration` or `registration`.
- Groupings and assumptions go in `notes`.
- Impact answers also give `hops`.

The parser in [public_transcript.py](../../scripts/eval/public_transcript.py) accepts the whole reply when it is one
object, or else the last fenced ```` ```json ```` block. Nothing else is extracted or repaired. Grouped `items`, a
missing or broken block, and an unknown status are contract failures, and the session is scored as an error. Scoring
uses only the locations. Kinds are recorded but not scored.

The runner is [session_runner.py](../../scripts/eval/session_runner.py), with the isolation in method.md:

- a fresh source copy for every session;
- a macOS sandbox with no network except the owned model relay;
- an isolated client profile, with settings discovery, skills and session persistence off;
- the API key held only by the relay;
- KnowCode's cache, config and data inside the session.

The bare client doesn't load the Claude Code plugin, so neither the Bash hook nor the skills are part of either
KnowCode arm. The study measures KnowCode through MCP only.

No wall-clock time is measured or compared. Token, call and cost counts are.

## Metrics

Per session, from the runner's `result.json` and transcript:

- **Answer correctness**, using [score.py](../../scripts/eval/neutral/score.py) against the frozen labels:
  - per-question F1 on the 10 exhaustive labels that have items;
  - recall on the 8 lower-bound labels;
  - correct-empty on the 5 empty labels;
  - recall on the uncertain label, reported apart;
  - hop agreement on impact questions, reported apart.

  An errored, refused or missing session counts as F1 0 and recall 0 and stays in every denominator.
- **Session tokens**, the research tokens with schemas included: the sum, over the model's turns, of every context
  token it processed (input, cache creation and cache reads), plus output tokens. Tool schemas and the system prompt
  are in every turn's context, so their cost is counted on every turn. Also reported: the first turn's context
  (fixed overhead, mostly schemas), context at answer, output tokens and the client's reported cost.
- **Calls:** research calls, KnowCode calls and budget overruns.
- **Re-checks:** see the detector below, applied to both KnowCode arms.

## Criteria

These are the four success criteria from the 0.2 plan, made operational. A criterion that is missed is reported as
missed.

1. **Re-check rate at least halved.** The pooled re-check rate in the `knowcode` arm of the final run is at most half
   the baseline run's. The pooled rate is the number of re-checked KnowCode answers over all KnowCode answers that
   make a claim.
2. **Answer F1 at least equal.** In the final run, the mean paired difference `knowcode − plain` in F1 (exhaustive
   labels with items) is ≥ 0, **and** the mean paired difference in recall (lower-bound labels) is ≥ 0.
3. **Fewer research tokens.** In the final run, the median paired difference `knowcode − plain` in session tokens is
   < 0.
4. **Unprompted use.** In the final run's `knowcode-unprompted` arm, KnowCode is called in at least 20 of the 24
   sessions. Every task type in the study is one KnowCode has tools for. The 0.1.0 clean-machine figure is for
   context only and isn't a threshold: zero calls before the hook fix, one call per question after.

Each criterion is evaluated on point estimates, and every number is published with a 95% interval:

- a percentile cluster bootstrap over questions: 10,000 resamples, seed `knowcode-public-v1-bootstrap-2026-10-20`;
- for criterion 4, a Wilson interval.

The baseline run evaluates criteria 2–4 for 0.1.1 and sets the reference rate for criterion 1.

**Expected precision** (planning estimates, before any data):

- With 10 F1 questions, a typical per-question paired spread of about 0.3 gives a 95% interval of about ±0.2 on the
  mean F1 difference. The 8 lower-bound recall questions give about the same.
- 20 of 24 unprompted sessions has a Wilson interval of 0.64–0.93.
- The re-check rate pools roughly 100–150 KnowCode answers but is resampled by question, so expect about ±0.1.

Only large effects are distinguishable at this size. Criteria 2 and 3 can be met by chance when the true difference
is near zero. The results page says so instead of treating a point estimate as a finding.

## Re-check detector

[recheck.py](../../scripts/eval/recheck.py) applies the rule below to each transcript. It counts coverage, not
intent.

- **A KnowCode answer** is the result of one successful `mcp__knowcode__*` call.
- **Its claims** are the source locations it names (`file` and `line` fields, or `path:line` text with a known
  source extension) and the symbols it names. Symbols come from `name`, `qualified`, `symbol` and `container` fields,
  from `kind Name` in tree answers, and from the symbol, query or pattern the call asked about. Answers with no claim,
  and failed calls, are counted apart.
- **A later call in the same session re-checks the answer** when it does one of these:
  - **Read:** it reads a claimed file with `Read`, `cat`, `head`, `tail`, `sed -n 'a,bp'`, `awk`, `less` or `more`,
    and the range read includes a claimed line of that file, within 3 lines. A whole-file read counts. A claim with
    no line counts only when the answer gives no line for that file at all. Paths match when one ends with the
    other.
  - **Search:** it searches for a claimed symbol, or inside a claimed file. That is `grep`, `egrep`, `fgrep`, `rg`,
    `ag` or `ack` where an identifier of the pattern equals a claimed symbol of 4 or more characters (a short list of
    keywords is ignored), a search whose path operand is a claimed file, or `find -name`, `-path` or `-iname` matching
    a claimed file name or containing a claimed symbol.
  - **Re-ask:** it calls the same KnowCode tool again with the same arguments.

The published per-session sequences give, for each answer, the tool, whether it was re-checked and the kinds of
call that re-checked it. Not every read of a claimed file is unnecessary, and the results page doesn't say it is.

**Validation against hand labels.** A separate session wrote synthetic transcripts and labelled each KnowCode answer
from a plain-language guide, without reading the detector. That guide is the rule above, in prose.

| Set | Scored answers | Re-checked | Precision | Recall |
| --- | ---: | ---: | ---: | ---: |
| Development, first version | 57 | 31 | 0.91 | 0.97 |
| Development, after two fixes | 57 | 31 | 1.00 | 1.00 |
| Held out (never used to change the rule) | 34 | 18 | 0.84 | 0.89 |
| Held out, without the 5 cases marked ambiguous | 29 | 15 | 0.94 | 1.00 |

The development set is in [recheck_fixtures/cases](../../scripts/eval/recheck_fixtures/), with 50 transcripts. It
exposed two bugs:

- A file named both with and without a line was treated as wholly claimed.
- A `kc_search` pattern wasn't counted as a claimed symbol.

Both were fixed. One synthetic symbol name was later renamed throughout the fixtures, by plain text replacement;
the labels and the results are unchanged. The held-out set was labelled after the fixes and never used to change the
rule. Its numbers are the
ones to quote. Its one unambiguous error is a read of the same relative path in a different repository, because the
detector matches paths by suffix and ignores the repository. Its ambiguous cases are:

- a regex grep with no whole identifier;
- a read 4 lines from a stated line;
- `tail` of a long file;
- `find` for a generic file name;
- a case-insensitive grep.

## Scoring script

[score_public_study.py](../../scripts/eval/score_public_study.py) runs as:

```sh
score_public_study.py --freeze study/freeze.json --runs RUN_DIR [--baseline BASELINE_SUMMARY] --out summary.json
```

It checks the frozen inputs' hashes and scores every scheduled session. Missing sessions are scored as errors. It
writes per-session rows and the per-arm aggregates, paired differences, intervals and criteria, plus the sensitivity
view. The summary contains no local paths.

Publication follows method.md. The committed results are the summary and per-session rows. Raw transcripts are kept
outside the repository, because they contain the build machine's paths. They are available on request, with those
paths removed.

## Transport retries (adopted 2026-10-21)

The baseline found a case the protocol had no rule for. In 7 of its 72 sessions, a model response stream between
the client and the owned relay broke mid-response: a broken pipe, a connection reset, or no status. In each case the
client retried and finished normally, with an answer and with clean transcript integrity. The strict transport check
rejected the session anyway, because the abandoned attempt has no matching client message, and it scored the session
as an error.

| Arm | Sessions affected |
| --- | ---: |
| `plain` | 3 |
| `knowcode` | 2 |
| `knowcode-unprompted` | 2 |

The rule below was proposed after the baseline (recorded first in deviations.md) and adopted for the final run. It
is implemented in [public_transcript.py](../../scripts/eval/public_transcript.py):

- **What counts as an abandoned attempt.** A model response that is incomplete, or has no valid status, and whose
  message id (if any) belongs to no client message. It is reported with its count and does not fail the transport
  check.
- **What still fails the check:**
  - any client message without a matching complete, successful provider response, including one the client took
    from a partial response;
  - responses that are reordered or repeated;
  - a model that differs from the configuration, on any attempt;
  - a permission mode that differs;
  - relay cleanup that isn't verified.

The baseline's results are published twice:

- under its frozen strict check (the primary baseline numbers);
- re-scored under this rule, which makes it comparable with the final run.

No baseline session was rerun.

## Pilot and correction (2026-10-20)

The first freeze's baseline was stopped after 23 of its 72 sessions. In 12 of them the agent's answer failed the
answer contract, and the failures were spread evenly over the arms: 4 in `plain`, 4 in `knowcode` and 4 in
`knowcode-unprompted`. There were two patterns:

- **Grouped `items` (7 sessions, all DI questions).** The DI questions asked for declarations and registrations "in
  separate groups", while the brief required one flat list, so agents returned an object of groups.
- **Prose before the JSON (5 sessions).** The old parser accepted only a whole-reply object.

Every failed session exited normally, with clean transcript and transport integrity: these were format failures,
not infrastructure failures. One more session was cut off by the operator's stop and has no result.

With the user's approval, the 23 completed sessions and the cut-off one are declared a **pilot**. No criterion,
correctness score or token comparison was computed or inspected from them, and none will be reported. Their raw
transcripts are kept outside the repository.

What changed for the second freeze:

1. **Parser.** It accepts prose before the last fenced ```` ```json ```` block, and `{"location", "kind"}` entries
   in `items`. Tests cover both failure shapes, using synthetic fixtures.
2. **Brief.** It now states that format, and says groupings belong in `notes`.
3. **DI questions.** The six questions now ask for each declaration and registration site "as one item with its
   file:line location and kind", with the implementation and registration distinctions going in `notes`. Subjects,
   ids and what is asked are unchanged. `review.json` records both question hashes.

Nothing else changed: labels, arms, model, client, binary, metrics, criteria, detector and intervals are all as
before. The full baseline runs again from scratch under the second freeze, and the pilot's spend counts toward the
baseline budget.

## Deviations from method.md and sampling.md

- **2026-10-20:** method.md describes two paired arms. A third arm, `knowcode-unprompted`, was added before any scored
  run to measure criterion 4 without changing the other two. The plain arm's brief changed from "use only the granted
  plain tools" to "use only the granted tools", so that the unprompted arm can use word-for-word the same brief.
- **2026-10-20:** labels, review and adjudication were done by AI agent sessions, as described above.
- **2026-10-20:** the answer contract was corrected after a pilot, as described in "Pilot and correction". The six DI
  questions were reworded for answer format only, after their labels and independent review.
- **2026-10-21:** after the baseline, a rule for transport retries was adopted for the final run. See "Transport
  retries"; the baseline is also re-scored under it.
