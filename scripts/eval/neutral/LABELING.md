# Labelling rules for the neutral benchmark

Labels are made from source only, so that neither compared tool can shape them. These rules are the ones every
labeller applies; the bank's `manifest.md` says which version of them a bank used.

## Hard rules

- Never run either compared tool and never read its output (index caches, snapshots, replay results, earlier
  answers). Use grep, find, reading files, `git log/show/blame` on the clones, and compiler indexes (SCIP dumps) where
  they exist.
- The clones are read-only. Label against each repo's HEAD, the commit the question pins.
- Scope is the cloned corpus. A true answer in a repository that is not cloned is left out of `items` and counted in
  the label's `outside_corpus`.
- Test code goes in `tests`, not `items`.
- Do not look at another labeller's label for the same question (double-labelled questions measure agreement).

## Label record

    {"id": "<question id>",
     "items": [{"item": "...", "evidence": "repo/path:line or the command that shows it", "note": "...",
                "optional": true, "via": "dispatch"}],
     "tests": [...], "negatives": [...],
     "completeness": "exhaustive" | "lower_bound" | "uncertain",
     "match": "file" | "file_line" | "line_within:N" | "symbol" | "ci",
     "dynamic_name": true, "outside_corpus": 0, "note": "...", "minutes": 4}

- **Completeness.** `exhaustive`: the whole corpus (or every place that could reference the subject) was searched
  and every true item is listed; precision and recall both count. `lower_bound`: every item is certainly true but
  more may exist (dynamic dispatch, reflection, names built at runtime); recall only. `uncertain`: reported apart.
- **Empty answers are real.** "Nothing calls it" is an empty `items` list and must be `exhaustive`.
- Note every judgement call in `note`.

## Rulings

- **Optional items** (`"optional": true`): true, but a tool may omit them without penalty. Applies to:
  - indirect dependencies (a provider behind a local, a caller through a wrapper);
  - registrations under a derived interface;
  - UI: endpoints of persistent child components and app-wide singletons (account settings, permissions) that
    would fire anyway; required are endpoints reached through the component's own injected services, its
    state-management effects or facades, and dialogs it opens;
  - UI: calls made with raw `fetch()` rather than the framework's HTTP client;
  - deploy chains: blue/green alternates (the second colour's job, swap steps) and release items (tagging,
    changelog, package publishing, semantic-release) that are not on the path that ships the app.
- **Dispatch** (`"via": "dispatch"`): a call that reaches the subject only through virtual, interface or override
  dispatch. A normal required item; results are reported with and without these.
- **Cascades** (`"via": "cascade"`): an effect reached only through a cascade (a database cascade delete or trigger,
  an ORM cascade, a chained event or effect). Kept as an item and flagged.
- **Library constructors** (`"via": "library"`): a subject constructed or invoked by a library or framework on the
  code's behalf (a DI container, a mapper, a serializer, a test-host factory). Kept as an item and flagged.
- **Negatives** (`negatives`): same-named but different targets a tool might plausibly return (another type's method
  of the same name, another database's table of the same name, a JavaScript function of the same name in a
  template). Returning one is always a false positive, even on a lower-bound label.
- **Dynamic names** (`"dynamic_name": true`): the subject's name is built at runtime (per-key tables, date shards,
  copied views). Such labels are usually `lower_bound`; results are reported with and without them.
- **Routes**: write an endpoint as `VERB /path/as/written`. The scorer drops leading and trailing slashes, ignores
  case and treats `{param}` and `:param` alike, so don't normalise by hand.

## Item formats per stratum

| stratum | item | match |
|---|---|---|
| method / template-function callers | each call site, `repo/path:line` of the call expression; method groups and delegate references count | `line_within:2` |
| interface implementations and DI | each implementing class (`repo/path:line` of the declaration, note `impl`) and each registration (note `registration`) | `line_within:2` |
| table readers and writers | each file that reads or writes the table in the named database, `repo/path` (note read, write or read/write); DDL-only files don't count | `file` |
| message publishers and consumers | each publish/send site and each consumer class, `repo/path:line` (note publish or consume) | `line_within:3` |
| cross-repo HTTP callers | each file in another repo that calls the endpoint, `repo/path`; resolve the route first | `file` |
| UI component to API flow | each backend endpoint, `VERB /path` as the service builds it, evidence the service method | `ci` |
| configuration-role users | each playbook, role or requirements file that uses the role, `repo/path` | `file` |
| deploy chain | each file that builds or deploys the app (CI file, included templates in the corpus, playbooks, roles, scripts, Dockerfiles, charts, infrastructure), `repo/path`, note its step | `file` |
| infrastructure dependencies | each address it references or that references it (`var.x`, `local.x`, `module.x`, `data.t.n`, `t.n`), note depends_on or depended_by | `ci` |
