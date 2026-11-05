# Server routes against each framework's own listing

The HTTP routes a public repository's code serves, each labelled by the framework that serves it: the route listing
the framework itself gives for the router the repository builds. Neither KnowCode nor a model labels anything. The set
is 0.7's gate item "Routes", server routes ([plan](../../docs/plans/0.7.md#gate)):

- **Precision and recall of at least 0.95 per framework**, each reported with its counts and a Wilson interval, both
  pooled over the framework's repositories and per repository: a large application's idiom shouldn't be a whole
  framework's verdict.
- A framework with fewer than 50 labelled routes is reported, not gated.

This file states the labelling rule. It is committed before KnowCode's routes on the set are read, and doesn't change
to fit them.

## What a route is

A method and a full path the framework would dispatch to a handler, as its listing gives them: groups, mounts and
sub-routers resolved, the path with its parameters in the framework's syntax (`:id`, `{id}`, `{id:[0-9]+}`, `*`). An
endpoint answering every method is one route with method `*`: a net/http pattern without a method, and a handler a
router mounts without being able to list what it serves (chi lists such a `Mount` once per method at `prefix/*`).
Within a repository a route counts once, however many of its programs list it.

A route matches a KnowCode server route row when the methods are equal (or either is `*`) and the paths are equal by
the neutral protocol's routes ruling ([LABELING.md](../../scripts/eval/neutral/LABELING.md)): without case, leading and
trailing slashes trimmed, and every parameter alike whatever its syntax.

## Scoring

Per repository, over the code the listed programs compile (a repository's other programs, such as examples that can't
start offline, are out of the set, and so are KnowCode's routes in code only they compile; a program never compiles
its packages' `_test.go` files, which a harness test compiles only for its own package):

- **precision**: KnowCode's server routes there that match a label, over all of KnowCode's server routes there;
- **recall**: labels a KnowCode server route matches, over all labels.

Each label and each KnowCode route counts once, however many programs list it. Every figure is reported with its
counts and a Wilson interval, three ways per framework: per repository; pooled over the framework's repositories; and
the macro average of the repositories' figures, so that one large repository can't carry a framework's number. The
gate is the pooled figure at 0.95 or more, for precision and for recall; the per-repository and macro figures are
published beside it, and a repository below 0.95 is listed by name with its misses.

**KnowCode's served routes.** No KnowCode answer lists every route a repository serves (`routes` reports the requests
a repository makes; `context` gives a symbol's served routes). The scorer reads them through a reader that isn't a
product path ([served_routes.rs](../../crates/kc-index/examples/served_routes.rs), an example program of `kc-index`):
the served routes of the workspace summary that every HTTP answer reads. Two checks keep it from drifting from the
answers: a test that its routes equal `context`'s "serves" rows on a C#, a Go and a Java/Spring fixture; and, on the
real repositories, 50 routes per framework drawn with a fixed seed from its list, each confirmed through `context` on
the symbol owning it, the agreement rate published beside the gate figures. Results say so.

## Go

| Framework | Repositories |
| --- | --- |
| gin | gin-gonic/examples, gotify/server |
| echo | labstack/echox, thomiceli/opengist |
| chi | go-chi/chi (`_examples`), benhoyt/go-routing (its chi router), flipt-io/flipt |
| net/http (Go 1.22 method patterns) | urgentry/urgentry (its API), benhoyt/go-routing (its stdlib router), eliben/code-for-blog (`2021/go-rest-servers/stdlib-newmux`) |

Pinned in [repos.txt](../../scripts/eval/routes/repos.txt) at their default branches' heads on 2026-10-29; the
programs run are in [programs.txt](../../scripts/eval/routes/go/programs.txt). Chosen before anything was labelled,
and changed only for repositories whose routers couldn't be listed under the rule, before any KnowCode answer was
read: umputun/remark42 serves through go-pkgz/routegroup on net/http (not chi, and not one of the frameworks the gate
names), so flipt-io/flipt took its place; dhax/go-base builds its router only after connecting to PostgreSQL.

### Labelling

[run.sh](../../scripts/eval/routes/run.sh) copies the pinned clone into a throwaway Linux container
(`golang:1.27-bookworm`, pinned by digest), downloads the modules with the network up, disconnects (and checks that
the container no longer resolves names), and builds and runs each program there, the urgentry harness included.
Nothing of the repositories' is built or run on the host. The repository's code is built as it is:
`go build -overlay` swaps in only

- a copy of the toolchain's `net/http/server.go` ([patch.sh](../../scripts/eval/routes/go/patch.sh)) whose `Serve`,
  `ListenAndServe` and `ListenAndServeTLS` hand their handler to a hook instead of serving, and whose `ServeMux`
  reports each pattern it registers, from `(*ServeMux).register`, the code path that parses and validates it;
- one file added to the program's main package ([dump.go.tmpl](../../scripts/eval/routes/go/dump.go.tmpl)) whose hook
  prints the router's own listing and exits: gin's `Engine.Routes()`, echo's `Echo.Routes()` (v4) or
  `Router().Routes()` (v5), chi's `chi.Walk`, or the patterns the served `ServeMux` registered (net/http has no
  listing of its own; its registrations, kept as it validates them, are its own view).

