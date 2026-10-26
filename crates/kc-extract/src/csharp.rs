//! C# extraction over tree-sitter-c-sharp: syntactic, shallower than Swift.
//!
//! Classes, records, structs, interfaces (as protocols), enums and delegates (as typealiases); methods,
//! constructors (as `init`), properties, fields and enum members. Namespaces don't nest names: `Acme.Cart.CartService`
//! is `CartService`, and partial classes across files merge like extensions. C# names methods in PascalCase, so
//! call kinds come from syntax (`new T()` instantiates), not from case. Top-level statements make a script.
//!
//! Entity Framework: `DbSet<T>` properties, table mappings (`[Table]`, `ToTable` on an entity builder), `Set<T>()`
//! accesses and stored procedure calls are recorded as [`DbFact`]s; which code touches which table is decided at
//! query time.

use std::collections::HashMap;

use tree_sitter::{Node, Parser};

use crate::swift::{children, collapse, line, name_col, starts_upper};
use crate::types::{PathRoot, Seg, TypeDecl, TypeExpr};
use crate::urls::{self, Piece};
use crate::{
    BusFact, DbFact, DiBinding, FileFacts, HostFact, HttpRoute, Import, Ref, RefKind, Relation, RelationKind, StringConst, Symbol, SymbolKind,
    Unsupported,
};

pub const VERSION: u32 = 35;

const SIGNATURE_CAP: usize = 240;
const RECEIVER_CAP: usize = 48;

/// Framework types whose uses say nothing about the code base.
const COMMON_TYPES: &[&str] = &[
    "Action", "DateTime", "Dictionary", "Exception", "Func", "Guid", "ICollection", "IDictionary", "IEnumerable",
    "IList", "IReadOnlyCollection", "IReadOnlyDictionary", "IReadOnlyList", "HashSet", "List", "Nullable", "Task",
    "TimeSpan", "ValueTask", "var",
];

const BRANCH_KINDS: &[&str] = &[
    "if_statement", "for_statement", "foreach_statement", "while_statement", "do_statement", "catch_clause",
    "conditional_expression", "switch_section", "switch_expression_arm",
];

/// Control-flow blocks that nest, for [`Symbol::nesting`](crate::Symbol::nesting).
const NEST_KINDS: &[&str] = &[
    "if_statement", "for_statement", "foreach_statement", "while_statement", "do_statement", "switch_statement",
    "switch_expression", "try_statement",
];

/// LINQ and `List<T>` methods whose lambda parameter is an element of the receiver.
const ELEMENT_LAMBDAS: &[&str] = &[
    "All", "Any", "Average", "Count", "DistinctBy", "Exists", "Find", "FindAll", "FindIndex", "First",
    "FirstOrDefault", "ForEach", "GroupBy", "Last", "LastOrDefault", "Max", "MaxBy", "Min", "MinBy", "OrderBy",
    "OrderByDescending", "RemoveAll", "Select", "SelectMany", "Single", "SingleOrDefault", "SkipWhile", "Sum",
    "TakeWhile", "ThenBy", "ThenByDescending", "ToDictionary", "ToLookup", "TrueForAll", "Where",
];

/// Query operators whose lambdas EF translates to SQL over the elements (and the navigations they name).
const LINQ_OPS: &[&str] = &[
    "All", "Any", "AnyAsync", "AllAsync", "Average", "Count", "CountAsync", "First", "FirstAsync", "FirstOrDefault",
    "FirstOrDefaultAsync", "GroupBy", "Max", "MaxAsync", "Min", "MinAsync", "OrderBy", "OrderByDescending", "Select",
    "SelectMany", "Single", "SingleAsync", "SingleOrDefault", "SingleOrDefaultAsync", "Sum", "SumAsync", "ThenBy",
    "ThenByDescending", "Where",
];

/// Moq methods whose lambda parameter is the mocked object (`Mock<T>` is read as `T`).
const MOCK_LAMBDAS: &[&str] = &["Setup", "SetupGet", "SetupSet", "SetupSequence", "Verify", "VerifyGet", "VerifySet"];

/// Argument matchers (Moq, NSubstitute) whose lambda parameter is the matcher's type argument: `It.Is<T>(x => …)`.
const MATCHER_LAMBDAS: &[(&str, &str)] = &[("It", "Is"), ("Arg", "Is"), ("Arg", "Do")];

/// Generic methods returning (a mock of) their one type argument: test doubles (`GetMock<T>()`, `Create<T>()`),
/// service locators (`Resolve<T>()`, `GetService<T>()`, `GetInstance<T>()`), configuration and deserializers
/// (`Get<T>()`, `DeserializeObject<T>(s)`), and assertions returning the exception thrown (`Should.Throw<T>(…)`).
const TYPE_ARG_FACTORIES: &[&str] = &[
    "Create", "CreateInstance", "Deserialize", "DeserializeObject", "Freeze", "Get", "GetInstance", "GetKeyedService",
    "GetMock", "GetRequiredKeyedService", "GetRequiredService", "GetService", "GetValue", "Mock", "Resolve",
    "ResolveKeyed", "ResolveNamed", "ResolveOptional", "Throw", "ThrowAsync", "Throws", "ThrowsAny", "ThrowsAnyAsync",
    "ThrowsAsync",
];

/// Keyed service locators taking the service and then the key's type: `resolver.GetKeyedInstance<IFoo, Tenants>(k)`.
const KEYED_FACTORIES: &[&str] = &["GetKeyedInstance", "ResolveKeyed", "GetKeyedService", "GetRequiredKeyedService"];

/// Entity Framework entity builder methods whose lambda parameter is the entity: `Property(e => e.Name)`.
const ENTITY_LAMBDAS: &[&str] = &[
    "HasAlternateKey", "HasIndex", "HasKey", "HasMany", "HasOne", "HasOptional", "HasQueryFilter", "HasRequired",
    "Ignore", "Navigation", "OwnsMany", "OwnsOne", "Property",
];

/// FluentValidation methods whose lambda parameter is the validated type: `RuleFor(x => x.Name)`.
const VALIDATOR_LAMBDAS: &[&str] = &["RuleFor", "RuleForEach", "When", "Unless", "WhenAsync", "UnlessAsync"];

const TEST_ATTRS: &[&str] = &["Fact", "Theory", "Test", "TestCase", "TestMethod", "DataTestMethod"];

pub fn extract(source: &str) -> FileFacts {
    let source = &*blank_verbatim_doubled_quotes(source);
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_c_sharp::LANGUAGE.into()).expect("tree-sitter-c-sharp ABI matches tree-sitter");
    let Some(mut tree) = parser.parse(source, None) else {
        return FileFacts { parse_errors: 1, ..Default::default() };
    };
    // A conditional splitting a declaration loses it: read the file with each conditional's first branch, when that
    // parses better.
    let first;
    let mut source = source;
    if tree.root_node().has_error()
        && let Some(text) = first_branches(source)
        && let Some(t) = parser.parse(&text, None)
        && parse_errors(t.root_node()) < parse_errors(tree.root_node())
    {
        first = text;
        source = &first;
        tree = t;
    }
    let conv = crate::conventions::current();
    // Only a file that publishes can have publish helpers.
    let publishes = conv.bus.publish.iter().any(|c| source.contains(c.name.as_str()));
    let mut cx = Cx {
        src: source,
        conv,
        facts: FileFacts::default(),
        scopes: Vec::new(),
        seen: HashMap::new(),
        routes: Vec::new(),
        controllers: Vec::new(),
        builders: Vec::new(),
        configured: Vec::new(),
        validated: Vec::new(),
        child_rules: Vec::new(),
        mapping_sources: Vec::new(),
        namespaces: Vec::new(),
        aliases: HashMap::new(),
        block_aliases: Vec::new(),
        injected: Vec::new(),
        pending_arg_types: None,
        usage: None,
        bare_members: Vec::new(),
        bound: HashMap::new(),
        helpers: publishes.then(BusHelpers::default),
        group_vars: Vec::new(),
        handlers: Vec::new(),
        method_groups: Vec::new(),
        string_fields: Vec::new(),
    };
    cx.visit(tree.root_node());
    cx.resolve_bus_helpers();
    cx.anchor_handlers();
    cx.method_group_refs();
    cx.bare_member_reads();
    cx.sql_holders();
    cx.tag_injected();
    cx.expand_aliases();
    cx.facts
}

struct Scope {
    sym: usize,
    /// The dotted type whose members this scope declares.
    type_name: Option<String>,
    callable: bool,
    locals: HashMap<String, TypeExpr>,
}

/// A bare name read as a receiver, before the file's members are all known (see [`Cx::bare_members`]).
struct BareMember {
    name: String,
    /// The enclosing symbol.
    from: Option<usize>,
    line: u32,
    col: u32,
    /// The enclosing type.
    owner: String,
    assigned: bool,
}

/// How a type's actions are routed, for [`Cx::action_route`].
#[derive(Debug, Clone, Default)]
struct ControllerScope {
    /// The prefix is left to the query: the type derives from a base that may give one, names its own `[Route]` by
    /// a constant, or is partial (another part may have the attribute).
    routed: bool,
    /// A concrete `…Controller` with no `[Route]` of its own: conventional routes may serve its public methods.
    conventional: bool,
    /// The class has no `[Route]` of its own, in this part or another (it isn't partial): its prefix can come from
    /// its bases only, never from another class of its name.
    unrouted: bool,
    /// `[Area("x")]`, for `[area]`.
    area: Option<String>,
}

/// `IOrders`: named like an interface, which gives no route prefix.
fn is_interface_name(n: &str) -> bool {
    let mut c = n.chars();
    c.next() == Some('I') && c.next().is_some_and(|x| x.is_ascii_uppercase())
}

struct Cx<'a> {
    src: &'a str,
    /// The conventions recognised by name (see [`crate::conventions`]).
    conv: std::sync::Arc<crate::conventions::Conventions>,
    facts: FileFacts,
    scopes: Vec<Scope>,
    /// Each reference recorded, to its index in `facts.refs`: one per symbol, name, kind, line and column, so two
    /// occurrences on a line stay two references.
    seen: HashMap<(Option<usize>, String, RefKind, u32, u32), usize>,
    /// Enclosing types' names and `[Route]` templates, innermost last, for controller actions' routes.
    routes: Vec<(String, Option<String>, Option<usize>)>,
    /// Per enclosing type, beside `routes`: how its actions' routes are placed (see [`ControllerScope`]).
    controllers: Vec<ControllerScope>,
    /// Identifiers bound to an EF entity builder, with the entity: a lambda parameter of `Entity<T>(b => …)`, an
    /// `EntityTypeBuilder<T>` parameter, or a local holding `modelBuilder.Entity<T>()`. Innermost last.
    builders: Vec<(String, String)>,
    /// Per enclosing type, the entity an EF6 `EntityTypeConfiguration<T>` subclass configures (bare `ToTable("x")`).
    configured: Vec<Option<String>>,
    /// Per enclosing type, the type a FluentValidation `AbstractValidator<T>` subclass validates (a tuple, too).
    validated: Vec<Option<TypeExpr>>,
    /// Lambda parameters of FluentValidation's `ChildRules(c => …)`, with the type the child rules validate: the
    /// elements of a `RuleForEach` property, or a `RuleFor` property. Innermost last.
    child_rules: Vec<(String, TypeExpr)>,
    /// Source types of the AutoMapper `ForMember` calls whose arguments are being visited, innermost last.
    mapping_sources: Vec<String>,
    /// Enclosing namespaces, full names, innermost last. A file-scoped namespace stays to the end of the file.
    namespaces: Vec<String>,
    /// `using Alias = A.B;` in scope here: alias to target. One declared in a namespace block applies only there.
    aliases: HashMap<String, String>,
    /// The aliases in scope in each namespace block that declares its own: the block's lines and those aliases.
    block_aliases: Vec<(u32, u32, HashMap<String, String>)>,
    /// Bare names read as a receiver (`Details.Add(x)` inside the class declaring `Details`). Kept as member references
    /// once the file's members are all known.
    bare_members: Vec<BareMember>,
    /// Per member (by its node's id), the names it binds itself (see [`Self::bound_in_member`]).
    bound: HashMap<usize, std::collections::HashSet<String>>,
    /// The argument types of the call being recorded (see [`Self::argument_types`]), for [`Self::add_call`].
    pending_arg_types: Option<String>,
    /// How the type references being recorded use the type, when it isn't plain (see [`FileFacts::usages`]).
    usage: Option<&'static str>,
    /// Members (type, name) holding what a constructor is given: assigned a constructor's parameter, or initialized
    /// from a primary constructor's. Tagged `injected` once the file is read.
    injected: Vec<(String, String)>,
    /// Publishes settled once the file is read (see [`Cx::resolve_bus_helpers`]); `None` in a file that never says
    /// `Publish` or `Send`.
    helpers: Option<BusHelpers>,
    /// Locals holding a minimal API route group, per enclosing symbol: the group's prefix (see
    /// [`Cx::route_base`]), or why it can't be known.
    group_vars: Vec<(Option<usize>, String, Result<String, &'static str>)>,
    /// Endpoints whose handler is a method group, anchored to its declaration once the file is read.
    handlers: Vec<PendingHandler>,
    /// Method groups passed as a delegate (`Subscribe(Handle)`), kept as references once the file's members are
    /// known; `true` for a local function's.
    method_groups: Vec<(BareMember, bool)>,
    /// `readonly string` fields holding a literal, by type and name: constants a URL can name in camelCase.
    string_fields: Vec<(String, String)>,
}

/// An endpoint (index into `facts.http_routes`) whose handler is a method group: `name`, qualified as written
/// (`CatalogApi.GetItem`), registered inside `owner`.
struct PendingHandler {
    route: usize,
    name: String,
    qualifier: Option<String>,
    owner: Option<String>,
}

/// Publishes whose message type is known only once the whole file is read: helper methods publishing what their
/// callers pass, the calls that may reach them, and publishes of what a method of the file builds.
#[derive(Default)]
struct BusHelpers {
    forwards: Vec<Forward>,
    calls: Vec<HelperCall>,
    /// A publish fact (index into `facts.bus`) of what a method builds: the method's owner as written, and its name.
    built: Vec<(usize, Option<String>, String)>,
}

/// A method (`sym`) publishing what its caller gives it.
struct Forward {
    sym: usize,
    what: Forwarded,
    /// The unknown-typed publish it makes (index into `facts.bus`), dropped once a caller says what it is; for a
    /// helper passing its type on, the publish at the end of the chain.
    fact: usize,
}

enum Forwarded {
    /// Its type parameter `index`: a type argument of the call, or the type of what is passed as one of `params`
    /// (parameter indices) declared of that type.
    TypeParam { index: usize, params: Vec<usize> },
    /// Its parameter `index`, declared `object`: whatever the caller passes.
    Param(usize),
}

/// An unqualified call (or `this.M(…)`) in a publishing file, kept in case it calls a publish helper.
struct HelperCall {
    from: usize,
    name: String,
    line: u32,
    targs: Vec<String>,
    args: Vec<ArgType>,
    scopes: Vec<String>,
    /// The calling method's type parameters, and its parameters' types as written.
    tparams: Vec<String>,
    params: Vec<String>,
    /// Type parameters of the enclosing types: never a concrete message.
    outer: Vec<String>,
}

/// What an argument's type is known to be.
#[derive(Clone)]
enum ArgType {
    Known(String),
    /// What a method of the file returns: its owner as written (none for the caller's type) and its name.
    Built(Option<String>, String),
    Unknown,
}

impl<'a> Cx<'a> {
    fn text(&self, n: Node) -> &'a str {
        &self.src[n.byte_range()]
    }

    fn current(&self) -> Option<usize> {
        self.scopes.last().map(|s| s.sym)
    }

    fn enclosing_type(&self) -> Option<String> {
        self.scopes.iter().rev().find_map(|s| s.type_name.clone())
    }

    fn visit(&mut self, node: Node) {
        if node.is_error() || node.is_missing() {
            self.facts.parse_errors += 1;
        }
        if (BRANCH_KINDS.contains(&node.kind())
            || (node.kind() == "binary_expression"
                && node.child_by_field_name("operator").is_some_and(|o| matches!(self.text(o), "&&" | "||" | "??"))))
            && let Some(s) = self.scopes.iter().rev().find(|s| s.callable)
        {
            self.facts.symbols[s.sym].complexity += 1;
        }
        if NEST_KINDS.contains(&node.kind())
            && let Some(s) = self.scopes.iter().rev().find(|s| s.callable)
        {
            let sym = &mut self.facts.symbols[s.sym];
            let depth = crate::nesting_depth(node, NEST_KINDS, (sym.start_byte, sym.end_byte));
            sym.nesting = sym.nesting.max(depth);
            crate::record_bump(sym, node, NEST_KINDS, depth);
        }
        if let Some(s) = self.scopes.iter().rev().find(|s| s.callable) {
            crate::record_condition(&mut self.facts.symbols[s.sym], node, self.src);
        }
        if matches!(
            node.kind(),
            "string_literal" | "verbatim_string_literal" | "raw_string_literal" | "interpolated_string_expression" | "binary_expression"
        ) {
            self.sql_string(node);
        }
        match node.kind() {
            "class_declaration" | "struct_declaration" | "record_declaration" | "interface_declaration"
            | "enum_declaration" => self.type_decl(node),
            "delegate_declaration" => {
                if let Some(n) = node.child_by_field_name("name") {
                    let name = self.text(n).to_string();
                    let sym = self.push(node, name, SymbolKind::Typealias, self.enclosing_type(), None);
                    self.facts.symbols[sym].type_name = node.child_by_field_name("type").and_then(|t| type_expr(self.src, t)).map(|t| t.encode());
                    self.in_scope(sym, None, true, HashMap::new(), |cx| cx.visit_children(node));
                }
            }
            "method_declaration" | "constructor_declaration" | "local_function_statement" | "operator_declaration" => {
                self.function(node)
            }
            "property_declaration" | "indexer_declaration" => self.property(node),
            "field_declaration" | "event_field_declaration" => self.field(node),
            "comment" if node.parent().is_some_and(|p| matches!(p.kind(), "declaration_list" | "method_declaration")) => {
                self.commented_routes(node)
            }
            "enum_member_declaration" => {
                if let Some(n) = node.child_by_field_name("name") {
                    let name = self.text(n).to_string();
                    self.push(node, name, SymbolKind::Case, self.enclosing_type(), None);
                }
            }
            "using_directive" => {
                let name = node.child_by_field_name("name").filter(|_| children(node).any(|c| c.kind() == "="));
                // `using X = A.B;` names the alias; the target is what's imported.
                let target = children(node).filter(|c| c.is_named() && Some(*c) != name).last();
                if let Some(t) = target {
                    self.facts.imports.push(Import { module: collapse(self.text(t), 120), line: line(node) });
                    if let Some(n) = name {
                        self.aliases.insert(self.text(n).to_string(), dotted_type(self.src, t));
                    }
                    // `global using X;`: every file of the project imports it.
                    if children(node).any(|c| c.kind() == "global") {
                        self.facts.global_usings.push(crate::GlobalUsing {
                            namespace: if name.is_some() { dotted_type(self.src, t) } else { collapse(self.text(t), 120) },
                            alias: name.map(|n| self.text(n).to_string()),
                            is_static: children(node).any(|c| c.kind() == "static"),
                            line: line(node),
                        });
                    }
                }
            }
            "namespace_declaration" => {
                let name = node.child_by_field_name("name").map(|n| dotted_type(self.src, n)).unwrap_or_default();
                let full = self.namespaces.last().map_or_else(|| name.clone(), |o| format!("{o}.{name}"));
                self.namespaces.push(full);
                let outer = self.aliases.clone();
                self.visit_children(node);
                self.namespaces.pop();
                // The block's own `using` aliases end with it.
                let inner = std::mem::replace(&mut self.aliases, outer);
                if inner != self.aliases {
                    self.block_aliases.push((line(node), node.end_position().row as u32 + 1, inner));
                }
            }
            "file_scoped_namespace_declaration" => {
                // Its declarations are its siblings: the namespace holds to the end of the file.
                if let Some(n) = node.child_by_field_name("name") {
                    let name = dotted_type(self.src, n);
                    self.namespaces.push(name);
                }
            }
            "global_statement" => {
                self.facts.script = true;
                self.visit_children(node);
            }
            "local_declaration_statement" | "using_statement" | "fixed_statement" => {
                self.locals(node);
                self.visit_children(node);
            }
            "catch_declaration" => {
                if let (Some(n), Some(t)) = (node.child_by_field_name("name"), node.child_by_field_name("type"))
                    && let Some(ty) = type_expr(self.src, t)
                {
                    self.bind(self.text(n).to_string(), ty);
                }
                self.visit_children(node);
            }
            "foreach_statement" => {
                if let (Some(l), Some(t)) = (node.child_by_field_name("left"), node.child_by_field_name("type"))
                    && l.kind() == "identifier"
                {
                    let ty = type_expr(self.src, t).or_else(|| {
                        node.child_by_field_name("right").and_then(|r| self.expr_type(r)).map(|r| TypeExpr::Elem(Box::new(r)))
                    });
                    if let Some(ty) = ty {
                        self.bind(self.text(l).to_string(), ty);
                    }
                }
                self.visit_children(node);
            }
            "invocation_expression" => {
                self.call(node);
                self.invoked_type_args(node);
                self.nameof_ref(node);
                self.bus_publish(node);
                self.helper_call(node);
                self.consumer_registration(node);
                if !chain_inner(node) {
                    self.di_registrations(node);
                }
                self.service_descriptor(node);
                self.db_call(node);
                self.outgoing_request(node);
                self.endpoint(node);
                self.app_host(node);
                if let Some(f) = node.child_by_field_name("function") {
                    match f.kind() {
                        "member_access_expression" => {
                            if let Some(e) = f.child_by_field_name("expression") {
                                self.visit(e);
                            }
                        }
                        "identifier" | "generic_name" => {}
                        // `x?.M(…)`: the call is recorded above; `x` is read.
                        "conditional_access_expression" => {
                            if let Some(c) = f.child_by_field_name("condition") {
                                self.visit(c);
                            }
                        }
                        _ => self.visit(f),
                    }
                }
                if let Some(a) = node.child_by_field_name("arguments") {
                    let source = self.lambda_params(node, a);
                    let sources = self.mapping_sources.len();
                    self.mapping_sources.extend(source);
                    let mark = self.builders.len();
                    // `b.OwnsMany(e => e.Payments, p => p.ToTable("x"))`: the second lambda's parameter builds the type
                    // owned through `Payments`.
                    if let Some(owned) = self.owned_call(node) {
                        for lambda in children(a).filter_map(|x| children(x).find(|c| c.kind() == "lambda_expression")).skip(1) {
                            if let Some(p) = lambda_param(lambda) {
                                self.builders.push((self.text(p).to_string(), owned.clone()));
                            }
                        }
                    }
                    // `modelBuilder.Entity<T>(b => b.ToTable("x"))`: the lambda's parameter builds `T`.
                    if let Some(entity) = self.entity_call(node) {
                        for lambda in children(a).filter_map(|x| children(x).find(|c| c.kind() == "lambda_expression")) {
                            if let Some(p) = lambda_param(lambda) {
                                self.builders.push((self.text(p).to_string(), entity.clone()));
                            }
                        }
                    }
                    // `RuleForEach(x => x.Items).ChildRules(c => c.RuleFor(i => …))`: `c` validates an item.
                    let children_mark = self.child_rules.len();
                    if let Some(f) = node.child_by_field_name("function").filter(|f| f.kind() == "member_access_expression")
                        && f.child_by_field_name("name").is_some_and(|n| self.text(n) == "ChildRules")
                        && let Some(target) = f.child_by_field_name("expression").and_then(|r| self.rule_target(r))
                    {
                        for lambda in children(a).filter_map(|x| children(x).find(|c| c.kind() == "lambda_expression")) {
                            if let Some(p) = lambda_param(lambda) {
                                self.child_rules.push((self.text(p).to_string(), target.clone()));
                            }
                        }
                    }
                    self.visit(a);
                    self.child_rules.truncate(children_mark);
                    self.builders.truncate(mark);
                    self.mapping_sources.truncate(sources);
                }
            }
            "object_creation_expression" => {
                self.procedure_call(node);
                self.request_message(node);
                if let Some(t) = node.child_by_field_name("type") {
                    let name = base_type_name(self.src, t);
                    if name == COMPONENT_REGISTRATION
                        && let Some(a) = node.child_by_field_name("arguments")
                    {
                        self.component_registration(a);
                    }
                    if name == SERVICE_DESCRIPTOR {
                        self.service_descriptor(node);
                    }
                    if !name.is_empty() {
                        self.creation(name, type_qualifier(self.src, t), node.child_by_field_name("arguments"), t);
                    }
                    self.type_refs(t, true);
                }
                for f in ["arguments", "initializer"] {
                    if let Some(c) = node.child_by_field_name(f) {
                        self.visit(c);
                    }
                }
            }
            // `Order o = new(id);`, `return new(…);`: the type is the target's.
            "implicit_object_creation_expression" => {
                if let Some((name, qualifier)) = self.target_type(node).filter(|(n, _)| !n.is_empty()) {
                    let args = children(node).find(|c| c.kind() == "argument_list");
                    self.with_usage("target_typed", |cx| cx.creation(name, qualifier, args, node));
                }
                self.visit_children(node);
            }
            // `: base(…)`, `: this(…)`: a call of the base type's or this type's constructor.
            "constructor_initializer" => {
                let receiver = if self.text(node).trim_start_matches(':').trim_start().starts_with("this") { "this" } else { "base" };
                let args = children(node).find(|c| c.kind() == "argument_list");
                // At the `base`/`this` keyword, not the colon, which may end the line before (`… int x) :`).
                let mut cursor = node.walk();
                let keyword = node.children(&mut cursor).find(|c| matches!(c.kind(), "base" | "this")).unwrap_or(node);
                self.chained(receiver, args, keyword);
                if receiver == "base"
                    && let Some(a) = args
                    && self.derives_component_registration(node)
                {
                    self.component_registration(a);
                }
                self.visit_children(node);
            }
            // `where T : IHandler, new()`: the constraint names the type.
            "type_parameter_constraints_clause" => {
                for c in children(node).filter(|c| c.kind() == "type_parameter_constraint") {
                    if let Some(t) = c.child_by_field_name("type") {
                        self.with_usage("constraint", |cx| cx.type_refs(t, false));
                    }
                }
            }
            "member_access_expression" => {
                if let Some(n) = node.child_by_field_name("name") {
                    let e = node.child_by_field_name("expression").map(unbang);
                    let receiver = e.map(|e| collapse(self.text(e), RECEIVER_CAP));
                    let rt = e.and_then(|e| self.expr_type(e)).map(|t| t.encode());
                    let name = name_text(self.src, n);
                    self.add(name, receiver, rt, RefKind::Member, n);
                }
                if let Some(e) = node.child_by_field_name("expression") {
                    self.visit(e);
                }
            }
            // `Details.Add(x)`, `Items.Count`, `Details = …` in the class declaring them: a use of the member (see
            // `bare_member_reads`).
            "identifier"
                if node.parent().is_some_and(|p| {
                    (matches!(p.kind(), "member_access_expression" | "conditional_access_expression")
                        && (p.child_by_field_name("expression") == Some(node) || p.child_by_field_name("condition") == Some(node)))
                        // `Details = new …` in the class declaring `Details` (not `new T { Details = … }`).
                        || (p.kind() == "assignment_expression"
                            && p.child_by_field_name("left") == Some(node)
                            && p.parent().is_none_or(|g| g.kind() != "initializer_expression"))
                }) =>
            {
                let name = self.text(node);
                let assigned = node.parent().is_some_and(|p| p.kind() == "assignment_expression");
                if self.local(name).is_none()
                    && !self.bound_in_member(node, name)
                    && let Some(owner) = self.enclosing_type()
                {
                    self.bare_members.push(BareMember {
                        name: name.to_string(),
                        from: self.current(),
                        line: line(node),
                        col: name_col(self.src, node, name),
                        owner,
                        assigned,
                    });
                }
            }
            // `x?.B`: read as `x.B`.
            "conditional_access_expression" => {
                // The condition first: a plain read of the same name on the line keeps its own receiver.
                if let Some(c) = node.child_by_field_name("condition") {
                    self.visit(c);
                }
                if let Some((e, n)) = conditional_member(node) {
                    let receiver = Some(collapse(self.text(e), RECEIVER_CAP));
                    let rt = self.expr_type(e).map(|t| t.encode());
                    let name = name_text(self.src, n);
                    self.add(name, receiver, rt, RefKind::Member, n);
                }
                for c in children(node).filter(|c| c.kind() != "member_binding_expression" && Some(*c) != node.child_by_field_name("condition")) {
                    self.visit(c);
                }
            }
            // `Subscribe(Handle)`, `Task.Run(Work)`, `x.Changed += OnChanged`: a method group passed as a delegate.
            "identifier" if method_group_position(self.src, node) => self.method_group(node),
            "assignment_expression" => {
                self.command_text(node);
                self.base_address(node);
                // `new Payout { Details = … }`: a write of the created type's member.
                if let Some(created) = node
                    .parent()
                    .filter(|p| p.kind() == "initializer_expression")
                    .and_then(|p| p.parent())
                    .filter(|g| g.kind() == "object_creation_expression")
                    .and_then(|g| g.child_by_field_name("type"))
                    .and_then(|t| type_expr(self.src, t))
                    && let Some(left) = node.child_by_field_name("left").filter(|l| l.kind() == "identifier")
                {
                    let name = self.text(left).to_string();
                    self.add(name.clone(), None, Some(created.encode()), RefKind::Member, left);
                    // `new Batch { Domains = new List<Domain>() }`: a new entity's empty collection adds no rows.
                    if !node.child_by_field_name("right").is_some_and(|r| empty_value(self.src, r)) {
                        self.assigned(name, left);
                    }
                }
                self.visit_children(node);
                // `x.Details = …`, `this.Details = …`: the member reference recorded above assigns.
                if let Some(n) = node
                    .child_by_field_name("left")
                    .filter(|l| l.kind() == "member_access_expression")
                    .and_then(|l| l.child_by_field_name("name"))
                {
                    self.assigned(name_text(self.src, n), n);
                }
            }
            "from_clause" | "join_clause" => {
                // `from d in ctx.Orders`: a query over the member.
                let source = children(node).skip_while(|c| c.kind() != "in").find(|c| c.is_named());
                if let Some(name) = source.and_then(|s| self.queried_member(s)) {
                    self.db(DbFact { kind: "query", sym: self.current(), entity: String::new(), name, line: line(node) });
                }
                self.visit_children(node);
            }
            // `Get<IOrders>()`, `Foo<Bar>.Baz`: each type argument is a use of its type.
            "type_argument_list" => {
                for t in children(node).filter(|c| c.is_named()) {
                    self.with_usage("type_argument", |cx| cx.type_refs(t, false));
                }
            }
            "variable_declaration" | "parameter" | "cast_expression" | "typeof_expression" | "declaration_expression"
            | "is_pattern_expression" | "as_expression" | "default_expression" => {
                if let Some(t) = node.child_by_field_name("type") {
                    if node.kind() == "typeof_expression" {
                        self.with_usage("typeof", |cx| cx.type_refs(t, false));
                    } else {
                        self.type_refs(t, false);
                    }
                }
                for c in children(node) {
                    if Some(c) != node.child_by_field_name("type") {
                        self.visit(c);
                    }
                }
            }
            _ => self.visit_children(node),
        }
    }

    fn visit_children(&mut self, node: Node) {
        for c in children(node) {
            self.visit(c);
        }
    }

    fn in_scope(&mut self, sym: usize, type_name: Option<String>, callable: bool, locals: HashMap<String, TypeExpr>, f: impl FnOnce(&mut Self)) {
        let mark = self.builders.len();
        self.scopes.push(Scope { sym, type_name, callable, locals });
        f(self);
        self.scopes.pop();
        self.builders.truncate(mark);
    }

    fn bind(&mut self, name: String, ty: TypeExpr) {
        if let Some(s) = self.scopes.iter_mut().rev().find(|s| s.callable) {
            s.locals.insert(name, ty);
        }
    }

    fn push(&mut self, node: Node, name: String, kind: SymbolKind, container: Option<String>, header_stop: Option<Node>) -> usize {
        let end = node.child_by_field_name("body").or(header_stop).map_or(node.end_byte(), |b| b.start_byte());
        let signature = collapse(self.src[node.start_byte()..end].trim_end_matches(['=', '>', ' ', '{', ';', '\n']), SIGNATURE_CAP);
        let attrs = attributes(self.src, node);
        let mut tags = Vec::new();
        if attrs.iter().any(|a| TEST_ATTRS.contains(&a.as_str())) {
            tags.push("test");
        }
        if kind == SymbolKind::Method && name == "Main" && has_modifier(self.src, node, "static") {
            tags.push("entrypoint");
        }
        self.facts.symbols.push(Symbol {
            parent: self.current(),
            name,
            kind,
            container,
            signature,
            doc: doc_comment(self.src, node),
            attrs,
            tags,
            start_line: line(node),
            end_line: node.end_position().row as u32 + 1,
            start_byte: node.start_byte() as u32,
            end_byte: node.end_byte() as u32,
            complexity: if matches!(kind, SymbolKind::Function | SymbolKind::Method | SymbolKind::Init) { 1 } else { 0 },
            nesting: 0,
            bumps: Vec::new(),
            cond_ops: 0,
            cond_line: 0,
            type_name: None,
            arity: None,
            lambda_types: None,
        });
        self.facts.symbols.len() - 1
    }

    fn type_decl(&mut self, node: Node) {
        let Some(n) = node.child_by_field_name("name") else { return self.visit_children(node) };
        let name = self.text(n).to_string();
        let kind = match node.kind() {
            "interface_declaration" => SymbolKind::Protocol,
            "enum_declaration" => SymbolKind::Enum,
            "struct_declaration" => SymbolKind::Struct,
            "record_declaration" if children(node).any(|c| c.kind() == "struct") => SymbolKind::Struct,
            _ => SymbolKind::Class,
        };
        let outer = self.enclosing_type();
        let qualified = outer.as_ref().map_or_else(|| name.clone(), |o| format!("{o}.{name}"));
        let header_stop = children(node).find(|c| c.kind() == "type_parameter_constraints_clause");
        let sym = self.push(node, name, kind, outer, header_stop);
        let mut aliases: Vec<(String, String)> = self.aliases.iter().map(|(a, t)| (a.clone(), t.clone())).collect();
        aliases.sort();
        let mut decl = TypeDecl { ns: self.namespaces.last().cloned(), params: type_params_of(self.src, node), bases: Vec::new(), aliases };
        if let Some(bases) = children(node).find(|c| c.kind() == "base_list") {
            for b in children(bases).filter(|c| c.is_named()) {
                let t = if b.kind() == "primary_constructor_base_type" { b.child_by_field_name("type").unwrap_or(b) } else { b };
                let target = base_type_name(self.src, t);
                if !target.is_empty() {
                    let g = if t.kind() == "qualified_name" { t.child_by_field_name("name").unwrap_or(t) } else { t };
                    if g.kind() == "generic_name" {
                        let args = children(g)
                            .filter(|c| c.kind() == "type_argument_list")
                            .flat_map(children)
                            .filter(|c| c.is_named())
                            .map(|a| type_expr(self.src, a).map(|e| e.encode()))
                            .collect();
                        decl.bases.push((target.clone(), args));
                    }
                    self.facts.relations.push(Relation { from: sym, target, kind: RelationKind::Inherits, line: line(b) });
                }
            }
        }
        self.facts.symbols[sym].type_name = decl.encode();
        if self.facts.symbols[sym].attrs.iter().any(|a| matches!(a.as_str(), "TestFixture" | "TestClass")) {
            self.facts.symbols[sym].tags.push("test");
        }
        // `[Table("orders", Schema = "sales")]`: the only positional argument is the name.
        if let Some((_, args)) = attribute_args(self.src, node, &["Table"]).into_iter().next()
            && let Some(table) = args.first().and_then(|a| string_value(self.src, *a))
        {
            let schema = args.get(1).and_then(|a| string_value(self.src, *a));
            let entity = self.facts.symbols[sym].name.clone();
            self.db(DbFact { kind: "table", sym: Some(sym), entity, name: qualify(schema, table), line: line(node) });
        }
        let configured = children(node)
            .filter(|c| c.kind() == "base_list")
            .flat_map(children)
            .filter(|b| {
                b.kind() == "generic_name"
                    && matches!(name_text(self.src, *b).as_str(), "EntityTypeConfiguration" | "IEntityTypeConfiguration")
            })
            .find_map(|b| first_type_arg(self.src, b));
        // A record's positional parameters are its properties.
        let mut locals = HashMap::new();
        if let Some(params) = children(node).find(|c| c.kind() == "parameter_list") {
            // A primary constructor: the type's own arity and parameter types, for telling its creations from those
            // of the constructors it declares.
            let (arity, _) = parameters(self.src, node, params);
            self.facts.symbols[sym].arity = Some(arity);
            if let Some(types) = parameter_types(self.src, node, params) {
                self.facts.param_types.insert(sym, types);
            }
            for p in children(params).filter(|c| c.kind() == "parameter") {
                if let Some(pn) = p.child_by_field_name("name") {
                    let pname = self.text(pn).to_string();
                    let ty = p.child_by_field_name("type").and_then(|t| type_expr(self.src, t));
                    let ps = self.push(p, pname.clone(), SymbolKind::Property, Some(qualified.clone()), None);
                    self.facts.symbols[ps].parent = Some(sym);
                    if node.kind() == "class_declaration" {
                        self.facts.symbols[ps].tags.push("injected");
                    }
                    self.facts.symbols[ps].type_name = ty.clone().map(|t| t.encode());
                    if let Some(t) = ty {
                        locals.insert(pname, t);
                    }
                }
            }
        }
        let class_name = self.facts.symbols[sym].name.clone();
        // `[Route("api/[controller]")]` (`[RoutePrefix]` in Web API 2): the prefix of the class's actions, and of its
        // subclasses' when they have none of their own, recorded as a `prefix` row for those (see `action_route`).
        let area = attribute_args(self.src, node, &["Area"]).into_iter().find_map(|(_, a)| a.first().and_then(|a| self.const_value(*a)));
        let template = attribute_args(self.src, node, &["Route", "RoutePrefix"])
            .into_iter()
            .find_map(|(_, args)| args.first().and_then(|a| self.route_template(*a)))
            .map(|(t, r)| (area.as_ref().map_or_else(|| t.clone(), |a| t.replace("[area]", a)), r));
        // `[Route("")]` is a prefix too: an empty one, replacing a base class's `[Route]`.
        let own_route = template.is_some();
        if let Some((t, r)) = template.clone() {
            self.facts.http_routes.push(HttpRoute { sym: Some(sym), side: "prefix", verb: String::new(), path: t, path_ref: r, service: None, service_ref: None, line: line(node) });
        }
        let route = template.filter(|(_, r)| r.is_none()).map(|(t, _)| t);
        // Whether a base class may give the actions a prefix: any base but the framework's own.
        let custom_base = children(node).filter(|c| c.kind() == "base_list").flat_map(children).filter(|b| b.is_named()).any(|b| {
            let t = if b.kind() == "primary_constructor_base_type" { b.child_by_field_name("type").unwrap_or(b) } else { b };
            let n = base_type_name(self.src, t);
            !n.is_empty() && !matches!(n.as_str(), "Controller" | "ControllerBase" | "ApiController") && !is_interface_name(&n)
        });
        let controller = ControllerScope {
            routed: own_route || custom_base || has_modifier(self.src, node, "partial"),
            conventional: class_name.ends_with("Controller") && !own_route && !has_modifier(self.src, node, "abstract"),
            unrouted: !own_route && !has_modifier(self.src, node, "partial"),
            area,
        };
        self.service_dto(sym, node);
        self.attribute_contract(sym, node);
        self.bus_type(sym, node, &qualified);
        // A service host (a configured base, `service_host_bases`): `Startup(…) : ServiceHost<S>(Names.Service, …)`,
        // or `: base(name, …)` in a constructor of a class deriving from it.
        let bases: Vec<Node> = children(node).filter(|c| c.kind() == "base_list").flat_map(children).filter(|b| b.is_named()).collect();
        let hosts = &self.conv.csharp_http.service_host_bases;
        let startup = !hosts.is_empty()
            && bases.iter().any(|b| {
                let t = if b.kind() == "primary_constructor_base_type" { b.child_by_field_name("type").unwrap_or(*b) } else { *b };
                let name = base_type_name(self.src, t);
                hosts.iter().any(|h| name.starts_with(h.as_str()))
            });
        if startup
            // The base's arguments: an `argument_list` beside a generic base type, or inside a plain one.
            && let Some(first) = bases
                .iter()
                .find_map(|b| match b.kind() {
                    "argument_list" => Some(*b),
                    "primary_constructor_base_type" => children(*b).find(|c| c.kind() == "argument_list"),
                    _ => None,
                })
                .and_then(first_argument)
        {
            self.register_service(sym, first);
        }
        self.routes.push((class_name, route, startup.then_some(sym)));
        self.controllers.push(controller);
        self.configured.push(configured);
        let validated = children(node)
            .filter(|c| c.kind() == "base_list")
            .flat_map(children)
            .filter(|b| {
                b.kind() == "generic_name"
                    && matches!(name_text(self.src, *b).as_str(), "AbstractValidator" | "InlineValidator")
            })
            .find_map(|b| {
                let arg = children(b).filter(|c| c.kind() == "type_argument_list").flat_map(children).find(|c| c.is_named())?;
                type_expr(self.src, arg)
            });
        self.validated.push(validated);
        self.in_scope(sym, Some(qualified), false, locals, |cx| {
            for c in children(node) {
                if !matches!(c.kind(), "identifier" | "attribute_list" | "base_list" | "parameter_list") {
                    cx.visit(c);
                } else if c.kind() == "base_list" {
                    // Base types are type references from this type, so `refs` finds subclasses and implementers.
                    for b in children(c).filter(|b| b.is_named()) {
                        match b.kind() {
                            // `class Audit(int id) : Entry(id)`: the primary constructor calls the base's.
                            "primary_constructor_base_type" => {
                                if let Some(t) = b.child_by_field_name("type") {
                                    cx.type_refs(t, false);
                                }
                                if let Some(args) = children(b).find(|c| c.kind() == "argument_list") {
                                    cx.chained("base", Some(args), b);
                                }
                                cx.visit_children(b);
                            }
                            // `: Entry<T>(id)` parses as the base type with its arguments beside it.
                            "argument_list" => {
                                cx.chained("base", Some(b), b);
                                cx.visit(b);
                            }
                            _ => cx.type_refs(b, false),
                        }
                    }
                }
            }
        });
        self.configured.pop();
        self.validated.pop();
        self.routes.pop();
        self.controllers.pop();
    }

    fn function(&mut self, node: Node) {
        let in_type = self.scopes.last().is_some_and(|s| s.type_name.is_some());
        let (name, kind) = match node.kind() {
            "constructor_declaration" => ("init".to_string(), SymbolKind::Init),
            "operator_declaration" => {
                let op = node.child_by_field_name("operator").map_or("", |o| self.text(o));
                (format!("operator {op}"), SymbolKind::Method)
            }
            _ => {
                let Some(n) = node.child_by_field_name("name") else { return };
                let kind = if in_type && node.kind() == "method_declaration" { SymbolKind::Method } else { SymbolKind::Function };
                (self.text(n).to_string(), kind)
            }
        };
        // `static void M(this T x)`: an extension method is a member of `T`, as Swift extension members are.
        let extended = node
            .child_by_field_name("parameters")
            .and_then(|ps| children(ps).find(|c| c.kind() == "parameter"))
            .filter(|p| has_modifier(self.src, *p, "this"))
            .and_then(|p| p.child_by_field_name("type"))
            // `this string s`: a member of `string`; `this T x` with `T` the method's own: a member of every type.
            .map(|t| if t.kind() == "predefined_type" { self.text(t).to_string() } else { base_type_name(self.src, t) })
            .map(|t| if type_params_of(self.src, node).contains(&t) { "object".to_string() } else { t })
            .filter(|t| !t.is_empty());
        let container = if in_type { extended.or_else(|| self.enclosing_type()) } else { None };
        let sym = self.push(node, name, kind, container, None);
        if kind == SymbolKind::Method {
            self.action_route(sym, node);
        }
        if kind == SymbolKind::Init
            && let Some(owner) = self.enclosing_type()
        {
            let params: Vec<&str> = node
                .child_by_field_name("parameters")
                .map(|ps| children(ps).filter(|c| c.kind() == "parameter").filter_map(|c| c.child_by_field_name("name")).map(|n| self.text(n)).collect())
                .unwrap_or_default();
            for member in assigned_params(self.src, node, &params) {
                self.injected.push((owner.clone(), member));
            }
        }
        // `: base(Names.Service, …)` in a service host's constructor.
        if kind == SymbolKind::Init
            && let Some((_, _, Some(host))) = self.routes.last().cloned()
            && let Some(first) = children(node)
                .find(|c| c.kind() == "constructor_initializer" && self.text(*c).trim_start_matches(':').trim_start().starts_with("base"))
                .and_then(|i| children(i).find(|c| c.kind() == "argument_list"))
                .and_then(first_argument)
        {
            self.register_service(host, first);
        }
        if let Some(params) = node.child_by_field_name("parameters") {
            let (arity, lambda_types) = parameters(self.src, node, params);
            self.facts.symbols[sym].arity = Some(arity);
            self.facts.symbols[sym].lambda_types = lambda_types;
            if let Some(types) = parameter_types(self.src, node, params) {
                self.facts.param_types.insert(sym, types);
            }
        }
        let ret = node.child_by_field_name("returns").or_else(|| node.child_by_field_name("type"));
        self.facts.symbols[sym].type_name = ret.and_then(|t| type_expr(self.src, t)).map(|t| t.encode());
        if let Some(t) = ret {
            self.type_refs(t, false);
        }
        let mut locals = HashMap::new();
        let mut builders = Vec::new();
        if let Some(params) = node.child_by_field_name("parameters") {
            for p in children(params).filter(|c| c.kind() == "parameter") {
                if let (Some(pn), Some(t)) = (p.child_by_field_name("name"), p.child_by_field_name("type")) {
                    // `Configure(EntityTypeBuilder<Order> builder)`.
                    if t.kind() == "generic_name"
                        && name_text(self.src, t) == "EntityTypeBuilder"
                        && let Some(entity) = first_type_arg(self.src, t)
                    {
                        builders.push((self.text(pn).to_string(), entity));
                    }
                    if let Some(te) = type_expr(self.src, t) {
                        locals.insert(self.text(pn).to_string(), te);
                    }
                }
            }
        }
        self.in_scope(sym, None, true, locals, |cx| {
            cx.builders.extend(builders);
            for c in children(node) {
                if !matches!(c.kind(), "identifier" | "attribute_list" | "modifier") && Some(c) != ret {
                    cx.visit(c);
                }
            }
        });
    }

    fn property(&mut self, node: Node) {
        let name = match node.child_by_field_name("name") {
            Some(n) => self.text(n).to_string(),
            None if node.kind() == "indexer_declaration" => "this[]".to_string(),
            None => return,
        };
        let kind = if node.kind() == "indexer_declaration" { SymbolKind::Subscript } else { SymbolKind::Property };
        let stop = node.child_by_field_name("accessors").or_else(|| node.child_by_field_name("value"));
        let sym = self.push(node, name, kind, self.enclosing_type(), stop);
        let t = node.child_by_field_name("type");
        self.facts.symbols[sym].type_name = t.and_then(|t| type_expr(self.src, t)).map(|t| t.encode());
        if let Some(t) = t {
            self.type_refs(t, false);
            if t.kind() == "generic_name"
                && matches!(name_text(self.src, t).as_str(), "DbSet" | "IDbSet")
                && let Some(entity) = first_type_arg(self.src, t)
            {
                let name = self.facts.symbols[sym].name.clone();
                self.db(DbFact { kind: "dbset", sym: Some(sym), entity, name, line: line(node) });
            }
        }
        // A setter's `value` is of the property's type.
        let locals =
            t.and_then(|t| type_expr(self.src, t)).map(|t| HashMap::from([("value".to_string(), t)])).unwrap_or_default();
        self.in_scope(sym, None, true, locals, |cx| {
            for f in ["accessors", "value"] {
                if let Some(c) = node.child_by_field_name(f) {
                    cx.visit(c);
                }
            }
        });
    }

    fn field(&mut self, node: Node) {
        let Some(decl) = children(node).find(|c| c.kind() == "variable_declaration") else { return };
        let t = decl.child_by_field_name("type");
        let declared = t.and_then(|t| type_expr(self.src, t));
        if let Some(t) = t {
            self.type_refs(t, false);
        }
        for d in children(decl).filter(|c| c.kind() == "variable_declarator") {
            let Some(n) = d.child_by_field_name("name") else { continue };
            let name = self.text(n).to_string();
            let sym = self.push(node, name.clone(), SymbolKind::Property, self.enclosing_type(), Some(decl));
            let value = children(d).filter(|c| c.is_named() && *c != n).last();
            // `const string X = "…"` (or `nameof(X)`), `static readonly string X = "…"`: constants routes can name. A
            // `readonly string baseUrl = "api/x/"` holds one too, named from its own type's members.
            let readonly = has_modifier(self.src, node, "readonly") && t.is_some_and(|t| matches!(self.text(t), "string" | "String"));
            if (has_modifier(self.src, node, "const") || has_modifier(self.src, node, "static") || readonly)
                && let Some(v) = value.and_then(|v| self.const_value(v))
            {
                let qualified = self.enclosing_type().map_or_else(|| name.clone(), |t| format!("{t}.{name}"));
                self.facts.constants.push(StringConst { qualified, value: v, line: line(d) });
                if readonly && let Some(owner) = self.enclosing_type() {
                    self.string_fields.push((owner, name.clone()));
                }
            }
            let ty = declared.clone().or_else(|| value.and_then(|v| self.expr_type(v)));
            self.facts.symbols[sym].type_name = ty.map(|t| t.encode());
            if let (Some(v), Some(owner)) = (value, self.enclosing_type())
                && let Some(param) = given(self.src, v)
                && self.scopes.last().is_some_and(|s| s.type_name.is_some() && s.locals.contains_key(param))
            {
                self.injected.push((owner, name.clone()));
            }
            self.in_scope(sym, None, true, HashMap::new(), |cx| {
                if let Some(v) = value {
                    cx.visit(v);
                }
            });
        }
    }

    /// Types the parameters of lambdas passed to a call. `xs.Where(x => …)` binds `x` to an element of `xs`;
    /// `mock.Setup(m => …)` binds `m` to the mocked type; `(Order o) => …` binds `o` to `Order`; framework
    /// conventions (an entity builder's `Property(e => …)`, AutoMapper's `ForMember`, FluentValidation's `RuleFor`)
    /// bind theirs. Otherwise a lambda's parameter takes its type from the delegate the called method's parameter
    /// declares, found at query time.
    ///
    /// Returns the source type an AutoMapper `ForMember` maps from, for the `MapFrom(s => …)` inside its arguments.
    fn lambda_params(&mut self, call: Node, args: Node) -> Option<String> {
        let list: Vec<Node> = children(args).filter(|c| c.kind() == "argument").collect();
        if !list.iter().any(|a| children(*a).any(|c| c.kind() == "lambda_expression")) {
            return None;
        }
        let f = call.child_by_field_name("function")?;
        let (method, recv) = match f.kind() {
            "member_access_expression" => {
                (name_text(self.src, f.child_by_field_name("name")?), self.call_receiver_type(f).map(Box::new))
            }
            "identifier" | "generic_name" => (name_text(self.src, f), None),
            _ => return None,
        };
        let member = f.kind() == "member_access_expression";
        let (fixed, first_only, source) = self.framework_lambda(f, &method)?;
        for (i, a) in list.iter().enumerate() {
            // A named argument's position says nothing about the parameter it fills.
            if a.child_by_field_name("name").is_some() && fixed.is_none() {
                continue;
            }
            let Some(lambda) = children(*a).find(|c| c.kind() == "lambda_expression") else { continue };
            let Some(ps) = lambda.child_by_field_name("parameters") else { continue };
            let params: Vec<(Node, Option<Node>)> = match ps.kind() {
                "implicit_parameter" => vec![(ps, None)],
                _ => children(ps)
                    .filter(|c| c.kind() == "parameter")
                    .filter_map(|c| Some((c.child_by_field_name("name")?, c.child_by_field_name("type"))))
                    .collect(),
            };
            let fixed = fixed.as_ref().filter(|_| !first_only || i == 0);
            for (j, (name, declared)) in params.into_iter().enumerate() {
                let ty = match (declared.and_then(|t| type_expr(self.src, t)), fixed) {
                    (Some(t), _) => t,
                    (None, Some(t)) if j == 0 => t.clone(),
                    (None, Some(_)) => continue,
                    (None, None) if first_only => continue,
                    // A method on a receiver of unknown type.
                    (None, None) if member && recv.is_none() => continue,
                    (None, None) => TypeExpr::LambdaParam {
                        recv: recv.clone(),
                        name: method.clone(),
                        args: list.len() as u32,
                        arg: i as u32,
                        param: j as u32,
                    },
                };
                self.bind(self.text(name).to_string(), ty);
            }
        }
        source
    }

    /// The type a framework convention gives the first parameter of a call's lambdas, whether only the first
    /// argument's, and the AutoMapper source type its arguments map from. `None` when the call's lambdas can't be
    /// typed at all (a LINQ or mock method on a receiver of unknown type).
    #[allow(clippy::type_complexity)]
    fn framework_lambda(&self, f: Node, method: &str) -> Option<(Option<TypeExpr>, bool, Option<String>)> {
        let member = f.kind() == "member_access_expression";
        let receiver = f.child_by_field_name("expression");
        let type_arg = |n: Node| {
            let mut args = children(n).filter(|c| c.kind() == "type_argument_list").flat_map(children);
            args.find(|c| c.is_named()).and_then(|t| type_expr(self.src, t))
        };
        // `It.Is<Order>(o => …)`: an argument matcher's lambda takes its type argument.
        if let (Some(e), Some(n)) = (receiver, f.child_by_field_name("name"))
            && n.kind() == "generic_name"
            && MATCHER_LAMBDAS.contains(&(self.text(e), method))
        {
            return Some((type_arg(n), false, None));
        }
        if member && ELEMENT_LAMBDAS.contains(&method) {
            return Some((Some(TypeExpr::Elem(Box::new(self.expr_type(receiver?)?))), false, None));
        }
        if member && MOCK_LAMBDAS.contains(&method) {
            return Some((Some(self.expr_type(receiver?)?), false, None));
        }
        // `builder.Property(e => e.Name)` on an `EntityTypeBuilder<T>`, or bare in an EF6 `EntityTypeConfiguration<T>`.
        if ENTITY_LAMBDAS.contains(&method) {
            let entity = match receiver {
                Some(r) if r.kind() == "identifier" || self.entity_call(r).is_some() => self.builder_entity(r),
                Some(_) => None,
                None => self.configured.last().cloned().flatten(),
            };
            if let Some(e) = entity {
                return Some((Some(TypeExpr::Named(e)), false, None));
            }
        }
        // `CreateMap<S, D>().ForMember(d => d.X, o => o.MapFrom(s => s.Y))`.
        if member
            && matches!(method, "ForMember" | "ForPath" | "ForSourceMember")
            && let Some((src, dst)) = receiver.and_then(|r| self.mapping(r))
        {
            let first = if method == "ForSourceMember" { &src } else { &dst };
            return Some((Some(TypeExpr::Named(first.clone())), true, Some(src)));
        }
        if member && matches!(method, "MapFrom" | "Condition" | "PreCondition")
            && let Some(src) = self.mapping_sources.last()
        {
            return Some((Some(TypeExpr::Named(src.clone())), false, None));
        }
        // `RuleFor(x => x.Name)` in an `AbstractValidator<T>`; `c.RuleFor(i => i.Amount)` inside `ChildRules(c => …)`
        // validates the child.
        if VALIDATOR_LAMBDAS.contains(&method) {
            if let Some(t) = receiver.and_then(|r| self.child_validator(r)) {
                return Some((Some(t), false, None));
            }
            if let Some(Some(t)) = self.validated.last() {
                return Some((Some(t.clone()), false, None));
            }
        }
        Some((None, false, None))
    }

    /// The type a `ChildRules(c => …)` parameter validates, for `c`.
    fn child_validator(&self, e: Node) -> Option<TypeExpr> {
        if e.kind() != "identifier" {
            return None;
        }
        let name = self.text(e);
        self.child_rules.iter().rev().find(|(p, _)| p == name).map(|(_, t)| t.clone())
    }

    /// What the rules of a FluentValidation chain apply to: `RuleFor(x => x.Child)…` the child's type,
    /// `RuleForEach(x => x.Items)…` an element of the items.
    fn rule_target(&self, e: Node) -> Option<TypeExpr> {
        let mut cur = e;
        while cur.kind() == "invocation_expression" {
            let f = cur.child_by_field_name("function")?;
            let (name, receiver) = match f.kind() {
                "member_access_expression" => (name_text(self.src, f.child_by_field_name("name")?), f.child_by_field_name("expression")),
                _ => (name_text(self.src, f), None),
            };
            if matches!(name.as_str(), "RuleFor" | "RuleForEach") {
                let root = match receiver {
                    Some(r) if r.kind() != "this_expression" => self.child_validator(r)?,
                    _ => self.validated.last().cloned().flatten()?,
                };
                let lambda = cur
                    .child_by_field_name("arguments")
                    .and_then(first_argument)
                    .filter(|l| l.kind() == "lambda_expression")?;
                let param = lambda_param(lambda)?;
                let mut body = lambda.child_by_field_name("body")?;
                let mut segs = Vec::new();
                while body.kind() == "member_access_expression" {
                    segs.push(Seg::Member(name_text(self.src, body.child_by_field_name("name")?)));
                    body = body.child_by_field_name("expression")?;
                }
                if body.kind() != "identifier" || self.text(body) != self.text(param) {
                    return None;
                }
                segs.reverse();
                let target = if segs.is_empty() { root } else { TypeExpr::Path { root: PathRoot::Expr(Box::new(root)), segs } };
                return Some(if name == "RuleForEach" { TypeExpr::Elem(Box::new(target)) } else { target });
            }
            cur = f.child_by_field_name("expression")?;
        }
        None
    }

    /// The source and destination of the AutoMapper map a fluent chain configures: `CreateMap<S, D>()…`, swapped
    /// after `ReverseMap()`. Written qualified, they keep their qualifiers (`Contracts.Order`).
    fn mapping(&self, e: Node) -> Option<(String, String)> {
        let mut cur = e;
        let mut reversed = false;
        while cur.kind() == "invocation_expression" {
            let f = cur.child_by_field_name("function")?;
            let n = if f.kind() == "member_access_expression" { f.child_by_field_name("name")? } else { f };
            match name_text(self.src, n).as_str() {
                "ReverseMap" => reversed = !reversed,
                "CreateMap" if n.kind() == "generic_name" => {
                    let args: Vec<String> = children(n)
                        .filter(|c| c.kind() == "type_argument_list")
                        .flat_map(children)
                        .filter(|c| c.is_named())
                        .map(|t| dotted_type(self.src, t))
                        .collect();
                    let [s, d] = args.as_slice() else { return None };
                    if s.is_empty() || d.is_empty() {
                        return None;
                    }
                    return Some(if reversed { (d.clone(), s.clone()) } else { (s.clone(), d.clone()) });
                }
                _ => {}
            }
            cur = f.child_by_field_name("expression")?;
        }
        None
    }

    /// `[HttpPost(nameof(Filters))]`, `[HttpGet("{id}")]`, `[HttpPut] [Route("x")]`: the method serves a route, under
    /// its controller's `[Route]` prefix unless the template starts with `/`. `[controller]` and `[action]` are
    /// replaced.
    ///
    /// A controller with no `[Route]` written out (one deriving from a base controller, naming its prefix by a
    /// constant, or partial) leaves the prefix to the query, which takes the nearest `[Route]` up the class's bases,
    /// as ASP.NET does, with `[controller]` the class's own name: the path starts `{^Action}`. With none there, an
    /// action without a template is conventionally routed, and so is a public method with no route attribute in a
    /// controller with no `[Route]` (`{^*Action}`, any verb), when the repo maps conventional routes. `{^!…}`: the
    /// class has no `[Route]` of its own (see [`ControllerScope::unrouted`]), so the query looks from its bases.
    fn action_route(&mut self, sym: usize, node: Node) {
        const VERBS: &[&str] = &["HttpGet", "HttpPost", "HttpPut", "HttpDelete", "HttpPatch", "HttpHead", "HttpOptions"];
        let attrs = attribute_args(self.src, node, &[VERBS, &["Route", "NonAction", "ActionName"]].concat());
        if attrs.iter().any(|(n, _)| n == "NonAction") {
            return;
        }
        let scope = self.controllers.last().cloned().unwrap_or_default();
        let action = attrs
            .iter()
            .find(|(n, _)| n == "ActionName")
            .and_then(|(_, a)| a.first().and_then(|a| self.const_value(*a)))
            .unwrap_or_else(|| self.facts.symbols[sym].name.clone());
        let route_args = attrs.iter().find(|(n, _)| n == "Route").map(|(_, a)| a.first().copied());
        let (verb, template_arg) = match (attrs.iter().find(|(n, _)| VERBS.contains(&n.as_str())), route_args) {
            (Some((v, args)), r) => (v.trim_start_matches("Http").to_ascii_uppercase(), args.first().copied().or(r.flatten())),
            // `[Route("x")]` alone: every verb.
            (None, Some(r)) => ("*".to_string(), r),
            (None, None) => {
                // A public method of a conventionally routed controller is an action.
                if scope.conventional && has_modifier(self.src, node, "public") && !has_modifier(self.src, node, "static") && !has_modifier(self.src, node, "override") {
                    let path = format!("{{^{}*{action}}}", if scope.unrouted { "!" } else { "" });
                    self.facts.http_routes.push(HttpRoute { sym: Some(sym), side: "server", verb: "*".to_string(), path, path_ref: None, service: None, service_ref: None, line: line(node) });
                }
                return;
            }
        };
        let (template, path_ref) = match template_arg {
            Some(a) => match self.route_template(a) {
                Some(t) => t,
                None => return, // a template that can't be known: no route rather than a wrong one
            },
            None => Default::default(),
        };
        let (class, prefix, _) = self.routes.last().cloned().unwrap_or_default();
        let controller = class.strip_suffix("Controller").unwrap_or(&class).to_string();
        let template = scope.area.as_ref().map_or(template.clone(), |a| template.replace("[area]", a));
        let absolute = template.starts_with('/') || template.starts_with("~/");
        let joined = if absolute {
            template.trim_start_matches('~').to_string()
        } else {
            [prefix.clone().unwrap_or_default(), template.clone()].iter().map(|s| s.trim_matches('/')).filter(|s| !s.is_empty()).collect::<Vec<_>>().join("/")
        };
        let mut path = joined.replace("[controller]", &controller).replace("[action]", &action).trim_matches('/').to_string();
        // No prefix written on the class: the query places it (see above), unless a constant names the template.
        if !absolute && prefix.is_none() && path_ref.is_none() && (scope.routed || scope.conventional && template.is_empty()) {
            path = format!("{{^{}{action}}}{}{path}", if scope.unrouted { "!" } else { "" }, if path.is_empty() { "" } else { "/" });
        }
        self.facts.http_routes.push(HttpRoute { sym: Some(sym), side: "server", verb, path, path_ref, service: None, service_ref: None, line: line(node) });
    }

    /// `// [HttpPost(nameof(Filters))]` in a controller, above or among a method's attributes or where a whole action
    /// was commented out: a `commented` route of the method or the controller, under the controller's prefix.
    /// Templates in constants aren't resolved.
    fn commented_routes(&mut self, node: Node) {
        let Some((class, prefix, _)) = self.routes.last().cloned() else { return };
        let Some(sym) = self.current() else { return };
        let controller = class.strip_suffix("Controller").unwrap_or(&class).to_string();
        let action = (self.facts.symbols[sym].kind == SymbolKind::Method).then(|| self.facts.symbols[sym].name.clone());
        for l in self.text(node).lines() {
            let Some((verb, template)) = commented_http(l) else { continue };
            let joined = if template.starts_with('/') || template.starts_with("~/") {
                template.trim_start_matches('~').to_string()
            } else {
                [prefix.clone().unwrap_or_default(), template].iter().map(|s| s.trim_matches('/')).filter(|s| !s.is_empty()).collect::<Vec<_>>().join("/")
            };
            let mut path = joined.replace("[controller]", &controller);
            if let Some(a) = &action {
                path = path.replace("[action]", a);
            }
            let path = path.trim_matches('/').to_string();
            self.facts.http_routes.push(HttpRoute { sym: Some(sym), side: "commented", verb, path, path_ref: None, service: None, service_ref: None, line: line(node) });
        }
    }

    /// A configured request attribute (`request_attributes`), `[SendsRequest(service, path, HttpMethods.Verb)]`: the
    /// class is a request sent through the service gateway.
    fn service_dto(&mut self, sym: usize, node: Node) {
        let conv = self.conv.clone();
        for call in &conv.csharp_http.request_attributes {
            for (_, args) in attribute_args(self.src, node, &[call.name.as_str()]) {
                self.service_dto_args(sym, node, call, &args);
            }
        }
    }

    fn service_dto_args(&mut self, sym: usize, node: Node, call: &crate::conventions::Call, args: &[Node]) {
        use crate::conventions::Arg;
        let at = |role| call.position(role).and_then(|i| args.get(i)).copied();
        let (Some(service), Some(path), Some(verb)) = (at(Arg::Service), at(Arg::Path), at(Arg::Verb)) else { return };
        let (Some((service, service_ref)), Some((path, path_ref))) = (self.route_template(service), self.route_template(path)) else {
            return;
        };
        let verb = match verb.kind() {
            "member_access_expression" => verb.child_by_field_name("name").map(|n| self.text(n).to_string()),
            _ => string_value(self.src, verb),
        };
        let Some(verb) = verb.map(|v| v.to_ascii_uppercase()) else { return };
        self.facts.http_routes.push(HttpRoute {
            sym: Some(sym),
            side: "client",
            verb,
            path: path.trim_matches('/').to_string(),
            path_ref,
            service: (!service.is_empty()).then_some(service),
            service_ref,
            line: line(node),
        });
    }

    /// Records that the repo hosts the service named by `arg` (a literal, `nameof`, or a constant).
    fn register_service(&mut self, sym: usize, arg: Node) {
        let Some((name, name_ref)) = self.route_template(arg) else { return };
        if name.is_empty() && name_ref.is_none() {
            return;
        }
        self.facts.http_routes.push(HttpRoute {
            sym: Some(sym),
            side: "service",
            verb: String::new(),
            path: String::new(),
            path_ref: None,
            service: (!name.is_empty()).then_some(name),
            service_ref: name_ref,
            line: line(arg),
        });
    }

    /// Message-bus facts about a type declaration: a message (a configured `message_attributes` attribute,
    /// `[BusMessage(Hosts.X)]` or with a queue name literal), and the messages a consumer class handles (a
    /// `consumer_bases` base: `: IConsumer<X>`, a batch `IConsumer<Batch<X>>`, `: IHandleMessages<X>`).
    fn bus_type(&mut self, sym: usize, node: Node, qualified: &str) {
        let attrs: Vec<&str> = self.conv.bus.message_attributes.iter().map(String::as_str).collect();
        if let Some((_, args)) = attribute_args(self.src, node, &attrs).into_iter().next() {
            let queue = args.first().and_then(|a| match a.kind() {
                "member_access_expression" => a.child_by_field_name("name").map(|n| self.text(n).to_string()),
                _ => string_value(self.src, *a),
            });
            self.facts.bus.push(BusFact {
                kind: "message",
                sym,
                message: Some(qualified.to_string()),
                scopes: self.namespaces.last().cloned().into_iter().collect(),
                queue,
                via: None,
                line: line(node),
            });
        }
        let own_params = type_params_of(self.src, node);
        let bases: Vec<Node> = children(node).filter(|c| c.kind() == "base_list").flat_map(children).filter(|b| b.is_named()).collect();
        let saga = bases.iter().any(|b| {
            let t = if b.kind() == "primary_constructor_base_type" { b.child_by_field_name("type").unwrap_or(*b) } else { *b };
            base_type_name(self.src, t) == "MassTransitStateMachine"
        });
        if saga {
            self.saga_events(sym, node);
        }
        for b in bases {
            let t = if b.kind() == "primary_constructor_base_type" { b.child_by_field_name("type").unwrap_or(b) } else { b };
            let g = if t.kind() == "qualified_name" { t.child_by_field_name("name").unwrap_or(t) } else { t };
            if g.kind() != "generic_name" {
                continue;
            }
            let base = name_text(self.src, g);
            if !self.conv.bus.consumer_bases.contains(&base) {
                continue;
            }
            let Some(mut arg) = children(g).filter(|c| c.kind() == "type_argument_list").flat_map(children).find(|c| c.is_named()) else {
                continue;
            };
            // `IConsumer<Batch<X>>` handles `X`s, a batch at a time.
            if arg.kind() == "generic_name" && name_text(self.src, arg) == "Batch"
                && let Some(inner) = children(arg).filter(|c| c.kind() == "type_argument_list").flat_map(children).find(|c| c.is_named())
            {
                arg = inner;
            }
            let message = dotted_type(self.src, arg);
            if message.is_empty() || own_params.contains(&message) {
                continue;
            }
            self.facts.bus.push(BusFact {
                kind: "consume",
                sym,
                message: Some(self.expand_alias(message)),
                scopes: self.bus_scopes(),
                queue: None,
                via: Some(base),
                line: line(b),
            });
        }
    }

    /// A MassTransit state machine consumes the messages of its `Event<X>` properties, and those its
    /// `Schedule<TState, X>` properties schedule to itself.
    fn saga_events(&mut self, sym: usize, node: Node) {
        let Some(body) = node.child_by_field_name("body").or_else(|| children(node).find(|c| c.kind() == "declaration_list")) else { return };
        for p in children(body).filter(|c| c.kind() == "property_declaration") {
            let Some(t) = p.child_by_field_name("type") else { continue };
            let g = if t.kind() == "qualified_name" { t.child_by_field_name("name").unwrap_or(t) } else { t };
            if g.kind() != "generic_name" {
                continue;
            }
            let args: Vec<Node> = children(g).filter(|c| c.kind() == "type_argument_list").flat_map(children).filter(|c| c.is_named()).collect();
            let arg = match (name_text(self.src, g).as_str(), args.as_slice()) {
                ("Event", [m]) | ("Schedule", [_, m]) => *m,
                _ => continue,
            };
            let message = dotted_type(self.src, arg);
            if message.is_empty() {
                continue;
            }
            self.facts.bus.push(BusFact {
                kind: "consume",
                sym,
                message: Some(self.expand_alias(message)),
                scopes: self.bus_scopes(),
                queue: None,
                via: Some("MassTransitStateMachine".to_string()),
                line: line(p),
            });
        }
    }

    /// A registration closing a generic consumer over a message consumes that message where it is registered:
    /// `cfg.AddConsumer<MailConsumer<X>>()`, or a helper registering one, `cfg.AddMailConsumer<X>()`.
    /// `AddConsumer<XConsumer>()` registers a consumer class, which says what it consumes itself.
    fn consumer_registration(&mut self, call: Node) {
        let Some(f) = call.child_by_field_name("function") else { return };
        let g = match f.kind() {
            "member_access_expression" => f.child_by_field_name("name"),
            "generic_name" => Some(f),
            _ => None,
        };
        let Some(g) = g.filter(|g| g.kind() == "generic_name") else { return };
        let name = name_text(self.src, g);
        if !(name.starts_with("Add") && name.ends_with("Consumer")) {
            return;
        }
        let args: Vec<Node> = children(g).filter(|c| c.kind() == "type_argument_list").flat_map(children).filter(|c| c.is_named()).collect();
        let message = match args.as_slice() {
            [c, ..] if unqualified(*c).kind() == "generic_name" => {
                match children(unqualified(*c)).filter(|x| x.kind() == "type_argument_list").flat_map(children).find(|x| x.is_named()) {
                    Some(m) => m,
                    None => return,
                }
            }
            [m] if !self.conv.bus.consumer_registrations.contains(&name) => *m,
            _ => return,
        };
        let message = dotted_type(self.src, message);
        if message.is_empty() || message.ends_with("Consumer") || type_params_around(self.src, call).contains(&message) {
            return;
        }
        let Some(from) = self.current() else { return };
        self.facts.bus.push(BusFact {
            kind: "consume",
            sym: from,
            message: Some(self.expand_alias(message)),
            scopes: self.bus_scopes(),
            queue: None,
            via: Some(name),
            line: line(g),
        });
    }

    /// `bus.Publish(new X())`, `_serviceBus.Publish(evt)`, `endpoint.Send(cmd)`, `context.Publish(…)` in a consumer,
    /// and the configured `publish` calls naming a destination, `bus.PublishToHost(e, Hosts.Jobs)` or
    /// `bus.SendToQueue(cmd, host, "queue")`:
    /// the enclosing method publishes the argument's type, when it's known (`new X`, a typed local or parameter,
    /// `Publish<X>(…)`, `_mapper.Map<X>(…)`, a cast). Receivers are told by name: a bus, publisher, endpoint or
    /// message session, or a consume or handler context.
    fn bus_publish(&mut self, call: Node) {
        let Some((receiver, n)) = call.child_by_field_name("function").and_then(member_call) else { return };
        let method = name_text(self.src, n);
        let conv = self.conv.clone();
        let Some(spec) = conv.bus.publish_call(&method) else { return };
        let last = self.text(receiver).rsplit(['.', '>']).next().unwrap_or_default().to_ascii_lowercase();
        // A bus, an endpoint, or NServiceBus's message session; a handler's or consumer's context sends as well as
        // publishes.
        let bus_like = (last.contains("bus") && !last.contains("business")) || last.ends_with("endpoint") || last.ends_with("session");
        let ok = match method.as_str() {
            "Send" => bus_like || last.ends_with("context"),
            _ => bus_like || last.contains("publish") || last.ends_with("context"),
        };
        let Some(from) = self.current().filter(|_| ok) else { return };
        let args = call.child_by_field_name("arguments").map(argument_exprs).unwrap_or_default();
        let role = |r| spec.position(r).and_then(|i| args.get(i)).copied();
        let Some(first) = role(crate::conventions::Arg::Message) else { return };
        // Moq: `bus.Verify(b => b.Publish(It.IsAny<X>()))` asserts a publish, it doesn't make one.
        if first.kind() == "invocation_expression"
            && first.child_by_field_name("function").is_some_and(|g| self.text(g).starts_with("It."))
        {
            return;
        }
        let explicit = (n.kind() == "generic_name")
            .then(|| children(n).filter(|c| c.kind() == "type_argument_list").flat_map(children).find(|c| c.is_named()))
            .flatten()
            .map(|t| dotted_type(self.src, t));
        let params = type_params_around(self.src, call);
        let message = explicit
            .clone()
            .or_else(|| self.message_type(first))
            .filter(|m| !m.is_empty() && !params.contains(m) && m != "object")
            .map(|m| self.expand_alias(m));
        let member = |e: Node| match e.kind() {
            "member_access_expression" => e.child_by_field_name("name").map(|n| self.text(n).to_string()),
            _ => string_value(self.src, e),
        };
        // The destination the call names: a host, or a queue on a host (`host/queue`).
        let queue = match (spec.position(crate::conventions::Arg::Host), spec.position(crate::conventions::Arg::Queue)) {
            (Some(h), None) => args.get(h).and_then(|a| member(*a)),
            (h, Some(q)) => {
                let host = h.and_then(|h| args.get(h)).and_then(|a| member(*a)).unwrap_or_default();
                let name = args.get(q).and_then(|a| string_value(self.src, *a)).unwrap_or_else(|| "?".to_string());
                Some(format!("{host}/{name}"))
            }
            (None, None) => None,
        };
        // Not known here: perhaps a method of the file builds it, or the caller passes it in.
        if message.is_none() && self.helpers.is_some() {
            let fact = self.facts.bus.len();
            let forwarded = self.forwarded(call, explicit.as_deref(), first);
            let built = self.arg_type(first);
            if let Some(h) = self.helpers.as_mut() {
                match (forwarded, built) {
                    (Some(what), _) => h.forwards.push(Forward { sym: from, what, fact }),
                    (None, ArgType::Built(owner, name)) => h.built.push((fact, owner, name)),
                    _ => {}
                }
            }
        }
        self.facts.bus.push(BusFact {
            kind: "publish",
            sym: from,
            message,
            scopes: self.bus_scopes(),
            queue,
            via: Some(method),
            line: line(n),
        });
    }

    /// How a publish inside a method sends what the method's caller gives it: its type parameter (`Publish<T>(…)`,
    /// a parameter of type `T`, `new T { … }`) or an `object` parameter.
    fn forwarded(&self, call: Node, explicit: Option<&str>, first: Node) -> Option<Forwarded> {
        let decl = enclosing_method(call)?;
        let tparams = type_params_of(self.src, decl);
        let params = param_types(self.src, decl);
        let of_type = |t: &str| params.iter().enumerate().filter(|(_, (_, p))| p == t).map(|(k, _)| k).collect::<Vec<_>>();
        let type_param = |t: &str| tparams.iter().position(|p| p == t).map(|index| Forwarded::TypeParam { index, params: of_type(t) });
        if let Some(t) = explicit {
            return type_param(t);
        }
        match first.kind() {
            "identifier" => {
                let name = self.text(first);
                let (k, (_, t)) = params.iter().enumerate().find(|(_, (n, _))| n == name)?;
                if t == "object" { Some(Forwarded::Param(k)) } else { type_param(t) }
            }
            "object_creation_expression" => type_param(self.text(first.child_by_field_name("type")?)),
            _ => None,
        }
    }

    /// What an argument's type is known to be: its obvious type (see [`Self::message_type`]), or the return type of
    /// a method of the file it calls (`Build(…)`, a local holding `Build(…)`).
    fn arg_type(&self, a: Node) -> ArgType {
        if let Some(t) = self.message_type(a).filter(|t| !t.is_empty()) {
            return ArgType::Known(t);
        }
        match self.expr_type(a) {
            Some(TypeExpr::Call { owner, name }) => ArgType::Built(owner, name),
            Some(TypeExpr::Path { root: PathRoot::SelfType, segs }) => match segs.as_slice() {
                [Seg::Call(name)] => ArgType::Built(None, name.clone()),
                _ => ArgType::Unknown,
            },
            _ => ArgType::Unknown,
        }
    }

    /// Keeps an unqualified call (`Helper(x)`, `this.Helper<X>(…)`) with what its arguments are, in case it calls a
    /// publish helper.
    fn helper_call(&mut self, call: Node) {
        if self.helpers.is_none() {
            return;
        }
        let Some(f) = call.child_by_field_name("function") else { return };
        let g = match (f.kind(), member_call(f)) {
            ("identifier" | "generic_name", _) => f,
            // `this.M(…)`, `this?.M(…)`.
            (_, Some((receiver, n))) if matches!(receiver.kind(), "this_expression" | "this") => n,
            _ => return,
        };
        let Some(from) = self.current() else { return };
        let targs: Vec<String> = if g.kind() == "generic_name" {
            children(g).filter(|c| c.kind() == "type_argument_list").flat_map(children).filter(|c| c.is_named()).map(|t| dotted_type(self.src, t)).collect()
        } else {
            Vec::new()
        };
        let args: Vec<ArgType> = call.child_by_field_name("arguments").map(argument_exprs).unwrap_or_default().into_iter().map(|a| self.arg_type(a)).collect();
        if targs.is_empty() && args.iter().all(|a| matches!(a, ArgType::Unknown)) {
            return;
        }
        let decl = enclosing_method(call);
        let call = HelperCall {
            from,
            name: name_text(self.src, g),
            line: line(g),
            targs,
            args,
            scopes: self.bus_scopes(),
            tparams: decl.map(|d| type_params_of(self.src, d)).unwrap_or_default(),
            params: decl.map(|d| param_types(self.src, d).into_iter().map(|p| p.1).collect()).unwrap_or_default(),
            outer: type_params_around(self.src, call),
        };
        if let Some(h) = self.helpers.as_mut() {
            h.calls.push(call);
        }
    }

    /// Settles what the file's publish helpers publish: a call to a helper publishes what it passes, at the call; a
    /// helper passing its own type parameter on is a helper too. A publish of what a method of the file builds
    /// publishes that method's return type.
    fn resolve_bus_helpers(&mut self) {
        let Some(mut h) = self.helpers.take() else { return };
        for (fact, owner, name) in std::mem::take(&mut h.built) {
            let at = self.facts.bus[fact].sym;
            if let Some(t) = self.built_type(at, owner.as_deref(), &name) {
                self.facts.bus[fact].message = Some(self.expand_alias(t));
            }
        }
        let mut done = std::collections::HashSet::new();
        let mut published = std::collections::HashSet::new();
        let mut resolved = std::collections::HashSet::new();
        loop {
            let mut grown = false;
            for fi in 0..h.forwards.len() {
                for ci in 0..h.calls.len() {
                    let (fw, call) = (&h.forwards[fi], &h.calls[ci]);
                    let root = fw.fact;
                    let helper = &self.facts.symbols[fw.sym];
                    // A helper is a method of the caller's type (or a local function).
                    if fw.sym == call.from
                        || helper.name != call.name
                        || (helper.container.is_some() && helper.container.as_deref() != self.owner_type(call.from))
                        || !done.insert((ci, fi))
                    {
                        continue;
                    }
                    let given = match &fw.what {
                        Forwarded::TypeParam { index, params } => call.targs.get(*index).cloned().map(ArgType::Known).or_else(|| {
                            params.iter().filter_map(|k| call.args.get(*k)).find(|a| !matches!(a, ArgType::Unknown)).cloned()
                        }),
                        Forwarded::Param(k) => call.args.get(*k).cloned(),
                    };
                    let ty = match given {
                        Some(ArgType::Known(t)) => t,
                        Some(ArgType::Built(owner, name)) => match self.built_type(call.from, owner.as_deref(), &name) {
                            Some(t) => t,
                            None => continue,
                        },
                        _ => continue,
                    };
                    if let Some(index) = call.tparams.iter().position(|p| *p == ty) {
                        let params = call.params.iter().enumerate().filter(|(_, p)| **p == ty).map(|(k, _)| k).collect();
                        h.forwards.push(Forward { sym: call.from, what: Forwarded::TypeParam { index, params }, fact: root });
                        grown = true;
                    } else if ty != "object" && !call.outer.contains(&ty) && published.insert((call.from, call.line, ty.clone())) {
                        let message = Some(self.expand_alias(ty));
                        let fact = BusFact {
                            kind: "publish",
                            sym: call.from,
                            message,
                            scopes: call.scopes.clone(),
                            queue: None,
                            via: Some(call.name.clone()),
                            line: call.line,
                        };
                        self.facts.bus.push(fact);
                        resolved.insert(root);
                    }
                }
            }
            if !grown {
                break;
            }
        }
        // A helper's own publish of what it doesn't know, once its callers say: the callers' publishes stand for it.
        if !resolved.is_empty() {
            let mut i = 0;
            self.facts.bus.retain(|_| {
                i += 1;
                !resolved.contains(&(i - 1))
            });
        }
    }

    /// The type whose member `sym` is, or (for a local function) whose member encloses it.
    fn owner_type(&self, mut sym: usize) -> Option<&str> {
        loop {
            let s = &self.facts.symbols[sym];
            match (&s.container, s.parent) {
                (Some(c), _) => return Some(c),
                (None, Some(p)) => sym = p,
                (None, None) => return None,
            }
        }
    }

    /// The type a method of the file named `name` returns, called from `at`: one of `at`'s type (or a local function)
    /// when `owner` is `None`, else one of the type `owner` names. `None` unless every such method agrees.
    fn built_type(&self, at: usize, owner: Option<&str>, name: &str) -> Option<String> {
        let here = self.owner_type(at);
        let mut types = self.facts.symbols.iter().filter(|s| {
            s.name == name
                && matches!(s.kind, SymbolKind::Method | SymbolKind::Function)
                && match owner {
                    None => s.container.is_none() || s.container.as_deref() == here,
                    Some(o) => s.container.as_deref().is_some_and(|c| c == o || c.ends_with(&format!(".{o}"))),
                }
        });
        let first = types.next()?;
        let t = match TypeExpr::decode(first.type_name.as_deref()?)? {
            TypeExpr::Named(n) => n,
            _ => return None,
        };
        types.all(|s| s.type_name == first.type_name).then_some(t).filter(|t| !matches!(t.as_str(), "object" | "string" | "bool" | "int"))
    }

    /// The type a published expression obviously has.
    fn message_type(&self, e: Node) -> Option<String> {
        match e.kind() {
            "object_creation_expression" => e.child_by_field_name("type").map(|t| dotted_type(self.src, t)),
            "cast_expression" => e.child_by_field_name("type").map(|t| dotted_type(self.src, t)),
            "await_expression" | "parenthesized_expression" => children(e).find(|c| c.is_named()).and_then(|c| self.message_type(c)),
            "identifier" => match self.expr_type(e)? {
                TypeExpr::Named(n) => Some(n),
                // `foreach (var e in events)` over a `List<X>`.
                TypeExpr::Elem(inner) => match *inner {
                    TypeExpr::Array(t) => match *t {
                        TypeExpr::Named(n) => Some(n),
                        _ => None,
                    },
                    _ => None,
                },
                _ => None,
            },
            // `_mapper.Map<X>(src)`, `src.Adapt<X>()`: the last type argument.
            "invocation_expression" => {
                let f = e.child_by_field_name("function")?;
                let g = if f.kind() == "member_access_expression" { f.child_by_field_name("name")? } else { f };
                if g.kind() != "generic_name" || !matches!(name_text(self.src, g).as_str(), "Map" | "Adapt") {
                    return None;
                }
                children(g).filter(|c| c.kind() == "type_argument_list").flat_map(children).filter(|c| c.is_named()).last().map(|t| dotted_type(self.src, t))
            }
            _ => None,
        }
    }

    /// SQL text a field or property holds as its value (`const string ByIdSql = "SELECT …"`, a `HELD` fact) is run by
    /// the methods of its type naming it (`conn.Query(ByIdSql)`, a bare name that leaves no reference; not
    /// `other.ByIdSql`, `ByIdSql(…)` or `ByIdSql<T>`): each gets the facts, at its first use. The holder's facts stay,
    /// marked [`HELD_USED`], so users in other files, resolved through their references (`Queries.ByIdSql`), are
    /// credited too, but not the holder. A holder no method of the file names keeps its facts as they are.
    fn sql_holders(&mut self) {
        let held: Vec<usize> = self
            .facts
            .db
            .iter()
            .enumerate()
            .filter(|(_, f)| matches!(f.kind, "sql_read" | "sql_write") && f.entity == HELD)
            .filter(|(_, f)| f.sym.is_some_and(|s| self.facts.symbols[s].kind == SymbolKind::Property))
            .map(|(i, _)| i)
            .collect();
        let mut users_of: HashMap<usize, Vec<(usize, u32)>> = HashMap::new();
        let mut added: Vec<DbFact> = Vec::new();
        for i in held {
            let holder = self.facts.db[i].sym.expect("filtered");
            let users = users_of.entry(holder).or_insert_with(|| {
                let h = &self.facts.symbols[holder];
                self.facts
                    .symbols
                    .iter()
                    .enumerate()
                    .filter(|(j, s)| *j != holder && matches!(s.kind, SymbolKind::Method | SymbolKind::Init | SymbolKind::Function) && s.container == h.container)
                    .filter_map(|(j, s)| {
                        let body = &self.src[s.start_byte as usize..s.end_byte as usize];
                        let at = body.match_indices(h.name.as_str()).find(|(k, _)| {
                            let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
                            let before = body[..*k].chars().last();
                            let after = body[k + h.name.len()..].trim_start().chars().next();
                            !word(before) && !word(body[k + h.name.len()..].chars().next()) && before != Some('.') && !matches!(after, Some('(' | '<'))
                        })?;
                        Some((j, s.start_line + body[..at.0].matches('\n').count() as u32))
                    })
                    .collect()
            });
            if users.is_empty() {
                continue;
            }
            let f = &self.facts.db[i];
            for &(j, line) in users.iter() {
                added.push(DbFact { kind: f.kind, sym: Some(j), entity: String::new(), name: f.name.clone(), line });
            }
            self.facts.db[i].entity = HELD_USED.into();
        }
        for f in added {
            self.db(f);
        }
    }

    /// The bare names read as receivers that name a field or property the enclosing type declares in this file, as
    /// member references on the caller's own type.
    fn bare_member_reads(&mut self) {
        for BareMember { name, from, line: l, col, owner, assigned } in std::mem::take(&mut self.bare_members) {
            let declared = self
                .facts
                .symbols
                .iter()
                .any(|s| s.kind == SymbolKind::Property && s.name == name && s.container.as_deref() == Some(owner.as_str()));
            if !declared {
                continue;
            }
            let i = match self.seen.entry((from, name.clone(), RefKind::Member, l, col)) {
                std::collections::hash_map::Entry::Occupied(e) => *e.get(),
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(self.facts.refs.len());
                    self.facts.refs.push(Ref { from, name, receiver: None, receiver_type: None, kind: RefKind::Member, line: l, col, args: None });
                    self.facts.refs.len() - 1
                }
            };
            if assigned {
                self.facts.assigns.insert(i);
            }
        }
    }

    /// Whether the member holding `at` binds `name` itself (see [`bound_names`]).
    fn bound_in_member(&mut self, at: Node, name: &str) -> bool {
        const MEMBERS: &[&str] = &[
            "method_declaration", "constructor_declaration", "destructor_declaration", "operator_declaration",
            "conversion_operator_declaration", "property_declaration", "indexer_declaration", "field_declaration",
            "event_declaration",
        ];
        let mut member = at;
        while let Some(p) = member.parent() {
            member = p;
            if MEMBERS.contains(&p.kind()) {
                break;
            }
        }
        let src = self.src;
        self.bound.entry(member.id()).or_insert_with(|| bound_names(src, member)).contains(name)
    }

    /// Marks the member reference to `name` recorded at `at` as an assignment (see [`FileFacts::assigns`]).
    fn assigned(&mut self, name: String, at: Node) {
        let col = name_col(self.src, at, &name);
        if let Some(&i) = self.seen.get(&(self.current(), name, RefKind::Member, line(at), col)) {
            self.facts.assigns.insert(i);
        }
    }

    /// Tags the members a constructor's parameter is stored in (see [`Self::injected`]) `injected`.
    fn tag_injected(&mut self) {
        for s in &mut self.facts.symbols {
            if s.kind == SymbolKind::Property
                && !s.tags.contains(&"injected")
                && self.injected.iter().any(|(t, n)| s.container.as_deref() == Some(t.as_str()) && *n == s.name)
            {
                s.tags.push("injected");
            }
        }
    }

    /// Type names written through a `using` alias (`NBW.Contracts.Order` with `using NBW = Shop.Models;`), in type
    /// references' qualifiers, receivers' types and members' types, spelled out: the index has no aliases. An alias
    /// declared in a namespace block applies only to what is written in that block.
    fn expand_aliases(&mut self) {
        if self.aliases.is_empty() && self.block_aliases.is_empty() {
            return;
        }
        let mut refs = std::mem::take(&mut self.facts.refs);
        let mut symbols = std::mem::take(&mut self.facts.symbols);
        // The aliases in scope at a line: the innermost block declaring some, else the file's.
        let at = |l: u32| {
            self.block_aliases
                .iter()
                .filter(|(a, b, _)| (*a..=*b).contains(&l))
                .min_by_key(|(a, b, _)| b - a)
                .map_or(&self.aliases, |(_, _, m)| m)
        };
        let expand = |l: u32, name: &str| expand_with(at(l), name.to_string());
        let retype = |l: u32, enc: &str| match TypeExpr::decode(enc) {
            Some(t) => t.map_names(&|n: &str| expand(l, n)).encode(),
            None => enc.to_string(),
        };
        for r in &mut refs {
            if matches!(r.kind, RefKind::TypeRef | RefKind::Instantiate)
                && let Some(q) = &r.receiver
            {
                r.receiver = Some(expand(r.line, q));
            }
            if let Some(enc) = &r.receiver_type {
                r.receiver_type = Some(retype(r.line, enc));
            }
        }
        for s in &mut symbols {
            if let Some(enc) = &s.type_name
                && TypeDecl::decode(Some(enc)) == TypeDecl::default()
            {
                s.type_name = Some(retype(s.start_line, enc));
            }
        }
        self.facts.refs = refs;
        self.facts.symbols = symbols;
    }

    /// `EP.X` with `using EP = A.B;` → `A.B.X`, for the aliases in scope here.
    fn expand_alias(&self, name: String) -> String {
        expand_with(&self.aliases, name)
    }

    /// Namespaces a type name written here may live in, nearest first: the enclosing namespace and its parents,
    /// then the file's `using`s so far, without framework namespaces.
    fn bus_scopes(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        if let Some(ns) = self.namespaces.last() {
            let mut cur = ns.as_str();
            loop {
                out.push(cur.to_string());
                match cur.rsplit_once('.') {
                    Some((parent, _)) => cur = parent,
                    None => break,
                }
            }
        }
        for i in &self.facts.imports {
            let m = i.module.as_str();
            let framework = FRAMEWORK_NAMESPACES.iter().any(|f| m == *f || m.starts_with(&format!("{f}.")));
            if !framework && !m.starts_with("static ") && !out.iter().any(|o| o == m) {
                out.push(m.to_string());
            }
        }
        out
    }

    /// A route template argument: a string literal, `nameof(A.B.Name)` (the last segment), or a constant to resolve
    /// later (returned as the second value).
    fn route_template(&self, a: Node) -> Option<(String, Option<String>)> {
        if let Some(lit) = string_value(self.src, a) {
            return Some((lit, None));
        }
        let nameof = |e: Node| -> Option<String> {
            (e.kind() == "invocation_expression" && e.child_by_field_name("function").is_some_and(|f| self.text(f) == "nameof"))
                .then(|| {
                    let arg = e.child_by_field_name("arguments").map(|x| self.text(x)).unwrap_or_default();
                    arg.trim_matches(['(', ')', ' ']).rsplit('.').next().unwrap_or_default().to_string()
                })
        };
        Some(match a.kind() {
            // `$"{nameof(Infra)}/Group"`: text and `nameof` interpolations; anything else is unknowable.
            "interpolated_string_expression" => {
                let mut out = String::new();
                for c in children(a) {
                    match c.kind() {
                        "string_content" => out.push_str(self.text(c)),
                        "interpolation" => {
                            out.push_str(&nameof(children(c).find(|e| e.is_named() && !e.kind().starts_with("interpolation_"))?)?)
                        }
                        _ => {}
                    }
                }
                (out, None)
            }
            "invocation_expression" => (nameof(a)?, None),
            "member_access_expression" | "identifier" => (String::new(), Some(self.text(a).to_string())),
            // `"Clients/" + nameof(GetClient)`: literal and `nameof` parts.
            "binary_expression" if a.child_by_field_name("operator").is_some_and(|o| self.text(o) == "+") => {
                let part = |side| match self.route_template(a.child_by_field_name(side)?)? {
                    (t, None) => Some(t),
                    _ => None,
                };
                (part("left")? + &part("right")?, None)
            }
            _ => return None,
        })
    }

    /// Dependency-injection registrations in a fluent chain ending at `call`: Autofac and its wrappers
    /// (`RegisterType<Foo>().As<IFoo>().SingleInstance()`, `.Scoped(Scope.X)`, `Register(_ => new Foo())`,
    /// `RegisterAssemblyTypes(…)`) and Microsoft DI (`AddScoped<IFoo, Foo>()`, `TryAdd*`, `typeof` pairs, factories).
    /// A chain can hold several registrations (`services.AddScoped<A, B>().AddSingleton<C, D>()`).
    fn di_registrations(&mut self, call: Node) {
        let links = chain_links(self.src, call);
        if !links.iter().any(|l| DI_ROOTS.contains(&l.name.as_str())) {
            return;
        }
        let mut cur: Option<PendingDi> = None;
        for link in &links {
            if let Some(root) = self.di_root(link) {
                if let Some(p) = cur.take() {
                    self.flush_di(p);
                }
                cur = Some(root);
                continue;
            }
            // A registration this doesn't read: said, so answers on the services it registers aren't complete.
            if let Some(construct) = self.di_unread(link) {
                if let Some(p) = cur.take() {
                    self.flush_di(p);
                }
                self.facts.unsupported.push(Unsupported { area: "di", construct, line: line(link.at) });
                continue;
            }
            let Some(p) = cur.as_mut() else { continue };
            match link.name.as_str() {
                "As" | "AsKeyed" | "Keyed" | "Named" => {
                    let typed: Vec<Node> = link.args.into_iter().flat_map(argument_exprs).filter_map(typeof_type).collect();
                    for t in link.type_args.iter().copied().chain(typed) {
                        p.services.push(Some(collapse(self.text(t), 120)));
                    }
                }
                "AsSelf" => p.services.push(p.implementation.clone().filter(|_| p.kind != "scan")),
                "AsImplementedInterfaces" => p.services.push(None),
                "Scoped" => {
                    p.lifetime = link
                        .args
                        .and_then(first_argument)
                        .filter(|a| a.kind() == "member_access_expression")
                        .and_then(|a| a.child_by_field_name("name"))
                        .and_then(|n| scope_lifetime(self.text(n), &self.conv.di));
                }
                "SingleInstance" => p.lifetime = Some("singleton"),
                "InstancePerLifetimeScope" | "InstancePerRequest" | "InstancePerMatchingLifetimeScope" | "InstancePerOwned" => {
                    p.lifetime = Some("scoped")
                }
                "InstancePerDependency" => p.lifetime = Some("transient"),
                "Where" if p.kind == "scan" => {
                    // `.Where(t => t.Name.EndsWith("Service"))`: the obvious name filters only.
                    let text = link.args.map(|a| self.text(a)).unwrap_or_default();
                    for (method, pattern) in [("EndsWith(\"", "*{}"), ("StartsWith(\"", "{}*")] {
                        if let Some(rest) = text.split_once(method).map(|(_, r)| r)
                            && let Some((lit, _)) = rest.split_once('"')
                        {
                            p.implementation = Some(pattern.replace("{}", lit));
                        }
                    }
                }
                _ => {}
            }
        }
        if let Some(p) = cur {
            self.flush_di(p);
        }
    }

    /// A component registration built from its parts rather than through a builder: the way a container registers
    /// itself, as the scope it is, under the services it answers for (`new ComponentRegistration(id, new
    /// DelegateActivator(typeof(Scope), …), CurrentScopeLifetime.Instance, InstanceSharing.Shared, …, new Service[] {
    /// new TypedService(typeof(IScope)) }, …)`, or the same arguments to `base(…)` in a class deriving
    /// `ComponentRegistration`). Each typed service is registered as the activator's type, at the line naming it.
    fn component_registration(&mut self, args: Node) {
        let mut services = Vec::new();
        let mut implementation = None;
        let mut stack = vec![args];
        while let Some(n) = stack.pop() {
            if n.kind() == "object_creation_expression"
                && let Some(t) = n.child_by_field_name("type")
            {
                let of = n.child_by_field_name("arguments").and_then(first_argument).and_then(typeof_type);
                match base_type_name(self.src, t).as_str() {
                    "TypedService" => services.extend(of.map(|s| (collapse(self.text(s), 120), line(n)))),
                    // `new DelegateActivator(typeof(Scope), …)`, `new ReflectionActivator(typeof(Foo), …)`.
                    name if name.ends_with("Activator") && implementation.is_none() => implementation = of.map(|t| base_type_name(self.src, t)),
                    _ => {}
                }
            }
            stack.extend(children(n).collect::<Vec<_>>().into_iter().rev());
        }
        // Shared in the scope that resolves it, or in the root scope; or one per resolve.
        let text = self.text(args);
        let lifetime = if text.contains("InstanceSharing.None") {
            Some("transient")
        } else if text.contains("RootScopeLifetime") {
            Some("singleton")
        } else if text.contains("CurrentScopeLifetime") {
            Some("scoped")
        } else {
            None
        };
        for (service, line) in services {
            self.facts.di.push(DiBinding { service: Some(service), implementation: implementation.clone(), lifetime, kind: "type", line, made: None, service_made: false, mode: None });
        }
    }

    /// Whether the class declaring the constructor `init` belongs to derives `ComponentRegistration`.
    fn derives_component_registration(&self, init: Node) -> bool {
        let mut n = init.parent();
        while let Some(c) = n.filter(|c| !matches!(c.kind(), "class_declaration" | "record_declaration")) {
            n = c.parent();
        }
        n.and_then(|c| children(c).find(|b| b.kind() == "base_list"))
            .is_some_and(|b| children(b).filter(|t| t.is_named()).any(|t| base_type_name(self.src, t) == COMPONENT_REGISTRATION))
    }

    /// The registration a chain link starts, if it is one.
    fn di_root(&self, link: &Link) -> Option<PendingDi> {
        let args: Vec<Node> = link.args.into_iter().flat_map(argument_exprs).collect();
        let first_lambda = args.first().filter(|a| matches!(a.kind(), "lambda_expression" | "anonymous_method_expression"));
        let ty = |n: Node| collapse(self.text(n), 120);
        let base = |n: Node| Some(base_type_name(self.src, n)).filter(|b| !b.is_empty());
        let line = line(link.at);
        // `TryAddScoped<IFoo, Foo>()` registers only when the service has no registration yet.
        let mode = link.name.starts_with("TryAdd").then_some("try");
        let pending = |kind, implementation: Option<String>, services: Vec<Option<String>>, lifetime| {
            Some(PendingDi { kind, implementation, services, lifetime, line, default_service: None, made: None, mode })
        };
        // What a factory or instance makes: its type when the code says, else what the index resolves.
        let made = |kind, e: Node, services: Vec<Option<String>>, lifetime| {
            let (implementation, made) = self.made_type(e);
            let mut p = pending(kind, implementation, services, lifetime)?;
            p.made = made;
            Some(p)
        };
        match link.name.as_str() {
            "RegisterType" | "RegisterGeneric" => {
                let t = link.type_args.first().copied().or_else(|| args.first().and_then(|a| typeof_type(*a)))?;
                pending("type", base(t), Vec::new(), None)
            }
            "Register" => {
                let lambda = first_lambda?;
                let mut p = made("factory", *lambda, Vec::new(), None)?;
                p.default_service = link.type_args.first().map(|t| ty(*t));
                // A container's factory takes the context (`c => …`); `token.Register(() => Cancel())` is no container.
                let parameterless = lambda
                    .child_by_field_name("parameters")
                    .is_some_and(|ps| ps.kind() == "parameter_list" && children(ps).all(|c| !c.is_named()));
                if parameterless {
                    p.made = None;
                }
                Some(p)
            }
            "RegisterInstance" => {
                let mut p = made("instance", *args.first()?, Vec::new(), None)?;
                p.default_service = link.type_args.first().map(|t| ty(*t));
                Some(p)
            }
            "RegisterAssemblyTypes" => pending("scan", None, Vec::new(), None),
            // `AddDbContext<ShopContext>(…)`: the context, scoped; `AddDbContextFactory<T>` makes it on request.
            "AddDbContext" | "AddDbContextPool" | "AddPooledDbContextFactory" | "AddDbContextFactory" => {
                let t = *link.type_args.last()?;
                let service = *link.type_args.first()?;
                let lifetime = if link.name.contains("Factory") { "transient" } else { "scoped" };
                pending("type", base(t), vec![Some(ty(service))], Some(lifetime))
            }
            // `RegisterGeneratedFactory<Foo.Factory>()`: the delegate type, its implementation generated.
            "RegisterGeneratedFactory" => pending("factory", None, vec![Some(ty(*link.type_args.first()?))], None),
            name => {
                if !(name.starts_with("Add") || name.starts_with("TryAdd")) {
                    return None;
                }
                let lifetime = match name.trim_start_matches("TryAdd").trim_start_matches("Add") {
                    "Scoped" => "scoped",
                    "Transient" => "transient",
                    "Singleton" => "singleton",
                    "HttpClient" if link.type_args.len() == 2 => "transient",
                    _ => return None,
                };
                match (link.type_args.as_slice(), args.as_slice()) {
                    ([s, i], _) => pending("type", base(*i), vec![Some(ty(*s))], Some(lifetime)),
                    ([t], []) => pending("type", base(*t), vec![Some(ty(*t))], Some(lifetime)),
                    ([t], [a, ..]) => {
                        let kind = if first_lambda.is_some() { "factory" } else { "instance" };
                        made(kind, *a, vec![Some(ty(*t))], Some(lifetime))
                    }
                    ([], [s, rest @ ..]) => match (typeof_type(*s), rest.first()) {
                        (Some(s), Some(i)) => match typeof_type(*i) {
                            Some(i) => pending("type", base(i), vec![Some(ty(s))], Some(lifetime)),
                            None => made("factory", *i, vec![Some(ty(s))], Some(lifetime)),
                        },
                        (Some(s), None) => pending("type", base(s), vec![Some(ty(s))], Some(lifetime)),
                        // `AddSingleton(new Foo(…))`, `AddScoped(sp => …)`: registered as what it makes.
                        (None, None) => {
                            let kind = if first_lambda.is_some() { "factory" } else { "instance" };
                            let mut p = made(kind, *s, Vec::new(), Some(lifetime))?;
                            if let Some(i) = p.implementation.clone() {
                                p.services.push(Some(i));
                            }
                            Some(p)
                        }
                        (None, Some(_)) => None,
                    },
                    _ => None,
                }
            }
        }
    }

    /// A Microsoft DI service descriptor handed to the service collection: `services.Add(new ServiceDescriptor(typeof(IFoo),
    /// typeof(Foo), ServiceLifetime.Scoped))`, `TryAdd(ServiceDescriptor.Singleton<IFoo, Foo>())`,
    /// `TryAddEnumerable(new[] { ServiceDescriptor.Transient<IFoo, Foo>(), … })`, `Replace(ServiceDescriptor.Describe(…))`,
    /// `Insert(0, …)`. `node` is the descriptor: a `new ServiceDescriptor(…)` or a call of one of its static factories.
    /// Each is a registration at its own line, with the lifetime it states (a `ServiceLifetime` member; singleton for an
    /// instance), and the implementation where it names one. A keyed descriptor is said, not read; a descriptor built
    /// elsewhere and handed over in a variable is not read.
    fn service_descriptor(&mut self, node: Node) {
        let (factory, args, type_args) = match node.kind() {
            "object_creation_expression" => (None, node.child_by_field_name("arguments"), Vec::new()),
            "invocation_expression" => {
                let Some(f) = node.child_by_field_name("function").filter(|f| f.kind() == "member_access_expression") else { return };
                let (Some(e), Some(n)) = (f.child_by_field_name("expression"), f.child_by_field_name("name")) else { return };
                if base_type_name(self.src, e) != SERVICE_DESCRIPTOR {
                    return;
                }
                let type_args: Vec<Node> = if n.kind() == "generic_name" {
                    children(n).filter(|c| c.kind() == "type_argument_list").flat_map(children).filter(|c| c.is_named()).collect()
                } else {
                    Vec::new()
                };
                (Some(name_text(self.src, n)), node.child_by_field_name("arguments"), type_args)
            }
            _ => return,
        };
        // Handed to the service collection: an argument (perhaps in an array) of `Add`, `TryAdd*`, `Replace`, `Insert`;
        // how it is added (see `DiBinding::mode`).
        let mut up = node.parent();
        let handed = loop {
            let Some(n) = up else { break None };
            if n.kind() == "invocation_expression" {
                let name = n.child_by_field_name("function").and_then(|f| match f.kind() {
                    "member_access_expression" => f.child_by_field_name("name"),
                    _ => Some(f),
                });
                break name.and_then(|m| match name_text(self.src, m).as_str() {
                    "Add" | "Insert" => Some(None),
                    "TryAdd" => Some(Some("try")),
                    "TryAddEnumerable" => Some(Some("enumerable")),
                    "Replace" => Some(Some("replace")),
                    _ => None,
                });
            }
            if matches!(n.kind(), "block" | "lambda_expression" | "anonymous_method_expression" | "local_declaration_statement") {
                break None;
            }
            up = n.parent();
        };
        let Some(mode) = handed else { return };
        let args: Vec<Node> = args.into_iter().flat_map(argument_exprs).collect();
        let line = line(node);
        let ty = |n: Node| collapse(self.text(n), 120);
        let base = |n: Node| Some(base_type_name(self.src, n)).filter(|b| !b.is_empty());
        let lambda = |n: &Node| matches!(n.kind(), "lambda_expression" | "anonymous_method_expression");
        // `ServiceLifetime.Scoped`.
        let stated = |n: &Node| -> Option<&'static str> {
            let n = (n.kind() == "member_access_expression").then(|| n.child_by_field_name("name")).flatten()?;
            Some(match self.text(n) {
                "Singleton" => "singleton",
                "Scoped" => "scoped",
                "Transient" => "transient",
                _ => return None,
            })
        };
        let keyed = factory.as_deref().is_some_and(|f| f.starts_with("Keyed"))
            || (factory.is_none() && args.len() == 4)
            || (factory.as_deref() == Some("Describe") && args.len() == 4);
        if keyed {
            let what = factory.map(|f| format!("ServiceDescriptor.{f}(…)")).unwrap_or_else(|| "new ServiceDescriptor(…)".to_string());
            self.facts.unsupported.push(Unsupported { area: "di", construct: format!("keyed registration {what}"), line });
            return;
        }
        let pending = |kind, implementation: Option<String>, service: String, lifetime| PendingDi {
            kind,
            implementation,
            services: vec![Some(service)],
            lifetime,
            line,
            default_service: None,
            made: None,
            mode,
        };
        let made = |kind, e: Node, service: String, lifetime| {
            let (implementation, made) = self.made_type(e);
            PendingDi { made, ..pending(kind, implementation, service, lifetime) }
        };
        // What the descriptor's second argument gives: a type, a factory or an instance.
        let given = |s: Node, i: Node, lifetime: Option<&'static str>| match typeof_type(i) {
            Some(i) => pending("type", base(i), ty(s), lifetime),
            None if lambda(&i) => made("factory", i, ty(s), lifetime),
            None => made("instance", i, ty(s), lifetime.or(Some("singleton"))),
        };
        let p = match (factory.as_deref(), type_args.as_slice(), args.as_slice()) {
            // `new ServiceDescriptor(typeof(IFoo), typeof(Foo) | factory, lifetime)`, `(typeof(IFoo), instance)`.
            (None, [], [s, i, rest @ ..]) => {
                let Some(s) = typeof_type(*s) else { return };
                match rest.first() {
                    Some(l) if typeof_type(*i).is_none() => made("factory", *i, ty(s), stated(l)),
                    Some(l) => given(s, *i, stated(l)),
                    None => made("instance", *i, ty(s), Some("singleton")),
                }
            }
            (Some("Describe"), [], [s, i, l]) => {
                let Some(s) = typeof_type(*s) else { return };
                match typeof_type(*i) {
                    Some(_) => given(s, *i, stated(l)),
                    None => made("factory", *i, ty(s), stated(l)),
                }
            }
            (Some(f @ ("Singleton" | "Scoped" | "Transient")), targs, args) => {
                let lifetime = Some(match f {
                    "Singleton" => "singleton",
                    "Scoped" => "scoped",
                    _ => "transient",
                });
                match (targs, args) {
                    ([s, i], []) => pending("type", base(*i), ty(*s), lifetime),
                    ([s, _], [a]) | ([s], [a]) => {
                        let kind = if lambda(a) { "factory" } else { "instance" };
                        made(kind, *a, ty(*s), lifetime)
                    }
                    ([], [s, i]) => {
                        let Some(s) = typeof_type(*s) else { return };
                        given(s, *i, lifetime)
                    }
                    _ => return,
                }
            }
            _ => return,
        };
        self.flush_di(p);
    }

    /// A registration method this recognises but doesn't read, in words with its type arguments: a typed HTTP client
    /// (`AddHttpClient<CatalogClient>(…)`, one type argument) or a keyed one (`AddKeyedScoped<IFoo, Foo>("k")`).
    fn di_unread(&self, link: &Link) -> Option<String> {
        let name = link.name.as_str();
        let targs: Vec<String> = link.type_args.iter().map(|t| collapse(self.text(*t), 120)).collect();
        let written = || format!("{name}<{}>(…)", targs.join(", "));
        let bare = name.trim_start_matches("Try");
        if bare == "AddHttpClient" && targs.len() == 1 {
            return Some(format!("typed HTTP client {}", written()));
        }
        if matches!(bare, "AddKeyedScoped" | "AddKeyedTransient" | "AddKeyedSingleton") {
            return Some(format!("keyed registration {}", written()));
        }
        None
    }

    fn flush_di(&mut self, p: PendingDi) {
        // An assembly scan registers types by a rule this doesn't apply: said, with the name filter it read (`*Service`).
        if p.kind == "scan" {
            let filter = p.implementation.as_deref().map(|f| format!(" [{f}]")).unwrap_or_default();
            self.facts.unsupported.push(Unsupported { area: "di", construct: format!("assembly scan RegisterAssemblyTypes(…){filter}"), line: p.line });
        }
        let mut services = p.services;
        // Autofac registers a component as its own type unless `As` says otherwise, and a factory as what it makes.
        let mut service_made = false;
        if services.is_empty() {
            let own = p.default_service.or_else(|| p.implementation.clone().filter(|_| p.kind != "scan"));
            if own.is_none() && p.kind != "scan" {
                // Nothing known on either side (`token.Register(() => …)` is not a container at all).
                if p.made.is_none() {
                    return;
                }
                service_made = true;
            }
            services.push(own);
        }
        services.dedup();
        for service in services {
            self.facts.di.push(DiBinding {
                service,
                implementation: p.implementation.clone(),
                lifetime: p.lifetime,
                kind: p.kind,
                line: p.line,
                made: p.made.clone(),
                service_made,
                mode: p.mode,
            });
        }
    }

    /// What a factory (a lambda or anonymous method) or an instance gives the container: the type's name when the
    /// code says it (`new Foo(…)`, `sp.GetRequiredService<Foo>()`, a cast, a local of the lambda declared or created
    /// as one, a variable or parameter of a declared type), else the encoded type expression of what it returns (a
    /// factory method's call, a property), for the index to resolve against the repo's declarations.
    fn made_type(&self, e: Node) -> (Option<String>, Option<String>) {
        if let Some(t) = self.created_type(e) {
            return (Some(t), None);
        }
        match self.factory_result(e, &mut Vec::new()) {
            Some(TypeExpr::Named(n) | TypeExpr::Generic { name: n, .. }) => {
                let bare = n.rsplit('.').next().unwrap_or(&n).to_string();
                (Some(bare).filter(|b| !b.is_empty()), None)
            }
            Some(t @ (TypeExpr::Call { .. } | TypeExpr::Path { .. })) => (None, Some(t.encode())),
            _ => (None, None),
        }
    }

    /// The type of what `e` evaluates to, as a factory's result, with `locals` the lambda's own (name, type).
    fn factory_result(&self, e: Node, locals: &mut Vec<(String, TypeExpr)>) -> Option<TypeExpr> {
        match e.kind() {
            "object_creation_expression" => self.created_type(e).map(TypeExpr::Named),
            "parenthesized_expression" => children(e).find(|c| c.is_named()).and_then(|c| self.factory_result(c, locals)),
            "conditional_expression" => {
                let a = self.factory_result(e.child_by_field_name("consequence")?, locals)?;
                let b = self.factory_result(e.child_by_field_name("alternative")?, locals)?;
                (a == b).then_some(a)
            }
            "identifier" if locals.iter().any(|(n, _)| n == self.text(e)) => {
                locals.iter().rev().find(|(n, _)| n == self.text(e)).map(|(_, t)| t.clone())
            }
            "lambda_expression" | "anonymous_method_expression" => {
                let body = e.child_by_field_name("body").or_else(|| children(e).filter(|c| c.is_named()).last())?;
                if body.kind() != "block" {
                    return self.factory_result(body, locals);
                }
                // The lambda's statements in order: its locals as declared, and what each `return` gives.
                let mut order = Vec::new();
                let mut stack = vec![body];
                while let Some(n) = stack.pop() {
                    match n.kind() {
                        "lambda_expression" | "anonymous_method_expression" | "local_function_statement" => {}
                        "return_statement" | "local_declaration_statement" => order.push(n),
                        _ => stack.extend(children(n).collect::<Vec<_>>().into_iter().rev()),
                    }
                }
                let mut returns = Vec::new();
                for n in order {
                    if n.kind() == "return_statement" {
                        returns.push(children(n).find(|c| c.is_named()).and_then(|r| self.factory_result(r, locals)));
                        continue;
                    }
                    let Some(decl) = children(n).find(|c| c.kind() == "variable_declaration") else { continue };
                    let declared = decl.child_by_field_name("type").filter(|t| self.text(*t) != "var").and_then(|t| type_expr(self.src, t));
                    for d in children(decl).filter(|c| c.kind() == "variable_declarator") {
                        let Some(name) = d.child_by_field_name("name") else { continue };
                        let value = children(d).filter(|c| c.is_named() && *c != name).last();
                        if let Some(t) = declared.clone().or_else(|| value.and_then(|v| self.factory_result(v, locals))) {
                            locals.push((self.text(name).to_string(), t));
                        }
                    }
                }
                match returns.first() {
                    Some(Some(t)) if returns.iter().all(|x| x.as_ref() == Some(t)) => Some(t.clone()),
                    _ => None,
                }
            }
            _ => self.expr_type(e),
        }
    }

    /// The type an expression obviously creates: `new Foo(…)`, or a lambda or anonymous method returning only that.
    fn created_type(&self, e: Node) -> Option<String> {
        match e.kind() {
            "object_creation_expression" => {
                Some(base_type_name(self.src, e.child_by_field_name("type")?)).filter(|t| !t.is_empty())
            }
            "parenthesized_expression" => children(e).find(|c| c.is_named()).and_then(|c| self.created_type(c)),
            "lambda_expression" | "anonymous_method_expression" => {
                let body = e.child_by_field_name("body").or_else(|| children(e).filter(|c| c.is_named()).last())?;
                if body.kind() != "block" {
                    return self.created_type(body);
                }
                let mut returns = Vec::new();
                let mut stack = vec![body];
                while let Some(n) = stack.pop() {
                    match n.kind() {
                        "return_statement" => returns.push(children(n).find(|c| c.is_named())),
                        "lambda_expression" | "anonymous_method_expression" | "local_function_statement" => {}
                        _ => stack.extend(children(n)),
                    }
                }
                let types: Vec<Option<String>> = returns.into_iter().map(|r| r.and_then(|r| self.created_type(r))).collect();
                match types.first() {
                    Some(Some(t)) if types.iter().all(|x| x.as_ref() == Some(t)) => Some(t.clone()),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Typed locals: `Foo x = …`, `var x = new Foo()`.
    fn locals(&mut self, node: Node) {
        let Some(decl) = children(node).find(|c| c.kind() == "variable_declaration") else { return };
        let declared = decl.child_by_field_name("type").filter(|t| self.text(*t) != "var").and_then(|t| type_expr(self.src, t));
        for d in children(decl).filter(|c| c.kind() == "variable_declarator") {
            let Some(n) = d.child_by_field_name("name") else { continue };
            let value = children(d).filter(|c| c.is_named() && *c != n).last();
            // `var e = _mapper.Map<X>(src)` is an `X`.
            let mapped = value.filter(|v| v.kind() == "invocation_expression").and_then(|v| self.message_type(v)).map(TypeExpr::Named);
            if let Some(ty) = declared.clone().or(mapped).or_else(|| value.and_then(|v| self.expr_type(v))) {
                self.bind(self.text(n).to_string(), ty);
            }
            if let Some(entity) = value.and_then(|v| self.builder_entity(v)) {
                self.builders.push((self.text(n).to_string(), entity));
            }
            // `var api = app.MapGroup("api/orders").RequireAuthorization();`: endpoints mapped on `api` are under its
            // prefix.
            if let Some(v) = value.filter(|v| self.holds_group(*v)) {
                let base = self.route_base(Some(v));
                self.group_vars.push((self.current(), self.text(n).to_string(), base));
            }
            // `IQueryable<Order> q = ctx.Orders;`: queried through the local.
            let queryable = decl.child_by_field_name("type").is_some_and(|t| {
                t.kind() == "generic_name" && matches!(name_text(self.src, t).as_str(), "IQueryable" | "IOrderedQueryable" | "IEnumerable")
            });
            if queryable && let Some(name) = value.and_then(|v| self.queried_member(v)) {
                self.db(DbFact { kind: "query", sym: self.current(), entity: String::new(), name, line: line(d) });
            }
            // `var sql = $"CALL p('{ids}')";`, run later through the local.
            if let Some(name) = value.and_then(|v| sql_text(self.src, v)).and_then(|t| call_procedure(&t)) {
                self.db(DbFact { kind: "procedure", sym: self.current(), entity: String::new(), name, line: line(d) });
            }
        }
    }

    /// Type references inside a type expression, skipping framework types. `skip_base` leaves out the outermost
    /// name (already recorded as an instantiation). A qualified name (`Models.Domain`) keeps its qualifier as the
    /// reference's receiver.
    fn type_refs(&mut self, t: Node, skip_base: bool) {
        let mut stack: Vec<(Node, bool, Option<String>)> = vec![(t, skip_base, None)];
        while let Some((n, skip, qualifier)) = stack.pop() {
            match n.kind() {
                "identifier" => {
                    let name = self.text(n);
                    if !skip && !COMMON_TYPES.contains(&name) {
                        self.add(name.to_string(), qualifier, None, RefKind::TypeRef, n);
                    }
                }
                "qualified_name" => {
                    if let Some(name) = n.child_by_field_name("name") {
                        stack.push((name, skip, type_qualifier(self.src, n)));
                    }
                }
                "generic_name" => {
                    for c in children(n) {
                        if c.kind() == "identifier" {
                            stack.push((c, skip, qualifier.clone()));
                        } else {
                            stack.extend(children(c).filter(|a| a.is_named()).map(|a| (a, false, None)));
                        }
                    }
                }
                // `(string Number, decimal Amount)`: the elements' names aren't types.
                "tuple_element" => stack.extend(n.child_by_field_name("type").map(|t| (t, skip, None))),
                _ => stack.extend(children(n).filter(|c| c.is_named()).map(|c| (c, skip, None))),
            }
        }
    }

    /// The type of a local, parameter or (record) property in scope named `name`.
    fn local(&self, name: &str) -> Option<TypeExpr> {
        self.scopes.iter().rev().take_while(|s| s.callable || s.type_name.is_some()).find_map(|s| s.locals.get(name).cloned())
    }

    fn expr_type(&self, e: Node) -> Option<TypeExpr> {
        match e.kind() {
            "this_expression" | "this" => Some(TypeExpr::root(PathRoot::SelfType)),
            // `base.x` is the base type's `x`, not an override or `new` member on the caller's type (as for calls).
            "base" => None,
            "identifier" => {
                let name = self.text(e);
                self.local(name).or_else(|| (!starts_upper(name)).then(|| TypeExpr::root(PathRoot::Prop(name.to_string()))))
            }
            "await_expression" | "parenthesized_expression" | "postfix_unary_expression" => {
                children(e).find(|c| c.is_named()).and_then(|c| self.expr_type(c))
            }
            "cast_expression" | "as_expression" => {
                e.child_by_field_name("type").or_else(|| e.child_by_field_name("right")).and_then(|t| type_expr(self.src, t))
            }
            "object_creation_expression" => e.child_by_field_name("type").and_then(|t| type_expr(self.src, t)),
            "member_access_expression" => {
                let n = e.child_by_field_name("name")?;
                let inner = unbang(e.child_by_field_name("expression")?);
                let name = name_text(self.src, n);
                match self.expr_type(inner) {
                    Some(t) => Some(t.then(Seg::Member(name))),
                    None if inner.kind() == "identifier" => {
                        Some(TypeExpr::root(PathRoot::Static(self.text(inner).to_string())).then(Seg::Member(name)))
                    }
                    None => None,
                }
            }
            "element_access_expression" => Some(self.expr_type(e.child_by_field_name("expression")?)?.then(Seg::Index)),
            // `x?.B`: as `x.B`.
            "conditional_access_expression" => {
                let (inner, n) = conditional_member(e)?;
                let name = name_text(self.src, n);
                match self.expr_type(inner) {
                    Some(t) => Some(t.then(Seg::Member(name))),
                    None if inner.kind() == "identifier" => {
                        Some(TypeExpr::root(PathRoot::Static(self.text(inner).to_string())).then(Seg::Member(name)))
                    }
                    None => None,
                }
            }
            "invocation_expression" => {
                let f = e.child_by_field_name("function")?;
                // `mocker.GetMock<IFoo>()`, `container.Resolve<IFoo>()`, `xs.Cast<Foo>()`: the type argument says.
                let g = if f.kind() == "member_access_expression" { f.child_by_field_name("name")? } else { f };
                let targs: Vec<Node> = if g.kind() == "generic_name" {
                    children(g).filter(|c| c.kind() == "type_argument_list").flat_map(children).filter(|c| c.is_named()).collect()
                } else {
                    Vec::new()
                };
                let method = name_text(self.src, g);
                match (method.as_str(), &targs[..]) {
                    (n, [arg]) if TYPE_ARG_FACTORIES.contains(&n) => return type_expr(self.src, *arg),
                    // `resolver.GetKeyedInstance<IFoo, Tenants>(key)`: the service is the first.
                    (n, [arg, _]) if KEYED_FACTORIES.contains(&n) => return type_expr(self.src, *arg),
                    // `_mapper.Map<Dest>(src)`, `Map<Src, Dest>(src)`, `src.Adapt<Dest>()`: the destination.
                    ("Map" | "Adapt", [.., last]) => return type_expr(self.src, *last),
                    ("Cast" | "OfType" | "Set" | "GetAllInstances" | "GetServices" | "ResolveAll", [arg]) => {
                        return type_expr(self.src, *arg).map(|t| TypeExpr::Array(Box::new(t)));
                    }
                    // `provider.GetService(typeof(IFoo))`.
                    (n, []) if TYPE_ARG_FACTORIES.contains(&n) => {
                        if let Some(t) = e.child_by_field_name("arguments").and_then(first_argument).and_then(typeof_type) {
                            return type_expr(self.src, t);
                        }
                    }
                    _ => {}
                }
                match f.kind() {
                    // A local delegate called: what it returns; one of a named delegate type, that type invoked.
                    "identifier" => match self.local(self.text(f)) {
                        Some(TypeExpr::Delegate(ret)) => Some(*ret),
                        Some(t) => Some(t.then(Seg::Call("Invoke".to_string()))),
                        None => Some(TypeExpr::Call { owner: None, name: name_text(self.src, f) }),
                    },
                    // `x?.M(…)`: as `x.M(…)`.
                    "conditional_access_expression" => {
                        let (inner, name) = conditional_member(f)?;
                        let name = name_text(self.src, name);
                        match self.expr_type(inner) {
                            Some(t) => Some(t.then(Seg::Call(name))),
                            None if inner.kind() == "identifier" => {
                                Some(TypeExpr::Call { owner: Some(self.text(inner).to_string()), name })
                            }
                            None => None,
                        }
                    }
                    "generic_name" => Some(TypeExpr::Call { owner: None, name: name_text(self.src, f) }),
                    "member_access_expression" => {
                        let name = name_text(self.src, f.child_by_field_name("name")?);
                        let inner = unbang(f.child_by_field_name("expression")?);
                        match self.expr_type(inner) {
                            Some(t) => Some(t.then(Seg::Call(name))),
                            None if inner.kind() == "identifier" => {
                                Some(TypeExpr::Call { owner: Some(self.text(inner).to_string()), name })
                            }
                            None => None,
                        }
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// The receiver's type in a member call `x.f(…)`: its expression's type, or a bare identifier read as a type.
    fn call_receiver_type(&self, f: Node) -> Option<TypeExpr> {
        let i = unbang(f.child_by_field_name("expression")?);
        // `base.M()` is the base type's `M`, not an override on the caller's type.
        if i.kind() == "base" {
            return None;
        }
        self.expr_type(i).or_else(|| (i.kind() == "identifier").then(|| TypeExpr::Named(self.text(i).to_string())))
    }

    /// `c.AddBus<IOrders>()`, `Get<IOrders>()`, `x?.Resolve<IOrders>()`: the type arguments of the method called,
    /// each a use of its type.
    fn invoked_type_args(&mut self, node: Node) {
        let Some(f) = node.child_by_field_name("function") else { return };
        let name = match f.kind() {
            "generic_name" => Some(f),
            "member_access_expression" => f.child_by_field_name("name"),
            "conditional_access_expression" => conditional_member(f).map(|(_, n)| n),
            _ => None,
        };
        if let Some(list) = name.filter(|n| n.kind() == "generic_name").and_then(|n| children(n).find(|c| c.kind() == "type_argument_list")) {
            self.visit(list);
        }
    }

    /// `nameof(Order)`, `nameof(Order.Total)`: the type named. A name that isn't a type's resolves to none.
    fn nameof_ref(&mut self, node: Node) {
        if node.child_by_field_name("function").is_none_or(|f| f.kind() != "identifier" || self.text(f) != "nameof") {
            return;
        }
        let Some(mut e) = node.child_by_field_name("arguments").and_then(first_argument) else { return };
        while e.kind() == "member_access_expression" {
            let Some(inner) = e.child_by_field_name("expression") else { return };
            e = inner;
        }
        let name = self.text(e);
        if e.kind() == "identifier" && starts_upper(name) && self.local(name).is_none() {
            let name = name.to_string();
            self.with_usage("nameof", |cx| cx.add(name, None, None, RefKind::TypeRef, e));
        }
    }

    fn call(&mut self, node: Node) {
        self.pending_arg_types = node.child_by_field_name("arguments").and_then(|a| self.argument_types(a));
        self.call_ref(node);
        self.pending_arg_types = None;
    }

    /// The types of a call's arguments as far as the call site tells them (see [`FileFacts::arg_types`]): `None` when an
    /// argument is named or passed `out`/`ref`, which positions can't match.
    fn argument_types(&self, list: Node) -> Option<String> {
        let mut out: Vec<serde_json::Value> = Vec::new();
        let mut any = false;
        for a in children(list).filter(|c| c.kind() == "argument") {
            if children(a).any(|c| matches!(c.kind(), "out" | "ref" | "in")) {
                return None;
            }
            let name = a.child_by_field_name("name");
            // The value: the argument's last node that isn't its name or the colon (`this` is no named node).
            let e = children(a).filter(|c| Some(*c) != name && c.kind() != ":").last()?;
            let t = self.argument_type(e).map(|t| t.encode());
            any |= t.is_some();
            out.push(match name {
                // `domainId: id`: matched to the parameter of that name.
                Some(n) => {
                    any = true;
                    serde_json::json!({ "n": self.text(n), "t": t })
                }
                None => serde_json::json!(t),
            });
        }
        (!out.is_empty() && any).then(|| serde_json::to_string(&out).expect("serializes"))
    }

    /// An argument's type: a literal's, a creation's, or the expression's as [`Self::expr_type`] reads it. An integer
    /// literal is `@int`: it converts to any numeric type it fits (and `0` to an enum).
    fn argument_type(&self, e: Node) -> Option<TypeExpr> {
        let named = |n: &str| Some(TypeExpr::Named(n.to_string()));
        match e.kind() {
            "string_literal" | "verbatim_string_literal" | "raw_string_literal" | "interpolated_string_expression" => named("string"),
            "this_expression" | "this" => self.expr_type(e),
            // `x.ToString()`, `string.Format(…)`, `string.Join(…)`: a string whatever `x` is.
            "invocation_expression"
                if e.child_by_field_name("function").is_some_and(|f| {
                    f.kind() == "member_access_expression"
                        && f.child_by_field_name("name").is_some_and(|n| {
                            let n = self.text(n);
                            n == "ToString"
                                || (matches!(n, "Format" | "Join" | "Concat")
                                    && f.child_by_field_name("expression").is_some_and(|r| matches!(self.text(r), "string" | "String")))
                        })
                }) =>
            {
                named("string")
            }
            "character_literal" => named("char"),
            "boolean_literal" => named("bool"),
            "integer_literal" => {
                let t = self.text(e).to_ascii_lowercase();
                if t.ends_with("ul") || t.ends_with("lu") {
                    named("ulong")
                } else if t.ends_with('l') {
                    named("long")
                } else if t.ends_with('u') {
                    named("uint")
                } else {
                    named("@int")
                }
            }
            "real_literal" => {
                let t = self.text(e).to_ascii_lowercase();
                named(if t.ends_with('m') { "decimal" } else if t.ends_with('f') { "float" } else { "double" })
            }
            "invocation_expression"
                if e.child_by_field_name("function").is_some_and(|f| self.text(f) == "nameof") =>
            {
                named("string")
            }
            "null_literal" | "default_expression" | "lambda_expression" | "anonymous_method_expression" => None,
            "object_creation_expression" => e.child_by_field_name("type").and_then(|t| type_expr(self.src, t)),
            "prefix_unary_expression" if children(e).any(|c| self.text(c) == "-") => {
                children(e).find(|c| c.is_named()).and_then(|c| self.argument_type(c))
            }
            _ => self.expr_type(e),
        }
    }

    fn call_ref(&mut self, node: Node) {
        let Some(f) = node.child_by_field_name("function") else { return };
        let args =
            node.child_by_field_name("arguments").map(|a| children(a).filter(|c| c.kind() == "argument").count() as u32);
        match f.kind() {
            // A local or parameter holding a delegate, called: the delegate invoked.
            "identifier" if !mocked_in_setup(self.src, f) && let Some(t) = self.local(self.text(f)) => {
                let receiver = self.text(f).to_string();
                self.add_call("Invoke".to_string(), Some(receiver), Some(t.encode()), f, args);
            }
            "identifier" | "generic_name" => {
                let name = name_text(self.src, f);
                self.add_call(name, None, None, f, args);
            }
            // `x?.M(…)`: as `x.M(…)`.
            "conditional_access_expression" => {
                let Some((inner, n)) = conditional_member(f) else { return };
                let receiver = Some(collapse(self.text(inner), RECEIVER_CAP));
                let rt = if inner.kind() == "base" {
                    None
                } else {
                    self.expr_type(inner).or_else(|| (inner.kind() == "identifier").then(|| TypeExpr::Named(self.text(inner).to_string())))
                };
                let name = name_text(self.src, n);
                self.add_call(name, receiver, rt.map(|t| t.encode()), n, args);
            }
            "member_access_expression" => {
                let Some(n) = f.child_by_field_name("name") else { return };
                let inner = f.child_by_field_name("expression").map(unbang);
                let receiver = inner.map(|i| collapse(self.text(i), RECEIVER_CAP));
                let rt = self.call_receiver_type(f);
                let name = name_text(self.src, n);
                // `_mapper.Map<Dest>(src)`, `src.Adapt<Dest>()`: the mapper creates a `Dest`, so code sending it is
                // found as a creator of the request (as with `new Dest(…)`).
                if n.kind() == "generic_name"
                    && matches!(name.as_str(), "Map" | "Adapt")
                    && let Some(dest) = children(n).filter(|c| c.kind() == "type_argument_list").flat_map(children).filter(|c| c.is_named()).last()
                {
                    let created = base_type_name(self.src, dest);
                    if !created.is_empty() {
                        self.add(created, type_qualifier(self.src, dest), None, RefKind::Instantiate, dest);
                    }
                }
                self.add_call(name, receiver, rt.map(|t| t.encode()), n, args);
            }
            _ => {}
        }
    }

    fn db(&mut self, fact: DbFact) {
        if !self.facts.db.contains(&fact) {
            self.facts.db.push(fact);
        }
    }

    /// `Orders` in `ctx.Orders` or `this.ctx.Orders`, a member a query reads from.
    fn queried_member(&self, e: Node) -> Option<String> {
        (e.kind() == "member_access_expression").then(|| e.child_by_field_name("name")).flatten().map(|n| name_text(self.src, n))
    }

    /// The entity of `Entity<T>(…)` / `x.Entity<T>(…)`.
    fn entity_call(&self, call: Node) -> Option<String> {
        let f = call.child_by_field_name("function")?;
        let n = match f.kind() {
            "member_access_expression" => f.child_by_field_name("name")?,
            _ => f,
        };
        (n.kind() == "generic_name" && name_text(self.src, n) == "Entity").then(|| first_type_arg(self.src, n)).flatten()
    }

    /// For `builder.OwnsMany(e => e.Payments, …)` or `OwnsOne(e => e.Address)` (or the navigation by name) on a
    /// builder of `Order`: the owned type, written `Order#Payments` (the navigation's type is resolved at query time).
    fn owned_call(&self, call: Node) -> Option<String> {
        let f = call.child_by_field_name("function").filter(|f| f.kind() == "member_access_expression")?;
        let n = f.child_by_field_name("name")?;
        if !matches!(name_text(self.src, n).as_str(), "OwnsMany" | "OwnsOne") {
            return None;
        }
        let owner = self.builder_entity(f.child_by_field_name("expression")?)?;
        let first = call.child_by_field_name("arguments").and_then(first_argument)?;
        let nav = match first.kind() {
            "lambda_expression" => {
                let body = first.child_by_field_name("body")?;
                (body.kind() == "member_access_expression").then(|| body.child_by_field_name("name")).flatten().map(|m| self.text(m).to_string())?
            }
            _ => string_value(self.src, first)?,
        };
        Some(format!("{owner}#{nav}"))
    }

    /// The entity an expression builds: `modelBuilder.Entity<T>()`, a fluent chain on one
    /// (`….Entity<T>().HasKey(…)`), an owned type's builder (`….OwnsOne(e => e.Address)`, see [`Self::owned_call`]), or
    /// an identifier bound to a builder.
    fn builder_entity(&self, e: Node) -> Option<String> {
        match e.kind() {
            "invocation_expression" => self.entity_call(e).or_else(|| self.owned_call(e)).or_else(|| {
                let f = e.child_by_field_name("function").filter(|f| f.kind() == "member_access_expression")?;
                self.builder_entity(f.child_by_field_name("expression")?)
            }),
            "identifier" => {
                let name = self.text(e);
                self.builders.iter().rev().find(|(p, _)| p == name).map(|(_, entity)| entity.clone())
            }
            "parenthesized_expression" => children(e).find(|c| c.is_named()).and_then(|c| self.builder_entity(c)),
            _ => None,
        }
    }

    /// EF calls: `ToTable` / `ToView` on an entity builder, `Set<T>()`, and stored procedures run through SQL text
    /// (`FromSqlRaw("EXEC sp_x …")`) or a call with `CommandType.StoredProcedure`.
    fn db_call(&mut self, node: Node) {
        let Some(f) = node.child_by_field_name("function") else { return };
        let (name, receiver) = match f.kind() {
            "member_access_expression" => (f.child_by_field_name("name"), f.child_by_field_name("expression")),
            "identifier" | "generic_name" => (Some(f), None),
            _ => (None, None),
        };
        let args: Vec<Node> = node
            .child_by_field_name("arguments")
            .map(|a| {
                children(a).filter(|c| c.kind() == "argument").filter_map(|c| children(c).filter(|x| x.is_named()).last()).collect()
            })
            .unwrap_or_default();
        if let Some(n) = name
            && LINQ_OPS.contains(&name_text(self.src, n).as_str())
        {
            self.lambda_members(&args);
        }
        if let Some(n) = name {
            match name_text(self.src, n).as_str() {
                "ToTable" | "ToView" => {
                    let entity = match receiver {
                        Some(r) => self.builder_entity(r),
                        None => self.configured.iter().rev().find_map(|c| c.clone()),
                    };
                    if let (Some(entity), Some(table)) = (entity, args.first().and_then(|a| string_value(self.src, *a))) {
                        let schema = args.get(1).and_then(|a| string_value(self.src, *a));
                        // A table of an owned type: `Order#Payments`, its owner and navigation.
                        let kind = if entity.contains('#') { "owned_table" } else { "table" };
                        self.db(DbFact { kind, sym: self.current(), entity, name: qualify(schema, table), line: line(node) });
                    }
                    return;
                }
                "Set" if n.kind() == "generic_name" => {
                    if let Some(entity) = first_type_arg(self.src, n) {
                        self.db(DbFact { kind: "set", sym: self.current(), entity, name: String::new(), line: line(node) });
                    }
                    return;
                }
                // `q.Include(o => o.Lines)`, `.ThenInclude(l => l.Product)`, `.Include("Lines.Product")`: each
                // navigation the query loads.
                "Include" | "ThenInclude" => {
                    for nav in args.first().map(|a| navigation_path(self.src, *a)).unwrap_or_default() {
                        self.db(DbFact { kind: "include", sym: self.current(), entity: String::new(), name: nav, line: line(node) });
                    }
                    return;
                }
                // `b.Navigation(o => o.Lines).AutoInclude()`: every query of the owner loads the navigation.
                "AutoInclude" if !args.first().is_some_and(|a| self.text(*a) == "false") => {
                    if let Some(nav_call) = receiver.filter(|r| r.kind() == "invocation_expression")
                        && let Some(f) = nav_call.child_by_field_name("function").filter(|f| f.kind() == "member_access_expression")
                        && f.child_by_field_name("name").is_some_and(|m| self.text(m) == "Navigation")
                        && let Some(entity) = f.child_by_field_name("expression").and_then(|b| self.builder_entity(b))
                        && let Some(nav) = nav_call
                            .child_by_field_name("arguments")
                            .and_then(first_argument)
                            .and_then(|a| navigation_path(self.src, a).pop())
                    {
                        self.db(DbFact { kind: "auto_include", sym: self.current(), entity, name: nav, line: line(node) });
                    }
                    return;
                }
                "UseLazyLoadingProxies" => {
                    self.db(DbFact { kind: "lazy_loading", sym: self.current(), entity: String::new(), name: String::new(), line: line(node) });
                    return;
                }
                "AddDbContext" | "AddDbContextPool" | "AddDbContextFactory" | "AddPooledDbContextFactory" if n.kind() == "generic_name" => {
                    // `AddDbContext<IShopContext, ShopContext>`: the last type argument is the context.
                    let context = children(n)
                        .filter(|c| c.kind() == "type_argument_list")
                        .flat_map(children)
                        .filter(|c| c.is_named())
                        .last()
                        .map(|t| base_type_name(self.src, t));
                    if let (Some(context), Some(conn)) = (context.filter(|c| !c.is_empty()), self.connection_in(node)) {
                        self.db(DbFact { kind: "connection", sym: self.current(), entity: context, name: conn, line: line(node) });
                    }
                    return;
                }
                // `optionsBuilder.UseMySql(Settings.ShopDbConnection, …)` in a context's `OnConfiguring`.
                name if is_provider_call(name)
                    && self.scopes.iter().rev().find(|s| s.callable).is_some_and(|s| self.facts.symbols[s.sym].name == "OnConfiguring") =>
                {
                    let context = self.enclosing_type().map(|t| t.rsplit('.').next().unwrap_or(&t).to_string());
                    if let (Some(context), Some(conn)) = (context, args.first().and_then(|a| connection_name(self.src, *a))) {
                        self.db(DbFact { kind: "connection", sym: self.current(), entity: context, name: conn, line: line(node) });
                    }
                    return;
                }
                _ => {}
            }
        }
        self.procedure_call(node);
    }

    /// `q.Where(a => a.Customer.Name == n)`: each member reached from the lambda's parameter, a `lambda_member` fact
    /// (once per symbol and name). On a query over a table, a navigation named so is a join of its table.
    fn lambda_members(&mut self, args: &[Node]) {
        for lambda in args.iter().filter(|a| a.kind() == "lambda_expression") {
            let (Some(p), Some(body)) = (lambda_param(*lambda), lambda.child_by_field_name("body")) else { continue };
            let param = self.text(p);
            let mut stack = vec![body];
            while let Some(e) = stack.pop() {
                if e.kind() == "member_access_expression" {
                    let mut root = e;
                    let mut names = Vec::new();
                    while root.kind() == "member_access_expression" {
                        if let Some(n) = root.child_by_field_name("name") {
                            names.push(name_text(self.src, n));
                        }
                        let Some(x) = root.child_by_field_name("expression") else { break };
                        root = x;
                    }
                    if root.kind() == "identifier" && self.text(root) == param {
                        let sym = self.current();
                        for name in names {
                            if !self.facts.db.iter().any(|f| f.kind == "lambda_member" && f.sym == sym && f.name == name) {
                                self.facts.db.push(DbFact { kind: "lambda_member", sym, entity: String::new(), name, line: line(e) });
                            }
                        }
                    }
                }
                stack.extend(children(e).filter(|c| c.is_named()));
            }
        }
    }

    /// The connection a context registration configures: the first argument of the provider call
    /// (`o.UseMySql(Settings.ShopDbConnection, …)`) inside `call`'s arguments, named as [`connection_name`] does.
    fn connection_in(&self, call: Node) -> Option<String> {
        let args = call.child_by_field_name("arguments")?;
        let mut stack = vec![args];
        while let Some(n) = stack.pop() {
            if n.kind() == "invocation_expression"
                && let Some(f) = n.child_by_field_name("function")
                && let Some(m) = match f.kind() {
                    "member_access_expression" => f.child_by_field_name("name"),
                    _ => Some(f),
                }
                && is_provider_call(&name_text(self.src, m))
            {
                return n.child_by_field_name("arguments").and_then(first_argument).and_then(|a| connection_name(self.src, a));
            }
            stack.extend(children(n).collect::<Vec<_>>().into_iter().rev());
        }
        None
    }

    /// SQL text in a string (a literal, an interpolated string, or a `+` concatenation of strings and values): the
    /// tables it reads and writes, as `sql_read` and `sql_write` facts of the enclosing symbol (a method, or the
    /// field or property holding the text, whose users run it). Text counts as SQL when it starts like a statement
    /// (`SELECT … FROM`, `INSERT INTO`, `UPDATE … SET`, `DELETE FROM`, `REPLACE INTO`, `TRUNCATE`, `WITH … AS`) and
    /// either is passed to a call or command running SQL (`Query`, `Execute`, `FromSqlRaw`, `new MySqlCommand`), is
    /// held in a name saying so (`sql`, `query`, `CommandText`), or reads as SQL beyond its first words (a `WHERE`,
    /// `JOIN`, `VALUES`, parameter or `;`). Values spliced in are names built at run time (see
    /// [`crate::sql::DYNAMIC`]).
    fn sql_string(&mut self, node: Node) {
        let is_concat = |n: Node| n.kind() == "binary_expression" && n.child_by_field_name("operator").is_some_and(|o| &self.src[o.byte_range()] == "+");
        if node.kind() == "binary_expression" && !is_concat(node) {
            return;
        }
        // A piece of a concatenation is read with the whole.
        let mut up = node.parent();
        while let Some(p) = up.filter(|p| p.kind() == "parenthesized_expression") {
            up = p.parent();
        }
        if up.is_some_and(is_concat) {
            return;
        }
        let mut leaves = Vec::new();
        string_leaves(self.src, node, &mut leaves);
        let Some(first) = leaves.first().and_then(|l| l.1.as_deref()) else { return };
        if sql_verb(first).is_none() {
            return;
        }
        // The text, with values as `DYNAMIC` and line breaks kept; `rows` holds each of its lines' row in the file (an
        // escaped `\n` ends a line of the text, and a `--` comment, but not one of the file).
        let start = node.start_position().row;
        let mut text = String::new();
        let mut rows = vec![start];
        let mut row = start;
        for (at, piece) in &leaves {
            while row < *at {
                text.push('\n');
                row += 1;
                rows.push(row);
            }
            match piece {
                Some(p) => {
                    for c in p.chars() {
                        match c {
                            '\n' => {
                                row += 1;
                                rows.push(row);
                                text.push('\n');
                            }
                            ESCAPED_NEWLINE => {
                                rows.push(row);
                                text.push('\n');
                            }
                            c => text.push(c),
                        }
                    }
                }
                None => text.push_str(crate::sql::DYNAMIC),
            }
        }
        if !sql_shaped(&text) {
            return;
        }
        // Without a call or name saying SQL, the text must read as SQL, and not be in a type querying InfluxDB.
        let influx = || self.enclosing_type().is_some_and(|t| t.rsplit('.').next().unwrap_or(&t).to_ascii_lowercase().contains("influx"));
        match self.sql_context(node) {
            Some(true) => {}
            None if sql_detailed(&text) && !influx() => {}
            _ => return,
        }
        let sym = self.current();
        // Text a field or property holds as its value is run by its users; text its getter runs is the property's.
        let held = sym.is_some_and(|s| self.facts.symbols[s].kind == SymbolKind::Property) && sql_value(self.src, node);
        for (name, kind, offset) in crate::sql::code_tables(&text) {
            if kind != "procedure" {
                let line = *rows.get(offset as usize).unwrap_or(&row) as u32 + 1;
                self.db(DbFact { kind, sym, entity: if held { HELD.into() } else { String::new() }, name, line });
            }
        }
        // A table named at run time: said, so the method's tables aren't taken for all it touches.
        for (kind, offset) in crate::sql::dynamic_tables(&text) {
            let line = *rows.get(offset as usize).unwrap_or(&row) as u32 + 1;
            let verb = if kind == "sql_write" { "writes" } else { "reads" };
            let construct = format!("SQL text that {verb} a table whose name is built at run time");
            if !self.facts.unsupported.iter().any(|u| u.line == line && u.construct == construct) {
                self.facts.unsupported.push(Unsupported { area: "sql", construct, line });
            }
        }
    }

    /// Whether a string is passed to a call or command running SQL, or held in a member or variable named for SQL
    /// (`Some(true)`); passed to one running another query language (`Some(false)`: WMI's `ObjectQuery`,
    /// `WqlObjectQuery`, `ManagementObjectSearcher`, or a callee naming Influx); else `None`.
    fn sql_context(&self, node: Node) -> Option<bool> {
        let sqlish = |s: &str| {
            let l = s.to_ascii_lowercase();
            ["sql", "query", "cmd", "command", "statement", "exec", "fetch", "select", "insert", "update", "delete"].iter().any(|w| l.contains(w))
        };
        let mut n = node;
        while let Some(p) = n.parent() {
            match p.kind() {
                "parenthesized_expression" | "argument" | "binary_expression" | "conditional_expression" => n = p,
                "argument_list" => {
                    let call = p.parent()?;
                    let callee = match call.kind() {
                        "invocation_expression" => call.child_by_field_name("function").map(|f| match f.kind() {
                            "member_access_expression" => f.child_by_field_name("name").map_or_else(String::new, |m| name_text(self.src, m)),
                            _ => name_text(self.src, f),
                        }),
                        "object_creation_expression" => call.child_by_field_name("type").map(|t| base_type_name(self.src, t)),
                        _ => None,
                    }?;
                    if NO_SQL_CALLEES.contains(&callee.as_str()) || callee.to_ascii_lowercase().contains("influx") {
                        return Some(false);
                    }
                    // A builder named for SQL: `sqlBuilder.AppendLine("…")`.
                    let builder = || {
                        callee.starts_with("Append")
                            && call
                                .child_by_field_name("function")
                                .filter(|f| f.kind() == "member_access_expression")
                                .and_then(|f| f.child_by_field_name("expression"))
                                .is_some_and(|r| sqlish(self.text(r).rsplit('.').next().unwrap_or("")))
                    };
                    return (sqlish(&callee) || builder()).then_some(true);
                }
                "equals_value_clause" | "variable_declarator" => {
                    let decl = if p.kind() == "variable_declarator" { Some(p) } else { p.parent().filter(|d| d.kind() == "variable_declarator") };
                    return decl.and_then(|d| d.child_by_field_name("name")).is_some_and(|m| sqlish(self.text(m))).then_some(true);
                }
                "assignment_expression" => return p.child_by_field_name("left").is_some_and(|l| sqlish(self.text(l))).then_some(true),
                "arrow_expression_clause" => {
                    return p.parent().and_then(|d| d.child_by_field_name("name")).is_some_and(|m| sqlish(self.text(m))).then_some(true);
                }
                _ => return None,
            }
        }
        None
    }

    /// A call or `new` whose first argument runs a stored procedure: SQL text starting with `EXEC`, or with `CALL`
    /// (MySQL) when the call runs SQL (`ExecuteSqlRawAsync`, `QueryAsync`, `new MySqlCommand`), or a bare name when the
    /// call also passes `CommandType.StoredProcedure`.
    fn procedure_call(&mut self, node: Node) {
        if !self.procedure_named(node) {
            self.wrapped_procedure(node);
        }
    }

    /// A call of a method wrapping a procedure call (`ExecuteStoredProcedureAsync("save_x", parameters)`, `RunProc`,
    /// `Exec`, `CreateCommand`): one whose name contains `StoredProcedure` or `Proc`, starts with `Exec` or ends with
    /// `Command`. Each string argument that reads as an identifier is a `wrapped_procedure` fact: a candidate only,
    /// linked when a procedure of that name is defined somewhere.
    fn wrapped_procedure(&mut self, node: Node) {
        if node.kind() != "invocation_expression" {
            return;
        }
        let Some(f) = node.child_by_field_name("function") else { return };
        let callee = match f.kind() {
            "member_access_expression" => f.child_by_field_name("name").map_or_else(String::new, |n| name_text(self.src, n)),
            _ => name_text(self.src, f),
        };
        // `connection.CreateCommand("schema.proc")`: an extension creating a command, often defaulting to
        // `CommandType.StoredProcedure`.
        if !(callee.contains("StoredProcedure") || callee.contains("Proc") || callee.starts_with("Exec") || callee.ends_with("Command")) {
            return;
        }
        let Some(args) = node.child_by_field_name("arguments") else { return };
        for a in children(args).filter(|c| c.kind() == "argument") {
            let Some(v) = children(a).filter(|c| c.is_named()).last() else { continue };
            let Some(text) = string_value(self.src, v) else { continue };
            let name = text.trim().replace(['[', ']', '`'], "");
            if name.len() >= 3 && identifier_like(&name) {
                self.db(DbFact { kind: "wrapped_procedure", sym: self.current(), entity: String::new(), name, line: line(node) });
            }
        }
    }

    /// [`Self::procedure_call`] for a call naming its procedure itself; whether it did.
    fn procedure_named(&mut self, node: Node) -> bool {
        let Some(first) = node.child_by_field_name("arguments").and_then(first_argument) else { return false };
        let Some(text) = sql_text(self.src, first) else { return false };
        let runs_sql = || {
            let callee = match node.kind() {
                "invocation_expression" => node.child_by_field_name("function").map(|f| match f.kind() {
                    "member_access_expression" => f.child_by_field_name("name").map_or_else(String::new, |n| name_text(self.src, n)),
                    _ => name_text(self.src, f),
                }),
                _ => node.child_by_field_name("type").map(|t| base_type_name(self.src, t)),
            };
            let callee = callee.unwrap_or_default().to_ascii_lowercase();
            ["sql", "query", "exec", "command"].iter().any(|w| callee.contains(w))
        };
        let name = exec_procedure(&text).or_else(|| call_procedure(&text).filter(|_| runs_sql())).or_else(|| {
            let bare = text.trim().replace(['[', ']'], "");
            (identifier_like(&bare) && self.text(node).contains("CommandType.StoredProcedure")).then_some(bare)
        });
        match name {
            Some(name) => {
                self.db(DbFact { kind: "procedure", sym: self.current(), entity: String::new(), name, line: line(node) });
                true
            }
            None => false,
        }
    }

    /// `cmd.CommandText = "sp_x"` in a member that sets `CommandType.StoredProcedure`.
    fn command_text(&mut self, node: Node) {
        let (Some(l), Some(r)) = (node.child_by_field_name("left"), node.child_by_field_name("right")) else { return };
        if !self.text(l).ends_with("CommandText") {
            // `query = $"CALL p('{d}')";`, run later through the variable.
            if let Some(name) = sql_text(self.src, r).and_then(|t| call_procedure(&t)) {
                self.db(DbFact { kind: "procedure", sym: self.current(), entity: String::new(), name, line: line(node) });
            }
            return;
        }
        let Some(text) = string_value(self.src, r) else { return };
        let name = exec_procedure(&text).or_else(|| call_procedure(&text)).or_else(|| {
            let bare = text.trim().replace(['[', ']'], "");
            let member = self.scopes.iter().rev().find(|s| s.callable).map(|s| &self.facts.symbols[s.sym]);
            let in_proc =
                member.is_some_and(|m| self.src[m.start_byte as usize..m.end_byte as usize].contains("CommandType.StoredProcedure"));
            (identifier_like(&bare) && in_proc).then_some(bare)
        });
        if let Some(name) = name {
            self.db(DbFact { kind: "procedure", sym: self.current(), entity: String::new(), name, line: line(node) });
        }
    }

    fn add(&mut self, name: String, receiver: Option<String>, receiver_type: Option<String>, kind: RefKind, at: Node) {
        // A value of a constrained type parameter (`where T : BaseItem`) has the constraint's members.
        let receiver_type = receiver_type.map(|enc| match TypeExpr::decode(&enc) {
            Some(t) => t.replace_named(&|n: &str| if starts_upper(n) { constraint_type(self.src, at, n) } else { None }).encode(),
            None => enc,
        });
        let from = self.current();
        let (l, col) = (line(at), name_col(self.src, at, &name));
        if let std::collections::hash_map::Entry::Vacant(e) = self.seen.entry((from, name.clone(), kind, l, col)) {
            e.insert(self.facts.refs.len());
            if let Some(u) = self.usage.filter(|_| matches!(kind, RefKind::TypeRef | RefKind::Instantiate)) {
                self.facts.usages.insert(self.facts.refs.len(), u.to_string());
            }
            self.facts.refs.push(Ref { from, name, receiver, receiver_type, kind, line: l, col, args: None });
        }
    }

    /// [`Self::add`] with `usage` saying how the type references it records use the type (see
    /// [`FileFacts::usages`]).
    fn with_usage(&mut self, usage: &'static str, f: impl FnOnce(&mut Self)) {
        let outer = self.usage.replace(usage);
        f(self);
        self.usage = outer;
    }

    /// [`Self::add`] for a call passing `args` arguments. A call recorded twice (through two paths) whose records
    /// disagree on the arguments has an unknown argument count.
    fn add_call(
        &mut self,
        name: String,
        receiver: Option<String>,
        receiver_type: Option<String>,
        at: Node,
        args: Option<u32>,
    ) {
        self.add_passing(name, receiver, receiver_type, RefKind::Call, at, args);
    }

    /// A creation of the type `name` (`new T(…)`, `new T { … }`, a target-typed `new(…)`), with its arguments, which
    /// pick the constructor: none written is none passed.
    fn creation(&mut self, name: String, qualifier: Option<String>, args: Option<Node>, at: Node) {
        let count = args.map_or(0, |a| children(a).filter(|c| c.kind() == "argument").count() as u32);
        self.pending_arg_types = args.and_then(|a| self.argument_types(a));
        self.add_passing(name, qualifier, None, RefKind::Instantiate, at, Some(count));
        self.pending_arg_types = None;
    }

    /// A constructor chained to another (`: base(…)`, `: this(…)`, a primary constructor's base type `: B(x)`): a
    /// call of `init` on `receiver` (`base` or `this`), whose arguments pick the overload.
    fn chained(&mut self, receiver: &str, args: Option<Node>, at: Node) {
        let count = args.map_or(0, |a| children(a).filter(|c| c.kind() == "argument").count() as u32);
        self.pending_arg_types = args.and_then(|a| self.argument_types(a));
        self.add_passing("init".to_string(), Some(receiver.to_string()), None, RefKind::Call, at, Some(count));
        self.pending_arg_types = None;
    }

    /// The type a target-typed `new(…)` creates, from where it goes: a declaration's type (a local, field or
    /// property), the return type of the method it is returned from (`T` for an `async` method's `Task<T>`), or a
    /// local or parameter it is assigned to. `None` elsewhere (an argument: the overload decides the type).
    fn target_type(&self, node: Node) -> Option<(String, Option<String>)> {
        let named = |t: Node| -> Option<(String, Option<String>)> {
            (!matches!(t.kind(), "implicit_type" | "predefined_type")).then(|| (base_type_name(self.src, t), type_qualifier(self.src, t)))
        };
        let p = node.parent()?;
        match p.kind() {
            "variable_declarator" => {
                let decl = p.parent().filter(|d| d.kind() == "variable_declaration")?;
                named(decl.child_by_field_name("type")?)
            }
            "property_declaration" => named(p.child_by_field_name("type")?),
            "return_statement" | "arrow_expression_clause" => {
                let mut cur = p.parent();
                while let Some(n) = cur {
                    match n.kind() {
                        "method_declaration" | "local_function_statement" | "property_declaration" => {
                            let t = n.child_by_field_name("returns").or_else(|| n.child_by_field_name("type"))?;
                            // `async Task<Order> M() { return new(…); }` returns an `Order`.
                            if has_modifier(self.src, n, "async") {
                                let g = unqualified(t);
                                if g.kind() == "generic_name" && matches!(name_text(self.src, g).as_str(), "Task" | "ValueTask") {
                                    return named(children(g).filter(|c| c.kind() == "type_argument_list").flat_map(children).find(|c| c.is_named())?);
                                }
                                return None;
                            }
                            return named(t);
                        }
                        "lambda_expression" | "anonymous_method_expression" | "constructor_declaration" | "accessor_declaration" => return None,
                        _ => cur = n.parent(),
                    }
                }
                None
            }
            "assignment_expression" if p.child_by_field_name("right") == Some(node) => {
                let left = p.child_by_field_name("left").filter(|l| l.kind() == "identifier")?;
                match self.local(self.text(left))? {
                    TypeExpr::Named(n) | TypeExpr::Generic { name: n, .. } => {
                        let (q, last) = match n.rsplit_once('.') {
                            Some((q, l)) => (Some(q.to_string()), l.to_string()),
                            None => (None, n.clone()),
                        };
                        Some((last, q))
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// [`Self::add_call`] for a reference of `kind`: a call, or a creation (`new T(…)`) whose arguments pick the
    /// constructor.
    fn add_passing(
        &mut self,
        name: String,
        receiver: Option<String>,
        receiver_type: Option<String>,
        kind: RefKind,
        at: Node,
        args: Option<u32>,
    ) {
        let key = (self.current(), name.clone(), kind, line(at), name_col(self.src, at, &name));
        let arg_types = self.pending_arg_types.clone();
        match self.seen.get(&key) {
            Some(&i) => {
                if self.facts.refs[i].args != args {
                    self.facts.refs[i].args = None;
                }
                // Records of one call disagreeing on its arguments' types: types unknown.
                if self.facts.arg_types.get(&i) != arg_types.as_ref() {
                    self.facts.arg_types.remove(&i);
                }
            }
            None => {
                self.add(name, receiver, receiver_type, kind, at);
                if let Some(r) = self.facts.refs.last_mut() {
                    r.args = args;
                }
                if let Some(t) = arg_types {
                    self.facts.arg_types.insert(self.facts.refs.len() - 1, t);
                }
            }
        }
    }
}

/// Chain links that start a dependency-injection registration (see [`Cx::di_root`]).
const DI_ROOTS: &[&str] = &[
    "RegisterType", "RegisterGeneric", "Register", "RegisterInstance", "RegisterAssemblyTypes", "RegisterGeneratedFactory",
    "AddDbContext", "AddDbContextPool", "AddDbContextFactory", "AddPooledDbContextFactory", "AddScoped",
    "AddTransient", "AddSingleton", "TryAddScoped", "TryAddTransient", "TryAddSingleton", "AddHttpClient",
    // Keyed services: recognised, not read (see `di_unread`).
    "AddKeyedScoped", "AddKeyedTransient", "AddKeyedSingleton", "TryAddKeyedScoped", "TryAddKeyedTransient", "TryAddKeyedSingleton",
];

/// `EP.X` with `using EP = A.B;` among `aliases` → `A.B.X`.
fn expand_with(aliases: &HashMap<String, String>, name: String) -> String {
    match name.split_once('.') {
        Some((head, rest)) => match aliases.get(head) {
            Some(target) => format!("{target}.{rest}"),
            None => name,
        },
        None => match aliases.get(&name) {
            Some(target) => target.clone(),
            None => name,
        },
    }
}

/// Whether `ident` names the parameter of a lambda passed to a Moq setup (`mock.Setup(f => f(1))`): the mocked
/// delegate's invocation there is recorded, not made.
fn mocked_in_setup(src: &str, ident: Node) -> bool {
    let name = &src[ident.byte_range()];
    let mut n = ident.parent();
    while let Some(cur) = n {
        if cur.kind() == "lambda_expression" && lambda_param(cur).is_some_and(|p| &src[p.byte_range()] == name) {
            let call = cur.parent().and_then(|a| a.parent()).and_then(|l| l.parent()).filter(|c| c.kind() == "invocation_expression");
            let method = call.and_then(|c| c.child_by_field_name("function")).filter(|f| f.kind() == "member_access_expression");
            return method.and_then(|m| m.child_by_field_name("name")).is_some_and(|m| MOCK_LAMBDAS.contains(&&src[m.byte_range()]));
        }
        n = cur.parent();
    }
    false
}

/// The parameter an expression passes on as is: `x`, `(x)`, `x!`, `x ?? throw …`.
fn given<'s>(src: &'s str, e: Node) -> Option<&'s str> {
    match e.kind() {
        "identifier" => Some(&src[e.byte_range()]),
        "parenthesized_expression" | "postfix_unary_expression" => children(e).find(|c| c.is_named()).and_then(|c| given(src, c)),
        "binary_expression" if e.child_by_field_name("operator").is_some_and(|o| &src[o.byte_range()] == "??") => {
            given(src, e.child_by_field_name("left")?)
        }
        _ => None,
    }
}

/// The members a constructor's body assigns one of `params` as is (`_make = make;`, `this.make = make ?? throw …`),
/// outside nested lambdas and local functions.
fn assigned_params(src: &str, ctor: Node, params: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    let Some(body) = ctor.child_by_field_name("body") else { return out };
    let mut stack = vec![body];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "lambda_expression" | "anonymous_method_expression" | "local_function_statement" => continue,
            "assignment_expression" => {
                let left = n.child_by_field_name("left");
                let member = match left.map(|l| (l.kind(), l)) {
                    Some(("identifier", l)) => Some(&src[l.byte_range()]),
                    Some(("member_access_expression", l))
                        if l.child_by_field_name("expression").is_some_and(|e| matches!(e.kind(), "this_expression" | "this")) =>
                    {
                        l.child_by_field_name("name").map(|m| &src[m.byte_range()])
                    }
                    _ => None,
                };
                if let (Some(m), Some(r)) = (member, n.child_by_field_name("right").and_then(|r| given(src, r)))
                    && params.contains(&r)
                    && !params.contains(&m)
                {
                    out.push(m.to_string());
                }
            }
            _ => {}
        }
        stack.extend(children(n));
    }
    out
}

/// An empty value: `null`, `default`, `[]`, `new List<T>()` (a collection type, no arguments or elements),
/// `new T[0]`, `new T[] { }`, `Array.Empty<T>()`, `Enumerable.Empty<T>()`.
fn empty_value(src: &str, n: Node) -> bool {
    const COLLECTIONS: &[&str] = &["List", "HashSet", "Collection", "ObservableCollection", "SortedSet", "LinkedList", "Queue", "Stack"];
    let text = |n: Node| &src[n.byte_range()];
    match n.kind() {
        "null_literal" | "default_expression" => true,
        "literal" => text(n) == "null",
        "collection_expression" => !children(n).any(|c| c.is_named()),
        "object_creation_expression" => {
            let no_args = n.child_by_field_name("arguments").is_none_or(|a| !children(a).any(|c| c.is_named()));
            let no_items = n.child_by_field_name("initializer").is_none_or(|i| !children(i).any(|c| c.is_named()));
            let ty = n.child_by_field_name("type").map(text).unwrap_or("");
            let base = ty.split('<').next().unwrap_or("").rsplit('.').next().unwrap_or("").trim();
            no_args && no_items && COLLECTIONS.contains(&base)
        }
        "array_creation_expression" => {
            let t: String = text(n).chars().filter(|c| !c.is_whitespace()).collect();
            t.ends_with("[0]") || t.ends_with("[]{}")
        }
        "invocation_expression" => {
            let t: String = text(n).chars().filter(|c| !c.is_whitespace()).collect();
            (t.starts_with("Array.Empty<") || t.starts_with("Enumerable.Empty<")) && t.ends_with(">()")
        }
        _ => false,
    }
}

/// The names a member (method, constructor, property, field…) binds itself: parameters (of it, a lambda or a local
/// function), locals, `foreach`, `using`, `out var` and pattern variables, catch variables, query range variables and
/// local functions. Such a name hides a field of the same name.
fn bound_names(src: &str, member: Node) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    let mut stack = vec![member];
    while let Some(n) = stack.pop() {
        let declared = match n.kind() {
            "parameter" | "variable_declarator" | "catch_declaration" | "local_function_statement" | "from_clause"
            | "let_clause" | "join_clause" | "declaration_expression" | "declaration_pattern" => n.child_by_field_name("name"),
            "single_variable_designation" => Some(n),
            "foreach_statement" => n.child_by_field_name("left").filter(|l| l.kind() == "identifier"),
            // `x => …`: an implicitly typed lambda's one parameter.
            "lambda_expression" => n.child_by_field_name("parameters").filter(|p| p.kind() == "identifier"),
            _ => None,
        };
        if let Some(d) = declared {
            out.insert(src[d.byte_range()].to_string());
        }
        stack.extend(children(n));
    }
    out
}

/// For `x?.M` (a `conditional_access_expression` binding a member): `x`, and `M`'s name node.
/// A member call's function, `x.M` or `x?.M`: the receiver and the name.
fn member_call(f: Node) -> Option<(Node, Node)> {
    match f.kind() {
        "member_access_expression" => Some((f.child_by_field_name("expression")?, f.child_by_field_name("name")?)),
        "conditional_access_expression" => conditional_member(f),
        _ => None,
    }
}

fn conditional_member(n: Node) -> Option<(Node, Node)> {
    // tree-sitter-c-sharp binds `?.` loosely: in `a != null && b?.C` the condition is the whole `a != null && b`. The
    // value accessed is the operand nearest the `?.`.
    let mut inner = unbang(n.child_by_field_name("condition")?);
    loop {
        let next = match inner.kind() {
            "binary_expression" | "assignment_expression" => inner.child_by_field_name("right"),
            "conditional_expression" => inner.child_by_field_name("alternative"),
            // `await this?.M()`: `this` is an unnamed token there.
            "prefix_unary_expression" | "await_expression" => children(inner).filter(|c| c.is_named() || c.kind() == "this").last(),
            _ => None,
        };
        match next {
            Some(x) => inner = unbang(x),
            None => break,
        }
    }
    let binding = children(n).find(|c| c.kind() == "member_binding_expression")?;
    Some((inner, binding.child_by_field_name("name")?))
}

/// `x` for `x!` (the null-forgiving operator, which changes nothing about `x`'s type or members); any other node as is.
fn unbang(n: Node) -> Node {
    if n.kind() == "postfix_unary_expression"
        && let Some(last) = n.child(n.child_count().saturating_sub(1))
        && last.kind() == "!"
        && let Some(inner) = n.named_child(0)
    {
        return inner;
    }
    n
}

/// Namespaces that never hold a code base's messages: left out of the scopes a message name resolves in.
const FRAMEWORK_NAMESPACES: &[&str] = &["System", "Microsoft", "MassTransit", "Newtonsoft", "Autofac", "AutoMapper", "Moq", "Xunit", "NUnit", "FluentAssertions", "Serilog", "Polly"];

/// `A.B.Foo<T>` → `A.B.Foo`; `Foo?` → `Foo`; `global::A.Foo` → `A.Foo`.
fn dotted_type(src: &str, t: Node) -> String {
    match t.kind() {
        "identifier" | "generic_name" => name_text(src, t),
        "qualified_name" => {
            let q = t.child_by_field_name("qualifier").map(|q| dotted_type(src, q)).unwrap_or_default();
            let n = t.child_by_field_name("name").map(|n| dotted_type(src, n)).unwrap_or_default();
            if q.is_empty() { n } else { format!("{q}.{n}") }
        }
        "alias_qualified_name" => t.child_by_field_name("name").map(|n| dotted_type(src, n)).unwrap_or_default(),
        "nullable_type" => t.child_by_field_name("type").map(|n| dotted_type(src, n)).unwrap_or_default(),
        _ => String::new(),
    }
}

/// The type parameters a declaration introduces (`class C<TMessage>`, `void M<T>()`).
fn type_params_of(src: &str, decl: Node) -> Vec<String> {
    children(decl)
        .filter(|c| c.kind() == "type_parameter_list")
        .flat_map(children)
        .filter(|p| p.kind() == "type_parameter")
        .filter_map(|p| p.child_by_field_name("name").or_else(|| children(p).find(|c| c.kind() == "identifier")))
        .map(|n| src[n.byte_range()].to_string())
        .collect()
}

/// What a value of type parameter `name`, in scope at `node`, has its members looked up on: the one type its nearest
/// declaration constrains it to (`where T : BaseItem, new()` → `BaseItem`). `None` when `name` isn't a type parameter
/// there, or it has no type constraint, or several (the member may be any one's), or one naming another type parameter.
fn constraint_type(src: &str, node: Node, name: &str) -> Option<TypeExpr> {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if type_params_of(src, n).iter().any(|p| p == name) {
            let types: Vec<Node> = children(n)
                .filter(|c| c.kind() == "type_parameter_constraints_clause")
                .filter(|c| children(*c).find(|t| t.kind() == "identifier").is_some_and(|t| &src[t.byte_range()] == name))
                .flat_map(children)
                .filter(|c| c.kind() == "type_parameter_constraint")
                .filter_map(|c| c.child_by_field_name("type"))
                // `where T : notnull`, `unmanaged`, `default`: keywords the grammar reads as type names.
                .filter(|t| !matches!(&src[t.byte_range()], "notnull" | "unmanaged" | "default"))
                .collect();
            let [t] = types.as_slice() else { return None };
            let around = type_params_around(src, n);
            return type_expr(src, *t).filter(|e| !matches!(e, TypeExpr::Named(c) if around.contains(c) || type_params_of(src, n).contains(c)));
        }
        cur = n.parent();
    }
    None
}

/// `A.B.Name<X>` → `Name<X>`; any other type node as it is.
fn unqualified(n: Node) -> Node {
    if n.kind() == "qualified_name" { n.child_by_field_name("name").unwrap_or(n) } else { n }
}

/// The method, local function or constructor whose body holds `node` (inside a lambda, too).
fn enclosing_method(node: Node) -> Option<Node> {
    let mut cur = node.parent();
    while let Some(n) = cur {
        match n.kind() {
            "method_declaration" | "local_function_statement" | "constructor_declaration" => return Some(n),
            "class_declaration" | "struct_declaration" | "record_declaration" | "interface_declaration" => return None,
            _ => cur = n.parent(),
        }
    }
    None
}

/// A method's parameters as written: name and type. An extension method's `this` parameter is kept.
fn param_types(src: &str, decl: Node) -> Vec<(String, String)> {
    let Some(list) = decl.child_by_field_name("parameters") else { return Vec::new() };
    children(list)
        .filter(|c| c.kind() == "parameter")
        .map(|p| {
            let text = |f: &str| p.child_by_field_name(f).map(|n| src[n.byte_range()].trim().to_string()).unwrap_or_default();
            (text("name"), text("type"))
        })
        .collect()
}

/// Type parameters in scope at `node`: those of every enclosing method and type.
fn type_params_around(src: &str, node: Node) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = node.parent();
    while let Some(n) = cur {
        out.extend(type_params_of(src, n));
        cur = n.parent();
    }
    out
}

/// A registration being read from its chain.
struct PendingDi {
    kind: &'static str,
    implementation: Option<String>,
    services: Vec<Option<String>>,
    /// The service when no `As` follows: `Register<IFoo>(…)`; else the implementation itself.
    default_service: Option<String>,
    lifetime: Option<&'static str>,
    line: u32,
    /// What a factory makes, encoded, when only the repo's declarations tell (see `DiBinding::made`).
    made: Option<String>,
    /// How it is added (see `DiBinding::mode`).
    mode: Option<&'static str>,
}

/// One call of a fluent chain: `.Name<TypeArgs>(args)`.
struct Link<'t> {
    name: String,
    type_args: Vec<Node<'t>>,
    args: Option<Node<'t>>,
    /// The method name, for the line.
    at: Node<'t>,
}

/// The calls of the chain ending at `call`, innermost (first called) first.
fn chain_links<'t>(src: &str, call: Node<'t>) -> Vec<Link<'t>> {
    let mut out = Vec::new();
    let mut cur = call;
    while cur.kind() == "invocation_expression" {
        let Some(f) = cur.child_by_field_name("function") else { break };
        let (name, next) = match f.kind() {
            "member_access_expression" => (f.child_by_field_name("name"), f.child_by_field_name("expression")),
            "identifier" | "generic_name" => (Some(f), None),
            _ => (None, None),
        };
        let Some(name) = name else { break };
        let type_args = if name.kind() == "generic_name" {
            children(name).filter(|c| c.kind() == "type_argument_list").flat_map(children).filter(|c| c.is_named()).collect()
        } else {
            Vec::new()
        };
        out.push(Link { name: name_text(src, name), type_args, args: cur.child_by_field_name("arguments"), at: name });
        match next {
            Some(n) => cur = n,
            None => break,
        }
    }
    out.reverse();
    out
}

/// Whether `call` is the receiver of a further call in its chain (`a.B()` in `a.B().C()`), so the outer call reads
/// the whole chain.
fn chain_inner(call: Node) -> bool {
    call.parent().is_some_and(|p| {
        p.kind() == "member_access_expression"
            && p.child_by_field_name("expression") == Some(call)
            && p.parent().is_some_and(|g| g.kind() == "invocation_expression" && g.child_by_field_name("function") == Some(p))
    })
}

/// Each argument's expression in an `argument_list`.
fn argument_exprs(list: Node) -> Vec<Node> {
    children(list).filter(|c| c.kind() == "argument").filter_map(|a| children(a).filter(|c| c.is_named()).last()).collect()
}

/// Microsoft DI's description of a registration (see `service_descriptor`).
const SERVICE_DESCRIPTOR: &str = "ServiceDescriptor";

/// The type of a container's registration built from its parts (see `component_registration`).
const COMPONENT_REGISTRATION: &str = "ComponentRegistration";

/// `typeof(Foo)` → the `Foo` type node.
fn typeof_type(e: Node) -> Option<Node> {
    if e.kind() == "typeof_expression" { e.child_by_field_name("type") } else { None }
}

/// A lifetime named by a `Scope` member (`Scoped(Scope.SingleInstance)`), or by one configured (`scope_lifetimes`).
fn scope_lifetime(name: &str, di: &crate::conventions::Di) -> Option<&'static str> {
    Some(match name {
        "SingleInstance" | "Singleton" => "singleton",
        "Transient" | "InstancePerDependency" => "transient",
        "Scoped" | "InstancePerLifetimeScope" | "Request" => "scoped",
        _ => return di.scope_lifetimes.iter().find(|(n, _)| n == name).map(|(_, l)| *l),
    })
}

/// HTTP client methods that take a URL first, and the verb each sends.
const CLIENT_METHODS: &[(&str, &str)] = &[
    ("DeleteAsync", "DELETE"), ("DeleteFromJsonAsync", "DELETE"), ("GetAsync", "GET"), ("GetByteArrayAsync", "GET"),
    ("GetFromJsonAsync", "GET"), ("GetStreamAsync", "GET"), ("GetStringAsync", "GET"), ("PatchAsJsonAsync", "PATCH"),
    ("PatchAsync", "PATCH"), ("PostAsJsonAsync", "POST"), ("PostAsync", "POST"), ("PutAsJsonAsync", "PUT"),
    ("PutAsync", "PUT"),
];

/// Requests a method makes by naming them in code, rather than with a request DTO.
impl Cx<'_> {
    /// A configured gateway call (`gateway_calls`), `gateway.CallService("BillingApi", $"orders/{id}", HttpMethod.Get)`
    /// or `SendToApi(HttpMethod.Post, service, path, dto)`: a gateway call naming the service and path. `http.GetAsync($"{baseUrl}/orders")`: an HTTP
    /// client call with a URL, on a receiver named for HTTP or a client. Both are client routes of the enclosing method.
    fn outgoing_request(&mut self, node: Node) {
        let Some(f) = node.child_by_field_name("function").filter(|f| f.kind() == "member_access_expression") else { return };
        let Some(method) = f.child_by_field_name("name").map(|n| name_text(self.src, n)) else { return };
        let args: Vec<Node> = node.child_by_field_name("arguments").map(argument_exprs).unwrap_or_default();
        let gateway = self.conv.csharp_http.gateway_calls.iter().find(|c| c.name == method).and_then(|c| {
            use crate::conventions::Arg;
            let at = |role| c.position(role).and_then(|i| args.get(i)).copied();
            Some((at(Arg::Service)?, at(Arg::Path)?, at(Arg::Verb)?))
        });
        if let Some((service, path, verb)) = gateway {
            let Some(verb) = self.http_verb(verb) else { return };
            // A path in a local or parameter can't be known.
            if path.kind() == "identifier" && !starts_upper(self.text(path)) {
                return;
            }
            let Some(path) = urls::encode(&self.url_pieces(path), false) else { return };
            let (service, service_ref) = match (self.const_value(service), self.constant_name(service)) {
                // `CallService("https://host", path, verb)` names an address, not a service.
                (Some(name), _) if name.contains("://") => (None, None),
                (Some(name), _) => (Some(name), None),
                (None, r) => (None, r),
            };
            return self.request_route(verb, path, service, service_ref, node);
        }
        let Some((_, verb)) = CLIENT_METHODS.iter().find(|(m, _)| *m == method) else { return };
        let receiver = f.child_by_field_name("expression").map(|e| self.text(e).to_ascii_lowercase()).unwrap_or_default();
        if !receiver.contains("http") && !receiver.contains("client") {
            return;
        }
        if let Some(url) = args.first() {
            self.client_url(verb.to_string(), *url, node);
        }
    }

    /// `new HttpRequestMessage(HttpMethod.Post, $"{base}/orders")`; RestSharp's `new RestRequest("orders/{id}",
    /// Method.Get)` (a GET without a method).
    fn request_message(&mut self, node: Node) {
        let ty = node.child_by_field_name("type").map(|t| base_type_name(self.src, t)).unwrap_or_default();
        let args: Vec<Node> = node.child_by_field_name("arguments").map(argument_exprs).unwrap_or_default();
        if ty == "RestRequest" {
            let verb = match args.get(1) {
                Some(m) if m.kind() == "member_access_expression" => m.child_by_field_name("name").and_then(|n| urls::verb(self.text(n))),
                Some(_) => None,
                None => Some("GET".to_string()),
            };
            if let (Some(url), Some(verb)) = (args.first(), verb) {
                self.client_url(verb, *url, node);
            }
            return;
        }
        if ty != "HttpRequestMessage" {
            return;
        }
        if let [verb, url, ..] = args.as_slice()
            && let Some(verb) = self.http_verb(*verb)
        {
            self.client_url(verb, *url, node);
        }
    }

    fn client_url(&mut self, verb: String, url: Node, at: Node) {
        // `var uri = $"{base}items/{id}"; http.GetAsync(uri)`: the URL the local holds.
        let url = match url.kind() {
            "identifier" if self.constant_name(url).is_none() => local_value(self.src, at, self.text(url)).unwrap_or(url),
            _ => url,
        };
        match urls::encode(&self.url_pieces(url), true) {
            Some(path) if urls::looks_like_path(&path) => self.request_route(verb, path, None, None, at),
            // A URL known only at run time: kept with no path, so an answer can say the request wasn't read.
            None => self.request_route(verb, String::new(), None, None, at),
            Some(_) => {}
        }
    }

    fn request_route(&mut self, verb: String, path: String, service: Option<String>, service_ref: Option<String>, at: Node) {
        let Some(sym) = self.current() else { return };
        self.facts.http_routes.push(HttpRoute { sym: Some(sym), side: "client", verb, path, path_ref: None, service, service_ref, line: line(at) });
    }

    /// `HttpMethod.Get`, `HttpMethods.Post`, `new HttpMethod("PATCH")`, `"GET"`.
    fn http_verb(&self, n: Node) -> Option<String> {
        match n.kind() {
            "member_access_expression" => {
                let owner = self.text(n.child_by_field_name("expression")?);
                if !owner.ends_with("HttpMethod") && !owner.ends_with("HttpMethods") {
                    return None;
                }
                urls::verb(self.text(n.child_by_field_name("name")?))
            }
            "object_creation_expression" => {
                let arg = n.child_by_field_name("arguments").and_then(first_argument)?;
                urls::verb(&string_value(self.src, arg)?)
            }
            _ => urls::verb(&string_value(self.src, n)?),
        }
    }

    /// A URL expression as pieces: literal text, `nameof(X)` as text, constants (`ServiceNames.Path`, a PascalCase
    /// name), and anything else unknown. Interpolations and `+` concatenations are walked.
    fn url_pieces(&self, e: Node) -> Vec<Piece> {
        let mut out = Vec::new();
        self.push_pieces(e, &mut out);
        out
    }

    fn push_pieces(&self, e: Node, out: &mut Vec<Piece>) {
        if let Some(s) = string_value(self.src, e) {
            return out.push(Piece::Text(s));
        }
        match e.kind() {
            "interpolated_string_expression" => {
                for c in children(e) {
                    match c.kind() {
                        "string_content" | "escape_sequence" => out.push(Piece::Text(self.text(c).to_string())),
                        "interpolation" => match children(c).find(|x| x.is_named() && !x.kind().starts_with("interpolation_")) {
                            Some(x) => self.push_pieces(x, out),
                            None => out.push(Piece::Unknown),
                        },
                        _ => {}
                    }
                }
            }
            "binary_expression" if e.child_by_field_name("operator").is_some_and(|o| self.text(o) == "+") => {
                for side in ["left", "right"] {
                    match e.child_by_field_name(side) {
                        Some(x) => self.push_pieces(x, out),
                        None => out.push(Piece::Unknown),
                    }
                }
            }
            "parenthesized_expression" => match children(e).find(|c| c.is_named()) {
                Some(x) => self.push_pieces(x, out),
                None => out.push(Piece::Unknown),
            },
            "invocation_expression" => match self.route_template(e) {
                Some((name, None)) => out.push(Piece::Text(name)),
                _ => out.push(Piece::Call),
            },
            // `new Uri(baseUri, "api/orders")`: a base address and a path relative to it; `new Uri(url)`: the URL.
            "object_creation_expression" if e.child_by_field_name("type").is_some_and(|t| base_type_name(self.src, t) == "Uri") => {
                match e.child_by_field_name("arguments").map(argument_exprs).unwrap_or_default().as_slice() {
                    [url] => self.push_pieces(*url, out),
                    [_, kind] if self.text(*kind).starts_with("UriKind") => out.push(Piece::Unknown),
                    [base, rel, ..] => {
                        self.push_pieces(*base, out);
                        out.push(Piece::Text("/".to_string()));
                        self.push_pieces(*rel, out);
                    }
                    _ => out.push(Piece::Unknown),
                }
            }
            _ => out.push(self.constant_name(e).or_else(|| self.string_field(e)).map_or(Piece::Unknown, Piece::Ref)),
        }
    }

    /// `baseUrl` or `this.baseUrl`, a `readonly string` field of the enclosing type holding a literal: its constant.
    fn string_field(&self, e: Node) -> Option<String> {
        let name = match e.kind() {
            "identifier" if self.local(self.text(e)).is_none() => self.text(e),
            "member_access_expression" if e.child_by_field_name("expression").is_some_and(|x| matches!(x.kind(), "this_expression" | "this")) => {
                self.text(e.child_by_field_name("name")?)
            }
            _ => return None,
        };
        let owner = self.enclosing_type()?;
        self.string_fields.iter().any(|(t, n)| *t == owner && n == name).then(|| format!("{owner}.{name}"))
    }

    /// A name that may be a string constant: `PaymentApi`, `ServiceNames.Path`. Locals, parameters and members of
    /// them (`call.Operation`) are camelCase in C#, and never are.
    fn constant_name(&self, e: Node) -> Option<String> {
        let t = self.text(e);
        let dotted = t.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.');
        (matches!(e.kind(), "identifier" | "member_access_expression") && dotted && starts_upper(t)).then(|| t.to_string())
    }

    /// The value of a string literal or `nameof(X)`.
    fn const_value(&self, e: Node) -> Option<String> {
        string_value(self.src, e).or_else(|| match self.route_template(e) {
            Some((v, None)) if e.kind() == "invocation_expression" && !v.is_empty() => Some(v),
            _ => None,
        })
    }
}

/// A conventional route template with the defaults an anonymous object gives (`new { action = "Index" }`) written
/// into its parameters as inline defaults (`{action=Index}`); a parameter with an inline default, or a value that
/// isn't a string literal, is left as it is.
fn with_defaults(src: &str, template: &str, defaults: Node) -> String {
    if defaults.kind() != "anonymous_object_creation_expression" {
        return template.to_string();
    }
    let mut values: Vec<(String, String)> = Vec::new();
    let mut name: Option<String> = None;
    // `identifier = expression` pairs, flat among the braces and commas.
    for c in children(defaults).filter(|c| c.is_named()) {
        if c.kind() == "identifier" && c.next_sibling().is_some_and(|n| n.kind() == "=") {
            name = Some(src[c.byte_range()].to_string());
            continue;
        }
        if let (Some(n), Some(v)) = (name.take(), string_value(src, c)) {
            values.push((n, v));
        }
    }
    template
        .split('/')
        .map(|seg| {
            let Some(inner) = seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) else { return seg.to_string() };
            let n = inner.split([':', '?']).next().unwrap_or_default();
            match values.iter().find(|(k, _)| k.eq_ignore_ascii_case(n)) {
                Some((_, v)) if !inner.contains('=') && !inner.starts_with('*') => {
                    let rest = &inner[n.len()..];
                    format!("{{{n}={v}{}}}", rest.trim_end_matches('?'))
                }
                _ => seg.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Minimal API registrations and the HTTP verb each serves (`MapMethods` names its own, `Map` serves every verb).
const MAP_VERBS: &[(&str, &str)] =
    &[("MapGet", "GET"), ("MapPost", "POST"), ("MapPut", "PUT"), ("MapDelete", "DELETE"), ("MapPatch", "PATCH"), ("MapMethods", ""), ("Map", "*")];

/// Conventional routing: routes a template makes from controller and action names. A literal template naming the
/// controller is a `conventional` fact, filled at query time; any other is `unsupported`.
const CONVENTIONAL_ROUTES: &[&str] = &[
    "MapControllerRoute", "MapDefaultControllerRoute", "MapAreaControllerRoute", "MapRoute", "MapHttpRoute", "MapFallbackToController",
    "MapFallbackToAreaController",
];

/// Minimal API endpoints (ASP.NET Core endpoint routing), request contracts declared by an attribute, method groups
/// passed as delegates, and the hosts code names.
impl Cx<'_> {
    /// `api.MapGet("/items/{id:int}", GetItemById)` under `var api = app.MapGroup("api/catalog")`: a server route of
    /// the handler (a method group) or of the method registering it (a lambda). A registration whose route can't be
    /// known (a template or group prefix that isn't a constant, a middleware branch, conventional routes) is an
    /// `unsupported` fact saying so; a helper called with a group records the group (`group`).
    fn endpoint(&mut self, call: Node) {
        let Some(f) = call.child_by_field_name("function") else { return };
        let (recv, n) = match f.kind() {
            "member_access_expression" => (f.child_by_field_name("expression"), f.child_by_field_name("name")),
            "identifier" => (None, Some(f)),
            _ => return,
        };
        let Some(n) = n else { return };
        let args = call_args(self.src, call);
        if f.kind() == "member_access_expression" && n.kind() == "identifier" {
            let method = self.text(n);
            if let Some((_, verb)) = MAP_VERBS.iter().find(|(m, _)| *m == method) {
                return self.map_endpoint(n, method, verb, recv, &args);
            }
            if CONVENTIONAL_ROUTES.contains(&method) {
                // `MapControllerRoute("default", "{controller=Home}/{action=Index}/{id?}")`: a template the query fills
                // with each conventionally routed action's controller and name.
                let named = |k: &str| args.iter().find(|(n, _)| n.as_deref() == Some(k)).map(|(_, e)| *e);
                let positional: Vec<Node> = args.iter().filter(|(n, _)| n.is_none()).map(|(_, e)| *e).collect();
                let template = match method {
                    "MapDefaultControllerRoute" => Some("{controller=Home}/{action=Index}/{id?}".to_string()),
                    "MapControllerRoute" | "MapRoute" | "MapHttpRoute" => ["pattern", "template", "url", "routeTemplate"]
                        .iter()
                        .find_map(|k| named(k))
                        .or_else(|| positional.get(1).copied())
                        .and_then(|t| string_value(self.src, t)),
                    _ => None,
                };
                // `defaults: new { controller = "Home", action = "Index" }`: written into the template, as inline ones.
                let defaults = match method {
                    "MapControllerRoute" | "MapRoute" | "MapHttpRoute" => named("defaults").or_else(|| positional.get(2).copied()),
                    _ => None,
                };
                let template = template.map(|t| match defaults {
                    Some(d) => with_defaults(self.src, &t, d),
                    None => t,
                });
                if let Some(t) = template.filter(|t| t.contains("{controller") && !t.contains("{area")) {
                    self.facts.http_routes.push(HttpRoute {
                        sym: self.current(),
                        side: "conventional",
                        verb: method.to_string(),
                        path: t.trim_matches('/').to_string(),
                        path_ref: None,
                        service: None,
                        service_ref: None,
                        line: line(n),
                    });
                    return;
                }
                let why = "conventional routes: a template filled from controller and action names, which KnowCode doesn't expand";
                self.facts.http_routes.push(HttpRoute {
                    sym: self.current(),
                    side: "unsupported",
                    verb: method.to_string(),
                    path: why.to_string(),
                    path_ref: None,
                    service: None,
                    service_ref: None,
                    line: line(n),
                });
                return;
            }
        }
        self.group_helper_call(recv, n, &args);
    }

    fn map_endpoint(&mut self, at: Node, method: &str, verb: &str, recv: Option<Node>, args: &[(Option<String>, Node)]) {
        let positional: Vec<Node> = args.iter().filter(|(n, _)| n.is_none()).map(|(_, e)| *e).collect();
        let named = |k: &str| args.iter().find(|(n, _)| n.as_deref() == Some(k)).map(|(_, e)| *e);
        let template = named("pattern").or_else(|| positional.first().copied());
        let handler = named("handler").or_else(|| named("requestDelegate")).or_else(|| positional.last().copied().filter(|h| Some(*h) != template));
        let (Some(template), Some(handler)) = (template, handler) else { return };
        let base = self.route_base(recv);
        // `mapper.Map(source, destination)`: `Map` maps an endpoint only with a template, or on a route group.
        if method == "Map" && !self.stringy(template) && !recv.is_some_and(|r| self.holds_group(r)) {
            return;
        }
        if method == "Map" && self.pipeline_branch(recv, handler) {
            return self.endpoint_fact("unsupported", "Map".to_string(), "a middleware branch: what it serves under its prefix isn't read".to_string(), handler, at);
        }
        let verbs: Vec<String> = if method == "MapMethods" {
            match named("httpMethods").or_else(|| positional.get(1).copied()).and_then(|v| self.verb_list(v)) {
                Some(v) => v,
                None => return self.endpoint_fact("unsupported", method.to_string(), "its HTTP methods are not constants".to_string(), handler, at),
            }
        } else {
            vec![verb.to_string()]
        };
        let path = match (base, self.route_text(template)) {
            (Err(why), _) => return self.endpoint_fact("unsupported", method.to_string(), why.to_string(), handler, at),
            (_, None) => {
                return self.endpoint_fact("unsupported", method.to_string(), "its route template is not a constant".to_string(), handler, at);
            }
            (Ok(b), Some(t)) => join_route(&b, &t),
        };
        for v in verbs {
            self.endpoint_fact("server", v, path.clone(), handler, at);
        }
    }

    /// Records an endpoint fact, anchored to its handler once the file is read when the handler is a method group.
    fn endpoint_fact(&mut self, side: &'static str, verb: String, path: String, handler: Node, at: Node) {
        let route = self.facts.http_routes.len();
        self.facts.http_routes.push(HttpRoute { sym: self.current(), side, verb, path, path_ref: None, service: None, service_ref: None, line: line(at) });
        if let Some((name, qualifier)) = self.handler_group(handler) {
            self.handlers.push(PendingHandler { route, name, qualifier, owner: self.enclosing_type() });
        }
    }

    /// The method group a handler names: `GetItem`, `this.GetItem` (no qualifier), `CatalogApi.GetItem`.
    fn handler_group(&mut self, h: Node) -> Option<(String, Option<String>)> {
        match h.kind() {
            "identifier" => {
                let name = self.text(h);
                let local = self.local(name).is_some() || (self.bound_in_member(h, name) && !local_function_named(self.src, h, name));
                (!local).then(|| (name.to_string(), None))
            }
            "member_access_expression" => {
                let name = self.text(h.child_by_field_name("name")?).to_string();
                let e = h.child_by_field_name("expression")?;
                let qualifier = (!matches!(e.kind(), "this_expression" | "this")).then(|| self.text(e).to_string());
                Some((name, qualifier))
            }
            _ => None,
        }
    }

    /// The prefix the route groups of a route builder expression put endpoints under, encoded as a route (`{=Const}`
    /// for a constant, a leading `{@Helper#i}` for a helper's parameter `i`, whose callers pass its groups): `""` for
    /// the application itself or anything else; why not, when a group's prefix isn't a constant.
    fn route_base(&self, e: Option<Node>) -> Result<String, &'static str> {
        let Some(e) = e.map(unbang) else { return Ok(String::new()) };
        match e.kind() {
            "invocation_expression" => {
                let Some(f) = e.child_by_field_name("function").filter(|f| f.kind() == "member_access_expression") else { return Ok(String::new()) };
                let inner = self.route_base(f.child_by_field_name("expression"))?;
                if f.child_by_field_name("name").is_none_or(|n| self.text(n) != "MapGroup") {
                    // A builder's fluent configuration (`.RequireAuthorization()`, `.HasApiVersion(1, 0)`) keeps it.
                    return Ok(inner);
                }
                match e.child_by_field_name("arguments").and_then(first_argument).and_then(|t| self.route_text(t)) {
                    Some(t) => Ok(join_route(&inner, &t)),
                    None => Err("its route group's prefix is not a constant"),
                }
            }
            "identifier" => {
                let name = self.text(e);
                if let Some((_, _, b)) = self.group_vars.iter().rev().find(|(s, n, _)| *s == self.current() && n == name) {
                    return b.clone();
                }
                Ok(param_base(self.src, e).unwrap_or_default())
            }
            "parenthesized_expression" => self.route_base(children(e).find(|c| c.is_named())),
            _ => Ok(String::new()),
        }
    }

    /// Whether an expression is a route group: made by `MapGroup`, a local holding one, or a helper's parameter.
    fn holds_group(&self, e: Node) -> bool {
        let mut cur = unbang(e);
        loop {
            match cur.kind() {
                "invocation_expression" => {
                    let Some(f) = cur.child_by_field_name("function").filter(|f| f.kind() == "member_access_expression") else { return false };
                    if f.child_by_field_name("name").is_some_and(|n| self.text(n) == "MapGroup") {
                        return true;
                    }
                    let Some(next) = f.child_by_field_name("expression") else { return false };
                    cur = unbang(next);
                }
                "identifier" => {
                    let name = self.text(cur);
                    let builder = self.local(name).is_some_and(|t| endpoint_builder(&t.encode()));
                    return self.group_vars.iter().any(|(s, n, _)| *s == self.current() && n == name) || (builder && param_base(self.src, cur).is_some());
                }
                "parenthesized_expression" => match children(cur).find(|c| c.is_named()) {
                    Some(c) => cur = c,
                    None => return false,
                },
                _ => return false,
            }
        }
    }

    /// A call that may be a helper mapping endpoints on a group passed to it (`MapCatalogApi(group)`,
    /// `app.MapGroup("api").MapCatalogApi()`): each group passed, as a `group` fact for the helper's parameter. A
    /// helper named `Map…` called on the application records that too (an empty prefix), so the helper's routes keep
    /// their own path beside the prefixed ones.
    fn group_helper_call(&mut self, recv: Option<Node>, n: Node, args: &[(Option<String>, Node)]) {
        let callee = name_text(self.src, n);
        if callee.is_empty() || callee == "MapGroup" || MAP_VERBS.iter().any(|(m, _)| *m == callee) {
            return;
        }
        // `Helpers.MapOrders(group)` passes its arguments from 0; `group.MapOrders()`, an extension method, its receiver
        // as 0.
        let type_like = |r: Node| {
            matches!(r.kind(), "identifier" | "member_access_expression")
                && starts_upper(self.text(r))
                && self.local(self.text(r)).is_none()
                && !self.holds_group(r)
        };
        // On a receiver, a helper is named for what it maps (`MapOrdersApi`, `OrderEndpoints`); the builder's fluent
        // configuration (`RequireAuthorization`, `WithTags`) is not one.
        let helper = callee.starts_with("Map") || (!callee.starts_with("Use") && ["Endpoints", "Routes"].iter().any(|s| callee.ends_with(s)));
        let mut passed: Vec<(usize, Node)> = Vec::new();
        let shift = match recv {
            Some(r) if !type_like(r) => {
                if helper {
                    passed.push((0, r));
                }
                1
            }
            _ => 0,
        };
        passed.extend(args.iter().filter(|(name, _)| name.is_none()).enumerate().map(|(i, (_, e))| (i + shift, *e)));
        for (i, e) in passed {
            let group = self.holds_group(e);
            // The application or an endpoint builder: typed so, or named so when its type isn't written.
            let app = !group && callee.starts_with("Map") && e.kind() == "identifier" && {
                let name = self.text(e);
                match self.local(name).map(|t| t.encode()) {
                    Some(t) => endpoint_builder(&t),
                    None => matches!(name, "app" | "application" | "endpoints" | "routes" | "builder"),
                }
            };
            if !group && !app {
                continue;
            }
            let (verb, path) = match self.route_base(Some(e)) {
                Ok(b) => (String::new(), b),
                Err(why) => ("?".to_string(), why.to_string()),
            };
            self.facts.http_routes.push(HttpRoute {
                sym: self.current(),
                side: "group",
                verb,
                path,
                path_ref: None,
                service: Some(format!("{callee}#{i}")),
                service_ref: None,
                line: line(n),
            });
        }
    }

    /// A route template as an encoded route: literal text, `{=Name}` for a constant; `None` when part of it isn't a
    /// constant.
    fn route_text(&self, e: Node) -> Option<String> {
        let mut out = String::new();
        for p in self.url_pieces(e) {
            match p {
                Piece::Text(t) => out.push_str(&t),
                Piece::Ref(r) => {
                    out.push_str("{=");
                    out.push_str(&r);
                    out.push('}');
                }
                Piece::Unknown | Piece::Call => return None,
            }
        }
        Some(out)
    }

    /// Whether an expression is written as a string: a literal, an interpolation, a concatenation or a constant name.
    fn stringy(&self, e: Node) -> bool {
        matches!(e.kind(), "string_literal" | "verbatim_string_literal" | "raw_string_literal" | "interpolated_string_expression")
            || (e.kind() == "binary_expression" && e.child_by_field_name("operator").is_some_and(|o| self.text(o) == "+"))
            || self.constant_name(e).is_some()
    }

    /// `[HttpMethods.Options]`, `new[] { "GET", "POST" }`, `HttpMethods.Get`: the verbs, when each is a constant.
    fn verb_list(&self, e: Node) -> Option<Vec<String>> {
        let items: Vec<Node> = match e.kind() {
            "collection_expression" => children(e).filter(|c| c.is_named()).collect(),
            "array_creation_expression" | "implicit_array_creation_expression" | "object_creation_expression" => children(e)
                .filter(|c| c.kind() == "initializer_expression")
                .flat_map(children)
                .filter(|c| c.is_named())
                .collect(),
            _ => vec![e],
        };
        let items: Vec<Node> = items.into_iter().map(collection_item).collect();
        let verbs: Option<Vec<String>> = items.iter().map(|i| self.http_verb(*i)).collect();
        verbs.filter(|v| !v.is_empty())
    }

    /// `app.Map("/tenant", branch => branch.UseX())` on an `IApplicationBuilder`: a middleware branch, not an endpoint.
    fn pipeline_branch(&self, recv: Option<Node>, handler: Node) -> bool {
        let on_app_builder = recv
            .filter(|r| r.kind() == "identifier")
            .and_then(|r| self.local(self.text(r)))
            .is_some_and(|t| t.encode().contains("ApplicationBuilder"));
        let text = self.text(handler);
        let builds = match handler.kind() {
            "lambda_expression" | "anonymous_method_expression" => {
                [".Run(", ".Use(", ".UseEndpoints(", ".UseMvc(", ".UseRouting(", ".UseMiddleware"].iter().any(|m| text.contains(m))
            }
            "identifier" => self.src.contains(&format!("{text}(IApplicationBuilder")),
            _ => false,
        };
        on_app_builder || builds
    }

    /// Endpoints whose handler is a method group: anchored to the method it names, when the file declares exactly one
    /// (in the qualifier's type, else the registering type, else anywhere in the file).
    fn anchor_handlers(&mut self) {
        let last = |s: &str| s.rsplit('.').next().unwrap_or(s).to_string();
        for h in std::mem::take(&mut self.handlers) {
            let named: Vec<usize> = (0..self.facts.symbols.len())
                .filter(|&i| matches!(self.facts.symbols[i].kind, SymbolKind::Method | SymbolKind::Function) && self.facts.symbols[i].name == h.name)
                .collect();
            let in_type = |t: &str| -> Vec<usize> {
                named.iter().copied().filter(|&i| self.facts.symbols[i].container.as_deref().map(last).as_deref() == Some(t)).collect()
            };
            let found = match (&h.qualifier, &h.owner) {
                (Some(q), _) => in_type(&last(q)),
                (None, Some(o)) => match in_type(&last(o)) {
                    v if v.is_empty() => named.clone(),
                    v => v,
                },
                (None, None) => named.clone(),
            };
            if let [i] = found.as_slice() {
                self.facts.http_routes[h.route].sym = Some(*i);
            }
        }
    }

    /// A name passed as a delegate (see [`method_group_position`]): a method group, kept once the file is read unless
    /// it names a local, a parameter, or a field or property of the type.
    fn method_group(&mut self, node: Node) {
        let name = self.text(node);
        if self.local(name).is_some() {
            return;
        }
        let local_fn = local_function_named(self.src, node, name);
        if !local_fn && self.bound_in_member(node, name) {
            return;
        }
        let owner = self.enclosing_type().unwrap_or_default();
        let m = BareMember { name: name.to_string(), from: self.current(), line: line(node), col: name_col(self.src, node, name), owner, assigned: false };
        self.method_groups.push((m, local_fn));
    }

    /// Method groups passed as delegates, as references: a member reference to the method (a call for a local
    /// function, which is no member), resolved like any other.
    fn method_group_refs(&mut self) {
        for (m, local_fn) in std::mem::take(&mut self.method_groups) {
            let field = self
                .facts
                .symbols
                .iter()
                .any(|s| s.kind == SymbolKind::Property && s.name == m.name && s.container.as_deref() == Some(m.owner.as_str()));
            if field {
                continue;
            }
            let kind = if local_fn { RefKind::Call } else { RefKind::Member };
            // Inside an extension method (a member of the type it extends), a method group names a method of the
            // class declaring it, which is no supertype of the extended type: say so, as `Class.Method` would.
            let owner = m.owner.rsplit('.').next().unwrap_or(&m.owner).to_string();
            let extension = m.from.is_some_and(|f| self.facts.symbols[f].container.as_deref().is_some_and(|c| c != m.owner));
            let declared = self.facts.symbols.iter().any(|s| s.kind == SymbolKind::Method && s.name == m.name && s.container.as_deref() == Some(m.owner.as_str()));
            let receiver = (extension && declared && !local_fn).then_some(owner);
            if let std::collections::hash_map::Entry::Vacant(e) = self.seen.entry((m.from, m.name.clone(), kind, m.line, m.col)) {
                e.insert(self.facts.refs.len());
                self.facts.refs.push(Ref { from: m.from, name: m.name, receiver, receiver_type: None, kind, line: m.line, col: m.col, args: None });
            }
        }
    }

    /// `[Endpoint("/orders", HttpMethod.Post)]` on a request class, sent by a generic client: any class attribute
    /// with exactly two arguments, a path (a literal with a `/`, or a constant) and an HTTP verb (`X.Post`, `"POST"`),
    /// makes the class a client route. Properties marked with a positive ordinal by an attribute naming a path, segment, route, URL or query
    /// (`[PathSegment(1)]`) append their values as path parameters, in order. A path that isn't a constant is an
    /// `unsupported` fact.
    fn attribute_contract(&mut self, sym: usize, node: Node) {
        if self.facts.http_routes.iter().any(|r| r.sym == Some(sym)) {
            return;
        }
        for (name, args) in all_attributes(self.src, node) {
            // Exactly a path and a verb: an attribute with more arguments (a service name, say) has another shape.
            if name == "Route" || args.len() != 2 {
                continue;
            }
            let Some((vi, verb)) = args.iter().enumerate().find_map(|(i, a)| self.attribute_verb(*a).map(|v| (i, v))) else { continue };
            let Some(path) = args.iter().enumerate().filter(|(i, _)| *i != vi).map(|(_, a)| *a).find(|a| self.path_like(*a)) else { continue };
            let (side, path) = match self.route_text(path) {
                Some(p) => ("client", format!("{}{}", p.trim_end_matches('/'), self.contract_segments(node))),
                None => ("unsupported", "a request contract whose path is not a constant".to_string()),
            };
            let verb = if side == "client" { verb } else { name.clone() };
            self.facts.http_routes.push(HttpRoute {
                sym: Some(sym),
                side,
                verb,
                path: path.trim_matches('/').to_string(),
                path_ref: None,
                service: None,
                service_ref: None,
                line: line(node),
            });
            return;
        }
    }

    /// `HttpMethod.Post`, `Verb.Get`, `"DELETE"`: an HTTP verb written as an attribute argument.
    fn attribute_verb(&self, a: Node) -> Option<String> {
        match a.kind() {
            "member_access_expression" => urls::verb(self.text(a.child_by_field_name("name")?)),
            _ => urls::verb(&string_value(self.src, a)?),
        }
    }

    /// Whether an attribute argument reads as a request path: a literal with a `/` in it, an interpolation, or a
    /// constant.
    fn path_like(&self, a: Node) -> bool {
        match string_value(self.src, a) {
            Some(s) => s.contains('/'),
            None => a.kind() == "interpolated_string_expression" || self.constant_name(a).is_some(),
        }
    }

    /// The path parameters a request contract's properties add (see [`Self::attribute_contract`]): `/{OrderId}`.
    fn contract_segments(&self, node: Node) -> String {
        let Some(body) = children(node).find(|c| c.kind() == "declaration_list") else { return String::new() };
        let mut segs: Vec<(u64, String)> = Vec::new();
        for p in children(body).filter(|c| c.kind() == "property_declaration") {
            let Some(pn) = p.child_by_field_name("name") else { continue };
            for (attr, args) in all_attributes(self.src, p) {
                let lower = attr.to_ascii_lowercase();
                if !["path", "segment", "route", "url", "query"].iter().any(|w| lower.contains(w)) {
                    continue;
                }
                if let [a] = args.as_slice()
                    && a.kind() == "integer_literal"
                    && let Ok(n) = self.text(*a).parse::<u64>()
                    && n > 0
                {
                    segs.push((n, self.text(pn).to_string()));
                }
            }
        }
        segs.sort();
        segs.iter().map(|(_, n)| format!("/{{{n}}}")).collect()
    }

    /// `c.BaseAddress = new Uri("https://orders.example.com/")`: where a typed HTTP client sends, for the client class
    /// (`AddHttpClient<TClient>(c => …)`'s, or the type setting its own client's).
    fn base_address(&mut self, node: Node) {
        let (Some(left), Some(right)) = (node.child_by_field_name("left"), node.child_by_field_name("right")) else { return };
        let target = match left.kind() {
            "identifier" => self.text(left),
            "member_access_expression" => left.child_by_field_name("name").map_or("", |n| self.text(n)),
            _ => return,
        };
        if target != "BaseAddress" {
            return;
        }
        let url = match right.kind() {
            "object_creation_expression" | "implicit_object_creation_expression" => children(right).find(|c| c.kind() == "argument_list").and_then(first_argument),
            _ => Some(right),
        };
        let Some(host) = url.and_then(|u| string_value(self.src, u)).and_then(|u| urls::host_of(&u)) else { return };
        let client = match typed_client(self.src, node) {
            Some(Some(t)) => t,
            // A named client (`AddHttpClient("orders", …)`): no class to attach it to.
            Some(None) => return,
            None => match self.enclosing_type() {
                Some(t) => t.rsplit('.').next().unwrap_or(&t).to_string(),
                None => return,
            },
        };
        self.facts.hosts.push(HostFact { role: "configures", kind: "http_client", host, target: Some(client), line: line(node) });
    }

    /// `builder.AddProject<Projects.Orders>("orders-api")` in an Aspire app host: the repo serves that resource name.
    fn app_host(&mut self, call: Node) {
        if !self.src.contains("DistributedApplication") {
            return;
        }
        let Some(n) = call.child_by_field_name("function").filter(|f| f.kind() == "member_access_expression").and_then(|f| f.child_by_field_name("name")) else {
            return;
        };
        let name = name_text(self.src, n);
        if !(name.starts_with("Add") && name.ends_with("Project")) {
            return;
        }
        let Some(host) = call.child_by_field_name("arguments").and_then(first_argument).and_then(|a| string_value(self.src, a)) else { return };
        if host_name_like(&host) {
            self.facts.hosts.push(HostFact { role: "serves", kind: "app_host", host: host.to_ascii_lowercase(), target: None, line: line(n) });
        }
    }
}

/// `[a, b]`: each `collection_element` holds an `expression_element` holding the expression.
fn collection_item(mut i: Node) -> Node {
    while matches!(i.kind(), "collection_element" | "expression_element") {
        match children(i).find(|c| c.is_named()) {
            Some(c) => i = c,
            None => break,
        }
    }
    i
}

/// Whether a type (encoded) is something endpoints are mapped on: the application, an endpoint route builder, a group.
fn endpoint_builder(t: &str) -> bool {
    ["WebApplication", "EndpointRouteBuilder", "RouteGroupBuilder"].iter().any(|b| t.contains(b))
}

fn host_name_like(h: &str) -> bool {
    !h.is_empty() && h.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_'))
}

/// A route's prefix and template joined: `{@Map#0}/api` and `/items/{id}` give `{@Map#0}/api/items/{id}`.
fn join_route(base: &str, template: &str) -> String {
    [base, template.trim_start_matches('~')].iter().flat_map(|s| s.split('/')).filter(|s| !s.is_empty()).collect::<Vec<_>>().join("/")
}

/// `{@Method#i}` when `e` names parameter `i` of the method or local function holding it (an extension method's `this`
/// is 0).
fn param_base(src: &str, e: Node) -> Option<String> {
    let name = &src[e.byte_range()];
    let m = enclosing_method(e).filter(|m| m.kind() != "constructor_declaration")?;
    let method = &src[m.child_by_field_name("name")?.byte_range()];
    let params = m.child_by_field_name("parameters")?;
    let i = children(params)
        .filter(|c| c.kind() == "parameter")
        .position(|p| p.child_by_field_name("name").is_some_and(|n| &src[n.byte_range()] == name))?;
    Some(format!("{{@{method}#{i}}}"))
}

/// The arguments of a call: each one's name (`handler:`), if any, and expression.
fn call_args<'t>(src: &str, call: Node<'t>) -> Vec<(Option<String>, Node<'t>)> {
    let Some(list) = call.child_by_field_name("arguments") else { return Vec::new() };
    children(list)
        .filter(|c| c.kind() == "argument")
        .filter_map(|a| {
            let name = a.child_by_field_name("name");
            let e = children(a).filter(|c| c.is_named() && Some(*c) != name).last()?;
            Some((name.map(|n| src[n.byte_range()].to_string()), e))
        })
        .collect()
}

/// Every attribute on a declaration, with its arguments' expressions (named ones too), its name without the
/// `Attribute` suffix.
fn all_attributes<'t>(src: &str, node: Node<'t>) -> Vec<(String, Vec<Node<'t>>)> {
    children(node)
        .filter(|c| c.kind() == "attribute_list")
        .flat_map(children)
        .filter(|a| a.kind() == "attribute")
        .filter_map(|a| {
            let name = base_type_name(src, a.child_by_field_name("name")?);
            let name = name.strip_suffix("Attribute").filter(|s| !s.is_empty()).map_or(name.clone(), str::to_string);
            let args = children(a)
                .filter(|c| c.kind() == "attribute_argument_list")
                .flat_map(children)
                .filter(|c| c.kind() == "attribute_argument")
                .filter_map(|arg| children(arg).filter(|c| c.is_named()).last())
                .collect();
            Some((name, args))
        })
        .collect()
}

/// Inside `AddHttpClient<TClient>(…)` (or `<IClient, TClient>`): `Some(Some(TClient))`; inside a named client's
/// `AddHttpClient("name", …)`: `Some(None)`; elsewhere `None`.
fn typed_client(src: &str, node: Node) -> Option<Option<String>> {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if n.kind() == "invocation_expression"
            && let Some(name) = n.child_by_field_name("function").filter(|f| f.kind() == "member_access_expression").and_then(|f| f.child_by_field_name("name"))
            && name_text(src, name) == "AddHttpClient"
        {
            let last = children(name).filter(|c| c.kind() == "type_argument_list").flat_map(children).filter(|c| c.is_named()).last();
            return Some(last.map(|t| base_type_name(src, t)).filter(|t| !t.is_empty()));
        }
        cur = n.parent();
    }
    None
}

/// Whether `node` (an identifier) is passed as a delegate: a call's or constructor's argument (not `nameof`'s, not an
/// argument's name), or the right side of `+=` or `-=`.
fn method_group_position(src: &str, node: Node) -> bool {
    let Some(p) = node.parent() else { return false };
    match p.kind() {
        "argument" => {
            p.child_by_field_name("name") != Some(node)
                && p.parent().and_then(|l| l.parent()).is_some_and(|c| match c.kind() {
                    "invocation_expression" => c.child_by_field_name("function").is_none_or(|f| &src[f.byte_range()] != "nameof"),
                    "object_creation_expression" | "implicit_object_creation_expression" | "constructor_initializer" => true,
                    _ => false,
                })
        }
        "assignment_expression" => {
            let (Some(l), Some(r)) = (p.child_by_field_name("left"), p.child_by_field_name("right")) else { return false };
            r == node && matches!(src[l.end_byte()..r.start_byte()].trim(), "+=" | "-=")
        }
        _ => false,
    }
}

/// Whether the member holding `at` declares a local function `name`.
fn local_function_named(src: &str, at: Node, name: &str) -> bool {
    let Some(m) = enclosing_method(at) else { return false };
    let mut stack = vec![m];
    while let Some(n) = stack.pop() {
        if n.kind() == "local_function_statement" && n.child_by_field_name("name").is_some_and(|x| &src[x.byte_range()] == name) {
            return true;
        }
        stack.extend(children(n));
    }
    false
}

/// The value a local `name` is initialized with, when the method holding `at` declares it once, before `at`, and never
/// assigns it again.
fn local_value<'t>(src: &str, at: Node<'t>, name: &str) -> Option<Node<'t>> {
    let body = enclosing_method(at)?;
    let (mut found, mut declared, mut assigned) = (None, 0, false);
    let mut stack = vec![body];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "variable_declarator" if n.child_by_field_name("name").is_some_and(|x| &src[x.byte_range()] == name) => {
                declared += 1;
                let id = n.child_by_field_name("name");
                if n.start_byte() < at.start_byte() {
                    found = children(n).filter(|c| c.is_named() && Some(*c) != id).last();
                }
            }
            "assignment_expression" if n.child_by_field_name("left").is_some_and(|l| &src[l.byte_range()] == name) => assigned = true,
            _ => {}
        }
        stack.extend(children(n));
    }
    if declared == 1 && !assigned { found } else { None }
}

/// `Models` in `Models.Domain`, `A.B` in `A.B.Foo<T>`; `None` for an unqualified or `global::` name.
fn type_qualifier(src: &str, t: Node) -> Option<String> {
    let t = if t.kind() == "nullable_type" { t.child_by_field_name("type")? } else { t };
    if t.kind() != "qualified_name" {
        return None;
    }
    let q = t.child_by_field_name("qualifier")?;
    if q.kind() == "alias_qualified_name" {
        return None;
    }
    Some(dotted_type(src, q)).filter(|q| !q.is_empty())
}

/// `Foo`, `Foo<T>` → `Foo`.
fn name_text(src: &str, n: Node) -> String {
    if n.kind() == "generic_name" {
        children(n).find(|c| c.kind() == "identifier").map_or_else(String::new, |i| src[i.byte_range()].to_string())
    } else {
        src[n.byte_range()].to_string()
    }
}

/// `Foo`, `Foo<T>`, `A.B.Foo`, `Foo?`, `global::Foo` → `Foo`. Empty for predefined and other unnamed types.
fn base_type_name(src: &str, t: Node) -> String {
    match t.kind() {
        "identifier" | "generic_name" => name_text(src, t),
        "qualified_name" | "alias_qualified_name" => t.child_by_field_name("name").map_or_else(String::new, |n| base_type_name(src, n)),
        "nullable_type" => t.child_by_field_name("type").map_or_else(String::new, |n| base_type_name(src, n)),
        _ => String::new(),
    }
}

/// A type as written, as a type expression. A qualified name keeps its qualifier (`Data.Policy`), for telling
/// same-named types apart; generic types keep their arguments, except the collections, dictionaries and wrappers
/// below, which become what they hold.
fn type_expr(src: &str, t: Node) -> Option<TypeExpr> {
    match t.kind() {
        "identifier" | "alias_qualified_name" => {
            let name = base_type_name(src, t);
            (!name.is_empty() && name != "var").then_some(TypeExpr::Named(name))
        }
        // `string`, `int`: the framework's, whose members never are the repo's.
        "predefined_type" => {
            let name = &src[t.byte_range()];
            (!matches!(name, "void" | "dynamic")).then(|| TypeExpr::Named(name.to_string()))
        }
        "qualified_name" => {
            let qualifier = type_qualifier(src, t);
            let inner = type_expr(src, t.child_by_field_name("name")?)?;
            Some(match (qualifier, inner) {
                (Some(q), TypeExpr::Named(n)) => TypeExpr::Named(format!("{q}.{n}")),
                (Some(q), TypeExpr::Generic { name, args }) => TypeExpr::Generic { name: format!("{q}.{name}"), args },
                (_, other) => other,
            })
        }
        "nullable_type" => t.child_by_field_name("type").and_then(|i| type_expr(src, i)),
        "array_type" => t.child_by_field_name("type").and_then(|i| type_expr(src, i)).map(|e| TypeExpr::Array(Box::new(e))),
        "tuple_type" => {
            let elements = children(t)
                .filter(|c| c.kind() == "tuple_element")
                .enumerate()
                .map(|(i, e)| {
                    let name = e.child_by_field_name("name").map_or_else(|| format!("Item{}", i + 1), |n| src[n.byte_range()].to_string());
                    (name, e.child_by_field_name("type").and_then(|ty| type_expr(src, ty)))
                })
                .collect();
            Some(TypeExpr::Tuple(elements))
        }
        "generic_name" => {
            let base = name_text(src, t);
            let all: Vec<Option<TypeExpr>> =
                children(t).filter(|c| c.kind() == "type_argument_list").flat_map(children).filter(|c| c.is_named()).map(|c| type_expr(src, c)).collect();
            let known: Vec<TypeExpr> = all.iter().flatten().cloned().collect();
            match (base.as_str(), known.as_slice()) {
                // `Func<A, R>`: what a call returns is the last argument.
                ("Func", _) if all.last().is_some_and(Option::is_some) => Some(TypeExpr::Delegate(Box::new(all.last().cloned().flatten()?))),
                (
                    "List" | "IList" | "IEnumerable" | "ICollection" | "IReadOnlyList" | "IReadOnlyCollection" | "HashSet"
                    | "ISet" | "IQueryable" | "IAsyncEnumerable" | "DbSet" | "IDbSet",
                    [e],
                ) => Some(TypeExpr::Array(Box::new(e.clone()))),
                ("Dictionary" | "IDictionary" | "IReadOnlyDictionary" | "ConcurrentDictionary", [k, v]) => {
                    Some(TypeExpr::Dict(Box::new(k.clone()), Box::new(v.clone())))
                }
                ("Task" | "ValueTask" | "Nullable" | "ActionResult" | "Mock", [inner]) => Some(inner.clone()),
                _ if known.is_empty() => Some(TypeExpr::Named(base)),
                _ => Some(TypeExpr::Generic { name: base, args: all }),
            }
        }
        _ => None,
    }
}

/// `T`'s base name in `Name<T, …>`.
fn first_type_arg(src: &str, generic: Node) -> Option<String> {
    let arg = children(generic).filter(|c| c.kind() == "type_argument_list").flat_map(children).find(|c| c.is_named())?;
    Some(base_type_name(src, arg)).filter(|n| !n.is_empty())
}

/// A method's arity (see [`Symbol::arity`]) and its parameters' delegate input types (see [`Symbol::lambda_types`]).
/// A parameter with a default is optional; a `params` array or `__arglist` takes any number; an extension method's
/// `this` parameter is the receiver, not an argument.
fn parameters(src: &str, decl: Node, list: Node) -> ((u32, Option<u32>), Option<String>) {
    let mut ps: Vec<Node> = children(list).filter(|c| c.kind() == "parameter").collect();
    if ps.first().is_some_and(|p| has_modifier(src, *p, "this")) {
        ps.remove(0);
    }
    let arglist = ps.iter().any(|p| p.child_by_field_name("name").is_some_and(|n| &src[n.byte_range()] == "__arglist"));
    // `params T[] xs` hangs off the list itself rather than being a `parameter`.
    let variadic = arglist || list.child_by_field_name("name").is_some();
    let ps: Vec<Node> =
        ps.into_iter().filter(|p| p.child_by_field_name("name").is_none_or(|n| &src[n.byte_range()] != "__arglist")).collect();
    let optional = |p: &Node| {
        let skip = [p.child_by_field_name("type"), p.child_by_field_name("name")];
        children(*p)
            .any(|c| c.is_named() && !matches!(c.kind(), "attribute_list" | "modifier") && !skip.contains(&Some(c)))
    };
    let min = ps.iter().filter(|p| !optional(p)).count() as u32;
    let max = (!variadic).then_some(ps.len() as u32);
    // Only a generic parameter type can be a delegate; the type parameters in scope are looked up only then.
    let generic_param =
        |p: &Node| p.child_by_field_name("type").is_some_and(|t| !matches!(t.kind(), "predefined_type" | "identifier"));
    let delegates: Vec<Option<Vec<Option<String>>>> = if ps.iter().any(generic_param) {
        let mut generic = type_params_of(src, decl);
        generic.extend(type_params_around(src, decl));
        ps.iter().map(|p| p.child_by_field_name("type").and_then(|t| delegate_inputs(src, t, &generic))).collect()
    } else {
        Vec::new()
    };
    let lambda_types =
        delegates.iter().any(Option::is_some).then(|| serde_json::to_string(&delegates).expect("serializes"));
    ((min, max), lambda_types)
}

/// A method's parameters' declared types (see [`FileFacts::param_types`]), as JSON; `None` without parameters. A type
/// parameter of the method or of an enclosing type is `null`, and so is any type built on one.
fn parameter_types(src: &str, decl: Node, list: Node) -> Option<String> {
    let mut ps: Vec<Node> = children(list).filter(|c| c.kind() == "parameter").collect();
    if ps.first().is_some_and(|p| has_modifier(src, *p, "this")) {
        ps.remove(0);
    }
    if ps.is_empty() {
        return None;
    }
    let mut generic = type_params_of(src, decl);
    generic.extend(type_params_around(src, decl));
    let mentions_generic = |t: &TypeExpr| {
        let any = std::cell::Cell::new(false);
        t.clone().map_names(&|n: &str| {
            if generic.iter().any(|g| g == n) {
                any.set(true);
            }
            n.to_string()
        });
        any.get()
    };
    let types: Vec<Option<String>> = ps
        .iter()
        .map(|p| {
            let t = type_expr(src, p.child_by_field_name("type")?)?;
            (!mentions_generic(&t)).then(|| t.encode())
        })
        .collect();
    let names: Vec<&str> = ps.iter().map(|p| p.child_by_field_name("name").map_or("", |n| &src[n.byte_range()])).collect();
    Some(serde_json::json!({ "types": types, "names": names }).to_string())
}

/// The input types of a delegate type, encoded: `Func<A, B, R>` → A, B; `Action<A>` and `Predicate<A>` → A;
/// `Expression<D>` → D's. A type parameter of the method or an enclosing type is unknown (`None`).
fn delegate_inputs(src: &str, t: Node, generic: &[String]) -> Option<Vec<Option<String>>> {
    let t = match t.kind() {
        "qualified_name" | "alias_qualified_name" => t.child_by_field_name("name")?,
        "nullable_type" => t.child_by_field_name("type")?,
        _ => t,
    };
    if t.kind() != "generic_name" {
        return None;
    }
    let args: Vec<Node> =
        children(t).filter(|c| c.kind() == "type_argument_list").flat_map(children).filter(|c| c.is_named()).collect();
    let inputs: Vec<Node> = match (name_text(src, t).as_str(), args.len()) {
        ("Expression", 1) => return delegate_inputs(src, args[0], generic),
        ("Func", n) if n >= 2 => args[..n - 1].to_vec(),
        ("Action", _) | ("Predicate", 1) => args,
        ("Comparison", 1) => vec![args[0], args[0]],
        _ => return None,
    };
    let typed: Vec<Option<String>> = inputs
        .iter()
        .map(|a| type_expr(src, *a).filter(|_| !generic.contains(&base_type_name(src, *a))).map(|e| e.encode()))
        .collect();
    typed.iter().any(Option::is_some).then_some(typed)
}

/// A lambda's single parameter: `b => …`, `(b) => …`, `(EntityTypeBuilder<T> b) => …`.
fn lambda_param(lambda: Node) -> Option<Node> {
    let p = lambda.child_by_field_name("parameters")?;
    match p.kind() {
        "implicit_parameter" | "identifier" => Some(p),
        _ => children(p).find(|c| c.kind() == "parameter").and_then(|c| c.child_by_field_name("name")),
    }
}

fn qualify(schema: Option<String>, table: String) -> String {
    match schema.filter(|s| !s.is_empty()) {
        Some(s) => format!("{s}.{table}"),
        None => table,
    }
}

/// The text of a string literal or interpolated string (interpolations left out), for SQL.
fn sql_text(src: &str, n: Node) -> Option<String> {
    match n.kind() {
        // `"CALL p(" + id + ")"`: the text up to the first value.
        "binary_expression" => sql_text(src, n.child_by_field_name("left")?),
        "interpolated_string_expression" => {
            Some(children(n).filter(|c| c.kind() == "string_content").map(|c| &src[c.byte_range()]).collect::<Vec<_>>().join(" "))
        }
        _ => string_value(src, n),
    }
}

/// The navigations an `Include` argument names, outermost first: `o => o.Lines` gives `Lines`, `o => o.Customer.Address`
/// `Customer` and `Address`, a filtered include `o => o.Lines.Where(…)` `Lines`, EF6's `o => o.Lines.Select(l =>
/// l.Product)` `Lines` and `Product`, and a string path `"Lines.Product"` both names.
fn navigation_path(src: &str, arg: Node) -> Vec<String> {
    fn walk(src: &str, e: Node, out: &mut Vec<String>) {
        match e.kind() {
            "lambda_expression" => {
                if let Some(b) = e.child_by_field_name("body") {
                    walk(src, b, out);
                }
            }
            "member_access_expression" => {
                if let Some(x) = e.child_by_field_name("expression") {
                    walk(src, x, out);
                }
                if let Some(n) = e.child_by_field_name("name") {
                    out.push(name_text(src, n));
                }
            }
            "invocation_expression" => {
                let Some(f) = e.child_by_field_name("function").filter(|f| f.kind() == "member_access_expression") else { return };
                if let Some(x) = f.child_by_field_name("expression") {
                    walk(src, x, out);
                }
                if f.child_by_field_name("name").is_some_and(|n| &src[n.byte_range()] == "Select")
                    && let Some(l) = e.child_by_field_name("arguments").and_then(first_argument)
                {
                    walk(src, l, out);
                }
            }
            "parenthesized_expression" | "cast_expression" => {
                if let Some(c) = children(e).filter(|c| c.is_named()).last() {
                    walk(src, c, out);
                }
            }
            _ => {}
        }
    }
    if let Some(path) = string_value(src, arg) {
        return path.split('.').filter(|p| !p.is_empty()).map(str::to_string).collect();
    }
    let mut out = Vec::new();
    walk(src, arg, &mut out);
    out
}

/// An EF provider call taking the connection: `UseMySql`, `UseSqlServer`, `UseNpgsql`, `UseSqlite`, `UseOracle` and
/// variants with a suffix (`UseMySql…`).
fn is_provider_call(name: &str) -> bool {
    name.strip_prefix("Use").is_some_and(|r| ["MySql", "MySQL", "SqlServer", "Npgsql", "Sqlite", "Oracle"].iter().any(|p| r.starts_with(p)))
}

/// The connection an expression names: `Settings.ShopDbConnection` and `shopConnection` by the member,
/// `GetConnectionString("Shop")`, `config["ConnectionStrings:Shop"]` and `GetValue<string>("Shop")` by the key, and a
/// literal connection string by its database, `Database=shop`.
fn connection_name(src: &str, e: Node) -> Option<String> {
    let name = match e.kind() {
        "member_access_expression" => e.child_by_field_name("name").map(|n| name_text(src, n)),
        "identifier" => Some(src[e.byte_range()].to_string()),
        "invocation_expression" => e.child_by_field_name("arguments").and_then(first_argument).and_then(|a| string_value(src, a)),
        "element_access_expression" => {
            let sub = children(e).find(|c| c.kind() == "bracketed_argument_list").and_then(first_argument);
            sub.and_then(|a| string_value(src, a)).map(|k| k.rsplit(':').next().unwrap_or(&k).to_string())
        }
        _ => {
            let text = string_value(src, e)?;
            let lower = text.to_ascii_lowercase();
            let i = lower.find("database=").or_else(|| lower.find("initial catalog="))?;
            let value = text[i..].split_once('=')?.1.split(';').next()?.trim().to_string();
            return (!value.is_empty()).then(|| format!("Database={value}"));
        }
    }?;
    let name = name.trim().to_string();
    (!name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == '-')).then_some(name)
}

/// An escaped `\n` in a regular or interpolated string: a line break of the text, not of the file.
const ESCAPED_NEWLINE: char = '\u{b}';

/// The `entity` of a `sql_read` / `sql_write` fact whose text is the value of the field or property it is on (see
/// [`sql_value`]): the methods using the holder run it, or the holder itself when none does.
const HELD: &str = "held";

/// A [`HELD`] fact whose holder's users in its file carry the facts themselves: other users are credited, never the
/// holder.
const HELD_USED: &str = "held_used";

/// Callees taking query text in another language (WMI's WQL): `SELECT * FROM Win32_Process` reads no table.
const NO_SQL_CALLEES: &[&str] = &["ObjectQuery", "WqlObjectQuery", "ManagementObjectSearcher"];

/// Whether a string is the value of the field or property holding it (`const string Sql = "…"`, `string Sql { get; } =
/// "…"`, `string Sql => "…"`, a getter returning it, or a local it returns: `get { string q = "…"; return q; }`),
/// possibly concatenated or chosen by a conditional; not text a getter runs (`get { return _conn.Query<int>("…")
/// .First(); }`).
fn sql_value(src: &str, node: Node) -> bool {
    // The getter holding `n`, if any (not past a lambda or local function).
    fn getter(n: Node) -> Option<Node> {
        let mut up = n.parent();
        while let Some(p) = up {
            match p.kind() {
                "accessor_declaration" => return Some(p),
                "lambda_expression" | "local_function_statement" | "anonymous_method_expression" => return None,
                _ => up = p.parent(),
            }
        }
        None
    }
    // Whether `getter` returns the local `name`.
    let returns = |getter: Node, name: &str| {
        let mut stack = vec![getter];
        while let Some(n) = stack.pop() {
            if n.kind() == "return_statement" && children(n).find(|c| c.is_named()).is_some_and(|e| e.kind() == "identifier" && &src[e.byte_range()] == name) {
                return true;
            }
            stack.extend(children(n));
        }
        false
    };
    let mut n = node;
    while let Some(p) = n.parent() {
        match p.kind() {
            "parenthesized_expression" | "binary_expression" | "conditional_expression" | "equals_value_clause" => n = p,
            "variable_declarator" => {
                if p.parent().and_then(|d| d.parent()).is_some_and(|f| f.kind() == "field_declaration") {
                    return true;
                }
                let name = p.child_by_field_name("name").map(|m| &src[m.byte_range()]);
                return name.zip(getter(p)).is_some_and(|(name, g)| returns(g, name));
            }
            "assignment_expression" => {
                let left = p.child_by_field_name("left").filter(|l| l.kind() == "identifier").map(|l| &src[l.byte_range()]);
                return left.zip(getter(p)).is_some_and(|(name, g)| returns(g, name));
            }
            "property_declaration" => return true,
            "arrow_expression_clause" => return p.parent().is_some_and(|d| matches!(d.kind(), "property_declaration" | "accessor_declaration")),
            "return_statement" => return getter(p).is_some(),
            _ => return false,
        }
    }
    false
}

/// The pieces of a string expression in order, each with its first row: a literal's or interpolated string's text
/// (an interpolation, or a `{0}` format hole, as `DYNAMIC`; an escaped `\n` as [`ESCAPED_NEWLINE`]), or `None` for a
/// value spliced in. A `+` concatenation gives its operands' pieces.
fn string_leaves(src: &str, e: Node, out: &mut Vec<(usize, Option<String>)>) {
    let row = e.start_position().row;
    match e.kind() {
        "binary_expression" if e.child_by_field_name("operator").is_some_and(|o| &src[o.byte_range()] == "+") => {
            for f in ["left", "right"] {
                if let Some(c) = e.child_by_field_name(f) {
                    string_leaves(src, c, out);
                }
            }
        }
        "parenthesized_expression" => match children(e).find(|c| c.is_named()) {
            Some(c) => string_leaves(src, c, out),
            None => out.push((row, None)),
        },
        "interpolated_string_expression" => {
            let mut text = String::new();
            for c in children(e) {
                match c.kind() {
                    "string_content" => text.push_str(&src[c.byte_range()]),
                    "interpolation" => text.push_str(crate::sql::DYNAMIC),
                    "escape_sequence" => text.push(if &src[c.byte_range()] == "\\n" { ESCAPED_NEWLINE } else { ' ' }),
                    _ => {}
                }
            }
            out.push((row, Some(text)));
        }
        "string_literal" | "verbatim_string_literal" | "raw_string_literal" => {
            let raw = string_value(src, e).unwrap_or_default();
            let text = if e.kind() == "string_literal" {
                raw.replace("\\n", &ESCAPED_NEWLINE.to_string()).replace("\\r", " ").replace("\\t", " ")
            } else {
                raw.replace("\"\"", "\"")
            };
            out.push((row, Some(format_holes(&text))));
        }
        _ => out.push((row, None)),
    }
}

/// `{0}`, `{1}` (a `string.Format` hole) as `DYNAMIC`.
fn format_holes(text: &str) -> String {
    if !text.contains('{') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        match after.find('}') {
            Some(j) if j > 0 && after[..j].bytes().all(|b| b.is_ascii_digit()) => {
                out.push_str(crate::sql::DYNAMIC);
                rest = &after[j + 1..];
            }
            _ => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The statement verb text starts with (upper-cased): `SELECT`, `INSERT`, `UPDATE`, `DELETE`, `REPLACE`, `TRUNCATE`,
/// `WITH` or `MERGE`.
fn sql_verb(text: &str) -> Option<String> {
    let t = text.trim_start_matches(|c: char| c.is_whitespace() || c == '(');
    let word: String = t.chars().take_while(|c| c.is_ascii_alphabetic()).collect::<String>().to_ascii_uppercase();
    matches!(word.as_str(), "SELECT" | "INSERT" | "UPDATE" | "DELETE" | "REPLACE" | "TRUNCATE" | "WITH" | "MERGE").then_some(word)
}

/// The upper-cased words of text.
fn sql_words(text: &str) -> Vec<String> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_')).filter(|w| !w.is_empty()).map(str::to_ascii_uppercase).collect()
}

/// Whether text starting with a statement verb has the statement's shape: `SELECT … FROM`, `INSERT [IGNORE] INTO`,
/// `UPDATE … SET`, `DELETE … FROM`, `REPLACE INTO`, `TRUNCATE`, `WITH … AS`, `MERGE INTO`.
fn sql_shaped(text: &str) -> bool {
    let words = sql_words(text);
    let has = |w: &str| words.iter().any(|x| x == w);
    match words.first().map(String::as_str) {
        Some("SELECT") | Some("DELETE") => has("FROM"),
        Some("INSERT") => words.get(1).is_some_and(|w| w == "INTO") || (words.get(1).is_some_and(|w| w == "IGNORE") && has("INTO")),
        Some("REPLACE") | Some("MERGE") => words.get(1).is_some_and(|w| w == "INTO"),
        Some("UPDATE") => has("SET"),
        Some("TRUNCATE") => words.len() >= 2,
        Some("WITH") => has("AS") && (has("SELECT") || has("INSERT") || has("UPDATE") || has("DELETE")),
        _ => false,
    }
}

/// Whether SQL-shaped text reads as SQL beyond its first words, not as a sentence ("Select a plan from the list; then
/// update the cart set by your admin", "Delete from server after 5 days"), by its verb: a `SELECT` or `DELETE` (or a
/// `WITH`) with a `WHERE`, `JOIN`, `LIMIT`, `ORDER BY`, `GROUP BY`, `UNION` or `HAVING`, an `@` parameter or a
/// backquoted name, or `SELECT *`, or a literal `SELECT` or `DELETE` ending in its only `;`; an `INSERT` or
/// `REPLACE` with `VALUES (` (or rows spliced in) or a `SELECT`; an `UPDATE` with `SET … WHERE`, a parameter, or a
/// `db.table` or quoted target; `TRUNCATE TABLE`; `MERGE … USING`. A value spliced in elsewhere, a `?` or a `;`
/// mid-text says nothing.
fn sql_detailed(text: &str) -> bool {
    let words = sql_words(text);
    let has = |w: &str| words.iter().any(|x| x == w);
    let at = |w: &str| words.iter().position(|x| x == w);
    let param = text.match_indices('@').any(|(i, _)| text[i + 1..].starts_with(|c: char| c.is_ascii_alphabetic() || c == '_'));
    let clauses = || {
        ["WHERE", "JOIN", "LIMIT", "UNION", "HAVING"].iter().any(|w| has(w))
            || words.windows(2).any(|p| matches!((p[0].as_str(), p[1].as_str()), ("ORDER" | "GROUP", "BY")))
            || param
            || text.contains('`')
    };
    // A `;` ending a literal statement, the only one: `SELECT a FROM t;` (one mid-text joins sentences, and a message
    // with values spliced in may end in one: `"Delete from server after " + n + " days;"`).
    let ends_statement =
        || text.trim_end().ends_with(';') && text.matches(';').count() == 1 && !text.contains(crate::sql::DYNAMIC);
    let star = || {
        let t = text.trim_start_matches(|c: char| c.is_whitespace() || c == '(');
        t.get(6..).is_some_and(|r| r.trim_start().starts_with('*'))
    };
    match words.first().map(String::as_str) {
        Some("SELECT") => clauses() || star() || ends_statement(),
        Some("DELETE") => clauses() || ends_statement(),
        Some("WITH") => clauses(),
        Some("INSERT") | Some("REPLACE") => {
            let upper = text.to_ascii_uppercase();
            // `VALUES (…)`, or rows spliced in (`"… values " + string.Join(", ", rows)`).
            let dynamic = crate::sql::DYNAMIC.to_ascii_uppercase();
            let values = upper.match_indices("VALUES").any(|(i, _)| {
                let rest = upper[i + 6..].trim_start();
                rest.starts_with('(') || rest.starts_with(dynamic.as_str())
            });
            // A column list, `INTO t (a, b) VALUES`, with the rows appended later.
            let columns = || {
                let Some(into) = upper.find("INTO ") else { return false };
                let rest = upper[into + 4..].trim_start();
                let name_end = rest.find(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '.' | '`' | '[' | ']' | '"'))).unwrap_or(rest.len());
                name_end > 0 && rest[name_end..].trim_start().starts_with('(')
            };
            (values || (has("VALUES") && columns())) || words.iter().skip(1).any(|w| w == "SELECT")
        }
        Some("UPDATE") => {
            // A qualified or quoted target (`UPDATE shop.orders SET `, built up clause by clause after it), or a
            // column assigned (`SET a = …`).
            let t = text.trim_start_matches(|c: char| c.is_whitespace() || c == '(');
            let target = t.get(6..).and_then(|r| r.split_whitespace().next()).unwrap_or("");
            let qualified = target.contains(['`', '[']) || target.split('.').filter(|p| !p.is_empty()).count() == 2;
            let upper = text.to_ascii_uppercase();
            let assigns = upper.match_indices("SET").any(|(i, _)| {
                let before = upper[..i].chars().last();
                let rest = &upper[i + 3..];
                let col = rest.trim_start();
                let n = col.find(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '.' | '`'))).unwrap_or(col.len());
                !before.is_some_and(|c| c.is_alphanumeric() || c == '_') && rest.starts_with(char::is_whitespace) && n > 0 && col[n..].trim_start().starts_with('=')
            });
            let last_where = words.iter().rposition(|w| w == "WHERE");
            param || qualified || assigns || at("SET").zip(last_where).is_some_and(|(s, w)| s < w)
        }
        Some("TRUNCATE") => words.get(1).is_some_and(|w| w == "TABLE"),
        Some("MERGE") => has("USING"),
        _ => false,
    }
}

/// `EXEC dbo.sp_x @a`, `EXECUTE [sp_x]`, `EXEC @rc = sp_x`: the procedure's name, brackets dropped.
fn exec_procedure(sql: &str) -> Option<String> {
    let t = sql.trim_start();
    let head = t.chars().take_while(|c| c.is_ascii_alphabetic()).collect::<String>().to_ascii_uppercase();
    if head != "EXEC" && head != "EXECUTE" {
        return None;
    }
    let mut rest = t[head.len()..].trim_start();
    let token = |s: &str| s.split(|c: char| c.is_whitespace() || matches!(c, ';' | ',' | '(')).next().unwrap_or("").to_string();
    if rest.starts_with('@') {
        let var = token(rest);
        rest = rest[var.len()..].trim_start().strip_prefix('=')?.trim_start();
    }
    let name = token(rest).replace(['[', ']'], "");
    identifier_like(&name).then_some(name)
}

/// `CALL p(…)`, `call db.p;`, `CALL p` (MySQL): the procedure's name, backticks dropped. Not `Call timed out`: the
/// name ends the text or is followed by `(` or `;`.
fn call_procedure(sql: &str) -> Option<String> {
    let t = sql.trim_start();
    let head: String = t.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
    if !head.eq_ignore_ascii_case("CALL") {
        return None;
    }
    let rest = t[head.len()..].strip_prefix(char::is_whitespace)?.trim_start();
    let end = rest.find(|c: char| c.is_whitespace() || c == '(' || c == ';').unwrap_or(rest.len());
    let name = rest[..end].replace('`', "");
    let after = rest[end..].trim_start();
    (identifier_like(&name) && (after.is_empty() || after.starts_with('(') || after.starts_with(';'))).then_some(name)
}

/// A (possibly dotted) SQL identifier: `sp_x`, `dbo.sp_x`.
fn identifier_like(s: &str) -> bool {
    s.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.')
        && s.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_')
        && !s.ends_with('.')
}

fn has_modifier(src: &str, node: Node, m: &str) -> bool {
    children(node).any(|c| c.kind() == "modifier" && &src[c.byte_range()] == m)
}

/// Attributes among `names` on a declaration, with their arguments' expressions.
fn attribute_args<'t>(src: &str, node: Node<'t>, names: &[&str]) -> Vec<(String, Vec<Node<'t>>)> {
    children(node)
        .filter(|c| c.kind() == "attribute_list")
        .flat_map(children)
        .filter(|a| a.kind() == "attribute")
        .filter_map(|a| {
            let name = base_type_name(src, a.child_by_field_name("name")?);
            let name = name.strip_suffix("Attribute").filter(|s| !s.is_empty()).map_or(name.clone(), str::to_string);
            names.contains(&name.as_str()).then(|| {
                let args = children(a)
                    .filter(|c| c.kind() == "attribute_argument_list")
                    .flat_map(children)
                    .filter(|c| c.kind() == "attribute_argument")
                    .filter_map(|arg| children(arg).filter(|c| c.is_named()).last())
                    .collect();
                (name, args)
            })
        })
        .collect()
}

/// tree-sitter-c-sharp 0.23 mis-parses a doubled quote (`""`) inside a verbatim interpolated string (`$@"…"`,
/// `@$"…"`), and error recovery loses the enclosing class. Those quotes are blanked to two spaces before parsing:
/// the same length, so every offset and line still indexes the original text, and only that string's text differs.
fn blank_verbatim_doubled_quotes(src: &str) -> std::borrow::Cow<'_, str> {
    if !src.contains("$@\"") && !src.contains("@$\"") {
        return std::borrow::Cow::Borrowed(src);
    }
    let mut out = src.as_bytes().to_vec();
    let mut i = 0;
    while i + 2 < out.len() {
        if matches!(&out[i..i + 3], b"$@\"" | b"@$\"") {
            i += 3;
            while i < out.len() {
                if out[i] == b'"' {
                    if out.get(i + 1) == Some(&b'"') {
                        out[i] = b' ';
                        out[i + 1] = b' ';
                        i += 2;
                        continue;
                    }
                    break; // the closing quote
                }
                i += 1;
            }
        }
        i += 1;
    }
    // Only ASCII quotes were replaced with ASCII spaces, so the text is still UTF-8.
    std::borrow::Cow::Owned(String::from_utf8(out).expect("ASCII-for-ASCII replacement keeps UTF-8"))
}

/// Error and missing nodes in a parse tree.
fn parse_errors(node: Node) -> usize {
    if !node.has_error() {
        return 0;
    }
    usize::from(node.is_error() || node.is_missing()) + children(node).map(parse_errors).sum::<usize>()
}

/// `src` with each conditional's first branch kept (`#if X` true) and the rest (`#elif`, `#else`) and the directive
/// lines themselves blanked to spaces: the same length, so every offset and line still indexes the original text.
/// tree-sitter-c-sharp reads a directive only between declarations or statements; one splitting a declaration
/// (`ILogger ForContext(Type t)` then `#if X` a body `#else` a `;` `#endif`) loses the declaration, and error recovery
/// its type's other members. `None` without a conditional.
fn first_branches(src: &str) -> Option<String> {
    if !src.contains("#if") {
        return None;
    }
    let mut out = String::with_capacity(src.len());
    // Per open conditional: whether the branch being read is a later one, blanked.
    let mut open: Vec<bool> = Vec::new();
    for line in src.split_inclusive('\n') {
        let t = line.trim_start();
        let directive = t.strip_prefix('#').map(|d| d.trim_start().split(|c: char| !c.is_ascii_alphabetic()).next().unwrap_or(""));
        let blank = match directive {
            Some("if") => {
                open.push(false);
                true
            }
            Some("elif" | "else") => {
                if let Some(later) = open.last_mut() {
                    *later = true;
                }
                true
            }
            Some("endif") => {
                open.pop();
                true
            }
            _ => open.iter().any(|&later| later),
        };
        if blank {
            // A multi-byte character becomes as many spaces as it has bytes.
            for c in line.chars() {
                if c == '\n' || c == '\r' {
                    out.push(c);
                } else {
                    out.extend(std::iter::repeat_n(' ', c.len_utf8()));
                }
            }
        } else {
            out.push_str(line);
        }
    }
    Some(out)
}

/// The expression of the first argument in an `argument_list`.
fn first_argument(list: Node) -> Option<Node> {
    children(list).find(|c| c.kind() == "argument").and_then(|a| children(a).filter(|c| c.is_named()).last())
}

/// The value of a string literal (regular, verbatim or raw).
fn string_value(src: &str, n: Node) -> Option<String> {
    match n.kind() {
        "string_literal" | "verbatim_string_literal" | "raw_string_literal" => {
            Some(src[n.byte_range()].trim_start_matches('@').trim_matches('"').to_string())
        }
        _ => None,
    }
}

/// Attribute names, without arguments or the `Attribute` suffix: `[Fact]`, `[HttpGet("x")]`.
fn attributes(src: &str, node: Node) -> Vec<String> {
    children(node)
        .filter(|c| c.kind() == "attribute_list")
        .flat_map(children)
        .filter(|a| a.kind() == "attribute")
        .filter_map(|a| a.child_by_field_name("name"))
        .map(|n| {
            let name = base_type_name(src, n);
            name.strip_suffix("Attribute").filter(|s| !s.is_empty()).map_or(name.clone(), str::to_string)
        })
        .collect()
}

/// The verb and template of a commented-out `[HttpPost(nameof(A.Filters))]`, `[HttpGet("x/{id}")]` or `[HttpPut]`
/// line: `Some(("POST", "Filters"))`. `None` for any other line, or a template held in a constant.
fn commented_http(line: &str) -> Option<(String, String)> {
    let l = line.trim_start().trim_start_matches(['/', '*']).trim_start();
    let rest = l.strip_prefix('[')?.trim_start().strip_prefix("Http")?;
    let end = rest.find(|c: char| !c.is_ascii_alphabetic()).unwrap_or(rest.len());
    let verb = &rest[..end];
    if !matches!(verb, "Get" | "Post" | "Put" | "Delete" | "Patch") {
        return None;
    }
    let after = rest[end..].trim_start();
    let template = if let Some(args) = after.strip_prefix('(') {
        let args = args.trim_start();
        if let Some(n) = args.strip_prefix("nameof(") {
            let inner = &n[..n.find(')')?];
            inner.trim().rsplit('.').next()?.to_string()
        } else {
            let q = args.strip_prefix('"')?;
            q[..q.find('"')?].to_string()
        }
    } else if after.starts_with([']', ',']) {
        String::new()
    } else {
        return None;
    };
    Some((verb.to_ascii_uppercase(), template))
}

/// First text line of the `///` comment above a declaration, with XML tags dropped.
fn doc_comment(src: &str, node: Node) -> Option<String> {
    let mut lines = Vec::new();
    let mut prev = node.prev_sibling();
    while let Some(p) = prev.filter(|p| p.kind() == "comment") {
        let t = &src[p.byte_range()];
        if !t.starts_with("///") {
            break;
        }
        lines.push(t);
        prev = p.prev_sibling();
    }
    lines.reverse();
    lines
        .iter()
        .flat_map(|c| c.lines())
        .map(|l| strip_xml(l.trim().trim_start_matches('/').trim()))
        .find(|l| !l.is_empty())
        .map(|l| collapse(&l, 200))
}

fn strip_xml(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RefKind, SymbolKind};

    /// Extraction under the conventions of a made-up code base (see [`crate::conventions::synthetic`]).
    fn extract(source: &str) -> FileFacts {
        crate::conventions::scoped(crate::conventions::synthetic(), || super::extract(source))
    }

    #[test]
    fn in_house_conventions_are_recognised_only_when_configured() {
        let src = "[ShopMessage(ShopHosts.Orders)]\npublic class OrderPlaced { }\n[ShopRequest(\"CartApi\", \"items\", HttpMethods.Get)]\npublic class GetItems { }\npublic class Startup : ShopServiceStartup { public Startup() : base(\"OrdersApi\") { } }\n";
        let plain = crate::conventions::scoped(Default::default(), || super::extract(src));
        assert!(plain.bus.is_empty() && plain.http_routes.is_empty(), "{:?} {:?}", plain.bus, plain.http_routes);
        let configured = extract(src);
        assert_eq!(configured.bus.iter().map(|b| (b.kind, b.queue.as_deref())).collect::<Vec<_>>(), [("message", Some("Orders"))]);
        let routes: Vec<(&str, &str, Option<&str>)> = configured.http_routes.iter().map(|r| (r.side, r.path.as_str(), r.service.as_deref())).collect();
        assert_eq!(routes, [("client", "items", Some("CartApi")), ("service", "", Some("OrdersApi"))]);
    }

    #[test]
    fn plain_masstransit_and_nservicebus_consumers_and_publishes_are_recognised_by_default() {
        let src = "public class A : IConsumer<OrderPlaced> { public Task Consume(ConsumeContext<OrderPlaced> context) => context.Publish(new Shipped()); }\n\
                   public class B : IHandleMessages<Refunded> { public Task Handle(Refunded m, IMessageHandlerContext context) => context.Send(new Notify()); }\n\
                   public class C { IMessageSession session; public Task Go() => session.Publish(new Placed()); }\n";
        let f = crate::conventions::scoped(Default::default(), || super::extract(src));
        let mut got: Vec<(&str, Option<&str>)> = f.bus.iter().map(|b| (b.kind, b.message.as_deref())).collect();
        got.sort();
        assert_eq!(got, [("consume", Some("OrderPlaced")), ("consume", Some("Refunded")), ("publish", Some("Notify")), ("publish", Some("Placed")), ("publish", Some("Shipped"))]);
    }

    const SAMPLE: &str = r#"
using System;
using Acme.Common.Data;

namespace Acme.Cart.Service;

/// <summary>
/// Prices and checks out carts.
/// </summary>
[ApiController]
public partial class CartService : BaseService, ICartService
{
    private readonly ICartRepository _repo;
    private int count = 0, limit = 5;
    public decimal Total { get; set; }
    public static CartService Default => new CartService(null);

    public CartService(ICartRepository repo) : base(repo)
    {
        _repo = repo;
    }

    public async Task<Cart> CheckoutAsync(int id, PaymentRequest request)
    {
        var cart = await _repo.GetAsync(id);
        if (cart == null || cart.Items.Count == 0)
        {
            throw new InvalidOperationException("empty");
        }
        Validate(request);
        request.Card.Charge(cart.Total);
        Logger.Info("done");
        var receipt = new Receipt(cart);
        receipt.Send();
        return cart;
    }

    public class Line { public string Sku { get; init; } }
}

public interface ICartService
{
    Task<Cart> CheckoutAsync(int id, PaymentRequest request);
}

public enum CartState { Open, Closed = 2 }

public record CartDto(int Id, string Name);

public struct Money { public decimal Amount; }

public delegate void CartChanged(Cart cart);

public class CartServiceTests
{
    [Fact]
    public void ChecksOut() { }

    static void Main(string[] args) { }
}
"#;

    #[test]
    fn extracts_csharp_types_members_and_refs() {
        let f = extract(SAMPLE);
        let sym = |name: &str, kind: SymbolKind| {
            f.symbols.iter().find(|s| s.name == name && s.kind == kind).unwrap_or_else(|| panic!("{name} {kind:?}"))
        };

        let class = sym("CartService", SymbolKind::Class);
        assert_eq!(class.doc.as_deref(), Some("Prices and checks out carts."));
        assert_eq!(class.attrs, ["ApiController"]);
        assert!(class.signature.starts_with("[ApiController] public partial class CartService"), "{}", class.signature);
        let targets: Vec<&str> = f.relations.iter().map(|r| r.target.as_str()).collect();
        assert!(targets.contains(&"BaseService") && targets.contains(&"ICartService"), "{targets:?}");

        assert_eq!(sym("_repo", SymbolKind::Property).type_name.as_deref(), Some("ICartRepository"));
        sym("count", SymbolKind::Property);
        sym("limit", SymbolKind::Property);
        assert_eq!(sym("Total", SymbolKind::Property).container.as_deref(), Some("CartService"));
        assert_eq!(sym("Default", SymbolKind::Property).type_name.as_deref(), Some("CartService"));
        assert_eq!(sym("init", SymbolKind::Init).container.as_deref(), Some("CartService"));
        let checkout = f
            .symbols
            .iter()
            .find(|s| s.name == "CheckoutAsync" && s.container.as_deref() == Some("CartService"))
            .unwrap();
        assert_eq!(checkout.kind, SymbolKind::Method);
        // Task<Cart> unwraps to Cart, so callers can chain on the awaited result.
        assert_eq!(checkout.type_name.as_deref(), Some("Cart"));
        // One `if` and one `||`.
        assert_eq!(checkout.complexity, 3);
        assert_eq!(sym("Line", SymbolKind::Class).container.as_deref(), Some("CartService"));
        assert_eq!(sym("Sku", SymbolKind::Property).container.as_deref(), Some("CartService.Line"));

        sym("ICartService", SymbolKind::Protocol);
        assert_eq!(sym("Closed", SymbolKind::Case).container.as_deref(), Some("CartState"));
        sym("CartState", SymbolKind::Enum);
        sym("CartDto", SymbolKind::Class);
        sym("Money", SymbolKind::Struct);
        assert_eq!(sym("Amount", SymbolKind::Property).container.as_deref(), Some("Money"));
        sym("CartChanged", SymbolKind::Typealias);
        assert!(sym("ChecksOut", SymbolKind::Method).tags.contains(&"test"));
        assert!(sym("Main", SymbolKind::Method).tags.contains(&"entrypoint"));

        assert_eq!(f.imports.iter().map(|i| i.module.as_str()).collect::<Vec<_>>(), ["System", "Acme.Common.Data"]);

        let call = |name: &str| f.refs.iter().find(|r| r.name == name && r.kind == RefKind::Call).unwrap_or_else(|| panic!("{name}"));
        // `_repo` is a field: resolved later through the caller's type.
        assert_eq!(call("GetAsync").receiver.as_deref(), Some("_repo"));
        assert!(call("GetAsync").receiver_type.as_deref().unwrap().contains("_repo"));
        assert_eq!(call("Validate").receiver, None);
        // `request` is a typed parameter.
        assert!(call("Charge").receiver_type.as_deref().unwrap().contains("PaymentRequest"));
        assert_eq!(call("Info").receiver_type.as_deref(), Some("Logger"));
        // `receipt` is typed from `new Receipt(...)`.
        assert_eq!(call("Send").receiver_type.as_deref(), Some("Receipt"));
        let inst: Vec<&str> = f.refs.iter().filter(|r| r.kind == RefKind::Instantiate).map(|r| r.name.as_str()).collect();
        assert!(inst.contains(&"Receipt") && inst.contains(&"InvalidOperationException") && inst.contains(&"CartService"), "{inst:?}");
        let type_refs: Vec<&str> = f.refs.iter().filter(|r| r.kind == RefKind::TypeRef).map(|r| r.name.as_str()).collect();
        assert!(type_refs.contains(&"PaymentRequest") && type_refs.contains(&"Cart"), "{type_refs:?}");
        assert!(!type_refs.contains(&"Task"), "{type_refs:?}");
        assert!(f.refs.iter().any(|r| r.name == "Items" && r.kind == RefKind::Member));
        assert_eq!(f.parse_errors, 0);
    }

    #[test]
    fn extension_methods_belong_to_the_extended_type() {
        let f = extract(
            "public static class ResultExtensions\n{\n    public static void ThrowIfError(this ApiResult response) { }\n    public static T Unwrap<T>(this IResult<T> r) => r.Value;\n    public static void Helper(int x) { }\n}\n",
        );
        let m = |name: &str| f.symbols.iter().find(|s| s.name == name).unwrap_or_else(|| panic!("{name}"));
        assert_eq!((m("ThrowIfError").kind, m("ThrowIfError").container.as_deref()), (SymbolKind::Method, Some("ApiResult")));
        assert_eq!(m("Unwrap").container.as_deref(), Some("IResult"));
        // The extension lives in the static class, which still contains it.
        assert_eq!(f.symbols[m("ThrowIfError").parent.unwrap()].name, "ResultExtensions");
        assert_eq!(m("Helper").container.as_deref(), Some("ResultExtensions"));
    }

    #[test]
    fn a_conditional_splitting_a_declaration_keeps_the_types_members() {
        let src = "namespace Shelf;\npublic interface IShelf\n{\n    IShelf Narrow(Type t)\n#if DEFAULT_BODIES\n        => Narrow(t, 1)\n#endif\n    ;\n\
                   \n    IShelf Narrow<T>()\n#if DEFAULT_BODIES\n    {\n        return Narrow(typeof(T));\n    }\n#else\n        ;\n#endif\n\
                   \n    IShelf Narrow(Type t, int depth);\n}\n";
        let f = extract(src);
        let members: Vec<(&str, SymbolKind, Option<&str>, u32)> =
            f.symbols.iter().filter(|s| s.name == "Narrow").map(|s| (s.name.as_str(), s.kind, s.container.as_deref(), s.start_line)).collect();
        assert_eq!(
            members,
            [
                ("Narrow", SymbolKind::Method, Some("IShelf"), 4),
                ("Narrow", SymbolKind::Method, Some("IShelf"), 10),
                ("Narrow", SymbolKind::Method, Some("IShelf"), 19)
            ],
            "{:?}",
            f.symbols.iter().map(|s| (&s.name, s.kind, &s.container, s.start_line)).collect::<Vec<_>>()
        );
        // The kept branch's call is at its own line; the text is the file's, offsets unchanged.
        assert!(f.refs.iter().any(|r| r.name == "Narrow" && r.line == 13 && r.kind == RefKind::Call));
        // A file whose conditionals parse is read as it is: both branches.
        let both = extract("public class C\n{\n#if A\n    void One() { }\n#else\n    void Two() { }\n#endif\n}\n");
        assert_eq!(both.symbols.iter().filter(|s| s.kind == SymbolKind::Method).count(), 2);
        let blanked = first_branches("a\n#if X\nb\n#elif Y\nc é\n#else\nd\n#endif\ne\n").unwrap();
        assert_eq!(blanked.len(), "a\n#if X\nb\n#elif Y\nc é\n#else\nd\n#endif\ne\n".len());
        assert_eq!(blanked.lines().map(str::trim).collect::<Vec<_>>(), ["a", "", "b", "", "", "", "", "", "e"]);
    }

    #[test]
    fn a_constrained_type_parameter_has_its_constraints_members() {
        let f = extract(
            r#"
public class Catalog<TEntry> where TEntry : Entry
{
    public void Load(TEntry e) => e.Attach();
    public T Make<T, TArg>(TArg arg, IList<T> all) where T : Item, new() where TArg : class, notnull, IArgs
    {
        var item = new T { Name = "x" };
        item.Attach(arg);
        all.First().Detach();
        arg.Read();
        return item;
    }
    public void Both<T>(T x) where T : Item, IOther => x.Attach();
    public void Bare<T>(T x) => x.Attach();
    public void Chained<T, U>(U u) where T : Item where U : T => u.Attach();
}
"#,
        );
        let rt = |name: &str, line: u32| {
            f.refs.iter().find(|r| r.name == name && r.line == line && r.kind == RefKind::Call).unwrap_or_else(|| panic!("{name}:{line}")).receiver_type.clone()
        };
        // The class's own type parameter, a local made from a method's, and one beside `class` and `notnull`.
        assert_eq!(rt("Attach", 4), Some("Entry".into()));
        assert_eq!(rt("Attach", 8), Some("Item".into()));
        assert_eq!(rt("Read", 10), Some("IArgs".into()));
        // Inside another type: the element of a list of `T` is an `Item`.
        assert!(rt("Detach", 9).unwrap().contains("Item"), "{:?}", rt("Detach", 9));
        // Several type constraints, none, or another type parameter: the member may be any one's, so left as it is.
        assert_eq!(rt("Attach", 13), Some("T".into()));
        assert_eq!(rt("Attach", 14), Some("T".into()));
        assert_eq!(rt("Attach", 15), Some("U".into()));
    }

    #[test]
    fn a_constructor_initializer_is_at_its_keyword_not_the_colon() {
        let f = extract(
            "public class One : Base\n{\n    public One(long z) :\n        base((int)z)\n    {\n    }\n    public One(int x)\n        : this(\n            x, 1)\n    {\n    }\n    public One(string s) : base(1) { }\n}\n",
        );
        let inits: Vec<(&str, u32, u32)> =
            f.refs.iter().filter(|r| r.name == "init").map(|r| (r.receiver.as_deref().unwrap_or(""), r.line, r.col)).collect();
        // The colon ending line 3 is not the call: `base` on line 4 is.
        assert_eq!(inits, vec![("base", 4, 9), ("this", 8, 11), ("base", 12, 28)]);
    }

    #[test]
    fn lambda_parameters_take_the_element_or_mocked_type() {
        let f = extract(
            r#"
public class Tests
{
    private readonly Mock<IGateway> _gateway = new();
    public void Run(List<Order> orders)
    {
        var open = orders.Where(o => o.IsOpen()).Select((x) => x.Total());
        _gateway.Verify(g => g.SendAsync(1), Times.Once);
        _gateway.Setup(g => g.GetAsync()).ReturnsAsync(null);
        Helper(h => h.Unknown());
    }
}
"#,
        );
        // A mock stands in for its type.
        assert_eq!(f.symbols.iter().find(|s| s.name == "_gateway").unwrap().type_name.as_deref(), Some("IGateway"));
        let rt = |name: &str| {
            f.refs.iter().find(|r| r.name == name && r.kind == RefKind::Call).unwrap_or_else(|| panic!("{name}")).receiver_type.clone()
        };
        let elem = |t: TypeExpr| Some(TypeExpr::Elem(Box::new(t)).encode());
        assert_eq!(rt("IsOpen"), elem(TypeExpr::Array(Box::new(TypeExpr::Named("Order".into())))));
        // `Select` is called on `Where(...)`'s result, whose type is a call result resolved later.
        assert!(rt("Total").unwrap().contains("elem"), "{:?}", rt("Total"));
        assert!(rt("SendAsync").unwrap().contains("_gateway"), "{:?}", rt("SendAsync"));
        assert!(rt("GetAsync").unwrap().contains("_gateway"), "{:?}", rt("GetAsync"));
        // A lambda passed to an unknown method gets no element type.
        assert!(!rt("Unknown").unwrap_or_default().contains("elem"));
    }

    #[test]
    fn framework_lambdas_take_their_conventional_types() {
        let f = extract(
            r#"
public class Profiles : Profile
{
    public Profiles()
    {
        CreateMap<OrderEntity, OrderDto>()
            .ForMember(d => d.Total, o => o.MapFrom(s => s.Amount))
            .ReverseMap()
            .ForMember(r => r.Amount, o => o.Ignore());
    }
}
public class OrderValidator : AbstractValidator<CreateOrder>
{
    public OrderValidator() { RuleFor(x => x.Sku).NotEmpty().When(y => y.Active); }
}
public class Context : DbContext
{
    public DbSet<Order> Orders { get; set; }
    protected override void OnModelCreating(ModelBuilder b)
    {
        b.Entity<Order>(e => { e.HasKey(k => k.Id); e.HasOne(o => o.Customer).WithMany(c => c.Orders); });
    }
    public Order Recent(AutoMocker mocker)
    {
        mocker.GetMock<IClock>().Setup(m => m.Now);
        var first = this.Orders.Where(p => p.Paid).First();
        return Set<Line>().Single(l => l.Qty > 0).Order;
    }
    private Order _last;
    public Order Last { get => _last; set { _last = value.Fresh; } }
}
"#,
        );
        let rt = |name: &str| {
            f.refs.iter().find(|r| r.name == name && r.kind == RefKind::Member).unwrap_or_else(|| panic!("{name}")).receiver_type.clone()
        };
        let named = |n: &str| Some(n.to_string());
        // AutoMapper: the destination, the source in `MapFrom`, and swapped after `ReverseMap`.
        assert_eq!(rt("Total"), named("OrderDto"));
        assert_eq!(f.refs.iter().filter(|r| r.name == "Amount").map(|r| r.receiver_type.clone()).collect::<Vec<_>>(), [named("OrderEntity"), named("OrderEntity")]);
        // FluentValidation: the validated type.
        assert_eq!(rt("Sku"), named("CreateOrder"));
        assert_eq!(rt("Active"), named("CreateOrder"));
        // An entity builder's lambdas take the entity; a lambda further down the chain doesn't.
        assert_eq!(rt("Id"), named("Order"));
        assert_eq!(rt("Customer"), named("Order"));
        assert_ne!(rt("Orders"), named("Order"));
        // A mock from a generic factory stands in for its type argument.
        assert_eq!(rt("Now"), named("IClock"));
        // A `DbSet<T>` and `Set<T>()` are sequences of `T`.
        let order = TypeExpr::Array(Box::new(TypeExpr::Named("Order".into())));
        assert_eq!(f.symbols.iter().find(|s| s.name == "Orders").unwrap().type_name, Some(order.encode()));
        assert!(rt("Paid").unwrap().contains("elem"));
        assert_eq!(rt("Qty"), Some(TypeExpr::Elem(Box::new(TypeExpr::Array(Box::new(TypeExpr::Named("Line".into()))))).encode()));
        // A setter's `value` is of the property's type.
        assert_eq!(rt("Fresh"), named("Order"));
    }

    #[test]
    fn base_types_are_type_references_from_the_subclass() {
        let f = extract("public class MxRecords : DnsRecordBase<MxRecord>, IRecordView { }\n");
        let refs: Vec<(&str, Option<&str>)> = f
            .refs
            .iter()
            .filter(|r| r.kind == RefKind::TypeRef)
            .map(|r| (r.name.as_str(), r.from.map(|i| f.symbols[i].name.as_str())))
            .collect();
        for name in ["DnsRecordBase", "MxRecord", "IRecordView"] {
            assert!(refs.contains(&(name, Some("MxRecords"))), "{name}: {refs:?}");
        }
    }

    #[test]
    fn controller_actions_are_server_routes() {
        let f = extract(
            r#"
public static class Routes { public const string Legacy = "legacy/find"; }

[Authorize("Api")]
public class SearchController : Controller
{
    [HttpPost(nameof(Filters))]
    public Task Filters([FromBody] FilterQuery q) => null;

    [HttpGet(nameof(Models.Domains.Contracts.GetDomainHistory))]
    public Task GetDomainHistory() => null;

    [HttpPost("Search")]
    public Task Search() => null;

    [HttpGet]
    public Task Index() => null;

    [HttpGet(Routes.Legacy)]
    public Task Legacy() => null;

    public void NotAnAction() { }
}

[ApiController]
[Route("api/[controller]")]
public class OrdersController : ControllerBase
{
    [HttpDelete("{id}")]
    public Task Delete(int id) => null;

    [HttpPut]
    [Route("[action]/all")]
    public Task Refresh() => null;

    [HttpGet("/health")]
    public Task Health() => null;
}

[Route(nameof(Waitlist))]
public class AccountController : Controller
{
    [HttpPost(nameof(ConfirmIdentity))]
    public Task ConfirmIdentity() => null;
}

[Route($"{nameof(Infra)}/Group")]
public class GroupController : Controller
{
    [HttpPost(nameof(AddRolesToGroup))]
    public Task AddRolesToGroup() => null;

    [HttpGet($"Find/{Version}")]
    public Task Find() => null;
}
"#,
        );
        let routes: Vec<(&str, &str, &str, &str, Option<&str>)> = f
            .http_routes
            .iter()
            .map(|r| (f.symbols[r.sym.unwrap()].name.as_str(), r.side, r.verb.as_str(), r.path.as_str(), r.path_ref.as_deref()))
            .collect();
        assert_eq!(
            routes,
            [
                ("Filters", "server", "POST", "Filters", None),
                ("GetDomainHistory", "server", "GET", "GetDomainHistory", None),
                ("Search", "server", "POST", "Search", None),
                // No template and no prefix: conventional routes may place it (see `kc_store::query`).
                ("Index", "server", "GET", "{^!Index}", None),
                ("Legacy", "server", "GET", "", Some("Routes.Legacy")),
                // A public method with no route attribute: an action only under conventional routes.
                ("NotAnAction", "server", "*", "{^!*NotAnAction}", None),
                ("OrdersController", "prefix", "", "api/[controller]", None),
                ("Delete", "server", "DELETE", "api/Orders/{id}", None),
                ("Refresh", "server", "PUT", "api/Orders/Refresh/all", None),
                // A template starting with `/` ignores the controller's prefix.
                ("Health", "server", "GET", "health", None),
                ("AccountController", "prefix", "", "Waitlist", None),
                ("ConfirmIdentity", "server", "POST", "Waitlist/ConfirmIdentity", None),
                ("GroupController", "prefix", "", "Infra/Group", None),
                ("AddRolesToGroup", "server", "POST", "Infra/Group/AddRolesToGroup", None),
                // `Find`'s template interpolates something other than `nameof`: unknown, so no route.
            ]
        );
        assert_eq!(f.constants.iter().map(|c| (c.qualified.as_str(), c.value.as_str())).collect::<Vec<_>>(), [("Routes.Legacy", "legacy/find")]);
    }

    /// A controller with no `[Route]` of its own leaves its prefix to the query (`{^Action}`; `{^!Action}` when it
    /// isn't partial, so no other part has one either): a base controller's
    /// `[Route]` (recorded as a `prefix`), or conventional routes (`conventional`), which also serve public methods
    /// with no route attribute (`{^*Action}`). `[Area]`, `[ActionName]` and `[NonAction]` are read.
    #[test]
    fn inherited_and_conventional_routes_are_left_to_the_query() {
        let f = extract(
            r#"
[ApiController]
[Route("api/[controller]")]
public abstract class ApiControllerBase : ControllerBase { }

public class ItemsController : ApiControllerBase
{
    [HttpGet("Latest")]
    public Task Latest() => null;
    [HttpGet]
    public Task List() => null;
    [ActionName("Find")]
    [HttpGet("[action]/{id}")]
    public Task Lookup(int id) => null;
    [NonAction]
    [HttpGet("x")]
    public void Helper() { }
}

[Area("Admin")]
[Route("[area]/[controller]")]
public class UsersController : Controller
{
    [HttpGet]
    public Task Get() => null;
}

public class HomeController : Controller
{
    public IActionResult Index() => null;
    [HttpPost]
    public IActionResult Save() => null;
    [Route("about")]
    public IActionResult About() => null;
    private void Hidden() { }
    public static void Util() { }
}

public partial class CartController : Controller
{
    [HttpGet]
    public Task Show() => null;
}

public static class Startup
{
    public static void Configure(WebApplication app)
    {
        app.MapControllerRoute(name: "default", pattern: "{controller=Home}/{action=Index}/{id?}");
        app.MapControllerRoute("areas", "{area:exists}/{controller=Home}/{action=Index}");
    }
}
"#,
        );
        let routes: Vec<(&str, &str, &str, &str)> =
            f.http_routes.iter().map(|r| (r.sym.map_or("-", |s| f.symbols[s].name.as_str()), r.side, r.verb.as_str(), r.path.as_str())).collect();
        assert_eq!(
            routes,
            [
                ("ApiControllerBase", "prefix", "", "api/[controller]"),
                ("Latest", "server", "GET", "{^!Latest}/Latest"),
                ("List", "server", "GET", "{^!List}"),
                ("Lookup", "server", "GET", "{^!Find}/Find/{id}"),
                ("UsersController", "prefix", "", "Admin/[controller]"),
                ("Get", "server", "GET", "Admin/Users"),
                ("Index", "server", "*", "{^!*Index}"),
                ("Save", "server", "POST", "{^!Save}"),
                ("About", "server", "*", "about"),
                // Another part of a partial class may have the `[Route]`: the query looks at the class's own name too.
                ("Show", "server", "GET", "{^Show}"),
                ("Configure", "conventional", "MapControllerRoute", "{controller=Home}/{action=Index}/{id?}"),
                // An area template needs the area: not placed.
                ("Configure", "unsupported", "MapControllerRoute", "conventional routes: a template filled from controller and action names, which KnowCode doesn't expand"),
            ]
        );
    }

    /// `[Route("")]` on a controller is its prefix, an empty one: its actions sit at the root, not under the base
    /// controller's `[Route("[controller]")]`.
    #[test]
    fn an_empty_route_on_a_controller_replaces_the_inherited_prefix() {
        let f = extract(
            r#"
[Route("[controller]")]
public abstract class GadgetControllerBase : ControllerBase { }

[Route("")]
public class WidgetShelfController : GadgetControllerBase
{
    [HttpGet("Widgets/{widgetId}")]
    public Task GetWidget(Guid widgetId) => null;
}

public class SprocketController : GadgetControllerBase
{
    [HttpGet("Latest")]
    public Task Latest() => null;
}
"#,
        );
        let routes: Vec<(&str, &str, &str, &str)> =
            f.http_routes.iter().map(|r| (r.sym.map_or("-", |s| f.symbols[s].name.as_str()), r.side, r.verb.as_str(), r.path.as_str())).collect();
        assert_eq!(
            routes,
            [
                ("GadgetControllerBase", "prefix", "", "[controller]"),
                ("WidgetShelfController", "prefix", "", ""),
                ("GetWidget", "server", "GET", "Widgets/{widgetId}"),
                ("Latest", "server", "GET", "{^!Latest}/Latest"),
            ]
        );
    }

    /// `new Uri(base, "path")` composes a URL; RestSharp's `RestRequest` names a path and a verb; a URL known only at
    /// run time is kept with no path.
    #[test]
    fn uri_composition_and_rest_requests_are_client_routes() {
        let f = extract(
            r#"
public class CatalogClient(HttpClient http, RestClient rest)
{
    public Task Items() => http.GetAsync(new Uri(_baseUri, "api/catalog/items"));
    public Task Brand(int id) => rest.ExecuteAsync(new RestRequest($"api/catalog/brands/{id}", Method.Delete));
    public Task Plain() => rest.ExecuteAsync(new RestRequest("api/catalog/types"));
    public Task Anywhere(string url) => http.GetAsync(url);
}
"#,
        );
        let routes: Vec<(&str, &str, &str)> =
            f.http_routes.iter().map(|r| (f.symbols[r.sym.unwrap()].name.as_str(), r.verb.as_str(), r.path.as_str())).collect();
        assert_eq!(
            routes,
            [("Items", "GET", "/api/catalog/items"), ("Brand", "DELETE", "api/catalog/brands/{}"), ("Plain", "GET", "api/catalog/types"), ("Anywhere", "GET", "")]
        );
    }

    #[test]
    fn commented_out_actions_are_commented_routes() {
        let f = extract(
            r#"
[Route("api/[controller]")]
public class OrdersController : Controller
{
    //[HttpPost(nameof(Models.DeleteOrder))]
    //public Task DeleteOrder() => null;

    [HttpGet("{id}")]
    // [HttpPut("[action]")]
    public Task Get(int id) => null;

    /*
    [HttpDelete("/legacy/{id}")]
    public Task Legacy(int id) => null;
    */

    // [HttpGet(Routes.Old)] has a constant; [Authorize] isn't a route; neither is a word in a sentence:
    // Use [HttpPost] for writes.
    public void Helper() { }
}
"#,
        );
        let routes: Vec<(&str, &str, &str, &str)> = f
            .http_routes
            .iter()
            .filter(|r| r.side == "commented")
            .map(|r| (f.symbols[r.sym.unwrap()].name.as_str(), r.side, r.verb.as_str(), r.path.as_str()))
            .collect();
        assert_eq!(
            routes,
            [
                ("OrdersController", "commented", "POST", "api/Orders/DeleteOrder"),
                ("Get", "commented", "PUT", "api/Orders/Get"),
                ("OrdersController", "commented", "DELETE", "legacy/{id}"),
            ]
        );
    }

    #[test]
    fn service_dtos_are_client_routes_and_startups_register_services() {
        let f = extract(
            r#"
public static class ServiceNames { public const string ApiName = "MailingApi"; }

[ShopRequest(ServiceNames.ApiName, nameof(DeleteSubscriber), HttpMethods.Delete)]
public class DeleteSubscriber { public int Id { get; set; } }

[ShopRequest(nameof(PermissionApi), nameof(Models.GetRoles), HttpMethods.Get)]
public record GetRoles(int AccountId);

[ShopRequest("PartnerApi", "Payouts/Find", HttpMethods.Post)]
public class FindPayouts { }

[ShopRequest(nameof(AuthApi), "Clients/" + nameof(GetClient), HttpMethods.Get)]
public class GetClient { }

public class Startup(IWebHostEnvironment env) : ShopServiceStartup<Settings>(ServiceNames.ApiName, Program.Port, env) { }

public class LegacyStartup : ShopServiceStartup
{
    public LegacyStartup(IWebHostEnvironment env) : base("LegacyApi", 5000, env) { }
}

public class NotAStartup : Base
{
    public NotAStartup() : base("x") { }
}
"#,
        );
        type Row<'a> = (&'a str, &'a str, &'a str, &'a str, Option<&'a str>, Option<&'a str>, Option<&'a str>);
        let routes: Vec<Row> = f
            .http_routes
            .iter()
            .map(|r| {
                (f.symbols[r.sym.unwrap()].name.as_str(), r.side, r.verb.as_str(), r.path.as_str(), r.path_ref.as_deref(), r.service.as_deref(), r.service_ref.as_deref())
            })
            .collect();
        assert_eq!(
            routes,
            [
                ("DeleteSubscriber", "client", "DELETE", "DeleteSubscriber", None, None, Some("ServiceNames.ApiName")),
                ("GetRoles", "client", "GET", "GetRoles", None, Some("PermissionApi"), None),
                ("FindPayouts", "client", "POST", "Payouts/Find", None, Some("PartnerApi"), None),
                ("GetClient", "client", "GET", "Clients/GetClient", None, Some("AuthApi"), None),
                // A registration: the service name, literal or a constant to resolve.
                ("Startup", "service", "", "", None, None, Some("ServiceNames.ApiName")),
                ("LegacyStartup", "service", "", "", None, Some("LegacyApi"), None),
            ]
        );
    }

    #[test]
    fn gateway_and_http_client_calls_are_client_routes_of_their_method() {
        let f = extract(
            r#"
public static class ServiceNames { public const string Inventory = nameof(Inventory); }

public class Orders
{
    private const string PaymentApi = "PaymentApi";
    public async Task Run(int id, string service, Uri baseUri)
    {
        await gateway.CallServiceAsync(ServiceNames.Inventory, nameof(GetDomain), HttpMethod.Get);
        await gateway.CallServiceAsync(PaymentApi, $"paymentGroups/{id}/payments", HttpMethod.Post, body);
        await gateway.CallService(nameof(BillingApi), "purchaseOrders", HttpMethod.Put);
        gateway.SendToApiAsync(HttpMethod.Delete, "CartApi", "items", dto);
        // Unknowable: a path in a parameter, or in a member of one.
        await gateway.CallServiceAsync(service, endpoint, HttpMethod.Get);
        await gateway.CallServiceAsync(call.Service, call.Operation, HttpMethod.Get);
        // An address is not a service.
        await gateway.CallServiceAsync("https://h.example", "v0/verify", HttpMethod.Post);
        await _http.GetFromJsonAsync<Order>($"{_baseUrl}/api/orders/{id}?full=true");
        await httpClient.PostAsync(ServiceNames.Inventory + "/x", content);
        using var req = new HttpRequestMessage(HttpMethod.Patch, "v1/auth/token");
        // Not HTTP: a cache's key, a repository's lookup.
        await cache.GetStringAsync($"{id}_key");
        await repo.GetAsync("a/b");
    }
}
"#,
        );
        type Row<'a> = (&'a str, &'a str, &'a str, Option<&'a str>, Option<&'a str>);
        let routes: Vec<Row> = f
            .http_routes
            .iter()
            .map(|r| (f.symbols[r.sym.unwrap()].name.as_str(), r.verb.as_str(), r.path.as_str(), r.service.as_deref(), r.service_ref.as_deref()))
            .collect();
        assert_eq!(
            routes,
            [
                ("Run", "GET", "GetDomain", None, Some("ServiceNames.Inventory")),
                ("Run", "POST", "paymentGroups/{}/payments", None, Some("PaymentApi")),
                ("Run", "PUT", "purchaseOrders", Some("BillingApi"), None),
                ("Run", "DELETE", "items", Some("CartApi"), None),
                ("Run", "POST", "v0/verify", None, None),
                ("Run", "GET", "/api/orders/{}?full=true", None, None),
                ("Run", "POST", "{=ServiceNames.Inventory}/x", None, None),
                ("Run", "PATCH", "v1/auth/token", None, None),
            ]
        );
        // A constant written with `nameof` is a constant too.
        assert!(f.constants.iter().any(|c| c.qualified == "ServiceNames.Inventory" && c.value == "Inventory"));
    }

    #[test]
    fn doubled_quotes_in_verbatim_interpolated_strings_parse() {
        // tree-sitter-c-sharp 0.23 mis-parses `""` inside `$@"…"` and loses the enclosing class.
        let src = "public class DataController : Controller\n{\n    [HttpGet(\"Export\")]\n    public void Export()\n    {\n        var row = $@\"{r.Id},\"\"{r.Name}\"\"\";\n        var other = @$\"\"\"{x}\"\"\";\n    }\n}\n";
        let f = extract(src);
        assert_eq!(f.parse_errors, 0);
        let m = f.symbols.iter().find(|s| s.name == "Export").unwrap();
        assert_eq!((m.kind, m.container.as_deref()), (SymbolKind::Method, Some("DataController")));
        assert_eq!(f.http_routes.len(), 1);
        // Offsets index the original text.
        assert!(src[m.start_byte as usize..].starts_with("[HttpGet"));
    }

    #[test]
    fn entity_framework_sets_tables_and_procedures() {
        let f = extract(
            r#"
public class ShopContext : DbContext
{
    public DbSet<Models.Order> Orders { get; set; }
    public virtual DbSet<Customer> Customers { get; set; }
    public DbSet<Invoice> Invoices => Set<Invoice>();
    public DbSet<Note> Notes { get; set; }

    protected override void OnModelCreating(ModelBuilder modelBuilder)
    {
        modelBuilder.Entity<Models.Order>(a =>
        {
            a.ToTable("shop_order");
            a.Property(b => b.Id).HasColumnName("order_id");
        });
        modelBuilder.Entity<Customer>().HasKey(c => c.Id).ToTable("customer", "sales");
        var inv = modelBuilder.Entity<Invoice>();
        inv.ToTable("invoice", schema: "billing");
        modelBuilder.ApplyConfiguration(new LineConfig());
    }
}

public class LineConfig : IEntityTypeConfiguration<Line>
{
    public void Configure(EntityTypeBuilder<Line> builder) => builder.ToTable("order_line");
}

public class PaymentMap : EntityTypeConfiguration<Payment>
{
    public PaymentMap() { ToTable("payment"); }
}

[Table("note", Schema = "crm")]
public class Note { }

public class Repo
{
    public void Run(ShopContext db, IDbConnection conn)
    {
        db.Set<Customer>().Add(new Customer());
        db.Orders.FromSqlRaw("EXEC dbo.usp_orders @p0", 1);
        db.Database.ExecuteSqlInterpolated($"EXECUTE [usp_purge] {1}");
        conn.Query<Order>("usp_find", new { }, commandType: CommandType.StoredProcedure);
        conn.Query<Order>("select * from x");
        var cmd = new SqlCommand { CommandText = "usp_cmd", CommandType = CommandType.StoredProcedure };
        db.Database.ExecuteSqlRawAsync("call p_rebuild");
        var sql = $"CALL p_find('{string.Join(',', ids)}')";
        _logger.LogInformation("Call p_not");
        conn.Execute("CALL timed out");
        q = "CALL p_assign;";
        db.RunQuery("CALL p_concat(" + id + ")");
    }
}
"#,
        );
        assert_eq!(f.parse_errors, 0);
        let facts: Vec<(&str, Option<&str>, &str, &str)> = f
            .db
            .iter()
            .map(|d| (d.kind, d.sym.map(|i| f.symbols[i].name.as_str()), d.entity.as_str(), d.name.as_str()))
            .collect();
        assert_eq!(
            facts,
            [
                ("dbset", Some("Orders"), "Order", "Orders"),
                ("dbset", Some("Customers"), "Customer", "Customers"),
                ("dbset", Some("Invoices"), "Invoice", "Invoices"),
                ("set", Some("Invoices"), "Invoice", ""),
                ("dbset", Some("Notes"), "Note", "Notes"),
                ("table", Some("OnModelCreating"), "Order", "shop_order"),
                ("table", Some("OnModelCreating"), "Customer", "sales.customer"),
                ("table", Some("OnModelCreating"), "Invoice", "billing.invoice"),
                ("table", Some("Configure"), "Line", "order_line"),
                ("table", Some("init"), "Payment", "payment"),
                ("table", Some("Note"), "Note", "crm.note"),
                ("set", Some("Run"), "Customer", ""),
                ("procedure", Some("Run"), "", "dbo.usp_orders"),
                ("procedure", Some("Run"), "", "usp_purge"),
                ("procedure", Some("Run"), "", "usp_find"),
                // SQL text naming a table.
                ("sql_read", Some("Run"), "", "x"),
                ("procedure", Some("Run"), "", "usp_cmd"),
                ("procedure", Some("Run"), "", "p_rebuild"),
                ("procedure", Some("Run"), "", "p_find"),
                ("procedure", Some("Run"), "", "p_assign"),
                ("procedure", Some("Run"), "", "p_concat"),
            ]
        );
        let g = extract(
            "class R { void M() { var q = from o in _db.Orders join c in _db.Customers on o.C equals c.Id select o; IQueryable<Note> n = _db.Notes; var x = _db.Other; } }",
        );
        let queried: Vec<&str> = g.db.iter().filter(|d| d.kind == "query").map(|d| d.name.as_str()).collect();
        assert_eq!(queried, ["Orders", "Customers", "Notes"]);
        // `a` builds Order only inside its lambda.
        assert!(!f.db.iter().any(|d| d.kind == "table" && d.name == "order_id"));
    }

    #[test]
    fn includes_connections_and_sql_text() {
        let f = extract(
            r#"
class Startup {
    void ConfigureServices(IServiceCollection s) {
        s.AddDbContext<ShopContext>(o => o.UseMySql(AppSettings.ShopDbConnection, ServerVersion.AutoDetect(AppSettings.ShopDbConnection)));
        s.AddDbContext<IAuditContext, AuditContext>(o => o.UseSqlServer(Configuration.GetConnectionString("Audit")).UseLazyLoadingProxies());
    }
}
class NotesContext : DbContext {
    protected override void OnConfiguring(DbContextOptionsBuilder b) => b.UseMySql("Server=x;Database=notes;Uid=u", v);
    protected override void OnModelCreating(ModelBuilder m) {
        m.Entity<Order>().Navigation(o => o.Lines).AutoInclude();
        m.Entity<Order>().Navigation(o => o.Notes).AutoInclude(false);
    }
}
class Repo {
    const string ByIdSql = "SELECT * FROM orders WHERE id = @id";
    string Label = "Select a plan from the list";
    Order Get(int id) => _db.Orders.Include(o => o.Lines.Where(l => l.Open)).ThenInclude(l => l.Product).Include("Customer.Address").First();
    void Save(Order o) {
        _conn.Execute($"UPDATE shop.orders SET total = @t WHERE id = {o.Id}", o);
        _conn.Execute("insert into order_log (id) " +
            "select id from orders where id = " + o.Id);
        var sql = @"REPLACE INTO `audit` (a, ""b"") VALUES (1)";
        _db.Database.ExecuteSqlRaw("DELETE FROM " + table + " WHERE x = 1");
        _db.Database.ExecuteSqlRaw(string.Format("TRUNCATE TABLE {0}", "t"));
    }
}
"#,
        );
        assert_eq!(f.parse_errors, 0);
        let facts: Vec<(&str, Option<&str>, &str, &str, u32)> = f
            .db
            .iter()
            .map(|d| (d.kind, d.sym.map(|i| f.symbols[i].name.as_str()), d.entity.as_str(), d.name.as_str(), d.line))
            .collect();
        assert_eq!(
            facts,
            [
                ("connection", Some("ConfigureServices"), "ShopContext", "ShopDbConnection", 4),
                ("connection", Some("ConfigureServices"), "AuditContext", "Audit", 5),
                ("lazy_loading", Some("ConfigureServices"), "", "", 5),
                ("connection", Some("OnConfiguring"), "NotesContext", "Database=notes", 9),
                ("auto_include", Some("OnModelCreating"), "Order", "Lines", 11),
                // The constant's value: its users run it (none here).
                ("sql_read", Some("ByIdSql"), "held", "orders", 16),
                // The outermost call of a chain first.
                ("include", Some("Get"), "", "Customer", 18),
                ("include", Some("Get"), "", "Address", 18),
                ("include", Some("Get"), "", "Product", 18),
                ("include", Some("Get"), "", "Lines", 18),
                // A member named in a query lambda (a navigation there is a join).
                ("lambda_member", Some("Get"), "", "Open", 18),
                ("sql_write", Some("Save"), "", "shop.orders", 20),
                ("sql_write", Some("Save"), "", "order_log", 21),
                ("sql_read", Some("Save"), "", "orders", 22),
                ("sql_write", Some("Save"), "", "audit", 23),
            ]
        );
    }

    fn sql_facts(src: &str) -> Vec<(&'static str, String, String, String, u32)> {
        let f = extract(src);
        assert_eq!(f.parse_errors, 0);
        f.db.iter()
            .filter(|d| d.kind.starts_with("sql_"))
            .map(|d| (d.kind, d.sym.map_or(String::new(), |i| f.symbols[i].name.clone()), d.entity.clone(), d.name.clone(), d.line))
            .collect()
    }

    #[test]
    fn sql_text_needs_sql_detail_by_its_verb() {
        let facts = sql_facts(
            r#"
class Report {
    void Write(StringBuilder sb, int days) {
        sb.AppendLine("Delete from server after " + days + " days");
        Console.WriteLine(days > 0 ? "Delete from server after " + days + " days;" : "Keep?");
        var help = "Select a plan from the list; then update the cart set by your admin";
        var wmi = new ObjectQuery("select * from Win32_Process");
        var s = new ManagementObjectSearcher("SELECT Name FROM Win32_Service WHERE State = 'Running'");
        var t = "SELECT * FROM t1";
        var u = "UPDATE t2 SET a = 1 WHERE id = 2";
        var v = "Update the list set by hand";
        var w = "INSERT INTO t3 (a) VALUES (1)";
        var x = "Insert into the form " + days + " values";
        var y = "DELETE FROM t4 WHERE id = " + days;
        var z = "SELECT j.id FROM t5 JOIN JSON_TABLE(t5.doc, '$[*]' COLUMNS (id INT PATH '$')) j";
        q = @"update shop.t6 set ";
        var r = "insert ignore into t7 (a) values " + string.Join(", ", rows);
        var i = "insert ignore into t8 (a, b) values ";
        q = "update t9 set last_updated=now(), ";
        sqlBuilder.AppendLine("SELECT a FROM t10");
        sb.AppendLine("Select a plan from the list");
        db.FetchAsync<dynamic>("select word from t11;");
        var n = "SELECT a FROM t12;";
    }
}
class InfluxStatsReader {
    Task<long> Read() { var q = "SELECT sum(value) FROM requests WHERE time > now() - 5m"; return ReadInflux(q); }
}
"#,
        );
        let names: Vec<(&str, &str, u32)> = facts.iter().map(|f| (f.0, f.3.as_str(), f.4)).collect();
        assert_eq!(
            names,
            [
                ("sql_read", "t1", 9),
                ("sql_write", "t2", 10),
                ("sql_write", "t3", 12),
                ("sql_write", "t4", 14),
                ("sql_read", "t5", 15),
                ("sql_write", "shop.t6", 16),
                ("sql_write", "t7", 17),
                ("sql_write", "t8", 18),
                ("sql_write", "t9", 19),
                ("sql_read", "t10", 20),
                ("sql_read", "t11", 22),
                ("sql_read", "t12", 23),
            ]
        );
    }

    #[test]
    fn escaped_line_breaks_keep_the_file_line() {
        let facts = sql_facts(concat!(
            "class R {\n",
            "    void M() {\n",
            "        _conn.Execute(\"SELECT a,\\n b,\\n c\\n FROM drift_table WHERE id = @id\");\n",
            "        _conn.Execute(\"SELECT a -- note\\nFROM after_comment\");\n",
            "        _conn.Execute($\"SELECT a\\n FROM {schema}.interp_table\\n WHERE id = @id\");\n",
            "        _conn.Execute(@\"SELECT a\n",
            " FROM verbatim_table\");\n",
            "    }\n",
            "}\n",
        ));
        let lines: Vec<(&str, u32)> = facts.iter().map(|f| (f.3.as_str(), f.4)).collect();
        assert_eq!(lines, [("drift_table", 3), ("after_comment", 4), ("interp_table", 5), ("verbatim_table", 7)]);
    }

    #[test]
    fn sql_held_in_members_goes_to_their_users() {
        let facts = sql_facts(
            r#"
class Repo {
    const string ByIdSql = "SELECT id FROM held_table WHERE id = @id";
    const string LogSql = "SELECT id FROM log_table WHERE id = @id";
    string Getter { get { return _conn.Query<string>("SELECT name FROM getter_table WHERE id = @id").First(); } }
    string Arrow => "SELECT name FROM arrow_table WHERE id = @id";
    static string Built { get { string q = "SELECT a FROM built_table WHERE id = @id"; return q; } }
    int Use() => _conn.Query<int>(ByIdSql).First();
    int NotAUse() => other.LogSql + LogSql(1) + LogSql<int>();
    string Name() => Getter;
}
"#,
        );
        let rows: Vec<(&str, &str, &str, &str)> = facts.iter().map(|f| (f.0, f.1.as_str(), f.2.as_str(), f.3.as_str())).collect();
        assert_eq!(
            rows,
            [
                // Used here: kept for users elsewhere, never as the holder's own access.
                ("sql_read", "ByIdSql", "held_used", "held_table"),
                // `other.LogSql`, `LogSql(…)` and `LogSql<…>` are other members.
                ("sql_read", "LogSql", "held", "log_table"),
                // A getter running SQL reads the table itself: `Name` doesn't get the fact.
                ("sql_read", "Getter", "", "getter_table"),
                ("sql_read", "Arrow", "held", "arrow_table"),
                // A getter returning the local holding the text.
                ("sql_read", "Built", "held", "built_table"),
                ("sql_read", "Use", "", "held_table"),
            ]
        );
    }

    #[test]
    fn exec_text_names_the_procedure() {
        assert_eq!(exec_procedure("exec sp_x @a, @b").as_deref(), Some("sp_x"));
        assert_eq!(exec_procedure("  EXEC @rc = [dbo].[sp_y];").as_deref(), Some("dbo.sp_y"));
        assert_eq!(exec_procedure("EXECUTE sp_z(1)").as_deref(), Some("sp_z"));
        assert_eq!(exec_procedure("SELECT 1"), None);
        assert_eq!(exec_procedure("EXECUTIVE x"), None);
        assert_eq!(call_procedure("CALL p_x(@a, @b)").as_deref(), Some("p_x"));
        assert_eq!(call_procedure("call `db`.`p_y`;").as_deref(), Some("db.p_y"));
        assert_eq!(call_procedure("CALL p_z").as_deref(), Some("p_z"));
        assert_eq!(call_procedure("Call timed out"), None);
        assert_eq!(call_procedure("Callback x"), None);
    }

    #[test]
    fn top_level_statements_make_a_script() {
        let f = extract("using X;\nvar app = Builder.Create(args);\napp.Run();\n");
        assert!(f.script);
        assert!(f.refs.iter().any(|r| r.name == "Run" && r.kind == RefKind::Call));
    }

    #[test]
    fn block_namespaces_and_generic_bases() {
        let f = extract("namespace A.B { class Repo<T> : IRepo<T>, IDisposable where T : class { } }");
        let repo = f.symbols.iter().find(|s| s.name == "Repo").unwrap();
        assert_eq!(repo.kind, SymbolKind::Class);
        assert_eq!(repo.container, None);
        assert_eq!(f.relations.iter().map(|r| r.target.as_str()).collect::<Vec<_>>(), ["IRepo", "IDisposable"]);
    }

    /// Service, implementation, lifetime, kind, line.
    type Row = (Option<String>, Option<String>, Option<&'static str>, &'static str, u32);

    fn di(src: &str) -> Vec<Row> {
        extract(src).di.into_iter().map(|b| (b.service, b.implementation, b.lifetime, b.kind, b.line)).collect()
    }

    fn s(v: &str) -> Option<String> {
        Some(v.to_string())
    }

    #[test]
    fn autofac_style_registrations() {
        let src = r#"
class Startup
{
    void Build(IContainer container)
    {
        container.RegisterType<OrderService>().As<IOrderService>();
        container.RegisterType<Gateway>().As<IGateway>().Scoped(Scope.PerJob);
        container.RegisterType<Limiter>().As<ILimiter>().Scoped(Scope.SingleInstance);
        container.RegisterType<Validator>().As<IValidator<Order>>();
        container.RegisterType<Job>();
        container.RegisterType<Store>().As<IReader>().As<IWriter>().SingleInstance();
        container.RegisterType<Handler>().AsImplementedInterfaces().InstancePerLifetimeScope();
        container.RegisterType<Clients>().AsSelf().InstancePerDependency();
        container.Register(_ => new Clock(1)).As<IClock>();
        container.Register(_ => mock.Object).As<IMailer>();
        container.Register(c => { var x = 1; return new Cache(x); }).As<ICache>();
        container.RegisterInstance(new Settings()).As<ISettings>();
        builder.RegisterAssemblyTypes(typeof(Startup).Assembly).Where(t => t.Name.EndsWith("Service")).AsImplementedInterfaces();
        Manager.Instance.Register(container);
        token.Register(() => Cancel());
        mock.As<IOther>();
    }
}
"#;
        assert_eq!(
            di(src),
            [
                (s("IOrderService"), s("OrderService"), None, "type", 6),
                (s("IGateway"), s("Gateway"), Some("unit_of_work"), "type", 7),
                (s("ILimiter"), s("Limiter"), Some("singleton"), "type", 8),
                (s("IValidator<Order>"), s("Validator"), None, "type", 9),
                (s("Job"), s("Job"), None, "type", 10),
                (s("IReader"), s("Store"), Some("singleton"), "type", 11),
                (s("IWriter"), s("Store"), Some("singleton"), "type", 11),
                (None, s("Handler"), Some("scoped"), "type", 12),
                (s("Clients"), s("Clients"), Some("transient"), "type", 13),
                (s("IClock"), s("Clock"), None, "factory", 14),
                (s("IMailer"), None, None, "factory", 15),
                (s("ICache"), s("Cache"), None, "factory", 16),
                (s("ISettings"), s("Settings"), None, "instance", 17),
                (None, s("*Service"), None, "scan", 18),
            ]
        );
    }

    #[test]
    fn a_container_registering_itself_is_a_registration_of_its_services() {
        let src = r#"
namespace Contoso.Wiring;
sealed class OwnScopeBinding : ComponentRegistration
{
    public OwnScopeBinding()
        : base(
            Guid.Empty,
            new DelegateActivator(typeof(WorkScope), (c, p) => throw new InvalidOperationException()),
            CurrentScopeLifetime.Instance,
            InstanceSharing.Shared,
            InstanceOwnership.ExternallyOwned,
            new Service[] { new TypedService(typeof(IWorkScope)), new TypedService(typeof(IResolver)) },
            new Dictionary<string, object>())
    {
    }
}
class Wiring
{
    void Build()
    {
        Add(new ComponentRegistration(Guid.Empty, new ReflectionActivator(typeof(Clock), ctors), RootScopeLifetime.Instance, InstanceSharing.Shared,
            InstanceOwnership.OwnedByLifetimeScope, new[] { new TypedService(typeof(IClock)) }, metadata));
        var asked = registry.IsRegistered(new TypedService(typeof(IMailer)));
    }
}
"#;
        assert_eq!(
            di(src),
            [
                (s("IWorkScope"), s("WorkScope"), Some("scoped"), "type", 12),
                (s("IResolver"), s("WorkScope"), Some("scoped"), "type", 12),
                (s("IClock"), s("Clock"), Some("singleton"), "type", 22),
            ]
        );
    }

    #[test]
    fn microsoft_di_registrations() {
        let src = r#"
static class Setup
{
    static void Add(IServiceCollection services)
    {
        services.AddScoped<IOrderService, OrderService>();
        services.AddSingleton<Clock>();
        services.AddTransient<IMailer>(sp => new SmtpMailer(sp.Get()));
        services.TryAddSingleton<ICache, MemoryCache>();
        services.AddScoped(typeof(IRepo<>), typeof(Repo<>));
        services.AddSingleton(new Settings());
        services.AddSingleton(existing);
        services.AddSingleton<IFeed>(config.Feed);
        services
            .AddScoped<IA, A>()
            .AddTransient<IB, B>();
        services.AddHttpClient<IApi, ApiClient>();
        dict.TryAdd(key, value);
    }
}
"#;
        assert_eq!(
            di(src),
            [
                (s("IOrderService"), s("OrderService"), Some("scoped"), "type", 6),
                (s("Clock"), s("Clock"), Some("singleton"), "type", 7),
                (s("IMailer"), s("SmtpMailer"), Some("transient"), "factory", 8),
                (s("ICache"), s("MemoryCache"), Some("singleton"), "type", 9),
                (s("IRepo<>"), s("Repo"), Some("scoped"), "type", 10),
                (s("Settings"), s("Settings"), Some("singleton"), "instance", 11),
                // Registered as what `existing` is: the index resolves it (`made`).
                (None, None, Some("singleton"), "instance", 12),
                (s("IFeed"), None, Some("singleton"), "instance", 13),
                (s("IA"), s("A"), Some("scoped"), "type", 15),
                (s("IB"), s("B"), Some("transient"), "type", 16),
                (s("IApi"), s("ApiClient"), Some("transient"), "type", 17),
            ]
        );
        // `TryAdd*` registers only when the service has none yet.
        let tried: Vec<u32> = extract(src).di.into_iter().filter(|b| b.mode == Some("try")).map(|b| b.line).collect();
        assert_eq!(tried, [9]);
    }

    #[test]
    fn service_descriptors_are_registrations() {
        let src = r#"
static class Setup
{
    static void Add(IServiceCollection services, Settings settings)
    {
        services.Add(new ServiceDescriptor(typeof(IOrderService), typeof(OrderService), ServiceLifetime.Scoped));
        services.Add(new ServiceDescriptor(typeof(IClock), sp => new SystemClock(), ServiceLifetime.Singleton));
        services.Add(new ServiceDescriptor(typeof(Settings), settings));
        services.TryAdd(ServiceDescriptor.Singleton<ICache, MemoryCache>());
        services.TryAdd(ServiceDescriptor.Transient(typeof(IRepo<>), typeof(Repo<>)));
        services.TryAddEnumerable(new[]
        {
            ServiceDescriptor.Scoped<IHandler, OrderHandler>(),
            ServiceDescriptor.Scoped<IHandler, RefundHandler>(),
        });
        services.Replace(ServiceDescriptor.Describe(typeof(IMailer), typeof(SmtpMailer), ServiceLifetime.Transient));
        services.Insert(0, ServiceDescriptor.Singleton<IFeed>(sp => new Feed(sp)));
        services.TryAddEnumerable(ServiceDescriptor.KeyedSingleton<IHandler, AuditHandler>("audit"));
        var unread = ServiceDescriptor.Singleton<IOther, Other>();
        Assert.Contains(services, d => d.ServiceType == typeof(IOrderService));
    }
}
"#;
        assert_eq!(
            di(src),
            [
                (s("IOrderService"), s("OrderService"), Some("scoped"), "type", 6),
                (s("IClock"), s("SystemClock"), Some("singleton"), "factory", 7),
                (s("Settings"), s("Settings"), Some("singleton"), "instance", 8),
                (s("ICache"), s("MemoryCache"), Some("singleton"), "type", 9),
                (s("IRepo<>"), s("Repo"), Some("transient"), "type", 10),
                (s("IHandler"), s("OrderHandler"), Some("scoped"), "type", 13),
                (s("IHandler"), s("RefundHandler"), Some("scoped"), "type", 14),
                (s("IMailer"), s("SmtpMailer"), Some("transient"), "type", 16),
                (s("IFeed"), s("Feed"), Some("singleton"), "factory", 17),
            ]
        );
        let unread: Vec<(String, u32)> = extract(src).unsupported.into_iter().map(|u| (u.construct, u.line)).collect();
        assert_eq!(unread, [("keyed registration ServiceDescriptor.KeyedSingleton(…)".to_string(), 18)]);
        // How each is added: by the call it is handed to.
        let modes: Vec<(u32, Option<&str>)> = extract(src).di.into_iter().map(|b| (b.line, b.mode)).collect();
        assert_eq!(
            modes,
            [
                (6, None),
                (7, None),
                (8, None),
                (9, Some("try")),
                (10, Some("try")),
                (13, Some("enumerable")),
                (14, Some("enumerable")),
                (16, Some("replace")),
                (17, None),
            ]
        );
    }

    #[test]
    fn factory_lambdas_say_what_they_make() {
        let src = r#"
class Setup
{
    void Add(IServiceCollection services, ContainerBuilder builder, Options options)
    {
        services.AddScoped<IFoo>(sp => sp.GetRequiredService<Foo>());
        services.AddSingleton<IBar>(_ => { var b = new Bar(1); b.Start(); return b; });
        services.AddTransient<IBaz>(sp => (Baz)sp.GetService(typeof(IBaz)));
        builder.Register(c => MakeQux(c)).As<IQux>();
        builder.Register(c => Factory.Create(c)).As<IPay>();
        services.AddSingleton(sp => MakeQux(sp));
        builder.RegisterInstance(options).As<IOptions>();
        builder.Register(c => c.Resolve<Settings>().Mail);
        services.AddSingleton<IRun>(sp => cond ? new Fast() : new Slow());
    }
}
"#;
        // Service, implementation, made (left to the index), service made, line.
        type Made = (Option<String>, Option<String>, bool, bool, u32);
        let made: Vec<Made> =
            extract(src).di.into_iter().map(|b| (b.service, b.implementation, b.made.is_some(), b.service_made, b.line)).collect();
        assert_eq!(
            made,
            [
                (s("IFoo"), s("Foo"), false, false, 6),
                (s("IBar"), s("Bar"), false, false, 7),
                (s("IBaz"), s("Baz"), false, false, 8),
                // A factory method's or a property's declared type: resolved by the index.
                (s("IQux"), None, true, false, 9),
                (s("IPay"), None, true, false, 10),
                (None, None, true, true, 11),
                (s("IOptions"), s("Options"), false, false, 12),
                (None, None, true, true, 13),
                // Different types on either branch: not known.
                (s("IRun"), None, false, false, 14),
            ]
        );
    }

    /// `(kind, symbol, message, queue, via, line)` of each bus fact.
    type BusRow = (&'static str, String, Option<String>, Option<String>, Option<String>, u32);

    fn bus(src: &str) -> (Vec<BusRow>, FileFacts) {
        let f = extract(src);
        let rows = f
            .bus
            .iter()
            .map(|b| (b.kind, f.symbols[b.sym].name.clone(), b.message.clone(), b.queue.clone(), b.via.clone(), b.line))
            .collect();
        (rows, f)
    }

    #[test]
    fn bus_messages_publishes_and_consumers() {
        let events = "using Shop.Queue.Abstractions;\n\nnamespace Shop.Orders.Models.Events\n{\n    [ShopMessage(ShopHosts.Orders)]\n    public class OrderPlaced { }\n\n    [ShopMessage(\"refunds\")]\n    public class Refunded { public class Line { } }\n\n    public class NotAMessage { }\n}\n";
        let (rows, f) = bus(events);
        assert_eq!(
            rows,
            [
                ("message", "OrderPlaced".into(), s("OrderPlaced"), s("Orders"), None, 5),
                ("message", "Refunded".into(), s("Refunded"), s("refunds"), None, 8),
            ]
        );
        assert_eq!(f.bus[0].scopes, ["Shop.Orders.Models.Events"]);

        let service = r#"using System.Threading.Tasks;
using Shop.Orders.Models.Events;
using Ev = Shop.Billing.Events;

namespace Shop.Orders.Service;

public class OrderService<TItem>
{
    private readonly IShopBus _serviceBus;
    public async Task Place(Order order)
    {
        await _serviceBus.Publish(new OrderPlaced { Id = order.Id });
        var refund = new Refunded();
        await _serviceBus.Publish(refund);
        await _serviceBus.PublishToHost(_mapper.Map<Ev.Charged>(order), ShopHosts.Billing);
        await _serviceBus.SendToQueue(new Recalculate(), ShopHosts.Jobs, "recalc-1");
        await _serviceBus.Publish<OrderShipped>(Build());
        var charged = _mapper.Map<Order, Charged>(order);
        await _serviceBus.Publish(charged);
        foreach (var failed in new List<PaymentFailed>()) await _serviceBus.Publish(failed);
        await _serviceBus.Publish(order.ToEvent());
        await _gateway.SendAsync(new GetOrder());
        await _businessRules.Publish(new Ignored());
        await _serviceBus.Publish(default(TItem));
    }
    public void Generic<T>(T e) => _bus.Publish(e);
}

public class OrderTests
{
    public void Verifies() => _busMock.Verify(b => b.Publish(It.Is<OrderPlaced>(e => e.Id == 1), It.IsAny<bool>()));
}
"#;
        let (rows, f) = bus(service);
        let got: Vec<_> = rows.iter().map(|r| (r.1.as_str(), r.2.as_deref(), r.3.as_deref(), r.4.as_deref())).collect();
        assert_eq!(
            got,
            [
                ("Place", Some("OrderPlaced"), None, Some("Publish")),
                ("Place", Some("Refunded"), None, Some("Publish")),
                ("Place", Some("Shop.Billing.Events.Charged"), Some("Billing"), Some("PublishToHost")),
                ("Place", Some("Recalculate"), Some("Jobs/recalc-1"), Some("SendToQueue")),
                ("Place", Some("OrderShipped"), None, Some("Publish")),
                ("Place", Some("Charged"), None, Some("Publish")),
                ("Place", Some("PaymentFailed"), None, Some("Publish")),
                ("Place", None, None, Some("Publish")),
                ("Place", None, None, Some("Publish")),
                ("Generic", None, None, Some("Publish")),
            ]
        );
        // Nearest namespace first, then the usings that aren't the framework's.
        assert_eq!(f.bus[0].scopes, ["Shop.Orders.Service", "Shop.Orders", "Shop", "Shop.Orders.Models.Events", "Shop.Billing.Events"]);

        let consumers = "namespace Shop.Mail.Consumers\n{\n    using Shop.Orders.Models.Events;\n    public class OrderPlacedConsumer : ShopConsumer<OrderPlaced, OrderPlacedConsumer>\n    {\n        public override Task Consume(OrderPlaced message, ConsumeContext<OrderPlaced> context) => context.Publish(new MailQueued());\n    }\n    public class Both : IConsumer<Refunded>, MassTransit.IConsumer<Batch<Shop.Orders.Models.Events.Shipped>>, IDisposable { }\n    public abstract class Base<TMessage> : ShopConsumer<TMessage, Base<TMessage>> { }\n}\n";
        let (rows, f) = bus(consumers);
        let got: Vec<_> = rows.iter().map(|r| (r.0, r.1.as_str(), r.2.as_deref(), r.4.as_deref())).collect();
        assert_eq!(
            got,
            [
                ("consume", "OrderPlacedConsumer", Some("OrderPlaced"), Some("ShopConsumer")),
                ("publish", "Consume", Some("MailQueued"), Some("Publish")),
                ("consume", "Both", Some("Refunded"), Some("IConsumer")),
                ("consume", "Both", Some("Shop.Orders.Models.Events.Shipped"), Some("IConsumer")),
            ]
        );
        assert_eq!(f.bus[0].scopes, ["Shop.Mail.Consumers", "Shop.Mail", "Shop", "Shop.Orders.Models.Events"]);
    }

    #[test]
    fn bus_publishes_through_helpers_are_the_callers() {
        let src = r#"namespace Shop.Accounts;
public class AccountController
{
    private readonly IPublishEndpoint publishEndpoint;
    private readonly IBus bus;
    public async Task Lock(User u)
    {
        await TryPublish(new AccountLocked { Id = u.Id }, "lock");
        await PublishLogin<LoginConfirmed>(u);
        await this.Send<PasswordReset>(new PasswordReset(), 1);
        await Untyped(new AccountClosed());
        await bus.Publish(BuildAudit(u));
        var note = BuildNote(u);
        await bus.Publish(note);
        async Task Local() { await TryPublish(new AccountOpened(), "local"); }
    }
    private async Task TryPublish<T>(T message, string context) where T : class
    {
        try { await publishEndpoint.Publish(message); } catch { }
    }
    private Task PublishLogin<T>(User u) where T : LoginEvent, new() => TryPublish(new T { UserId = u.Id }, "mfa");
    private Task Send<T>(object m, int n) => bus.Publish<T>(m);
    private Task Untyped(object m) => bus.Publish(m);
    private AuditRecorded BuildAudit(User u) => new AuditRecorded();
    private async Task<NoteAdded> BuildNote(User u) => new NoteAdded();
    // Not a helper: publishes a message of its own.
    private Task Other<T>(T x) => bus.Publish(new Pinged());
}
"#;
        let (rows, _) = bus(src);
        let got: Vec<_> = rows.iter().map(|r| (r.1.as_str(), r.2.as_deref(), r.4.as_deref(), r.5)).collect();
        assert_eq!(
            got,
            [
                ("Lock", Some("AuditRecorded"), Some("Publish"), 12),
                ("Lock", Some("NoteAdded"), Some("Publish"), 14),
                // The helpers' own untyped publishes are gone: their callers say what they publish.
                ("Other", Some("Pinged"), Some("Publish"), 27),
                // The helpers' callers, at the call, with the helper as `via`; a local function calls its type's.
                ("Lock", Some("AccountLocked"), Some("TryPublish"), 8),
                ("Local", Some("AccountOpened"), Some("TryPublish"), 15),
                ("Lock", Some("PasswordReset"), Some("Send"), 10),
                ("Lock", Some("AccountClosed"), Some("Untyped"), 11),
                // `PublishLogin<T>` hands its `T` on to `TryPublish`: a helper too.
                ("Lock", Some("LoginConfirmed"), Some("PublishLogin"), 9),
            ]
        );
    }

    #[test]
    fn bus_publishes_through_null_conditional_calls() {
        let src = r#"public class OrderService
{
    private readonly IPublishEndpoint _bus;
    public async Task Place(int id) { await _bus?.Publish(new OrderPlaced(id)); }
    public async Task Ship(int id) { await this?.PublishAsync(new OrderShipped(id)); }
    private Task PublishAsync<T>(T message) where T : class => _bus?.Publish(message);
}
"#;
        let (rows, _) = bus(src);
        let got: Vec<_> = rows.iter().map(|r| (r.1.as_str(), r.2.as_deref(), r.4.as_deref(), r.5)).collect();
        assert_eq!(
            got,
            [
                ("Place", Some("OrderPlaced"), Some("Publish"), 4),
                ("Ship", Some("OrderShipped"), Some("PublishAsync"), 5),
            ]
        );
    }

    #[test]
    fn saga_events_and_registered_generic_consumers_consume() {
        let src = r#"namespace Shop.Auctions;
public class OrderStateMachine : MassTransitStateMachine<AuctionState>
{
    public State Open { get; private set; }
    public Event<AuctionStarted> Started { get; private set; }
    public Event<Events.BidPlaced> Bid { get; private set; }
    public Schedule<AuctionState, CheckAuction> Check { get; private set; }
    public OrderStateMachine() { Event(() => Started); }
}
public class Startup
{
    public void ConfigureServices(IServiceCollection s)
    {
        s.AddMassTransit(cfg =>
        {
            cfg.AddMailConsumer<WelcomeEmail>();
            cfg.AddConsumer<MailConsumer<ResetEmail>, Definition>();
            cfg.AddConsumer<OrderConsumer>();
            cfg.AddShopConsumer<RefundConsumer>();
            cfg.AddSagaStateMachine<OrderStateMachine, AuctionState>();
        });
    }
}
public static class Registration
{
    public static void AddMailConsumer<T>(this IBusRegistrationConfigurator c) => c.AddConsumer<MailConsumer<T>>();
}
"#;
        let (rows, _) = bus(src);
        let got: Vec<_> = rows.iter().map(|r| (r.0, r.1.as_str(), r.2.as_deref(), r.4.as_deref(), r.5)).collect();
        assert_eq!(
            got,
            [
                ("consume", "OrderStateMachine", Some("AuctionStarted"), Some("MassTransitStateMachine"), 5),
                ("consume", "OrderStateMachine", Some("Events.BidPlaced"), Some("MassTransitStateMachine"), 6),
                ("consume", "OrderStateMachine", Some("CheckAuction"), Some("MassTransitStateMachine"), 7),
                ("consume", "ConfigureServices", Some("WelcomeEmail"), Some("AddMailConsumer"), 16),
                ("consume", "ConfigureServices", Some("ResetEmail"), Some("AddConsumer"), 17),
            ]
        );
    }

    #[test]
    fn arities_and_argument_counts() {
        let src = r#"
static class Ext
{
    static void Upsert(this Repo r, Order o, bool force = false) { }
    static void Log(string format, params object[] args) { }
    static void Raw(__arglist) { }
    static void None() { }
    static void Run(Repo r)
    {
        r.Upsert(o);
        Log("x", 1, 2, 3);
        None();
        Log(format: "x");
        r.Put(1); r.Put(1, 2);
    }
}
"#;
        let f = extract(src);
        let arity = |name: &str| f.symbols.iter().find(|s| s.name == name).and_then(|s| s.arity);
        assert_eq!(arity("Upsert"), Some((1, Some(2))));
        assert_eq!(arity("Log"), Some((1, None)));
        assert_eq!(arity("Raw"), Some((0, None)));
        assert_eq!(arity("None"), Some((0, Some(0))));
        let args = |name: &str| f.refs.iter().find(|r| r.name == name && r.kind == RefKind::Call).map(|r| r.args);
        assert_eq!(args("Upsert"), Some(Some(1)));
        assert_eq!(args("None"), Some(Some(0)));
        // Two calls of `Log` on different lines: the first's count is kept.
        assert_eq!(args("Log"), Some(Some(4)));
        // Two calls on one line: each its own reference, with its own count.
        let put: Vec<(u32, Option<u32>)> =
            f.refs.iter().filter(|r| r.name == "Put" && r.kind == RefKind::Call).map(|r| (r.col, r.args)).collect();
        assert_eq!(put, [(11, Some(1)), (21, Some(2))]);
    }

    #[test]
    fn lambdas_are_typed_from_the_callees_delegate() {
        let src = r#"
class Repo<T>
{
    Task<Order> Find(Expression<Func<Order, bool>> where) => null;
    void Each(int n, Action<Order, Line> f) { }
    void Generic<U>(Func<U, bool> f, Func<T, bool> g) { }
    void Run()
    {
        Find(o => o.Paid);
        _repo.Each(2, (o, l) => l.Total);
        _repo.Other((Order typed) => typed.Paid);
        Find(where: x => x.Paid);
        _mock.Verify(m => m.Add(It.Is<Line>(l => l.Qty > 0)));
    }
}
"#;
        let f = extract(src);
        let lt = |name: &str| f.symbols.iter().find(|s| s.name == name).and_then(|s| s.lambda_types.clone());
        assert_eq!(lt("Find").as_deref(), Some(r#"[["Order"]]"#));
        assert_eq!(lt("Each").as_deref(), Some(r#"[null,["Order","Line"]]"#));
        // Type parameters of the method or its type say nothing.
        assert_eq!(lt("Generic"), None);
        let rt = |name: &str| f.refs.iter().find(|r| r.name == name && r.kind == RefKind::Member).and_then(|r| r.receiver_type.clone());
        let decoded = |name: &str| rt(name).and_then(|t| TypeExpr::decode(&t));
        let lp = |recv: Option<TypeExpr>, name: &str, args, arg, param| TypeExpr::LambdaParam {
            recv: recv.map(Box::new),
            name: name.into(),
            args,
            arg,
            param,
        };
        let repo = TypeExpr::root(PathRoot::Prop("_repo".into()));
        assert_eq!(decoded("Total"), Some(lp(Some(repo), "Each", 2, 1, 1)));
        // A declared parameter type wins; a named argument's position isn't the parameter's.
        assert_eq!(rt("Paid").as_deref(), Some(lp(None, "Find", 1, 0, 0).encode().as_str()));
        let paid: Vec<_> = f.refs.iter().filter(|r| r.name == "Paid").map(|r| r.receiver_type.clone()).collect();
        assert_eq!(paid, [Some(lp(None, "Find", 1, 0, 0).encode()), Some("Order".to_string()), Some("{\"path\":{\"root\":{\"prop\":\"x\"},\"segs\":[]}}".to_string())]);
        // An argument matcher's lambda takes the matcher's type argument.
        assert_eq!(rt("Qty").as_deref(), Some("Line"));
    }

    #[test]
    fn delegates_called_and_conditional_calls_are_typed() {
        let src = r#"namespace Shop;
public class C
{
    public void Run(Repo.Factory make, Order order)
    {
        make(1).Get();
        order?.Ship(2);
        make?.Invoke(3).Get();
    }
}
"#;
        let f = extract(src);
        let named = |n: &str| TypeExpr::Named(n.into());
        let rt = |name: &str, line: u32| {
            let r = f.refs.iter().find(|r| r.name == name && r.kind == RefKind::Call && r.line == line).unwrap();
            (r.receiver.clone(), TypeExpr::decode(r.receiver_type.as_deref().unwrap()).unwrap())
        };
        // A parameter of a delegate type, called: the delegate invoked, and what follows is on its result.
        assert_eq!(rt("Invoke", 6), (Some("make".to_string()), named("Repo.Factory")));
        assert_eq!(rt("Get", 6).1, named("Repo.Factory").then(Seg::Call("Invoke".into())));
        // `x?.M(…)` is a call of `M` on `x`.
        assert_eq!(rt("Ship", 7), (Some("order".to_string()), named("Order")));
        assert_eq!(rt("Invoke", 8), (Some("make".to_string()), named("Repo.Factory")));
        assert_eq!(rt("Get", 8).1, named("Repo.Factory").then(Seg::Call("Invoke".into())));
        assert!(!f.refs.iter().any(|r| r.name == "make"));
    }

    #[test]
    fn a_namespace_blocks_using_alias_applies_only_in_that_block() {
        let src = r#"using Top = Shop.Shared;
namespace Shop.Left
{
    using M = Shop.Models;
    public class A { public void Run(M.Order o, Top.Clock c) { o.Ship(); c.Tick(); } }
}
namespace Shop.Right
{
    using M = Shop.Entities;
    public class B { public void Run(M.Order o) { o.Ship(); } }
}
public class C { public void Run(M.Order o) { o.Ship(); } }
"#;
        let f = extract(src);
        let ships: Vec<TypeExpr> = f
            .refs
            .iter()
            .filter(|r| r.name == "Ship")
            .map(|r| TypeExpr::decode(r.receiver_type.as_deref().unwrap()).unwrap())
            .collect();
        let named = |n: &str| TypeExpr::Named(n.into());
        assert_eq!(ships, [named("Shop.Models.Order"), named("Shop.Entities.Order"), named("M.Order")]);
        let tick = f.refs.iter().find(|r| r.name == "Tick").unwrap();
        assert_eq!(TypeExpr::decode(tick.receiver_type.as_deref().unwrap()), Some(named("Shop.Shared.Clock")));
    }

    #[test]
    fn types_keep_qualifiers_arguments_tuples_and_delegates() {
        let src = r#"
using Ent = Shop.Entities;
namespace Shop.Data;
public class Repo<TEntity> : BaseRepo<ShopContext, TEntity>, IRepo
{
    private readonly Func<Owned<IUnitOfWork>> _factory;
    private readonly Lazy<Cache> _cache;
    public void Run(Data.Policy policy, PagedList<Order> page, IEnumerable<(string Number, decimal Amount)> credits, Ent.Domain d, string s)
    {
        policy.Check();
        page.Items.Count();
        _factory().Value.Complete();
        _cache.Value.Clear();
        foreach (var c in credits) { c.Amount.ToString(); }
        d.Rename();
        s.Trim();
        _scope.GetInstance<IReport>().Run();
        var shown = Should.Throw<ValidationException>(() => Go());
        shown.Errors.Reset();
        var r = _mapper.Map<Models.Request>(page);
        r.Send();
        var p = provider.GetService(typeof(IClock)) ;
    }
}
"#;
        let f = extract(src);
        let repo = f.symbols.iter().find(|s| s.name == "Repo").unwrap();
        let decl = TypeDecl::decode(repo.type_name.as_deref());
        assert_eq!(decl.ns.as_deref(), Some("Shop.Data"));
        assert_eq!(decl.params, ["TEntity"]);
        assert_eq!(decl.bases, [("BaseRepo".to_string(), vec![Some("ShopContext".to_string()), Some("TEntity".to_string())])]);
        assert_eq!(decl.aliases, [("Ent".to_string(), "Shop.Entities".to_string())]);
        let rt = |name: &str| {
            let r = f.refs.iter().find(|r| r.name == name && r.kind == RefKind::Call).unwrap();
            TypeExpr::decode(r.receiver_type.as_deref().unwrap()).unwrap()
        };
        let named = |n: &str| TypeExpr::Named(n.into());
        // Written qualified, a type keeps its qualifier; an alias is spelled out.
        assert_eq!(rt("Check"), named("Data.Policy"));
        assert_eq!(rt("Rename"), named("Shop.Entities.Domain"));
        assert_eq!(rt("Trim"), named("string"));
        let generic = |n: &str, a: TypeExpr| TypeExpr::Generic { name: n.into(), args: vec![Some(a)] };
        assert_eq!(rt("Count"), generic("PagedList", named("Order")).then(Seg::Member("Items".into())));
        assert_eq!(rt("Complete"), TypeExpr::Call { owner: None, name: "_factory".into() }.then(Seg::Member("Value".into())));
        assert_eq!(rt("Run"), named("IReport"));
        assert_eq!(rt("Reset"), named("ValidationException").then(Seg::Member("Errors".into())));
        assert_eq!(rt("Send"), named("Models.Request"));
        let field = |n: &str| TypeExpr::decode(f.symbols.iter().find(|s| s.name == n).unwrap().type_name.as_deref().unwrap()).unwrap();
        assert_eq!(field("_factory"), TypeExpr::Delegate(Box::new(generic("Owned", named("IUnitOfWork")))));
        assert_eq!(field("_cache"), generic("Lazy", named("Cache")));
        let credits = TypeExpr::Tuple(vec![("Number".into(), Some(named("string"))), ("Amount".into(), Some(named("decimal")))]);
        let amount = f.refs.iter().find(|r| r.name == "Amount" && r.kind == RefKind::Member).unwrap();
        assert!(!f.refs.iter().any(|r| r.name == "Amount" && r.kind == RefKind::TypeRef));
        assert_eq!(
            TypeExpr::decode(amount.receiver_type.as_deref().unwrap()),
            Some(TypeExpr::Elem(Box::new(TypeExpr::Array(Box::new(credits)))))
        );
        // A mapper creates its destination; a qualified type keeps its qualifier as the receiver.
        let created: Vec<(&str, Option<&str>)> =
            f.refs.iter().filter(|r| r.kind == RefKind::Instantiate).map(|r| (r.name.as_str(), r.receiver.as_deref())).collect();
        assert_eq!(created, [("Request", Some("Models"))]);
        let qualified: Vec<(&str, Option<&str>)> = f
            .refs
            .iter()
            .filter(|r| r.kind == RefKind::TypeRef && r.receiver.is_some())
            .map(|r| (r.name.as_str(), r.receiver.as_deref()))
            .collect();
        assert!(qualified.contains(&("Policy", Some("Data"))) && qualified.contains(&("Domain", Some("Shop.Entities"))), "{qualified:?}");
    }

    #[test]
    fn child_rules_extensions_and_command_wrappers() {
        let src = r#"
public class InvoiceValidator : AbstractValidator<(Invoice Invoice, bool Strict)>
{
    public InvoiceValidator()
    {
        RuleFor(x => x.Invoice).NotNull();
        RuleForEach(x => x.Invoice.Lines).ChildRules(line => { line.RuleFor(l => l.Amount).NotEqual(0); });
    }
}
public static class TextExtensions { public static string Tld(this string s) => s; }
public class Repo
{
    public void Load(DbConnection connection) { using var cmd = connection.CreateCommand("appconfig.GetHierarchy"); }
}
"#;
        let f = extract(src);
        let rt = |name: &str| f.refs.iter().find(|r| r.name == name && r.kind == RefKind::Member).and_then(|r| TypeExpr::decode(r.receiver_type.as_deref()?));
        let validated = TypeExpr::Tuple(vec![("Invoice".into(), Some(TypeExpr::Named("Invoice".into()))), ("Strict".into(), Some(TypeExpr::Named("bool".into())))]);
        assert_eq!(rt("Invoice"), Some(validated.clone()));
        let lines = TypeExpr::Path { root: PathRoot::Expr(Box::new(validated)), segs: vec![Seg::Member("Invoice".into()), Seg::Member("Lines".into())] };
        assert_eq!(rt("Amount"), Some(TypeExpr::Elem(Box::new(lines))));
        // An extension method on a predefined type is a member of it.
        assert_eq!(f.symbols.iter().find(|s| s.name == "Tld").unwrap().container.as_deref(), Some("string"));
        let wrapped: Vec<&str> = f.db.iter().filter(|d| d.kind == "wrapped_procedure").map(|d| d.name.as_str()).collect();
        assert_eq!(wrapped, ["appconfig.GetHierarchy"]);
    }

    /// An endpoint fact as (side, verb, path, the symbol it belongs to, line).
    fn endpoints(f: &FileFacts) -> Vec<(&'static str, String, String, String, u32)> {
        f.http_routes
            .iter()
            .map(|r| (r.side, r.verb.clone(), r.path.clone(), r.sym.map_or_else(|| "-".to_string(), |s| f.symbols[s].name.clone()), r.line))
            .collect()
    }

    fn e(side: &'static str, verb: &str, path: &str, sym: &str, line: u32) -> (&'static str, String, String, String, u32) {
        (side, verb.to_string(), path.to_string(), sym.to_string(), line)
    }

    /// dotnet/eShop's shape: groups in locals and in fluent chains, a helper extending the application, a constant
    /// template, route constraints, method groups and lambdas, nested groups, a group passed to another helper.
    #[test]
    fn minimal_api_endpoints_are_server_routes_of_their_handlers() {
        let src = r#"
public static class Routes { public const string Item = "/items/{id:int}"; }
public static class CatalogApi
{
    public static IEndpointRouteBuilder MapCatalogApi(this IEndpointRouteBuilder app)
    {
        var vApi = app.NewVersionedApi("Catalog");
        var api = vApi.MapGroup("api/catalog").HasApiVersion(1, 0);
        api.MapGet("/items", GetAllItems);
        api.MapGet(Routes.Item, GetItemById).WithName("GetItem");
        api.MapPost("/items", async (CatalogItem item) => Results.Ok());
        api.MapMethods("/check", [HttpMethods.Options, HttpMethods.Head], Check);
        var admin = api.MapGroup("admin");
        admin.MapDelete("/items/{id}", CatalogApi.DeleteItem);
        app.MapGroup("api/orders").MapOrdersApi();
        api.Map("/any", (HttpContext c) => c.Response);
        return app;
    }
    public static IResult GetAllItems() => null;
    public static IResult GetItemById(int id) => null;
    public static IResult DeleteItem(int id) => null;
    public static IResult Check() => null;
    public static void MapOrdersApi(this RouteGroupBuilder group)
    {
        group.MapGet("{id}", (int id) => id);
        Lines.MapLines(group.MapGroup("lines"));
    }
}
"#;
        let f = extract(src);
        let base = "{@MapCatalogApi#0}/api/catalog";
        assert_eq!(
            endpoints(&f),
            [
                e("server", "GET", &format!("{base}/items"), "GetAllItems", 9),
                e("server", "GET", &format!("{base}/{{=Routes.Item}}"), "GetItemById", 10),
                e("server", "POST", &format!("{base}/items"), "MapCatalogApi", 11),
                e("server", "OPTIONS", &format!("{base}/check"), "Check", 12),
                e("server", "HEAD", &format!("{base}/check"), "Check", 12),
                e("server", "DELETE", &format!("{base}/admin/items/{{id}}"), "DeleteItem", 14),
                e("group", "", "{@MapCatalogApi#0}/api/orders", "MapCatalogApi", 15),
                e("server", "*", &format!("{base}/any"), "MapCatalogApi", 16),
                e("server", "GET", "{@MapOrdersApi#0}/{id}", "MapOrdersApi", 25),
                e("group", "", "{@MapOrdersApi#0}/lines", "MapOrdersApi", 26),
            ]
        );
        let groups: Vec<Option<&str>> = f.http_routes.iter().filter(|r| r.side == "group").map(|r| r.service.as_deref()).collect();
        assert_eq!(groups, [Some("MapOrdersApi#0"), Some("MapLines#0")]);
        // The application passed to a `Map…` helper at the top level: the helper's routes keep their own paths.
        let program = extract("var app = builder.Build();\napp.MapCatalogApi();\nEndpoints.MapHealth(app);\napp.MapGet(\"/\", () => \"up\");\n");
        assert_eq!(
            endpoints(&program),
            [e("group", "", "", "-", 2), e("group", "", "", "-", 3), e("server", "GET", "", "-", 4)]
        );
        assert_eq!(program.http_routes[1].service.as_deref(), Some("MapHealth#0"));
    }

    /// Registrations whose routes can't be known say so, on their handler when it is a method group.
    #[test]
    fn endpoint_registrations_that_cannot_be_placed_are_unsupported() {
        let src = r#"
public class Startup
{
    public void Configure(IApplicationBuilder app, string prefix, string[] verbs)
    {
        app.UseEndpoints(e =>
        {
            e.MapControllerRoute("default", prefix + "/{controller}/{action}");
            e.MapGet(prefix + "/x", Handle);
            e.MapGroup(prefix).MapGet("/y", () => 1);
            e.MapMethods("/z", verbs, Handle);
        });
        app.Map("/tenant", Tenant);
        mapper.Map(source, destination);
    }
    void Handle() { }
    void Tenant(IApplicationBuilder b) { }
}
"#;
        let f = extract(src);
        let got: Vec<(String, String, String)> = f.http_routes.iter().map(|r| (r.verb.clone(), r.sym.map(|s| f.symbols[s].name.clone()).unwrap_or_default(), r.path.clone())).collect();
        assert!(f.http_routes.iter().all(|r| r.side == "unsupported"), "{got:?}");
        let short: Vec<(&str, &str)> = got.iter().map(|(v, s, _)| (v.as_str(), s.as_str())).collect();
        assert_eq!(short, [("MapControllerRoute", "Configure"), ("MapGet", "Handle"), ("MapGet", "Configure"), ("MapMethods", "Handle"), ("Map", "Tenant")]);
        assert!(got[1].2.contains("template is not a constant") && got[2].2.contains("prefix is not a constant"), "{got:?}");
        assert!(got[4].2.contains("middleware branch"), "{got:?}");
    }

    /// A method group passed as a delegate is a reference to the method; a local, a parameter, a field and `nameof`
    /// are not method groups.
    #[test]
    fn method_groups_passed_as_delegates_are_references() {
        let src = r#"
public class Worker
{
    private readonly Action _field;
    public void Start(Bus bus, Action given)
    {
        bus.Subscribe(Handle);
        Task.Run(Work);
        var t = new Thread(Work);
        bus.Changed += OnChanged;
        Run(callback: Handle);
        Run(given);
        Run(_field);
        Run(nameof(Handle));
        Task.Run(Local);
        void Local() { }
    }
    void Handle(int x) { }
    void Work() { }
    void OnChanged(object s, EventArgs e) { }
}
"#;
        let f = extract(src);
        let groups: Vec<(&str, RefKind, u32)> = f
            .refs
            .iter()
            // `nameof(Handle)` is a name use of its own (a type reference, usage `nameof`), not a method group.
            .filter(|r| r.kind != RefKind::TypeRef)
            .filter(|r| matches!(r.name.as_str(), "Handle" | "Work" | "OnChanged" | "given" | "_field" | "Local") && r.receiver.is_none() && r.kind != RefKind::Call || r.name == "Local")
            .map(|r| (r.name.as_str(), r.kind, r.line))
            .collect();
        assert_eq!(
            groups,
            [
                ("Handle", RefKind::Member, 7),
                ("Work", RefKind::Member, 8),
                ("Work", RefKind::Member, 9),
                ("OnChanged", RefKind::Member, 10),
                ("Handle", RefKind::Member, 11),
                ("Local", RefKind::Call, 15),
            ]
        );
    }

    /// A request class carrying a path and a verb in an attribute is a client route, whatever the attribute is
    /// called; ordinal path properties append parameters.
    #[test]
    fn attribute_declared_request_contracts_are_client_routes() {
        let src = r#"
public static class Paths { public const string Orders = "/orders"; }
[Endpoint("/orders", HttpMethod.Post)]
public class CreateOrder { public string Sku { get; set; } }
[SendTo("/orders", Verb.Get)]
public class GetOrder
{
    [PathSegment(2)] public string Line { get; set; }
    [PathSegment(1)] public string OrderId { get; set; }
    [QueryParam] public string Q { get; set; }
    [MaxLength(5)] public string Note { get; set; }
}
[Endpoint(Paths.Orders, HttpMethod.Delete)]
public class DeleteOrder { }
[Endpoint($"{prefix}/x", HttpMethod.Put)]
public class Unknown { }
[Produces("application/json")]
public class NotAContract { }
[Permission(Access.Delete)]
public class AlsoNot { }
[ServiceRequest(Names.Orders, Paths.Orders + "/" + nameof(Other), HttpMethods.Get)]
public class ThreeArguments { }
"#;
        let f = extract(src);
        assert_eq!(
            endpoints(&f),
            [
                e("client", "POST", "orders", "CreateOrder", 3),
                e("client", "GET", "orders/{OrderId}/{Line}", "GetOrder", 5),
                e("client", "DELETE", "{=Paths.Orders}", "DeleteOrder", 13),
                e("unsupported", "Endpoint", "a request contract whose path is not a constant", "Unknown", 15),
            ]
        );
    }

    /// eShop's typed client: a base path in a `readonly` field, the URL built in a local.
    #[test]
    fn client_urls_read_readonly_fields_and_locals() {
        let src = r#"
public class CatalogService(HttpClient httpClient)
{
    private readonly string remoteServiceBaseUrl = "api/catalog/";
    public Task<Item> GetCatalogItem(int id)
    {
        var uri = $"{remoteServiceBaseUrl}items/{id}";
        return httpClient.GetFromJsonAsync<Item>(uri);
    }
    public Task Reassigned(int id)
    {
        var uri = "a/b";
        uri = Pick(id);
        return httpClient.GetAsync(uri);
    }
}
"#;
        let f = extract(src);
        // A local assigned again holds a URL known only at run time: kept with no path.
        assert_eq!(
            endpoints(&f),
            [e("client", "GET", "{=CatalogService.remoteServiceBaseUrl}items/{}", "GetCatalogItem", 8), e("client", "GET", "", "Reassigned", 14)]
        );
        assert!(f.constants.iter().any(|c| c.qualified == "CatalogService.remoteServiceBaseUrl" && c.value == "api/catalog/"));
    }

    /// Typed HTTP clients' base addresses and an Aspire app host's project resources are host evidence.
    #[test]
    fn base_addresses_and_app_host_resources_name_hosts() {
        let src = r#"
var builder = DistributedApplication.CreateBuilder(args);
builder.AddProject<Projects.Catalog_Api>("catalog-api");
services.AddHttpClient<CatalogService>(o => o.BaseAddress = new("https+http://catalog-api"));
services.AddHttpClient<IOrders, OrdersClient>(c => { c.BaseAddress = new Uri("https://orders.example.test/"); });
services.AddHttpClient("named", c => c.BaseAddress = new Uri("https://named.example.test"));
public class SelfClient
{
    public SelfClient(HttpClient h) { h.BaseAddress = new Uri("http://self.example.test:8080"); }
}
"#;
        let f = extract(src);
        let hosts: Vec<(&str, &str, &str, Option<&str>)> = f.hosts.iter().map(|h| (h.role, h.kind, h.host.as_str(), h.target.as_deref())).collect();
        assert_eq!(
            hosts,
            [
                ("serves", "app_host", "catalog-api", None),
                ("configures", "http_client", "catalog-api", Some("CatalogService")),
                ("configures", "http_client", "orders.example.test", Some("OrdersClient")),
                ("configures", "http_client", "self.example.test:8080", Some("SelfClient")),
            ]
        );
    }

    /// Every minimal API registration with a constant template yields a route, or an `unsupported` fact when its
    /// group's prefix can't be known; one whose template isn't a constant always yields an `unsupported` fact. Over
    /// every verb, receiver, template and handler shape.
    #[test]
    fn invariant_every_map_registration_yields_a_route_or_says_unsupported() {
        let receivers = [("app", true), ("api", true), ("app.MapGroup(\"v1\")", true), ("app.MapGroup(\"v1\").RequireAuthorization()", true), ("app.MapGroup(prefix)", false), ("dyn", false), ("group", true)];
        // (template, constant, written as a string).
        let templates = [
            ("\"/a/{id:int}\"", true, true),
            ("Routes.A", true, true),
            ("$\"{nameof(Orders)}/x\"", true, true),
            ("\"/a\" + \"/b\"", true, true),
            ("path", false, false),
            ("$\"/{path}\"", false, true),
            ("Build()", false, false),
        ];
        let handlers = ["Handle", "Api.Handle", "(int id) => id", "async () => await Task.Delay(1)", "delegate { return 1; }", "this.Handle"];
        let verbs = ["MapGet", "MapPost", "MapPut", "MapDelete", "MapPatch", "Map", "MapMethods"];
        for verb in verbs {
            for (recv, known_prefix) in receivers {
                for (template, constant, stringy) in templates {
                    for handler in handlers {
                        let extra = if verb == "MapMethods" { "new[] { \"GET\" }, " } else { "" };
                        let src = format!(
                            "public static class Api\n{{\n    public static void MapAll(this RouteGroupBuilder group, string prefix, string path)\n    {{\n        var api = app.MapGroup(\"api\");\n        var dyn = app.MapGroup(prefix);\n        {recv}.{verb}({template}, {extra}{handler});\n    }}\n    static int Handle(int id) => id;\n}}\n"
                        );
                        let f = extract(&src);
                        let at: Vec<&HttpRoute> = f.http_routes.iter().filter(|r| r.line == 7 && matches!(r.side, "server" | "unsupported")).collect();
                        let case = format!("{recv}.{verb}({template}, {handler})");
                        // `x.Map(value, other)` off a route builder is an object mapper's call as much as an endpoint.
                        if verb == "Map" && recv == "app" && !stringy {
                            assert!(at.is_empty(), "{case}: {at:?}");
                            continue;
                        }
                        assert_eq!(at.len(), 1, "{case}: {:?}", f.http_routes);
                        let want = if constant && known_prefix { "server" } else { "unsupported" };
                        assert_eq!(at[0].side, want, "{case}: {:?}", at[0]);
                        if want == "server" {
                            assert!(!at[0].path.is_empty() && !at[0].path.starts_with('/'), "{case}: {:?}", at[0]);
                        }
                        // A method group handler owns its route.
                        if handler.contains("Handle") {
                            assert_eq!(at[0].sym.map(|s| f.symbols[s].name.as_str()), Some("Handle"), "{case}");
                        }
                    }
                }
            }
        }
    }

    /// A method group used as a delegate is always a reference, wherever it is passed and however it is written.
    #[test]
    fn invariant_a_method_group_used_as_a_delegate_is_a_reference() {
        let sites = [
            "bus.Subscribe(Handle);",
            "bus.Subscribe<Msg>(Handle);",
            "Task.Run(Handle);",
            "_ = new Thread(Handle);",
            "Run(1, Handle);",
            "Run(callback: Handle);",
            "bus.Changed += Handle;",
            "bus.Changed -= Handle;",
            "app.MapGet(\"/x\", Handle);",
            "Run(this.Handle);",
            "Run(Worker.Handle);",
            "list.ForEach(Handle);",
            "await Task.WhenAll(items.Select(Handle));",
        ];
        for site in sites {
            let src = format!("public class Worker\n{{\n    public async Task Start(Bus bus, List<int> list)\n    {{\n        {site}\n    }}\n    public static void Handle(int x) {{ }}\n}}\n");
            let f = extract(&src);
            let found = f.refs.iter().any(|r| r.name == "Handle" && r.line == 5 && matches!(r.kind, RefKind::Member | RefKind::Call));
            assert!(found, "{site}: {:?}", f.refs.iter().filter(|r| r.line == 5).collect::<Vec<_>>());
        }
    }
}


