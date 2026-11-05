# find-dev-v1: the base (integration/0.8 at e87de6b)

KnowCode built from integration/0.8 (`find NAME --json -n 200`), 711 questions, 0 errors. Each option is a rule over that one answer (README, "What is measured"). Precision and recall are on the first page (10 rows); "with similar" adds similar-apart's `similar` list (up to 5 rows).

| Option | Form | Questions | Precision | Recall | Found | Recall with similar | Found with similar | Rows | KB |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| current | exact | 192 | 0.637 (654/1027) | 0.876 (644/735) | 187 | 0.876 | 187 | 1098 | 122 |
| current | lower | 154 | 0.727 (472/649) | 0.897 (468/522) | 151 | 0.897 | 151 | 661 | 74 |
| current | prefix | 186 | 0.456 (543/1190) | 0.743 (522/703) | 171 | 0.743 | 171 | 1270 | 144 |
| current | typo | 179 | 0.444 (8/18) | 0.012 (8/669) | 1 | 0.012 | 1 | 18 | 3 |
| cursor | exact | 192 | 0.974 (644/661) | 0.876 (644/735) | 187 | 0.876 | 187 | 696 | 76 |
| cursor | lower | 154 | 0.727 (460/633) | 0.874 (456/522) | 148 | 0.874 | 148 | 644 | 73 |
| cursor | prefix | 186 | 0.469 (518/1104) | 0.710 (499/703) | 163 | 0.710 | 163 | 1183 | 135 |
| cursor | typo | 179 | 0.444 (8/18) | 0.012 (8/669) | 1 | 0.012 | 1 | 18 | 3 |
| exact-only | exact | 192 | 0.974 (644/661) | 0.876 (644/735) | 187 | 0.876 | 187 | 696 | 76 |
| exact-only | lower | 154 | 0.727 (460/633) | 0.874 (456/522) | 148 | 0.874 | 148 | 644 | 73 |
| exact-only | prefix | 186 | 0.469 (518/1104) | 0.710 (499/703) | 163 | 0.710 | 163 | 1183 | 135 |
| exact-only | typo | 179 | 0.444 (8/18) | 0.012 (8/669) | 1 | 0.012 | 1 | 18 | 3 |
| similar-apart | exact | 192 | 0.974 (644/661) | 0.876 (644/735) | 187 | 0.882 | 187 | 1145 | 128 |
| similar-apart | lower | 154 | 0.727 (460/633) | 0.874 (456/522) | 148 | 0.906 | 152 | 667 | 75 |
| similar-apart | prefix | 186 | 0.469 (518/1104) | 0.710 (499/703) | 163 | 0.741 | 171 | 1248 | 142 |
| similar-apart | typo | 179 | 0.444 (8/18) | 0.012 (8/669) | 1 | 0.012 | 1 | 18 | 3 |

On the `exact` form: 190 of 192 names have an exact (case-sensitive) row; 13 have more than 10 exact rows, so the first page cuts them (the rest are behind the cursor in every option). Of the `lower` and `prefix` forms, 5 and 13 questions have another declaration named exactly as the query: those are the ones exact-only and cursor lose, and similar-apart keeps.
