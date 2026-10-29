# Spring pairs: the labels and their agreement

[labels.jsonl](labels.jsonl) labels every client call site that `route_links.py sites` finds in the four Spring
groups ([repos.txt](repos.txt)), by the rule in [the README](../README.md). The rule, the site search and the pins
were committed before any label was made. No KnowCode answer on these repositories was read while labelling.

| Group | Sites | Reaching a handler | Reaching none in the corpus | Items | Negatives |
| --- | ---: | ---: | ---: | ---: | ---: |
| jhipster | 52 | 42 | 10 | 42 | 73 |
| petclinic-rest | 31 | 31 | 0 | 31 | 52 |
| petclinic-microservices | 7 | 7 | 0 | 19 | 7 |
| piggymetrics | 13 | 10 | 3 | 10 | 11 |
| All | 103 | 90 | 13 | 102 | 143 |

The 13 sites that reach no handler: eight of jhipster's calls to `/management/*`, which Spring Boot Actuator and the
JHipster framework serve; two jhipster calls that no handler maps at all (`PATCH /api/admin/users/{login}` and
`GET /api/users/{id}`); piggymetrics' OAuth2 token request, which Spring Security OAuth2 serves; and two external APIs
in piggymetrics. One site, petclinic-microservices' `fetch` wrapper in `client.js`, is `lower_bound` with
`dynamic_name`: it reaches thirteen handlers through the gateway, one per caller, each optional as a raw `fetch`.

## Double labelling

`route_links.py seeded` chose 21 of the 103 sites (20%, rounded up), the lowest by the SHA-256 of their id: 11 in
jhipster, 5 in petclinic-rest, 4 in piggymetrics and 1 in petclinic-microservices. A second labeller, a separate agent
given the rule, LABELING.md, the site list and the clones, and none of the first labels, labelled them from source
([second.jsonl](second.jsonl)).

**Agreement: 21 of 21 sites (1.00).** The two target sets are identical on every double-labelled site, to the
handler's line. Verb, completeness, `outside_corpus` and `dynamic_name` agree on all 21 too, and both labellers left
the same three sites with no target.

The labellers differ only in negatives, which aren't part of the agreement measure: the first listed 25 on these
sites, the second 33, and 24 are shared. A link to any handler outside a site's items is wrong whether or not it is
listed as a negative, so the difference changes no score. With no disagreement to settle, the first labels are the
ones scored.
