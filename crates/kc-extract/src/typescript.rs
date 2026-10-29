//! TypeScript extraction over tree-sitter-typescript (and its TSX grammar): syntactic, shallower than Swift. Plain
//! JavaScript (`.js`, `.mjs`, `.cjs`, `.jsx`) is read with the TSX grammar, which parses JavaScript with JSX; a
//! minified or bundled file (see [`is_minified`]) gives no facts.
//!
//! Classes, interfaces (as protocols), enums, type aliases, functions and `const f = () => …` (as functions),
//! methods, constructors (as `init`), fields, accessors and module-level constants (as properties). Angular's
//! dependency injection types fields: `constructor(private repo: Repo)` and `repo = inject(Repo)` both give a `repo`
//! property of type `Repo`, so `this.repo.load()` resolves. Decorators are attributes. Bare identifiers are locals
//! or module names, never members: TypeScript reaches members through `this`.

use std::collections::{HashMap, HashSet};

use tree_sitter::{Node, Parser};

use crate::swift::{children, collapse, line, name_col, starts_upper};
use crate::types::{PathRoot, Seg, TypeExpr};
use crate::urls::{self, Piece};
use crate::{
    DiBinding, FileFacts, HttpRoute, Import, ImportBinding, NgFact, Ref, RefKind, Relation, RelationKind, StringConst, Symbol,
    SymbolKind, Unsupported,
};

pub const VERSION: u32 = 27;

const SIGNATURE_CAP: usize = 240;
const RECEIVER_CAP: usize = 48;

/// A JavaScript line longer than this marks minified or bundled output.
const MINIFIED_LINE: usize = 1000;

/// Whether JavaScript source is minified or bundled rather than written: in its first 64 KB, a line over
/// [`MINIFIED_LINE`] bytes, or (past 4 KB) lines of 150 bytes on average.
pub fn is_minified(source: &str) -> bool {
    let head = &source.as_bytes()[..source.len().min(64 << 10)];
    let mut lines = 0;
    for l in head.split(|&b| b == b'\n') {
        if l.len() > MINIFIED_LINE {
            return true;
        }
        lines += 1;
    }
    head.len() > 4096 && head.len() / lines > 150
}

/// Whether JavaScript source opens with a library's license banner (`@license`, `@preserve`, "Licensed under",
/// "Released under the MIT license"): a copied library, not the repo's code.
pub fn is_library(source: &str) -> bool {
    let mut cut = source.len().min(2048);
    while !source.is_char_boundary(cut) {
        cut -= 1;
    }
    let head = source[..cut].to_ascii_lowercase();
    ["@license", "@preserve", "licensed under", "released under the", "mit license"].iter().any(|m| head.contains(m))
}

/// Library types whose uses say nothing about the code base.
const COMMON_TYPES: &[&str] = &[
    "Array", "BehaviorSubject", "Date", "Error", "EventEmitter", "Map", "Observable", "Omit", "Partial", "Pick",
    "Promise", "Readonly", "ReadonlyArray", "Record", "Required", "Set", "Signal", "Subject", "Subscription",
    "WritableSignal",
];

/// Array and Observable methods whose callback parameter is an element of the receiver.
const ELEMENT_CALLBACKS: &[&str] =
    &["every", "filter", "find", "findIndex", "findLast", "flatMap", "forEach", "map", "some", "sort", "subscribe"];

/// Test-double wrappers (`Mocked<T>`, `jasmine.SpyObj<T>`): a value of one has T's members.
const MOCK_WRAPPERS: &[&str] = &["DeepMocked", "MockProxy", "Mocked", "MockedObject", "SpyObj"];

/// RxJS operators whose callback sees the source's elements and passes them on unchanged.
const RX_PASSING: &[&str] =
    &["debounce", "distinctUntilChanged", "filter", "find", "first", "skipWhile", "takeWhile", "tap", "throttle"];

/// RxJS operators whose callback sees the source's elements and emits something else.
const RX_CHANGING: &[&str] = &["concatMap", "exhaustMap", "map", "mergeMap", "switchMap"];

/// Built-in objects used as values (`Object.keys`, `JSON.parse`): not the code base's classes.
const VALUE_GLOBALS: &[&str] = &[
    "Boolean", "Infinity", "Intl", "JSON", "Math", "NaN", "Number", "Object", "Reflect", "RegExp", "String", "Symbol",
];

const BRANCH_KINDS: &[&str] = &[
    "if_statement", "for_statement", "for_in_statement", "while_statement", "do_statement", "catch_clause",
    "ternary_expression", "switch_case",
];

/// Control-flow blocks that nest, for [`Symbol::nesting`](crate::Symbol::nesting).
const NEST_KINDS: &[&str] = &[
    "if_statement", "for_statement", "for_in_statement", "while_statement", "do_statement", "switch_statement",
    "try_statement",
];

pub fn extract(source: &str, tsx: bool) -> FileFacts {
    let mut parser = Parser::new();
    let language = if tsx { tree_sitter_typescript::LANGUAGE_TSX } else { tree_sitter_typescript::LANGUAGE_TYPESCRIPT };
    parser.set_language(&language.into()).expect("tree-sitter-typescript ABI matches tree-sitter");
    let Some(tree) = parser.parse(source, None) else {
        return FileFacts { parse_errors: 1, ..Default::default() };
    };
    let mut cx = Cx {
        src: source,
        http: crate::conventions::current(),
        facts: FileFacts::default(),
        scopes: Vec::new(),
        seen: HashSet::new(),
        url_locals: HashMap::new(),
        const_urls: HashMap::new(),
        plain: std::cell::Cell::new(false),
        globals: HashMap::new(),
        field: None,
        action_locals: HashMap::new(),
    };
    cx.visit(tree.root_node());
    cx.facts
}

struct Scope {
    sym: usize,
    type_name: Option<String>,
    callable: bool,
    locals: HashMap<String, TypeExpr>,
    /// Parameters and locals of unknown type: they hide a file-level name of the same spelling.
    untyped: HashSet<String>,
}

struct Cx<'a> {
    src: &'a str,
    /// The conventions recognised by name (see [`crate::conventions`]); this extractor reads `ts_http`.
    http: std::sync::Arc<crate::conventions::Conventions>,
    facts: FileFacts,
    scopes: Vec<Scope>,
    seen: HashSet<(Option<usize>, String, RefKind, u32, u32)>,
    /// Locals holding a URL or part of one, by enclosing symbol and name (see `url_local`).
    url_locals: HashMap<(Option<usize>, String), Vec<Piece>>,
    /// `const` locals holding a URL, by the node id of the function declaring them and name: what a request whose
    /// URL is just the name reads (see `const_url`).
    const_urls: HashMap<(usize, String), ConstUrl>,
    /// Reading a `const` URL's initializer: names in it are plain values (see `const_url`).
    plain: std::cell::Cell<bool>,
    /// Typed variables outside any function: module-level ones, and those a test suite declares in its `describe`
    /// callback (`let service: Service;`, or assigned `service = TestBed.inject(Service)` in `beforeEach`).
    globals: HashMap<String, TypeExpr>,
    /// The class field whose initializer is being visited, for telling the callbacks run while it is evaluated from
    /// those it only stores (see `deferred`).
    field: Option<Field>,
    /// Locals holding an action a call created (`const action = Group.load(…)`), by enclosing symbol and name: the
    /// creator, its receiver as written, and the values its arguments are made of; dispatched later, perhaps.
    action_locals: HashMap<(Option<usize>, String), Created>,
}

/// A local a request's URL is read from: its name and the line declaring it.
type Local = (String, u32);

/// A `const` local holding a URL (see [`Cx::const_url`]).
struct ConstUrl {
    pieces: Vec<Piece>,
    line: u32,
    /// The block declaring it: a use outside it is another variable's.
    scope: std::ops::Range<usize>,
    /// Where the declaration ends: a use before it is another variable's.
    after: usize,
}

/// An action a call creates: the creator, its receiver as written (empty for none), and the values its arguments are
/// made of.
type Created = (String, String, Vec<String>);

/// A class field being initialized.
struct Field {
    /// The field's symbol.
    prop: usize,
    /// The field declaration's node id: where looking up from a callback stops.
    node: usize,
}

/// Calls that run the callback they are given while a field is initialized, or right after: signals and
/// subscriptions. Any other callback in an initializer (an RxJS operator's in a `pipe`, a handler stored in a `Map` or
/// an object) runs later, if at all.
const EAGER_CALLS: &[&str] = &[
    "toSignal", "effect", "computed", "linkedSignal", "subscribe", "afterNextRender", "afterRender", "resource",
    "rxResource", "createEffect",
];

