# Contract changes against both sides' source

Server commits that remove or change an HTTP route, from public repositories whose clients live in another public
repository, each labelled with the client requests the route served. The labels are read from both sides' source at
pinned commits by a script, [contract_labels.py](../../scripts/eval/contract_labels.py). Neither KnowCode nor a model
labels anything. The set is 0.8's gate item "Contract changes" ([plan](../../docs/plans/0.8.md#gate)): of the
consumers `review` reports as `broken`, at least 0.95 must be labelled consumers, with recall reported.

This README, the labeller and the frozen rows were committed before any KnowCode answer on the set was read. A change
to a rule after that makes a new version, contract-v2.

## Pairs

| Pair | Server | Client | Server pin | Client pin |
| --- | --- | --- | --- | --- |
| jellyfin | [jellyfin/jellyfin](https://github.com/jellyfin/jellyfin) | [jellyfin/jellyfin-web](https://github.com/jellyfin/jellyfin-web) | `aec42b34` (tasks-v1's) | `f21682ad` (tasks-v1's) |
| bitwarden | [bitwarden/server](https://github.com/bitwarden/server) | [bitwarden/clients](https://github.com/bitwarden/clients) | `9129efbd` (public-v2's) | `abccf1e3` (public-v2's) |
| jellyfin-vue | [jellyfin/jellyfin](https://github.com/jellyfin/jellyfin) | [jellyfin/jellyfin-vue](https://github.com/jellyfin/jellyfin-vue) | `aec42b34` | `80a2f392` |
| bitwarden-dc | [bitwarden/server](https://github.com/bitwarden/server) | [bitwarden/directory-connector](https://github.com/bitwarden/directory-connector) | `9129efbd` | `0145d393` (public-v2's) |

PAIRS in the labeller holds the full commits.

**How the set grows** (decided before any count was taken past jellyfin's and bitwarden's, and before any KnowCode
answer). The unit is a server commit that removes or changes a route with at least one labelled consumer: the plan's
"public commits that remove or change" a contract. A commit counts once, whichever clients consume it. The set needs
at least 30. Pairs and clients are added in this order, stopping as soon as the count reaches 30:

1. jellyfin (server, jellyfin-web) and bitwarden (server, clients), surveyed first;
2. spring-petclinic-rest with spring-petclinic-angular (route-links-v1's pins);
3. jellyfin-vue, a second client of jellyfin's server;
4. bitwarden's directory-connector (public-v2's pin), a second client of bitwarden's server;
5. jellyfin-kodi, a third client of jellyfin's server, only if still short. KnowCode doesn't link Python HTTP clients, so
   its consumers count against recall, never against precision.

The table above lists what the set took. The growth stopped at step 4:

| Step | Pair | Routes removed or changed | Commits | Commits with a consumer | New | Running total |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| 1 | jellyfin | 122 | 52 | 10 | 10 | 10 |
| 1 | bitwarden | 209 | 86 | 19 | 19 | 29 |
| 2 | petclinic | 5 | 3 | 0 | 0 | 29 |
| 3 | jellyfin-vue | 122 (jellyfin's) | 52 | 0 | 0 | 29 |
| 4 | bitwarden-dc | 209 (bitwarden's) | 86 | 7 | 4 | 33 |

jellyfin-vue requests none of the 122 routes: the generated client it vendored until 2023 declares some of them, and
nothing in the app calls them.

**The rows** are every route each client's survey found gone in the set's 33 commits: 160 rows (jellyfin 46,
jellyfin-vue 46, bitwarden 34, bitwarden-dc 34), 48 of them with 58 labelled consumers. They aren't committed: they
are rebuilt from the pins, and the rebuild must match the frozen checksum. Two surveys run from their clones repeated
byte for byte. `WORK/clones` holds each repository with its full history under the name PAIRS gives it, and
jellyfin-apiclient ([jellyfin/jellyfin-apiclient-javascript](https://github.com/jellyfin/jellyfin-apiclient-javascript))
beside them.

```sh
L=scripts/eval/contract_labels.py
for p in jellyfin bitwarden petclinic; do python3 $L survey --work WORK --pair $p --out $p.jsonl; done
python3 $L survey --work WORK --pair jellyfin-vue --from jellyfin.jsonl --out jellyfin-vue.jsonl
python3 $L survey --work WORK --pair bitwarden-dc --from bitwarden.jsonl --out bitwarden-dc.jsonl
python3 $L rows jellyfin.jsonl bitwarden.jsonl petclinic.jsonl jellyfin-vue.jsonl bitwarden-dc.jsonl --out rows.jsonl
shasum -a 256 rows.jsonl   # 64138f5f282c5daa49a66e4bf58c01028016ba97ea70e285e5629417f94c8bee
```

## The rule

- **Commits:** non-merge server commits since 2020-01-01, reachable from the server pin, that change a controller
  (jellyfin: `Jellyfin.Api/Controllers/*.cs`; bitwarden: `src/**/Controllers/*.cs`, not tests).
- **Routes:** each changed controller's routes, read from its source at the commit's parent and at the commit, by
  ASP.NET attribute routing: a class's `[Route]` prefix, `[controller]` and `[action]`, each verb attribute's
  template, and a template starting `/` taken whole. A route is its verb and path. Paths compare without case, with
  each parameter written `{}`.
- **A contract change:** a route the parent serves that no controller serves at the commit. It is a change when its
  action still serves another route, and a removal otherwise.
- **The client:** read at its last commit at or before the server commit's committer date, with `git grep` and
  `git show` (no checkout).
- **The labels:** the client's requests for the old route.
  - jellyfin: a call of the generated SDK method for the action (its name in lower camel case, in a file importing
    `@jellyfin/sdk`), a `getUrl(` call whose built path, with each value `{}`, is the route's, or a call of a
    jellyfin-apiclient method that requests the route. The apiclient methods are read from that library's own
    repository at the tag of the version the client's lock file installs; a method is called on an `ApiClient` or
    `apiClient`, or on one a call returns (`ServerConnections.getApiClient(id).method(`). JavaScript, TypeScript and
    Vue files are read.
  - bitwarden: an `apiService.send(verb, path, …)` call whose verb and built path are the route's.
  - Test files are left out.

**The double-label check**, before freezing: 20% of the rows, drawn in two strata (rows with consumers, 10 of 48;
rows without, 23 of 112) with seed 20261105, were labelled again by reading the client at the row's client commit.
Each labelled consumer was checked to request the route, and each row's list was checked to be whole by searching the
client for the route's literal path runs and the action's and the apiclient's method names, through any receiver.

- Under the rule as it first stood, 10 of 10 labels were right and 32 of 33 lists whole: agreement 42/43 = 0.977.
- The one list short of whole missed two jellyfin-web calls, `getApiClient(session.ServerId).sendPlayStateCommand(…)`:
  an apiclient method called on the client a call returns. The rule was widened to those (above). Over all 122
  jellyfin rows that added exactly those two consumers and changed nothing else. Under the rule as frozen, 12 of 12
  labels are right and 33 of 33 lists whole.

## Scoring

For each row's pair and each of the set's commits it covers (jellyfin's for jellyfin and jellyfin-vue, bitwarden's for
bitwarden and bitwarden-dc), the server at the commit and the client at the row's client commit are indexed into one
fresh cache, and `review --diff <parent>` is asked in the server. Every consumer its `contracts` section reports
`broken` is scored:

- **Right** when a row of that commit and pair has the same verb and path (compared as in the rule) and a labelled
  consumer in the same client file, starting at most four lines before the reported line (a call written over
  several lines).
- **Wrong** otherwise, including a consumer of a route no row of the commit holds.

**The test:** precision, right over reported `broken`, is at least 0.95, pooled. `possible` consumers are counted and
reported, never scored as `broken`.

**Recall** is reported two ways, as matched labelled consumers (`broken` or `possible`) over:

1. all labelled consumers;
2. the labelled consumers whose request writes its path in the client's own source (`send` and `url`), leaving out
   those made through a library (`sdk`, the generated jellyfin SDK; `wrap`, jellyfin-apiclient), which no indexed
   repo holds.

The figures are given per pair and pooled, so one pair's volume doesn't carry the result.

## Result, scored once

Scored once against `review` at integration/0.8 `8faf6c3` (the crates unchanged through `231643c`), with the scorer
([contract_changes_gate.py](../../scripts/eval/contract_changes_gate.py)) on the frozen rows: every one of the 66
(pair, commit) answers asked, none in error. [score.json](score.json); every reported and labelled consumer, judged, in
[judged.jsonl](judged.jsonl). The run first stopped at jellyfin-vue: one row's client commit is empty, since
jellyfin-vue had no commit yet at that jellyfin server commit, and the scorer tried to check it out. The scorer was
fixed to review such a commit's server alone (#377), and the run went on from the answers it had kept; no answer was
asked twice.

| Pair | Commits | `broken` | Right | Precision | Recall (all) | Recall (own source) |
| --- | ---: | ---: | ---: | --- | --- | --- |
| bitwarden | 23 | 26 | 24 | 0.923 | 1.000 (24/24) | 1.000 (24/24) |
| bitwarden-dc | 23 | 8 | 8 | 1.000 | 1.000 (8/8) | 1.000 (8/8) |
| jellyfin | 10 | 3 | 3 | 1.000 | 0.12 (3/26) | 0.14 (3/21) |
| jellyfin-vue | 10 | 0 | 0 | – | – (no labels) | – |
| pooled | 66 | 37 | 35 | **0.946** | 0.603 (35/58) | 0.660 (35/53) |

**The gate fails:** pooled precision of `broken` is 0.946 (35/37, Wilson 0.823–0.985), under 0.95. No `possible`
consumer was reported.

Read after scoring:

- **The two `broken` consumers not labelled** are bitwarden-clients' requests for `POST
  providers/{providerId:guid}/billing/payment-method/verify-bank-account` (commit `398741ce`) and `GET
  tax/is-country-supported` (`90a9473a`). In both commits the route's declaration is gone from the server's source, and
  the client still requests it. Both routes were declared with a method-level `[Route(…)]` attribute, and neither is
  among its commit's rows, so the labeller didn't record either route as removed. Under the rule as frozen they count as
  wrong. A labeller change is a new version (contract-v2), not a change to this result.
- **jellyfin's recall** (3 of 26) is low because 21 of its 26 labelled consumers build their URL in the client (`url`: the path
  passed to the API client's `getUrl`), and KnowCode's HTTP client linking doesn't read those requests. `review` lists
  the routes the commits remove (for example DLNA's) with no consumer.
