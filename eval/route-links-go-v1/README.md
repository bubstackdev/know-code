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
