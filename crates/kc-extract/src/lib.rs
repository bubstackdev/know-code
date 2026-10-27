//! Syntactic extraction: parse a source file with tree-sitter and return the symbols it defines, the names it
//! references, and the type relations it declares.
//!
//! Extraction is per file and context-free. Names are resolved later, at query time, against the whole index, so
//! changing one file never forces other files to be re-extracted.

pub mod ansible;
pub mod cfml;
pub mod conventions;
mod csharp;
pub mod gitlabci;
pub mod hosts;
pub mod html;
mod manifest;
mod python;
mod rust;
pub mod sql;
mod swift;
pub mod terraform;
mod tsconfig;
mod typescript;

pub use manifest::implicit_usings;
pub use typescript::{action_name, api_tag, is_minified};
pub mod types;
pub mod urls;
mod yaml;

use serde::Serialize;
pub use types::{PathRoot, Seg, TypeDecl, TypeExpr};

/// Languages with an extractor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    Swift,
    Python,
    Rust,
    CSharp,
    TypeScript,
    /// TypeScript with JSX (`.tsx`).
    Tsx,
    /// Plain JavaScript (`.js`, `.mjs`, `.cjs`, `.jsx`), read by the TypeScript extractor with the TSX grammar.
    /// Minified and bundled files are left out (see [`Lang::from_path`] and [`typescript::is_minified`]).
    JavaScript,
    /// SwiftPM `Package.swift`.
    SwiftPackage,
    /// XcodeGen `project.yml`.
    XcodeGen,
    /// `Cargo.toml`.
    Cargo,
    /// MSBuild `*.csproj`.
    CsProj,
    /// HTML, read as Angular templates.
    Html,
    /// npm `package.json`.
    PackageJson,
    /// ColdFusion: `.cfm` and `.cfs` templates, `.cfc` components.
    Cfml,
    /// SQL scripts: schema dumps, migrations, routines.
    Sql,
    /// Ansible: playbooks, roles, task and variable files, requirements, Jinja templates (see [`ansible`]).
    Ansible,
    /// GitLab CI: `.gitlab-ci.yml` and the files pipelines include (see [`gitlabci`]).
    GitlabCi,
    /// Terraform configuration (`.tf`, see [`terraform`]).
    Terraform,
    /// TypeScript project configuration: `tsconfig.json`, `tsconfig.*.json`, `jsconfig.json` (see [`TsConfig`]).
    TsConfig,
    /// Configuration naming hosts: ASP.NET settings and launch profiles, .NET `*.config`, Docker Compose, ingress
    /// manifests, HAProxy and nginx configs (see [`hosts`]).
    HostConfig,
}

impl Lang {
    /// Every language this build indexes.
    pub const ALL: [Lang; 20] = [
        Lang::Swift,
        Lang::Python,
        Lang::Rust,
        Lang::CSharp,
        Lang::TypeScript,
        Lang::Tsx,
        Lang::JavaScript,
        Lang::SwiftPackage,
        Lang::XcodeGen,
        Lang::Cargo,
        Lang::CsProj,
        Lang::PackageJson,
        Lang::Html,
        Lang::Cfml,
        Lang::Sql,
        Lang::Ansible,
        Lang::GitlabCi,
        Lang::Terraform,
        Lang::TsConfig,
        Lang::HostConfig,
    ];

    /// `as_str` of the languages with symbols (not manifests).
    pub const SOURCE: [&str; 11] =
        ["swift", "python", "rust", "csharp", "typescript", "tsx", "javascript", "cfml", "sql", "ansible", "terraform"];

    pub fn from_path(path: &str) -> Option<Lang> {
        let file = path.rsplit('/').next().unwrap_or(path);
        match file {
            "Package.swift" => Some(Lang::SwiftPackage),
            "project.yml" => Some(Lang::XcodeGen),
            "Cargo.toml" => Some(Lang::Cargo),
            "package.json" => Some(Lang::PackageJson),
            "jsconfig.json" => Some(Lang::TsConfig),
            f if f.starts_with("tsconfig") && f.ends_with(".json") => Some(Lang::TsConfig),
            _ if hosts::is_host_config(path) => Some(Lang::HostConfig),
            _ => match path.rsplit_once('.').map(|(_, e)| e) {
                Some("swift") => Some(Lang::Swift),
                Some("py") => Some(Lang::Python),
                Some("rs") => Some(Lang::Rust),
                Some("cs") => Some(Lang::CSharp),
                Some("csproj") => Some(Lang::CsProj),
                Some("html") => Some(Lang::Html),
                Some("ts" | "mts" | "cts") => Some(Lang::TypeScript),
                Some("tsx") => Some(Lang::Tsx),
                Some("js" | "mjs" | "cjs" | "jsx") if !is_built_js(path) => Some(Lang::JavaScript),
                Some("sql" | "SQL") => Some(Lang::Sql),
                Some("tf") => Some(Lang::Terraform),
                _ if cfml::is_cfml_path(path) => Some(Lang::Cfml),
                _ if ansible::place(path).is_some() => Some(Lang::Ansible),
                _ if gitlabci::is_ci_path(path) => Some(Lang::GitlabCi),
                _ => None,
            },
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Lang::Swift => "swift",
            Lang::Python => "python",
            Lang::Rust => "rust",
            Lang::CSharp => "csharp",
            Lang::TypeScript => "typescript",
            Lang::Tsx => "tsx",
            Lang::JavaScript => "javascript",
            Lang::SwiftPackage => "swiftpm",
            Lang::XcodeGen => "xcodegen",
            Lang::Cargo => "cargo",
            Lang::CsProj => "csproj",
            Lang::Html => "html",
            Lang::PackageJson => "npm",
            Lang::Cfml => "cfml",
            Lang::Sql => "sql",
            Lang::Ansible => "ansible",
            Lang::GitlabCi => "gitlabci",
            Lang::Terraform => "terraform",
            Lang::TsConfig => "tsconfig",
            Lang::HostConfig => "hostconfig",
        }
    }

    pub fn is_manifest(self) -> bool {
        matches!(self, Lang::SwiftPackage | Lang::XcodeGen | Lang::Cargo | Lang::CsProj | Lang::PackageJson)
    }

    /// Bumped whenever the extractor's output changes, so indexed files of this language get re-extracted. The
    /// configured conventions the language's extraction depends on are part of it (see [`conventions::stamp`]).
    pub fn extractor_version(self) -> u32 {
        self.base_version() + conventions::stamp(self)
    }

