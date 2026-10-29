# Client calls to server routes, labelled by hand

Which server handler each HTTP client call in a set of pinned public repositories reaches. 0.7's gate item "Client
links" ([plan](../../docs/plans/0.7.md#gate)) scores KnowCode's links from client calls to server routes against it:
precision of at least 0.95, with recall reported. No compiler or framework says which handler a client's URL reaches
across a network boundary, so these pairs are labelled by hand from source, under the neutral protocol's rules
([LABELING.md](../../scripts/eval/neutral/LABELING.md)). They are the only hand labels in 0.7's gate.

This file states the labelling rule. It was written and committed before any label was made and before KnowCode's
answers on these repositories were read, and it doesn't change to fit them. A change to the rule is a new set.

## Repositories

Each group is one workspace: the client and server repositories KnowCode indexes together. Pinned at their heads on
2026-10-29.

| Group | Repository | Commit | Client | Server |
| --- | --- | --- | --- | --- |
| jhipster | [jhipster/jhipster-sample-app](https://github.com/jhipster/jhipster-sample-app) | `6b000b5d` | Angular (TypeScript) | Spring MVC (Java) |
| petclinic-rest | [spring-petclinic/spring-petclinic-angular](https://github.com/spring-petclinic/spring-petclinic-angular) | `efe6c201` | Angular (TypeScript) | |
| petclinic-rest | [spring-petclinic/spring-petclinic-rest](https://github.com/spring-petclinic/spring-petclinic-rest) | `afc8fc1d` | | Spring MVC (Java), mappings from OpenAPI |
| petclinic-microservices | [spring-petclinic/spring-petclinic-microservices](https://github.com/spring-petclinic/spring-petclinic-microservices) | `e8b747ab` | `WebClient`, `RestClient` (Java), `fetch` (JavaScript) | Spring MVC and Spring Cloud Gateway (Java) |
| piggymetrics | [sqshq/piggymetrics](https://github.com/sqshq/piggymetrics) | `6bb2cf9d` | `@FeignClient` (Java), jQuery (JavaScript) | Spring MVC (Java) |

[repos.txt](jvm/repos.txt) holds the full commits.

## The unit

One **client call site**: an expression in client code that sends an HTTP request, at `repo/path:line` of the call.
The sites are found from source, by searching each client's request APIs:

| Client | Request APIs searched |
| --- | --- |
| TypeScript and JavaScript | Angular's `HttpClient` (`http.get`, `post`, `put`, `patch`, `delete`, `request`, `head`, `options`) and `httpResource`, `fetch`, axios, jQuery's `$.ajax`/`$.get`/`$.post`/`$.getJSON`, AngularJS's `$http` and `$resource` |
| Java and Kotlin | Spring's `RestTemplate`, `WebClient` and `RestClient` request methods (`getForObject`, `exchange`, `get()`, `post()`, `method(…)`, `uri(…)` and their relatives), and each method of a `@FeignClient` or `@HttpExchange` interface |
| Go | `net/http`'s `Get`, `Post`, `PostForm`, `Head`, `NewRequest` and `NewRequestWithContext`, and `Client.Do` |
| C# | `HttpClient`'s `GetAsync`, `PostAsync`, `PutAsync`, `PatchAsync`, `DeleteAsync`, `SendAsync`, `Get…`/`Post…AsJsonAsync` |

A call chained across lines (`this.http` on one line, `.put<…>(…)` on the next) is at the line of its method name.
Code a tool generated and the repository commits is not the client's: spring-petclinic-angular's `docs/` is
compodoc's output, a search index and compodoc's own template playground, and is left out. A function or method that
is only named like a request API (`private fetch()`) is not a site.

Lines are numbered from 1 with LF, CR LF or a lone CR ending a line, as an editor counts them: one client file in the
corpus (piggymetrics' `login.js`) ends its lines with a lone CR. Two sites on one line are told apart by their order on
it, as `repo/path:line#2` and on.

A wrapper that builds a request and passes it to one of these (a base service's `query` that every entity service
calls) is one site, at the request API's call. A Feign or `@HttpExchange` method is a site at its declaration: the
interface method is what callers call. Test code is left out, as in LABELING.md.

## The label

One record per site, in LABELING.md's form, with its words:

    {"id": "repo/path:line", "verb": "GET", "path": "/api/owners/{ownerId}",
     "items": [{"item": "GET /petclinic/api/owners/{ownerId}", "evidence": "repo/path:line", "note": "...",
                "optional": true}],
     "negatives": [{"item": "PUT /petclinic/api/owners/{ownerId}", "evidence": "repo/path:line", "note": "..."}],
     "completeness": "exhaustive" | "lower_bound" | "uncertain",
     "dynamic_name": false, "outside_corpus": 0, "note": "...", "minutes": 3}

- **verb** and **path**: the request's method and its URL as the code builds it, with the client's base URL resolved
  where the source sets it (an environment file, a constant, a `baseUrl(…)`), and each runtime value written as
  `{name}`.
- **items**: each server handler in the corpus that the request reaches. `item` is the route as served, `VERB /path`
  with the context path, written as the server declares it; `evidence` is `repo/path:line` of the handler method's
  name. A request that can reach more than one handler (a verb chosen at runtime) lists each.
- **items is empty** when nothing in the corpus serves the request, and `outside_corpus` counts the endpoints
  served elsewhere: an external API, a server that isn't cloned, an endpoint a library serves with no handler in source
  (Spring Boot Actuator's `/management/*`, Spring Security's login), or a static file. KnowCode is right on such a site
  only when it links it to nothing.
- **negatives**: handlers a match on the path or name alone might pick wrongly: the same path under another verb,
  the same path in another service, a same-named route behind another context path. Linking one is always wrong.
- **completeness**: `exhaustive` when every handler the request can reach is listed; `lower_bound` when the path is
  built at runtime from values the source doesn't fix (then `dynamic_name` is true) and the listed items are certain
  but more may exist; `uncertain` otherwise, reported apart.
- **optional**, as LABELING.md rules for UI calls: a target reached through a raw `fetch()` rather than the
  framework's HTTP client is optional, and a tool may omit it without penalty.

How a request reaches its handler is read from the server's own source and configuration: its mapping annotations
on classes and methods (Spring's `@RequestMapping` and its relatives, including those an interface declares and the
controller implements), its routing calls (Go's `HandleFunc`, chi, gin, echo), the context path
(`server.servlet.context-path`), and a gateway's route table where the corpus has one. A mapping that exists only in
generated code (an OpenAPI generator's interfaces) is read from the specification the code is generated from, and the
item's evidence is the controller method the request lands in.

## Scoring

KnowCode's links from each site to server handlers are scored as below. The scorer joins
[route_links.py](../../scripts/eval/route_links.py) with the first change that links Spring clients, so the rule here
fixes it in advance. A link matches an item when its handler is in the item's file within 3 lines of its evidence, or, where an answer gives the
route, when the routes are equal by LABELING.md's Routes ruling (case ignored, leading and trailing slashes dropped,
`{param}` and `:param` alike).

- **Precision:** of KnowCode's links from labelled sites, the share that match one of the site's items. A link from a
  site with no items is wrong, and so is a link to a negative.
- **Recall:** of all required items, the share KnowCode links. Optional items count only when linked.

Both are reported with their counts and Wilson 95% intervals, per client language and per repository group, with and
without the `dynamic_name` sites. A `lower_bound` site counts for recall only; an `uncertain` one is reported apart.

## Double labelling

A seeded 20% of the sites, chosen by the SHA-256 of their id, is labelled a second time, from source and under these
rules, by a second labeller who never sees the first labels. The **agreement** is the share of double-labelled sites
whose two target sets are identical, published with every disagreement and how it was settled. The settled label is
the one scored.
