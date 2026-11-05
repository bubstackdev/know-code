# impact-dev-v1: a dev set for KnowCode's impact walk

Impact questions to measure and tune KnowCode's `impact` on, kept apart from compare-v1's gate. compare-v1's 96
impact questions are frozen gate questions ([compare-v1](../compare-v1/README.md)), and none of them is drawn here:
a change to the impact walk is measured and justified on this set alone, and compare-v1 then checks it.

## The draw

[draw.py](draw.py) applies compare-v1's impact rules ([questions.py](../../scripts/eval/compare/questions.py),
"The draw" in compare-v1's README) to the same enumerations, compare-v1's, whose digests are in
[SHA256SUMS](../compare-v1/enumerations/SHA256SUMS). Targets are callables with at least one caller, stratified by
the number of hop-1 callers (`1`, `2-9`, `10+`), up to 4 per stratum and repository. Labels are compare-v1's: hop 1
is the target's callers, hop 2 their callers, by the toolchain's bindings. Two things differ:

- Within a stratum, targets are taken in SHA-256 order of `impact-dev-v1:repo:key`; the seed is `impact-dev-v1`.
- No declaration that is the target of any compare-v1 question (322 keys) is drawn.

```sh
python3 eval/impact-dev-v1/draw.py ENUMERATIONS/enumeration-*.jsonl.gz \
  --out eval/impact-dev-v1/questions.jsonl --strata eval/impact-dev-v1/strata.json
```

`questions.jsonl` isn't checked in, as the enumerations aren't: the draw is deterministic, and
[questions.sha256](questions.sha256) holds its digest, so a run checks that it asks exactly the committed list.

The draw gives 92 questions: 12 in each repository but hono, which has 8, because compare-v1 took all four of hono's
callables with 10 or more callers. [strata.json](strata.json) holds, per stratum, how many targets were available and
how many were drawn.

## What it is for

The list (its digest) was committed before any tool answered a question from it. The first measurement asks KnowCode's `impact`
for each target at depth 2, follows every page, and matches answers to labels by compare-v1's declaration rule
(`judge_decls` in [score.py](../../scripts/eval/compare/score.py)). Each label the answer misses is attributed to the
first of these that finds it: no row limit, every edge regardless of evidence (`--evidence all`), depth 3. Those are
the cases a change to impact's default limit, evidence policy or depth would act on.