A served handler the dump doesn't know (a router wrapped in a function or a middleware type) can't be listed: the
program lists nothing, and is out of the set with the code only it compiles.

Every gin, echo and chi application serves through `net/http`, so one hook reaches all four. A program is run with
the arguments it needs to start (flipt's `server`) and nothing else: no database, network or service. Two
applications embed a front-end build that isn't committed and read a file of it when they start; an empty stand-in
is created in the container's copy before the build (`prepare.sh` in
[harness/gotify](../../scripts/eval/routes/go/harness/gotify/prepare.sh) and
[harness/opengist](../../scripts/eval/routes/go/harness/opengist/prepare.sh)). The routes the Go code registers
don't depend on those files.

One router is built by a test instead: urgentry's API, by a harness test
([harness/urgentry](../../scripts/eval/routes/go/harness/urgentry/kc_routes_test.go)) that builds it as the package's
own tests do, over a test SQLite database. A program that can't build or start that way lists nothing, and is out of
the set with the code only it compiles.

### The set

[go/labels.jsonl](go/labels.jsonl): 679 routes, one row per repository, framework, method and path, with the
handlers and registration sites the listings give; [go/programs.json](go/programs.json): each program run, whether it
listed its routes, and the repository directories it compiles.

| Framework | Routes | Per repository |
| --- | ---: | --- |
| chi | 76 | go-chi/chi 52, flipt-io/flipt 13, benhoyt/go-routing 11 |
| echo | 219 | labstack/echox 25, thomiceli/opengist 194 |
| gin | 99 | gin-gonic/examples 49, gotify/server 50 |
| net/http | 285 | urgentry/urgentry 267, benhoyt/go-routing 11, eliben/code-for-blog 7 |

Programs listed: gin-gonic/examples 30 of 32, labstack/echox 28 of 31 (its subdomain example serves a virtual-host
handler the dump can't list; its reverse-proxy example registers no routes, only a proxy), go-chi/chi 10 of 12 (its
logging and router-walk examples don't serve), every other repository all of its programs.

Two labelling passes, each in a container of its own, build identical labels and program records. The listings
themselves list a route's methods in the order a map gives (chi, echo), so
[go/listings.sha256](go/listings.sha256) holds each listing's digest over its sorted lines.

## Java and Kotlin (Spring)

Spring is one framework here: Spring MVC and WebFlux request mappings (`@RequestMapping`, `@GetMapping` and their
relatives), in Java and in Kotlin. Its figures are pooled over the repositories below and reported per repository; the
Kotlin repository's figures are also reported on their own, so Kotlin isn't a footnote to Java.

| Repository | Commit | Build | Programs (modules) |
| --- | --- | --- | --- |
| jhipster/jhipster-sample-app | `6b000b5d` | Maven | the application (`.`) |
| spring-petclinic/spring-petclinic-rest | `afc8fc1d` | Maven | the application (`.`) |
| spring-petclinic/spring-petclinic-microservices | `e8b747ab` | Maven | every module (`spring-petclinic-*`) |
| sqshq/piggymetrics | `6bb2cf9d` | Maven | every module |
| spring-petclinic/spring-petclinic-kotlin | `817dba3b` | Gradle | the application (`.`), in Kotlin |

The four Java repositories are route-links-v1's Spring servers ([route-links-v1](../route-links-v1/README.md)), pinned
at the same commits, so one set of server routes serves both gates; spring-petclinic-kotlin is pinned at identity-v2's
commit. They were chosen, and this section committed, before any of them was labelled and before any KnowCode route on
them was read. Pins and families are in [repos.txt](../../scripts/eval/routes/repos.txt) (family `jvm`), the modules in
[jvm/programs.txt](../../scripts/eval/routes/jvm/programs.txt).

### What a Spring route is

A method and a path Spring's own handler mapping gives a handler method: for each type in a listed module's main code
(`src/main/java`, `src/main/kotlin`, compiled) that Spring's `RequestMappingHandlerMapping` (MVC's, or WebFlux's for a
reactive module) takes for a handler (`isHandler`: a `@Controller` or `@RestController`, and on Spring 5 a type
mapped by `@RequestMapping`), each method Spring's `getMappingForMethod` maps, read as Spring reads them
(`MethodIntrospector`, so a mapping a controller inherits from an interface or superclass counts for it). Each of a
mapping's path patterns is a route for each of its HTTP methods, a mapping with none is one route with method `*`.
Abstract types and interfaces aren't handlers themselves; the concrete types inheriting their mappings are.

