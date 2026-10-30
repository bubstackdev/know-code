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

The table above lists what the set took.

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
    `@jellyfin/sdk`), or a `getUrl(` call whose built path, with each value `{}`, is the route's.
  - bitwarden: an `apiService.send(verb, path, …)` call whose verb and built path are the route's.
  - Test files are left out.

LABEL_CHECK

## Scoring

SCORING

The figures are given per pair and pooled, so one pair's volume doesn't carry the result.
