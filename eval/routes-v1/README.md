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
