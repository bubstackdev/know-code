//! `kc_help`: the documentation an agent reads when it needs it, rather than in every tool's advertised schema.
//!
//! The MCP server advertises each tool with a sentence or two on when to use it and a short line per parameter
//! (see `mcp.rs`); everything else lives here, by topic: one per tool (what its answer holds, its edge cases, and its
//! parameters, listed from its schema so none goes undocumented), sections of the large ones (`kc_context.http`),
//! and the concepts every answer shares (`contract`, `evidence`, `cursors`, `privacy`, `profiles`). The same topics
//! are served as MCP resources (`knowcode://help/<topic>`) for clients that read resources.

/// One topic: its name, a line for the index, and its text.
pub struct Topic {
    pub name: &'static str,
    pub summary: &'static str,
    pub text: &'static str,
}

/// Every topic, tools first (in the order the index lists them), then their sections, then the concepts.
pub const TOPICS: &[Topic] = &[
    Topic {
        name: "kc_find",
        summary: "definitions by name",
        text: r#"Find definitions by name, prefix or camelCase words ("settings view", "Store.load"), definitions ranked
first; also packages and targets from manifests. Each item has file:line and signature; a name qualified by a
namespace or module (`B.Worker.Run`, `src/worker.Worker.run`) finds that one, and items sharing a qualified name carry
their `namespace`. When more than 2,000 definitions match one way, `capped` and `truncated` are set and `total` is a
lower bound: narrow with Outer.name, a longer name or kind. A query naming something exactly adds `declared`, one line
saying it has every declaration of the name in the workspace (whichever repo was asked), and in which repos: a C#
`partial` type's parts (each part's file:line), a type's constructors (a record's primary one too), a method's
overloads, and how many references in code name it across the workspace (the other mentions: uses, which kc_refs
resolves). When files KnowCode doesn't read as code (Razor, vendored JavaScript) name it too, the count is "at least",
with those lines. Each item declaring the name asked for quotes the line naming it in `cite`, read from the file now
and checked against the index (`check: verified`). Matches with the name asked for (in any case) come first: when only
others are past the limit, the answer is `complete` and they are a `more` entry. Use kc_locate when you know what the code does but not its name,
kc_search for exact text."#,
    },
    Topic {
        name: "kc_locate",
        summary: "files for a concept or bug report",
        text: r#"Find the files for a bug report, ticket or feature: use it first when you know what the code does but not
what it is called. Files are ranked for a plain-language query ("refund approval", "lock domains for transfer", a
ticket title) by BM25 over the words of each file's path, symbol names, doc comments and the names its code
references, and of the commit subjects that changed it over the last year (camelCase split, plurals and -ing/-ed
folded; filler words and ticket ids dropped). Config, docs, scripts and templates the index holds no symbols for (YAML,
JSON, Markdown, shell, Dockerfiles, Terraform and HCL, .tftpl, .tpl, Jinja .j2) are ranked too, by their path,
headings, keys and comments, at half score. Test files score half unless the query mentions tests. Each item: file,
score, the query words it matched (`terms`), and up to three symbols whose names or docs match, with kind and line.
Several questions at once: `queries`, one result per query (at most 20), within max_chars. kc_find when you know a
name, kc_search for exact text."#,
    },
    Topic {
        name: "kc_symbol_at",
        summary: "the symbols enclosing a file:line",
        text: r#"The symbols enclosing a line of a file, innermost first (a method, then its class), each with its
qualified name (what kc_context takes), kind, signature and line range. For mapping a stack frame to code.
`file_indexed` is false when the file has no symbol index. `file` picks its repo: an absolute path inside a repo, one
elsewhere by the repo name in it (a stack frame from another machine), a path after the repo's name (`shop/src/a.cs`),
one relative to a workspace root, or a repo-relative path in exactly one repo; or pass `repo`."#,
    },
    Topic {
        name: "kc_refs",
        summary: "every reference to a name, resolved",
        text: r#"Every reference to a name (calls, instantiations, type and member uses), each resolved to its definition
with a confidence (exact, unique_name, ambiguous, external_likely, unresolved) and `evidence` (see `evidence`). A name
resolves only to definitions in its own language (TypeScript, TSX and JavaScript count as one), and in C# only to those
compiled by the caller's project (its .csproj, sources it links in) or a project it references, transitively; where no
project file decides it, never exact. Edges between languages come from HTTP routes, the bus, SQL and DI, never a
shared name. Each place a name is written is its own reference, with `col` (1-based byte column) beside `line`:
`a.Save(); b.Save();` on one line is two references, each resolved through its own receiver. Includes references from
repos that depend on this one. Long lists come most certain first, then by how central each file is to the definition
(personalized PageRank); `total` and the `incomplete` entry say how many more there are and how to get them.

In C#, a type named only as a generic argument (`AddBus<IOrders>()`, `Get<IOrders>()`, `AddScoped<IFoo, Foo>()`), in
`typeof`, `nameof` or a generic constraint (`where T : IFoo`) is a `type_ref` with `usage` saying so (`type_argument`,
`typeof`, `nameof`, `constraint`); a target-typed `new(…)` is an `instantiate` with `usage: target_typed`. A file sees
its own `using`s and what its project imports into every file: `global using` directives (GlobalUsings.cs), the
.csproj's `<Using>` items, and the SDK's implicit usings (Microsoft.NET.Sdk, .Web, .Worker, MSTest.Sdk,
Aspire.AppHost.Sdk) when ImplicitUsings is enabled; where those can't be read (a property sets ImplicitUsings, another SDK) nothing is ruled out by them.
A constructor's callers are the creations its arguments pick (count, named and optional arguments, `params`, the
argument types the site tells) and the constructors chaining to it (`: base(…)`, `: this(…)`, a primary
constructor's base): a creation more than one overload fits is `ambiguous` to each, never confident.

References come grouped by caller (each caller's together, at the place of its first; code before tests), a row in a
test file marked `test`. `callers` tallies the callers and sites over every reference found, tests apart, and names the
definitions a name-wide query resolves to when there are several. When the references resolve to overloads (methods or
constructors sharing a qualified name), `overloads` numbers them with their signatures and the sites reaching each
alone, and each row's `overload` says which it calls, or which it may call when its arguments don't decide it.

For an interface or base class, `hierarchy` lists its `implementers` (or `subclasses`, with those in indexed repos
depending on it), those reaching it `through` another type (a subclass of an implementation, `via` the type between),
`derived` interfaces, and `di`: its registrations and those of its derived interfaces and implementations (C#); each
list empty is said in `none_found` with what was searched. An assembly scan that may register it is in `incomplete`
(when nothing registers or injects the type, a scan of an assembly the code doesn't name is in its `none_found` line
instead); one that can't (in test code, or exposing only other services) is counted in that line. Razor and other files KnowCode doesn't read as code are checked as text for the name: the lines naming it
are in `incomplete`; when none does, `checked` says so.

`Type.member` keeps only references that can point at that member. Filter with `kinds` (call, instantiate, type_ref,
member; SQL read, write) or `edges` (the same filter named as kc_trace names edges: calls, instantiates, uses, member,
reads, writes). Several names at once with `names` (at most 20), one result per name. SCIP, TypeScript imports, SQL,
Ansible and Terraform: `kc_refs.languages`."#,
    },
    Topic {
        name: "kc_refs.languages",
        summary: "kc_refs: SCIP, TypeScript imports, SQL, Ansible, Terraform",
        text: r#"When the repo has a SCIP index (index.scip at its root, from scip-typescript, scip-dotnet, …) covering the
file, unchanged since it was made, a reference takes the compiler's answer: `precise: true`, exact (or unresolved for a
symbol outside the repo), and `corrected_from` (the heuristic confidence) where the heuristics had pointed elsewhere;
`precise` and `corrected` count them.

A TypeScript import is placed by tsconfig `paths`/`baseUrl` (nearest config, `extends` followed); `basis` explains a
confidence the evidence capped: an import no config maps, placed by its file name (at most unique_name), one a config
maps to no indexed file (unresolved), or a name several imported dependencies declare (ambiguous).

SQL files: `read` and `write` of a table, `call` of a procedure or function, `type_ref` for ALTER TABLE, indexes,
foreign keys and a trigger's table. SQL names match without case.

Ansible: `role` (a play's roles:, include_role, a meta dependency), `include` (a task file or playbook, by file name),
`notify` (a handler), `var` (a Jinja variable use) and `template` (a template file); a role's references from playbooks
in other repos are included, linked through requirements.yml (exact when its src is the role's repo).

Terraform: names are addresses (`aws_s3_bucket.logs`, `data.aws_iam_policy.p`, `module.net`, `var.region`,
`local.name`, `output.arn`, `provider.aws.east`) and kinds are `tf_ref` (an expression using it, in arguments, count,
for_each, templates and templatefile() arguments), `depends_on`, `provider` (a provider = meta-argument), `tf_input` (a
module call's argument setting the module's variable) and `tf_output` (module.m.out reading the module's output); every
reference resolves only within its own directory (the configuration), so per-environment copies with the same names
stay apart, and a module's variables and outputs are reached from the module calls of other repos whose source (git URL
or registry address) links to it."#,
    },
    Topic {
        name: "kc_context",
        summary: "one bundle about a symbol (sections: kc_context.*)",
        text: r#"One bundle about a symbol: its source, members, extensions, conformances, top callers and callees with
file:line, within a token budget (`budget`, default 1200). Usually the first call after kc_find. Give the repo's own
name to ask how the repo is deployed (`kc_context.deploy`).

When several definitions share the name (copies, template variants), the live one is shown and the others are listed in
`alternatives` (with `namespace` where qualified names are the same); pick one with `path/to/file#name`, `file:line` or
its namespace or module (`B.Worker.Run`). A place may be written after its repo's name (`shop/src/a.cs:12`), here and
in kc_impact, kc_trace and kc_refs (which answers for the definition there); one naming no definition is refused, saying
what didn't resolve (no such file, or the definitions nearest the line). A stored procedure or table defined in another
indexed repo (a schema repo) is found there.

`source` is `auto` by default: a long symbol comes as a skeleton (signatures, types and doc comments with bodies elided
as `{ … }`, plus the lines making its calls into the indexed code and its return/throw lines; `source_lines` lists the
file lines kept), a short one in full; ask for `full`, `skeleton` or `none`. Each caller carries its source line
(`code`).

For a method implementing an interface member, or an `override`, calls through that member are callers too, with
`through` naming the interface or base type (ambiguous when the call may reach another implementation; test doubles
beside one real implementation don't count). An override of a base class in an indexed dependency also lists the
base's calls of it, with `repo` naming that repo. For an interface or base class, `conformers_elsewhere` lists its
implementations and subclasses in the indexed repos depending on its repo, matched by its namespace, each with `repo`;
`conformers_indirect` the types reaching it through another (`Name file:line via Base`: a subclass of an
implementation, an interface deriving from a derived interface and its implementations), and `di_implementers` the
DI registrations of its derived interfaces and implementations; `none_found` says each of its lists that came back
empty (implementations, derived interfaces, subclasses, DI registrations) with what was searched. A C# `partial` type
lists its `parts` (file:line each); members list constructors first. When no reference resolves to a method, `note` says so and counts the references by its name that resolve elsewhere or
not at all (kc_refs lists them). `hints` say which lists were cut and how to get the rest. Several symbols at once:
`symbols` (at most 20), one bundle each, sharing the budget.

Sections present when they apply, each a topic: `kc_context.http` (routes across repos), `kc_context.angular`,
`kc_context.di` (dependency injection), `kc_context.tables` (Entity Framework and SQL text), `kc_context.bus` (message
bus), `kc_context.sql` (SQL objects), `kc_context.ansible`, `kc_context.deploy` (CI deploy chains), and
`kc_context.terraform`."#,
    },
    Topic {
        name: "kc_context.http",
        summary: "kc_context: HTTP links across repos",
        text: r#"`http` (when present) links across repos: for a request contract (a class attribute with a path and a
verb, or a request attribute or decorator the config names), code that creates one, or a method calling a service
gateway by service and path (a gateway call the config names) or an HTTP client with a URL (fetch, axios, $.ajax,
Angular and C# HttpClient, wrappers named for HTTP or API clients; base + path composed), the route it requests and
the controller actions serving it, with their source; a call to a generated API client (`getItemsApi(api).getItems()`)
links by operation name (assumption operation_name), in the servers its repo is known to call (server_unverified when
none is); for a controller action, the contracts and calls requesting it, said first too as `called_over_http`
({route, by: the first 5 as `repo/file:line Symbol`, with `(ambiguous)`, `(dominant)` or `(by operation name)` when
so; by_total when cut}).
With no links, `note` says what was searched and what is left: requests whose URL is known only at run time, and the
nearest routes by path. A contract that looks
left over is marked: `unsent` (no code in the indexed repos creates it) and `commented_out` (nothing serves its route,
but a controller has a commented-out [Http*] attribute for it, with file:line). Each link has a confidence: exact (one
repo serves the route), dominant (the frontend's main backend among several), ambiguous.

Served: controller actions (prefix from the class or the nearest base class's [Route], [controller] and [action]
filled; conventional routes with a literal template) and minimal API endpoints (MapGet/MapPost/MapPut/MapDelete/MapPatch/MapMethods, Map for
every verb, under MapGroup prefixes, through helper methods); the handler owns the route when it is a method group.
Requested: also request classes with a path and verb in an attribute ([Endpoint("/orders", HttpMethod.Get)]). A link
with `host` rests on a host: the client's URL, base address or settings name it, and launch profiles, compose,
Aspire, ingress or proxy configuration map it to the repo (assumption configured_host). A registration KnowCode can't
place (a template that isn't a constant) makes the answer incomplete with reason `unsupported`. A call to an
external client the repo declares in a .d.ts links through its SDK package's code when that is installed at the
version the lockfile pins: `package` {package, version, method, at, request} (assumption package_code)."#,
    },
    Topic {
        name: "kc_context.angular",
        summary: "kc_context: Angular components",
        text: r#"`angular` (for a component) gives its selector, template file, the route paths that show it, the
components whose templates use it (as an element or an attribute directive), and the elements and directives its
template uses; where a component is opened as a dialog, routed or declared shows in its callers (type references)."#,
    },
    Topic {
        name: "kc_context.di",
        summary: "kc_context: dependency injection (C#)",
        text: r#"`di` (for a C# type) lists its dependency-injection registrations (lifetime, kind, file:line; test ones
marked; a factory's implementation is what its lambda returns: `new X`, `GetRequiredService<X>()`, a local, a factory
method's declared return type, a property's type), the implementations a service is bound to, injection sites
(constructors taking it), and flags: `unregistered` (an interface injected but never registered in the repo) and
`conflicting` (several implementations registered for one service by plain registrations: not `TryAdd*`,
`TryAddEnumerable` or `Replace`). A call through a service bound to one implementation carries it as `via_di`; a
generic service is one service per type arguments (`IHandler<Order>`, `IHandler<Invoice>`), an open generic
registration (`typeof(IRepo<>)`) serves every one, and same-named services in two namespaces are two."#,
    },
    Topic {
        name: "kc_context.tables",
        summary: "kc_context: Entity Framework tables and SQL text",
        text: r#"`tables` (Entity Framework, for an entity class or DbSet property) gives the table with every method reading
or writing it (mode read, write, read_write for a method doing both, or access; an assignment to an entity writes only
where the method saves it, itself or through a repository or unit of work it calls, or in the entity's own methods; one
call deep, `via` names the callee a write goes through: `saved: here` when this method saves what a helper changed (the
helper is marked `saved_by_caller` and doesn't write by itself), `saved: callee` when the callee writes and saves,
`saved: here` when it only stages its write (Add, Update, an entity's own method) and this method saves it, the callee
then marked `saved_by_caller`; a callee's write is left out of the mode and counted in `writes_via`, not `writes` or
`other`; callers not touching the table themselves are listed with `only_via`, at most 5 per callee and none from test
code, the rest counted in `hidden_via`); for a DbContext, its tables; `touches_tables` (for a method) gives the tables
and stored procedures it reaches, itself (hop 0) or through its calls, including interface implementations.

Tables named in SQL text (CFML `<cfquery>` and `queryExecute`, and C# strings passed to
Query/Execute/FromSqlRaw/MySqlCommand or held in a sql/query field) count too: the method or template running the query
reads or writes them. Tables reached through a navigation count: `.Include`/`.ThenInclude` of it, a query on the owner
naming it in a lambda, an `AutoInclude()` of it (every reader of the owner reads it) are reads; a saved change to a
collection navigation (`order.Lines.Add(l)`) is a write. Each table carries `database`: the database it is in, with `by`
(schema: the mapping or SQL text names it; connection: the context's connection setting; context, context_tables, repo,
repo_tables, only) and `same_name_in` (other databases with a table of that name), or confidence ambiguous. A table
name ("shop_order") that names no symbol returns that table. EF tables in `tables`, and procedures in `touches_tables`,
carry their SQL definitions (`sql`, `definition`) with the confidences of `kc_context.sql`."#,
    },
    Topic {
        name: "kc_context.bus",
        summary: "kc_context: message bus (C#)",
        text: r#"`bus` (C# message bus) gives, for a message type (one a consumer handles, `IConsumer<X>` or
`IHandleMessages<X>`, or a class a message attribute the config names marks), its publishers and consumers in
any indexed repo; for a method that publishes, each message (full name, queue) and its consumers; for a consumer class
or its Consume method, the publishers. A publish through a helper method of the same type (`TryPublish<T>(T m)`, a
generic or `object` parameter passed on) is the helper's caller's, at the call; a message built by a method of the
file (`Publish(Build(…))`) has that method's return type. Consumers include MassTransit state machines (their
`Event<X>` and `Schedule<…, X>` properties) and registrations closing a generic consumer over a message
(`AddConsumer<MailConsumer<X>>()`, `AddMailConsumer<X>()`), at the registration. Each link is exact (both sides name
the same declared message type) or ambiguous (the bare name only)."#,
    },
    Topic {
        name: "kc_context.sql",
        summary: "kc_context: SQL tables, views, procedures",
        text: r#"For a SQL table, view, procedure, function or trigger (from .sql files: dumps, migrations, routines),
callers are the SQL reading or writing a table (kind read, write, type_ref) or calling a routine, callees what its body
reads, writes and calls, and `sql` gives its columns, `reads`, `writes` and `calls` by name, and `used_by`: code in any
indexed repo using it, either an Entity Framework entity mapped to the table (with its readers and writers) or a stored
procedure call (EXEC, MySQL CALL, CommandType.StoredProcedure), each with a link confidence: exact (the repo defines it,
or both sides name the schema), unique_name (one schema in the workspace defines it), dominant (several schemas do; for
an EF table, the one its DbContext is named for or that defines most of the context's tables, else the one defining
most of the repo's tables), ambiguous. A procedure name passed to a wrapper method (`ExecuteStoredProcedureAsync("p",
…)`) counts as a call when a procedure of that name is defined."#,
    },
    Topic {
        name: "kc_context.ansible",
        summary: "kc_context: Ansible roles, playbooks, tasks",
        text: r#"For an Ansible role, playbook, play, task, handler, variable or template, callers are the references
reaching it (role uses, includes, notifies, variable uses, template tasks), and `ansible` gives `roles` (roles it uses
from other repos, with the repo and confidence), `used_by` (for a role: plays, tasks and roles in other repos using it)
and `deploys` (services, images, source repos, RabbitMQ vhosts, databases and URLs its tasks name, each with the task's
`action`: deploys, undeploys, restarts or stops, from the module's state or the verb opening the task's name, never
from the role; variables read where the role defines them once, each linked to an application repo, bus host or SQL
schema when one matches; infrastructure names like consul or mysql never link). For a playbook, a role, or a CI job or
template, `ansible.deploys_apps` gives the application repos deployed with it (`count`, and per app its job, playbook
and key values)."#,
    },
    Topic {
        name: "kc_context.deploy",
        summary: "kc_context: how a repo is deployed (CI chains)",
        text: r#"A query that is the repo's own name returns, as the first item and as the answer's top-level `deploy`
(ahead of any same-named symbol), `ansible.deployed_by`: the roles and playbooks in other repos deploying or
configuring it, and the repo's own playbooks and roles. It also returns `ansible.deploy_chains`, how the repo's GitLab
CI pipeline deploys it: for each `ansible-playbook` run, the `path` from the repo's job through shared CI templates
(other repos, by include) and child pipelines, the `playbook` (in the repo the job clones), `inventory`, `vars` (each
extra var's `value` and `from` where it is set, or `unresolved`: variables nothing in the files sets, such as GitLab
project settings or values computed at run time), the playbook's `roles`, the `deploys` those values become (systemd
units, consul names, paths, nginx sites), undecided rule `conditions`, and `confidence`.

Jobs deploying without Ansible are chains too, `kind` naming the tool (`wrangler`, `aws`, `terraform`, `docker`,
`kubectl`, `helm`, `sam`, `cdk`; `release` for `npm publish`, `upack push`, `release-cli create`): their `steps` give
each command with `at`, `via` (an `npm run` script read from package.json), `files` (wrangler config and `main` entry,
Dockerfile and context, Terraform directory, manifests, chart; `found` in the checkout), `values` (worker name, S3
source and destination, CloudFront distribution, ECS cluster and service, image, …) and, for Terraform, `modules` linked
to the repos their sources name. AWS CLI deploys (launch templates, auto scaling refreshes, `ssm send-command`,
CloudFormation, Elastic Beanstalk) are chains too, also inside `X=$(…)`.

A chain through an include whose rule can't be decided is `ambiguous`; through a GitLab built-in template
(`Terraform/Base.latest.gitlab-ci.yml`, known, not fetched) `external_likely`, and `ci_templates` lists those
templates with the jobs they provide. `include: component:` of an indexed project is read with its inputs. Each chain
gives `ci_files` (every CI file it goes through, with the files including them); an Ansible chain's `playbook.files`
(templates, included task and variable files, requirements) and `roles` (to depth 4, each with `tasks`, `handlers`,
`meta` and `templates` files; a role several repos define lists every candidate `ambiguous`, a Galaxy role no repo
holds is `external_likely` without a repo). A `parallel: matrix:` job gives a chain per combination (`matrix`).
`ci_unread` lists includes it couldn't read (a component no indexed repo holds is external).

`ci_includes` accounts for every `include:` entry of every CI file the pipeline read, recursively (`local`, `project`
with `file` and `ref`, `remote`, `template`, `component` with its `@ref`): `at` (file:line, repo:file:line in another
repo), `target`, and `status`. `followed`: the `repo` it was read from (which joins the answer's provenance), `files`,
the `jobs` those files define with their stage (`deploy_app (deploy)`), and the `deploys` chains run from them; an
include of files already read (a cycle) says so in `why` and is read once. Otherwise `why` says what stopped it:
`missing` (the repo holding it isn't indexed, or its checkout is gone: the answer is incomplete, `missing_repo`, naming the project path, file
and ref), `unread` (a path holding a variable nothing sets, a file not there), `unsupported` (a GitLab built-in
template this doesn't know, a job's artifact, an include in a form this doesn't read: kind `unknown`) or `cut` (past
a limit). Includes whose rules leave them out of a default-branch push are listed apart, one line each, in
`ci_skipped` (`file:line: kind target`): nothing in the chains comes from them. No include is dropped silently.

Every list of the answer continues by cursor when cut (`<list>_cursor`, kc_more): `limit` cuts each to that many rows
(saying `<list>_total`); `max_chars` cuts the long tails first (`ci_includes`, `ci_skipped`, `ci_templates`, `own`),
then `deployed_by` and `deploy_chains`, a chain or an include whole. `ci_unread` and `ci_cut` are never cut. The answer
is given once, at `items[0]` (the CLI's `--json` repeats it as `deploy`)."#,
    },
    Topic {
        name: "kc_context.terraform",
        summary: "kc_context: Terraform blocks and modules",
        text: r#"Terraform blocks are named by address (`aws_s3_bucket.logs`, `data.aws_iam_policy.p`, `module.net`,
`var.region`, `local.name`, `output.arn`, `provider.aws.east`): callees are what it depends on (expression references,
depends_on, its provider), callers what uses it, both within its directory only; the same address in several
environment directories gives `alternatives` (pick one with `path/main.tf#aws_s3_bucket.logs`). `terraform.module`
(for a module call) says where its `source` leads: kind local, git or registry, the `repo` and `dir` holding it, and a
confidence (exact: a local directory with configuration or a git URL that is the repo's origin; unique_name: a registry
address matching one repo by host, namespace and name; external_likely: not indexed). `terraform.module_called_by`
(for any block of a module's configuration) lists the module calls using it, in this repo and others, and a module
variable's or output's callers include the calls in other repos setting or reading it."#,
    },
    Topic {
        name: "kc_trace",
        summary: "call and relation tree around a symbol",
        text: r#"Call and relation tree around a symbol, one hop by default (depth up to {max:kc_trace.depth}): callers (in), callees (out),
conformers, protocol implementations (witness), calls through a protocol (via_protocol), and in C#, calls through a
service the DI container binds to one implementation (via_di, with where it is registered), which out traces follow
instead of stopping at the interface. Every edge has a confidence and evidence; by default only exact and unique_name
edges are expanded (see `evidence` for the `evidence` policy).

Inward, a controller action continues across HTTP routes to the code creating a contract that requests it, in any
indexed repo: edge `http`, with `http` {verb, path, contract, link, call} (call: the node requests the route itself, by
service and path or an HTTP client URL; link: exact, dominant or ambiguous; an ambiguous link makes the node ambiguous)
and `repo` on nodes in another repo. A message type continues inward, and a method publishing one outward, to the
consumers handling it (C# message bus), in any indexed repo: edge `bus`, with `bus` {message, queue, link} (link: exact
or ambiguous), the node being the consumer's Consume method; from a publishing method, `site` is the publish call, with
`site_repo` naming its repo when it isn't the node's. SQL routines and views trace out to the tables they read and write
(edges reads, writes); a SQL table, view or procedure continues inward to the code using it (edge `sql`, as in
kc_impact).

Answers as an indented tree of text: each symbol under the one it relates to, with the relation read from its parent
(`calls`, `called by`, `requested by (http …)`), confidence and site; `format: json` for the envelope. Filter with
`edges` (calls with via_di, via_protocol, dispatch; instantiates, uses, reads, writes, renders, conforms_to, conformer,
witness, via_di, http, bus, sql) and `kinds` (method, function, class, property, … or type and func; the walk goes
through hidden kinds).

A call made through an interface member, its base interface's (`Impl : IDerived`, `IDerived : IBase`, the member on
`IBase`), or the abstract or virtual base-class member an override overrides reaches the implementation (edge
`via_protocol`). Depth goes up to {max:kc_trace.depth}: asked deeper, the answer says it walked {max:kc_trace.depth} (`depth: {asked, walked}`). Nodes the
tree lists without growing that lead further (at the depth, or reached only by an ambiguous edge) are its `frontier`
({qualified, file, line, hop, reason, beyond, next}: `next` is the exact kc_trace call going on from it), with an
`incomplete` entry (reason `depth`, or `policy` for ambiguous edges) saying how to go on."#,
    },
    Topic {
        name: "kc_path",
        summary: "how one symbol reaches another",
        text: r#"How symbol `from` reaches symbol `to`, possibly across repos (`to_repo`; without it, the repos defining `to`
when `repo` doesn't, up to 8; more are named in the error, to pick one): the best {min:kc_path.paths}-{max:kc_path.paths} paths (`paths`, default {default:kc_path.paths}), each a
list of steps from `from` to `to` with the edge reaching each step (calls, via_di: a call through a DI-bound service to
the implementation, dispatch: an interface member to its one implementation, instantiates, uses, reads/writes: Entity
Framework access to a DbSet, or SQL reading a table, sql: a DbSet to its SQL table or a procedure call to its SQL
routine, http: a request to the ASP.NET action serving it, with {verb, path, contract, link}, bus: a publish to a
consumer, with {message, queue, link}, ngrx: an NgRx action dispatched to an effect handling it, member: between a type
and its members), its confidence and site (file:line of the call, request or publish), and `repo` on every step.

By default only exact and unique_name edges (a dominant HTTP or SQL link counts as unique_name) are followed; with
`allow_ambiguous` ambiguous edges are too, and paths with none rank first. Ranked by weight: exact edges 2, unique_name
3, ambiguous 10, plus 1 for a type use or instantiation, or for an http or sql link that is dominant or unique_name; 1
between a type and its member. Found by bidirectional search from both ends within `max_hops` (default 8),
`max_nodes` (default 2000) and `max_repos` (default 16); `explored` counts the symbols whose edges were read. No path:
`paths` is empty, meaning no confident path within `max_hops` unless `allow_ambiguous`, and `next` is the call
looking further (more hops, or ambiguous edges too); `truncated` says a node or repo budget stopped the search first,
with a `capped` entry whose hint is the call raising it. Answers as indented text by default (each step under the one before, with the
relation reaching it); `format: json` for the envelope. `edges` limits the edge kinds a path may use; `evidence` as in
`evidence`."#,
    },
    Topic {
        name: "kc_flow",
        summary: "what happens end to end from an entry point",
        text: r#"What happens end to end from an entry point: an Angular route path (resolved to the component it shows), a
component (by name, selector like app-order-list, or its .ts or template file: its methods, constructor, field
initializers and inherited members), a controller action, a method (`Type.member`), or a file:line.

Follows, across repos: calls to methods of other types, calls through a DI-bound service to the implementation (via_di;
C# registrations and Angular providers: useClass, useExisting, useFactory, InjectionTokens, an abstract class's one
providedIn-root subclass; when several providers are in scope, such as one per route, each is an ambiguous via_di step
bound at its provider), services a component injects whose construction runs code (constructs; a providedIn-root one is
marked singleton; callbacks a field stores are not construction, and an observable's operators are marked lazy),
base-class calls through this.field re-resolved with the subclass's field type, an NgRx effect's branch on its action's
value narrowed to the one a dispatch names (else each marked one of the branches), template children from library
repos, components a method opens as dialogs (dialog), the components and directives a component's template uses
(child, `children` levels deep, default 2, 0 for none; a child leading nowhere is counted as quiet_child; dialogs are
always followed), NgRx actions to the effects handling them (ngrx), HTTP requests to the ASP.NET action serving them
(http), publishes to consumers (bus), Entity Framework reads and writes of DbSets (reads, writes, uses), DbSets to their
SQL tables and procedure calls to SQL routines (sql).

Calls to the caller's own type, to free functions and to interface members (which run as their one implementation,
dispatch) are followed through without listing them. So are logging (a type, file or method with Log as a word of its
name, or Logger or Logging in it; files under a Logging directory), mapping (types or files *Mapper(s), *Mapping(s),
*Profile, *Converter, *Formatter; Map, MapTo, MapFrom, ProjectTo, Adapt) and utilities (types or files *Extensions,
*Helper(s), *Util(s), *Utility, *Utilities, *Constants): counted in `hidden`, and a step reached through them names them
in `through`. Left out and counted in `left_out`: ambiguous edges, test code, and transport (the gateway SendAsync or
Publish call at the site of an http or bus edge).

`steps` are numbered depth first (index, parent, depth), each with repo, symbol, file:line, the edge reaching it, its
confidence and site; each symbol appears once, unless the depth limit (`depth`, default {default:kc_flow.depth}, up to {max:kc_flow.depth}) cut it where first
reached and it is reached again nearer the entry, where it is grown. A step at the depth limit that leads on has `stop`
({reason, beyond, next}: `next` the exact kc_flow call going on from it), and the answer an `incomplete` entry (reason
`depth`). `limit` caps the steps (default 40), `max_repos` the repos opened (default 16); a flow they cut has a
`capped` entry whose hint is the call raising them. Answers as an indented tree of text by default; `format: json` for the envelope. `edges`
limits the edge kinds followed (calls, reads, writes, uses, http, bus, sql, ngrx, dialog, child); `evidence` as in
`evidence`."#,
    },
    Topic {
        name: "kc_impact",
        summary: "what depends on a change, by proximity",
        text: r#"What depends on something you change. Give one of: `symbol`, `file`, or `diff` ("working" for uncommitted
changes, or a git revision). One hop by default (the direct dependents); pass depth 2-{max:kc_impact.depth} for more.

Each affected symbol is classed by proximity: `direct` (hop 1), `near` (2), `far` (3+), or `test` (test code at any
hop, which never sets `nearest`). Proximity is graph distance and nothing more: it says how many edges separate a
dependent from the change, not how much the dependent matters, how often it runs, or whether the change breaks it.
`nearest` is the nearest class present (a non-test dependent in another repo counts as `direct`); `counts` counts each
class; `low_confidence` counts the dependents reached through an edge that isn't exact. `worst` is deprecated (one
release): `nearest` in the old names (critical, high, medium, low), kept for scripts that read it; it never measured
risk either.

Across a library boundary the walk goes on: a method's or property's calls in repos depending on its repo, and for an
implementation of an interface member a dependency declares, the calls through that member in the dependency and the
repos using it (ambiguous: another implementation may run), each with its `repo` and `library` (the repo crossed, and
`through` the interface). Looked up for at most 64 walked symbols (then `incomplete` says how many it left). Also:
cross-repo dependents the walk doesn't list (`cross_repo`; for an interface or base class, or an interface member, its
implementations and subclasses, or implementing members, in repos depending on its repo: kind `inherits` or
`implements`; a type's uses and creations). Each dependent is listed once, by its best way: a confident edge over an ambiguous one, then the nearest
hop; when an ambiguous way reaches it nearer, `nearest_ambiguous_hop` says so and its class goes by that hop. An
ambiguous link counts as low-confidence and, under the default evidence policy, isn't walked past (`evidence: all`
walks past it, marked ambiguous). A dependent reaching what it was reached from on several lines gives the first as
its `site` and the rest in `also`. In text a dependent leads with its site, the line to open, then its other sites
and the line it is declared at (`a.cs:12  [exact] direct (also a.cs:30) (declared at line 9)`); `trace` names callers
the same way. A dependent the walk reaches again through another symbol, at lines its row doesn't name, keeps that
way in `ways` ({via, hop, confidence, site, also}; in text, last on its row: `(also via Store.Drop [exact] at a.cs:40)`); a capped text
answer leaves those out first, and says how many. When the walk stops at the limit (`limit`,
default 30; `truncated`), `counts_cover` is "shown": the counts are of the affected symbols listed, not all.

Where the walk stops: the walk goes up to {max:kc_impact.depth} hops (deeper asked: `depth: {asked, walked}`), its budget the symbol
limit. Listed symbols it didn't walk past that have dependents it doesn't list are its `frontier` ({qualified, file,
line, hop, reason, beyond, next}: `depth` at the depth reached, `ambiguous` reached only by ambiguous edges; `beyond`
counts the dependents past it, at least; `next` is the exact kc_impact call going on from it), each kind with an `incomplete` entry (reason `depth` or `policy`) naming
the first and how to go on: a deeper depth, `evidence: all`, or kc_impact on one of them; a walk the limit cut
counts its stops without checking them. Calls through a base
interface's member or an overridden base-class member reach an implementation as calls through its own interface
do. `registrations` lists the DI registrations of the types on the path ({type, role (implementation or service),
service, implementation, lifetime, file, line, hop}): where the container makes them, which no reference reaches.

Answers as an indented tree of text (each dependent under the symbol it depends on, `called by`, `used by`, …);
`format: json` for the envelope. Within a hop, dependents come by how tied their files are to the changed files
(personalized PageRank over the reference graph; `order: hop` for the walk's order). Filter with `edges`, `kinds` and
`evidence` as in kc_trace. Crossings into other repos and other languages (HTTP, bus, SQL, Ansible, CI deploys,
Terraform): `kc_impact.crossings`. Files history couples to the change, warnings about a diff, and removed definitions:
`kc_impact.history`."#,
    },
    Topic {
        name: "kc_impact.crossings",
        summary: "kc_impact: HTTP, bus, SQL, Ansible, deploy and Terraform crossings",
        text: r#"A controller action continues across HTTP routes to the code creating a contract that requests it
(frontend request contracts, gateway DTOs) or requesting it by path (gateway calls naming the service, HTTP client URLs),
in any indexed repo, and on to its callers, one hop for the route: edge `http`, with `http` {verb, path, contract,
link, call} and `repo` when in another repo.

A message type, or a method publishing one, continues to the consumers handling it (C# message bus) in any indexed
repo, one hop: edge `bus`, with `bus` {message, queue, link}; a consumer's own changes don't reach its publishers.

A SQL table or view reaches the procedures, views and triggers reading or writing it (edges reads, writes, uses), a
procedure the routines calling it, and both continue to the code using them in any indexed repo, one hop: the methods
reading or writing an Entity Framework entity mapped to the table (or naming it in SQL text), in the database that
definition is in, or calling the procedure: edge `sql`, with `sql` {object, how (entity or call), entity, mode, link};
code linked to another repo's copy of the same schema.name counts. A SQL object dumped in several places is listed once,
with `copies` (file:line of the others).

An Ansible variable reaches the tasks and templates using it, a template the tasks rendering it, a handler the tasks
notifying it, a task the play, role or file running it (edge `runs`); a role continues to the plays and roles using it
in other repos, one hop: edge `role`, with `role` {role, link}. A GitLab CI job or shared template, an Ansible
playbook, or a role continues to the CI job of each application repo whose pipeline deploys with it (resolved through
includes, extends and child pipelines): edge `deploy`, with `deploy` {playbook, app (the application name passed),
link}.

A Terraform block reaches the blocks and locals of its directory using it (edges uses, depends_on, provider, sets); a
module's output reaches the blocks reading module.m.out in the calling configurations, and a module variable the calls
setting it, in this repo through a local source and in other repos one hop through a git or registry source: edge
`module`, with `module` {module, source, link}."#,
    },
    Topic {
        name: "kc_impact.history",
        summary: "kc_impact: coupled files, missed partners, removed definitions",
        text: r#"`coupled` (a class of its own, never a proximity: it isn't reached by any reference) lists files of any kind
but docs (config, scripts, Terraform, templates, code; `file` may be any tracked file too) that git history couples to
the changed files (changed together in at least 2 commits and 30% of the changed file's) and that no reference links
to them (the walk didn't reach it and the changed file names none of its types), with support, confidence and the
changed file `with`. `as_of` (a commit, with its ancestors, or a date) and `since` (`<n>d`, a date or `all`; default
365 days) set the history it reads, as in kc_coupled.

With diff, `missed_partners` warns about usual partners the diff leaves alone (at least 3 shared commits and 70% of the
changed file's; source files only, not docs or extension-less files): "you changed A; B changed with it in 8 of 10
commits". With diff, definitions the change deletes, renames or cuts are read from the base commit: `removed` lists
each with `named_by` (the code still naming it), and that code is affected at hop 1 with `via_removed: true`; matched by
name, so approximate (`pre_change`)."#,
    },
    Topic {
        name: "kc_unused",
        summary: "dead-code candidates",
        text: r#"Dead code: symbols (C# and TypeScript types, methods, functions, properties) nothing reaches, for a repo.
Each finding has a tier: `unused` when every reference to its name, in the repo and in indexed repos depending on it,
resolves elsewhere (exact, or to a framework member) and no other occurrence of the name could mean it;
`possibly_unused` with `reasons` when only uncertain evidence names it: ambiguous or unresolved references, tests only,
nameof, strings (reflection, routes), config, templates, Markdown docs, a member that may implement an interface
declared outside the index, an attribute that may register it, or public API of a library or of a package other repos
may use. Never reported (counted in `exempt` by reason): entry points (Main, Program, Startup), controllers and
actions, tests, consumers, overrides, interface implementations, framework conventions (Configure*, OnModelCreating,
Dispose, Angular lifecycle hooks, event handlers), Angular-decorated classes and members, NgRx effects, DbSets, data
properties of DTO-like or entity types, generated code and migrations. Preview only: nothing is edited. `library` and
`published` say why public API is held back; `dependents` lists the indexed repos whose references were checked.
`tier` picks `unused`, `possibly` or `all` (default); `kinds` the kinds checked."#,
    },
    Topic {
        name: "kc_rename_preview",
        summary: "every site a rename would edit",
        text: r#"Preview renaming a symbol (C# or TypeScript), without editing anything. `items` lists every edit site in the
repo: file, line, col, kind (definition; interface_member, implementation and override for members renamed with it,
listed in `group`; constructor; reference; import; nameof; cref; string, a literal holding exactly the name; template,
an Angular binding; config, a JSON or YAML key; route, an action whose URL is built from its name) and confidence
(exact, unique_name, ambiguous). `cross_repo` lists sites in indexed repos depending on this one, each with its repo.
`collisions`: symbols already named new_name in the type, its hierarchy, or (for a type) the repo. `mentions`: comments
and prose strings naming it, not edited. `refused` is true, with `reasons`, when any site is ambiguous, the name
collides, the member may override or implement one declared outside the index, or the size cap cut its sites (a
partial preview can't be applied as it is: get the rest with its cursor); pass allow_ambiguous to see the preview
anyway. Sites are occurrences, matched to references by line and column: of two calls of the name on one line, only
the one resolving to the symbol is a site; one whose receiver can't be resolved is an ambiguous site. Overloads are
separate symbols: name one by file:line."#,
    },
    Topic {
        name: "kc_coupled",
        summary: "files that change together (git history)",
        text: r#"Files that historically change in the same commits as a file, from local git history (365 days before HEAD
by default; a file unchanged in that year is read over its own last 50 commits, however old; `as_of` ends the history
at a commit, with its ancestors, or a date, so an answer can't see later commits (partners are the files of that
commit's tree; one gone since has `exists_now: false`); `since` sets how far back: `<n>d`, a date or `all`; `history`
in the answer says which span was read; following renames; commits of more than 50 files, automated commits (CI jobs,
bots, nightly dumps) and pairs only ever created together don't count), ranked by shared commits, the most recent
first among equals; a partner that isn't code and shared under a fifth of its own commits with the file (a rule or
settings file that sweeps touched) comes after the rest. The file can be any file git tracks, indexed or not (a
Dockerfile, Terraform, a shell script, YAML, Markdown), and so can its partners, except generated files (lockfiles,
minified bundles), binaries and hubs (a file that isn't code, changed in 10+ commits and half the repo's, changing with
this one in under a fifth of its own commits and under half of this one's, like a changelog). Each partner: kind (its
language, or template, style, javascript, config, docs, script, infra or other), support (shared commits), confidence
(the share of the file's commits that also changed it), commits (the file's), weight (recency: the shared commits
weighted by age, half per 90 days), test, and static_link (false: kc_impact doesn't reach it and the file names none of
its types: a hidden coupling), and basis `history`. Use it to find the files a change to this one usually needs.

When history has nothing to rank (no partner shares two commits with the file: a new file, a quiet one, or `as_of`
before its history), the item's `note` says so and `structural` lists files related to it by structure instead, a
guess, never mixed into `partners`: each with basis `structure`, confidence `low`, and `relations`, each with its own
`evidence` (`uses` / `used_by`: a name only one of the two declares, name_unique; `implements` / `implemented_by`:
a supertype, declaration; `test_named` / `subject_named`: a test named for it, filename_guess; `same_directory`,
convention). With `as_of` the structure is that commit's: files changed since are read at it, so nothing later is
seen. `structural` is a sample (the strongest few, `structural_total` counts all)."#,
    },
    Topic {
        name: "kc_tests",
        summary: "the tests to run for a change",
        text: r#"The tests to run for a change. Give targets (changed files or symbols) or diff ("working" or a git revision). Ranks
test files by how likely they exercise the change, from reverse edges into test code (up to 5 hops; ambiguous edges
are followed too unless `evidence` says otherwise, and rank low), naming (FooTests/foo.spec.ts/test_foo.py for Foo)
and co-change, which adds weight but never decides alone. `tests` (described by `tests_are`): the most likely
relevant, not every test that matters, each with confidence (0-1), reasons ("FooTests.Bar calls Foo.Baz at hop 1",
"named for src/Foo.cs", "changed with src/Foo.cs in 6 of its 9 commits"), the test methods reaching the change and the
raw signals. `left_out` ranks the other candidates (`left_out_total`, `tests_total` count them): they may fail too, so
none is safe to skip. Changed test files are selected outright, except helpers with no tests in them (base classes,
fixtures), which are left out."#,
    },
    Topic {
        name: "kc_hotspots",
        summary: "files that keep hurting (churn, complexity, health)",
        text: r#"Files that keep hurting, from local git history and the index: weighted churn over 30/90/365 days ending
today (`as_of` moves the end: unix seconds, YYYY-MM-DD or "head"; `history` says when the windows end, whether the repo
is dormant, whether the clone is shallow and how many automated commits were left out), recurring churn, bug-fix ratio
and time-weighted fix score (recent fixes count most), author count, complexity, max nesting depth, function count and
longest function, fan-in/out, co-change coupling (the file most often changed in the same commits, and how many files
are strongly coupled) and code-health biomarkers counted by kind, combined into concern, risk (0-100) and health
(1-10). With workspace=true, repos ranked by health. With path, one file's vitals and its biomarkers, each with a reason
and lines: bumpy_road (a function with 2+ separate blocks nesting 2+ deep), deep_nesting (4+), complex_conditional (a
branch condition with 2+ && / ||), brain_method (70+ lines, complexity 15+, nesting 3+, 20+ distinct names referenced),
large_file (1000+ lines), low_cohesion (a C#/TS class whose methods split into 3+ groups sharing no field or call,
LCOM4)."#,
    },
    Topic {
        name: "kc_ownership",
        summary: "historical contribution (lines added) and surviving lines",
        text: r#"Historical contribution, from local git history (names and emails already in it, after .mailmap;
automated commits from CI jobs and bots left out), for a file, a directory or the whole repo:

- `top_contributor`: the author with the largest share of lines added; `contributors`: the largest few, each with its
  share of lines added, commits and last commit (`contributors_total` counts them all);
- `truck_factor`: the fewest authors whose lines added exceed half the total;
- `contributors_gone`: the share of lines added by authors listed as former (`former_authors` in the user config dir),
  and `contributors_possibly_gone`, adding authors with no commit in the repo for 6 months before its newest commit (an
  inference).

All of this counts history: lines added and since rewritten or deleted still count, so it is not who wrote the code as
it is now, and nothing here measures who understands it today. For a file, `surviving` gives its current lines by who
last changed them (git blame of the working tree; `uncommitted` and `automated` lines counted apart): the code as it is.
Directories and repos don't get it (a blame per file); that is a follow-up.

A directory or the repo also lists its areas (directories `depth` deep, `top` of them). `without` simulates authors
leaving: the files they are top contributor of (`top_contributor_files`, `top_contributor_lines`, the largest in
`files`) and `contributors_gone` before and after (`gone_before`, `gone_after`). A shallow clone only knows history
since its boundary (coverage.shallow)."#,
    },
    Topic {
        name: "kc_history",
        summary: "one symbol's git history: its commits, first and last change, writers, files changed with it",
        text: r#"One definition's git history, read over the lines the index says it spans (the method, not its file):
`commits`, every commit that changed those lines, newest first (`git log -L`, which follows them through edits and
renames of the file; whitespace changes count), each with sha, date, author (after .mailmap) and subject; `last`, the
newest; `first`, the oldest, given only when the whole history was read; `commits_total`. `writers`: who last changed
its current lines (git blame of the working tree, whitespace ignored; `writers_uncommitted` and `writers_automated`
counted apart). `changed_with`: the files changed in those commits, by shared commits (`changed_with_total`; commits of
more than 50 files left out and counted in `sweeps`). `callers_changed_with`: its direct callers (kc_impact's hop 1,
in this repo, calls through an interface it implements included) whose files changed in those commits, each with how
many (`shared`), the newest few, `test` for test code and `ambiguous` when the call may run another implementation;
`callers_total` counts every direct caller, those in its own file included (they aren't listed: each of its commits
changes that file). `since` keeps commits since a date (`2025-01-01`, `90 days ago`).

Give the symbol as a name, `Type.member` or file:line; when several definitions match (overloads), the first is read
and `others` lists the rest: ask with file:line for another. Uncommitted edits that move the lines are allowed for
(`lines_at_head`); a definition whose declaration line isn't committed as it is has no history. Bounded: at most 200
commits, and the MCP server stops git at the call's time budget, keeping what it read; either is said in
`incomplete`. A shallow clone only knows history since its boundary (`knowcode history deepen`)."#,
    },
    Topic {
        name: "kc_reviewers",
        summary: "who could review a change",
        text: r#"Who could review a change: give files or diff ("working" or a git revision). Authors ranked by recent
contribution to the changed files (lines added, each halved every 180 days), with how many of the files they added
lines to and their last commit there. Changelogs, lockfiles and generated files don't count (left_out). Former and
possibly former authors are left out, and so is the change's author: for files or "working", the repo's user.email;
for a revision, the authors and committers of the commits since the diff's base, with contribution read from the
history up to that base (base), so a change never recommends its own authors. History only: a recommendation, not a
measure of expertise."#,
    },
    Topic {
        name: "kc_overview",
        summary: "what a repo is",
        text: r#"What a repo is: languages, packages and targets, repos it depends on and repos that depend on it (one entry
per repo, with `dependencies` and the manifest `sites` declaring them, consecutive lines joined: `package.json:119-145`),
entry points, key types, top hotspots, and a ranked map (the most central files and the symbols other files use,
within `map_budget` tokens, default 400, 0 to leave it out; `focus` biases it toward a name or path). Repos with Entity
Framework or SQL tables also list their tables (per DbContext, with read and write counts; the 20 most used when there
are more, `tables_total` counting all) and a stored procedure count. With workspace=true, one line per indexed repo
plus repo-to-repo edges."#,
    },
    Topic {
        name: "kc_areas",
        summary: "a repo's functional areas",
        text: r#"The repo's functional areas: its files clustered (Louvain with refinement, deterministic) over resolved
references, co-change from git history and the directory tree, so you can see what the parts of a repo are and which
one owns a file. Each area: id, name (its dominant directory, plus a distinctive file-name word when areas share a
directory), dir, files, lines, cohesion (0-1: the share of its reference and co-change weight that stays inside),
tokens, key_files (PageRank within the area), entry_points, exposed (the files other areas use most) and depends_on
(other areas its references go into, with file_edges). modularity says how clear the split is (above 0.3 is clear);
cochange_pairs 0 means no history helped. With area (an id, name or directory, or a fragment of one name), that area
and every file in it (`members`). kc_context on a symbol or a file path says which area it is in."#,
    },
    Topic {
        name: "kc_notes",
        summary: "notes anchored to symbols (read)",
        text: r#"Notes anchored to symbols (read only): for a symbol, or every note in the repo. Each note: id, symbol
(qualified name when it was added), namespace, kind, file, line, tag (decision, gotcha, todo), text, commit, author,
date, and its status against the code now: `current`; `stale` (the symbol's body changed since the note: `stale_since`
is the first commit that changed it, or `working tree`, and `change` its size, `+3 −1 lines`); or `orphaned` (no
declaration has its identity now: renamed, deleted, or only a same-named one in another namespace or overload, listed
in `candidates` and never attached). A note re-attaches when a declaration with its identity exists again (`moved_from`
when it is in another file); `identity: unknown` marks an old note whose declaration couldn't be found to record its
identity. Read stale and orphaned notes with care: the code they describe has changed. kc_context shows a symbol's
notes; kc_impact lists notes on the symbols a change affects."#,
    },
    Topic {
        name: "kc_note_add",
        summary: "attach a note to a symbol (writes local state)",
        text: r#"WRITES LOCAL STATE: attaches a note to a symbol, stored in KnowCode's data directory on this machine (never
in the repo or the cache, never sent anywhere). Use it for a short, durable fact a later reader of this symbol needs and the code
doesn't say: a decision and its reason, a gotcha, a todo (`tag`). It records the declaration's identity (namespace or
module, qualified name, kind, C# parameter types), file, a hash of its body, HEAD and the git user.name, so the note
shows as stale when the body changes and orphaned when the symbol is gone. The symbol is the first definition, as
kc_context picks it. Answers the note as kc_notes does. Notes are removed with `knowcode note rm <id>`."#,
    },
    Topic {
        name: "kc_search",
        summary: "ranked text and regex search",
        text: r#"Search file contents across repos (literal by default, or regex: Rust syntax, plus a look-ahead ending the
pattern or a look-behind starting it, `\bSave\b(?!\()`, `(?<!\.)Save`; matches never span lines),
like a ranked grep over an index: definitions of matching symbols first, then declarations, other code, then tests,
comments and docs. Matches are per line. Each item is a text row, `repo/file:line kind symbol | text`: `repo/` when
the answer spans repos; kind def, decl, code, test, comment or doc; the symbol a def or decl declares, or `in X` for
the one enclosing the line, with its parameter names on the first code hit inside a method or function (`in
Orders.Place(order, user)`); `[stale]` for a file that could not be read; the trimmed line after the first ` | `. The
rest of the hit's statement follows it, one per line (`  41- text`): the chained call a `.then(` line continues, the
`.map(` continuing it, the `case` label it runs under; with `context`, the lines around it instead. With
`files_only`, a row per file, `repo/file:line best (n matches)`. `case`: sensitive (default), insensitive, or smart
(insensitive unless the pattern has an uppercase letter); `word` for whole words. Filter with `lang` (swift, python, rust, markdown, json, yaml,
toml, shell, …, or an extension: `ts`, `cs`) and `paths` globs (a leading `!` excludes; without a `/`, a glob matches file
names: "*.swift", "!*Tests*"; after a repo's name, `shop/src/**` or `**/shop/src/**` searches that repo only; an absolute
path, the repo it is in). A filter matching no file is refused, saying so (`paths matched 0 files (…)`), never answered as
a missing match. `context` lines around each hit (max 10). A first page at the default `limit` shows every hit in code
(definitions, declarations, code lines) when there are at most 60 hits and they fit `max_chars`; hits in tests,
comments and prose past the page stay a page away (all of them when they are all there is). A first page cut short names the other files with a match in
`other_files`, by directory, with a count when more than one (`src/core/: a.cs 2, b.cs`; up to 60 files, past that
`files` counts them and `files_only` lists them); with every file named, the answer is `complete` and the other lines
are a `more` entry with the cursor. Page with `offset` (`next_offset`) or kc_more with `cursor`. The scan stops at
`max_lines` matching lines (default 50000) at the same file on every run, saying `capped`; unreadable files are an `incomplete` entry (failed). A search past its deadline
(KNOWCODE_SEARCH_DEADLINE, 120 s) answers `complete: false` with an `incomplete` entry of reason `deadline`.
`workspace` searches every indexed repo, `repos` the ones named. Use it for strings, config keys, error messages, TODOs
and anything that isn't a symbol name."#,
    },
    Topic {
        name: "kc_grep_ast",
        summary: "structural search with ast-grep patterns",
        text: r#"Structural code search with an ast-grep pattern, over one language's files (`lang`: csharp, typescript (.ts
and .tsx), javascript, python, rust, swift): code with metavariables that matches syntax, not text, so formatting,
comments and line breaks don't matter. $X matches one node, $$$ARGS any number, $_ one node uncaptured. Examples: "new
HttpClient($$$)", "$T.Result", "$X.Wait()". Or "kind:<tree-sitter node kind>" for every node of a kind, and
"kind:<kind>~<regex>" for those whose text matches the regex.

Code that doesn't parse on its own goes in context with a `selector` (the node kind to match within the pattern parsed
as context): pattern "try { $$$ } catch $$$ { }" with selector "catch_clause" finds empty catch blocks (comments don't
count). Narrow with `inside` / `has` / `not_inside` / `not_has` (a pattern, "kind:<node kind>", "kind:<node
kind>~<regex>", or "kind:<node kind> <context pattern>": the node of that kind within the pattern parsed as context):
subscribe calls inside ngOnInit are pattern "$S.subscribe($$$)" with inside
"kind:method_definition~^[\w\s]*ngOnInit\("; and `text` (a regex over the match). Modifiers in a pattern must line up
with the code's ("async void $M" misses "public async void"): use "kind:method_declaration" with a text regex instead.

Each item is a text row, `repo/file:line:column[-end_line] [$X=captured, …] | text` (`repo/` when the answer spans
repos; the match's first line after the first ` | `). Non-test code first; page
with `offset` (`next_offset`). `paths` globs (a leading `!` excludes; `<repo>/…` scopes to that repo); `max_matches` stops reading files after that many
matches (default 50000), at the same file on every run. `workspace` searches every indexed repo, `repos` the ones
named."#,
    },
    Topic {
        name: "kc_more",
        summary: "the next page of a cut list",
        text: r#"The next page of a list an answer cut. Pass the answer's `cursor` (its `items`) or a list's
`<list>_cursor` (a context's callers, conformers, members, callees, a table's accessors; a trace's nodes; impact's
affected and coupled; a path search's paths and candidate ends; rename sites; search and grep-ast hits) as it is;
every `incomplete` entry for a cut list gives the call. Answers `items` (the page, its rows as the list's own: a
search's are text rows), `list` (which list), `offset`, `total` when known and `cursor` for the page after: following
every cursor gives each row of the list once, in its order. See `cursors`."#,
    },
    Topic {
        name: "kc_status",
        summary: "index status, coverage, config and SCIP",
        text: r#"Index status: graph schema, file coverage (symbolic links skipped: `coverage.skipped_symlinks`), files with
parse errors, freshness (`freshness.failed`: unreadable files, answered as last indexed), extractor versions, HEAD
commit and dirty flag. `config`: the config file (`file`, `exists`), the cache directory with its `source` (env,
config, legacy or default), `notes` (where notes live: `path`, `source`, `count`), the `workspace_roots` (none: the workspace is every indexed repo) and where user-level
ignore rules come from. `scip` (when the repo has a SCIP index): its file and indexer, documents covered (stale:
changed since the indexer ran; unindexed: files KnowCode doesn't index), `coverage.current` (covered files still
unchanged, in projects whose inputs are unchanged too: the ones it applies to), `coverage.stale_projects` (projects
with any compilation input changed since: KnowCode's own resolution answers there), reference sites and
implementations it answers, and, after `knowcode index`, `agreement`: the heuristics against it per confidence (same,
among, other, missed, external_ok, external_false), upgraded and corrected counts. `privacy` (when the repo's
privacy file blocks anything or is malformed): the file's state (read, empty, malformed with why), its global level,
`block_at`, and what is `blocked` ("repo", or the folders and files its rules name); a workspace status also lists the
repos blocked whole. `workspace` for every indexed repo."#,
    },
    Topic {
        name: "kc",
        summary: "the one tool of the default profile: every tool as an `op`",
        text: r#"The default profile (`one`) advertises a single tool, `kc`, so a client carries one schema on every turn.
`op` names the tool to run, without `kc_`: impact, find, refs, context, search, locate, more and help, and every other
tool (trace, path, flow, symbol_at, unused, rename_preview, coupled, tests, hotspots, ownership, reviewers, history,
overview, areas, notes, note_add, grep_ast, status). `q` fills the op's main parameter, `qs` its list of several:

- impact, context, trace, rename_preview, history, notes, note_add: `symbol`; context: `symbols` from `qs`
- find: `query`; locate: `query`, `queries` from `qs`
- refs: `name`, `names` from `qs`
- search, grep_ast: `pattern`
- more: `cursor`; help: `topic` (an op's name is its tool's topic: `impact`, `impact.crossings`)
- path: `from`; flow: `entry`; symbol_at: `file`; coupled: `file`; ownership: `path`; overview: `focus`; areas: `area`
- tests: `targets`, reviewers: `files` (`q` as a list of one, or `qs`)
- unused, hotspots, status: no `q` (an empty one is ignored)

Every other parameter of the op's tool is passed by its own name, as that tool's topic lists it (`kc {op: impact,
q: Orders.Place, depth: 2, repo: shop}`, `kc {op: search, q: "retry", regex: true}`, `kc {op: path, q: A, to: B}`).
Answers name the calls that continue them as ops (`kc {op: more, cursor: "kc~1f"}`); the other topics name tools
as `kc_<op>`, which is `kc {op: <op>}` here. `KNOWCODE_TOOLS=core` or `all` advertises the separate tools instead (see
`profiles`)."#,
    },
    Topic {
        name: "kc_help",
        summary: "this documentation",
        text: r#"KnowCode's documentation by topic: each tool (`kc_context`, `kc_impact`, …) with every parameter, sections of
the large ones (`kc_context.http`), and the concepts every answer shares. No topic: the index of topics. The same
topics are MCP resources, `knowcode://help/<topic>`."#,
    },
    Topic {
        name: "contract",
        summary: "complete, incomplete, provenance: what every answer says about itself",
        text: r#"Every answer, from every tool, says how complete it is and which repos it was read from.

- `complete`: true only when nothing below makes the answer partial for what was asked. Every answer says it, true or
  false; a text answer (a tree) ends its first line with `; complete` when it is.
- `more`: lists on the side of what was asked that a limit or `max_chars` cut, `{reason, detail, hint}` (reason
  `capped` or `budget`): a symbol's callers, callees and members, the tables its calls reach, one reference's candidate
  targets, its source, cites past the first few without their line. The answer is still `complete`; `hint` is the call
  for the rest. The lists an answer is about (its `items`, implementations and conformers, DI registrations, HTTP, bus
  and SQL links, a walk's nodes) are `incomplete` when cut. So is a page of `items` cut, unless its `rest` says the
  rows cut are past what was asked: kc_find's when every match with the name asked for is listed (the rest match by
  prefix or word), kc_search's when every file with a match is named, on the page or in `other_files`.
- `incomplete`: why it is partial, one entry per reason, `{reason, detail, hint}`: `detail` names what is missing (the
  list, with how many of how many were shown), `hint` how to get the rest. Reasons: `capped` (a limit cut a list),
  `budget` (cut to fit max_chars, or a batch leaving inputs out), `deadline` (a time or work budget ran out), `stale`
  (answered from data that doesn't match the working tree), `failed` (files or includes could not be read; what comes
  from them is as last indexed; also a repo, of several the answer spans, that could not be read at all), `missing_repo`
  (a repo the answer needed isn't cloned or indexed, or its checkout was deleted: such a repo is left out of cross-repo
  links and named where it would have joined one, or where the answer spans it), `unsupported` (a
  construct the analysis doesn't model, where known to be present: also one an extractor recognises but doesn't read,
  named with file:line, such as a DI assembly scan, SQL naming its table at run time, a publish of a message of unknown
  type, a Jinja-named include, a Terraform `moved` block, a CI include or Angular lazy route in a form not read),
  `policy` (the evidence policy left edges out; see
  `evidence`), `withheld` (an AI privacy file blocks content; see `privacy`), `depth` (a walk stopped at its depth,
  asked or the deepest it goes, where symbols lead further: its `frontier` names them).
- A summary's samples by design (overview's entry points, key types, hotspots; areas' key files; contributors; the
  ranked map) show the top few with their `<list>_total` and are named in `sampled`: not incomplete.
- `cursor` (and `<list>_cursor` beside a nested list): only on a list that was cut and continues; see `cursors`.
- `provenance`: the repos the answer consulted besides its own (dependencies and dependents opened, repos crossed over
  HTTP, the bus, SQL or Terraform, repos whose workspace summary an item came from), each `{repo, commit, generation,
  dirty, fresh}`; `fresh` says the index was checked against the working tree for this answer and matched it.
- Every edge, reference, trace or impact row, path or flow step carries `confidence` (exact, unique_name, ambiguous,
  external_likely, unresolved) and `evidence` (see `evidence`). Ambiguous edges are listed, never guessed.
- `cite`: on every row with a location (trace nodes, impact rows, path and flow steps, references, a context's callers
  and callees), the source lines it rests on, read from the working tree when the answer is built: an edge's site, with
  the rest of its statement when it goes on over other lines (at most 2 each side; `at` is then the range and `cut`
  says where a longer one starts or that it goes on), else the line naming the declaration. `{at, text, cut, repo}`:
  `at` is `file:line` or `file:start-end`; `cut` says what a cap left out, or why nothing was quoted. By default
  (`cite: brief`) the first 12 rows quote their line, up to 120 characters, and the rest give `at` only (a `more`
  entry, `capped`, says so); the size cap drops statement lines before any row; `cite: lines` quotes every row,
  a declaration from its first line through its name (at most 3), each line up to 160 characters; `cite: check` gives
  no lines; `cite: none` leaves it out. Text trees print the lines under each row. Every cite's `check` compares the
  file with the index the answer read: `verified` (unchanged), `changed` (edited, the line still names what it cites),
  `moved` (`at` is where it is now, `was` where the index had it), `gone`, or `unchecked`; `drift` says what moved,
  and any drift makes the answer `stale`.
- An empty answer says what was searched (`note`). Older fields agree with `incomplete`: `truncated`, `total`,
  `capped`, `freshness.failed`, `left_out` and `hint`.

The MCP server's answers are compact: repos are named by their short name (what `repo` parameters take; the full path
only when two indexed repos share a name); what goes without saying is left out (false flags such as `dirty` and
`truncated`, nulls, `offset: 0`, a search's `stats`; `complete` stays); a clean, current answer has no `repos` or
`provenance` (no repo dirty or answered from its last sync, nothing `stale`, `failed` or `missing_repo`), and otherwise
commits are 12 hex digits, the queried repo's own provenance entry is left out (the envelope's `repo`, `commit` and
`dirty` say it) and the others are strings (`billing @ 0123456789ab (dirty), from its last sync`); `stale` is one
entry naming repos by name; `evidence` is a string (`"receiver_type [project_unverified]"`: mechanism, then any
assumptions), cursors are short handles, a repo's deploy answer is given once (at `items[0]`), search hits are text
rows (see kc_search), and internal row ids, the build stamp and timings (`ms`, `*_ms`) are left out. Answers are
capped at `max_chars` (default 8000, up to 40000): a list cut to fit has a `budget` entry (in `more` for a list on the
side). The lists an answer is about
(implementations and conformers, DI registrations, HTTP, bus and SQL links, a search's `other_files`) are cut last:
every other list (callers, callees, members) down to none, the source shortened and their rows' detail (signature, doc)
dropped first; links are text rows (`file:line kind symbol`), what they share said once (`links_repo`, …). kc_trace, kc_impact, kc_path
and kc_flow answer as text trees by default: a header line per `incomplete` and `more` entry, with the kc_more call naming the
cursor. kc_context, kc_refs and kc_locate take several inputs (`symbols`, `names`, `queries`, at most 20) and answer
`{"results": [...]}`; inputs that don't fit are listed in `left_out`."#,
    },
    Topic {
        name: "evidence",
        summary: "what an edge rests on, and the evidence policy",
        text: r#"`evidence` says how an edge's target was found (`mechanism`) and what that did not check (`assumptions`).

Mechanisms: `scip` (a SCIP index: the compiler's answer), `import` (an import naming the target's module), `receiver_type`
(the receiver's static type, or the caller's own type), `lexical_scope` (the caller's own type or file declares the
name), `namespace` (the namespaces the file sees narrow same-named types to one, C#), `project_scope` (the caller's
project references rule the others out, C#), `name_unique` (the one definition of the name in scope), `name_dominant`
(one of several preferred by a heuristic: an HTTP handler in the frontend's main backend, a SQL schema),
`name_ambiguous` (several, nothing deciding), `di_binding` (a dependency-injection registration), `http_route` (the
route a client requests matching one an action serves), `bus_message` (a message type published and consumed),
`sql_name` (a SQL object matched by name), `template` (a template naming the component), `convention` (a layout
convention: Ansible role directories, Terraform configuration directories, NgRx actions by name), `filename_guess` (a
module placed by its file name, no config mapping the import), `declaration` (written in a declaration: a base list,
an interface member's implementation, containment).

Assumptions: `project_unverified` (C#: the caller or target is in no project file), `directory_convention` (a directory
layout stands in for configuration), `same_deployment` (an HTTP route matched by verb and path reaches the repo serving
it, unchecked), `same_destination` (a message reaches the consumer on its virtual host or queue, unchecked),
`same_database` (a SQL object matched by name is in the database the code uses, unchecked), `revision_unverified` (the
dependency's checkout is the revision asked for, unchecked), `configured_host` (an HTTP client reaches the repo through a
host configuration maps to it; which environment's applies, unchecked), `operation_name` (a generated client's operation
is the action of that name; the API description wasn't read), `server_unverified` (nothing says which server the
client calls: every action of the operation's name is a candidate), `url_local` (a request's URL is a `const` of its
function, read from its declaration; the request's `local` says where), `client_unread` (a method of an external client
the repo only declares, in a `.d.ts`: its code wasn't read, so the route is matched by the method's name, its
`…Legacy` twin too), `package_code` (a declared client's method requests the route its package's code shows, read
from the repo's node_modules at the version its lockfile pins; the link's `package` says which method, where).

Evidence policy: kc_trace, kc_impact, kc_path and kc_flow take `evidence`, which edges to follow:
- `default`: exact and unique_name edges followed, ambiguous ones listed, not followed;
- `confirmed`: a compiler's or an explicit link only (scip, import, receiver_type, lexical_scope, namespace,
  project_scope, di_binding, http_route, bus_message, template, declaration): no name match, convention or ambiguous edge;
- `all`: every edge, ambiguous ones followed too;
- mechanisms alone (`scip,receiver_type`) follow only those; a preset changed adds or takes away (`confirmed,name_unique`,
  `all,-filename_guess`); `-<assumption>` leaves out the edges making it (`default,-same_deployment`).
Under any policy but `default` the answer has `policy: {evidence, excluded, excluded_by}`, counting the edges left out
(a lower bound: nothing past them was read), and is incomplete (reason `policy`)."#,
    },
    Topic {
        name: "cursors",
        summary: "continuing a cut list with kc_more",
        text: r#"A list an answer cut and can continue has a cursor: `cursor` for the answer's own `items`, `<list>_cursor`
beside a nested list (`items[0].callers_cursor`). Pass it as it is to kc_more for the next page, which has its own
`cursor` until the list ends; the `incomplete` entry for the cut list names the call. Following every cursor gives each
row of the full list exactly once: in the list's order for lists cut as a prefix, and for a walk's ranked list
(impact's `affected`, a path search's `paths`), the rows shown first and then the rest of the walk, ranked the same
way. A cursor is refused when the index, HEAD or the rows delivered so far changed since (`stale cursor`): ask the
original question again.

Over MCP a cursor is a short handle (`kc~1f`) the server maps to the full cursor; it lasts while the server runs, for
the last 4096 cursors. `kc_search` and `kc_grep_ast` also page with `offset` and `next_offset`."#,
    },
    Topic {
        name: "privacy",
        summary: "AI privacy files and the `withheld` reason",
        text: r#"A repo can carry an AI privacy file (`.knowcode-privacy.yml`, or the name `privacy.file` configures) naming
content AI tools must not read. KnowCode honours it: it never indexes, shows or searches blocked files, a repo blocked whole is refused, and a dependency on one
is named only as blocked. An answer that may have drawn on blocked content is incomplete with reason `withheld`
(detail naming the repo). Don't try to read that content another way. kc_status shows a repo's `privacy` state."#,
    },
    Topic {
        name: "profiles",
        summary: "which tools the server advertises",
        text: r#"The server advertises one tool by default, `kc` (profile `one`): every tool is an `op` of it (see `kc`),
so a client carries one schema and short instructions on every turn, whether or not it defers tools. Answers, the
instructions and the Claude Code plugin's notes and skills then name calls as ops (`kc {op: refs, q: Save}`).
`KNOWCODE_TOOLS=core` (or `knowcode serve --tools core`) advertises the separate core tools instead: kc_find,
kc_locate, kc_context, kc_refs, kc_impact, kc_search, kc_more and kc_help. `all` advertises every tool; a comma list
adds tools by name (`core,kc_trace,kc_tests`). Under those, tools left out are listed in the help index and can't be
called until the server is started with them; kc_find, kc_context, kc_refs, kc_impact and kc_search are marked always
loaded (`anthropic/alwaysLoad`), so Claude Code loads them up front and defers the rest to tool search, among them
kc_locate, kc_more and kc_help. A cut answer's `hint` names the call that continues it."#,
    },
];

/// The parameters every tool shares, described once (their schemas carry no description, to keep each tool's
/// advertised schema short); the server's instructions name `repo`. `max_chars` is described by its bounds
/// (`mcp::ANSWER_CHARS`).
pub const COMMON: &[(&str, &str)] = &[("repo", "Repo to query: a path, or the name of an indexed repo. Default: the server's repo.")];

/// The topic named `name`: as listed, or a tool's without its `kc_` prefix (`context`, `context.http`).
pub fn topic(name: &str) -> Option<&'static Topic> {
    let n = name.trim().trim_start_matches("knowcode://help/");
    TOPICS.iter().find(|t| t.name == n).or_else(|| TOPICS.iter().find(|t| t.name.strip_prefix("kc_") == Some(n)))
}

/// A tool topic's text with its parameters, read from the tool's schema (`schema`: its `inputSchema`), so every
/// parameter is documented here whatever the text names.
pub fn render(t: &Topic, schema: Option<&serde_json::Map<String, serde_json::Value>>) -> String {
    let mut out = format!("{}\n{}\n", t.name, fill(t.text));
    if let Some(props) = schema.and_then(|s| s.get("properties")).and_then(|p| p.as_object()) {
        let required: Vec<&str> =
            schema.and_then(|s| s.get("required")).and_then(|r| r.as_array()).map(|r| r.iter().filter_map(|v| v.as_str()).collect()).unwrap_or_default();
        out.push_str("\nParameters:\n");
        for (name, p) in props {
            let ty = match &p["type"] {
                serde_json::Value::String(s) if s == "array" => "list".to_string(),
                serde_json::Value::String(s) => s.clone(),
                _ => "value".to_string(),
            };
            let req = if required.contains(&name.as_str()) { ", required" } else { "" };
            let common = COMMON.iter().find(|(n, _)| n == name).map(|(_, d)| d.to_string());
            let common = common.or_else(|| crate::mcp::range(t.name, name).map(|r| r.line()));
            let desc = p["description"].as_str().filter(|d| !d.is_empty()).map(str::to_string).or(common).unwrap_or_default();
            out.push_str(&format!("- `{name}` ({ty}{req}): {desc}\n"));
        }
    }
    let subs: Vec<&Topic> = TOPICS.iter().filter(|s| s.name.strip_prefix(t.name).is_some_and(|r| r.starts_with('.'))).collect();
    if !subs.is_empty() {
        out.push_str("\nSections:\n");
        for s in subs {
            out.push_str(&format!("- `{}`: {}\n", s.name, s.summary));
        }
    }
    out
}

/// `text` with each bounds placeholder filled from `mcp::RANGES`: `{max:kc_impact.depth}`, and `min`, `default` and
/// `span` likewise, so the prose says the bounds the answers apply. Any other braces are left as they are.
pub fn fill(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('{') {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        let filled = tail.find('}').and_then(|end| {
            let (which, param) = tail[1..end].split_once(':')?;
            let (tool, name) = param.split_once('.')?;
            let r = crate::mcp::range(tool, name)?;
            let v = match which {
                "min" => r.min.to_string(),
                "max" => r.max.to_string(),
                "default" => r.default.to_string(),
                "span" => r.span(),
                _ => return None,
            };
            Some((v, end + 1))
        });
        match filled {
            Some((v, n)) => {
                out.push_str(&v);
                rest = &tail[n..];
            }
            None => {
                out.push('{');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The index: every topic with its line; tools the server doesn't advertise (`hidden`) are said so.
pub fn index(hidden: &[String]) -> String {
    let mut out = String::from("KnowCode help topics (kc_help {topic}):\n");
    for t in TOPICS {
        let off = if hidden.iter().any(|h| h == t.name) { " (not advertised by this server: see `profiles`)" } else { "" };
        out.push_str(&format!("- `{}`: {}{off}\n", t.name, t.summary));
    }
    out
}
