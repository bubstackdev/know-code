# compare-v1: first results (0.7)

KnowCode, codebase-memory-mcp and Serena answered the same reference and impact questions on eight pinned public
repositories. Their answers are scored against labels from each language's toolchain: Roslyn for C#, the
TypeScript compiler, and the Swift index store. No tool made a label. No model was called and no agent took part.
These figures compare answers, not agents, and support no claim beyond themselves. The protocol, the frozen
mapping and the scoring rules are in [README.md](README.md). The mapping's freeze commit is 1290b5e.

## What ran

| | |
| --- | --- |
| Questions | 280 `refs` and 96 `impact`, drawn before any tool answered ([questions.jsonl](questions.jsonl)), on serilog, autofac, jellyfin-server (C#), jellyfin-web, hono (TypeScript), swift-argument-parser, swift-collections and swift-nio (Swift). Go and Java aren't included: their labels weren't frozen in time for 0.7. |
| KnowCode | integration/0.7 at b6c9bd5, built for Linux (`cargo build --release --locked`, in the pinned rust image) |
| codebase-memory-mcp | v0.12.1, the static `linux-arm64-portable` release asset, sha256 `ed0f1f54…` ([tools.toml](tools.toml)) |
| Serena | v2.1.0, the PyPI source distribution (sha256 `312c8c7c…`), installed with its own uv.lock, run with its classical tool interface |
| Pinned and latest | On 2026-10-29 the latest releases were the pinned versions, so each run below counts as both the pinned run and the latest run. |
| Where | One throwaway Linux container per repository and repetition (Docker on an Apple-silicon Mac, 4 CPUs and 12 GB each). The network was up while the tools indexed and cut before the first question. No speed or cost is measured or claimed. |
| `impact` | Two hops. Serena documents no impact or transitive call, so it has `no_counterpart` for every impact question. That is reported as such, never as a miss or a win. |

## Three runs

- **The first run** is published as it was scored.
- **The rerun** followed an audit of the three adapters against the frozen protocol. Four deviations were fixed: three favour KnowCode and one favours Serena.
- **The third run** followed a second audit, after KnowCode's maintainers found that the adapter never read KnowCode's impact continuation pages (see "A correction" below). That fix favours KnowCode.
- [README.md, "Changes after the first run"](README.md#changes-after-the-first-run) lists each fix, when and how it was found, the tool it favours, and its effect on the stored answers.
- The questions, the mapping and the scoring are unchanged throughout. All three runs used the same images and the same KnowCode build, so the adapters are the only difference between them.

Each run is two complete repetitions in fresh containers. The protocol asks for identical scores, and neither run
met it. The differences are explained here, and the figures below are each run's first repetition:

- **The first run's repetitions:** 12 of 1,128 answers differ.
  - 11 are Serena's: references that came back empty in one repetition only, 9 of them on swift-nio.
  - 1 is KnowCode's: on jellyfin-server, an answer in one repetition became the error "no indexed file has the path". KnowCode was still building its index during the first questions; see the README's index-phase fix. KnowCode answered before its index was built; a fix that answers `indexing` rather than `not_found` in that case is in progress (it is unrelated to #230). KnowCode's errors are scored as errors.
- **The rerun's repetitions:** 20 of 1,128 answers differ, all of them Serena's on Swift (18 on swift-nio, 2 on swift-argument-parser).
  - The cause is sourcekit-lsp still indexing in the background when asked.
  - Serena documents no way to wait for it, so none was added.
  - Serena's Swift `refs` recall on answered questions is 0.589 in the first repetition and 0.524 in the second, and its recall on all languages 0.851 and 0.829 (see "Serena's Swift references vary between repetitions").
  - KnowCode's and codebase-memory-mcp's answers are identical, item for item, in both repetitions.

## Reading the figures

- **Precision:** of the items an answer names that the toolchain has a view on, the share that are labels. Items the toolchain has no view on are counted as `unjudged` and left out (README, "Scoring").
- **Recall:** the share of labels named.
- Both are pooled over each group's labels, with 95% Wilson intervals. Labels within one question aren't independent, so the intervals are narrower than the questions alone would give.
- **Spans:** codebase-memory-mcp names the symbols that reference a declaration, not the reference sites. Its `refs` answers are scored as spans: one item per symbol, credited with every label inside it.
- **Two scores per run:**
  - over the questions each tool answered, with `not_found` (its own lookup found no such declaration) and `error` counted apart;
  - over every question, with those counted as every label missed.

## Serena's Swift references vary between repetitions

Serena's Swift `refs` answers aren't repeatable. In the rerun, 18 swift-nio and 2 swift-argument-parser answers
differ between the two repetitions. Most of them are references that came back empty in one repetition only, such as
`EventLoop.inEventLoop` with 89 sites in one and 0 in the other, and `CircularBuffer` with 40 and 0.

| Run | Serena Swift refs recall, first repetition | Second repetition |
| --- | ---: | ---: |
| first run | 0.550 (1360/2473) | 0.504 (1247/2473) |
| rerun | 0.589 (1489/2526) | 0.524 (1324/2526) |

Serena starts sourcekit-lsp with background indexing on, and it waits a fixed 5 seconds before the first
references request. It documents no way to wait for the index, so compare-v1 adds none. Both repetitions are shown
under each run, in "Figures that differ between the two repetitions".

## A correction: KnowCode's impact pages weren't read

An earlier version of this page said that KnowCode's impact answers stop at a default walk limit, and gave that as
the reason for its low impact recall. That cause is wrong. Withdrawn: a claim this page shouldn't have made.

- **What was wrong.** The cause was the harness.
  - A KnowCode impact answer's first page is text, and its continuation pages (`kc_more`) are JSON.
  - The adapter read only text, so it took no rows from a continuation page and never asked for the page after it.
  - In the first run and the rerun, every KnowCode impact answer was scored on its first 30 rows at most. The protocol says to follow up to 10 pages.
- **What `incomplete (depth)` means.** The note says only that there are dependents past hop 2. impact is asked and labelled at exactly two hops, so the note cuts nothing in scope.
- **How it was found.** KnowCode's maintainers read the rerun's stored answers. On jellyfin-web, for example, page 2 of one answer held 15 rows the score never saw, with 180 more rows past it.
- **What was done.** The adapters of all three tools were audited again for continuations in every format and every question type. KnowCode's JSON pages were the only deviation found. After the fix, the third run follows every page up to the protocol's 10.
- **How far the first two runs' figures understate.** Re-parsing the rerun's stored second pages alone adds 359 rows to 24 answers, and moves KnowCode's impact recall on the questions it answered from 0.371 to 0.456. The third run fetches every page.
- **The figures kept.** The first run's and the rerun's KnowCode impact figures are kept below as they were scored, for the record. They understate what KnowCode's answers held. The tables under each run still count the answers by the incomplete reasons they state.

## Run: first

Repetitions: two complete runs in fresh containers. Scores identical: no. Answers that differ between them (outcome or set of items): 12 of 1128.

| Tool | Answers that differ |
| --- | ---: |
| KnowCode | 1 |
| Serena | 11 |

### Figures that differ between the two repetitions

| Tool | Question | Language | Score | Measure | First repetition | Second repetition |
| --- | --- | --- | --- | --- | ---: | ---: |
| KnowCode | impact | All | answered | precision | 0.707 (657/929; 0.68–0.74) | 0.709 (656/925; 0.68–0.74) |
| KnowCode | impact | All | answered | recall | 0.330 (657/1990; 0.31–0.35) | 0.330 (656/1989; 0.31–0.35) |
| KnowCode | impact | C# | answered | precision | 0.679 (309/455; 0.63–0.72) | 0.683 (308/451; 0.64–0.72) |
| KnowCode | impact | C# | answered | recall | 0.531 (309/582; 0.49–0.57) | 0.530 (308/581; 0.49–0.57) |
| Serena | refs | All | answered | precision | 0.956 (5804/6070; 0.95–0.96) | 0.955 (5689/5955; 0.95–0.96) |
| Serena | refs | All | answered | recall | 0.831 (5804/6986; 0.82–0.84) | 0.814 (5689/6986; 0.81–0.82) |
| Serena | refs | C# | answered | precision | 0.903 (2473/2738; 0.89–0.91) | 0.903 (2471/2736; 0.89–0.91) |
| Serena | refs | C# | answered | recall | 1.000 (2473/2473; 1.00–1.00) | 0.999 (2471/2473; 1.00–1.00) |
| Serena | refs | Swift | answered | precision | 0.999 (1360/1361; 1.00–1.00) | 0.999 (1247/1248; 1.00–1.00) |
| Serena | refs | Swift | answered | recall | 0.550 (1360/2473; 0.53–0.57) | 0.504 (1247/2473; 0.48–0.52) |
| KnowCode | impact | All | every question | precision | 0.707 (657/929; 0.68–0.74) | 0.709 (656/925; 0.68–0.74) |
| KnowCode | impact | All | every question | recall | 0.316 (657/2080; 0.30–0.34) | 0.315 (656/2080; 0.30–0.34) |
| KnowCode | impact | C# | every question | precision | 0.679 (309/455; 0.63–0.72) | 0.683 (308/451; 0.64–0.72) |
| KnowCode | impact | C# | every question | recall | 0.510 (309/606; 0.47–0.55) | 0.508 (308/606; 0.47–0.55) |
| Serena | refs | All | every question | precision | 0.956 (5804/6070; 0.95–0.96) | 0.955 (5689/5955; 0.95–0.96) |
| Serena | refs | All | every question | recall | 0.781 (5804/7436; 0.77–0.79) | 0.765 (5689/7436; 0.76–0.77) |
| Serena | refs | C# | every question | precision | 0.903 (2473/2738; 0.89–0.91) | 0.903 (2471/2736; 0.89–0.91) |
| Serena | refs | C# | every question | recall | 0.864 (2473/2861; 0.85–0.88) | 0.864 (2471/2861; 0.85–0.88) |
| Serena | refs | Swift | every question | precision | 0.999 (1360/1361; 1.00–1.00) | 0.999 (1247/1248; 1.00–1.00) |
| Serena | refs | Swift | every question | recall | 0.538 (1360/2527; 0.52–0.56) | 0.493 (1247/2527; 0.47–0.51) |

Every other figure is the same in both repetitions. The tables below give the first repetition.

### Scored on the questions each tool answered

**refs**

| Tool | Language | Questions | Precision | Recall | Unjudged items | not_found | error | no_counterpart |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| KnowCode | C# | 105 | 0.706 (2414/3419; 0.69–0.72) | 0.844 (2414/2860; 0.83–0.86) | 193 | 1 | 0 | 0 |
| KnowCode | Swift | 104 | 0.779 (2267/2911; 0.76–0.79) | 0.897 (2267/2527; 0.88–0.91) | 101 | 0 | 0 | 0 |
| KnowCode | TypeScript | 63 | 0.808 (1326/1642; 0.79–0.83) | 0.786 (1326/1686; 0.77–0.81) | 249 | 7 | 0 | 0 |
| KnowCode | All | 272 | 0.754 (6007/7972; 0.74–0.76) | 0.849 (6007/7073; 0.84–0.86) | 543 | 8 | 0 | 0 |
| codebase-memory-mcp | C# | 92 | 0.699 (880/1259; 0.67–0.72) | 0.493 (1217/2471; 0.47–0.51) | 355 | 14 | 0 | 0 |
| codebase-memory-mcp | Swift | 53 | 0.880 (403/458; 0.85–0.91) | 0.703 (697/992; 0.67–0.73) | 83 | 51 | 0 | 0 |
| codebase-memory-mcp | TypeScript | 42 | 0.989 (351/355; 0.97–1.00) | 0.837 (829/991; 0.81–0.86) | 373 | 28 | 0 | 0 |
| codebase-memory-mcp | All | 187 | 0.789 (1634/2072; 0.77–0.81) | 0.616 (2743/4454; 0.60–0.63) | 811 | 93 | 0 | 0 |
| Serena | C# | 95 | 0.903 (2473/2738; 0.89–0.91) | 1.000 (2473/2473; 1.00–1.00) | 297 | 11 | 0 | 0 |
| Serena | Swift | 102 | 0.999 (1360/1361; 1.00–1.00) | 0.550 (1360/2473; 0.53–0.57) | 38 | 2 | 0 | 0 |
| Serena | TypeScript | 66 | 1.000 (1971/1971; 1.00–1.00) | 0.966 (1971/2040; 0.96–0.97) | 691 | 3 | 1 | 0 |
| Serena | All | 263 | 0.956 (5804/6070; 0.95–0.96) | 0.831 (5804/6986; 0.82–0.84) | 1026 | 16 | 1 | 0 |

**impact**

| Tool | Language | Questions | Precision | Recall | Unjudged items | not_found | error | no_counterpart |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| KnowCode | C# | 33 | 0.679 (309/455; 0.63–0.72) | 0.531 (309/582; 0.49–0.57) | 13 | 0 | 3 | 0 |
| KnowCode | Swift | 30 | 0.703 (204/290; 0.65–0.75) | 0.210 (204/972; 0.19–0.24) | 43 | 5 | 1 | 0 |
| KnowCode | TypeScript | 23 | 0.783 (144/184; 0.72–0.84) | 0.330 (144/436; 0.29–0.38) | 114 | 1 | 0 | 0 |
| KnowCode | All | 86 | 0.707 (657/929; 0.68–0.74) | 0.330 (657/1990; 0.31–0.35) | 170 | 6 | 4 | 0 |
| codebase-memory-mcp | C# | 25 | 0.623 (268/430; 0.58–0.67) | 0.810 (268/331; 0.76–0.85) | 7 | 11 | 0 | 0 |
| codebase-memory-mcp | Swift | 26 | 0.777 (476/613; 0.74–0.81) | 0.575 (474/825; 0.54–0.61) | 115 | 10 | 0 | 0 |
| codebase-memory-mcp | TypeScript | 22 | 0.756 (322/426; 0.71–0.79) | 0.736 (320/435; 0.69–0.77) | 159 | 2 | 0 | 0 |
| codebase-memory-mcp | All | 73 | 0.726 (1066/1469; 0.70–0.75) | 0.668 (1062/1591; 0.64–0.69) | 281 | 23 | 0 | 0 |
| Serena | C# | 0 | – | – | 0 | 0 | 0 | 36 |
| Serena | Swift | 0 | – | – | 0 | 0 | 0 | 36 |
| Serena | TypeScript | 0 | – | – | 0 | 0 | 0 | 24 |
| Serena | All | 0 | – | – | 0 | 0 | 0 | 96 |


### Scored on every question (`not_found` and `error` count as every label missed)

**refs**

| Tool | Language | Questions | Precision | Recall | Unjudged items |
| --- | --- | ---: | ---: | ---: | ---: |
| KnowCode | C# | 106 | 0.706 (2414/3419; 0.69–0.72) | 0.844 (2414/2861; 0.83–0.86) | 193 |
| KnowCode | Swift | 104 | 0.779 (2267/2911; 0.76–0.79) | 0.897 (2267/2527; 0.88–0.91) | 101 |
| KnowCode | TypeScript | 70 | 0.808 (1326/1642; 0.79–0.83) | 0.647 (1326/2048; 0.63–0.67) | 249 |
| KnowCode | All | 280 | 0.754 (6007/7972; 0.74–0.76) | 0.808 (6007/7436; 0.80–0.82) | 543 |
| codebase-memory-mcp | C# | 106 | 0.699 (880/1259; 0.67–0.72) | 0.425 (1217/2861; 0.41–0.44) | 355 |
| codebase-memory-mcp | Swift | 104 | 0.880 (403/458; 0.85–0.91) | 0.276 (697/2527; 0.26–0.29) | 83 |
| codebase-memory-mcp | TypeScript | 70 | 0.989 (351/355; 0.97–1.00) | 0.405 (829/2048; 0.38–0.43) | 373 |
| codebase-memory-mcp | All | 280 | 0.789 (1634/2072; 0.77–0.81) | 0.369 (2743/7436; 0.36–0.38) | 811 |
| Serena | C# | 106 | 0.903 (2473/2738; 0.89–0.91) | 0.864 (2473/2861; 0.85–0.88) | 297 |
| Serena | Swift | 104 | 0.999 (1360/1361; 1.00–1.00) | 0.538 (1360/2527; 0.52–0.56) | 38 |
| Serena | TypeScript | 70 | 1.000 (1971/1971; 1.00–1.00) | 0.962 (1971/2048; 0.95–0.97) | 691 |
| Serena | All | 280 | 0.956 (5804/6070; 0.95–0.96) | 0.781 (5804/7436; 0.77–0.79) | 1026 |

**impact**

| Tool | Language | Questions | Precision | Recall | Unjudged items |
| --- | --- | ---: | ---: | ---: | ---: |
| KnowCode | C# | 36 | 0.679 (309/455; 0.63–0.72) | 0.510 (309/606; 0.47–0.55) | 13 |
| KnowCode | Swift | 36 | 0.703 (204/290; 0.65–0.75) | 0.201 (204/1013; 0.18–0.23) | 43 |
| KnowCode | TypeScript | 24 | 0.783 (144/184; 0.72–0.84) | 0.312 (144/461; 0.27–0.36) | 114 |
| KnowCode | All | 96 | 0.707 (657/929; 0.68–0.74) | 0.316 (657/2080; 0.30–0.34) | 170 |
| codebase-memory-mcp | C# | 36 | 0.623 (268/430; 0.58–0.67) | 0.442 (268/606; 0.40–0.48) | 7 |
| codebase-memory-mcp | Swift | 36 | 0.777 (476/613; 0.74–0.81) | 0.468 (474/1013; 0.44–0.50) | 115 |
| codebase-memory-mcp | TypeScript | 24 | 0.756 (322/426; 0.71–0.79) | 0.694 (320/461; 0.65–0.73) | 159 |
| codebase-memory-mcp | All | 96 | 0.726 (1066/1469; 0.70–0.75) | 0.511 (1062/2080; 0.49–0.53) | 281 |
| Serena | All | no_counterpart | | | |


### KnowCode impact answers by what their first page says is incomplete

| Incomplete reasons | Answers |
| --- | ---: |
| complete | 25 |
| capped,depth | 18 |
| depth | 15 |
| policy | 11 |
| depth,policy | 5 |
| capped | 3 |
| capped,unsupported | 2 |
| policy,unsupported | 2 |
| unsupported | 2 |
| capped,depth,unsupported | 1 |
| depth,policy,unsupported | 1 |
| depth,unsupported | 1 |

Every miss of every tool: [results/first.misses.tsv](results/first.misses.tsv).

## Run: rerun

Repetitions: two complete runs in fresh containers. Scores identical: no. Answers that differ between them (outcome or set of items): 20 of 1128.

| Tool | Answers that differ |
| --- | ---: |
| Serena | 20 |

### Figures that differ between the two repetitions

| Tool | Question | Language | Score | Measure | First repetition | Second repetition |
| --- | --- | --- | --- | --- | ---: | ---: |
| Serena | refs | All | answered | precision | 0.957 (6321/6606; 0.95–0.96) | 0.956 (6156/6441; 0.95–0.96) |
| Serena | refs | All | answered | recall | 0.851 (6321/7427; 0.84–0.86) | 0.829 (6156/7427; 0.82–0.84) |
| Serena | refs | Swift | answered | precision | 0.999 (1489/1490; 1.00–1.00) | 0.999 (1324/1325; 1.00–1.00) |
| Serena | refs | Swift | answered | recall | 0.589 (1489/2526; 0.57–0.61) | 0.524 (1324/2526; 0.50–0.54) |
| Serena | refs | All | every question | precision | 0.957 (6321/6606; 0.95–0.96) | 0.956 (6156/6441; 0.95–0.96) |
| Serena | refs | All | every question | recall | 0.850 (6321/7436; 0.84–0.86) | 0.828 (6156/7436; 0.82–0.84) |
| Serena | refs | Swift | every question | precision | 0.999 (1489/1490; 1.00–1.00) | 0.999 (1324/1325; 1.00–1.00) |
| Serena | refs | Swift | every question | recall | 0.589 (1489/2527; 0.57–0.61) | 0.524 (1324/2527; 0.50–0.54) |

Every other figure is the same in both repetitions. The tables below give the first repetition.

### Scored on the questions each tool answered

**refs**

| Tool | Language | Questions | Precision | Recall | Unjudged items | not_found | error | no_counterpart |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| KnowCode | C# | 105 | 0.706 (2414/3419; 0.69–0.72) | 0.844 (2414/2860; 0.83–0.86) | 193 | 1 | 0 | 0 |
| KnowCode | Swift | 104 | 0.779 (2267/2911; 0.76–0.79) | 0.897 (2267/2527; 0.88–0.91) | 101 | 0 | 0 | 0 |
| KnowCode | TypeScript | 63 | 0.808 (1326/1642; 0.79–0.83) | 0.786 (1326/1686; 0.77–0.81) | 249 | 7 | 0 | 0 |
| KnowCode | All | 272 | 0.754 (6007/7972; 0.74–0.76) | 0.849 (6007/7073; 0.84–0.86) | 543 | 8 | 0 | 0 |
| codebase-memory-mcp | C# | 92 | 0.699 (880/1259; 0.67–0.72) | 0.493 (1217/2471; 0.47–0.51) | 355 | 14 | 0 | 0 |
| codebase-memory-mcp | Swift | 53 | 0.880 (403/458; 0.85–0.91) | 0.703 (697/992; 0.67–0.73) | 83 | 51 | 0 | 0 |
| codebase-memory-mcp | TypeScript | 42 | 0.989 (351/355; 0.97–1.00) | 0.837 (829/991; 0.81–0.86) | 373 | 28 | 0 | 0 |
| codebase-memory-mcp | All | 187 | 0.789 (1634/2072; 0.77–0.81) | 0.616 (2743/4454; 0.60–0.63) | 811 | 93 | 0 | 0 |
| Serena | C# | 106 | 0.910 (2861/3145; 0.90–0.92) | 1.000 (2861/2861; 1.00–1.00) | 311 | 0 | 0 | 0 |
| Serena | Swift | 103 | 0.999 (1489/1490; 1.00–1.00) | 0.589 (1489/2526; 0.57–0.61) | 26 | 1 | 0 | 0 |
| Serena | TypeScript | 66 | 1.000 (1971/1971; 1.00–1.00) | 0.966 (1971/2040; 0.96–0.97) | 691 | 3 | 1 | 0 |
| Serena | All | 275 | 0.957 (6321/6606; 0.95–0.96) | 0.851 (6321/7427; 0.84–0.86) | 1028 | 4 | 1 | 0 |

**impact**

| Tool | Language | Questions | Precision | Recall | Unjudged items | not_found | error | no_counterpart |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| KnowCode | C# | 36 | 0.694 (333/480; 0.65–0.73) | 0.550 (333/606; 0.51–0.59) | 13 | 0 | 0 | 0 |
| KnowCode | Swift | 36 | 0.713 (286/401; 0.67–0.76) | 0.281 (285/1013; 0.25–0.31) | 50 | 0 | 0 | 0 |
| KnowCode | TypeScript | 23 | 0.783 (144/184; 0.72–0.84) | 0.330 (144/436; 0.29–0.38) | 114 | 1 | 0 | 0 |
| KnowCode | All | 95 | 0.716 (763/1065; 0.69–0.74) | 0.371 (762/2055; 0.35–0.39) | 177 | 1 | 0 | 0 |
| codebase-memory-mcp | C# | 25 | 0.623 (268/430; 0.58–0.67) | 0.810 (268/331; 0.76–0.85) | 7 | 11 | 0 | 0 |
| codebase-memory-mcp | Swift | 26 | 0.777 (476/613; 0.74–0.81) | 0.575 (474/825; 0.54–0.61) | 115 | 10 | 0 | 0 |
| codebase-memory-mcp | TypeScript | 22 | 0.756 (322/426; 0.71–0.79) | 0.736 (320/435; 0.69–0.77) | 159 | 2 | 0 | 0 |
| codebase-memory-mcp | All | 73 | 0.726 (1066/1469; 0.70–0.75) | 0.668 (1062/1591; 0.64–0.69) | 281 | 23 | 0 | 0 |
| Serena | C# | 0 | – | – | 0 | 0 | 0 | 36 |
| Serena | Swift | 0 | – | – | 0 | 0 | 0 | 36 |
| Serena | TypeScript | 0 | – | – | 0 | 0 | 0 | 24 |
| Serena | All | 0 | – | – | 0 | 0 | 0 | 96 |


### Scored on every question (`not_found` and `error` count as every label missed)

**refs**

| Tool | Language | Questions | Precision | Recall | Unjudged items |
| --- | --- | ---: | ---: | ---: | ---: |
| KnowCode | C# | 106 | 0.706 (2414/3419; 0.69–0.72) | 0.844 (2414/2861; 0.83–0.86) | 193 |
| KnowCode | Swift | 104 | 0.779 (2267/2911; 0.76–0.79) | 0.897 (2267/2527; 0.88–0.91) | 101 |
| KnowCode | TypeScript | 70 | 0.808 (1326/1642; 0.79–0.83) | 0.647 (1326/2048; 0.63–0.67) | 249 |
| KnowCode | All | 280 | 0.754 (6007/7972; 0.74–0.76) | 0.808 (6007/7436; 0.80–0.82) | 543 |
| codebase-memory-mcp | C# | 106 | 0.699 (880/1259; 0.67–0.72) | 0.425 (1217/2861; 0.41–0.44) | 355 |
| codebase-memory-mcp | Swift | 104 | 0.880 (403/458; 0.85–0.91) | 0.276 (697/2527; 0.26–0.29) | 83 |
| codebase-memory-mcp | TypeScript | 70 | 0.989 (351/355; 0.97–1.00) | 0.405 (829/2048; 0.38–0.43) | 373 |
| codebase-memory-mcp | All | 280 | 0.789 (1634/2072; 0.77–0.81) | 0.369 (2743/7436; 0.36–0.38) | 811 |
| Serena | C# | 106 | 0.910 (2861/3145; 0.90–0.92) | 1.000 (2861/2861; 1.00–1.00) | 311 |
| Serena | Swift | 104 | 0.999 (1489/1490; 1.00–1.00) | 0.589 (1489/2527; 0.57–0.61) | 26 |
| Serena | TypeScript | 70 | 1.000 (1971/1971; 1.00–1.00) | 0.962 (1971/2048; 0.95–0.97) | 691 |
| Serena | All | 280 | 0.957 (6321/6606; 0.95–0.96) | 0.850 (6321/7436; 0.84–0.86) | 1028 |

**impact**

| Tool | Language | Questions | Precision | Recall | Unjudged items |
| --- | --- | ---: | ---: | ---: | ---: |
| KnowCode | C# | 36 | 0.694 (333/480; 0.65–0.73) | 0.550 (333/606; 0.51–0.59) | 13 |
| KnowCode | Swift | 36 | 0.713 (286/401; 0.67–0.76) | 0.281 (285/1013; 0.25–0.31) | 50 |
| KnowCode | TypeScript | 24 | 0.783 (144/184; 0.72–0.84) | 0.312 (144/461; 0.27–0.36) | 114 |
| KnowCode | All | 96 | 0.716 (763/1065; 0.69–0.74) | 0.366 (762/2080; 0.35–0.39) | 177 |
| codebase-memory-mcp | C# | 36 | 0.623 (268/430; 0.58–0.67) | 0.442 (268/606; 0.40–0.48) | 7 |
| codebase-memory-mcp | Swift | 36 | 0.777 (476/613; 0.74–0.81) | 0.468 (474/1013; 0.44–0.50) | 115 |
| codebase-memory-mcp | TypeScript | 24 | 0.756 (322/426; 0.71–0.79) | 0.694 (320/461; 0.65–0.73) | 159 |
| codebase-memory-mcp | All | 96 | 0.726 (1066/1469; 0.70–0.75) | 0.511 (1062/2080; 0.49–0.53) | 281 |
| Serena | All | no_counterpart | | | |


### KnowCode impact answers by what their first page says is incomplete

| Incomplete reasons | Answers |
| --- | ---: |
| complete | 28 |
| capped,depth | 18 |
| depth | 17 |
| policy | 14 |
| depth,policy | 5 |
| capped | 3 |
| policy,unsupported | 3 |
| capped,unsupported | 2 |
| unsupported | 2 |
| capped,depth,unsupported | 1 |
| depth,policy,unsupported | 1 |
| depth,unsupported | 1 |

Every miss of every tool: [results/rerun.misses.tsv](results/rerun.misses.tsv).


## What these results don't show

- **A fresh test of KnowCode.** KnowCode's resolvers were developed against identity-v1's labels on these same eight repositories ([eval/identity-v1](../identity-v1/README.md)). codebase-memory-mcp and Serena weren't. The questions here are drawn from those repositories' toolchain enumerations, so KnowCode's figures are on code its resolution was tuned on, and they may be higher than on code it hasn't seen. 0.8's Go and Java questions come from repositories nobody tuned on.
- **Agents.** Whether an agent does better with any of these tools; compare-v1 asks no agent.
- **Speed and cost.** No timing or cost is measured.
- **Other languages and questions.** Languages other than C#, TypeScript and Swift, and the questions 0.8 adds (callers, find, locate, task, co-change, cross-repository HTTP).
- **Features nobody else documents.** KnowCode features no other tool documents a counterpart for (history, test selection, SQL links), which compare-v1 asks no tool about ([mapping.toml](mapping.toml)).
- **How the tools would do outside the mapping.** Each tool was asked through the calls its own documentation recommends, as the mapping records. A tool might answer differently through other calls; that isn't measured.