Spring computes the mappings from the compiled classes and the module's own runtime classpath (its Spring version),
with no application started: no database, network, profile or configuration is read. So:

- a path pattern with a `${…}` placeholder is configuration's: such a route is left out of both sides (a label whose
  pattern has one, and a KnowCode route whose path does), and the counts left out are published;
- a handler type every module declares is listed whether or not a running application would create its bean (a
  `@ConditionalOn…` or a profile isn't read, on either side);
- a servlet context path (`server.servlet.context-path`), a path prefix configured in code (`addPathPrefix`) and
  routes that aren't request mappings (Spring Cloud Gateway's configured routes, functional `RouterFunction`s, static
  resources, actuator endpoints) aren't handler mappings, and are in neither side.

A route matches a KnowCode server route as routes-v1's rule says (methods equal or either `*`; paths equal without
case and leading or trailing slashes, every parameter alike), and scoring is the same, per repository, pooled and
macro-averaged with Wilson intervals: over the listed modules' main code, so KnowCode's routes in test code or in an
unlisted module are out of the set. Generated sources a build writes (an OpenAPI generator's interfaces) are part of
what Spring maps and not of the repository, so they are labels KnowCode may miss; how many is published.

### Labelling

[run.sh](../../scripts/eval/routes/run.sh)'s `jvm` arm ([jvm/label.sh](../../scripts/eval/routes/jvm/label.sh)) copies the pinned clone's tracked files into a throwaway Linux
container (a Maven or JDK image pinned by digest per repository, in [jvm/images.txt](../../scripts/eval/routes/jvm/images.txt):
a repository built on Java 8 gets a Java 8 JDK), and with the network up compiles each listed module's main code and
writes its runtime classpath, by the repository's own build (Maven's `compile` and `dependency:build-classpath`, or
Gradle's `classes` and a classpath task added by an init script; a front-end build a Maven profile runs is skipped).
Then it disconnects (and checks that the container no longer resolves names), and for each module compiles and runs
[KcMappings.java](../../scripts/eval/routes/jvm/KcMappings.java) (Spring MVC) or
[KcFluxMappings.java](../../scripts/eval/routes/jvm/KcFluxMappings.java) (WebFlux), whichever the module's classpath
holds, on that classpath: a subclass of the module's `RequestMappingHandlerMapping` that lists every handler type's mappings as above, one row per method and pattern, with
the handler (`pkg.Class#method`). Nothing of the repositories' is built or run on the host, and no repository code
runs beyond class loading.

Two labelling passes, each in a container of its own, must build identical labels. The labels are committed and
frozen before KnowCode's routes on these repositories are read.

### The Spring set

[jvm/labels.jsonl](jvm/labels.jsonl): 119 routes, one row per repository, method and path, with the handlers Spring
gives (`pkg.Class#method`); [jvm/programs.json](jvm/programs.json): each module listed and the directories of its main
code. Every listed module listed its mappings; none of the routes has a `${…}` placeholder.

| Repository | Routes |
| --- | ---: |
| jhipster/jhipster-sample-app | 38 |
| spring-petclinic/spring-petclinic-rest | 37 |
| spring-petclinic/spring-petclinic-kotlin (Kotlin) | 18 |
| spring-petclinic/spring-petclinic-microservices | 16 |
| sqshq/piggymetrics | 10 |

Two labelling passes, each in a container of its own, built identical labels and program records;
[jvm/listings.sha256](jvm/listings.sha256) holds each listing's digest over its sorted lines. Two labeller fixes were
made before the passes, while no label existed: spring-petclinic-kotlin's build asks for a Java 17 toolchain, so its
image is Java 17's (not 21's); and Spring 5.0's logging in piggymetrics writes to standard output, so only the listers'
rows are kept from it (`label.sh`). These labels were committed before KnowCode's routes on these repositories were
read.

### Spring results