    /// [`Self::extractor_version`] under the default conventions.
    pub fn base_version(self) -> u32 {
        match self {
            Lang::Swift => swift::VERSION,
            Lang::Python => python::VERSION,
            Lang::Rust => rust::VERSION,
            Lang::CSharp => csharp::VERSION,
            Lang::Html => html::VERSION,
            Lang::Cfml => cfml::VERSION,
            Lang::Sql => sql::VERSION,
            Lang::Ansible => ansible::VERSION,
            Lang::GitlabCi => gitlabci::VERSION,
            Lang::Terraform => terraform::VERSION,
            Lang::TsConfig => tsconfig::VERSION,
            Lang::HostConfig => hosts::VERSION,
            Lang::TypeScript | Lang::Tsx | Lang::JavaScript => typescript::VERSION,
            Lang::SwiftPackage | Lang::XcodeGen | Lang::Cargo | Lang::CsProj | Lang::PackageJson => manifest::VERSION,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Class,
    Struct,
    Enum,
    Actor,
    Protocol,
    Extension,
    Function,
    Method,
    Init,
    Subscript,
    Property,
    Case,
    Typealias,
    /// A CFML template (`.cfm`): a page or an included fragment, run top to bottom.
    Template,
    /// SQL: a table, view, stored procedure, trigger or scheduled event. SQL functions are `Function`s.
    Table,
    View,
    Procedure,
    Trigger,
    Event,
    /// Ansible: a role (in its `tasks/main.yml`, and its `meta/main.yml` tagged `meta`), a playbook file, a play, a
    /// named task or block, a handler, a task file other than a role's main one, and a variable.
    Role,
    Playbook,
    Play,
    Task,
    Handler,
    Tasks,
    Variable,
    /// GitLab CI: a job, or a hidden job (`.name`, tagged `hidden`) other jobs extend.
    Job,
    /// Terraform (see [`terraform`]), named by address: a resource (`aws_s3_bucket.x`), a data source
    /// (`data.aws_iam_policy.y`), a module call (`module.z`), a local value (`local.l`), an output (`output.o`) and a
    /// provider configuration (`provider.aws`, `provider.aws.east`). Input variables (`var.v`) are `Variable`s.
    Resource,
    Data,
    Module,
    Local,
    Output,
    Provider,
}

impl SymbolKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SymbolKind::Class => "class",
            SymbolKind::Struct => "struct",
            SymbolKind::Enum => "enum",
            SymbolKind::Actor => "actor",
            SymbolKind::Protocol => "protocol",
            SymbolKind::Extension => "extension",
            SymbolKind::Function => "function",
            SymbolKind::Method => "method",
            SymbolKind::Init => "init",
            SymbolKind::Subscript => "subscript",
            SymbolKind::Property => "property",
            SymbolKind::Case => "case",
            SymbolKind::Typealias => "typealias",
            SymbolKind::Template => "template",
            SymbolKind::Table => "table",
            SymbolKind::View => "view",
            SymbolKind::Procedure => "procedure",
            SymbolKind::Trigger => "trigger",
            SymbolKind::Event => "event",
            SymbolKind::Role => "role",
            SymbolKind::Playbook => "playbook",
            SymbolKind::Play => "play",
            SymbolKind::Task => "task",
            SymbolKind::Handler => "handler",
            SymbolKind::Tasks => "tasks",
            SymbolKind::Variable => "variable",
            SymbolKind::Job => "job",
            SymbolKind::Resource => "resource",
            SymbolKind::Data => "data",
            SymbolKind::Module => "module",
            SymbolKind::Local => "local",
            SymbolKind::Output => "output",
            SymbolKind::Provider => "provider",
        }
    }

    pub fn is_type(self) -> bool {
        matches!(
            self,
            SymbolKind::Class | SymbolKind::Struct | SymbolKind::Enum | SymbolKind::Actor | SymbolKind::Protocol
        )
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Symbol {
    /// Index of the enclosing symbol in `FileFacts::symbols`.
    pub parent: Option<usize>,
    pub name: String,
    pub kind: SymbolKind,
    /// Dotted name of the enclosing type, e.g. `Store` or `Outer.Inner`. For members of an extension this is the
    /// extended type, so extension members merge onto their type.
    pub container: Option<String>,
    /// The declaration header, whitespace-collapsed and capped.
    pub signature: String,
    /// First line of the doc comment, if any.
    pub doc: Option<String>,
    /// Attribute names without `@`, e.g. `MainActor`, `Published`, `Model`.
    pub attrs: Vec<String>,
    /// Domain tags: `entrypoint`, `swiftui_view`, `swiftdata_model`, `observable`, `app_intent`, `test`.
    pub tags: Vec<&'static str>,
    pub start_line: u32,
    pub end_line: u32,
    pub start_byte: u32,
    pub end_byte: u32,
    /// Branch count plus one for functions and initializers; branch count for computed properties; else 0.
    pub complexity: u32,
    /// Deepest nesting of control-flow blocks (`if`, loops, `switch`, `try`) inside a function body; `else if` is the
    /// same level as its `if`. 0 for non-callables and straight-line bodies.
    pub nesting: u32,
    /// Bumps of a function (CodeScene's "Bumpy Road"): its top-level control-flow blocks that nest at least two deep,
    /// as (first line, last line). C# and TypeScript only.
    pub bumps: Vec<(u32, u32)>,
    /// The most `&&`/`||` operators in one branch condition (`if`, `while`, `do`, `for`, `?:`) of a function, and
    /// that condition's line; 0 when none. C# and TypeScript only.
    pub cond_ops: u32,
    pub cond_line: u32,
    /// Declared or initialized type of a property (`let store: Store`, `let store = Store()`), when it is a plain
    /// nominal type. Used to resolve member calls on the property.
    pub type_name: Option<String>,
    /// How many arguments a call to this method passes, as (fewest, most); most is `None` with a `params` array.
    /// An extension method's receiver isn't counted. `None` where unknown. C# only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arity: Option<(u32, Option<u32>)>,
    /// For a method with a delegate parameter (`Func<T, bool>`, `Expression<Func<T, bool>>`, `Action<T>`): per
    /// parameter, the encoded input types of its delegate, or nothing, as JSON (`[null, ["Order"]]`). A lambda passed
    /// there is typed from it at query time. C# only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lambda_types: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RefKind {
    Call,
    Instantiate,
    TypeRef,
    /// A member used without being called: `x.count`, `.secondary`, `\.theme`.
    Member,
    /// SQL: a table or view a statement reads (`FROM`, `JOIN`).
    Read,
    /// SQL: a table a statement writes (`INSERT`, `UPDATE`, `DELETE`, `MERGE`, `TRUNCATE`).
    Write,
    /// Ansible: a role used (`roles:`, `include_role`, a meta dependency).
    Role,
    /// Ansible: a task file or playbook included or imported, by file name (the path as written is the receiver).
    Include,
    /// Ansible: a handler notified, by name or `listen` topic.
    Notify,
    /// Ansible: a variable a Jinja expression or condition uses.
    Var,
    /// Ansible: a template a `template` task renders, by file name (the `src` as written is the receiver).
    Template,
    /// Terraform: an address an expression uses (`var.x`, `aws_s3_bucket.b`, `module.m`).
    TfRef,
    /// Terraform: an address in a `depends_on` list.
    DependsOn,
    /// Terraform: the provider configuration a resource, data source or module uses (`provider.aws.east`).
    Provider,
    /// Terraform: a module argument, as the variable it sets in the module (`var.x`, the module as the receiver).
    TfInput,
    /// Terraform: a module output an expression uses (`output.o`, the module as the receiver).
    TfOutput,
}

impl RefKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RefKind::Call => "call",
            RefKind::Instantiate => "instantiate",
            RefKind::TypeRef => "type_ref",
            RefKind::Member => "member",
            RefKind::Read => "read",
            RefKind::Write => "write",
            RefKind::Role => "role",
            RefKind::Include => "include",
            RefKind::Notify => "notify",
            RefKind::Var => "var",
            RefKind::Template => "template",
            RefKind::TfRef => "tf_ref",
            RefKind::DependsOn => "depends_on",
            RefKind::Provider => "provider",
            RefKind::TfInput => "tf_input",
            RefKind::TfOutput => "tf_output",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Ref {
    /// The innermost enclosing symbol, if any.
    pub from: Option<usize>,
    pub name: String,
    /// Receiver text for member calls (`self`, `store`, `Foo`, `.` for implicit members), capped.
    pub receiver: Option<String>,
    /// The receiver's type when it is known syntactically: a parameter or local with a declared or constructed
    /// type, or a constructor expression (`Archive(url: u).load()`).
    pub receiver_type: Option<String>,
    pub kind: RefKind,
    pub line: u32,
    /// The 1-based byte column on `line` where the name is written; 0 when not known. Each occurrence is a
    /// reference of its own: two calls of one name on a line are two references, which may resolve differently.
    pub col: u32,
    /// How many arguments a call passes, for telling overloads apart. C# only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationKind {
    /// `: Target` on a type or extension. Whether it's inheritance or conformance is decided at query time, once
    /// the target's kind is known.
    Inherits,
}

#[derive(Debug, Clone, Serialize)]
pub struct Relation {
    pub from: usize,
    /// Dotted target name with generic arguments removed.
    pub target: String,
    pub kind: RelationKind,
    pub line: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct Import {
    pub module: String,
    pub line: u32,
}

/// A C# `global using`: imported by every file of the file's project (`global using Shop.Domain;`, `global using
/// static Shop.Clock;`, `global using Money = Shop.Decimal4;`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GlobalUsing {
    /// The namespace imported, or for `static` the type, or for an alias its target.
    pub namespace: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    pub is_static: bool,
    pub line: u32,
}

/// A name a file takes from a module. TypeScript: `import { name as local } from 'module'`, `import * as local`
/// (`name` is `*`), `import local from` (`name` is `default`); with `export`, a re-export: `export { name as local }
/// from 'module'`, or `export * from 'module'` (`local` and `name` both `*`). Python: `from module import name as
/// local` (`module` keeps its leading dots), `import module as local` (`name` is `*`; `local` is the dotted module
/// itself without `as`), `from module import *` (`local` and `name` both `*`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImportBinding {
    pub local: String,
    pub name: String,
    pub module: String,
    pub line: u32,
    pub export: bool,
}

