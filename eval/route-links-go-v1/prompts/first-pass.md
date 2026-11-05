You are the first labeller of route-links-go-v1: hand labels of which server handler each HTTP client call in a few
pinned public repositories reaches. You label from source alone. Work carefully; these labels are a gate's ground truth.

## Inputs (read only these)

- The rule: `{RULE}` (route-links-v1's README: read "The unit", "The label", "Scoring" and "Double labelling") and
  `{LABELING}` (the neutral protocol it cites, including its Routes ruling and UI ruling).
- This set's README: `{SET}` (its groups, and what was left out of the sites).
- The sites to label, one JSON object per line: `{SITES}` (`id`, `group`, `repo`, `file`, `line`, `api`, `context`).
- The pinned clones, one directory per repository named as in the site ids: `{CLONES}`. Read them; never modify,
  build or run anything in them.

## Hard rules

- Never run `knowcode` or any KnowCode tool, MCP server or plugin, and never read any KnowCode index, answer or
  output, or any other labels of this set. Use only reading tools (Read, grep, sed, git log/show on the clones).
- Public repositories only. Never read anything outside the inputs above and your own work directory.
- Write only in your work directory: `{OUT}`.
- No network access is needed; don't fetch anything.

## What to produce

`{OUT}/labels.jsonl`: one line per site, every site exactly once, in the rule's label form:

    {"id": "<site id>", "group": "<group>", "verb": "GET", "path": "/api/...",
     "items": [{"item": "GET /served/route", "evidence": "<repo>/<path>:<line of the handler's name>", "note": "",
                "optional": true}],
     "negatives": [{"item": "...", "evidence": "...", "note": "..."}],
     "completeness": "exhaustive" | "lower_bound" | "uncertain",
     "dynamic_name": false, "outside_corpus": 0, "note": "...", "minutes": 3}

Follow the rule exactly, in particular:

- Resolve the client's base URL where the source sets it, and write runtime values as `{name}`.
- `items` are the handlers in the corpus (any repository of the site's group) the request reaches, the route written
  as the server declares it, with the evidence at the handler function or method's name. A request whose verb or path
  is chosen at run time among values the source fixes lists each target.
- `items` is empty when nothing in the corpus serves the request; then count the endpoints served elsewhere in
  `outside_corpus` (an external API, a service not cloned).
- A wrapper's request-API call is one site: its targets are what its callers in the repository pass it.
- A route served only through a mapping in generated code is read from the specification it's generated from
  (a proto's `google.api.http` option, an OpenAPI file), with the evidence at the method the request lands in.
- `optional`, as LABELING.md's UI ruling says, for a target reached through a raw `fetch()`.
- `negatives`: handlers a match on path or name alone might wrongly pick (same path, another verb; a similar path).
- Use `uncertain` only when the source can't tell you the targets; say why in `note`.
- `minutes`: your honest time on the site.

When done, check that every site id appears once and every evidence path exists in the clones, and reply with: the
count of labels by completeness, the number of required (non-optional) items, the number of sites with
`outside_corpus` > 0, and every site you found hard to decide with one line on how you decided.