/// The namespaces enclosing `node`, outermost first, joined by `.` (`Outer.Inner`); `None` at a module's top.
fn namespaces_around(src: &str, node: Node) -> Option<String> {
    let mut names = Vec::new();
    let mut at = node.parent();
    while let Some(p) = at {
        if matches!(p.kind(), "internal_module" | "module")
            && let Some(n) = p.child_by_field_name("name").filter(|n| n.kind() != "string")
        {
            names.push(&src[n.byte_range()]);
        }
        at = p.parent();
    }
    names.reverse();
    (!names.is_empty()).then(|| names.join("."))
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

    fn in_callable(&self) -> bool {
        self.scopes.iter().any(|s| s.callable)
    }

    fn visit(&mut self, node: Node) {
        self.visit_decl(node, node);
    }

    /// `outer` is the node a declaration's header, doc comment and decorators hang off: the `export` statement
    /// around it, or the node itself.
    fn visit_decl(&mut self, node: Node, outer: Node) {
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
        match node.kind() {
            "export_statement" => match node.child_by_field_name("declaration") {
                Some(d) => self.visit_decl(d, node),
                None => {
                    if let Some(src) = node.child_by_field_name("source") {
                        let module = self.text(src).trim_matches(['"', '\'', '`']).to_string();
                        self.bindings(node, &module, true);
                    }
                    self.visit_children(node)
                }
            },
            "class_declaration" | "abstract_class_declaration" | "class" => self.class(node, outer),
            "interface_declaration" => self.interface(node, outer),
            // `namespace Shapes { … }`, `declare namespace Express { … }`, `module Legacy { … }`: a symbol for the
            // namespace, named as written within the namespaces around it, so a type named through it
            // (`Shapes.Circle`) is found among the declarations it spans. Its members keep their own containers: a
            // namespace scopes names, it is no type. `declare module "pkg"` names a module, not a namespace.
            "internal_module" | "module" => {
                if let Some(n) = node.child_by_field_name("name").filter(|n| n.kind() == "string") {
                    let module = self.text(n).trim_matches(['"', '\'', '`']).to_string();
                    self.facts.augments.push(Import { module, line: line(node) });
                }
                if let Some(n) = node.child_by_field_name("name").filter(|n| n.kind() != "string") {
                    let container = namespaces_around(self.src, node);
                    self.push(node, outer, self.text(n).to_string(), SymbolKind::Module, container, None);
                }
                self.visit_children(node)
            }
            "enum_declaration" => {
                let Some(n) = node.child_by_field_name("name") else { return };
                let name = self.text(n).to_string();
                let sym = self.push(node, outer, name.clone(), SymbolKind::Enum, self.enclosing_type(), None);
                if let Some(body) = node.child_by_field_name("body") {
                    for c in children(body) {
                        let m = if c.kind() == "enum_assignment" { c.child_by_field_name("name") } else { Some(c) };
                        if let Some(m) = m.filter(|m| matches!(m.kind(), "property_identifier" | "string")) {
                            let case = self.text(m).trim_matches(['"', '\'']).to_string();
                            let cs = self.push(c, c, case, SymbolKind::Case, Some(name.clone()), None);
                            self.facts.symbols[cs].parent = Some(sym);
                        }
                    }
                }
            }
            "type_alias_declaration" => {
                let Some(n) = node.child_by_field_name("name") else { return };
                let name = self.text(n).to_string();
                let value = node.child_by_field_name("value");
                let sym = self.push(node, outer, name.clone(), SymbolKind::Typealias, self.enclosing_type(), value);
                self.facts.symbols[sym].type_name = value.and_then(|v| type_expr(self.src, v)).map(|t| t.encode());
                if let Some(v) = value {
                    // `type TaskProps = { task: TaskInfo }`: its properties are members of the alias, as an interface's
                    // are, so a value of the type (`({ task }: TaskProps)`) has a known `task`.
                    if v.kind() == "object_type" {
                        self.in_scope(sym, Some(name.clone()), false, HashMap::new(), |cx| {
                            for m in children(v) {
                                cx.member(m, &name);
                            }
                        });
                    } else {
                        self.in_scope(sym, None, false, HashMap::new(), |cx| cx.visit(v));
                    }
                }
            }
            "function_declaration" | "generator_function_declaration" | "function_signature" => {
                let Some(n) = node.child_by_field_name("name") else { return };
                let name = self.text(n).to_string();
                if !self.in_callable() && self.enclosing_type().is_none() {
                    self.query_appender(node, &name);
                }
                self.callable(node, outer, name, SymbolKind::Function, None, node);
            }
            "lexical_declaration" | "variable_declaration" => {
                for d in children(node).filter(|c| c.kind() == "variable_declarator") {
                    self.declarator(d, outer);
                }
            }
            "import_statement" => {
                if let Some(src) = node.child_by_field_name("source") {
                    let module = self.text(src).trim_matches(['"', '\'', '`']).to_string();
                    self.facts.imports.push(Import { module: collapse(&module, 120), line: line(node) });
                    self.bindings(node, &module, false);
                }
            }
            "call_expression" => {
                self.call(node);
                self.http_call(node);
                self.opens(node);
                self.payload(node);
                self.action_group(node);
                if self.field.is_some() {
                    self.filter_branch(node);
                }
                if let Some(f) = node.child_by_field_name("function") {
                    match f.kind() {
                        "member_expression" => {
                            if let Some(o) = f.child_by_field_name("object") {
                                self.visit(o);
                            }
                        }
                        "identifier" => {}
                        _ => self.visit(f),
                    }
                }
                self.callback_params(node);
                for field in ["type_arguments", "arguments"] {
                    if let Some(c) = node.child_by_field_name(field) {
                        self.visit(c);
                    }
                }
            }
            "new_expression" => {
                if let Some(c) = node.child_by_field_name("constructor") {
                    let name = match c.kind() {
                        "identifier" => Some(self.text(c).to_string()),
                        "member_expression" => c.child_by_field_name("property").map(|p| self.text(p).to_string()),
                        _ => None,
                    };
                    if let Some(name) = name {
                        self.add(name, None, None, RefKind::Instantiate, c);
                    }
                    if c.kind() == "member_expression" && let Some(o) = c.child_by_field_name("object") {
                        self.visit(o);
                    }
                }
                for field in ["type_arguments", "arguments"] {
                    if let Some(c) = node.child_by_field_name(field) {
                        self.visit(c);
                    }
                }
            }
            "member_expression" => {
                if let Some(p) = node.child_by_field_name("property") {
                    let o = node.child_by_field_name("object");
                    let receiver = o.map(|o| collapse(self.text(o), RECEIVER_CAP));
                    let rt = o.and_then(|o| self.expr_type(o)).map(|t| t.encode());
                    let name = self.text(p).trim_start_matches('#').to_string();
                    self.add(name, receiver, rt, RefKind::Member, p);
                }
                if let Some(o) = node.child_by_field_name("object") {
                    self.visit(o);
                }
            }
            // `<Page …>`, `</Page>`, `<Dialog.Title />`: rendering a component calls it, so its tag is a call (a member
            // of its namespace for `Ns.Comp`), as the compiler binds it, and not the type reference a capitalised name
            // used as a value is. Attributes and children are visited as before.
            "jsx_opening_element" | "jsx_self_closing_element" | "jsx_closing_element" => {
                let tag = node.child_by_field_name("name");
                if let Some(t) = tag {
                    self.jsx_tag(t);
                }
                for c in children(node).filter(|c| Some(c.id()) != tag.map(|t| t.id())) {
                    self.visit(c);
                }
            }
            // A class used as a value (`component: X`, `open(X)`, `declarations: [X]`): a type reference.
            "identifier" => {
                let name = self.text(node);
                if starts_upper(name) && !COMMON_TYPES.contains(&name) && !VALUE_GLOBALS.contains(&name) {
                    self.add(name.to_string(), None, None, RefKind::TypeRef, node);
                }
            }
            "object" if is_route(self.src, node) => self.route(node, ""),
            // In a field initializer (an NgRx effect), branches on a value: which calls run for which value.
            "switch_statement" if self.field.is_some() => {
                self.switch_branches(node);
                self.visit_children(node);
            }
            "if_statement" if self.field.is_some() => {
                self.if_branch(node);
                self.visit_children(node);
            }
            // `providers: [Facade, …]`: each class listed provides itself there.
            "pair" if self.providers_key(node) => {
                if let Some(v) = node.child_by_field_name("value") {
                    for c in children(v).filter(|c| c.kind() == "identifier") {
                        let name = self.text(c);
                        if starts_upper(name) {
                            let b = DiBinding { service: Some(name.to_string()), implementation: Some(name.to_string()), lifetime: None, kind: "type", line: line(c), made: None, service_made: false, mode: None };
                            self.facts.di.push(b);
                        }
                    }
                }
                self.visit_children(node);
            }
            // `{ provide: X, useClass: Y }`, in any providers array.
            "object" if is_provider(self.src, node) => {
                self.provider(node);
                self.visit_children(node);
            }
            "type_identifier" => {
                let name = self.text(node);
                if !COMMON_TYPES.contains(&name) {
                    self.add(name.to_string(), None, None, RefKind::TypeRef, node);
                }
            }
            // `ns.Type`, `globalThis.Event`: the name is looked up in its qualifier, never in the scope around it.
            "nested_type_identifier" => {
                if let (Some(n), Some(q)) = (node.child_by_field_name("name"), node.child_by_field_name("module")) {
                    let qualifier = self.text(q).to_string();
                    self.add(self.text(n).to_string(), Some(qualifier), None, RefKind::TypeRef, n);
                }
            }
            // A callback (`(control: AbstractControl) => …`): its parameters are locals of the enclosing function.
            "arrow_function" | "function_expression" => {
                // A callback a field's initializer stores rather than runs (`lazy`: an RxJS operator's, run when the
                // observable is subscribed; `stored`: kept in a `Map` or object, run when called), or one a method
                // registers for later (`this.handlers.set('renew', { run: () => … })`): a `deferred` fact of the
                // field or method with the callback's lines, so what it calls isn't taken for what building the
                // object runs.
                let how = match self.field.as_ref().filter(|f| self.current() == Some(f.prop)) {
                    Some(f) => self.deferred(node, f.node),
                    None => self.current().filter(|&m| self.stored_in_method(node, m)).map(|_| "stored"),
                };
                if let (Some(how), Some(sym)) = (how, self.current()) {
                    let (start, end) = (line(node), node.end_position().row as u32 + 1);
                    self.facts.ng.push(NgFact { kind: "deferred", sym: Some(sym), value: how.to_string(), extra: Some(format!("{start}-{end}")), line: start });
                }
                self.bind_params(node);
                self.visit_children(node);
            }
            // `service = TestBed.inject(Service)` types a variable declared without a type.
            "assignment_expression" => {
                if let (Some(l), Some(r)) = (node.child_by_field_name("left"), node.child_by_field_name("right"))
                    && l.kind() == "identifier"
                    && let Some(t) = self.expr_type(r)
                {
                    self.bind_assigned(self.text(l).to_string(), t);
                }
                self.visit_children(node);
            }
            "for_in_statement" => {
                if let (Some(l), Some(r)) = (node.child_by_field_name("left"), node.child_by_field_name("right"))
                    && l.kind() == "identifier"
                    && children(node).any(|c| c.kind() == "of")
                    && let Some(t) = self.expr_type(r)
                {
                    self.bind(self.text(l).to_string(), TypeExpr::Elem(Box::new(t)));
                }
                self.visit_children(node);
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
        self.in_scope_with(sym, type_name, callable, locals, HashSet::new(), f)
    }

    fn in_scope_with(
        &mut self,
        sym: usize,
        type_name: Option<String>,
        callable: bool,
        locals: HashMap<String, TypeExpr>,
        untyped: HashSet<String>,
        f: impl FnOnce(&mut Self),
    ) {
        self.scopes.push(Scope { sym, type_name, callable, locals, untyped });
        f(self);
        self.scopes.pop();
    }

    fn bind(&mut self, name: String, ty: TypeExpr) {
        if let Some(s) = self.scopes.iter_mut().rev().find(|s| s.callable) {
            s.locals.insert(name, ty);
        }
    }

    /// A local declared without a type, which hides a file-level variable of the same name.
    fn bind_untyped(&mut self, name: String) {
        if let Some(s) = self.scopes.iter_mut().rev().find(|s| s.callable) {
            s.untyped.insert(name);
        }
    }

    fn bind_params(&mut self, f: Node) {
        if let Some(params) = f.child_by_field_name("parameters") {
            for p in children(params) {
                let Some(n) = p.child_by_field_name("pattern").filter(|n| n.kind() == "identifier") else { continue };
                let name = self.text(n).to_string();
                match p.child_by_field_name("type").and_then(|t| type_expr(self.src, t)) {
                    Some(t) => self.bind(name, t),
                    None => self.bind_untyped(name),
                }
            }
        } else if let Some(p) = f.child_by_field_name("parameter").filter(|p| p.kind() == "identifier") {
            self.bind_untyped(self.text(p).to_string());
        }
    }

    /// An assignment to a variable whose type isn't known yet: a local declared without one, or else a variable
    /// outside any function (a test suite's `let service;`).
    fn bind_assigned(&mut self, name: String, ty: TypeExpr) {
        for s in self.scopes.iter_mut().rev() {
            if s.locals.contains_key(&name) {
                return;
            }
            if s.untyped.remove(&name) {
                s.locals.insert(name, ty);
                return;
            }
        }
        self.globals.entry(name).or_insert(ty);
    }

    /// The names an `import` (or, with `export`, a re-export) binds from `module`.
    fn bindings(&mut self, node: Node, module: &str, export: bool) {
        let l = line(node);
        let module = collapse(module, 120);
        let src = self.src;
        let text = |n: Node| &src[n.byte_range()];
        let mut out: Vec<(String, String)> = Vec::new();
        for c in children(node) {
            match c.kind() {
                "import_clause" => {
                    for b in children(c) {
                        match b.kind() {
                            "identifier" => out.push((text(b).to_string(), "default".to_string())),
                            "namespace_import" => {
                                if let Some(id) = children(b).find(|x| x.kind() == "identifier") {
                                    out.push((text(id).to_string(), "*".to_string()));
                                }
                            }
                            "named_imports" => {
                                for spec in children(b).filter(|x| x.kind() == "import_specifier") {
                                    let Some(n) = spec.child_by_field_name("name") else { continue };
                                    let local = spec.child_by_field_name("alias").unwrap_or(n);
                                    out.push((text(local).to_string(), text(n).to_string()));
                                }
                            }
                            _ => {}
                        }
                    }
                }
                "export_clause" => {
                    for spec in children(c).filter(|x| x.kind() == "export_specifier") {
                        let Some(n) = spec.child_by_field_name("name") else { continue };
                        let local = spec.child_by_field_name("alias").unwrap_or(n);
                        out.push((text(local).to_string(), text(n).to_string()));
                    }
                }
                "namespace_export" => {
                    if let Some(id) = children(c).find(|x| x.is_named()) {
                        out.push((text(id).to_string(), "*".to_string()));
                    }
                }
                "*" if export && !children(node).any(|x| x.kind() == "namespace_export") => out.push(("*".to_string(), "*".to_string())),
                _ => {}
            }
        }
        for (local, name) in out {
            self.facts.bindings.push(ImportBinding { local, name, module: module.clone(), line: l, export });
        }
    }

    fn push(&mut self, node: Node, outer: Node, name: String, kind: SymbolKind, container: Option<String>, header_stop: Option<Node>) -> usize {
        let end = node.child_by_field_name("body").or(header_stop).map_or(node.end_byte(), |b| b.start_byte());
        let header = self.src[outer.start_byte()..end.max(outer.start_byte())].trim_end_matches(['=', '>', ' ', '{', ';', ':', '\n']);
        let attrs = decorators(self.src, node, outer);
        self.facts.symbols.push(Symbol {
            parent: self.current(),
            name,
            kind,
            container,
            signature: collapse(header, SIGNATURE_CAP),
            doc: doc_comment(self.src, outer),
            attrs,
            tags: Vec::new(),
            start_line: line(outer),
            end_line: node.end_position().row as u32 + 1,
            start_byte: outer.start_byte() as u32,
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

    fn class(&mut self, node: Node, outer: Node) {
        let Some(n) = node.child_by_field_name("name") else { return self.visit_children(node) };
        let name = self.text(n).to_string();
        let container = self.enclosing_type();
        let sym = self.push(node, outer, name.clone(), SymbolKind::Class, container, None);
        self.request_contract(sym, node, outer);
        self.angular_component(sym, node, outer);
        self.injectable(&name, node, outer);
        if let Some(h) = children(node).find(|c| c.kind() == "class_heritage") {
            for clause in children(h) {
                for t in children(clause).filter(|c| c.is_named() && c.kind() != "type_arguments") {
                    let target = base_type_name(self.src, t);
                    if !target.is_empty() {
                        self.facts.relations.push(Relation { from: sym, target, kind: RelationKind::Inherits, line: line(t) });
                    }
                }
            }
        }
        let body = node.child_by_field_name("body");
        let decorators: Vec<Node> = children(outer).chain(if outer != node { children(node).collect() } else { Vec::new() })
            .filter(|c| c.kind() == "decorator")
            .collect();
        self.in_scope(sym, Some(name.clone()), false, HashMap::new(), |cx| {
            // Decorator arguments (`@NgModule({ declarations: [X] })`) are uses from the class.
            for d in decorators {
                cx.visit_children(d);
            }
            if let Some(h) = children(node).find(|c| c.kind() == "class_heritage") {
                cx.heritage_refs(h);
            }
            for m in body.iter().flat_map(|b| children(*b)) {
                cx.member(m, &name);
            }
        });
    }

    /// `@Component({ selector, templateUrl | template })`: the selector, the template file as written, and the custom
    /// elements and directive attributes an inline template uses.
    fn angular_component(&mut self, sym: usize, node: Node, outer: Node) {
        let mut decorators: Vec<Node> = children(outer).filter(|c| c.kind() == "decorator").collect();
        if outer != node {
            decorators.extend(children(node).filter(|c| c.kind() == "decorator"));
        }
        for d in decorators {
            let Some(call) = children(d).find(|c| c.kind() == "call_expression") else { continue };
            if call.child_by_field_name("function").is_none_or(|f| !matches!(self.text(f), "Component" | "Directive")) {
                continue;
            }
            let Some(obj) = call.child_by_field_name("arguments").and_then(|a| children(a).find(|c| c.kind() == "object")) else {
                continue;
            };
            // `selector: SELECTORS` or `{ selector }` with an imported constant: resolved at query time.
            let shorthand = children(obj).find(|c| c.kind() == "shorthand_property_identifier" && self.text(*c) == "selector");
            let named = children(obj).filter(|c| c.kind() == "pair").find_map(|p| {
                (p.child_by_field_name("key").is_some_and(|k| self.text(k) == "selector"))
                    .then(|| p.child_by_field_name("value").filter(|v| v.kind() == "identifier"))
                    .flatten()
            });
            if let Some(n) = shorthand.or(named) {
                let name = imported_name(self.src, obj, self.text(n));
                self.facts.ng.push(NgFact { kind: "selector", sym: Some(sym), value: format!("{{={name}}}"), extra: None, line: line(n) });
            }
            for pair in children(obj).filter(|c| c.kind() == "pair") {
                let (Some(k), Some(v)) = (pair.child_by_field_name("key"), pair.child_by_field_name("value")) else { continue };
                let Some(value) = string_literal(self.src, v) else { continue };
                let fact = |kind, value: String| NgFact { kind, sym: Some(sym), value, extra: None, line: line(pair) };
                match self.text(k) {
                    "selector" => self.facts.ng.push(fact("selector", value)),
                    "templateUrl" => self.facts.ng.push(fact("template", value)),
                    "template" => self.facts.ng.extend(crate::html::template_facts(&value, Some(sym), line(v) - 1)),
                    _ => {}
                }
            }
        }
    }

    /// A route object (`{ path, component | loadComponent | loadChildren | children }`): records its full path within
    /// this file and what it loads, then its children under that path.
    fn route(&mut self, obj: Node, prefix: &str) {
        let pairs: Vec<(String, Node)> = children(obj)
            .filter(|c| c.kind() == "pair")
            .filter_map(|p| Some((self.text(p.child_by_field_name("key")?).trim_matches(['"', '\'']).to_string(), p.child_by_field_name("value")?)))
            .collect();
        let get = |k: &str| pairs.iter().find(|(key, _)| key == k).map(|(_, v)| *v);
        let path = get("path").and_then(|v| string_literal(self.src, v)).unwrap_or_default();
        let full = [prefix, path.as_str()].iter().filter(|p| !p.is_empty()).copied().collect::<Vec<_>>().join("/");
        let target = if let Some(c) = get("component").filter(|c| c.kind() == "identifier") {
            Some((self.text(c).to_string(), false))
        } else if let Some(p) = get("loadComponent").and_then(|v| lazy_class(self.src, v)) {
            self.add(self.text(p).to_string(), None, None, RefKind::TypeRef, p);
            Some((self.text(p).to_string(), false))
        } else if let Some(p) = get("loadChildren").and_then(|v| lazy_class(self.src, v)) {
            self.add(self.text(p).to_string(), None, None, RefKind::TypeRef, p);
            Some((self.text(p).to_string(), true))
        } else if let Some((p, from)) = get("loadChildren").and_then(|v| lazy_routes(self.src, v)) {
            // A routes constant (`.then(m => m.routes)`): names are often shared, so the import path says which.
            Some((format!("{}@{from}", self.text(p)), true))
        } else {
            // What a route loads in a form this doesn't read (a default export, a module path string, a qualified
            // component): said, so the route table isn't taken for complete.
            if let Some((key, v)) = pairs.iter().find(|(k, _)| matches!(k.as_str(), "component" | "loadComponent" | "loadChildren")) {
                let construct = format!("route {:?} loading {key}: {}", full, collapse(self.text(*v), 120));
                self.facts.unsupported.push(Unsupported { area: "angular", construct, line: line(obj) });
            }
            None
        };
        if let Some((name, lazy)) = target {
            let extra = if lazy { format!("loads:{name}") } else { name };
            self.facts.ng.push(NgFact { kind: "route", sym: None, value: full.clone(), extra: Some(extra), line: line(obj) });
        }
        for (key, v) in &pairs {
            match key.as_str() {
                "children" => {
                    for child in children(*v).filter(|c| c.kind() == "object") {
                        if is_route(self.src, child) {
                            self.route(child, &full);
                        } else {
                            self.visit(child);
                        }
                    }
                }
                "loadComponent" | "loadChildren" | "path" => {}
                _ => self.visit(*v),
            }
        }
    }

    /// A configured request decorator (`request_decorators`), `@Request({ endpoint, httpMethod })`: the class is a
    /// request the app sends, to the path and with the verb its object's `path_keys` and `verb_keys` hold.
    fn request_contract(&mut self, sym: usize, node: Node, outer: Node) {
        let conv = self.http.clone();
        let http = &conv.ts_http;
        if http.request_decorators.is_empty() {
            return;
        }
        let mut decorators: Vec<Node> = children(outer).filter(|c| c.kind() == "decorator").collect();
        if outer != node {
            decorators.extend(children(node).filter(|c| c.kind() == "decorator"));
        }
        for d in decorators {
            let Some(call) = children(d).find(|c| c.kind() == "call_expression") else { continue };
            if call.child_by_field_name("function").is_none_or(|f| !http.request_decorators.iter().any(|d| d == self.text(f))) {
                continue;
            }
            let Some(obj) = call.child_by_field_name("arguments").and_then(|a| children(a).find(|c| c.kind() == "object")) else {
                continue;
            };
            let (mut path, mut path_ref, mut verb) = (String::new(), None, None);
            for pair in children(obj).filter(|c| c.kind() == "pair") {
                let (Some(k), Some(v)) = (pair.child_by_field_name("key"), pair.child_by_field_name("value")) else { continue };
                let key = self.text(k);
                if http.path_keys.iter().any(|p| p == key) {
                    match string_literal(self.src, v) {
                        Some(lit) => path = lit.trim_matches('/').to_string(),
                        None if matches!(v.kind(), "member_expression" | "identifier") => path_ref = Some(self.text(v).to_string()),
                        None => {}
                    }
                } else if http.verb_keys.iter().any(|p| p == key) {
                    let m = match v.kind() {
                        "member_expression" => v.child_by_field_name("property").map(|p| self.text(p).to_string()),
                        _ => string_literal(self.src, v),
                    };
                    verb = m.map(|m| m.to_ascii_uppercase());
                }
            }
            if let Some(verb) = verb.filter(|_| !path.is_empty() || path_ref.is_some()) {
                self.facts.http_routes.push(HttpRoute { sym: Some(sym), side: "client", verb, path, path_ref, service: None, service_ref: None, line: line(d) });
            }
        }
    }

    /// `extends Base<T> implements I`: type references from the declaring type, so `refs` finds subclasses and
    /// implementers.
    fn heritage_refs(&mut self, heritage: Node) {
        for clause in children(heritage) {
            for t in children(clause).filter(|c| c.is_named()) {
                match t.kind() {
                    // `extends` takes an expression: a class name, or `ns.Class`.
                    "identifier" | "member_expression" => {
                        let target = base_type_name(self.src, t);
                        if !target.is_empty() && !COMMON_TYPES.contains(&target.as_str()) {
                            self.add(target, None, None, RefKind::TypeRef, t);
                        }
                    }
                    _ => self.visit(t),
                }
            }
        }
    }

    fn member(&mut self, m: Node, class: &str) {
        let container = Some(class.to_string());
        match m.kind() {
            "method_definition" | "method_signature" | "abstract_method_signature" => {
                let Some(n) = m.child_by_field_name("name") else { return };
                let raw = self.text(n).trim_start_matches('#').to_string();
                let accessor = children(m).any(|c| matches!(c.kind(), "get" | "set"));
                let (name, kind) = match raw.as_str() {
                    "constructor" => ("init".to_string(), SymbolKind::Init),
                    _ if accessor => (raw, SymbolKind::Property),
                    _ => (raw, SymbolKind::Method),
                };
                // A getter and its setter are one property.
                if kind == SymbolKind::Property
                    && self.facts.symbols.iter().any(|s| s.kind == kind && s.name == name && s.container == container)
                {
                    return self.visit_children(m);
                }
                if kind == SymbolKind::Init {
                    self.parameter_properties(m, class);
                }
                let at = self.facts.symbols.len();
                self.callable(m, m, name, kind, container, m);
                // A method's decorators (`@HostListener(…)`) precede it in the class body.
                self.facts.symbols[at].attrs = member_decorators(self.src, m);
            }
            "public_field_definition" | "property_signature" => {
                let Some(n) = m.child_by_field_name("name") else { return };
                let name = self.text(n).trim_start_matches('#').to_string();
                let value = m.child_by_field_name("value");
                if let Some(v) = value.filter(|v| matches!(v.kind(), "arrow_function" | "function_expression")) {
                    self.callable(v, m, name, SymbolKind::Method, container, m);
                    return;
                }
                // `static readonly x = 'Value'`: a constant routes can name.
                if let Some(v) = value.and_then(|v| string_literal(self.src, v))
                    && children(m).any(|c| c.kind() == "static")
                {
                    self.facts.constants.push(StringConst { qualified: format!("{class}.{name}"), value: v, line: line(m) });
                }
                let stop = value.or_else(|| m.child_by_field_name("type"));
                let sym = self.push(m, m, name, SymbolKind::Property, container, stop.filter(|_| value.is_some()));
                let ty = m.child_by_field_name("type").and_then(|t| type_expr(self.src, t)).or_else(|| value.and_then(|v| self.expr_type(v)));
                self.facts.symbols[sym].type_name = ty.map(|t| t.encode());
                let outer = self.field.replace(Field { prop: sym, node: m.id() });
                self.in_scope(sym, None, true, HashMap::new(), |cx| {
                    for c in children(m).filter(|c| c.kind() != "decorator" && Some(*c) != m.child_by_field_name("name")) {
                        cx.visit(c);
                    }
                });
                self.field = outer;
            }
            "decorator" | "{" | "}" | ";" | "comment" => {}
            _ => self.visit(m),
        }
    }

    /// `constructor(private repo: Repo)`: parameters with an accessibility or `readonly` modifier are fields.
    fn parameter_properties(&mut self, ctor: Node, class: &str) {
        let Some(params) = ctor.child_by_field_name("parameters") else { return };
        for p in children(params).filter(|c| matches!(c.kind(), "required_parameter" | "optional_parameter")) {
            let modified = children(p).any(|c| matches!(c.kind(), "accessibility_modifier" | "readonly" | "override_modifier"));
            let Some(n) = p.child_by_field_name("pattern").or_else(|| p.child_by_field_name("name")).filter(|n| n.kind() == "identifier") else { continue };
            if !modified {
                continue;
            }
            let name = self.text(n).to_string();
            let sym = self.push(p, p, name, SymbolKind::Property, Some(class.to_string()), None);
            self.facts.symbols[sym].type_name = p.child_by_field_name("type").and_then(|t| type_expr(self.src, t)).map(|t| t.encode());
        }
    }

    fn interface(&mut self, node: Node, outer: Node) {
        let Some(n) = node.child_by_field_name("name") else { return };
        let name = self.text(n).to_string();
        let sym = self.push(node, outer, name.clone(), SymbolKind::Protocol, self.enclosing_type(), None);
        for clause in children(node).filter(|c| c.kind() == "extends_type_clause") {
            for t in children(clause).filter(|c| c.is_named()) {
                let target = base_type_name(self.src, t);
                if !target.is_empty() {
                    self.facts.relations.push(Relation { from: sym, target, kind: RelationKind::Inherits, line: line(t) });
                }
            }
        }
        let body = node.child_by_field_name("body");
        self.in_scope(sym, Some(name.clone()), false, HashMap::new(), |cx| {
            for t in children(node).filter(|c| c.kind() == "extends_type_clause").flat_map(children).filter(|t| t.is_named()) {
                cx.visit(t);
            }
            for m in body.iter().flat_map(|b| children(*b)) {
                cx.member(m, &name);
            }
        });
    }

    /// A function-like symbol: a declaration, method, or arrow function bound to a name. `f` holds the parameters,
    /// return type and body; `outer` the header.
    fn callable(&mut self, f: Node, outer: Node, name: String, kind: SymbolKind, container: Option<String>, header: Node) {
        let sym = self.push(f, outer, name, kind, container.clone(), None);
        if header != f {
            // An arrow function bound to a field or const: the symbol spans the whole declaration.
            self.facts.symbols[sym].end_line = header.end_position().row as u32 + 1;
        }
        let ret = f.child_by_field_name("return_type").and_then(|t| type_expr(self.src, t)).map(|t| match t {
            TypeExpr::Named(n) if n == "this" => container.clone().map_or(TypeExpr::Named(n), TypeExpr::Named),
            t => t,
        });
        self.facts.symbols[sym].type_name = ret.map(|t| t.encode());
        let mut locals = HashMap::new();
        let mut untyped = HashSet::new();
        if let Some(params) = f.child_by_field_name("parameters") {
            for p in children(params) {
                // `({ task, item: it }: TaskProps)`: each name bound is the annotated type's property of its key.
                if let Some(pattern) = p.child_by_field_name("pattern").filter(|n| n.kind() == "object_pattern")
                    && let Some(t) = p.child_by_field_name("type").and_then(|t| type_expr(self.src, t))
                {
                    for (key, local) in destructured(self.src, pattern) {
                        locals.insert(local, TypeExpr::root(PathRoot::Expr(Box::new(t.clone()))).then(Seg::Member(key)));
                    }
                    continue;
                }
                let Some(n) = p.child_by_field_name("pattern").or_else(|| p.child_by_field_name("name")).filter(|n| n.kind() == "identifier")
                else {
                    continue;
                };
                match p.child_by_field_name("type").and_then(|t| type_expr(self.src, t)) {
                    Some(t) => {
                        locals.insert(self.text(n).to_string(), t);
                    }
                    None => {
                        untyped.insert(self.text(n).to_string());
                    }
                }
            }
        } else if let Some(p) = f.child_by_field_name("parameter").filter(|p| p.kind() == "identifier") {
            untyped.insert(self.text(p).to_string());
        }
        self.in_scope_with(sym, None, true, locals, untyped, |cx| {
            for c in children(f) {
                if !matches!(c.kind(), "decorator" | "property_identifier" | "private_property_identifier" | "identifier")
                    || Some(c) == f.child_by_field_name("body")
                {
                    cx.visit(c);
                }
            }
        });
    }

    /// `xs.filter(x => …)` and `xs$.subscribe(x => …)` bind `x` to an element of the receiver, as do the callbacks
    /// of RxJS operators in `xs$.pipe(…)` until an operator changes the element type.
    fn callback_params(&mut self, call: Node) {
        let Some(f) = call.child_by_field_name("function").filter(|f| f.kind() == "member_expression") else { return };
        let Some(method) = f.child_by_field_name("property").map(|p| self.text(p)) else { return };
        let Some(args) = call.child_by_field_name("arguments") else { return };
        if !ELEMENT_CALLBACKS.contains(&method) && method != "pipe" {
            return;
        }
        let Some(receiver) = f.child_by_field_name("object").and_then(|o| self.expr_type(o)) else { return };
        let elem = TypeExpr::Elem(Box::new(receiver));
        if method != "pipe" {
            for a in children(args).filter(|a| matches!(a.kind(), "arrow_function" | "function_expression")) {
                self.bind_first_param(a, elem.clone());
            }
            return;
        }
        for op in children(args).filter(|a| a.is_named()) {
            let name = op.child_by_field_name("function").filter(|_| op.kind() == "call_expression").map(|n| self.text(n));
            let Some(name) = name.filter(|n| RX_PASSING.contains(n) || RX_CHANGING.contains(n)) else { break };
            if let Some(cb) = op
                .child_by_field_name("arguments")
                .and_then(|a| children(a).find(|c| matches!(c.kind(), "arrow_function" | "function_expression")))
            {
                self.bind_first_param(cb, elem.clone());
            }
            if RX_CHANGING.contains(&name) {
                break;
            }
        }
    }

    fn bind_first_param(&mut self, f: Node, ty: TypeExpr) {
        let param = f.child_by_field_name("parameter").or_else(|| {
            f.child_by_field_name("parameters")
                .and_then(|ps| children(ps).find(|c| matches!(c.kind(), "required_parameter" | "optional_parameter")))
                .and_then(|p| p.child_by_field_name("pattern"))
        });
        if let Some(p) = param.filter(|p| p.kind() == "identifier") {
            self.bind(self.text(p).to_string(), ty);
        }
    }

    /// `const x: T = …` binds a typed local inside a function; at module level it declares a constant, or a
    /// function when its value is an arrow function or function expression.
    fn declarator(&mut self, d: Node, outer: Node) {
        let Some(n) = d.child_by_field_name("name") else { return self.visit_children(d) };
        let value = d.child_by_field_name("value");
        let declared = d.child_by_field_name("type").and_then(|t| type_expr(self.src, t));
        if n.kind() != "identifier" {
            return self.visit_children(d);
        }
        let name = self.text(n).to_string();
        if self.in_callable() {
            if let Some(v) = value {
                self.url_local(&name, v);
                self.const_url(d, &name, v);
                if let Some(a) = self.created_action(v) {
                    self.action_locals.insert((self.current(), name.clone()), a);
                }
            }
            match declared.or_else(|| value.and_then(|v| self.expr_type(v))) {
                Some(t) => self.bind(name, t),
                None => self.bind_untyped(name),
            }
            for c in children(d).filter(|c| *c != n) {
                self.visit(c);
            }
            return;
        }
        let container = self.enclosing_type();
        if let Some(v) = value.filter(|v| matches!(v.kind(), "arrow_function" | "function_expression")) {
            let hdr = if outer == d { d.parent().unwrap_or(d) } else { outer };
            return self.callable(v, hdr, name, SymbolKind::Function, container, d);
        }
        let hdr = if outer == d { d.parent().unwrap_or(d) } else { outer };
        if container.is_none()
            && let Some(v) = value
        {
            // `{ … } as const`: the object.
            let v = if matches!(v.kind(), "as_expression" | "satisfies_expression") { children(v).find(|c| c.is_named()).unwrap_or(v) } else { v };
            match v.kind() {
                // `export const Api = { search: 'Search' }`: each string property is a constant.
                "object" => {
                    for pair in children(v).filter(|c| c.kind() == "pair") {
                        let (Some(k), Some(pv)) = (pair.child_by_field_name("key"), pair.child_by_field_name("value")) else { continue };
                        let key = string_literal(self.src, k).unwrap_or_else(|| self.text(k).to_string());
                        if let Some(val) = string_literal(self.src, pv) {
                            self.facts.constants.push(StringConst { qualified: format!("{name}.{key}"), value: val, line: line(pair) });
                        }
                    }
                }
                _ => {
                    if let Some(val) = string_literal(self.src, v) {
                        self.facts.constants.push(StringConst { qualified: name.clone(), value: val, line: line(d) });
                    } else if matches!(v.kind(), "template_string" | "member_expression" | "binary_expression")
                        && let Some(val) = urls::encode(&self.url_pieces(v), true)
                    {
                        // `const API = environment.apiUrl`, `` const BASE = `${environment.apiUrl}/v2` ``: a URL
                        // prefix, encoded as a request path is.
                        self.facts.constants.push(StringConst { qualified: name.clone(), value: val, line: line(d) });
                    }
                }
            }
        }
        let ty = declared.or_else(|| value.and_then(|v| self.expr_type(v)));
        if container.is_none()
            && let Some(t) = &ty
        {
            self.globals.insert(name.clone(), t.clone());
        }
        let sym = self.push(d, hdr, name, SymbolKind::Property, container, value);
        self.facts.symbols[sym].type_name = ty.map(|t| t.encode());
        self.in_scope(sym, None, true, HashMap::new(), |cx| {
            for c in children(d).filter(|c| *c != n) {
                cx.visit(c);
            }
        });
    }

    fn expr_type(&self, e: Node) -> Option<TypeExpr> {
        match e.kind() {
            "this" => Some(TypeExpr::root(PathRoot::SelfType)),
            "identifier" => {
                let name = self.text(e);
                for s in self.scopes.iter().rev() {
                    if let Some(t) = s.locals.get(name) {
                        return Some(t.clone());
                    }
                    if s.untyped.contains(name) {
                        return None;
                    }
                }
                self.globals.get(name).cloned()
            }
            "await_expression" | "parenthesized_expression" | "non_null_expression" => {
                children(e).find(|c| c.is_named()).and_then(|c| self.expr_type(c))
            }
            "as_expression" | "satisfies_expression" => {
                children(e).filter(|c| c.is_named()).nth(1).and_then(|t| type_expr(self.src, t))
            }
            "new_expression" => {
                let c = e.child_by_field_name("constructor")?;
                // `new InjectionToken<T>(…)`: what `inject(TOKEN)` gives is a T.
                if self.text(c) == "InjectionToken"
                    && let Some(t) = e
                        .child_by_field_name("type_arguments")
                        .and_then(|a| children(a).find(|c| c.is_named()))
                        .and_then(|t| type_expr(self.src, t))
                {
                    return Some(t);
                }
                if c.kind() != "identifier" {
                    return None;
                }
                // `new Map<K, V>()`, `new BehaviorSubject<T>(…)`: what the declared type would say.
                let args: Vec<TypeExpr> = e
                    .child_by_field_name("type_arguments")
                    .map(|a| children(a).filter(|c| c.is_named()).filter_map(|c| type_expr(self.src, c)).collect())
                    .unwrap_or_default();
                generic(self.text(c), &args)
            }
            "member_expression" => {
                let p = self.text(e.child_by_field_name("property")?).trim_start_matches('#').to_string();
                let o = e.child_by_field_name("object")?;
                match self.expr_type(o) {
                    Some(t) => Some(t.then(Seg::Member(p))),
                    None if o.kind() == "identifier" && starts_upper(self.text(o)) => {
                        Some(TypeExpr::root(PathRoot::Static(self.text(o).to_string())).then(Seg::Member(p)))
                    }
                    None => None,
                }
            }
            "subscript_expression" => Some(self.expr_type(e.child_by_field_name("object")?)?.then(Seg::Index)),
            "call_expression" => {
                let f = e.child_by_field_name("function")?;
                match f.kind() {
                    "identifier" => {
                        let name = self.text(f);
                        // Angular's `inject(Service)`.
                        if name == "inject"
                            && let Some(t) = self.injected(e)
                        {
                            return Some(t);
                        }
                        Some(TypeExpr::Call { owner: None, name: name.to_string() })
                    }
                    "member_expression" => {
                        let name = self.text(f.child_by_field_name("property")?).trim_start_matches('#').to_string();
                        let o = f.child_by_field_name("object")?;
                        // `TestBed.inject(Service)`, `fixture.debugElement.injector.get(Service)`.
                        let injector = self.text(o) == "TestBed" || self.text(o).to_ascii_lowercase().ends_with("injector");
                        if (name == "inject" || (name == "get" && injector))
                            && let Some(t) = self.injected(e)
                        {
                            return Some(t);
                        }
                        match self.expr_type(o) {
                            Some(t) => Some(t.then(Seg::Call(name))),
                            None if o.kind() == "identifier" && starts_upper(self.text(o)) => {
                                Some(TypeExpr::Call { owner: Some(self.text(o).to_string()), name })
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

    /// Whether the callback `f`, in the method or constructor `m` of a class, is registered for later: held in an
    /// object literal or array (or given directly) passed to a call storing it (`set`, `push`, `add`, `register`,
    /// `addHandler`, `on`) or to `new Map(…)`/`new Set(…)`.
    fn stored_in_method(&self, f: Node, m: usize) -> bool {
        let s = &self.facts.symbols[m];
        if !matches!(s.kind, SymbolKind::Method | SymbolKind::Init) || s.container.is_none() || s.parent.is_none() {
            return false;
        }
        let mut n = f.parent();
        while let Some(p) = n {
            match p.kind() {
                "pair" | "object" | "array" | "arguments" => {}
                "call_expression" => {
                    let name = p.child_by_field_name("function").filter(|c| c.kind() == "member_expression").and_then(|c| c.child_by_field_name("property"));
                    return name.is_some_and(|x| matches!(self.text(x), "set" | "push" | "add" | "register" | "addHandler" | "on"));
                }
                "new_expression" => return p.child_by_field_name("constructor").is_some_and(|c| matches!(self.text(c), "Map" | "Set")),
                _ => return false,
            }
            n = p.parent();
        }
        false
    }

    /// A `providers: [...]` or `viewProviders: [...]` pair.
    fn providers_key(&self, pair: Node) -> bool {
        pair.child_by_field_name("key").is_some_and(|k| matches!(self.text(k), "providers" | "viewProviders"))
            && pair.child_by_field_name("value").is_some_and(|v| v.kind() == "array")
    }

    /// Whether the callback `f`, inside the initializer of the field declared at node `field`, is stored rather than
    /// run (no call between it and the field is one of [`EAGER_CALLS`]): `lazy` inside a `pipe`, `stored` otherwise.
    fn deferred(&self, f: Node, field: usize) -> Option<&'static str> {
        let mut n = f.parent();
        let mut how = "stored";
        while let Some(p) = n {
            if p.id() == field {
                return Some(how);
            }
            if p.kind() == "call_expression"
                && let Some(c) = p.child_by_field_name("function")
            {
                let name = match c.kind() {
                    "member_expression" => c.child_by_field_name("property").map_or("", |x| self.text(x)),
                    _ => self.text(c),
                };
                if EAGER_CALLS.contains(&name) {
                    return None;
                }
                if name == "pipe" {
                    how = "lazy";
                }
            }
            n = p.parent();
        }
        Some(how)
    }

    /// The service an injection call (`inject(Service)`, `inject(Store<State>)`) asks for.
    fn injected(&self, call: Node) -> Option<TypeExpr> {
        let mut arg = call.child_by_field_name("arguments").and_then(|a| children(a).find(|c| c.is_named()))?;
        if arg.kind() == "instantiation_expression" {
            arg = children(arg).find(|c| c.is_named())?;
        }
        let name = match arg.kind() {
            "identifier" => self.text(arg),
            "member_expression" => self.text(arg.child_by_field_name("property")?),
            _ => return None,
        };
        starts_upper(name).then(|| TypeExpr::Named(name.to_string()))
    }

    fn call(&mut self, node: Node) {
        let Some(f) = node.child_by_field_name("function") else { return };
        match f.kind() {
            "identifier" => {
                let name = self.text(f).to_string();
                self.add(name, None, None, RefKind::Call, f);
            }
            "member_expression" => {
                let Some(p) = f.child_by_field_name("property") else { return };
                let o = f.child_by_field_name("object");
                let receiver = o.map(|o| collapse(self.text(o), RECEIVER_CAP));
                let rt = o.and_then(|o| {
                    self.expr_type(o)
                        .or_else(|| (o.kind() == "identifier" && starts_upper(self.text(o))).then(|| TypeExpr::Named(self.text(o).to_string())))
                });
                let name = self.text(p).trim_start_matches('#').to_string();
                self.add(name, receiver, rt.map(|t| t.encode()), RefKind::Call, p);
            }
            _ => {}
        }
    }

    /// A JSX tag's name: a component (`Page`) is called, a namespaced one (`Dialog.Title`) is a member of its
    /// namespace; an intrinsic element (`div`) or a namespaced name (`svg:rect`) names no declaration.
    fn jsx_tag(&mut self, tag: Node) {
        match tag.kind() {
            "identifier" if starts_upper(self.text(tag)) => self.add(self.text(tag).to_string(), None, None, RefKind::Call, tag),
            "member_expression" => {
                let (Some(o), Some(p)) = (tag.child_by_field_name("object"), tag.child_by_field_name("property")) else { return };
                let receiver = Some(collapse(self.text(o), RECEIVER_CAP));
                let rt = self.expr_type(o).map(|t| t.encode());
                self.add(self.text(p).to_string(), receiver, rt, RefKind::Member, p);
                self.visit(o);
            }
            _ => {}
        }
    }

    fn add(&mut self, name: String, receiver: Option<String>, receiver_type: Option<String>, kind: RefKind, at: Node) {
        let from = self.current();
        let (l, col) = (line(at), name_col(self.src, at, &name));
        if self.seen.insert((from, name.clone(), kind, l, col)) {
            self.facts.refs.push(Ref { from, name, receiver, receiver_type, kind, line: l, col, args: None });
        }
    }
}

/// Requests a method makes with an HTTP client and a URL: Angular's `HttpClient`, `fetch`, axios (and instances of it),
/// jQuery's `$.ajax`, superagent and wrappers named like them; and operations of generated API clients.
impl Cx<'_> {
    /// `this.http.get<T>(`${API}/orders/${id}`)`, `http.request('POST', url)`, `fetch(url, { method: 'POST' })`,
    /// `axios.post(url)`, `axios({ url, method })`, `api.get('/orders')`, `$.ajax({ url, type: 'POST' })`: a client
    /// route of the enclosing method, when the receiver is named for HTTP (or an API client) and the URL is a string
    /// built from literals and constants. A call that isn't one of those may be an operation of a generated client
    /// (see [`Self::operation_call`]).
    fn http_call(&mut self, node: Node) {
        let Some(f) = node.child_by_field_name("function") else { return };
        let args: Vec<Node> = node.child_by_field_name("arguments").map(|a| children(a).filter(|c| c.is_named() && c.kind() != "comment").collect()).unwrap_or_default();
        let (receiver, method) = match f.kind() {
            "member_expression" => {
                let (Some(o), Some(p)) = (f.child_by_field_name("object"), f.child_by_field_name("property")) else { return };
                (Some(o), self.text(p).to_string())
            }
            "identifier" => (None, self.text(f).to_string()),
            _ => return,
        };
        let recv = receiver.map(|o| self.text(o).rsplit('.').next().unwrap_or_default().to_ascii_lowercase());
        let named = |r: &str, words: &[&str]| words.iter().any(|w| r.contains(w));
        // `fetch(…)`, `window.fetch(…)`, `axios(…)`, `$.ajax(…)`: always HTTP. Otherwise the receiver says so.
        let fetch = method == "fetch" && recv.as_deref().is_none_or(|r| matches!(r, "window" | "globalthis" | "self" | "this"));
        let direct = recv.is_none() && matches!(method.as_str(), "axios" | "ajax" | "superagent" | "ky");
        let strong = recv.as_deref().is_some_and(|r| r == "$" || r == "jquery" || named(r, &["http", "axios", "ajax", "superagent"]));
        // `this.send(…)` in an API wrapper's own methods: the class is the client, named like one (`ApiService`).
        let own = (recv.as_deref() == Some("this")).then(|| self.enclosing_type()).flatten().map(|t| t.to_ascii_lowercase());
        let weak = recv.as_deref().is_some_and(|r| named(r, &["api", "client", "request", "fetch", "rest"]))
            || own.as_deref().is_some_and(|t| named(t, &["api", "client", "http"]));
        if !(fetch || direct || strong || weak) {
            if let Some(o) = receiver {
                self.operation_call(node, o, &method);
            }
            return;
        }
        let verb_of = |m: &str| urls::verb(m);
        // The verb of `fetch(url, init)`: `init.method`, GET without one.
        let init_verb = |cx: &Self, init: Option<&Node>| -> Option<String> {
            match init.filter(|i| i.kind() == "object").and_then(|i| object_pairs(cx.src, *i).into_iter().find(|(k, _)| k == "method")) {
                Some((_, v)) => string_literal(cx.src, v).and_then(|m| verb_of(&m)),
                None => Some("GET".to_string()),
            }
        };
        let (verb, url) = match (method.as_str(), args.as_slice()) {
            ("fetch", [url, rest @ ..]) if fetch || strong || weak => (init_verb(self, rest.first()), *url),
            ("request" | "ajax" | "axios" | "superagent" | "ky", [obj, ..]) if obj.kind() == "object" => match request_config(self.src, *obj) {
                Some(c) => c,
                None => return,
            },
            ("request" | "send", [m, url, ..]) if string_literal(self.src, *m).is_some_and(|m| verb_of(&m).is_some()) => {
                (string_literal(self.src, *m).and_then(|m| verb_of(&m)), *url)
            }
            ("axios" | "ajax" | "request" | "ky", [url, rest @ ..]) => match rest.first().filter(|o| o.kind() == "object").and_then(|o| config_verb(self.src, *o)) {
                Some(v) => (Some(v), *url),
                None => (Some("GET".to_string()), *url),
            },
            (m @ ("get" | "post" | "put" | "delete" | "patch" | "head"), [url, ..]) => (verb_of(m), *url),
            ("getJSON", [url, ..]) => (Some("GET".to_string()), *url),
            ("del", [url, ..]) => (Some("DELETE".to_string()), *url),
            _ => {
                if let Some(o) = receiver {
                    self.operation_call(node, o, &method);
                }
                return;
            }
        };
        let Some(verb) = verb else { return };
        let sym = self.current();
        let surely = fetch || direct || strong;
        let Some((pieces, local)) = self.request_pieces(url) else { return };
        // A client named for an API passed no literal text (`this.ordersClient.delete(id)`): an operation, not a URL.
        let named = |p: &Piece| matches!(p, Piece::Text(t) if !t.is_empty()) || matches!(p, Piece::Ref(r) if starts_upper(r));
        if !surely && !pieces.iter().any(named) {
            return self.operation_call(node, receiver.unwrap_or(f), &method);
        }
        let Some(path) = urls::encode(&pieces, true) else {
            // A request whose URL is only known at run time: kept with no path, so an answer can say it wasn't read.
            if surely {
                self.facts.http_routes.push(HttpRoute { sym, side: "client", verb, path: String::new(), path_ref: None, service: None, service_ref: None, line: line(node) });
            }
            return;
        };
        // A bare word (`api.get('orders')`) is a path only from a client that is surely HTTP.
        let bare = !path.contains(['/', '?', '{']) && surely && path.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        if urls::looks_like_path(&path) || bare {
            // A URL read from a local: the local and its line, after the path (`{~url@44}`), for the query to name.
            let path = match local {
                Some((name, at)) => format!("{path}{{~{name}@{at}}}"),
                None => path,
            };
            self.facts.http_routes.push(HttpRoute { sym, side: "client", verb, path, path_ref: None, service: None, service_ref: None, line: line(node) });
        }
    }

    /// A request's URL as pieces, and the local it was read from (its name and line) when the URL is just a name
    /// (`fetch(url)`, `{ url }`) or a URL builder given one (`apiClient.getUrl(url)`). Such a name is read only as a
    /// `const` URL of the function making the request (see [`Self::const_url`]). Otherwise a name passed directly
    /// is read as any URL is (a local of the method, a module constant named in capitals), and a URL builder or a
    /// `{ url }` option given one isn't read (`None`).
    fn request_pieces(&self, url: Node) -> Option<(Vec<Piece>, Option<Local>)> {
        let named = |n: Node| self.const_url_at(n).map(|c| (c.pieces.clone(), (self.text(n).to_string(), c.line)));
        if matches!(url.kind(), "identifier" | "shorthand_property_identifier") {
            return match named(url) {
                Some((pieces, local)) => Some((pieces, Some(local))),
                None if url.kind() == "identifier" => Some((self.url_pieces(url), None)),
                None => None,
            };
        }
        if let Some(arg) = (url.kind() == "call_expression").then(|| self.url_builder_arg(url)).flatten().filter(|a| a.kind() == "identifier")
            && let Some((pieces, local)) = named(arg)
        {
            let mut out = vec![Piece::Unknown, Piece::Text("/".to_string())];
            out.extend(pieces);
            return Some((out, Some(local)));
        }
        Some((self.url_pieces(url), None))
    }

    /// `getItemsApi(api).getItems(…)`, `new OrdersClient(base).list()`, `this.ordersApi.getOrder(id)`, `apiClient.getItems()`:
    /// an operation of a generated API client (its code is a package, not indexed), recorded as an `operation` route
    /// of the enclosing method: `path` is the operation, `service` the tag the client is named for (`Items`), when it
    /// names one. The linker matches it to the action of that name, under the controller named like the tag.
    fn operation_call(&mut self, node: Node, receiver: Node, method: &str) {
        const NOT_OPERATIONS: &[&str] = &[
            "then", "catch", "finally", "subscribe", "send", "request", "ajax", "fetch", "getJSON", "pipe", "toPromise", "bind", "call", "apply", "toString", "valueOf", "on", "off", "emit",
            "next", "complete", "use", "interceptors", "defaults", "setHeader", "setHeaders", "getUri", "getUrl", "buildUrl",
            "setConfiguration", "getConfiguration", "configure", "init", "initialize", "dispose", "destroy", "close", "open",
        ];
        if NOT_OPERATIONS.contains(&method) || !method.starts_with(|c: char| c.is_ascii_lowercase()) || method.len() < 4 {
            return;
        }
        let client = match receiver.kind() {
            // `getItemsApi(api)`, `ItemsApiFactory(config)`.
            "call_expression" => receiver.child_by_field_name("function").map(|f| self.text(f).rsplit('.').next().unwrap_or_default().to_string()),
            "new_expression" => receiver.child_by_field_name("constructor").map(|c| self.text(c).rsplit('.').next().unwrap_or_default().to_string()),
            "identifier" | "member_expression" => Some(self.text(receiver).rsplit('.').next().unwrap_or_default().to_string()),
            _ => None,
        };
        let Some(client) = client else { return };
        let Some(tag) = api_tag(&client) else { return };
        let sym = self.current();
        self.facts.http_routes.push(HttpRoute {
            sym,
            side: "operation",
            verb: String::new(),
            path: method.to_string(),
            path_ref: None,
            service: (!tag.is_empty()).then_some(tag),
            service_ref: None,
            line: line(node),
        });
    }

    /// A URL expression as pieces: literal text, constants (`Api.orders`, `API`, `this.BASE`, a local `const` holding
    /// one), and anything else unknown. Template strings and `+` concatenations are walked.
    fn url_pieces(&self, e: Node) -> Vec<Piece> {
        let mut out = Vec::new();
        self.push_pieces(e, &mut out);
        out
    }

    fn push_pieces(&self, e: Node, out: &mut Vec<Piece>) {
        if let Some(s) = string_literal(self.src, e) {
            return out.push(Piece::Text(s));
        }
        match e.kind() {
            "template_string" => {
                for c in children(e) {
                    match c.kind() {
                        "string_fragment" | "escape_sequence" => out.push(Piece::Text(self.text(c).to_string())),
                        "template_substitution" => match children(c).find(|x| x.is_named()) {
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
            "identifier" | "shorthand_property_identifier" => {
                let name = self.text(e);
                match self.url_locals.get(&(self.current(), name.to_string())) {
                    // In a `const` URL's initializer, another local is a plain value.
                    Some(pieces) if !self.plain.get() => out.extend(pieces.iter().cloned()),
                    _ if is_constant_name(name) => out.push(Piece::Ref(name.to_string())),
                    _ => out.push(Piece::Unknown),
                }
            }
            // `apiClient.getUrl('Items/Latest', query)`, `buildUrl(`orders/${id}`)`: a base address and the path given.
            "call_expression" => match self.url_builder_arg(e) {
                Some(path) => {
                    out.push(Piece::Unknown);
                    out.push(Piece::Text("/".to_string()));
                    match self.const_url_at(path) {
                        Some(c) => out.extend(c.pieces.iter().cloned()),
                        None => self.push_pieces(path, out),
                    }
                }
                None => match self.wrapped_path(e) {
                    // `addQuery('/events', start)`: the path given, and what the function adds, which the query
                    // reads from its declaration (see `query_appender`) or reports as unread.
                    Some((f, path)) => {
                        self.push_pieces(path, out);
                        out.push(Piece::Ref(format!("{f}()")));
                    }
                    None => out.push(Piece::Call),
                },
            },
            // `isAdmin ? '/sync?all=true' : '/sync'`: one path when both give the same one, its query aside.
            "ternary_expression" => {
                let (Some(a), Some(b)) = (e.child_by_field_name("consequence"), e.child_by_field_name("alternative")) else {
                    return out.push(Piece::Unknown);
                };
                let (pa, pb) = (self.url_pieces(a), self.url_pieces(b));
                let path = |p: &[Piece]| urls::encode(p, true).map(|u| u.split('?').next().unwrap_or_default().to_string());
                match (path(&pa), path(&pb)) {
                    (Some(x), Some(y)) if x == y => {
                        // The pieces up to the query.
                        for p in pa {
                            if let Piece::Text(t) = &p
                                && let Some((before, _)) = t.split_once('?')
                            {
                                out.push(Piece::Text(before.to_string()));
                                break;
                            }
                            out.push(p);
                        }
                    }
                    _ => out.push(Piece::Unknown),
                }
            }
            // `new URL('orders', base)`: the path.
            "new_expression" if e.child_by_field_name("constructor").is_some_and(|c| self.text(c) == "URL") => {
                match e.child_by_field_name("arguments").and_then(|a| children(a).find(|c| c.is_named())) {
                    Some(u) => {
                        out.push(Piece::Unknown);
                        out.push(Piece::Text("/".to_string()));
                        self.push_pieces(u, out);
                    }
                    None => out.push(Piece::Unknown),
                }
            }
            "member_expression" if self.text(e).chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == '$') => {
                out.push(Piece::Ref(self.text(e).trim_start_matches("this.").to_string()))
            }
            _ => out.push(Piece::Unknown),
        }
    }

    /// The path argument of a call building a URL from a base address: a function named for a URL (`getUrl`,
    /// `buildUrl`, `apiUrl`, `endpoint`) given a string first, or a `const` URL of the function (see
    /// [`Self::const_url`]).
    fn url_builder_arg<'t>(&self, call: Node<'t>) -> Option<Node<'t>> {
        let f = call.child_by_field_name("function")?;
        let name = self.text(f).rsplit('.').next().unwrap_or_default().to_ascii_lowercase();
        let builds = ["url", "uri", "endpoint"].iter().any(|w| name.contains(w))
            && !name.starts_with("encode")
            && !name.starts_with("decode")
            && !name.contains("objecturl");
        if !builds {
            return None;
        }
        let first = call.child_by_field_name("arguments").and_then(|a| children(a).find(|c| c.is_named() && c.kind() != "comment"))?;
        (matches!(first.kind(), "string" | "template_string" | "binary_expression") || self.const_url_at(first).is_some()).then_some(first)
    }

    /// `addQuery('/events', start, end)`: a plain function (not a method) given a path with literal text first, which
    /// may build a URL from it: the function's name and that argument.
    fn wrapped_path<'t>(&self, call: Node<'t>) -> Option<(String, Node<'t>)> {
        let f = call.child_by_field_name("function").filter(|f| f.kind() == "identifier")?;
        let first = call.child_by_field_name("arguments").and_then(|a| children(a).find(|c| c.is_named() && c.kind() != "comment"))?;
        let text = self.url_pieces(first).iter().any(|p| matches!(p, Piece::Text(t) if t.contains('/')));
        text.then(|| (self.text(f).to_string(), first))
    }

    /// `function addQuery(base: string, …) { if (start) base += "?start=" + start; …; return base; }`: a function
    /// returning its first parameter with only a query string appended (each append starting with `?` or `&`, or
    /// following one in its block). Recorded as the constant `addQuery()` with no value, so a URL it builds from a
    /// path is that path (see [`Self::wrapped_path`]); any other function's URL is left unread.
    fn query_appender(&mut self, node: Node, name: &str) {
        let first = node
            .child_by_field_name("parameters")
            .and_then(|ps| children(ps).find(|c| matches!(c.kind(), "required_parameter" | "optional_parameter")))
            .and_then(|p| p.child_by_field_name("pattern"))
            .filter(|p| p.kind() == "identifier");
        let (Some(p), Some(body)) = (first, node.child_by_field_name("body")) else { return };
        let p = self.text(p).to_string();
        let mut returns = 0;
        if body.kind() == "statement_block" && self.appends_query(body, &p, false, &mut returns) && returns > 0 {
            self.facts.constants.push(StringConst { qualified: format!("{name}()"), value: String::new(), line: line(node) });
        }
    }

    /// Whether the statements of `block` change `p` only by appending a query string, and return it unchanged.
    fn appends_query(&self, block: Node, p: &str, mut query: bool, returns: &mut u32) -> bool {
        // `?…`, `&…`, or `cond ? "&" : "?"`.
        let starts_query = |e: Node| -> bool {
            let begins = |e: Node| matches!(self.url_pieces(e).first(), Some(Piece::Text(t)) if t.starts_with(['?', '&']));
            begins(e)
                || (e.kind() == "ternary_expression"
                    && [e.child_by_field_name("consequence"), e.child_by_field_name("alternative")].iter().all(|b| b.is_some_and(begins)))
        };
        for s in children(block).filter(|c| c.is_named() && c.kind() != "comment") {
            match s.kind() {
                "expression_statement" => {
                    let Some(e) = children(s).find(|c| c.is_named()) else { continue };
                    let target = e.child_by_field_name("left").is_some_and(|l| self.text(l) == p);
                    match e.kind() {
                        "augmented_assignment_expression" if target => {
                            let op = e.child_by_field_name("operator").map(|o| self.text(o));
                            let Some(r) = e.child_by_field_name("right") else { return false };
                            if op != Some("+=") || !(query || starts_query(r)) {
                                return false;
                            }
                            query = true;
                        }
                        _ if assigns(self.src, s, p) => return false,
                        _ => {}
                    }
                }
                "if_statement" => {
                    let parts = [s.child_by_field_name("consequence"), s.child_by_field_name("alternative").and_then(|a| children(a).find(|c| c.is_named()))];
                    for b in parts.into_iter().flatten() {
                        let ok = match b.kind() {
                            "statement_block" => self.appends_query(b, p, query, returns),
                            _ => !assigns(self.src, b, p) && !self.has_return(b),
                        };
                        if !ok {
                            return false;
                        }
                    }
                    if s.child_by_field_name("condition").is_some_and(|c| assigns(self.src, c, p)) {
                        return false;
                    }
                }
                "return_statement" => {
                    let returned = children(s).find(|c| c.is_named());
                    if !returned.is_some_and(|r| r.kind() == "identifier" && self.text(r) == p) {
                        return false;
                    }
                    *returns += 1;
                }
                _ if assigns(self.src, s, p) || self.has_return(s) => return false,
                _ => {}
            }
        }
        true
    }

    fn has_return(&self, n: Node) -> bool {
        n.kind() == "return_statement" || children(n).any(|c| !matches!(c.kind(), "function_expression" | "arrow_function" | "function_declaration") && self.has_return(c))
    }

    /// `const url = 'Videos/' + id + '/Subtitles/' + index;`, `const url = apiClient.getUrl('Collections', query);`: a
    /// URL in a `const` of a function, remembered for a request in that function whose URL is just its name, or a URL
    /// builder given it ([`Self::request_pieces`]). The function must bind the name only there (no parameter, no other
    /// declaration, no assignment), and the request must come after it in its block. The initializer is a URL as
    /// [`Self::url_local`] reads one, with other names in it plain values: one naming a local holding URL text (a
    /// local built from another local) isn't read.
    fn const_url(&mut self, d: Node, name: &str, value: Node) {
        let Some(decl) = d.parent().filter(|p| p.kind() == "lexical_declaration") else { return };
        if decl.child_by_field_name("kind").is_none_or(|k| self.text(k) != "const") {
            return;
        }
        let (Some(f), Some(block)) = (enclosing_function(d), decl.parent()) else { return };
        if binds(self.src, f, name) != 1 || f.child_by_field_name("body").is_some_and(|b| assigns(self.src, b, name)) {
            return;
        }
        // The part of the initializer the URL is read from: a builder's path, or the whole of it.
        let read = match value.kind() {
            "string" | "template_string" | "member_expression" | "binary_expression" | "ternary_expression" => value,
            "call_expression" => match self.url_builder_arg(value).or_else(|| self.wrapped_path(value).map(|(_, p)| p)) {
                Some(p) => p,
                None => return,
            },
            "new_expression" if value.child_by_field_name("constructor").is_some_and(|c| self.text(c) == "URL") => {
                match value.child_by_field_name("arguments").and_then(|a| children(a).find(|c| c.is_named())) {
                    Some(p) => p,
                    None => return,
                }
            }
            _ => return,
        };
        if self.names_url_text(read) {
            return;
        }
        self.plain.set(true);
        let pieces = self.url_pieces(value);
        self.plain.set(false);
        if pieces.iter().any(|p| !matches!(p, Piece::Unknown)) {
            let c = ConstUrl { pieces, line: line(d), scope: block.byte_range(), after: d.end_byte() };
            self.const_urls.insert((f.id(), name.to_string()), c);
        }
    }

    /// Whether `e` names a local holding URL text (outside the functions in it).
    fn names_url_text(&self, e: Node) -> bool {
        match e.kind() {
            "identifier" | "shorthand_property_identifier" => {
                let text = |p: &[Piece]| p.iter().any(|p| matches!(p, Piece::Text(_)));
                self.const_url_at(e).is_some_and(|c| text(&c.pieces))
                    || self.url_locals.get(&(self.current(), self.text(e).to_string())).is_some_and(|p| text(p))
            }
            k if FUNCTION_KINDS.contains(&k) => false,
            _ => children(e).any(|c| self.names_url_text(c)),
        }
    }

    /// The `const` URL a name refers to where it is used (see [`Self::const_url`]).
    fn const_url_at(&self, n: Node) -> Option<&ConstUrl> {
        if !matches!(n.kind(), "identifier" | "shorthand_property_identifier") {
            return None;
        }
        let f = enclosing_function(n)?;
        let c = self.const_urls.get(&(f.id(), self.text(n).to_string()))?;
        (n.start_byte() >= c.after && c.scope.contains(&n.start_byte())).then_some(c)
    }

    /// `const endpoint = Api.payouts;` in a function: remembered, for a URL built from it later in that function.
    fn url_local(&mut self, name: &str, value: Node) {
        let builder = |v: Node| match v.kind() {
            "call_expression" => self.url_builder_arg(v).is_some(),
            "new_expression" => v.child_by_field_name("constructor").is_some_and(|c| self.text(c) == "URL"),
            _ => false,
        };
        let wrapped = |v: Node| v.kind() == "call_expression" && self.wrapped_path(v).is_some();
        if matches!(value.kind(), "string" | "template_string" | "member_expression" | "binary_expression" | "ternary_expression") || builder(value) || wrapped(value) {
            let pieces = self.url_pieces(value);
            if pieces.iter().any(|p| !matches!(p, Piece::Unknown)) {
                self.url_locals.insert((self.current(), name.to_string()), pieces);
            }
        }
    }
}

/// Angular's dependency injection and dialogs.
impl Cx<'_> {
    /// `{ provide: X, useClass: Y }`, `useExisting: Y`, `useFactory` (when what it returns is plain) and
    /// `useValue: new Y()`: the token or class `X` bound to `Y`, wherever the object is (the `providers` of an
    /// `@NgModule`, an `@Component` or a route, `bootstrapApplication`'s, a `provideX()` function's array).
    fn provider(&mut self, obj: Node) {
        let pairs = object_pairs(self.src, obj);
        let get = |k: &str| pairs.iter().find(|(key, _)| key == k).map(|(_, v)| *v);
        let Some(service) = get("provide").and_then(|v| class_name(self.src, v)) else { return };
        let (kind, implementation) = if let Some(v) = get("useClass") {
            ("type", class_name(self.src, v))
        } else if let Some(v) = get("useExisting") {
            ("existing", class_name(self.src, v))
        } else if let Some(v) = get("useFactory") {
            factory_type(self.src, v, &service)
        } else if let Some(v) = get("useValue") {
            ("instance", new_class(self.src, v))
        } else {
            return;
        };
        self.facts.di.push(DiBinding { service: Some(service), implementation, lifetime: None, kind, line: line(obj), made: None, service_made: false, mode: None });
    }

    /// `@Injectable({ providedIn: 'root' })` registers the class itself; with `useClass`, `useExisting` or
    /// `useFactory`, what the injector hands out for it instead.
    fn injectable(&mut self, class: &str, node: Node, outer: Node) {
        let mut decorators: Vec<Node> = children(outer).filter(|c| c.kind() == "decorator").collect();
        if outer != node {
            decorators.extend(children(node).filter(|c| c.kind() == "decorator"));
        }
        for d in decorators {
            let Some(call) = children(d).find(|c| c.kind() == "call_expression") else { continue };
            if call.child_by_field_name("function").is_none_or(|f| self.text(f) != "Injectable") {
                continue;
            }
            let Some(obj) = call.child_by_field_name("arguments").and_then(|a| children(a).find(|c| c.kind() == "object")) else {
                continue;
            };
            let pairs = object_pairs(self.src, obj);
            let get = |k: &str| pairs.iter().find(|(key, _)| key == k).map(|(_, v)| *v);
            let Some(scope) = get("providedIn") else { continue };
            let lifetime = string_literal(self.src, scope).is_some_and(|s| matches!(s.as_str(), "root" | "platform")).then_some("singleton");
            let (kind, implementation) = if let Some(v) = get("useClass") {
                ("type", class_name(self.src, v))
            } else if let Some(v) = get("useExisting") {
                ("existing", class_name(self.src, v))
            } else if let Some(v) = get("useFactory") {
                factory_type(self.src, v, class)
            } else {
                ("type", Some(class.to_string()))
            };
            self.facts.di.push(DiBinding { service: Some(class.to_string()), implementation, lifetime, kind, line: line(d), made: None, service_made: false, mode: None });
        }
    }

    /// `dialog.open(XComponent, …)`, `bottomSheet.open(X)`, `snackBar.openFromComponent(X)`,
    /// `modals.openDialog(X)`, `dialogs.open({ content: X })`: the enclosing symbol opens the component `X` (checked to be one at query time).
    fn opens(&mut self, call: Node) {
        let Some(f) = call.child_by_field_name("function").filter(|f| f.kind() == "member_expression") else { return };
        let Some(p) = f.child_by_field_name("property").map(|p| self.text(p).trim_start_matches('#')) else { return };
        let opener = p == "open"
            || p == "openFromComponent"
            || (p.starts_with("open") && ["Dialog", "Modal", "Drawer", "Sheet"].iter().any(|w| p.contains(w)));
        if !opener {
            return;
        }
        let Some(mut arg) = call.child_by_field_name("arguments").and_then(|a| children(a).find(|c| c.is_named() && c.kind() != "comment"))
        else {
            return;
        };
        // Kendo's `dialogs.open({ content: X, … })`.
        if arg.kind() == "object"
            && let Some((_, v)) = object_pairs(self.src, arg).into_iter().find(|(k, _)| matches!(k.as_str(), "content" | "component"))
        {
            arg = v;
        }
        let name = self.text(arg);
        if arg.kind() != "identifier" || !starts_upper(name) || VALUE_GLOBALS.contains(&name) || COMMON_TYPES.contains(&name) {
            return;
        }
        let Some(sym) = self.current() else { return };
        self.facts.ng.push(NgFact { kind: "opens", sym: Some(sym), value: name.to_string(), extra: Some(p.to_string()), line: line(arg) });
    }
}

/// What an NgRx dispatch passes and what an effect branches on, so a flow follows the branch a dispatch takes.
impl Cx<'_> {
    /// `store.dispatch(Action({ setting: 'autoRenew' }))`: a `payload` fact of the enclosing symbol, `value` the action
    /// creator, `extra` the values its arguments are made of (see [`payload_tokens`]).
    ///
    /// Every dispatch is also a `dispatch` fact: `value` the creator, `extra` its receiver as written (`AccountActions`,
    /// empty for a bare creator). An action a local holds (`const action = Group.load(…); store.dispatch(action)`)
    /// counts at the dispatch, once.
    fn payload(&mut self, call: Node) {
        let Some(f) = call.child_by_field_name("function").filter(|f| f.kind() == "member_expression") else { return };
        if f.child_by_field_name("property").is_none_or(|p| self.text(p) != "dispatch") {
            return;
        }
        let Some(arg) = call.child_by_field_name("arguments").and_then(|a| children(a).find(|c| c.is_named() && c.kind() != "comment")) else {
            return;
        };
        let found = match arg.kind() {
            "call_expression" => self.created_action(arg),
            "identifier" => self.action_locals.get(&(self.current(), self.text(arg).to_string())).cloned(),
            _ => None,
        };
        let (Some((creator, receiver, tokens)), Some(sym)) = (found, self.current()) else { return };
        if creator.is_empty() {
            return;
        }
        let l = line(arg);
        self.facts.ng.push(NgFact { kind: "dispatch", sym: Some(sym), value: creator.clone(), extra: Some(receiver), line: l });
        if !tokens.is_empty() {
            self.facts.ng.push(NgFact { kind: "payload", sym: Some(sym), value: creator, extra: Some(tokens.join(" ")), line: l });
        }
    }

    /// The action a call creates (`Group.load({ id })`, `loadOrders()`): the creator, its receiver as written (empty
    /// for none), and the values its arguments are made of.
    fn created_action(&self, call: Node) -> Option<Created> {
        if call.kind() != "call_expression" {
            return None;
        }
        let f = call.child_by_field_name("function")?;
        let (creator, receiver) = match f.kind() {
            "member_expression" => (self.text(f.child_by_field_name("property")?), f.child_by_field_name("object").map_or("", |o| self.text(o))),
            "identifier" => (self.text(f), ""),
            _ => return None,
        };
        let mut tokens = Vec::new();
        if let Some(args) = call.child_by_field_name("arguments") {
            payload_tokens(self.src, args, &mut tokens);
        }
        Some((creator.to_string(), receiver.to_string(), tokens))
    }

    /// `createActionGroup({ source, events: { 'Search Accounts': props<…>(), … } })`: an `action` fact of the
    /// enclosing symbol (the group's constant) per event, `value` the action creator's name as NgRx derives it
    /// (`searchAccounts`), or `{=Types.search}` for a key computed from a constant (resolved at query time), `extra`
    /// the source.
    fn action_group(&mut self, call: Node) {
        if call.child_by_field_name("function").is_none_or(|f| self.text(f) != "createActionGroup") {
            return;
        }
        let Some(obj) = call.child_by_field_name("arguments").and_then(|a| children(a).find(|c| c.kind() == "object")) else { return };
        let pairs = object_pairs(self.src, obj);
        let source = pairs.iter().find(|(k, _)| k == "source").and_then(|(_, v)| string_literal(self.src, *v)).unwrap_or_default();
        let Some(events) = pairs.iter().find(|(k, _)| k == "events").map(|(_, v)| *v).filter(|v| v.kind() == "object") else { return };
        let Some(sym) = self.current() else { return };
        for pair in children(events).filter(|c| matches!(c.kind(), "pair" | "method_definition")) {
            let Some(k) = pair.child_by_field_name("key").or_else(|| pair.child_by_field_name("name")) else { continue };
            let value = match k.kind() {
                "string" => string_literal(self.src, k).map(|s| action_name(&s)),
                "property_identifier" => Some(action_name(self.text(k))),
                "computed_property_name" => children(k).find(|c| c.is_named()).and_then(|e| match string_literal(self.src, e) {
                    Some(s) => Some(action_name(&s)),
                    None if matches!(e.kind(), "member_expression" | "identifier") => Some(format!("{{={}}}", self.text(e))),
                    None => None,
                }),
                _ => None,
            };
            if let Some(value) = value {
                self.facts.ng.push(NgFact { kind: "action", sym: Some(sym), value, extra: Some(source.clone()), line: line(pair) });
            }
        }
    }

    fn branch(&mut self, label: String, start: u32, end: u32) {
        if let Some(sym) = self.current() {
            self.facts.ng.push(NgFact { kind: "branch", sym: Some(sym), value: label, extra: Some(format!("{start}-{end}")), line: start });
        }
    }

    /// `switch (action.x) { case 'a': … }`: a `branch` fact per case (`*` for `default`), with its lines.
    fn switch_branches(&mut self, node: Node) {
        let Some(body) = node.child_by_field_name("body") else { return };
        for case in children(body).filter(|c| matches!(c.kind(), "switch_case" | "switch_default")) {
            let label = match case.child_by_field_name("value") {
                Some(v) => match literal_token(self.src, v) {
                    Some(t) => t,
                    None => continue,
                },
                None => "*".to_string(),
            };
            self.branch(label, line(case), case.end_position().row as u32 + 1);
        }
    }

    /// `if (action.x === 'a') { … }`: a `branch` fact for the block it guards.
    fn if_branch(&mut self, node: Node) {
        let (Some(cond), Some(then)) = (node.child_by_field_name("condition"), node.child_by_field_name("consequence")) else { return };
        if let Some(label) = compared_literal(self.src, cond) {
            self.branch(label, line(then), then.end_position().row as u32 + 1);
        }
    }

    /// `filter(a => a.x === 'a')` in an effect: the whole effect handles only that value.
    fn filter_branch(&mut self, call: Node) {
        if call.child_by_field_name("function").is_none_or(|f| self.text(f) != "filter") {
            return;
        }
        let Some(cb) = call.child_by_field_name("arguments").and_then(|a| children(a).find(|c| c.kind() == "arrow_function")) else { return };
        let Some(label) = cb.child_by_field_name("body").and_then(|b| compared_literal(self.src, b)) else { return };
        let Some(f) = self.field.as_ref().map(|f| f.prop) else { return };
        let (start, end) = (self.facts.symbols[f].start_line, self.facts.symbols[f].end_line);
        self.branch(label, start, end);
    }
}

/// The action creator's name NgRx's `createActionGroup` derives from an event name: words split on spaces, the first
/// with its first letter lowered, the rest with theirs raised (`'Load Success'` → `loadSuccess`).
pub fn action_name(event: &str) -> String {
    event
        .split_whitespace()
        .enumerate()
        .map(|(i, w)| {
            let mut c = w.chars();
            match c.next() {
                Some(f) if i == 0 => f.to_lowercase().chain(c).collect::<String>(),
                Some(f) => f.to_uppercase().chain(c).collect::<String>(),
                None => String::new(),
            }
        })
        .collect()
}

/// A literal a payload or a branch names: a string's text, an enum member's name (`Setting.AutoRenew` → `AutoRenew`),
/// a number or boolean as written.
fn literal_token(src: &str, n: Node) -> Option<String> {
    if let Some(s) = string_literal(src, n) {
        return Some(s);
    }
    match n.kind() {
        "member_expression" if n.child_by_field_name("object").is_some_and(|o| o.kind() == "identifier" && starts_upper(&src[o.byte_range()])) => {
            n.child_by_field_name("property").map(|p| src[p.byte_range()].to_string())
        }
        "number" | "true" | "false" => Some(src[n.byte_range()].to_string()),
        "parenthesized_expression" => children(n).find(|c| c.is_named()).and_then(|c| literal_token(src, c)),
        _ => None,
    }
}

/// `x === 'a'` (or `'a' == x`): the literal compared against.
fn compared_literal(src: &str, cond: Node) -> Option<String> {
    let cond = if cond.kind() == "parenthesized_expression" { children(cond).find(|c| c.is_named())? } else { cond };
    if cond.kind() != "binary_expression" || !cond.child_by_field_name("operator").is_some_and(|o| matches!(&src[o.byte_range()], "===" | "==")) {
        return None;
    }
    let (l, r) = (cond.child_by_field_name("left")?, cond.child_by_field_name("right")?);
    literal_token(src, r).or_else(|| literal_token(src, l))
}

/// The values a dispatched action's arguments are made of: literals and enum members, and `this.field` as written (its
/// value looked up at query time, in the class running the code), through object literals and constructor calls.
fn payload_tokens(src: &str, n: Node, out: &mut Vec<String>) {
    if let Some(t) = literal_token(src, n) {
        out.push(t);
        return;
    }
    if n.kind() == "member_expression" && n.child_by_field_name("object").is_some_and(|o| o.kind() == "this") {
        out.push(src[n.byte_range()].to_string());
        return;
    }
    if matches!(n.kind(), "arguments" | "object" | "pair" | "new_expression" | "array" | "parenthesized_expression") {
        for c in children(n).filter(|c| c.is_named()) {
            // An object's keys aren't values.
            if n.kind() == "pair" && Some(c) == n.child_by_field_name("key") {
                continue;
            }
            payload_tokens(src, c, out);
        }
    }
}

/// A request's options object (`{ url, method }`, jQuery's `{ url, type }`): its verb (GET without one; `None` when not
/// a literal) and URL.
fn request_config<'t>(src: &str, obj: Node<'t>) -> Option<(Option<String>, Node<'t>)> {
    let pairs = object_pairs(src, obj);
    // `{ url: … }`, or `{ url }`.
    let shorthand = || children(obj).find(|c| c.kind() == "shorthand_property_identifier" && &src[c.byte_range()] == "url");
    let url = pairs.iter().find(|(k, _)| k == "url").map(|(_, v)| *v).or_else(shorthand)?;
    let verb = match pairs.iter().find(|(k, _)| k == "method" || k == "type") {
        Some(_) => config_verb(src, obj),
        None => Some("GET".to_string()),
    };
    Some((verb, url))
}

/// The verb a request's options name (`{ method: 'POST' }`, jQuery's `{ type: 'POST' }`).
fn config_verb(src: &str, obj: Node) -> Option<String> {
    let (_, v) = object_pairs(src, obj).into_iter().find(|(k, _)| k == "method" || k == "type")?;
    urls::verb(&string_literal(src, v)?)
}

/// The tag a generated API client is named for: `getItemsApi` and `ItemsApi` are `Items`, `OrdersClient` is
/// `Orders`, `apiClient` and `api` name none (`""`). `None` for a name that isn't an API client's.
pub fn api_tag(name: &str) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    let lower = lower.strip_suffix("factory").unwrap_or(&lower);
    let suffix = ["apiclient", "api", "client"].iter().find(|s| lower.ends_with(*s))?;
    let mut stem = &name[..lower.len() - suffix.len()];
    for p in ["get", "create", "use", "new"] {
        if let Some(rest) = stem.strip_prefix(p).filter(|r| r.starts_with(|c: char| c.is_ascii_uppercase())) {
            stem = rest;
        }
    }
    let stem = stem.trim_matches(['_', '$']);
    if matches!(stem.to_ascii_lowercase().as_str(), "" | "http" | "rest" | "the" | "my" | "this" | "base" | "default" | "web" | "app" | "graphql") {
        return Some(String::new());
    }
    let mut c = stem.chars();
    Some(c.next().map(|f| f.to_ascii_uppercase().to_string() + c.as_str()).unwrap_or_default())
}

/// A module constant is named in capitals (`API`, `BASE_URL`); other names are locals.
fn is_constant_name(name: &str) -> bool {
    name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

const FUNCTION_KINDS: &[&str] =
    &["function_declaration", "function_expression", "function", "arrow_function", "method_definition", "generator_function_declaration", "generator_function"];

/// The innermost function around `n`.
fn enclosing_function(n: Node) -> Option<Node> {
    std::iter::successors(n.parent(), |p| p.parent()).find(|p| FUNCTION_KINDS.contains(&p.kind()))
}

/// How many times the function `f` binds `name`: as a parameter, and in declarations in its body outside the
/// functions in it (a function or class declared in it binds its own name there).
fn binds(src: &str, f: Node, name: &str) -> usize {
    // Every identifier in a parameter list or a declared pattern (a default value's too: counting more is safe).
    fn idents(src: &str, n: Node, name: &str) -> usize {
        let here = matches!(n.kind(), "identifier" | "shorthand_property_identifier_pattern") && &src[n.byte_range()] == name;
        usize::from(here) + children(n).map(|c| idents(src, c, name)).sum::<usize>()
    }
    fn declared(src: &str, n: Node, name: &str) -> usize {
        match n.kind() {
            "variable_declarator" => {
                n.child_by_field_name("name").map_or(0, |p| idents(src, p, name)) + n.child_by_field_name("value").map_or(0, |v| declared(src, v, name))
            }
            "catch_clause" => {
                n.child_by_field_name("parameter").map_or(0, |p| idents(src, p, name)) + n.child_by_field_name("body").map_or(0, |b| declared(src, b, name))
            }
            "function_declaration" | "generator_function_declaration" | "class_declaration" => {
                n.child_by_field_name("name").map_or(0, |p| usize::from(&src[p.byte_range()] == name))
            }
            k if FUNCTION_KINDS.contains(&k) => 0,
            _ => children(n).map(|c| declared(src, c, name)).sum(),
        }
    }
    let params = f.child_by_field_name("parameters").or_else(|| f.child_by_field_name("parameter")).map_or(0, |p| idents(src, p, name));
    params + f.child_by_field_name("body").map_or(0, |b| declared(src, b, name))
}

/// An object literal's `key: value` pairs, keys unquoted.
fn object_pairs<'t>(src: &str, obj: Node<'t>) -> Vec<(String, Node<'t>)> {
    children(obj)
        .filter(|c| c.kind() == "pair")
        .filter_map(|p| Some((src[p.child_by_field_name("key")?.byte_range()].trim_matches(['"', '\'']).to_string(), p.child_by_field_name("value")?)))
        .collect()
}

/// An Angular provider: an object with `provide` and one of `useClass`, `useExisting`, `useFactory`, `useValue`.
fn is_provider(src: &str, obj: Node) -> bool {
    let pairs = object_pairs(src, obj);
    pairs.iter().any(|(k, _)| k == "provide") && pairs.iter().any(|(k, _)| matches!(k.as_str(), "useClass" | "useExisting" | "useFactory" | "useValue"))
}

/// The class or token an expression names: `Foo`, `ns.Foo`, `forwardRef(() => Foo)`.
fn class_name(src: &str, v: Node) -> Option<String> {
    let name = match v.kind() {
        "identifier" => &src[v.byte_range()],
        "member_expression" => &src[v.child_by_field_name("property")?.byte_range()],
        "call_expression" if v.child_by_field_name("function").is_some_and(|f| &src[f.byte_range()] == "forwardRef") => {
            let f = v.child_by_field_name("arguments").and_then(|a| children(a).find(|c| c.kind() == "arrow_function"))?;
            return class_name(src, f.child_by_field_name("body")?);
        }
        _ => return None,
    };
    starts_upper(name).then(|| name.to_string())
}

/// `new Foo(…)` → `Foo`.
fn new_class(src: &str, v: Node) -> Option<String> {
    (v.kind() == "new_expression").then(|| v.child_by_field_name("constructor").and_then(|c| class_name(src, c))).flatten()
}

/// What a `useFactory` for `service` returns, when that is plain: the factory's declared return type, or else the one
/// class its body creates with `new` or asks the injector for (`inject(Foo)`), as a `factory` binding. A factory asking
/// for the service itself first (`inject(X, { skipSelf: true }) ?? inject(Local)`) is a `parent` binding: a parent
/// injector's provider runs, else what it falls back to. A factory named rather than written inline is looked up in
/// the same file.
fn factory_type(src: &str, v: Node, service: &str) -> (&'static str, Option<String>) {
    match factory_made(src, v, service) {
        Some((parent, made)) => (if parent { "parent" } else { "factory" }, made),
        None => ("factory", None),
    }
}

/// Whether a factory defers to a parent's binding, and the one class it otherwise makes.
fn factory_made(src: &str, v: Node, service: &str) -> Option<(bool, Option<String>)> {
    let f = match v.kind() {
        "arrow_function" | "function_expression" => v,
        "identifier" => {
            let name = &src[v.byte_range()];
            let mut root = v;
            while let Some(p) = root.parent() {
                root = p;
            }
            let named = |n: Node| n.child_by_field_name("name").is_some_and(|n| &src[n.byte_range()] == name);
            children(root)
                .flat_map(|c| if c.kind() == "export_statement" { c.child_by_field_name("declaration") } else { Some(c) })
                .flat_map(|d| match d.kind() {
                    "function_declaration" if named(d) => vec![d],
                    "lexical_declaration" | "variable_declaration" => children(d)
                        .filter(|c| c.kind() == "variable_declarator" && named(*c))
                        .filter_map(|c| c.child_by_field_name("value"))
                        .filter(|v| matches!(v.kind(), "arrow_function" | "function_expression"))
                        .collect(),
                    _ => Vec::new(),
                })
                .next()?
        }
        _ => return None,
    };
    if let Some(TypeExpr::Named(n)) = f.child_by_field_name("return_type").and_then(|t| type_expr(src, t))
        && starts_upper(&n)
        && n != service
    {
        return Some((false, Some(n)));
    }
    let mut made: Vec<String> = Vec::new();
    let mut parent = false;
    let mut stack = vec![f.child_by_field_name("body")?];
    while let Some(n) = stack.pop() {
        let found = match n.kind() {
            "new_expression" => new_class(src, n),
            "call_expression" if n.child_by_field_name("function").is_some_and(|c| &src[c.byte_range()] == "inject") => {
                n.child_by_field_name("arguments").and_then(|a| children(a).find(|c| c.is_named())).and_then(|a| class_name(src, a))
            }
            _ => None,
        };
        match found {
            // Deferring to a parent injector's binding first: which one runs depends on where it is injected.
            Some(c) if c == service => parent = true,
            Some(c) if !COMMON_TYPES.contains(&c.as_str()) => {
                if !made.contains(&c) {
                    made.push(c);
                }
            }
            _ => stack.extend(children(n)),
        }
    }
    Some((parent, (made.len() == 1).then(|| made.remove(0))))
}

/// The exported name `local` is imported as (`import { flexSelectors as selector }` gives `flexSelectors`), or
/// `local` itself.
fn imported_name(src: &str, any: Node, local: &str) -> String {
    let mut root = any;
    while let Some(p) = root.parent() {
        root = p;
    }
    for spec in children(root)
        .filter(|c| c.kind() == "import_statement")
        .flat_map(children)
        .filter(|c| c.kind() == "import_clause")
        .flat_map(children)
        .filter(|c| c.kind() == "named_imports")
        .flat_map(children)
        .filter(|c| c.kind() == "import_specifier")
    {
        if let (Some(n), Some(a)) = (spec.child_by_field_name("name"), spec.child_by_field_name("alias"))
            && &src[a.byte_range()] == local
        {
            return src[n.byte_range()].to_string();
        }
    }
    local.to_string()
}

/// `m.XComponent` in a lazy import (`() => import('./x').then(m => m.XComponent)`): the class it loads.
fn lazy_class<'t>(src: &str, v: Node<'t>) -> Option<Node<'t>> {
    let mut stack = vec![v];
    while let Some(n) = stack.pop() {
        if n.kind() == "member_expression"
            && let Some(p) = n.child_by_field_name("property")
            && starts_upper(&src[p.byte_range()])
        {
            return Some(p);
        }
        stack.extend(children(n));
    }
    None
}

/// The routes constant a lazy `loadChildren` loads (`import('./login/login.routes').then(m => m.routes)`), and the
/// module it is imported from.
fn lazy_routes<'t>(src: &str, v: Node<'t>) -> Option<(Node<'t>, String)> {
    let (mut prop, mut from) = (None, None);
    let mut stack = vec![v];
    while let Some(n) = stack.pop() {
        if n.kind() == "member_expression" && prop.is_none() {
            prop = n.child_by_field_name("property");
        }
        if n.kind() == "call_expression"
            && n.child_by_field_name("function").is_some_and(|f| f.kind() == "import")
            && let Some(arg) = n.child_by_field_name("arguments").and_then(|a| children(a).find(|c| c.kind() == "string"))
        {
            from = string_literal(src, arg);
        }
        stack.extend(children(n));
    }
    Some((prop?, from?))
}

/// An Angular route: an object with a string `path` and something to show.
fn is_route(src: &str, obj: Node) -> bool {
    let keys: Vec<&str> = children(obj)
        .filter(|c| c.kind() == "pair")
        .filter_map(|p| p.child_by_field_name("key"))
        .map(|k| src[k.byte_range()].trim_matches(['"', '\'']))
        .collect();
    keys.contains(&"path") && keys.iter().any(|k| matches!(*k, "component" | "loadComponent" | "loadChildren" | "children"))
}

/// The value of a string literal, or of a template string without substitutions.
fn string_literal(src: &str, n: Node) -> Option<String> {
    match n.kind() {
        "string" => Some(children(n).filter(|c| c.kind() == "string_fragment").map(|c| &src[c.byte_range()]).collect()),
        "template_string" if !children(n).any(|c| c.kind() == "template_substitution") => {
            Some(src[n.byte_range()].trim_matches('`').to_string())
        }
        _ => None,
    }
}

/// Whether `n` assigns the variable `name` anywhere in it (`name = …`, `name += …`, `name++`).
fn assigns(src: &str, n: Node, name: &str) -> bool {
    let target = |t: Option<Node>| t.is_some_and(|t| &src[t.byte_range()] == name);
    let here = match n.kind() {
        "assignment_expression" | "augmented_assignment_expression" => target(n.child_by_field_name("left")),
        "update_expression" => target(n.child_by_field_name("argument")),
        _ => false,
    };
    here || children(n).any(|c| assigns(src, c, name))
}

/// `Foo`, `Foo<T>`, `ns.Foo`, `ns.Foo<T>` → `Foo`.
fn base_type_name(src: &str, t: Node) -> String {
    match t.kind() {
        "identifier" | "type_identifier" => src[t.byte_range()].to_string(),
        "generic_type" | "nested_type_identifier" => t.child_by_field_name("name").map_or_else(String::new, |n| base_type_name(src, n)),
        "member_expression" => t.child_by_field_name("property").map_or_else(String::new, |n| src[n.byte_range()].to_string()),
        _ => String::new(),
    }
}

/// The names an object pattern binds, each with the property it takes (`{ task, item: it }`: `task` from `task`, `it`
/// from `item`). A nested pattern, a rest element or a computed key binds nothing here.
fn destructured(src: &str, pattern: Node) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for c in children(pattern) {
        match c.kind() {
            "shorthand_property_identifier_pattern" => {
                let name = src[c.byte_range()].to_string();
                out.push((name.clone(), name));
            }
            // `{ task = fallback }`: the left side names the property.
            "object_assignment_pattern" => {
                if let Some(l) = c.child_by_field_name("left").filter(|l| l.kind() == "shorthand_property_identifier_pattern") {
                    let name = src[l.byte_range()].to_string();
                    out.push((name.clone(), name));
                }
            }
            "pair_pattern" => {
                if let (Some(k), Some(v)) = (c.child_by_field_name("key"), c.child_by_field_name("value"))
                    && k.kind() == "property_identifier"
                    && v.kind() == "identifier"
                {
                    out.push((src[k.byte_range()].to_string(), src[v.byte_range()].to_string()));
                }
            }
            _ => {}
        }
    }
    out
}

fn type_expr(src: &str, t: Node) -> Option<TypeExpr> {
    match t.kind() {
        "type_annotation" | "parenthesized_type" | "readonly_type" => children(t).find(|c| c.is_named()).and_then(|c| type_expr(src, c)),
        "type_identifier" | "nested_type_identifier" => Some(TypeExpr::Named(base_type_name(src, t))),
        "predefined_type" | "this_type" => Some(TypeExpr::Named(src[t.byte_range()].to_string())),
        "array_type" => children(t).find(|c| c.is_named()).and_then(|e| type_expr(src, e)).map(|e| TypeExpr::Array(Box::new(e))),
        // `T | null | undefined` → T.
        "union_type" => children(t)
            .filter(|c| c.is_named() && !matches!(&src[c.byte_range()], "null" | "undefined"))
            .find_map(|c| type_expr(src, c)),
        "generic_type" => {
            let base = base_type_name(src, t);
            let args: Vec<TypeExpr> = t
                .child_by_field_name("type_arguments")
                .map(|a| children(a).filter(|c| c.is_named()).filter_map(|c| type_expr(src, c)).collect())
                .unwrap_or_default();
            generic(&base, &args)
        }
        _ => None,
    }
}

/// A generic type with its arguments (`Observable<Order>`, `Map<K, V>`), as far as members on it can be followed.
fn generic(base: &str, args: &[TypeExpr]) -> Option<TypeExpr> {
    match (base, args) {
        // An Observable is a sequence of its values: `subscribe` and operator callbacks see elements.
        (
            "Array" | "ReadonlyArray" | "Set" | "Observable" | "Subject" | "BehaviorSubject" | "ReplaySubject"
            | "EventEmitter",
            [e],
        ) => Some(TypeExpr::Array(Box::new(e.clone()))),
        ("Map" | "Record", [k, v]) => Some(TypeExpr::Dict(Box::new(k.clone()), Box::new(v.clone()))),
        ("Promise" | "Readonly" | "Partial" | "Required", [inner]) => Some(inner.clone()),
        (w, [inner]) if MOCK_WRAPPERS.contains(&w) => Some(inner.clone()),
        ("Pick" | "Omit", [inner, ..]) => Some(inner.clone()),
        // Computed types (`ReturnType<typeof f>`): no name says what they are.
        ("ReturnType" | "InstanceType" | "Parameters" | "Awaited" | "NonNullable" | "Extract" | "Exclude", _) => None,
        _ => Some(TypeExpr::Named(base.to_string())),
    }
}

/// Decorator names (`@Component({…})` → `Component`), on the declaration or the `export` around it.
fn decorators(src: &str, node: Node, outer: Node) -> Vec<String> {
    let mut nodes: Vec<Node> = children(outer).filter(|c| c.kind() == "decorator").collect();
    if outer != node {
        nodes.extend(children(node).filter(|c| c.kind() == "decorator"));
    }
    nodes
        .into_iter()
        .filter_map(|d| children(d).find(|c| c.is_named()))
        .map(|e| {
            let e = if e.kind() == "call_expression" { e.child_by_field_name("function").unwrap_or(e) } else { e };
            let t = &src[e.byte_range()];
            t.rsplit('.').next().unwrap_or(t).to_string()
        })
        .collect()
}

/// The names of the decorators just before a class member in the class body (`@HostListener('click')` above a
/// method).
fn member_decorators(src: &str, m: Node) -> Vec<String> {
    let mut out = Vec::new();
    let mut prev = m.prev_named_sibling();
    while let Some(d) = prev.filter(|d| d.kind() == "decorator") {
        if let Some(e) = children(d).find(|c| c.is_named()) {
            let e = if e.kind() == "call_expression" { e.child_by_field_name("function").unwrap_or(e) } else { e };
            let t = &src[e.byte_range()];
            out.push(t.rsplit('.').next().unwrap_or(t).to_string());
        }
        prev = d.prev_named_sibling();
    }
    out.reverse();
    out
}

/// First text line of the `/** … */` comment above a declaration.
fn doc_comment(src: &str, node: Node) -> Option<String> {
    let p = node.prev_sibling().filter(|p| p.kind() == "comment")?;
    let t = &src[p.byte_range()];
    if !t.starts_with("/**") {
        return None;
    }
    t.trim_start_matches("/**")
        .trim_end_matches("*/")
        .lines()
        .map(|l| l.trim().trim_start_matches('*').trim())
        .find(|l| !l.is_empty() && !l.starts_with('@'))
        .map(|l| collapse(l, 200))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RefKind, SymbolKind};
    use crate::types::TypeExpr;

    /// Extraction under the conventions of a made-up code base (see [`crate::conventions::synthetic`]).
    fn extract(source: &str, tsx: bool) -> FileFacts {
        crate::conventions::scoped(crate::conventions::synthetic(), || super::extract(source, tsx))
    }

    const SAMPLE: &str = r#"
import { Component, inject } from '@angular/core';
import { CartRepository } from '../data/cart.repository';
import type { Cart } from './cart';

/** Shows the cart. */
@Component({ selector: 'app-cart', templateUrl: './cart.component.html' })
export class CartComponent extends BaseComponent implements OnInit, OnDestroy {
  @Input() cartId!: string;
  private readonly logger = inject(LoggerService);
  total: number = 0;
  static readonly LIMIT = 5;

  constructor(private repo: CartRepository, public router: Router, label: string) {
    super();
  }

  async ngOnInit(): Promise<void> {
    const cart = await this.repo.load(this.cartId);
    if (!cart || cart.items.length === 0) {
      return;
    }
    this.logger.info('loaded');
    const pricer = new Pricer(cart);
    pricer.price();
    format(cart);
  }

  get count(): number { return this.total; }

  private reset = (): void => { this.total = 0; };
}

export interface CartLine { sku: string; qty?: number; price(): number; }

export enum CartState { Open, Closed = 'closed' }

export type CartId = string;

export function format(cart: Cart): string {
  return cart.name ?? '';
}

export const toLines = (cart: Cart): CartLine[] => cart.items.map((i) => i.line);

export abstract class BaseComponent {}
"#;

    #[test]
    fn a_namespace_is_a_symbol_spanning_its_declarations() {
        let f = extract("export namespace Shapes { export interface Circle { r: number } }\ndeclare global { namespace Express { interface Request {} } }\nnamespace A.B { namespace C { type D = 1 } }\ndeclare module \"pkg\" { interface X {} }\n", false);
        let ns: Vec<(&str, Option<&str>)> =
            f.symbols.iter().filter(|s| s.kind == SymbolKind::Module).map(|s| (s.name.as_str(), s.container.as_deref())).collect();
        assert_eq!(ns, [("Shapes", None), ("Express", None), ("A.B", None), ("C", Some("A.B"))]);
        // Members keep their own containers.
        assert!(f.symbols.iter().any(|s| s.name == "Circle" && s.container.is_none()));
    }

    #[test]
    fn a_qualified_type_keeps_its_qualifier() {
        let f = extract("export type FocusEvent = globalThis.FocusEvent\nlet a: ns.Inner.Leaf\nlet b: Plain\n", false);
        let types: Vec<(&str, Option<&str>)> =
            f.refs.iter().filter(|r| r.kind == RefKind::TypeRef).map(|r| (r.name.as_str(), r.receiver.as_deref())).collect();
        assert_eq!(types, [("FocusEvent", Some("globalThis")), ("Leaf", Some("ns.Inner")), ("Plain", None)]);
    }

    #[test]
    fn extracts_typescript_classes_members_and_refs() {
        let f = extract(SAMPLE, false);
        let sym = |name: &str, kind: SymbolKind| {
            f.symbols.iter().find(|s| s.name == name && s.kind == kind).unwrap_or_else(|| panic!("{name} {kind:?}"))
        };

        let class = sym("CartComponent", SymbolKind::Class);
        assert_eq!(class.doc.as_deref(), Some("Shows the cart."));
        assert_eq!(class.attrs, ["Component"]);
        assert!(class.signature.starts_with("@Component("), "{}", class.signature);
        let targets: Vec<&str> = f.relations.iter().map(|r| r.target.as_str()).collect();
        assert_eq!(targets, ["BaseComponent", "OnInit", "OnDestroy"]);

        assert_eq!(sym("cartId", SymbolKind::Property).attrs, ["Input"]);
        // `inject(T)` and constructor parameter properties type the field.
        assert_eq!(sym("logger", SymbolKind::Property).type_name.as_deref(), Some("LoggerService"));
        assert_eq!(sym("repo", SymbolKind::Property).type_name.as_deref(), Some("CartRepository"));
        assert_eq!(sym("router", SymbolKind::Property).container.as_deref(), Some("CartComponent"));
        assert!(!f.symbols.iter().any(|s| s.name == "label"));
        sym("LIMIT", SymbolKind::Property);
        assert_eq!(sym("init", SymbolKind::Init).container.as_deref(), Some("CartComponent"));
        let on_init = sym("ngOnInit", SymbolKind::Method);
        assert_eq!(on_init.container.as_deref(), Some("CartComponent"));
        // One `if` and one `||`.
        assert_eq!(on_init.complexity, 3);
        assert_eq!(sym("count", SymbolKind::Property).type_name.as_deref(), Some("number"));
        assert_eq!(sym("reset", SymbolKind::Method).container.as_deref(), Some("CartComponent"));

        let line = sym("CartLine", SymbolKind::Protocol);
        assert_eq!(line.container, None);
        assert_eq!(sym("qty", SymbolKind::Property).container.as_deref(), Some("CartLine"));
        assert_eq!(sym("price", SymbolKind::Method).container.as_deref(), Some("CartLine"));
        assert_eq!(sym("Closed", SymbolKind::Case).container.as_deref(), Some("CartState"));
        sym("CartId", SymbolKind::Typealias);
        assert_eq!(sym("format", SymbolKind::Function).type_name.as_deref(), Some("string"));
        assert_eq!(sym("toLines", SymbolKind::Function).container, None);
        sym("BaseComponent", SymbolKind::Class);

        assert_eq!(
            f.imports.iter().map(|i| i.module.as_str()).collect::<Vec<_>>(),
            ["@angular/core", "../data/cart.repository", "./cart"]
        );

        let call = |name: &str| f.refs.iter().find(|r| r.name == name && r.kind == RefKind::Call).unwrap_or_else(|| panic!("{name}"));
        assert_eq!(call("load").receiver.as_deref(), Some("this.repo"));
        assert!(call("load").receiver_type.as_deref().unwrap().contains("repo"));
        assert!(call("info").receiver_type.as_deref().unwrap().contains("logger"));
        assert_eq!(call("price").receiver_type.as_deref(), Some("Pricer"));
        assert_eq!(call("format").receiver, None);
        let inst: Vec<&str> = f.refs.iter().filter(|r| r.kind == RefKind::Instantiate).map(|r| r.name.as_str()).collect();
        assert_eq!(inst, ["Pricer"]);
        let type_refs: Vec<&str> = f.refs.iter().filter(|r| r.kind == RefKind::TypeRef).map(|r| r.name.as_str()).collect();
        assert!(type_refs.contains(&"CartRepository") && type_refs.contains(&"Cart") && type_refs.contains(&"CartLine"), "{type_refs:?}");
        assert!(!type_refs.contains(&"Promise"), "{type_refs:?}");
        assert!(f.refs.iter().any(|r| r.name == "items" && r.kind == RefKind::Member));
        assert_eq!(f.parse_errors, 0);
    }

    /// A function returning its first parameter with only a query string appended is recorded (`addQuery()`), so a
    /// URL it builds is the path given; one that may change the path, or returns something else, is not. A request
    /// building its URL so, or choosing between two URLs with one path, keeps the path with the function marked.
    #[test]
    fn query_appending_functions_and_conditional_urls() {
        let f = extract(
            r#"
export function addQuery(base: string, start: string, end: string): string {
  if (start != null) {
    base += "?start=" + start;
  }
  if (end != null) {
    base += base.indexOf("?") > -1 ? "&" : "?";
    base += "end=" + end;
  }
  return base;
}
export function addSegment(base: string, id: string): string {
  if (id) {
    base += "/" + id;
  }
  return base;
}
export function reset(base: string): string {
  base = "/home";
  return base;
}
export function other(base: string): string {
  return base.trim();
}
export class ApiService {
  getEvents(start: string) { return this.send("GET", addQuery("/events", start, null)); }
  getSync(all: boolean) {
    const path = all ? "/sync?all=true" : "/sync";
    return this.send("GET", path);
  }
  getEither(a: boolean) { return this.send("GET", a ? "/one" : "/two"); }
}
"#,
            false,
        );
        let consts: Vec<(&str, &str)> = f.constants.iter().map(|c| (c.qualified.as_str(), c.value.as_str())).collect();
        assert_eq!(consts, [("addQuery()", "")]);
        let routes: Vec<(&str, &str)> = f.http_routes.iter().map(|r| (r.side, r.path.as_str())).collect();
        assert_eq!(routes, [("client", "/events{=addQuery()}"), ("client", "/sync{~path@28}")]);
    }

    #[test]
    fn callback_parameters_take_the_element_type() {
        let f = extract(
            r#"
export class Orders {
  load(items: Item[], orders$: Observable<Order>) {
    items.filter((i) => i.isOpen()).forEach(i => i.total());
    orders$.subscribe(o => o.close());
    orders$.pipe(filter(o => o.isValid()), map(o => o.toDto()), tap(d => d.send())).subscribe();
    helper(h => h.unknown());
  }
}
"#,
            false,
        );
        let rt = |name: &str| {
            f.refs.iter().find(|r| r.name == name && r.kind == RefKind::Call).unwrap_or_else(|| panic!("{name}")).receiver_type.clone()
        };
        let elem = |t: &str| Some(TypeExpr::Elem(Box::new(TypeExpr::Array(Box::new(TypeExpr::Named(t.into()))))).encode());
        assert_eq!(rt("isOpen"), elem("Item"));
        assert!(rt("total").unwrap().contains("elem"), "{:?}", rt("total"));
        // Observable<T> is a sequence of T.
        assert_eq!(rt("close"), elem("Order"));
        // Operators see the source's elements until one changes the type.
        assert_eq!(rt("isValid"), elem("Order"));
        assert_eq!(rt("toDto"), elem("Order"));
        assert_eq!(rt("send"), None);
        assert_eq!(rt("unknown"), None);
    }

    #[test]
    fn heritage_types_are_type_references_from_the_subclass() {
        let f = extract(
            "export class MxRecords extends DnsRecordBaseDirective<MxRecord> implements OnInit {}\nexport interface Row extends Base<Cell> {}\n",
            false,
        );
        let refs: Vec<(&str, Option<&str>)> = f
            .refs
            .iter()
            .filter(|r| r.kind == RefKind::TypeRef)
            .map(|r| (r.name.as_str(), r.from.map(|i| f.symbols[i].name.as_str())))
            .collect();
        for (name, from) in [("DnsRecordBaseDirective", "MxRecords"), ("MxRecord", "MxRecords"), ("OnInit", "MxRecords"), ("Base", "Row"), ("Cell", "Row")] {
            assert!(refs.contains(&(name, Some(from))), "{name}: {refs:?}");
        }
    }

    #[test]
    fn api_contracts_are_client_routes_and_constants_resolve_them() {
        let f = extract(
            r#"
export class Api {
  static readonly filters = 'Filters';
  static readonly getDomain = 'GetDomainV2';
}
export const BASE = '/v2/Thing';
export const SellerApi = {
  PayoutsSearch: 'Payouts/Search',
  'Quoted': "x",
  nested: { no: 'skip' },
};

@RequestContract({ endpoint: Api.filters, httpMethod: HttpMethods.post })
export class ApiGetFilters {
  constructor(readonly searchTerm: string) {}
}

@RequestContract({
  httpMethod: HttpMethods.get,
  endpoint: '/Domains/{id}',
})
export class ApiGetDomain {}

export class NotAContract {}
"#,
            false,
        );
        let route = |class: &str| f.http_routes.iter().find(|r| f.symbols[r.sym.unwrap()].name == class).unwrap_or_else(|| panic!("{class}"));
        let r = route("ApiGetFilters");
        assert_eq!((r.side, r.verb.as_str(), r.path.as_str(), r.path_ref.as_deref()), ("client", "POST", "", Some("Api.filters")));
        let r = route("ApiGetDomain");
        assert_eq!((r.verb.as_str(), r.path.as_str(), r.path_ref.as_deref()), ("GET", "Domains/{id}", None));
        assert_eq!(f.http_routes.len(), 2);
        let consts: Vec<(&str, &str)> = f.constants.iter().map(|c| (c.qualified.as_str(), c.value.as_str())).collect();
        assert_eq!(
            consts,
            [
                ("Api.filters", "Filters"),
                ("Api.getDomain", "GetDomainV2"),
                ("BASE", "/v2/Thing"),
                ("SellerApi.PayoutsSearch", "Payouts/Search"),
                ("SellerApi.Quoted", "x"),
            ]
        );
    }

    #[test]
    fn angular_components_routes_and_class_values() {
        let f = extract(
            r#"
@Component({
  selector: 'nb-mx-records',
  templateUrl: './mx-records.component.html',
})
export class MxRecordsComponent {}

@Component({ selector: 'nb-badge, [nbBadge]', template: `<span class="b"><nb-icon name="x"></nb-icon></span>` })
export class BadgeComponent {}

export const routes: Routes = [
  { path: '', redirectTo: 'dns', pathMatch: 'full' },
  {
    path: 'dns',
    component: DnsShellComponent,
    children: [
      { path: 'mx', component: MxRecordsComponent },
      { path: ':id/txt', loadComponent: () => import('./txt.component').then((m) => m.TxtRecordsComponent) },
    ],
  },
  { path: 'account', loadChildren: () => import('./account/account.module').then(m => m.AccountModule) },
  { path: 'login', loadChildren: () => import('./login/login.routes').then(m => m.routes) },
];

export class RootService {
  constructor(private dialogService: DialogService) {}
  edit(): void {
    this.dialogService.open({ content: MxRecordsComponent, width: 400 });
    this.dialogs.open(BadgeComponent);
  }
}

@NgModule({ declarations: [MxRecordsComponent], imports: [CommonModule] })
export class DnsModule {}
"#,
            false,
        );
        let ng: Vec<(&str, Option<&str>, &str, Option<&str>)> =
            f.ng.iter().map(|n| (n.kind, n.sym.map(|i| f.symbols[i].name.as_str()), n.value.as_str(), n.extra.as_deref())).collect();
        assert_eq!(
            ng,
            [
                ("selector", Some("MxRecordsComponent"), "nb-mx-records", None),
                ("template", Some("MxRecordsComponent"), "./mx-records.component.html", None),
                ("selector", Some("BadgeComponent"), "nb-badge, [nbBadge]", None),
                ("renders", Some("BadgeComponent"), "nb-icon", Some("nb-icon name")),
                ("route", None, "dns", Some("DnsShellComponent")),
                ("route", None, "dns/mx", Some("MxRecordsComponent")),
                ("route", None, "dns/:id/txt", Some("TxtRecordsComponent")),
                ("route", None, "account", Some("loads:AccountModule")),
                ("route", None, "login", Some("loads:routes@./login/login.routes")),
                ("opens", Some("edit"), "MxRecordsComponent", Some("open")),
                ("opens", Some("edit"), "BadgeComponent", Some("open")),
            ]
        );
        // Classes used as values (routes, dialogs, module metadata) are type references from where they appear.
        let uses: Vec<(&str, Option<&str>)> = f
            .refs
            .iter()
            .filter(|r| r.kind == RefKind::TypeRef && (r.name.ends_with("Component") || r.name.ends_with("Module")))
            .map(|r| (r.name.as_str(), r.from.map(|i| f.symbols[i].name.as_str())))
            .collect();
        for (name, from) in [
            ("MxRecordsComponent", "edit"),
            ("BadgeComponent", "edit"),
            ("MxRecordsComponent", "DnsModule"),
            ("DnsShellComponent", "routes"),
            ("TxtRecordsComponent", "routes"),
            ("AccountModule", "routes"),
        ] {
            assert!(uses.contains(&(name, Some(from))), "{name} from {from}: {uses:?}");
        }
    }

    #[test]
    fn http_client_calls_are_client_routes_of_their_method() {
        let f = extract(
            r#"
import { flexSelectors as selector } from './selectors';
const API = environment.apiUrl;
export const BASE = `${environment.apiUrl}/v2`;

export class OrdersApi {
  list() { return this.http.get<Order[]>(`${API}/orders`, { params }); }
  one(id: number) { return this.http.delete<void>(`${API}/orders/${encodeURIComponent(id)}`); }
  download() {
    const endpoint = Api.download;
    return this.env.config$.pipe(switchMap(config => this._http.get(`${config.apiUrl}/${endpoint}`)));
  }
  raw() { return this.httpClient.request('PUT', 'https://h.example/api/x?y=1'); }
  // Not a URL: a contract sent through a wrapper, and a local.
  send(req: ApiGetOrders) { return this.http.get<ApiGetOrders, Order[]>(req); }
  computed() { return this.http.post(`${base(account)}/health`, {}); }
}

@Directive({ selector })
export class FlexDirective {}
"#,
            false,
        );
        let routes: Vec<(&str, &str, &str)> =
            f.http_routes.iter().map(|r| (f.symbols[r.sym.unwrap()].name.as_str(), r.verb.as_str(), r.path.as_str())).collect();
        assert_eq!(
            routes,
            [
                ("list", "GET", "{=API}/orders"),
                ("one", "DELETE", "{=API}/orders/{}"),
                ("download", "GET", "{=config.apiUrl}/{=Api.download}"),
                ("raw", "PUT", "https://h.example/api/x?y=1"),
                // A URL (here a contract) known only at run time: kept without a path.
                ("send", "GET", ""),
                ("computed", "POST", "{}/health"),
            ]
        );
        // URL prefixes are constants, encoded the same way.
        let consts: Vec<(&str, &str)> = f.constants.iter().map(|c| (c.qualified.as_str(), c.value.as_str())).collect();
        assert_eq!(consts, [("API", "{=environment.apiUrl}"), ("BASE", "{=environment.apiUrl}/v2")]);
        // A selector held in an imported constant names the constant's exported name.
        assert!(f.ng.iter().any(|n| n.kind == "selector" && n.value == "{=flexSelectors}"));
    }

    /// An API wrapper's own requests (`this.send(…)` in `ApiService`) are client routes of its methods, as calls
    /// through it from other services are; a class not named for an API isn't taken for one.
    #[test]
    fn an_api_wrappers_own_requests_are_client_routes() {
        let f = extract(
            r#"
export class ApiService {
  deleteAttachment(id: string, attachmentId: string) {
    return this.send("DELETE", "/ciphers/" + id + "/attachment/" + attachmentId, null, true, false);
  }
  getProfile() { return this.send("GET", "/accounts/profile", null, true, true); }
  forward(path: string) { return this.send("GET", path, null, true, true); }
  async send(method: string, path: string, body: any, auth: boolean, hasResponse: boolean) { return fetch(path); }
}
export class MessageBus {
  publish() { return this.send("POST", "/events/" + this.topic); }
}
"#,
            false,
        );
        let routes: Vec<(&str, &str, &str)> =
            f.http_routes.iter().map(|r| (f.symbols[r.sym.unwrap()].name.as_str(), r.verb.as_str(), r.path.as_str())).collect();
        assert_eq!(
            routes,
            [
                ("deleteAttachment", "DELETE", "/ciphers/{}/attachment/{}"),
                ("getProfile", "GET", "/accounts/profile"),
                // The wrapper's own `fetch` of a path known only at run time.
                ("send", "GET", ""),
            ]
        );
    }

    /// A request or operation in code no named symbol encloses (an anonymous default export, a callback in it, a
    /// function assigned to a property) is kept, the file's (`sym` none), as top-level statements' are: the global
    /// client's `ApiClient.getLiveTvSeriesTimers(query)` there was dropped.
    #[test]
    fn requests_outside_any_symbol_are_the_files() {
        let f = extract(
            r#"
const query = { SortBy: 'SortName' };
export default function (view, params, tabContent) {
    let timersPromise;
    const self = this;
    const userId = params.userId || ApiClient.getCurrentUserId();
    self.preRender = function () {
        timersPromise = ApiClient.getLiveTvSeriesTimers(query);
    };
    view.addEventListener('viewshow', function () {
        ApiClient.getItem(ApiClient.getCurrentUserId(), params.id).then(function (item) { view.title = item.Name; });
        fetch('/Library/Refresh', { method: 'POST' });
    });
}
"#,
            false,
        );
        let routes: Vec<(Option<&str>, &str, &str, &str, u32)> = f
            .http_routes
            .iter()
            .map(|r| (r.sym.map(|s| f.symbols[s].name.as_str()), r.side, r.verb.as_str(), r.path.as_str(), r.line))
            .collect();
        assert_eq!(
            routes,
            [
                (Some("userId"), "operation", "", "getCurrentUserId", 6),
                (None, "operation", "", "getLiveTvSeriesTimers", 8),
                (None, "operation", "", "getItem", 11),
                (None, "operation", "", "getCurrentUserId", 11),
                (None, "client", "POST", "/Library/Refresh", 12),
            ]
        );
    }

    /// fetch, axios (and instances), jQuery, superagent-style wrappers and URL builders, with composed URLs; a request
    /// whose URL is known only at run time is kept without a path; calls to generated API clients are operations.
    #[test]
    fn fetch_axios_jquery_wrappers_and_generated_clients_are_requests() {
        let f = extract(
            r#"
const API_ROOT = 'https://api.example.test/api';
export class Library {
  latest(userId: string) { return fetch(`${API_ROOT}/Users/${userId}/Items/Latest`); }
  remove(id: string) { return fetch(API_ROOT + '/Items/' + id, { method: 'DELETE' }); }
  post() { return axios.post(`/api/orders`, {}); }
  config() { return axios({ url: '/api/orders/export', method: 'post' }); }
  instance() { return this.api.get('/orders/' + this.id); }
  jquery() { return $.ajax({ url: apiClient.getUrl('Sessions/Playing'), type: 'POST' }); }
  wrapper() { return this.apiService.send('GET', '/accounts/profile', null, true); }
  helper() { return this.apiClient.getJSON(this.apiClient.getUrl(`Shows/${id}/Episodes`)); }
  local() {
    const url = new URL('Videos/Merge', base);
    return fetch(url, { method: 'POST' });
  }
  unknown(url: string) { return fetch(url); }
  generated(api: Api) { return getItemsApi(api).getItems({ userId }); }
  constructed() { return new OrdersClient(base).listOrders(); }
  injected() { return this.userLibraryApi.getLatestMedia(); }
  untagged() { return this.apiClient.getPublicUsers(); }
  byConst() { return this.api.get(Api.orders); }
  listDelete() { return this.listsClient.delete(this.selectedList.id); }
  // Not requests: a map lookup, a promise, encoding.
  other() { return this.cache.get('a/b').then(x => encodeURIComponent(x)); }
}
"#,
            false,
        );
        let routes: Vec<(&str, &str, &str, &str, Option<&str>)> = f
            .http_routes
            .iter()
            .map(|r| (f.symbols[r.sym.unwrap()].name.as_str(), r.side, r.verb.as_str(), r.path.as_str(), r.service.as_deref()))
            .collect();
        assert_eq!(
            routes,
            [
                ("latest", "client", "GET", "{=API_ROOT}/Users/{}/Items/Latest", None),
                ("remove", "client", "DELETE", "{=API_ROOT}/Items/{}", None),
                ("post", "client", "POST", "/api/orders", None),
                ("config", "client", "POST", "/api/orders/export", None),
                ("instance", "client", "GET", "/orders/{=id}", None),
                ("jquery", "client", "POST", "/Sessions/Playing", None),
                ("wrapper", "client", "GET", "/accounts/profile", None),
                ("helper", "client", "GET", "/Shows/{}/Episodes", None),
                ("local", "client", "POST", "/Videos/Merge{~url@13}", None),
                ("unknown", "client", "GET", "", None),
                ("generated", "operation", "", "getItems", Some("Items")),
                ("constructed", "operation", "", "listOrders", Some("Orders")),
                ("injected", "operation", "", "getLatestMedia", Some("UserLibrary")),
                ("untagged", "operation", "", "getPublicUsers", None),
                ("byConst", "client", "GET", "{=Api.orders}", None),
                ("listDelete", "operation", "", "delete", Some("Lists")),
            ]
        );
    }

    /// A request whose URL is just a name (`fetch(url)`, `{ url }`, `getUrl(url)`) reads it from a `const` the same
    /// function declares before it, with the local after the path. A URL builder or a `{ url }` option given any
    /// other name isn't read; a name passed directly is read as before.
    #[test]
    fn urls_in_const_locals_of_the_requesting_function() {
        let f = extract(
            r#"const url = 'Module';
function createAlbum(apiClient, dlg) {
    const url = apiClient.getUrl('Albums', {
        Name: dlg.name,
        Ids: dlg.ids || ''
    });
    apiClient.ajax({ type: 'POST', url, dataType: 'json' }).then(done);
}
function removeCaption(context, index) {
    confirm(context).then(function () {
        const trackId = currentTrack.Id;
        const url = 'Tracks/' + trackId + '/Captions/' + index;
        const apiClient = connections.getApiClient(currentTrack.ServerId);
        apiClient.ajax({ type: 'DELETE', url: apiClient.getUrl(url) });
    });
}
function plain(id) {
    const url = `/api/orders/${id}`;
    return fetch(url, { method: 'DELETE' });
}
function reassigned(apiClient, id) {
    let url = 'Tracks/' + id;
    if (id) { url = 'Archive/' + id; }
    return apiClient.ajax({ type: 'DELETE', url: apiClient.getUrl(url) });
}
function viaVar(apiClient, id) { var url = apiClient.getUrl('Tracks/' + id); return apiClient.ajax({ type: 'DELETE', url }); }
function captured(apiClient, id) {
    const url = apiClient.getUrl('Tracks/' + id);
    return load().then(() => apiClient.ajax({ type: 'DELETE', url }));
}
function parameter(apiClient, url) { return apiClient.ajax({ type: 'DELETE', url: apiClient.getUrl(url) }); }
function shadowed(apiClient, id) {
    const url = 'Tracks/' + id;
    if (id) { const url = 'Other'; }
    return apiClient.ajax({ type: 'DELETE', url: apiClient.getUrl(url) });
}
function inBlock(apiClient, id) {
    if (id) { const url = 'Tracks/' + id; }
    return apiClient.ajax({ type: 'DELETE', url: apiClient.getUrl(url) });
}
function fromLocal(apiClient, id) {
    const base = 'Tracks/';
    const url = base + id;
    return apiClient.ajax({ type: 'DELETE', url: apiClient.getUrl(url) });
}
function joined(apiClient, id) {
    const url = ['Tracks', id].join('/');
    return apiClient.ajax({ type: 'DELETE', url: apiClient.getUrl(url) });
}
function either(apiClient, a) {
    const url = a ? 'Tracks' : 'Albums';
    return apiClient.ajax({ type: 'DELETE', url: apiClient.getUrl(url) });
}
function moduleLevel(apiClient) { return apiClient.ajax({ type: 'DELETE', url: apiClient.getUrl(url) }); }
function direct(id) {
    let url = '/api/orders/' + id;
    return fetch(url);
}
"#,
            false,
        );
        let routes: Vec<(&str, &str, &str)> =
            f.http_routes.iter().map(|r| (f.symbols[r.sym.unwrap()].name.as_str(), r.verb.as_str(), r.path.as_str())).collect();
        assert_eq!(
            routes,
            [
                ("createAlbum", "POST", "/Albums{~url@3}"),
                // Another local in the URL is a value, as a parameter is.
                ("removeCaption", "DELETE", "/Tracks/{}/Captions/{}{~url@12}"),
                ("plain", "DELETE", "/api/orders/{}{~url@18}"),
                // Not read: a `let` assigned again, a `var`, a `const` of the enclosing function, a parameter, a
                // name declared twice, a `const` of a block the request isn't in, one built from another local, a
                // joined array, a conditional with two paths, a module's. A name passed directly, as before.
                ("direct", "GET", "/api/orders/{}"),
            ]
        );
    }

    #[test]
    fn tsx_parses_jsx() {
        let f = extract("export function Cart({ id }: Props) { return <div onClick={() => load(id)}>{id}</div>; }\n", true);
        assert_eq!(f.parse_errors, 0);
        assert!(f.symbols.iter().any(|s| s.name == "Cart" && s.kind == SymbolKind::Function));
        assert!(f.refs.iter().any(|r| r.name == "load" && r.kind == RefKind::Call));
    }

    #[test]
    fn a_jsx_tag_calls_its_component() {
        let src = "export function Shelf({ items }: Props) {\n    return <Panel title={<Badge />}>\n        <Dialog.Title>{items.length}</Dialog.Title>\n        <div>{render(items)}</div>\n    </Panel>;\n}\n";
        let f = extract(src, true);
        assert_eq!(f.parse_errors, 0);
        let got: Vec<(&str, RefKind, Option<&str>, u32)> =
            f.refs.iter().map(|r| (r.name.as_str(), r.kind, r.receiver.as_deref(), r.line)).collect();
        // The opening and closing tags each call `Panel`, as the compiler binds both; `div` names nothing.
        for want in [("Panel", RefKind::Call, None, 2), ("Badge", RefKind::Call, None, 2), ("Panel", RefKind::Call, None, 5),
            ("Title", RefKind::Member, Some("Dialog"), 3), ("render", RefKind::Call, None, 4)]
        {
            assert!(got.contains(&want), "{want:?} in {got:?}");
        }
        assert!(!got.iter().any(|r| r.0 == "div" || (r.0 == "Panel" && r.1 == RefKind::TypeRef)), "{got:?}");
    }

    #[test]
    fn imports_and_reexports_bind_names_to_modules() {
        let f = extract(
            "import Default, * as CartActions from './cart.actions';\nimport { map, first as head } from 'rxjs/operators';\n\
             import type { Cart } from '../cart';\nexport * from './a';\nexport { b as c } from './b';\nexport * as fromD from './d';\n",
            false,
        );
        let b: Vec<(&str, &str, &str, bool)> =
            f.bindings.iter().map(|b| (b.local.as_str(), b.name.as_str(), b.module.as_str(), b.export)).collect();
        assert_eq!(
            b,
            [
                ("Default", "default", "./cart.actions", false),
                ("CartActions", "*", "./cart.actions", false),
                ("map", "map", "rxjs/operators", false),
                ("head", "first", "rxjs/operators", false),
                ("Cart", "Cart", "../cart", false),
                ("*", "*", "./a", true),
                ("c", "b", "./b", true),
                ("fromD", "*", "./d", true),
            ]
        );
    }

    #[test]
    fn method_decorators_are_attributes() {
        let f = extract(
            "export class Panel {\n  @Input() size = 1;\n  @HostListener('window:resize', ['$event'])\n  @Debounce()\n  onResize(e: Event) {}\n  plain() {}\n}\n",
            false,
        );
        let attrs = |name: &str| f.symbols.iter().find(|s| s.name == name).unwrap().attrs.clone();
        assert_eq!(attrs("onResize"), ["HostListener", "Debounce"]);
        assert_eq!(attrs("size"), ["Input"]);
        assert!(attrs("plain").is_empty());
    }

    #[test]
    fn test_suite_variables_and_injections_are_typed() {
        let f = extract(
            r#"
export const CART = new InjectionToken<CartService>('cart');
describe('Cart', () => {
  let service: Mocked<CartService>;
  let repo;
  let partial: Mocked<Pick<OrderService, 'save'>>;
  beforeEach(() => {
    repo = TestBed.inject(CartRepository);
  });
  it('loads', () => {
    service.load.mockReturnValue(1);
    repo.fetch();
    partial.save();
  });
});
export class Facade {
  private readonly store = inject(Store<State>);
  private readonly lookup = new Map<string, Order>();
  run() {
    this.store.dispatch(x);
    const check = (control: AbstractControl<string>) => control.value;
    const shadow = (repo) => repo.other();
  }
}
"#,
            false,
        );
        let rt = |name: &str| f.refs.iter().find(|r| r.name == name).unwrap_or_else(|| panic!("{name}")).receiver_type.clone();
        assert_eq!(rt("load").as_deref(), Some("CartService"));
        assert_eq!(rt("fetch").as_deref(), Some("CartRepository"));
        assert_eq!(rt("save").as_deref(), Some("OrderService"));
        assert_eq!(rt("value").as_deref(), Some("AbstractControl"));
        // An untyped callback parameter hides the suite's variable of the same name.
        assert_eq!(rt("other"), None);
        assert!(rt("dispatch").unwrap().contains("store"));
        let ty = |name: &str| f.symbols.iter().find(|s| s.name == name).unwrap().type_name.clone();
        assert_eq!(ty("store").as_deref(), Some("Store"));
        assert_eq!(ty("CART").as_deref(), Some("CartService"));
        assert_eq!(ty("lookup"), Some(TypeExpr::Dict(Box::new(TypeExpr::Named("string".into())), Box::new(TypeExpr::Named("Order".into()))).encode()));
    }

    #[test]
    fn providers_injectables_and_dialogs() {
        let f = extract(
            r#"
@NgModule({
  providers: [
    OrdersStore,
    { provide: OrdersFacade, useClass: HttpOrdersFacade },
    { provide: ORDERS_API, useExisting: forwardRef(() => OrdersApiImpl) },
    { provide: CARTS, useFactory: () => inject(CARTS, { skipSelf: true }) ?? inject(LocalCarts) },
    { provide: Clock, useFactory: makeClock, deps: [] },
    { provide: Flags, useFactory: (a: Auth) => a.flags$ },
    { provide: CONFIG, useValue: new AppConfig() },
  ],
})
export class OrdersModule {}
function makeClock(): SystemClock { return null; }
@Injectable({ providedIn: 'root', useClass: RealPrices })
export abstract class Prices {}
@Injectable({ providedIn: 'root' })
export class RealPrices extends Prices {}
export class Page {
  open() {
    this.dialog.open(EditOrderComponent, { width: '400px' });
    this.snack.openFromComponent(SavedToast);
    window.open('https://x');
    this.panel.open(this.config);
  }
}
"#,
            false,
        );
        type Row<'a> = (Option<&'a str>, Option<&'a str>, &'a str, Option<&'a str>);
        let di: Vec<Row> =
            f.di.iter().map(|b| (b.service.as_deref(), b.implementation.as_deref(), b.kind, b.lifetime)).collect();
        assert_eq!(
            di,
            [
                // Listed in `providers`: provided as itself.
                (Some("OrdersStore"), Some("OrdersStore"), "type", None),
                (Some("OrdersFacade"), Some("HttpOrdersFacade"), "type", None),
                (Some("ORDERS_API"), Some("OrdersApiImpl"), "existing", None),
                // The factory deferring to a parent's binding first, else making its own.
                (Some("CARTS"), Some("LocalCarts"), "parent", None),
                (Some("Clock"), Some("SystemClock"), "factory", None),
                (Some("Flags"), None, "factory", None),
                (Some("CONFIG"), Some("AppConfig"), "instance", None),
                (Some("Prices"), Some("RealPrices"), "type", Some("singleton")),
                (Some("RealPrices"), Some("RealPrices"), "type", Some("singleton")),
            ]
        );
        let opens: Vec<(&str, &str, &str)> = f
            .ng
            .iter()
            .filter(|n| n.kind == "opens")
            .map(|n| (f.symbols[n.sym.unwrap()].name.as_str(), n.value.as_str(), n.extra.as_deref().unwrap()))
            .collect();
        assert_eq!(opens, [("open", "EditOrderComponent", "open"), ("open", "SavedToast", "openFromComponent")]);
    }

    #[test]
    fn callbacks_a_field_or_method_stores_are_deferred() {
        let f = extract(
            r#"
export class Actions {
  private handlers = new Map([['a', () => this.svc.run()]]);
  prices$ = timer(0, 1000).pipe(switchMap(() => this.api.load()));
  rows = toSignal(this.api.rows().pipe(map(r => this.fmt.go(r))));
  load$ = createEffect(() => this.actions$.pipe(ofType(Load), mergeMap(() => this.api.all())));
  constructor() {
    this.handlers.set('renew', { id: 'renew', run: async (d) => await this.renew(d) });
    this.items.forEach((i) => this.each(i));
  }
}
"#,
            false,
        );
        // Calls stay with the field or method writing them; only no synthetic symbol is made.
        let from = |call: &str| f.symbols[f.refs.iter().find(|r| r.name == call).unwrap_or_else(|| panic!("{call}")).from.unwrap()].name.as_str();
        assert_eq!((from("run"), from("load"), from("renew"), from("each")), ("handlers", "prices$", "init", "init"));
        assert!(!f.symbols.iter().any(|s| s.name.contains('$') && s.name.ends_with("lazy")));
        let deferred: Vec<(&str, &str, &str)> = f
            .ng
            .iter()
            .filter(|n| n.kind == "deferred")
            .map(|n| (f.symbols[n.sym.unwrap()].name.as_str(), n.value.as_str(), n.extra.as_deref().unwrap()))
            .collect();
        // Stored, or lazy until something subscribes; signals and effects run their callbacks with the field.
        assert_eq!(deferred, [("handlers", "stored", "3-3"), ("prices$", "lazy", "4-4"), ("init", "stored", "8-8")]);
    }

    #[test]
    fn dispatch_payloads_and_effect_branches() {
        let f = extract(
            r#"
export class Page {
  readonly kind: Kind = 'privacy';
  off(): void {
    this.store.dispatch(Disable({ disable: new Setting(id, this.kind), mode: Mode.Hard, n: 3 }));
  }
}
export class Effects {
  disable$ = createEffect(() => this.actions$.pipe(ofType(Disable), mergeMap((action) => {
    switch (action.disable.setting) {
      case 'autoRenew':
        return this.svc.removeAutoRenew();
      case Setting.Privacy:
        return this.svc.removePrivacy();
      default:
        throw new Error('x');
    }
  })));
  only$ = createEffect(() => this.actions$.pipe(ofType(Save), filter(a => a.kind === 'bulk'), mergeMap(() => this.svc.bulk())));
  guarded$ = createEffect(() => this.actions$.pipe(ofType(Save), tap((a) => {
    if (a.kind === 'one') {
      this.svc.one();
    }
  })));
}
"#,
            false,
        );
        let facts: Vec<(&str, &str, &str, &str)> = f
            .ng
            .iter()
            .filter(|n| matches!(n.kind, "payload" | "branch"))
            .map(|n| (n.kind, f.symbols[n.sym.unwrap()].name.as_str(), n.value.as_str(), n.extra.as_deref().unwrap()))
            .collect();
        assert_eq!(
            facts,
            [
                ("payload", "off", "Disable", "this.kind Hard 3"),
                ("branch", "disable$", "autoRenew", "11-12"),
                ("branch", "disable$", "Privacy", "13-14"),
                ("branch", "disable$", "*", "15-16"),
                ("branch", "only$", "bulk", "19-19"),
                ("branch", "guarded$", "one", "21-23"),
            ]
        );
    }
}