/// What module resolution needs from a `tsconfig.json` (or `jsconfig.json`), as written: paths are relative to the
/// file's directory, and the files it `extends` (in order, later ones winning) supply what it doesn't set itself.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct TsConfig {
    /// `compilerOptions.baseUrl`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// `compilerOptions.paths`: each pattern (at most one `*`) and its substitutions, tried in order. `None` when the
    /// file doesn't set `paths` (an empty map does set it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paths: Option<Vec<(String, Vec<String>)>>,
    /// `extends`: a path (`./tsconfig.base.json`) or a package (`@tsconfig/node20/tsconfig.json`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extends: Vec<String>,
    /// `references[].path`: the projects of a solution-style config.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DepSource {
    Path,
    Url,
    Registry,
    Workspace,
    /// Not a dependency: a C# project's sources from outside its directory (`<Compile Include="..\X.cs" />`, a shared
    /// project's `.projitems`), `location` their path pattern relative to the project file.
    Compile,
    /// Not a dependency: a namespace every file of a C# project imports. `name` the namespace (or type, or alias
    /// target), `location` how: `include` (`<Using Include="X" />`), `remove`, `static`, `alias:<A>`, `implicit` (the
    /// SDK's implicit usings, `ImplicitUsings` enabled), or `unknown` (the project's usings can't be read: a property
    /// sets `ImplicitUsings`, or an SDK whose implicit list isn't known).
    Using,
}

impl DepSource {
    pub fn as_str(self) -> &'static str {
        match self {
            DepSource::Path => "path",
            DepSource::Url => "url",
            DepSource::Registry => "registry",
            DepSource::Workspace => "workspace",
            DepSource::Compile => "compile",
            DepSource::Using => "using",
        }
    }
}

/// A package-level dependency declared in a manifest.
#[derive(Debug, Clone, Serialize)]
pub struct PackageDep {
    /// Package identity as the manifest refers to it.
    pub name: String,
    pub source: DepSource,
    /// The path or URL, as written.
    pub location: String,
    /// Version, branch or revision requirement, as written.
    pub requirement: Option<String>,
    pub line: u32,
    /// For path dependencies: the absolute root of the repo the path lands in. Filled in by the indexer.
    pub resolved_repo: Option<String>,
}

/// A dependency of one target or product.
#[derive(Debug, Clone, Serialize)]
pub struct TargetDep {
    pub name: String,
    pub product: Option<String>,
    pub package: Option<String>,
    /// `target`, `product`, `by_name` (SwiftPM bare string), or `sdk`.
    pub kind: &'static str,
    pub line: u32,
}

impl TargetDep {
    pub fn target(name: String, line: u32) -> TargetDep {
        TargetDep { name, product: None, package: None, kind: "target", line }
    }
}

