# Source sampling plan (public-v2)

public-v2 is a fresh question bank for the 0.3 study. It is sampled the same way as
[public-v1](../public-v1/sampling.md), whose rules apply here unless this page says otherwise. public-v1 has been
run five times, and 0.2's fixes were tuned against questions near it, so public-v2 asks nothing public-v1 asked.

## Cohort

[manifest.json](manifest.json) pins ten public repositories in four families:

- the six public-v1 repositories, at their public-v1 commits;
- a second client in the Bitwarden family, `bitwarden-directory-connector`;
- a new family, Serilog: `serilog`, `serilog-extensions-hosting` and `serilog-aspnetcore`.

The subject-file scope is in [scope.json](scope.json): public-v1's rules, plus the new repositories' source
directories. Tests, samples and generated code stay available as evidence but are not subjects.

## Bank

The bank has 40 questions:

| Task | Jellyfin | Bitwarden core | Autofac | Serilog | Total |
| --- | ---: | ---: | ---: | ---: | ---: |
| HTTP callers of a C# controller action | 5 | 5 | 0 | 0 | 10 |
| C# interface implementations and DI registrations | 2 | 3 | 2 | 3 | 10 |
| Callers at one and two hops from a C# method | 2 | 3 | 2 | 3 | 10 |
| Single-repository definition lookup | 2 | 3 | 2 | 3 | 10 |

Each repository contributes one lookup control. HTTP subjects come from the server repositories. An HTTP question
asks for callers in all of its family's client repositories, so Bitwarden questions name both `bitwarden-clients`
and `bitwarden-directory-connector`.

The pilot decides the bank's size: whether these 40 questions are enough, and how many runs each build gets. It runs
before the protocol is fixed.

## Selection

- **Ranking.** [sampling-plan.json](sampling-plan.json) holds the seed (`knowcode-public-v2-source-2026-10-22`), the
  quotas and the server repositories. `scripts/sample-public-eval.py --plan` ranks the full candidate population with
  them, by public-v1's rule.
- **Exclusions.** The plan also lists the 24 public-v1 subjects and the 12 subjects of an exploratory A/B run during
  0.2 that informed 0.2's fixes. A candidate is excluded when one of them has its task and either its file or its
  symbol name.
- **Picks.** The provisional picks were reviewed in rank order against pinned source. Every pick was accepted; none
  was rejected, and none was replaced ([subject-review.json](study/subject-review.json)).

## Labels

Every label was made from pinned source only, before any arm ran and without any tool's output. Six separate AI agent
sessions labelled the 40 questions. Two more sessions labelled a seeded subset again, blind to the primary labels: two
questions per task type, seed `knowcode-public-v2-independent-review-2026-10-22`.

On all 8, the required items, the completeness and the impact hop assignments agree. One adjudication concerns
whether two dispatch-only call sites are optional ([review.json](study/review.json)). No human reviewed the labels.

HTTP labels may list `undecided` calls: calls through an external SDK or client library whose route the labeller
can't establish from source. Naming one in an answer is neither a hit nor a false positive.

Five Bitwarden HTTP questions and one Jellyfin HTTP question, plus one impact question, have empty labels. For
Bitwarden, the callers of several sampled endpoints most likely sit in the separately licensed `bitwarden_license/`
tree, which the export leaves out. Those labels say so.
