# Source sampling plan (public-v3)

public-v3 is the question bank for the 0.4 study. It is sampled the way [public-v2](../public-v2/sampling.md) was, and
the public-v1 and public-v2 rules apply here unless this page says otherwise. It is weighted toward the questions the
[0.4 plan](../../docs/plans/0.4.md#3-a-study-where-knowcode-should-win) names: HTTP callers across repositories and
impact two hops out, with fewer lookups that grep answers well.

## Cohort

The cohort is public-v2's: [manifest.json](manifest.json) and [scope.json](scope.json) are byte-identical to
public-v2's. That is ten public repositories in four families, at the same pins, so the source receipt is the same.

## Bank

The bank has 40 questions:

| Task | Jellyfin | Bitwarden core | Autofac | Serilog | Total |
| --- | ---: | ---: | ---: | ---: | ---: |
| HTTP callers of a C# controller action | 6 | 6 | 0 | 0 | 12 |
| Callers at one and two hops from a C# method | 4 | 4 | 3 | 3 | 14 |
| C# interface implementations and DI registrations | 2 | 2 | 2 | 2 | 8 |
| Single-repository definition lookup | 2 | 2 | 1 | 1 | 6 |

- **Lookups:** quotas are per family here, not one per repository (`sample-public-eval.py --plan` supports both).
- **HTTP:** an HTTP question asks for callers in every client repository of its family.
- **Pilot:** the pilot decides how many runs each build gets.

## Selection

- **Seed and quotas:** [sampling-plan.json](sampling-plan.json) holds the seed (`knowcode-public-v3-source-2026-10-26`),
  the quotas and the server repositories.
- **Exclusions:** the plan lists every earlier subject:
  - public-v1's 24;
  - the 12 of 0.2's exploratory A/B;
  - public-v2's 40;
  - the 28 of 0.3's two development sets (`kc-discover`, `kc-http2`).
- **Exclusion rule:** a candidate is excluded when an earlier subject has its task and file, or names the same symbol
  under any task. This is stricter than public-v2's rule, which looked only within a task. 1,388 candidates were
  excluded.
- **Review:** the provisional picks were reviewed in rank order against pinned source. Every pick was accepted;
  none was rejected, and none was replaced ([subject-review.json](study/subject-review.json)).

## Labels

- **How they were made:** every label comes from pinned source only, before any arm ran and without any tool's output.
  Six separate AI agent sessions labelled the 40 questions.
- **Completeness rule:** the labellers were told to use `exhaustive` only when a search shows nothing else can exist in
  the cohort. Otherwise they were to use `lower_bound` (interface or virtual dispatch, reflection, SDK-mediated or
  dynamically built calls), and not to choose either to reach a count.
- **Independent review:** two more sessions labelled a seeded subset of 8 again, blind to the primary labels: two per
  task type, seed `knowcode-public-v3-independent-review-2026-10-26`. All 8 agree on required items, completeness and
  impact hops ([review.json](study/review.json)).
- **No human has reviewed the labels.**
- **What came out:**
  - 30 labels are exhaustive with items, and 10 are exhaustive and empty.
  - HTTP calls through an external client library whose route can't be read from source are listed as `undecided`;
    naming one is neither right nor wrong.

### Change before pre-registration: recall over every label with items (October 26, 2026)

The 0.4 plan asked for recall to be judged over at least eight questions with lower-bound labels. Under the rule
above, **none of the 40 labels came out lower-bound**: on these questions, a careful source search could establish
completeness.

Drawing more questions until eight lower-bound labels appeared would select questions on their labels, so it wasn't
done. With the user's approval, criterion 3 instead judges recall over every question whose label names items:

- **F1:** the mean paired F1 difference is ≥ 0 over exhaustive labels with items.
- **Recall:** the mean paired recall difference is ≥ 0 over every label with items, lower bounds included if any
  appear.

That is 30 questions, against public-v2's 1. The plan's criterion 3 is updated to match.