/// A target, product or workspace member.
#[derive(Debug, Clone, Serialize)]
pub struct TargetFact {
    pub name: String,
    /// `target`, `testTarget`, `executableTarget`, `product:library`, `application`, `lib`, `bin`, `member`, …
    pub kind: String,
    pub line: u32,
    pub deps: Vec<TargetDep>,
}

/// A package (SwiftPM package, XcodeGen project or Cargo package/workspace) declared by a manifest.
#[derive(Debug, Clone, Serialize)]
pub struct PackageFact {
    /// `swiftpm`, `xcodegen` or `cargo`.
    pub kind: &'static str,
    pub name: String,
    pub line: u32,
    pub targets: Vec<TargetFact>,
    pub deps: Vec<PackageDep>,
}

/// An HTTP route: one the code serves (a controller action, a minimal API endpoint) or one it sends requests to (a
/// request contract).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HttpRoute {
    /// The symbol the route belongs to: the action method, the endpoint's handler (a method group) or the method
    /// registering it (a lambda), the request contract class, or the method making the request (a gateway call
    /// naming its service and path, an HTTP client call with a URL). `None` in top-level statements.
    pub sym: Option<usize>,
    /// `server` (an action serving the route), `client` (a contract requesting it), `service` (a registration of
    /// the repo's service name, with no verb or path), `commented` (a commented-out `[Http*]` attribute in a
    /// controller: a route it served once, a sign its contract is left over), `group` (a minimal API route group
    /// passed to a helper method: `path` is its prefix, `service` the helper and parameter, `MapOrders#0`) or
    /// `unsupported` (an endpoint registration KnowCode recognises but can't place: `verb` names its style,
    /// `MapGet` or `MapControllerRoute`, and `path` says why), `prefix` (a class's `[Route]` template, for the actions
    /// of classes deriving from it: `path`, or the constant `path_ref`), `conventional` (a conventional route
    /// template, `verb` the call) or `operation` (a call to a generated API client: `path` is the operation,
    /// `service` the tag the client is named for). A controller action whose prefix the query places starts its path
    /// with `{^Action}` (`{^*Action}` for a public method with no route attribute); a client request whose URL is
    /// known only at run time has an empty path.
    pub side: &'static str,
    /// Upper-case HTTP verb; `*` for an endpoint answering every verb (`Map`).
    pub verb: String,
    /// The path as written, without leading or trailing `/`; empty when it comes from a constant. A request built in
    /// code, and a minimal API endpoint, are encoded by [`urls::encode`]: `{=Name}` for a constant, `{}` for any
    /// other value. A minimal API endpoint registered on a helper's parameter starts with `{@Helper#i}`: the
    /// prefixes of the groups callers pass as its parameter `i` (see `group`). A request whose URL is a `const` local
    /// of its function ends `{~name@line}`: the local and the line declaring it.
    pub path: String,
    /// The constant naming the path (`Api.filters`), resolved at query time against [`StringConst`]s.
    pub path_ref: Option<String>,
    /// The service a client route is sent to, or the one a `service` registration names (`MailingApi`), when
    /// written literally or with `nameof`.
    pub service: Option<String>,
    /// The constant naming the service (`ServiceNames.ApiName`), resolved like `path_ref`.
    pub service_ref: Option<String>,
    pub line: u32,
}

/// An Angular fact: a component's `selector` or `template` (its `templateUrl`, as written), a `route` (`value` is the
/// path, joined with enclosing routes in the same file; `extra` the component, or `loads:<Name>` for a lazy
/// `loadChildren`, `loads:<routes>@<module>` for one loading a routes constant), a `renders` (a custom element tag used in a template) or an `attr` (an attribute in a template
/// that may select a directive, as written). For both, `sym` is the component for an inline template, `None` in an
/// `.html` file, and `extra` the element's tag and attribute names, space-separated.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NgFact {
    pub kind: &'static str,
    pub sym: Option<usize>,
    pub value: String,
    pub extra: Option<String>,
    pub line: u32,
}

/// An Entity Framework fact: a `dbset` (a `DbSet<T>` property: `sym` is the property, `entity` is `T`, `name` the
/// property name), a `table` mapping (`[Table("x")]` on an entity class, or `ToTable("x")` / `ToView("x")` on an entity
/// builder: `sym` is the class or the enclosing method, `name` the table, `schema.table` when a schema is given), a
/// `set` access (`context.Set<T>()`: `sym` is the enclosing symbol, `name` empty), a `procedure` call (`EXEC sp_x`
/// through `FromSqlRaw` and friends, or a command with `CommandType.StoredProcedure`: `name` is the procedure, `entity`
/// empty), or a `query` over a member (`from o in ctx.Orders`, `IQueryable<T> q = ctx.Orders`: `name` is the member,
/// `entity` empty), which makes the access a read. Also: an `include` of a navigation (`.Include(o => o.Lines)`,
/// `.ThenInclude`, a string path: `name` is the navigation, `entity` empty), an `auto_include` (`Navigation(o =>
/// o.Lines).AutoInclude()` on a builder of `entity`), `lazy_loading` (`UseLazyLoadingProxies()`), a `connection`
/// (`AddDbContext<entity>(o => o.UseMySql(Settings.ShopDbConnection))` or the context's `OnConfiguring`: `name` is the
/// connection's setting or key, or `Database=x` from a literal connection string), and `sql_read` / `sql_write` (a
/// table SQL text reads or writes: a CFML query, or a C# string; `sym` is the method, or the field or property
/// holding the text).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DbFact {
    pub kind: &'static str,
    pub sym: Option<usize>,
    pub entity: String,
    pub name: String,
    pub line: u32,
}

/// What a config or code file says about HTTP hosts, for linking clients to the backends their base URLs reach.
///
/// `serves`: the repo (or `target`, a service or backend name) answers requests for `host`: a launch profile's or
/// Kestrel's URL, a compose service built from the repo, an Aspire project resource, an ingress rule, a proxy's host
/// rule (`target` its backend's service). `configures`: the repo's clients send to `host`: a URL in its configuration
/// (`target` the key), or a typed HTTP client's base address (`target` the client class). Only the host of a value is
/// kept, never the rest of it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HostFact {
    pub role: &'static str,
    /// `launch_settings`, `kestrel`, `compose`, `app_host`, `ingress`, `proxy`, `config` or `http_client`.
    pub kind: &'static str,
    /// Lower-case `name` or `name:port` (see [`urls::host_of`]).
    pub host: String,
    pub target: Option<String>,
    pub line: u32,
}