Scored against KnowCode at `w2/spring-routes` `8814a9c` (#343, integration/0.7 with the Spring server routes) after the
labels above were frozen ([jvm/score.json](jvm/score.json)); no `${…}` placeholder was left out on either side, and 50
of 50 sampled routes were confirmed through `context` on the handler owning them.

| Repository | Labels | Precision | Recall |
| --- | ---: | --- | --- |
| jhipster/jhipster-sample-app | 38 | 1.000 (38/38, 0.908–1.000) | 1.000 (38/38, 0.908–1.000) |
| spring-petclinic/spring-petclinic-rest | 37 | 1.000 (1/1, 0.206–1.000) | 0.027 (1/37, 0.005–0.138) |
| spring-petclinic/spring-petclinic-kotlin (Kotlin) | 18 | 1.000 (18/18, 0.824–1.000) | 1.000 (18/18, 0.824–1.000) |
| spring-petclinic/spring-petclinic-microservices | 16 | 1.000 (16/16, 0.806–1.000) | 1.000 (16/16, 0.806–1.000) |
| sqshq/piggymetrics | 10 | 1.000 (9/9, 0.701–1.000) | 1.000 (10/10, 0.723–1.000) |
| **Spring, pooled** | 119 | **1.000 (82/82, 0.955–1.000)** | **0.698 (83/119, 0.610–0.773)** |

Macro-averaged over the repositories: precision 1.000, recall 0.805. Every KnowCode route matched a label. Every miss
is spring-petclinic-rest's: its controllers implement interfaces (`VetsApi`, `OwnersApi`, …) that the build's
openapi-generator writes from `src/main/resources/openapi.yml`, so the mappings Spring reads are on generated sources
the repository doesn't hold; 36 of its 37 routes are mapped there. The rule above counts them, as stated before
scoring. The Kotlin repository's 18 routes are all matched, so the grammar's misread annotations (#348) cost no
Spring route here.

**Verdict: Spring passes 0.7's gate, by precision.** The plan's gate item for server routes is "precision of at least
0.95 per framework, with recall reported" ([plan](../../docs/plans/0.7.md#gate)); this README's general rule, above,
asks for precision and recall of 0.95 or more. They disagree, and the user ruled (2026-11-04) that the plan's reading
decides 0.7's gate: Spring's pooled precision is 1.000 (82/82, interval 0.955–1.000), and its recall, 0.698, is
published with its cause, generated sources, rather than gated. The rule above is unchanged, and under it Spring's
recall is below 0.95.

## routes-v1.1: configuration-dependent registrations, and echo's placeholders

routes-v1's rule above is unchanged, and its gate figures are scored under it. routes-v1.1 is a second rule, written
after routes-v1's results were read: it states two exclusions those results showed. Because it was written after
reading them, it is never scored as a gate on routes-v1's repositories (figures under it there are a post-hoc
breakdown, said to be one). It is tested out of sample on the held-out repositories below, which were named, and this
section committed, before any of them was labelled.

Two things are left out of both sides, labels and KnowCode's routes alike, and the counts left out are published:

1. **A configuration-dependent registration.** A route's *registration site* is the call in the repository's code
   that registered it: on the labels' side, the first stack frame outside the framework's packages and the standard
   library when the framework adds the route (the labeller records it, as it records net/http's patterns now); on
   KnowCode's side, the file and line of its server route row. A registration site is configuration-dependent when,
   by the Go type checker over the packages the repository's listed programs compile:
   - its call statement lies, within its function, inside the body or `else` of an `if`, or a `case` of a `switch`
     or `select` (a loop is not a condition: a route registered for each item of a list is served); or
   - every reference to the function (or function literal) containing it, from those packages, is itself at a
     configuration-dependent site, recursively; or nothing in those packages refers to that function at all, or its
     package isn't one of them.

   A program's `main` function, and a harness test's test function, are referred to by the program itself and are
   never configuration-dependent on that count.
2. **echo's internal placeholders.** A label whose handler is echo's not-found placeholder (`echo_route_not_found`, a
   handler echo v4 adds itself under a group's prefix and `prefix/*`, not one the code registers) is not a route.

A route is left out only when its registration site is configuration-dependent on both sides' own evidence: the
labels' recorded site, and KnowCode's file and line. Where the two disagree (one side's site is configuration-dependent
and the other's isn't, or the sites differ), the route stays in, and the disagreement is listed with the results. A
route only one side has (a label KnowCode doesn't match, or a KnowCode route no label matches) is left out when its one
site is configuration-dependent: there is no other evidence to disagree.

The exclusions are computed by a script run in the labelling container from the clone alone, with the Go type
checker. It never reads KnowCode's output: it lists every configuration-dependent registration site in the code, and
the scorer applies that list to both sides. Its output is committed beside the labels.

## routes-v1.2: one hop, not transitive

routes-v1.1 is kept above as it was approved; routes-v1.2 replaces its first exclusion's second clause, and nothing
else. It was made before any held-out repository was labelled, from what the exclusion tool showed on routes-v1's
(development) repositories: applied transitively, v1.1's clause makes a whole server configuration-dependent whenever
its program chooses a command with a `switch` (`case "serve":` reaching the router through `serve`), which leaves
nothing to score. In v1.2, a registration site is configuration-dependent when, by the Go type checker over the
packages the repository's listed programs compile:

- its call lies inside the body or `else` of an `if`, or a `case` of a `switch` or `select`, within the function (or
  function literal) containing it (a loop is not a condition); or
- every reference to that function, from those packages, lies inside such a condition within the function making the
  reference, or nothing in those packages refers to it. One hop only: what calls the referring function doesn't matter.

The program's `main`, a harness test's test function and `init` functions are exempt from the second clause. A
reference is a call, or a function or method value; a call through an interface method refers to every method of
that name in those packages. echo's placeholders, the agreement of both sides, the listing of disagreements, the rule
for a route only one side has, and the tool's independence from KnowCode's output are as in v1.1. The held-out
repositories are scored under v1.2.

### Held-out repositories

One per framework, each serving at least 20 routes by a search of its source for registration calls. Pinned at their
default branches' heads on 2026-10-29:

| Framework | Repository | Commit | Program |
| --- | --- | --- | --- |
| gin | gothinkster/golang-gin-realworld-example-app | `626c372d` | the module's main package |
| echo | xesina/golang-echo-realworld-example-app | `e578590b` | the module's main package |
| chi | arp242/goatcounter | `c031008a` | a harness test in `handlers` building `NewBackend`, the router `goatcounter serve` serves for any host, with the program's defaults, as the package's own tests build it |
| net/http | golang/pkgsite | `fb32d103` | a harness test in `internal/frontend` building the frontend's `ServeMux` as the package's own tests do (`newTestServer`) |

Two were changed under the replacement rule below, before any was labelled and before any KnowCode answer on them
was read. goatcounter's `serve` serves a host router (`zhttp.HostRoute`) the dump can't list, so its router is listed
by a harness test, routes-v1's own mechanism. miniflux/v2 can't be listed: its served `ServeMux` holds about six
patterns and mounts the rest as nested muxes, so the rule's net/http listing (the served mux's own registrations)
isn't its routes; golang/pkgsite (pinned at its head on 2026-10-29), whose frontend registers its routes on one
`ServeMux`, took its place.

They are labelled once, with routes-v1's labeller and this rule, and KnowCode is scored on them once, after the Go
route changes this set was used to develop. Their figures are published beside routes-v1's gate figures, under
v1.2, and aren't tuned against. A held-out repository that can't be listed under the rule is replaced, before any
KnowCode answer on it is read, by the next candidate for its framework, and the replacement is named here with the
reason.

### Held-out results, scored once

Labelled once in the container ([run.sh](../../scripts/eval/routes/run.sh) with `SITES=1`; every program listed its
routes) and scored once under v1.2 against KnowCode at integration/0.7 with the Go routes (#281), before any change made
after reading them. [go/heldout](go/heldout/): the labels, each with its registration sites; the program records; the
exclusion tool's regions; the listings' digests; and the score.

| Framework | Repository | Labels | Precision | Recall | `context` check |
| --- | --- | ---: | --- | --- | ---: |
| chi | arp242/goatcounter | 135 | 0.959 (117/122, 0.908–0.982) | 1.000 (135/135, 0.972–1.000) | 50/50 |
| echo | xesina/golang-echo-realworld-example-app | 82 | 0.000 (0/19, 0.000–0.168) | 0.000 (0/82, 0.000–0.045) | 19/19 |
| gin | gothinkster/golang-gin-realworld-example-app | 27 | 0.731 (19/26, 0.539–0.863) | 0.963 (26/27, 0.817–0.993) | 36/36 |
| net/http | golang/pkgsite | 48 | – (no routes read) | 0.000 (0/48, 0.000–0.074) | – |

chi holds out of sample. echo and gin fail on group prefixes through helpers: echo-realworld registers every route in
`(*Handler).Register(v1)`, called on a value whose type the syntax doesn't give, so none of its 19 routes carries the
`/api` group's prefix; gin-realworld's `users` helpers lose `/api` (7 false routes) while its `articles` helpers, called
the same way, keep it. net/http fails on registration through a function parameter: pkgsite's frontend registers every
route through `Install(handle func(string, http.Handler), …)`, which KnowCode doesn't read (named as the expected
weakness before scoring). goatcounter's 5 false routes are `NewWebsite`'s, a router the program serves only for its
own website's host, which the package's tests reference.

v1.2 left out 2 of goatcounter's KnowCode routes and 1 label (`GET /*`, under a condition on both sides; the ACME
challenge, under `if acmeh != nil`) and 1 pkgsite route (in a test file the harness compiles, unreferenced); there were
no disagreements. echo-realworld pins echo v4.1.16, whose group placeholders carry real methods and echo's
`NotFoundHandler` rather than the later `echo_route_not_found` marker: 62 of its 82 labels are placeholders the scorer
didn't recognise; by the rule's words they aren't routes, and without them echo's recall is 0/20.

goatcounter was relabelled once after scoring, before any change to KnowCode: its first listing keyed chi's sites by
handler identity, which one wrapper's closures share, so routes carried other routes' sites. Keyed by endpoint and
pattern, every exclusion decision, and every figure above, is the same.

These held-outs have now been read, so a score on them after a fix isn't out of sample: the fixes they show are made
and measured on routes-v1's repositories, and a new held-out per framework is named before its labelling.

### Held-out repositories, round 2

Named after the fixes the first round showed (helper prefixes, tests' groups, registration through a function
parameter), chosen by their structure alone before any KnowCode answer on them was read, and committed before any was
labelled. Open-source licences only, since the labels quote their paths and routes. Pinned at their default branches'
heads on 2026-10-30 ([repos-heldout.txt](../../scripts/eval/routes/repos-heldout.txt)):

| Framework | Repository | Commit | Program |
| --- | --- | --- | --- |
| gin | flipped-aurora/gin-vue-admin (the module in `server/`) | `cee04b08` | a harness test in `core` initialising the server as its `main` does short of the database (the repository's `config.yaml`) and building `initialize.Routers()`, the gin engine `RunServer` serves |
| echo | go-vikunja/vikunja | `d4899715` | a harness test in `pkg/routes` building `NewEcho()` and `RegisterRoutes(e)`, as `vikunja web` does, over the default configuration |
| chi | smallstep/certificates | `06ac06a1` | a harness test in `api` building `Route(chi.NewRouter())`, as `ca/ca.go` does |
| net/http | none | | no open-source repository found, in the time given, whose `ServeMux` of at least 20 routes a harness can list under the rule: round 2 has no net/http held-out |

Two programs were corrected before any was listed or any answer read: gin-vue-admin's harness first built `Routers()`
without the configuration its `main` loads, and the empty file-store path's catch-all conflicted with its swagger route;
vikunja embeds its front end's build, which isn't committed, so an empty stand-in is created in the container's copy
([prepare.sh](../../scripts/eval/routes/go/harness/vikunja/prepare.sh)), as for gotify and opengist. vikunja uses echo
v5, whose registration sites the labeller records at `DefaultRouter.Add`, and whose route answering every method (`Any`)
its listing names `echo_route_any`, which the labeller writes as method `*`, as the rule does. vikunja registers its v2
API through huma's echo adapter, so those routes' sites are the adapter's (the first frame outside echo and the
standard library), never configuration-dependent (no routes-v1 repository
uses echo v5, so those sites are read in the listing before scoring). They are labelled once and scored once, under
v1.2, and published beside the first round. The replacement rule above applies, with these fallbacks named now:
eddycjy/go-gin-example for gin and navidrome/navidrome for chi.

### Held-out results, round 2, scored once

Labelled once in the container ([run.sh](../../scripts/eval/routes/run.sh) with `SITES=1`; each repository's one
program listed its routes: gin-vue-admin 139, vikunja 396, step-ca 29) and scored once under v1.2 against KnowCode at
integration/0.7 `5b59ef0`, the labels frozen before any answer was read. Before scoring, vikunja's echo v5 sites were
read in the listing: its 188 routes registered in its own code are each at an echo registration call, and its other 208
are at huma's echo adapter (`humaecho.go`, outside the repository), so the exclusion tool's regions never cover them.
[go/heldout2](go/heldout2/): the labels with their registration sites, the program records, the exclusion tool's
regions, the listings' digests and the score.

| Framework | Repository | Labels | Precision | Recall | `context` check |
| --- | --- | ---: | --- | --- | ---: |
| chi | smallstep/certificates | 29 | – (no routes read) | 0.000 (0/29, 0.000–0.117) | – |
| echo | go-vikunja/vikunja | 345 | 0.900 (126/140, 0.839–0.940) | 0.365 (126/345, 0.316–0.417) | 50/50 |
| gin | flipped-aurora/gin-vue-admin | 139 | 0.682 (137/201, 0.614–0.742) | 0.978 (136/139, 0.939–0.993) | 50/50 |

All three fail, each on its own cause:

- **chi**: step-ca registers every route in `Route(r Router)`, through `MethodFunc` on its own interface `Router`,
  which a `*chi.Mux` satisfies; KnowCode doesn't read a call on a value of a repository's interface type as chi's, so it
  has no routes.
- **echo**: 208 of the 219 missed labels are vikunja's v2 API, which it registers through huma (`huma.Register` with a
  `huma.Operation`), a framework KnowCode doesn't read, on echo by huma's adapter. Of the rest, 4 routes on the `/api/v2`
  group lose their prefix (a constant from another package, `apiv2.GroupPrefix`), 4 false routes beside 4 misses; 6
  build their path at runtime (`"/"+ms.Name()+"/status"`); and `u.GET("s", …)` on the `/user` group is joined with a
  slash (`/api/v1/user/s`), while echo concatenates (`/api/v1/users`), 1 false route and 1 miss. The other false routes
  are 8 metrics and pprof routes registered after an early return on configuration (`if !enabled { return }`), which the
  rule's conditions don't cover, and 1 route in a `_test.go` file the harness compiles.
- **gin**: the 64 false routes are gin-vue-admin's plugins' routes (`server/plugin/…`), registered only once the
  database is ready: `InstallPlugin` returns early, subscribing them for later, when no database is configured, as in the
  harness; an early return isn't one of the rule's conditions. The 3 misses are paths read from configuration: swagger's
  `RouterPrefix + "/swagger/*any"` and the file store's `StaticFS` (GET and HEAD).

v1.2 left out 56 of gin-vue-admin's KnowCode routes (its auto-code and skills routers, under conditions, no label
matching them), and 51 vikunja labels with 60 KnowCode routes (its CalDAV routes and others under configuration
conditions on both sides); no echo placeholders were found, and there were no disagreements.

