# Languages and frameworks

What KnowCode extracts, what it resolves, and what it does not. All extraction is syntactic (tree-sitter, or a
scanner for SQL); names are resolved at query time against the whole index. Nothing is compiled or run. Where a
compiler-written SCIP index is present, it takes precedence (see [usage](usage.md#precise-references-from-scip)).

A construct KnowCode doesn't model usually means a reference it can't resolve, not an error. When it knows the
construct is there, the answer says `unsupported`. When it doesn't, the answer can be `complete` and still miss
edges: the gaps below are the places to check by hand.

## Summary

| Language or format | Files | Symbols and references | Resolution |
| --- | --- | --- | --- |
| C# | `.cs`, `.csproj` | types, members, calls, instantiations, type and member uses | namespaces, `using`s, project references, receiver types, DI |
| TypeScript, TSX, JavaScript | `.ts`, `.mts`, `.cts`, `.tsx`, `.js`, `.mjs`, `.cjs`, `.jsx`, `tsconfig*.json`, `jsconfig.json`, `package.json` | the same, plus Angular templates (`.html`) | imports, tsconfig `paths`/`baseUrl`, receiver types, Angular DI |
| Swift | `.swift`, `Package.swift`, XcodeGen `project.yml` | types, extensions, conformances, members, calls, attributes | receiver and caller types, supertypes, protocols |
| Java | `.java` | types, members, calls, instantiations, type and member uses, imports | packages, single-type, on-demand and static imports, receiver types |
| Python | `.py` | definitions, calls, imports | imports (relative and absolute) |
| Rust | `.rs`, `Cargo.toml` | definitions, calls, uses | by name and receiver within the repo |
| Go | `.go`, `go.mod` | types, functions, methods, fields, interface methods, calls, type and member uses, HTTP routes and clients | packages, imports through `go.mod`, receiver types, embedding |
| ColdFusion (CFML) | `.cfm`, `.cfc`, `.cfs` | templates, components, functions, includes, queries | by path, as the engine resolves them |
| SQL | `.sql` | tables, views, procedures, functions, triggers, events | by name, without case or quotes |
| Terraform | `.tf` | resources, data sources, modules, variables, locals, outputs, providers | by address within a configuration directory |
| Ansible | playbooks, roles, task and variable files, Jinja templates | roles, plays, tasks, handlers, variables, templates | as Ansible looks things up |
| GitLab CI | `.gitlab-ci.yml` and included files | jobs, includes, variables, deploy steps | includes across repos, `extends`, variable precedence |
| Host configuration | `appsettings*.json`, `launchSettings.json`, `web.config`/`app.config`, Docker Compose files, `*ingress*.yaml`, HAProxy `.cfg`, nginx `.conf` | hosts only: which a repo serves, which its clients send to | links HTTP clients to backends (see [C#](#c), Hosts) |

Not supported: Kotlin, Ruby, PHP, C, C++, Objective-C, Vue and Svelte single-file components, and any other
language not listed. Their files are still covered by `search` and by history (`coupled`, `hotspots`), but have no
symbols. `unused` and `rename` cover C# and TypeScript only. `grep-ast` covers C#, TypeScript, JavaScript, Python,
Rust and Swift.

A name resolves only to definitions in its own language (TypeScript, TSX and JavaScript count as one). Edges between
languages come only from HTTP routes, the message bus, SQL, dependency injection and templates.

## C#

Extracted: classes, structs, records, interfaces, enums, methods, constructors, properties, fields; calls,
instantiations, type and member uses; attributes; `.csproj` project and package references.

Resolved:

- **Scope.** Names resolve by namespace as the compiler scopes them: the file's namespace and its parents, then
  `using` aliases, then imported namespaces. A type no namespace in view declares is a library's. A file imports
  what its project imports into every file too: the `global using` directives of the project's files (a
  GlobalUsings.cs), the project file's `<Using Include/Remove/Alias/Static>` items, and the SDK's implicit usings
  (Microsoft.NET.Sdk, .Web, .Worker, .Razor, MSTest.Sdk with its `EnableAspireTesting` and `EnablePlaywright`
  features, Aspire.AppHost.Sdk) when `ImplicitUsings` is enabled. A project whose imports can't be read
  (a property setting `ImplicitUsings`, another SDK) rules nothing out for its files, never one confident pick, and
  answers drawing on it say `unsupported` (area `imports`). `Directory.Build.props` is not read.
- **Projects.** A name resolves only to definitions compiled by the caller's project (the deepest `.csproj` above
  the file, or one linking it in with `<Compile Include>` or a shared project's `.projitems`) or by a project it
  references, transitively. Where no project file decides it, a binding is never `exact`.
- **Receivers.** Typed fields, locals and parameters, generic bases, tuples, `base.`, delegates and factories
  (`Func<T>`, `Owned<T>`, `GetInstance<T>()`, Autofac delegate factories), mapper calls, FluentValidation child
  rules, and extension methods in scope.
- **Dependency injection.** Autofac-style and Microsoft DI registrations (`RegisterType<Foo>().As<IFoo>()`,
  `AddScoped<IFoo, Foo>()`, `TryAdd*`, `typeof` pairs, factories, assembly scans, open generics), service descriptors
  handed to the collection (`services.Add(new ServiceDescriptor(typeof(IFoo), typeof(Foo), ServiceLifetime.Scoped))`,
  `ServiceDescriptor.Singleton<IFoo, Foo>()` and its relatives under `TryAdd`, `TryAddEnumerable`, `Replace` or
  `Insert`, with the lifetime they state; keyed ones are said, not read), and a registration
  built from its parts, the way a container registers itself under the services it answers for (`new
  ComponentRegistration(…, new DelegateActivator(typeof(Scope), …), …, new TypedService(typeof(IScope)) …)`, or those
  arguments to `base(…)` in a class deriving `ComponentRegistration`). A call through a
  service bound to one implementation carries it as `via_di`; generic services are one per type arguments. Test
  doubles beside one real implementation don't count as another implementation. `context` lists registrations and
  flags interfaces injected but never registered.
- **Interfaces and overrides.** Calls through an interface member or base member are callers of the implementation,
  marked `through`, `ambiguous` when another implementation may be reached. Implementations in other indexed repos
  depending on the interface's repo are listed. `impact` and `trace` reach an implementation through its own
  interfaces, their base interfaces (`Impl : IDerived`, `IDerived : IBase`, the member on `IBase`), and the abstract
  or virtual member an override overrides, through a receiver the implementation is one of: `IOrders.Find` reaches
  `OrderRepo.Find` through `IRepo<Order>`, `IPlans.Find` doesn't.
- **Type uses.** A type named as a generic argument (`AddBus<IOrders>()`, `Get<IOrders>()`,
  `AddScoped<IFoo, Foo>()`), in `typeof`, `nameof` or a constraint (`where T : IFoo`) is a reference, its `usage`
  saying which (`type_argument`, `typeof`, `nameof`, `constraint`).
- **Constructors.** A creation (`new T(…)`, `new T { … }`, a target-typed `new(…)` whose type the declaration,
  return type or assigned local gives) reaches the constructor its arguments pick: by count, named and optional
  arguments, `params`, and the argument types the site shows; a primary constructor counts as one, and a struct's
  `new S()` is its implicit one. `: base(…)`, `: this(…)` and a primary constructor's base call reach the constructor
  they chain to. A creation more than one constructor fits is `ambiguous` to each, never a confident pick.
- **HTTP.** ASP.NET controller actions (`[Route]` and `[Http*]` attributes, `nameof` and constants resolved; each
  `[Http*]` on an action is a route of its own, and a named argument such as `Name = …` is no template) and
  minimal API endpoints are linked to the requests that reach them: C# `HttpClient` calls with a URL (base addresses
  dropped, constants resolved, interpolated values as route parameters; a base path in a `readonly string` field,
  a URL built in a local and `new Uri(base, path)` are read), RestSharp's `RestRequest`, request classes declaring a
  path and a verb in an attribute, TypeScript and JavaScript requests (below), ColdFusion `<cfhttp>`, and a code
  base's own request-contract and gateway conventions (see [configurable conventions](#configurable-conventions)).
  Links match verb and path and assume `same_deployment` unless the code names the service, or a host decides
  (below). A request whose URL is known only at run time is counted, with where, in the `note` of an action nothing
  is found to request; the note also names the requests nearest by path.
- **Controller prefixes.** A controller with no `[Route]` (or `[RoutePrefix]`) of its own takes the nearest one up
  its base classes, in any file of the repo, as ASP.NET does, with `[controller]` its own name and `[action]` the
  action's (`[ActionName]` read); `[area]` is the class's `[Area]`. A method's lone `[Route]` serves every verb;
  `[NonAction]` methods serve nothing. Conventional routes with a literal template (`MapControllerRoute`,
  `MapDefaultControllerRoute`, `MapRoute`, `MapHttpRoute`, areas aside) place the actions of controllers with no
  attribute route: those with an `[Http*]` attribute and no template, and public methods with no route attribute,
  each with and without the template's trailing parameters.
- **TypeScript and JavaScript requests.** Angular `HttpClient`, `fetch` (its `method`), axios (`axios.get`,
  `axios({ url, method })`, instances), jQuery (`$.ajax({ url, type })`, `$.get`, `$.getJSON`), superagent, and
  wrappers named for HTTP or API clients (`api.get('/orders')`). URLs are literals, template literals and
  concatenations, with constants resolved, a URL builder's path (`apiClient.getUrl('Items')`, `buildUrl(…)`) and
  `new URL(path, base)` after a base address, and locals holding any of them. A URL builder or a `{ url }` option
  given a name (`apiClient.getUrl(url)`, `$.ajax({ url })`) reads it only from a `const` the same function declares
  before the call, bound nowhere else in it (no parameter, no other declaration or assignment), whose initializer
  names no other local holding URL text; a `let` or `var`, a parameter, or a `const` of an enclosing function or the
  module isn't read. A request whose URL is such a `const`, given to the builder, the option or the call itself
  (`fetch(url)`), names the local (`local: url at src/a.js:44`), and its links assume `url_local`. A call to a
  generated API client whose
  code isn't indexed (`getItemsApi(api).getItems()`, `new OrdersClient(base).list()`, `this.ordersApi.getOrder(id)`)
  is an operation, linked to the action of that name under the controller named like the client (`Items`), else
  under any controller, assuming `operation_name`. Only actions in the servers the client's repo is known to call
  count: the repo itself, a repo it depends on, the one repo serving another of its requests or a host its settings
  name, the one repo a tagged operation of its links to. With none known, an untagged operation links to every
  action of its name, `ambiguous`, assuming `server_unverified`. A client the repo declares itself is read through its
  own requests instead. A method of an external client the repo only declares in a `.d.ts` (`declare module
  'media-apiclient'`), called with no tag (`apiClient.getItem(…)`), makes its request in code that isn't read: it
  links to the action of its name and that action's `…Legacy` twin, `ambiguous`, assuming `client_unread`. When the
  package is installed in the repo's `node_modules` at the version its lockfile pins, its code is read instead (see
  [configuration](configuration.md#ignoring-paths)): the call links through the request the package's method makes
  (its own, or that of a method of its class it calls, one call deep), as the repo's own request of that verb and
  path would, assuming `package_code`, with `package` naming the package, version, method and where it is (a
  minified bundle's original source, from its source map). A method making two requests links `ambiguous`; one
  making none that KnowCode reads, or whose URL is built at run time, keeps the name guess, saying so.
- **Minimal APIs.** `MapGet`, `MapPost`, `MapPut`, `MapDelete`, `MapPatch`, `MapMethods` and `Map` (every verb)
  are server routes, under their `MapGroup` prefixes: groups held in locals or chained, nested, and passed to helper
  methods (`app.MapGroup("api/catalog").MapCatalogApi()`, `MapOrders(group)`, up to four helpers deep). Templates may
  be literals, constants, `nameof` interpolations or concatenations of them; route constraints (`{id:int}`) are
  normalised as for controllers. The route belongs to its handler when that is a method group in the file, else to
  the method registering it (a lambda). A method group passed as a delegate (`MapGet("/x", GetItem)`,
  `Subscribe(Handle)`, `Task.Run(Work)`) is a reference to the method in `refs` and `context` callers.
- **Attribute request contracts.** Any class attribute with exactly two arguments, a path (a literal with a `/`, or a constant) and an
  HTTP verb (`X.Post`, `"POST"`) makes the class a client route, whatever the attribute is called
  (`[Endpoint("/orders", HttpMethod.Get)]`). Properties marked by an attribute naming a path, segment, route, URL or
  query with a positive ordinal (`[PathSegment(1)]`) append their values as path parameters, in order. Code creating
  the class requests the route.
- **Hosts.** A client route's URL or its typed client's base address (`AddHttpClient<T>(c => c.BaseAddress = …)`),
  or a URL in its repo's settings (`appsettings*.json`, `web.config`/`app.config`, a compose service's
  environment), names a host. Launch profiles and Kestrel URLs, compose services built from a repo, Aspire project
  resources, Kubernetes ingress rules, and HAProxy or nginx host rules (sending to a backend service a repo hosts,
  serves as a host or is named for) say which repo serves it. A host exactly one other repo serves links a route
  naming it to that repo only (`exact`); among several repos serving a route, the one serving a host the client's
  settings name is `dominant`. Such a link carries `host` (the host, where the client names it, where it is mapped)
  and assumes `configured_host`. Only hosts are read from these files, never other values.
- **Entity Framework.** EF Core and EF6 tables per `DbContext` (`DbSet<T>`, `[Table]`, `ToTable`, entity type
  configurations), and the methods reading and writing each: set access, `Add`/`Update`/`Remove`, `SaveChanges`,
  entity property assignments where the change is saved, navigations (`Include`, `AutoInclude`, lazy loading), bulk
  extensions, and writes through one call deep (`via`). The database a table is in comes from the mapping, the SQL
  text, or the context's connection setting, and says `by` which.
- **SQL in strings.** SQL text in C# strings passed to a call running SQL, or held in a name saying so, is parsed by
  the SQL scanner. Names built from a value (`FROM {table}`) are left out.
- **Stored procedures.** `EXEC` text, `CommandType.StoredProcedure`, and names passed to a wrapper when a procedure
  of that name is defined, linked to their SQL definitions.
- **Message bus.** MassTransit and NServiceBus publishes, sends and consumers (`IConsumer<X>`, `IConsumer<Batch<X>>`,
  `IHandleMessages<X>`, state machine events and schedules, registrations closing a generic consumer), linked across
  repos by the message type's full name. A message type is known when a configured attribute marks it (see
  [configurable conventions](#configurable-conventions)). Without one, as in plain MassTransit code, publishers and
  consumers link by the type's bare name only and every such link is `ambiguous`. A publish whose message type isn't
  known (a lambda's or loop's variable, a helper's result) is reported as `unsupported` on the message types it may
  publish.

Known gaps:

- A minimal API handler that is a method group declared in another file keeps its route on the registering method,
  and helper methods are matched by name and parameter position, not by type. `MapHub`, `MapHealthChecks`,
  `MapForwarder` and the like are not routes.
- Overloads are narrowed by argument count and by argument types where the call site shows them (literals, typed
  locals, fields, `new X()`); generic parameters, `object`, `params`, named arguments and library types stay unsure,
  and every remaining overload is listed. Overloads are separate symbols: pick one by `file:line`.
- Bare property reads through `this` inside a type are not recorded as references.
- Reflection, source generators, and code built at run time are not seen. `unused` treats names in strings,
  attributes and config as reasons for doubt rather than proof of use.

## TypeScript and JavaScript

Extracted: classes, interfaces, functions, methods, properties, type aliases, enums, imports and exports; calls,
instantiations and member uses. Plain JavaScript goes through the TypeScript extractor with the TSX grammar.
Minified, built and vendored files are left out (`dist/`, `bundles/`, `plugins/` outside a `src` directory, a library
by name such as `jquery.form.js`). A function used through a named import (`import { f } from './m'`) or a namespace
import (`import * as m from './m'`, then `m.f()`) resolves to its declaration. Answers that count uses (`find`'s
`declared`, `refs`) check the files left out as text for the name and name the lines that do.

Resolved:

- **Imports.** A module that isn't a relative path is placed as `tsc` places it: by the nearest `tsconfig.json` or
  `jsconfig.json` above the file and the configs it `extends`, through `paths` and then `baseUrl`, then the declared
  packages. A module no config maps (a bundler alias) is placed by its file name, at most `unique_name`, with evidence
  `filename_guess`. A name imported from a package never resolves to a same-named helper in the repo. Into
  dependency repos, an imported name resolves only in the repo providing that package.
- **Receivers.** Typed fields and parameters, Angular constructor injection and `inject()`, `TestBed.inject(T)`,
  `Mocked<T>` and injection tokens.
- **Angular.** Component selectors (elements, attribute directives, compound selectors, selectors in constants),
  templates, routes (lazy prefixes resolved), dialogs, and providers (`useClass`, `useExisting`, `useFactory`,
  `useValue`, `providedIn: 'root'`) scoped as injectors scope them. Several providers in scope are all listed as
  `ambiguous`, none picked.
- **NgRx.** Action creators and action groups linked to the effects whose `ofType` names them, by name (evidence
  `convention`).

Re-exports (`export { x } from`, `export * from`) are followed a few levels deep.

Known gaps: dynamic `import()` with a computed path, and JavaScript without imports (globals, script tags), are
resolved by name only, if at all. Decorators other than Angular's carry no meaning. Frameworks other than Angular
(React, Vue, Svelte) get plain TypeScript resolution, with no component or DI model.

## Swift

Extracted: classes, structs, enums, actors, protocols, extensions merged onto their types, functions, methods,
initializers, properties, conformances and inheritance, calls and instantiations, member uses (`x.count`,
`.secondary`, `\.theme`), imports and attributes. Apple-domain tags: `@main` entry points, SwiftUI views, SwiftData
models, `@Observable`, App Intents and tests.

Resolved: through the receiver's type, the caller's own type and its supertypes, and protocols (witnesses and calls
through a protocol requirement). SwiftPM `Package.swift` and XcodeGen `project.yml` give packages, targets and
repo-to-repo edges; an `import` of a module another indexed repo provides resolves into it.

Known gaps: no type inference beyond declared types, so a receiver whose type comes from an expression is resolved by
name; a common standard-library member on an unknown receiver is `external_likely`. No compiler index store is read.

## Java

Extracted: classes, interfaces, enums, records and annotation types (an interface or annotation type is a protocol, a
constructor an `init`), methods, fields, record components and enum constants; `extends` and `implements`; the package
and every `import` (single-type, on-demand `a.b.*`, and `static`); calls with their receivers and argument counts,
instantiations, method references (`Type::method`, `Type::new`), field and member uses, annotations and type uses. A
parameter's or local's declared type (including a pattern variable's, `o instanceof Order placed`) types its receiver,
and so does a chain of fields, calls and member uses on `this`, a field or a type written by name
(`Report.builder().title(t).build()`, `this.store.items()`): each step is followed through the repo's declarations as
Java reads them, a member type written inside its outer type (`Builder` in `Report`) included. Methods annotated `@Test`
(and JUnit 5's other test annotations) are tagged `test`, `main` `entrypoint`.

Resolved as javac scopes names: a simple type name is found in the types the code is nested in, then single-type
imports, then the file's own package, then on-demand imports. A type a single-type import brings from another package,
or a name no rule reaches, is outside the repo and stays `unresolved`, never linked to a same-named declaration of the
repo's (`import java.util.List` next to a repo class `List`). A member named through an outside type (`BigInteger.ONE`,
`Collections.emptyList()`) is unresolved too, and a bare call a static import brings from outside is unresolved unless
the caller's own types declare or inherit the name. A local class is seen only in its method. On a receiver of a known
repo type, the type's own member wins over a same-named one a supertype or interface declares, and overloads are told
apart by argument count. Fields and methods are separate names, as in Java: a field access never resolves to a method of
the same name, nor a call to a field. A member of a value whose type isn't written (a chained call's result) is listed
as `ambiguous` candidates, never linked by its name alone.

Known gaps: members generated by annotation processors (Lombok, AutoValue) and by records (`values()`, a record's
accessors) aren't seen; a call's overload is told apart by its argument count only; generics are erased to their type's
name. Maven and Gradle builds aren't read yet, so a repo's modules aren't areas or test targets. Kotlin files aren't
read.

## Python

Extracted: classes, functions, methods, imports, calls and member uses. Names imported with `from m import`, and
members of an imported module (`m.save()`), resolve in that module: relative imports from the file's package,
absolute ones by module path (certain only from a plausible source root), and never into the repo when the module is
from outside it.

Known gaps: no type inference for receivers; dynamic attributes, `getattr`, and monkey-patching are not seen. No
manifest (`pyproject.toml`, `requirements.txt`) is read for repo-to-repo edges.

## Rust

Extracted: structs, enums, traits, functions, methods, `impl` blocks, `use` declarations, calls and member uses.
`Cargo.toml` gives packages and path and registry dependencies between indexed repos.

Known gaps: no trait resolution or macro expansion; a method call on a receiver of unknown type resolves by name.
For precise references, use a SCIP index from `rust-analyzer scip`.

## Go

Extracted: package-level types (structs, interfaces, defined types and aliases), functions, methods, struct fields,
interface methods, package-level variables and constants, imports, calls, composite literals, and type and member
uses. A method belongs to its receiver's type wherever it is declared; an embedded field or interface promotes its
members to the type embedding it. Test functions in `_test.go` files (`TestXxx`, `BenchmarkXxx`, `ExampleXxx`,
`FuzzXxx`) are tests, and `main` in package `main` is an entry point.

Resolved as the compiler scopes names, and never by a name alone:

- **Packages.** A package is the files of one directory sharing a package clause; a bare name is a package-level
  declaration of the caller's package. Only tests see a `_test.go` file's declarations, and a `_test` package is a
  package of its own.
- **Imports.** An import path is placed through the repo's `go.mod` files: under the caller's module root, or in
  another module of the repo when the caller's `go.mod` replaces it with that directory. The standard library, a
  dependency, and a released version of the repo's own module are outside the repo, so their names stay
  `unresolved`.
- **Receivers.** Locals, parameters and fields with a declared type, composite literals (`&T{}`), `new(T)`, a call's
  declared result, ranges over typed values, type assertions and type switches; chains through fields and calls
  (`s.cfg.Get()`). A member is the receiver type's own, else one promoted through embedding, nearest first; a receiver
  whose type is outside the repo, or embeds an outside type, keeps the member outside. A receiver of unknown type lists
  the repo's members of that name as `ambiguous` candidates, never one linked.
- **The default build.** Linux, any architecture, cgo on, no custom tags. A file another build compiles
  (`x_windows.go`, `//go:build nomsgpack`) is read, but its declarations give way to the default build's.

HTTP routes:

- **Servers.** `net/http`'s `Handle` and `HandleFunc` on a `ServeMux` or the default one, with method patterns
  (`"GET /items/{id}"`); gin, echo and chi verbs, `Any`/`Match`/`Handle`/`Add`/`Method`, static files, groups
  (`Group`, `Route`, `With`) and chi's `Mount`. A router is placed by its constructor, its type, or the group it was
  made from; a router a function receives or returns takes the prefixes its callers give it, so a route registered in
  a helper (`func api(r gin.IRouter)`) carries the prefix of every group it's registered under. A helper method called
  on a value whose type the syntax doesn't give (`h.Register(v1)`) takes the groups passed to methods of that name
  when no caller names its type; a group a test builds doesn't count toward a helper's prefixes. A function taking a
  registration function (`handle func(string, http.Handler)`, which callers pass as `mux.Handle`) registers its calls'
  patterns on that mux. A path may be a package constant. A registration under a run-time condition (`if cfg.OIDC { … }`) is listed: which configuration
  serves it isn't known from source.
- **Clients.** `net/http`'s `Get`, `Head`, `Post`, `PostForm`, `NewRequest` and `NewRequestWithContext`, on the
  package, `http.DefaultClient` or an `http.Client`. The URL is read from literals, `+`, `fmt.Sprintf` and package
  constants; a value before the path (`base + "/orders"`) is the base address and dropped.

Known gaps: a type isn't matched to the interfaces it satisfies implicitly (a call through an interface resolves to
the interface's method); a generic receiver resolves to its generic type's members; dot imports leave their names
unresolved. Measured against the type checker on three public repositories in
[identity-v2](../eval/identity-v2/README.md).

## ColdFusion (CFML)

`.cfm` templates, `.cfc` components (tag-based and script) and `.cfs` files, with `<cfscript>` parsed as CFScript.
Includes, user-defined function calls, and components from `createObject`, `new`, `<cfobject>` and `<cfinvoke>`
resolve by path the way the engine resolves them: relative to the file, from the web root, through `this.mappings`,
then through `extends`. Custom tags resolve too. Lookups ignore case. Tables named in `<cfquery>` and `queryExecute`
are read or written by the function or template running them. `<cfhttp>` requests link to ASP.NET actions. Files
under a CF engine's own `WEB-INF` are not indexed.

## SQL

`.sql` files (MySQL and MariaDB dumps, migrations and routines; T-SQL scripts; UTF-8 or UTF-16), read by a
statement-level scanner. `CREATE TABLE`, `VIEW`, `PROCEDURE`, `FUNCTION`, `TRIGGER` and `EVENT` become symbols in
their schema, and each body gives the tables it reads and writes and the routines it calls. `ALTER TABLE`, indexes
and foreign keys are references. Names match without case, quotes or a schema prefix where none is written.

Entity Framework tables and stored procedure calls in C# and CFML link to these definitions in any indexed repo,
with a confidence and evidence `sql_name`, assuming `same_database` unless the schema is named.

Known gaps: SQL references record a line but no column. Dynamic SQL built from values is not parsed. Dialects other
than MySQL/MariaDB and T-SQL are read on a best-effort basis.

## Terraform

`.tf` files: resources, data sources, module calls, variables, locals, outputs and provider configurations, each named
by its address (`aws_s3_bucket.logs`, `module.net`, `var.region`). What a block depends on comes from its
expressions, `depends_on` and `provider =`. Every address resolves within its own directory, so per-environment
copies with the same names stay apart. Module calls with a local source link to the directory; a git source links to
the indexed repo whose origin it is; a registry address links by namespace and name.

Not read: `.tf.json`, `.tfvars`, `terraform_remote_state` outputs, and the default provider a block uses without
`provider =`.

## Ansible

Playbooks, roles (in `roles/<name>/`, or a repo that is one role), task and handler files, variable files
(`defaults`, `vars`, `group_vars`, `host_vars`), `requirements.yml`, and Jinja templates (`.j2`, `.jn2`). A YAML
file elsewhere is read only when its top level is a list of plays. References resolve as Ansible finds things:
roles beside the playbook or up the tree, included files and templates by path, handlers by name or `listen`, and
variables to their definitions in scope, highest precedence first. A role another repo provides links through the
`requirements.yml` naming its `src`. What tasks deploy (services, images, source repos, databases, URLs) shows in
`context`, with the task's action (`deploys`, `undeploys`, `restarts`, `stops`).

Not modelled: inventory and extra vars, and anything computed at run time.

## GitLab CI

`.gitlab-ci.yml` and the files pipelines include: includes, jobs (hidden ones tagged), `extends`, variables, rules,
child pipelines, and the script steps that matter for a deploy. At query time a repo's pipeline is resolved as GitLab
would for a push to the default branch: includes across indexed repos by project path, `extends` chains, variable
precedence, `parallel: matrix:`, CI components with their inputs, and GitLab's built-in Terraform templates (known
without being fetched).

`knowcode context <repo name>` answers how a repo is deployed: each deploy chain from the application's job through
shared templates and child pipelines to what runs, with the values passed and where each is set. Chains cover
`ansible-playbook` runs, `wrangler`, the AWS CLI (S3, CloudFront, ECS, Lambda, EC2 launch templates, auto scaling,
SSM, CloudFormation, Elastic Beanstalk), `sam`, `cdk`, `terraform`/`tofu`, `docker`, `kubectl`, `helm`, and releases
(`npm publish`, `upack push`, `release-cli create`).

Every `include:` is accounted for in `ci_includes`: `followed` (with the repo it came from, the files read, the jobs
they define and the deploys run from them), or `missing`, `unread`, `unsupported` (a GitLab template this doesn't
know, a job's artifact, or a form not read: kind `unknown`) or `cut`, with why. Includes whose rules leave them out
of a default-branch push are listed apart, one line each, in `ci_skipped` (`file:line: kind target`), so nothing in
the chains is taken to come from them. Each list of the deploy answer (`deploy_chains`, `deployed_by`, `own`,
`ci_templates`, `ci_includes`, `ci_skipped`) continues by cursor when cut; `knowcode context <repo name> --limit N`
(MCP `limit`) cuts each to N rows and says `<list>_total`. `ci_unread` and `ci_cut` are never cut.

Not supported: other CI systems (GitHub Actions, Jenkins, Azure Pipelines). Variables set only in project settings or
at run time are reported as `unresolved`.

## Manifests

SwiftPM, XcodeGen, Cargo, MSBuild (`.csproj`), npm (`package.json`) and Go (`go.mod`: the module, its requires and
replaces) manifests give packages and repo-to-repo edges. A path dependency links to the repo it lands in; a registry dependency (NuGet, npm, crates) links to the
indexed repo declaring that package, with `revision_unverified` when the version can't be checked. Checkouts of one
origin (a clone, a worktree, a second clone) count as one repo.

## Configurable conventions

Some HTTP and message-bus linking recognises a code base's own conventions by name, as configured under
`[conventions.*]` in `config.toml` (see [configuration](configuration.md#conventions)): a TypeScript request-contract
decorator, a gateway DTO attribute and gateway calls naming a service and path, a service host's base class, a
message-type attribute, consumer base classes, registration helpers and publish helpers naming a destination, DI scope
values, the CI variables naming a deployed application, and the names Ansible linking leaves out or compares without.
The defaults are the public libraries' names, so repos without conventions of their own get the framework-level
linking described above.

## Constructs recognised but not read

Some constructs are recognised by an extractor but not read. They are stored where they are, and an answer that draws
on them is incomplete with reason `unsupported`, naming the first few with `file:line`, instead of saying `complete`:

| Area | Constructs | What the answer may lack |
| --- | --- | --- |
| C# DI | assembly-scan registrations (`RegisterAssemblyTypes`, for the types its name filter lets through, from the project its argument names: `typeof(T).Assembly`, `GetExecutingAssembly()`), typed HTTP clients (`AddHttpClient<T>`), keyed services (`AddKeyedScoped<…>`). A scan of an assembly the code doesn't name, for a type nothing registers or injects, is said in the type's "DI registrations: none found" line instead | registrations made through them; a service may be said unregistered |
| SQL | SQL text naming its table at run time; dynamic SQL in a procedure (`EXEC (@sql)`, `EXEC @sql`, `sp_executesql`, `PREPARE … FROM`) | the tables they name |
| Message bus | a publish of a message whose type isn't known | links from it to consumers |
| Ansible | includes, templates and handlers named by Jinja | the files they include or render |
| Terraform | `moved`, `import`, `removed` and `check` blocks | the addresses they name, as uses |
| GitLab CI | an include or trigger in a form not read (kind `unknown` in `ci_includes`) | the jobs and includes it brings |
| Angular | a lazy route in a form not read (a default export, a module string) | the routes it loads |
| Go | a file the default build leaves out (`x_windows.go`, a `//go:build` constraint false without custom tags), area `build`; `import "C"` (cgo), area `cgo` | the declarations only another build compiles; the C code `C.name` refers to |
| HTTP (Go) | a registration whose path or method isn't a literal or a constant; a request whose method is chosen at run time | the routes it registers, and the requests it sends |
| Razor | `.razor` and `.cshtml` files, which aren't parsed: a C# `refs` or `impact` answer from a repo holding any checks them as text for the name and names the lines that name it (`refs` says in `checked` when none does); with more than 20 names to check, it says the files exist, naming one | uses of C# code in their markup and `@code` blocks |
| HTTP (C#) | an endpoint registration whose route can't be placed: a `Map*` template or `MapGroup` prefix that isn't a constant, `MapMethods` verbs that aren't, `Map` as a middleware branch, conventional routes whose template isn't a literal or names an area, an attribute request contract whose path isn't a constant | the routes it registers and their callers: in `routes` for the repo, in `context` on its handler, and on a controller action with no route and no callers where conventional routes may serve it |

Constructs not in this table and not listed as gaps above are simply not seen, and an answer can be `complete`
without them.

## Code health and history

`hotspots` biomarkers (Bumpy Road, Deep Nesting, Complex Conditional, Brain Method, Large File) are computed for every
extracted language; Low Cohesion (LCOM4) for C# and TypeScript classes only. History answers work for any file git
tracks, extracted or not.