/// A named string constant (`static readonly filters = 'Filters'`, `const string Name = "x"`), for resolving routes.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StringConst {
    /// `Container.name`, or the bare name at top level.
    pub qualified: String,
    pub value: String,
    pub line: u32,
}

/// A dependency-injection registration: `RegisterType<Foo>().As<IFoo>()`, `AddScoped<IFoo, Foo>()`, a factory
/// `Register(_ => new Foo(…))`, or an assembly scan. One per service the registration exposes.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DiBinding {
    /// The service type as written (`IFoo`, `IValidator<Order>`). `None` for `AsImplementedInterfaces` (every
    /// interface the implementation declares) and for an assembly scan exposing the scanned types themselves.
    pub service: Option<String>,
    /// The concrete type's name, when known: `RegisterType<Foo>`, `typeof(Foo)`, a factory or instance that is
    /// `new Foo(…)`. For an assembly scan, the name filter when it is obvious (`*Service`).
    pub implementation: Option<String>,
    /// `singleton`, `scoped`, `transient` or `unit_of_work`; `None` for the container's default.
    pub lifetime: Option<&'static str>,
    /// `type`, `factory`, `instance` or `scan`; for Angular providers also `existing` (`useExisting`) and `parent` (a
    /// factory deferring to a parent injector's provider first, `implementation` what it otherwise makes).
    pub kind: &'static str,
    pub line: u32,
    /// For a factory or instance whose type only the repo's declarations tell (a factory method's call, a
    /// property): what it returns, an encoded [`TypeExpr`], resolved into `implementation` when the repo is indexed.
    pub made: Option<String>,
    /// The registration names no service: the container registers it as what it makes (`Register(_ => Make())`).
    pub service_made: bool,
    /// How a Microsoft DI registration is added, when not plainly: `try` (`TryAdd*`: only when the service has no
    /// registration yet), `enumerable` (`TryAddEnumerable`: unless the same implementation is registered for it) or
    /// `replace` (`Replace`: in place of the service's first registration). `None` for `Add*`, Autofac and the rest.
    pub mode: Option<&'static str>,
}

/// A message-bus fact (C#): a message type (`[ShopMessage(ShopHosts.X)]`), a call publishing or sending
/// one (`bus.Publish(new X())`), or a consumer of one (`: ShopConsumer<X, Self>`, `: IConsumer<X>`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BusFact {
    /// `message`, `publish` or `consume`.
    pub kind: &'static str,
    /// The message type, the method publishing, or the consumer class.
    pub sym: usize,
    /// The message type: for `message`, the type's dotted name in its namespace (`Outer.X`); otherwise as written
    /// (`X`, `Events.X`, an alias expanded). `None` for a publish whose argument's type isn't known syntactically.
    pub message: Option<String>,
    /// For `message`, its namespace (or nothing at top level). Otherwise where a name written here may live,
    /// nearest first: the enclosing namespace and its parents, then the file's `using`s (framework ones left out).
    pub scopes: Vec<String>,
    /// The virtual host a message type declares (`Orders`) or a publish names (`PublishToHost(e, host)`),
    /// with `/queue` for a send to a named queue; a literal queue name a message declares.
    pub queue: Option<String>,
    /// The call (`Publish`, `PublishToHost`, `SendToQueue`, `Send`) or the consumer's base
    /// (`ShopConsumer`, `IConsumer`).
    pub via: Option<String>,
    pub line: u32,
}

/// An Ansible fact (see [`ansible`]): a `requirement` (`name` the role's name, `value` its `src`), a `role_name` (the
/// name a role's meta gives it), a `vars_file` a play or task loads (`name` the path as written), or what a task
/// `deploys` (`name` is `service`, `image`, `repo`, `vhost`, `queue`, `exchange`, `database`, `url`, `path` or `site`;
/// `value` as written, Jinja included). GitLab CI files keep their facts here too (`ci_*`, see [`gitlabci`]).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AnsibleFact {
    pub kind: &'static str,
    pub sym: Option<usize>,
    pub name: String,
    pub value: String,
    pub line: u32,
}

/// A construct an extractor recognises but doesn't read, so what it would add (a registration, a table, an include,
/// a route) is missing from the facts: answers drawing on its `area` say they are incomplete (`unsupported`) instead
/// of complete. Stored in the index's `unsupported` table (see `kc_store::unsupported`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Unsupported {
    /// `di`, `bus`, `sql`, `ansible`, `terraform`, `ci`, `angular` or `imports` (a C# project whose usings can't be read).
    pub area: &'static str,
    /// What was met, in words: `assembly scan RegisterAssemblyTypes(…)`.
    pub construct: String,
    pub line: u32,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct FileFacts {
    /// Constructs met but not read (see [`Unsupported`]).
    pub unsupported: Vec<Unsupported>,
    pub symbols: Vec<Symbol>,
    pub refs: Vec<Ref>,
    pub relations: Vec<Relation>,
    pub imports: Vec<Import>,
    /// C# `global using` directives (see [`GlobalUsing`]).
    pub global_usings: Vec<GlobalUsing>,
    /// The names imports bind and re-exports pass on (TypeScript, Python).
    pub bindings: Vec<ImportBinding>,
    /// What a TypeScript project configuration says about module resolution.
    pub ts_config: Option<TsConfig>,
    /// Packages declared by a manifest file.
    pub packages: Vec<PackageFact>,
    pub http_routes: Vec<HttpRoute>,
    /// Hosts the repo serves or its clients send to (see [`HostFact`]).
    pub hosts: Vec<HostFact>,
    pub constants: Vec<StringConst>,
    pub ng: Vec<NgFact>,
    /// Dependency-injection registrations (C#).
    pub di: Vec<DiBinding>,
    /// Entity Framework entities, table mappings, `Set<T>()` accesses and stored procedure calls.
    pub db: Vec<DbFact>,
    /// Message-bus messages, publishes and consumers (C#).
    pub bus: Vec<BusFact>,
    /// Ansible requirements, role names, vars files and what tasks deploy; GitLab CI includes, variables and scripts.
    pub ansible: Vec<AnsibleFact>,
    /// Per call reference (by index in `refs`), its arguments' types as far as the call site tells them, as JSON: an
    /// encoded type or `null` per argument, a named one as `{"n": name, "t": type}` (`["string", {"n": "id", "t":
    /// null}]`). Absent when an argument is passed `out` or `ref`. Swift: the call's argument labels instead
    /// (`{"l": [null, "from"], "t": [null]}`, see `swift::call_labels`).
    pub arg_types: std::collections::BTreeMap<usize, String>,
    /// The member references (by index in `refs`) that assign the member: `x.P = …`, `P = …` in the class declaring
    /// `P`, `new T { P = … }`. C# only.
    pub assigns: std::collections::BTreeSet<usize>,
    /// Per type reference or creation (by index in `refs`), how it uses the type when that isn't a plain use (C#):
    /// `type_argument` (`Get<IOrders>()`, `AddScoped<IFoo, Foo>()`), `typeof`, `nameof`, `constraint` (`where T :
    /// IFoo`), `target_typed` (a creation `new(…)` that doesn't write the type's name).
    pub usages: std::collections::BTreeMap<usize, String>,
    /// Per method (by index in `symbols`), its parameters as JSON: their declared types, an encoded type or `null` for
    /// a type parameter of the method or its type, and their names (`{"types": ["string", null], "names": ["s", "x"]}`).
    /// An extension method's `this` isn't one. C# only.
    pub param_types: std::collections::BTreeMap<usize, String>,
    /// Number of ERROR or MISSING nodes tree-sitter produced.
    pub parse_errors: u32,
    /// The file runs top-level statements (a script or `main.swift`): its globals and free functions serve that
    /// file, not the rest of the repo.
    pub script: bool,
}