An early return on configuration (`if !enabled { return }` before the registrations) is a known limit of v1.2: the
rule's conditions are the bodies of `if`, `switch` and `select`, so routes registered after such a return stay in, on
both sides, although the listed program doesn't serve them. It accounts for 8 of vikunja's false routes and gin-vue-admin's 64. The rule is
not changed for it now.

### Held-out repositories, round 3

Named for 0.8, to decide whether Go routes lose 0.7's "preview" label, after the fixes the second round showed
(own-type router parameters, cross-package constant prefixes, echo's concatenated joins, struct-field routers).
Chosen by their structure alone, before any KnowCode answer on them was read, and committed before any was labelled:
none was used in developing KnowCode or in any earlier set. Open-source licences only. Pinned at their default
branches' heads on 2026-11-05 ([repos-heldout.txt](../../scripts/eval/routes/repos-heldout.txt)):

| Framework | Repository | Commit | Program |
| --- | --- | --- | --- |
| net/http | go-shiori/shiori | `aebb4021` | `shiori server`, with its defaults (SQLite in its data directory); Go 1.22 method patterns on one `ServeMux` |
| net/http | Forceu/gokapi | `d0b47654` | `cmd/gokapi` over a configuration written as its setup wizard writes one (SQLite, username authentication, no TLS; [prepare.sh](../../scripts/eval/routes/go/harness/gokapi/prepare.sh)), after a run of its own non-interactive deployment mode creating its first user ([setup.sh](../../scripts/eval/routes/go/harness/gokapi/setup.sh)) |
| chi | navidrome/navidrome | `7c1e68a1` | the program, with its defaults, over an empty music folder ([prepare.sh](../../scripts/eval/routes/go/harness/navidrome/prepare.sh)), built with the tags its Makefile gives (`netgo,sqlite_fts5`, [tags](../../scripts/eval/routes/go/harness/navidrome/tags)) |
| gin | ollama/ollama | `84dc81db` | a harness test in `server` building the gin engine `ollama serve` mounts at `/` of `http.DefaultServeMux` (`GenerateRoutes`, as the package's own tests do); the program's served mux holds that one pattern, so the dump can't list it ([kc_routes_test.go](../../scripts/eval/routes/go/harness/ollama/kc_routes_test.go)) |
| gin | sosedoff/pgweb | `e4858a16` | the program, started with no server to connect to |
| echo | hatchet-dev/hatchet | `79f07e21` | a harness test in `api/v1/server/run` building the echo instance its API server serves (`getCoreEchoService`, as `Run` does), over a server configuration short of the database and other services ([kc_routes_test.go](../../scripts/eval/routes/go/harness/hatchet/kc_routes_test.go)) |

routes-v1 gates no framework with fewer than 50 labelled routes. gin has two repositories for that reason, and so does
net/http: a trial listing of shiori (made to check that each program lists its routes; no KnowCode answer was read, and
the labels are made afresh after this section is merged) gave 47 routes, so a second repository was added. Its named
fallback, openbao/openbao `70d18235` (`bao server -dev`), couldn't be listed under the rule: the handler it serves wraps
its `ServeMux` in middleware, so the dump can't name the mux it serves and the program lists nothing. Forceu/gokapi
(AGPL-3.0), chosen by its structure (about 28 registrations on one `ServeMux` its `http.Server` serves directly) and
approved by the maintainer before anything was labelled, took its place; its trial listing gave 27 routes. Without a
configuration it serves its setup wizard's routes instead, hence the configuration above. Had it not been listable,
net/http would have been reported at shiori's count, not gated. A
framework that still lists fewer than 50 adds its fallback below, after labelling and before any KnowCode answer on
the round is read, and this section says so: muety/wakapi `aba6a447` (the program, with its SQLite defaults) for chi.
A repository that can't be listed under the rule is replaced by its fallback, under the same condition.

Two labeller additions, made for this round before any of it was labelled: a program built with build tags by its own
build names them in its harness directory's `tags` file, which run.sh passes to every go command in the container, the
exclusion tool's included; and a program needing a step of its own before it serves runs it from the harness
directory's `setup.sh`, offline, before the listed build (gokapi's first user). Neither changes how any earlier
repository is labelled: none has either file.

echo's pick was knadh/listmonk (`517b4616`, echo v5); it was replaced, before anything was labelled, by its fallback,
hatchet: listmonk's `main` package connects to PostgreSQL in its `init` function, so neither the program nor a test in
that package can start without a database, which the rule doesn't allow.

**The pass rule, stated before labelling:** per framework, pooled precision of at least 0.95 under routes-v1.2, the rule
the earlier rounds were scored under, written before this round. Recall, the per-repository figures and the macro
averages are reported with their counts and Wilson intervals, not gated, and the figure under routes-v1's own rule (no
exclusions) is published beside each, on the same table. Each repository is labelled once, with run.sh and `SITES=1`,
after this section is merged; the labels are committed before any KnowCode answer on them is read, and KnowCode is
scored on them once, at the integration/0.8 commit the results name.

