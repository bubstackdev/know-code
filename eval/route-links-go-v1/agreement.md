# Double labelling: agreement

The seeded 21 of the 105 sites ([second-ids.txt](second-ids.txt)) were labelled by two agents, each given only the
rule, this set's README, its site list and the pinned clones ([prompts](prompts/)); the second never saw the first.

**Agreement: 21 of 21 sites (1.000)** have identical target sets: the same routes (by LABELING.md's Routes ruling) in
the same handler files, every pair of evidence lines within 3 of each other. Completeness and `outside_corpus` agree
on all 21 too. There were no disagreements to settle, so the first pass ([labels.jsonl](labels.jsonl)) is the set
scored, and the second is kept as [second.jsonl](second.jsonl).

The first pass: 101 sites `exhaustive` and 4 `lower_bound` (sftpgo's event-rule HTTP action and its hook `Post`
wrapper, whose URLs come from configuration); 113 required items; 19 sites served only outside the corpus.