/// Depth of the control-flow block `node` within the callable spanning bytes `outer` (`node` itself is level 1):
/// the number of `nest` kinds on the path up to the callable, not counting `else if` links.
fn nesting_depth(node: tree_sitter::Node, nest: &[&str], outer: (u32, u32)) -> u32 {
    let mut depth = 0;
    let mut n = Some(node);
    while let Some(c) = n {
        if c.start_byte() as u32 <= outer.0 && c.end_byte() as u32 >= outer.1 {
            break;
        }
        if nest.contains(&c.kind()) && !is_else_if(c) {
            depth += 1;
        }
        n = c.parent();
    }
    depth
}

/// The deepest nesting of `nest` kinds within `node`, counting `node` itself as 1 (and `else if` links as their
/// `if`'s level).
fn max_depth(node: tree_sitter::Node, nest: &[&str]) -> u32 {
    let own = (nest.contains(&node.kind()) && !is_else_if(node)) as u32;
    let mut deepest = 0;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        deepest = deepest.max(max_depth(child, nest));
    }
    own + deepest
}

/// Records `node` as a bump of `sym` when it is a top-level control-flow block of the function (depth 1) that nests
/// at least two deep.
fn record_bump(sym: &mut Symbol, node: tree_sitter::Node, nest: &[&str], depth: u32) {
    if depth == 1 && !is_else_if(node) && max_depth(node, nest) >= 2 {
        sym.bumps.push((node.start_position().row as u32 + 1, node.end_position().row as u32 + 1));
    }
}

/// Branches whose `condition` field [`record_condition`] reads, across the C# and TypeScript grammars.
const CONDITION_KINDS: &[&str] =
    &["if_statement", "while_statement", "do_statement", "for_statement", "conditional_expression", "ternary_expression"];

/// `&&` and `||` operators within a condition, not counting those inside lambdas or nested functions.
fn condition_ops(node: tree_sitter::Node, src: &str) -> u32 {
    if matches!(node.kind(), "lambda_expression" | "arrow_function" | "function_expression" | "anonymous_method_expression") {
        return 0;
    }
    let own = (node.kind() == "binary_expression"
        && node.child_by_field_name("operator").is_some_and(|o| matches!(&src[o.byte_range()], "&&" | "||"))) as u32;
    let mut cursor = node.walk();
    own + node.children(&mut cursor).map(|c| condition_ops(c, src)).sum::<u32>()
}

/// Updates `sym`'s most complex condition with `node`'s, when `node` is a branch with a condition.
fn record_condition(sym: &mut Symbol, node: tree_sitter::Node, src: &str) {
    if !CONDITION_KINDS.contains(&node.kind()) {
        return;
    }
    let Some(cond) = node.child_by_field_name("condition") else { return };
    let ops = condition_ops(cond, src);
    if ops > sym.cond_ops {
        sym.cond_ops = ops;
        sym.cond_line = cond.start_position().row as u32 + 1;
    }
}

/// An `if` that is the `else` branch of another `if`: an `else_clause` child, or a direct child of an `if` of the same
/// kind that isn't its then-branch.
fn is_else_if(node: tree_sitter::Node) -> bool {
    let Some(p) = node.parent() else { return false };
    p.kind() == "else_clause"
        || (p.kind() == node.kind() && node.kind().starts_with("if") && p.child_by_field_name("consequence") != Some(node))
}

/// Libraries vendored into a repo by copying, recognised by the file's first word or a directory's: `jquery.form.js`,
/// `highcharts.src.js`, `ace-editor-122/`.
const JS_LIBRARIES: &[&str] = &[
    "ace", "angular", "backbone", "bootstrap", "chart", "ckeditor", "codemirror", "d3", "datatables", "dojo", "dropzone",
    "echarts", "fullcalendar", "handlebars", "highcharts", "highstock", "html5shiv", "jquery", "jscal", "json2", "kendo",
    "knockout", "lodash", "microsoftajax", "microsoftmvc", "modernizr", "moment", "mootools", "mustache", "phantomjs",
    "plupload", "popper", "prototype", "qforms", "raphael", "react", "require", "respond", "select2", "summernote",
    "sweetalert", "swfobject", "tinymce", "toastr", "underscore", "vue", "webforms",
];

/// Directories holding vendored or built JavaScript.
const JS_VENDOR_DIRS: &[&str] =
    &["dist", "bundles", "plugins", "oldplugins", "external", "third-party", "thirdparty", "vendors", "bower_components", "jspm_packages", "webjars", "i18n"];

/// Of [`JS_VENDOR_DIRS`], those an app's own sources also use (`src/plugins/player/plugin.js`): vendored only outside
/// a `src` directory, or under a web root within it (`src/Admin/wwwroot/plugins/`).
const JS_VENDOR_DIRS_OUTSIDE_SRC: &[&str] = &["plugins", "oldplugins"];

