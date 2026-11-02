# Client calls to Go server routes, labelled by hand

Which Go server handler each HTTP client call in a set of pinned public repositories reaches: the Go part of 0.7's
gate item "Client links" ([plan](../../docs/plans/0.7.md#gate)), KnowCode's links from client calls to Go routes
scored against these labels.

**The rule is route-links-v1's, word for word.** Its sections "The unit", "The label", "Scoring" and "Double
labelling" ([eval/route-links-v1/README.md](../route-links-v1/README.md), as committed in `fde5357`) apply to this set
unchanged, including its request APIs for TypeScript and JavaScript and for Go, its wrapper rule, its test-code
exclusion, and its 20% second labelling seeded by the SHA-256 of each site's id. Only the repositories differ. A
change to that rule is a new set, here as there.

This file was written and committed before any label was made and before KnowCode's answers on these repositories
were read, and it doesn't change to fit them.

## Repositories

Each group is one workspace: the client and server repositories KnowCode indexes together. The first three are
routes-v1's repositories at the commits routes-v1 pinned
([repos.txt](../../scripts/eval/routes/repos.txt)); the fourth was found by a search for Go code requesting another
Go program's API directly with `net/http`.

| Group | Repository | Commit | Client | Server |
| --- | --- | --- | --- | --- |
| gotify | [gotify/server](https://github.com/gotify/server) | `dd06dc77` | its web UI, axios (TypeScript) | gin (Go) |
| opengist | [thomiceli/opengist](https://github.com/thomiceli/opengist) | `2acf3dae` | its front end's requests (TypeScript and JavaScript) | echo (Go) |
| flipt | [flipt-io/flipt](https://github.com/flipt-io/flipt) | `4cacb7ca` | its web UI (TypeScript) | chi (Go) |
| sftpgo | [drakkan/terraform-provider-sftpgo](https://github.com/drakkan/terraform-provider-sftpgo) | `bfceb850` | its API client, `net/http` (Go) | |
| sftpgo | [drakkan/sftpgo](https://github.com/drakkan/sftpgo) | `9712163e` | | chi (Go) |

[repos.txt](repos.txt) holds the full commits.

## Power

The set is small: 105 sites after the exclusions below, 71 Go and 34 TypeScript and JavaScript, many of them
requests to services outside the corpus. Its figures are reported with their counts and Wilson 95% intervals, per
client language and per group, and called under-powered where a language has fewer than 50 sites with a required
item. Go client links don't gate 0.7 on their own.

Not included: go-gitea/gitea, whose routes go through its own router (a separate step, not 0.7), and whose web
client's requests go through a few wrappers, so the wrapper rule leaves few sites.

## The site search

`route_links.py sites` searched only TypeScript, JavaScript and JVM request APIs when this file was frozen. Its search
was then extended, before any site was labelled, to Go's request APIs as route-links-v1's rule names them (net/http's
`Get`, `Post`, `PostForm`, `Head`, `NewRequest`, `NewRequestWithContext`, and `Client.Do`), with a unit test. The tool
changed; the rule didn't.

## Left out

Found while listing the sites, before any was labelled, and applied by
[route_links.py](../../scripts/eval/route_links.py)'s search under route-links-v1's own exclusions (code a tool
generated and the repository commits; test code):

- Go files that say a tool generated them (`// Code generated … DO NOT EDIT.`, Go's convention): flipt's HTTP SDKs
  under `sdk/go/`, generated from its protobuf services. Text in a Go string literal (a code generator's template,
  such as flipt's `protoc-gen-go-flipt-sdk`) is not a call either.
- drakkan/sftpgo's bundled third-party scripts (`openapi/swagger-ui/`, `static/assets/js/scripts.bundle.js`) and its
  API test helpers (`internal/httpdtest/`, which only its tests import).
- Two matches of the search that aren't net/http's request API: drakkan/sftpgo's `internal/httpclient/httpclient.go`
  lines 243 and 257 call go-retryablehttp's `Client.Do`, a request method only named like net/http's.

## Labelling

105 sites remain. The first pass labels all of them; the second labels the seeded 20%
([second-ids.txt](second-ids.txt), `route_links.py seeded`, 21 sites). Each pass is made by its own agent, given
only this file, route-links-v1's rule, the site list and the pinned clones: neither runs KnowCode or sees KnowCode's
output, and the second never sees the first. Their instructions are committed in [prompts](prompts/).

## Results, scored once

Scored once with `route_links.py score` (#320: route-links-v1's Scoring, a link giving a route matched by the route
alone, the file clause beside it as a secondary figure) against KnowCode at integration/0.7 `5b59ef0`; the labels were
frozen in #282. [score.json](score.json) holds every site with its links, misses and wrong links. The reader's links
agree with the `routes` answer's on 139 of 139 requesting symbols.

**Under-powered.** 47 Go sites and 22 TypeScript and JavaScript sites have a required item, both under 50, so neither
language's figure gates on its own; they are reported with their Wilson 95% intervals.

| Slice | Sites | Precision | Recall | Precision, file clause only |
| --- | ---: | --- | --- | --- |
| Go | 71 | 0.000 (0/1, 0.000–0.793) | 0.000 (0/90, 0.000–0.041) | 0.000 (0/1) |
| TypeScript and JavaScript | 34 | 1.000 (5/5, 0.566–1.000) | 0.179 (5/28, 0.079–0.356) | 0.000 (0/5) |
| gotify | 22 | – (no links) | 0.000 (0/23, 0.000–0.143) | – |
| opengist | 14 | 0.833 (5/6, 0.436–0.970) | 0.385 (5/13, 0.177–0.645) | 0.000 (0/6) |
| flipt | 8 | – (no links) | – (no required item; none of 24 optional linked) | – |
| sftpgo | 61 | – (no links) | 0.000 (0/82, 0.000–0.045) | – |
| all | 105 | 0.833 (5/6, 0.436–0.970) | 0.042 (5/118, 0.018–0.095) | 0.000 (0/6) |

The figures without `dynamic_name` sites are the same. Every match used the route clause (5); the file clause alone
finds none, because the links' rows are the handlers' or their registrars' declarations, never within 3 lines of a
label's evidence on these repositories.

Where the misses come from:

- **sftpgo's server routes aren't read** (78 of its 82 missed items, all its terraform provider's). sftpgo registers on
  a router held in a struct field (`s.router.Get(…)`) and writes paths as a package constant joined to a literal
  (`adminPath+"/{username}"`); KnowCode reads neither form (both reproduced on a made-up fixture), so the provider's
  requests, whose paths KnowCode does read (`api/v2/admins/{}`), have no route to link to.
- **`Client.Do` sites** (43 Go items): the request is built on an earlier line (`http.NewRequest`), where KnowCode
  places it; the `Do` call that sends it has no request of its own.
- **gotify's base URL** (17 items with a request read, 6 without): every request is
  `` `${config.get('url')}application/${id}` ``, a base read at runtime glued to the first segment, so the path read is
  `{}application/{}`, which no route matches; a path with no slash after the base (`{}application`) isn't read at all.
- **opengist**: its IPC client builds `baseURL+path` (8 items, no request read), and its checkbox request is relative to
  the page (`location.href… + '/checkbox'`). The one wrong link is a request to an external host,
  `https://github.com/{nickname}.keys`, linked to opengist's own `GET /:user`.
- **flipt**: every item is optional (raw `fetch`, or a base URL from the server's answer) and none is linked.