### Held-out labels, round 3

Labelled once in the container ([run.sh](../../scripts/eval/routes/run.sh) with `SITES=1`, from integration/0.8
`97b689c` with the fix below), and committed before any KnowCode answer on the round was read. Each repository's one
program listed its routes:

| Framework | Repository | Labels |
| --- | --- | ---: |
| net/http | go-shiori/shiori | 47 |
| net/http | Forceu/gokapi | 27 |
| chi | navidrome/navidrome | 39 |
| chi | muety/wakapi | 67 |
| gin | ollama/ollama | 49 |
| gin | sosedoff/pgweb | 32 |
| echo | hatchet-dev/hatchet | 158 |

navidrome's 50 listing rows merged to 39 routes, under 50, so chi's named fallback, muety/wakapi `aba6a447` (the
program with its SQLite defaults), was added and labelled before any answer was read, as the section above says. Every
framework now has at least 50 labels: net/http 74, chi 106, gin 81, echo 158.

One labeller fix was made during labelling. navidrome builds its routers on several goroutines at once, under an
errgroup, and the registration-site hook's map ([sites.sh](../../scripts/eval/routes/go/sites.sh)) then stopped the
program with Go's "concurrent map writes", so its first labelling listed nothing. The trial listing hadn't recorded
sites, so it never ran the hook. The map's writes are now under a mutex. For a program that registers its routes on one
goroutine, as every earlier repository does, the hook records exactly what it did before. navidrome was labelled again
with the fix; the crashed run's empty listing is not used.

[go/heldout3](go/heldout3/): the labels with their registration sites, the program records, the exclusion tool's
regions and the listings' digests. The score follows in its own commit, run once against integration/0.8.