/// A JavaScript path that is built or vendored rather than written: minified (`.min.js`), bundled (`.bundle.js`,
/// `*-bundle.js`, `.chunk.js`), a library's unminified build (`.src.js`) or a known library by name
/// ([`JS_LIBRARIES`]); or under a build-output or vendored directory ([`JS_VENDOR_DIRS`]), a versioned one
/// (`jquery-ui-1.8.2/`, `phantomjs.exe.1.8.1/`), one named for a library, or `lib`/`libs` under a web root (`wwwroot`,
/// `js`, `scripts`, `static`, `assets`, `content`, `public`). A `plugins` directory under `src` is the app's own code.
/// `node_modules` and `vendor` are never indexed at all.
fn is_built_js(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let parts: Vec<&str> = lower.split('/').collect();
    let (file, dirs) = parts.split_last().unwrap_or((&"", &[]));
    if [".min.", ".bundle.", "-bundle.", ".chunk.", ".pack."].iter().any(|m| file.contains(m)) || file.ends_with(".src.js") {
        return true;
    }
    let first_word = |s: &str| s.split(['.', '-', '_']).next().unwrap_or("").to_string();
    if JS_LIBRARIES.contains(&first_word(file).as_str()) {
        return true;
    }
    // `name-1.8.2`, `name_v3`, `name.exe.1.8.1`: a name, a separator, then an optional `v` and a dotted version.
    let versioned = |d: &str| {
        let version = d.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
        let name = version.strip_suffix('v').unwrap_or(version);
        version.len() < d.len()
            && name.ends_with(['-', '_', '.'])
            && name.bytes().any(|b| b.is_ascii_alphabetic())
            && d[version.len()..].starts_with(|c: char| c.is_ascii_digit())
    };
    const WEB_ROOTS: &[&str] = &["wwwroot", "js", "scripts", "static", "assets", "content", "public"];
    let in_src = |i: usize| dirs[..i].contains(&"src") && !dirs[..i].iter().any(|d| WEB_ROOTS.contains(d));
    let vendor_dir = |i: usize, d: &str| JS_VENDOR_DIRS.contains(&d) && !(JS_VENDOR_DIRS_OUTSIDE_SRC.contains(&d) && in_src(i));
    dirs.iter().enumerate().any(|(i, d)| vendor_dir(i, d) || versioned(d) || JS_LIBRARIES.contains(&first_word(d).as_str()))
        || dirs.windows(2).any(|w| {
            matches!(w[1], "lib" | "libs") && WEB_ROOTS.contains(&w[0])
        })
}

pub fn extract(lang: Lang, source: &str) -> FileFacts {
    extract_file(lang, "", source)
}

/// [`extract`] for a file at `path` (repo-relative), for formats whose facts depend on the file name.
/// The facts of a package's JavaScript or TypeScript source (`tsx` for JavaScript and `.tsx`), read for the requests
/// its methods make: a package's own code is a library by definition, so a license banner doesn't leave it out.
pub fn extract_package_source(source: &str, tsx: bool) -> FileFacts {
    typescript::extract(source, tsx)
}

pub fn extract_file(lang: Lang, path: &str, source: &str) -> FileFacts {
    match lang {
        Lang::Swift => swift::extract(source),
        Lang::Python => python::extract(source),
        Lang::Rust => rust::extract(source),
        Lang::CSharp => csharp::extract(source),
        Lang::TypeScript => typescript::extract(source, false),
        Lang::Tsx => typescript::extract(source, true),
        Lang::JavaScript if typescript::is_minified(source) || typescript::is_library(source) => FileFacts::default(),
        Lang::JavaScript => typescript::extract(source, true),
        Lang::SwiftPackage => manifest::swiftpm(source),
        Lang::XcodeGen => manifest::xcodegen(source),
        Lang::Cargo => manifest::cargo(source),
        Lang::CsProj => {
            let file = path.rsplit('/').next().unwrap_or(path);
            manifest::csproj(source, file.strip_suffix(".csproj").unwrap_or(file))
        }
        Lang::PackageJson => manifest::package_json(source),
        Lang::Html => html::extract(source),
        Lang::Cfml => cfml::extract(path, source),
        Lang::Sql => sql::extract_at(path, source),
        Lang::Ansible => ansible::extract(path, source),
        Lang::GitlabCi => gitlabci::extract(path, source),
        Lang::Terraform => terraform::extract(source),
        Lang::TsConfig => tsconfig::extract(source),
        Lang::HostConfig => hosts::extract(path, source),
    }
}

/// A source file's text: UTF-16 when it starts with a UTF-16 byte order mark (SQL Server tools save scripts so),
/// else UTF-8, invalid bytes replaced. A UTF-8 byte order mark is kept, so offsets into the text are offsets into the
/// file. For UTF-16 they are offsets into the decoded text: read the file through this function to slice it.
pub fn decode(bytes: &[u8]) -> std::borrow::Cow<'_, str> {
    let units = |b: &[u8], le: bool| -> Vec<u16> {
        b.as_chunks::<2>().0.iter().map(|&c| if le { u16::from_le_bytes(c) } else { u16::from_be_bytes(c) }).collect()
    };
    match bytes {
        [0xFF, 0xFE, rest @ ..] => std::borrow::Cow::Owned(String::from_utf16_lossy(&units(rest, true))),
        [0xFE, 0xFF, rest @ ..] => std::borrow::Cow::Owned(String::from_utf16_lossy(&units(rest, false))),
        _ => String::from_utf8_lossy(bytes),
    }
}

