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
start offline, are out of the set, and so are KnowCode's routes in code only they compile):

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
   - its call statement lies, within its function, inside the body or `else` of an `if`, a `case` of a `switch` or
     `select`, or a `for` or `range` loop's body; or
   - every reference to the function (or function literal) containing it, from those packages, is itself at a
     configuration-dependent site, recursively; or nothing in those packages refers to that function at all, or its
     package isn't one of them.

   A program's `main` function, and a harness test's test function, are referred to by the program itself and are
   never configuration-dependent on that count.
2. **echo's internal placeholders.** A label whose handler is echo's not-found placeholder (`echo_route_not_found`, a
   handler echo v4 adds itself under a group's prefix and `prefix/*`, not one the code registers) is not a route.

The exclusions are computed by a script run in the labelling container from the clone alone (the type checker, no
KnowCode answer), and its output is committed beside the labels.

### Held-out repositories

One per framework, each serving at least 20 routes by a search of its source for registration calls. Pinned at their
default branches' heads on 2026-10-29:

| Framework | Repository | Commit | Program |
| --- | --- | --- | --- |
| gin | gothinkster/golang-gin-realworld-example-app | `626c372d` | the module's main package |
| echo | xesina/golang-echo-realworld-example-app | `e578590b` | the module's main package |
| chi | arp242/goatcounter | `c031008a` | `cmd/goatcounter serve`, over a new SQLite database |
| net/http (Go 1.22 method patterns) | miniflux/v2 | `229035a4` | a harness test building its HTTP handler as its own tests do, since the program connects to PostgreSQL before it routes |

They are labelled once, with routes-v1's labeller and this rule, and KnowCode is scored on them once, after the Go
route changes this set was used to develop. Their figures are published beside routes-v1's gate figures, under
v1.1, and aren't tuned against. A held-out repository that can't be listed under the rule is replaced, before any
KnowCode answer on it is read, by the next candidate for its framework, and the replacement is named here with the
reason.