/// The tree-sitter grammar a source language is parsed with, for tools that query syntax trees themselves
/// (structural search). `None` for manifests and HTML.
pub fn grammar(lang: Lang) -> Option<tree_sitter::Language> {
    Some(match lang {
        Lang::Swift => tree_sitter_swift::LANGUAGE.into(),
        Lang::Python => tree_sitter_python::LANGUAGE.into(),
        Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
        Lang::CSharp => tree_sitter_c_sharp::LANGUAGE.into(),
        Lang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Lang::Tsx | Lang::JavaScript => tree_sitter_typescript::LANGUAGE_TSX.into(),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn javascript_is_read_unless_built() {
        for p in ["src/cart.js", "server/index.mjs", "config.cjs", "web/View.jsx", "wwwroot/js/site.js"] {
            assert_eq!(Lang::from_path(p), Some(Lang::JavaScript), "{p}");
        }
        for p in [
            "web/jquery.min.js",
            "dist/app.js",
            "static/app.bundle.js",
            "wwwroot/lib/bootstrap/js/bootstrap.js",
            "js/main-bundle.js",
            "x/bower_components/a.js",
            "Site/js/highcharts.src.js",
            "Site/js/jquery.form.js",
            "editor/ace-editor-122/mode-sql.js",
            "js/jquery-ui-1.8.2/ui.core.js",
            "web/plugins/slider.js",
            "src/Admin/wwwroot/plugins/slider.js",
            "static/js/lib/util.js",
            "js/qForms_v2/cal.js",
        ] {
            assert_eq!(Lang::from_path(p), None, "{p}");
        }
        let f = extract(Lang::JavaScript, "export function total(items) {\n  return items.reduce((a, b) => a + b, 0);\n}\nconst View = () => <div />;\n");
        let names: Vec<&str> = f.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"total") && names.contains(&"View"), "{names:?}");
        let packed = format!("var a=[{}];\nfunction f(){{}}\n", "1,".repeat(600));
        assert!(extract(Lang::JavaScript, &packed).symbols.is_empty());
        let banner = "/*! Chosen v1.2 | (c) 2011 Harvest | Licensed under the MIT license */\nfunction chosen() {}\n";
        assert!(extract(Lang::JavaScript, banner).symbols.is_empty());
        for p in ["lib/index.js", "api/v2/routes.js", "migrations/2024-09/up.js", "src/es2015/a.js", "src/plugins/htmlVideoPlayer/plugin.js"] {
            assert_eq!(Lang::from_path(p), Some(Lang::JavaScript), "{p}");
        }
    }

    #[test]
    fn utf16_files_decode_to_their_text() {
        let text = "CREATE TABLE t (id int);\n";
        let le: Vec<u8> = [0xFF, 0xFE].into_iter().chain(text.encode_utf16().flat_map(u16::to_le_bytes)).collect();
        let be: Vec<u8> = [0xFE, 0xFF].into_iter().chain(text.encode_utf16().flat_map(u16::to_be_bytes)).collect();
        assert_eq!(decode(&le), text);
        assert_eq!(decode(&be), text);
        assert_eq!(decode(text.as_bytes()), text);
        let f = extract_file(Lang::Sql, "t.sql", &decode(&le));
        assert_eq!(f.symbols[0].name, "t");
    }

    #[test]
    fn a_byte_order_mark_does_not_break_parsing_or_shift_offsets() {
        let src = "\u{feff}using Autofac;\nusing System;\n\nnamespace Acme.Api.Controllers\n{\n    public class DataController : Controller\n    {\n        [HttpGet(\"GetResult\")]\n        public Task GetResult() => null;\n    }\n}\n";
        let f = extract_file(Lang::CSharp, "DataController.cs", src);
        assert_eq!(f.parse_errors, 0);
        let class = f.symbols.iter().find(|s| s.name == "DataController").expect("class");
        assert_eq!(class.kind, SymbolKind::Class);
        // Byte offsets still index the original text, mark included.
        assert!(src[class.start_byte as usize..].starts_with("public class DataController"));
        assert_eq!(f.http_routes.len(), 1);
    }

    /// `run` has two bumps (an `if` holding an `if`, and a loop holding an `if` chain); the `else if` chain and the
    /// flat `if` are not bumps. Its most complex condition has 4 operators (the lambda's `&&` doesn't count).
    #[test]
    fn bumps_and_complex_conditions() {
        let cases: &[(Lang, &str)] = &[
            (
                Lang::CSharp,
                "class C {\n  void run(int[] xs) {\n    if (a) {\n      if (b) { }\n    }\n    if (a) {} else if (b) {} else if (c) {}\n    if (d) { Go(); }\n    foreach (var x in xs) {\n      if (x > 0) { } else if (x < 0) { }\n    }\n    while (a && (b || c) && !d && xs.Any(y => y > 0 && y < 9)) { }\n  }\n  int flat() { return a && b ? 1 : 0; }\n}\n",
            ),
            (
                Lang::TypeScript,
                "export function run(xs: number[]) {\n  if (a) {\n    if (b) { }\n  }\n  if (a) {} else if (b) {} else if (c) {}\n  if (d) { go(); }\n  for (const x of xs) {\n    if (x > 0) { } else if (x < 0) { }\n  }\n  while (a && (b || c) && !d && xs.some(y => y > 0 && y < 9)) { }\n}\nfunction flat() { return a && b ? 1 : 0; }\n",
            ),
        ];
        for (lang, src) in cases {
            let f = extract(*lang, src);
            let sym = |name: &str| f.symbols.iter().find(|s| s.name == name).unwrap();
            let run = sym("run");
            let first = if *lang == Lang::CSharp { 3 } else { 2 };
            assert_eq!(run.bumps, [(first, first + 2), (first + 5, first + 7)], "{lang:?}");
            assert_eq!((run.cond_ops, run.cond_line), (4, first + 8), "{lang:?}");
            assert!(sym("flat").bumps.is_empty());
            assert_eq!(sym("flat").cond_ops, 1, "{lang:?}");
        }
    }

    /// `run` nests a loop, an `if` and a `try` (3 deep) after an `else if` chain that stays at one level; `flat`
    /// has none.
    #[test]
    fn nesting_depth_per_function() {
        let cases: &[(Lang, &str)] = &[
            (
                Lang::Python,
                "def run(xs):\n    if a:\n        pass\n    elif b:\n        pass\n    for x in xs:\n        if x:\n            try:\n                go()\n            except E:\n                pass\n\ndef flat():\n    return 1\n",
            ),
            (
                Lang::Rust,
                "fn run(xs: &[u8]) {\n    if a { } else if b { } else if c { }\n    for x in xs {\n        if *x > 0 {\n            match x { _ => {} }\n        }\n    }\n}\nfn flat() -> u8 { 1 }\n",
            ),
            (
                Lang::TypeScript,
                "export function run(xs: number[]) {\n  if (a) {} else if (b) {} else if (c) {}\n  for (const x of xs) {\n    if (x) {\n      try { go(); } catch (e) {}\n    }\n  }\n}\nfunction flat() { return 1; }\n",
            ),
            (
                Lang::CSharp,
                "class C {\n  void run(int[] xs) {\n    if (a) {} else if (b) {} else if (c) {}\n    foreach (var x in xs) {\n      if (x > 0) {\n        try { Go(); } catch (E e) {}\n      }\n    }\n  }\n  int flat() { return 1; }\n}\n",
            ),
            (
                Lang::Swift,
                "func run(xs: [Int]) {\n    if a {} else if b {} else if c {}\n    for x in xs {\n        if x > 0 {\n            switch x { default: break }\n        }\n    }\n}\nfunc flat() -> Int { 1 }\n",
            ),
        ];
        for (lang, src) in cases {
            let f = extract(*lang, src);
            let depth = |name: &str| f.symbols.iter().find(|s| s.name == name).map(|s| s.nesting);
            assert_eq!(depth("run"), Some(3), "{lang:?}");
            assert_eq!(depth("flat"), Some(0), "{lang:?}");
        }
    }
}

