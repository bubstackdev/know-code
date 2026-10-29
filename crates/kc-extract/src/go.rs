//! Go extraction over tree-sitter-go.
//!
//! A Go file declares into its package: a directory's files that share a package clause. The clause is kept as
//! [`FileFacts::package`], and resolution joins it with the file's directory, so extraction stays per file.
//!
//! - Types are structs (`type T struct`, and any other defined type, `type Celsius float64`), protocols (interfaces)
//!   and type aliases (`type ID = string`). A method's container is its receiver's base type, wherever the method is
//!   declared, so a type's methods merge onto it. A struct's fields and an interface's methods are its members.
//! - An embedded field (`*Base`, `sync.Mutex`) and an embedded interface are `Inherits` relations: the embedded
//!   type's fields and methods are promoted to the outer type, which is what inheritance means for member lookup.
//! - Imports are bindings: `local` is the name written before the path (`h "net/http"`, `.`, `_`), or empty when
//!   the file uses the imported package's own name; `module` is the import path. A name qualified by an import
//!   (`fmt.Println`) is a reference with the qualifier as its receiver, and the qualifier itself isn't one.
//! - A bare identifier that isn't a local, a parameter or an import is a reference to a package-level declaration
//!   (a function, variable or constant), whatever file of the package declares it.
//! - Locals are typed where the syntax says: a declared type, a composite literal (`&T{}`), `new(T)`, a call (its
//!   declared result, finished at query time), a range over a typed value. Receivers chain through them
//!   (`s.cfg.Get()`), as [`TypeExpr::Path`]s.

use std::collections::{HashMap, HashSet};

use tree_sitter::{Node, Parser};

use crate::swift::{children, collapse, line, name_col};
use crate::types::{PathRoot, Seg, TypeExpr};
use crate::{FileFacts, ImportBinding, Ref, RefKind, Relation, RelationKind, Symbol, SymbolKind, Unsupported};

pub const VERSION: u32 = 1;

const SIGNATURE_CAP: usize = 240;
const RECEIVER_CAP: usize = 48;

/// Predeclared types: a use says nothing about the code base, and no declaration in it can be meant (a package
/// shadowing `error` or `any` is left to the reader).
const PREDECLARED_TYPES: &[&str] = &[
    "any", "bool", "byte", "comparable", "complex64", "complex128", "error", "float32", "float64", "int", "int8",
    "int16", "int32", "int64", "rune", "string", "uint", "uint8", "uint16", "uint32", "uint64", "uintptr",
];

const BRANCH_KINDS: &[&str] = &["if_statement", "for_statement", "expression_case", "type_case", "communication_case"];

/// Control-flow blocks that nest, for [`Symbol::nesting`](crate::Symbol::nesting).
const NEST_KINDS: &[&str] =
    &["if_statement", "for_statement", "expression_switch_statement", "type_switch_statement", "select_statement"];

/// Go's test functions, by prefix (`go test` runs them from `_test.go` files).
const TEST_PREFIXES: &[&str] = &["Test", "Benchmark", "Example", "Fuzz"];

pub fn extract(path: &str, source: &str) -> FileFacts {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_go::LANGUAGE.into()).expect("tree-sitter-go ABI matches tree-sitter");
    let Some(tree) = parser.parse(source, None) else {
        return FileFacts { parse_errors: 1, ..Default::default() };
    };
    let mut cx = Cx {
        src: source,
        test_file: path.ends_with("_test.go"),
        facts: FileFacts::default(),
        scopes: Vec::new(),
        imports: HashSet::new(),
        seen: HashSet::new(),
    };
    cx.header(tree.root_node());
    if let Some((constraint, line)) = excluded(path, source) {
        // The default build doesn't compile the file: its declarations are another build's, which resolution doesn't
        // take for the default build's (see `kc_store::go`).
        cx.facts.unsupported.push(Unsupported { area: "build", construct: constraint, line });
    }
    cx.visit(tree.root_node());
    cx.facts
}

/// Operating systems a file name or constraint may name; the default build is Linux (`unix` included).
const GOOS: &[&str] = &[
    "aix", "android", "darwin", "dragonfly", "freebsd", "hurd", "illumos", "ios", "js", "linux", "nacl", "netbsd",
    "openbsd", "plan9", "solaris", "wasip1", "windows", "zos",
];
const GOARCH: &[&str] = &[
    "386", "amd64", "arm", "arm64", "loong64", "mips", "mipsle", "mips64", "mips64le", "ppc64", "ppc64le", "riscv64",
    "s390x", "wasm",
];

/// Why the default build leaves `path` out, and the line saying so: a `_GOOS` file name for another system, or a
/// build constraint that is false without custom tags. The default build is Linux on any architecture, with cgo and
/// the gc compiler at any Go release: an architecture decides nothing, so a file that only it excludes stays in.
fn excluded(path: &str, source: &str) -> Option<(String, u32)> {
    let file = path.rsplit('/').next().unwrap_or(path);
    let stem = file.trim_end_matches(".go").trim_end_matches("_test");
    let parts: Vec<&str> = stem.split('_').collect();
    // `name_GOOS`, `name_GOARCH` or `name_GOOS_GOARCH`; a file named only for one (`linux.go`) isn't constrained.
    if parts.len() >= 2 {
        let tail = &parts[1..];
        let goos = match tail {
            [.., os, arch] if GOOS.contains(os) && GOARCH.contains(arch) => Some(*os),
            [.., os] if GOOS.contains(os) => Some(*os),
            _ => None,
        };
        if let Some(os) = goos.filter(|os| *os != "linux") {
            return Some((format!("file for `{os}` only"), 1));
        }
    }
    // Constraints come before the package clause, in line comments.
    for (i, raw) in source.lines().enumerate() {
        let l = raw.trim();
        if l.starts_with("package ") {
            break;
        }
        let expr = l.strip_prefix("//go:build ").map(|e| (e, false)).or_else(|| l.strip_prefix("// +build ").map(|e| (e, true)));
        if let Some((e, legacy)) = expr {
            let v = if legacy { legacy_constraint(e) } else { Constraint::new(e).eval() };
            if v == Some(false) {
                return Some((format!("build constraint `{}`", e.trim()), i as u32 + 1));
            }
            if !legacy {
                break; // `//go:build` supersedes `// +build` lines
            }
        }
    }
    None
}

/// The default build's value of one build tag: `None` for an architecture, which it doesn't fix.
fn tag(t: &str) -> Option<bool> {
    if GOARCH.contains(&t) {
        return None;
    }
    Some(matches!(t, "linux" | "unix" | "cgo" | "gc") || t.strip_prefix("go1.").is_some_and(|v| v.bytes().all(|b| b.is_ascii_digit())))
}

/// A `// +build` line: space-separated alternatives of comma-separated terms, each maybe negated with `!`.
fn legacy_constraint(e: &str) -> Option<bool> {
    let mut any: Option<bool> = Some(false);
    for alt in e.split_whitespace() {
        let mut all: Option<bool> = Some(true);
        for term in alt.split(',') {
            let v = match term.strip_prefix('!') {
                Some(t) => tag(t).map(|b| !b),
                None => tag(term),
            };
            all = and(all, v);
        }
        any = or(any, all);
    }
    any
}

fn and(a: Option<bool>, b: Option<bool>) -> Option<bool> {
    match (a, b) {
        (Some(false), _) | (_, Some(false)) => Some(false),
        (Some(true), Some(true)) => Some(true),
        _ => None,
    }
}

fn or(a: Option<bool>, b: Option<bool>) -> Option<bool> {
    match (a, b) {
        (Some(true), _) | (_, Some(true)) => Some(true),
        (Some(false), Some(false)) => Some(false),
        _ => None,
    }
}

/// A `//go:build` expression: `||`, `&&`, `!`, parentheses and tags, evaluated three-valued (see [`tag`]).
struct Constraint<'a> {
    tokens: Vec<&'a str>,
    at: usize,
}

impl<'a> Constraint<'a> {
    fn new(e: &'a str) -> Self {
        let mut tokens = Vec::new();
        let mut rest = e.trim();
        while !rest.is_empty() {
            let len = if rest.starts_with("||") || rest.starts_with("&&") {
                2
            } else if rest.starts_with(['!', '(', ')']) {
                1
            } else {
                rest.find(|c: char| c.is_whitespace() || "!()&|".contains(c)).unwrap_or(rest.len()).max(1)
            };
            tokens.push(&rest[..len]);
            rest = rest[len..].trim_start();
        }
        Constraint { tokens, at: 0 }
    }

    fn eval(mut self) -> Option<bool> {
        let v = self.or();
        // Anything left over is a constraint this reading doesn't follow: it decides nothing.
        if self.at == self.tokens.len() { v } else { None }
    }

    fn or(&mut self) -> Option<bool> {
        let mut v = self.and();
        while self.tokens.get(self.at) == Some(&"||") {
            self.at += 1;
            v = or(v, self.and());
        }
        v
    }

    fn and(&mut self) -> Option<bool> {
        let mut v = self.not();
        while self.tokens.get(self.at) == Some(&"&&") {
            self.at += 1;
            v = and(v, self.not());
        }
        v
    }

    fn not(&mut self) -> Option<bool> {
        match self.tokens.get(self.at).copied() {
            Some("!") => {
                self.at += 1;
                self.not().map(|b| !b)
            }
            Some("(") => {
                self.at += 1;
                let v = self.or();
                if self.tokens.get(self.at) == Some(&")") {
                    self.at += 1;
                }
                v
            }
            Some(t) => {
                self.at += 1;
                tag(t)
            }
            None => None,
        }
    }
}

/// A declaration being visited: a function or method (whose body has locals), a type, or a package-level value.
struct Scope {
    sym: usize,
    callable: bool,
    /// Locals and parameters, with their type where the syntax gives it.
    locals: HashMap<String, Option<TypeExpr>>,
    /// Type parameters, which are never references to a declaration.
    type_params: HashSet<String>,
}

struct Cx<'a> {
    src: &'a str,
    test_file: bool,
    facts: FileFacts,
    scopes: Vec<Scope>,
    /// The names imports bind in this file: aliases, and each unaliased path's likely package name.
    imports: HashSet<String>,
    seen: HashSet<(Option<usize>, String, RefKind, u32, u32)>,
}

impl<'a> Cx<'a> {
    fn text(&self, n: Node) -> &'a str {
        &self.src[n.byte_range()]
    }

    fn current(&self) -> Option<usize> {
        self.scopes.last().map(|s| s.sym)
    }

    /// The package clause and the imports, read first so that every reference sees every import.
    fn header(&mut self, root: Node) {
        for c in children(root) {
            match c.kind() {
                "package_clause" => {
                    self.facts.package = children(c).find(|n| n.kind() == "package_identifier").map(|n| self.text(n).to_string());
                }
                "import_declaration" => {
                    let specs: Vec<Node> = match children(c).find(|n| n.kind() == "import_spec_list") {
                        Some(list) => children(list).filter(|n| n.kind() == "import_spec").collect(),
                        None => children(c).filter(|n| n.kind() == "import_spec").collect(),
                    };
                    for spec in specs {
                        let Some(p) = spec.child_by_field_name("path") else { continue };
                        let module = self.text(p).trim_matches(['"', '`']).to_string();
                        let local = spec.child_by_field_name("name").map(|n| self.text(n).to_string()).unwrap_or_default();
                        if module == "C" {
                            // cgo: what `C.name` refers to is C code, which isn't read.
                            self.facts.unsupported.push(Unsupported { area: "cgo", construct: "import \"C\"".into(), line: line(spec) });
                            continue;
                        }
                        match local.as_str() {
                            "" => {
                                self.imports.insert(package_name_guess(&module).to_string());
                            }
                            "." | "_" => {}
                            l => {
                                self.imports.insert(l.to_string());
                            }
                        }
                        self.facts.bindings.push(ImportBinding { local, name: "*".into(), module, line: line(spec), export: false });
                    }
                }
                _ => {}
            }
        }
    }

    fn local(&self, name: &str) -> Option<&Option<TypeExpr>> {
        self.scopes.iter().rev().find_map(|s| s.locals.get(name))
    }

    fn is_type_param(&self, name: &str) -> bool {
        self.scopes.iter().any(|s| s.type_params.contains(name))
    }

    /// An import's name, used as a qualifier: not shadowed by a local.
    fn is_import(&self, name: &str) -> bool {
        self.imports.contains(name) && self.local(name).is_none()
    }

    fn declare(&mut self, name: &str, ty: Option<TypeExpr>) {
        if name == "_" {
            return;
        }
        if let Some(s) = self.scopes.iter_mut().rev().find(|s| s.callable) {
            s.locals.insert(name.to_string(), ty);
        }
    }

    fn visit(&mut self, node: Node) {
        if node.is_error() || node.is_missing() {
            self.facts.parse_errors += 1;
        }
        let kind = node.kind();
        if (BRANCH_KINDS.contains(&kind)
            || (kind == "binary_expression"
                && node.child_by_field_name("operator").is_some_and(|o| matches!(self.text(o), "&&" | "||"))))
            && let Some(s) = self.scopes.iter().rev().find(|s| s.callable)
        {
            self.facts.symbols[s.sym].complexity += 1;
        }
        if NEST_KINDS.contains(&kind)
            && let Some(s) = self.scopes.iter().rev().find(|s| s.callable)
        {
            let sym = &mut self.facts.symbols[s.sym];
            let depth = crate::nesting_depth(node, NEST_KINDS, (sym.start_byte, sym.end_byte));
            sym.nesting = sym.nesting.max(depth);
        }
        match kind {
            "package_clause" | "import_declaration" | "comment" => {}
            "type_declaration" => {
                for spec in children(node).filter(|c| matches!(c.kind(), "type_spec" | "type_alias")) {
                    self.type_spec(spec);
                }
            }
            "function_declaration" | "method_declaration" => self.function(node),
            "func_literal" => self.func_literal(node),
            "const_declaration" | "var_declaration" => self.values(node),
            "short_var_declaration" => self.short_var(node),
            "range_clause" => self.range(node),
            "type_switch_statement" => self.type_switch(node),
            "call_expression" => self.call(node),
            "type_conversion_expression" => self.conversion(node),
            "selector_expression" => self.selector(node, false),
            "composite_literal" => self.composite(node),
            "qualified_type" => {
                if let (Some(p), Some(n)) = (node.child_by_field_name("package"), node.child_by_field_name("name")) {
                    let (pkg, name) = (self.text(p).to_string(), self.text(n).to_string());
                    self.add(name, Some(pkg), None, RefKind::TypeRef, n);
                }
            }
            "type_identifier" => {
                let name = self.text(node);
                if !PREDECLARED_TYPES.contains(&name) && !self.is_type_param(name) {
                    self.add(name.to_string(), None, None, RefKind::TypeRef, node);
                }
            }
            "identifier" => self.identifier(node, RefKind::Member),
            "labeled_statement" => {
                // The label isn't a reference.
                for c in children(node).filter(|c| c.kind() != "label_name") {
                    self.visit(c);
                }
            }
            "break_statement" | "continue_statement" | "goto_statement" => {}
            "keyed_element" => self.keyed(node, None, None),
            _ => self.visit_children(node),
        }
    }

    fn visit_children(&mut self, node: Node) {
        for c in children(node) {
            self.visit(c);
        }
    }

    /// A bare identifier in an expression: a reference to a package-level declaration unless it is a local, a
    /// parameter, a type parameter or the blank identifier.
    fn identifier(&mut self, node: Node, kind: RefKind) {
        let name = self.text(node);
        if name == "_" || self.local(name).is_some() || self.is_type_param(name) {
            return;
        }
        self.add(name.to_string(), None, None, kind, node);
    }

    fn push(&mut self, node: Node, name: String, kind: SymbolKind, container: Option<String>, header_end: usize) -> usize {
        let signature = collapse(self.src[node.start_byte()..header_end].trim_end_matches(['=', ' ', '{']), SIGNATURE_CAP);
        let mut tags = Vec::new();
        if kind == SymbolKind::Function {
            if self.test_file && TEST_PREFIXES.iter().any(|p| is_test_name(&name, p)) {
                tags.push("test");
            }
            // `init` runs when its package is imported, so it isn't an entry point of the program.
            if name == "main" && self.facts.package.as_deref() == Some("main") {
                tags.push("entrypoint");
            }
        }
        self.facts.symbols.push(Symbol {
            parent: self.current(),
            name,
            kind,
            container,
            signature,
            doc: doc_comment(self.src, node),
            attrs: Vec::new(),
            tags,
            start_line: line(node),
            end_line: node.end_position().row as u32 + 1,
            start_byte: node.start_byte() as u32,
            end_byte: node.end_byte() as u32,
            complexity: if matches!(kind, SymbolKind::Function | SymbolKind::Method) { 1 } else { 0 },
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

    fn add(&mut self, name: String, receiver: Option<String>, receiver_type: Option<String>, kind: RefKind, node: Node) {
        if name.is_empty() || name == "_" {
            return;
        }
        let from = self.current();
        let (l, col) = (line(node), name_col(self.src, node, &name));
        if !self.seen.insert((from, name.clone(), kind, l, col)) {
            return;
        }
        self.facts.refs.push(Ref { from, name, receiver, receiver_type, kind, line: l, col, args: None });
    }

    fn type_params(&self, node: Node) -> HashSet<String> {
        let mut out = HashSet::new();
        if let Some(list) = node.child_by_field_name("type_parameters") {
            for decl in children(list).filter(|c| c.kind() == "type_parameter_declaration") {
                for n in children(decl).filter(|c| c.kind() == "identifier") {
                    out.insert(self.text(n).to_string());
                }
            }
        }
        out
    }

    fn type_spec(&mut self, spec: Node) {
        let Some(n) = spec.child_by_field_name("name") else { return };
        let name = self.text(n).to_string();
        let ty = spec.child_by_field_name("type");
        let kind = match (spec.kind(), ty.map(|t| t.kind())) {
            ("type_alias", _) => SymbolKind::Typealias,
            (_, Some("interface_type")) => SymbolKind::Protocol,
            _ => SymbolKind::Struct,
        };
        let header_end = match ty.map(|t| t.kind()) {
            Some("struct_type" | "interface_type") => ty.map_or(spec.end_byte(), |t| t.start_byte() + self.text(t).find('{').unwrap_or(0)),
            _ => spec.end_byte(),
        };
        let sym = self.push(spec, name.clone(), kind, None, header_end);
        let type_params = self.type_params(spec);
        if let Some(t) = ty
            && !matches!(t.kind(), "struct_type" | "interface_type")
        {
            // A defined type's underlying type, or an alias's target: what its values are.
            self.facts.symbols[sym].type_name = type_expr(self.src, t, &type_params).map(|t| t.encode());
        }
        self.scopes.push(Scope { sym, callable: false, locals: HashMap::new(), type_params });
        if let Some(tp) = spec.child_by_field_name("type_parameters") {
            self.visit(tp);
        }
        match ty {
            Some(t) if t.kind() == "struct_type" => self.struct_fields(t, sym, &name),
            Some(t) if t.kind() == "interface_type" => self.interface(t, sym, &name),
            Some(t) => self.visit(t),
            None => {}
        }
        self.scopes.pop();
    }

    fn struct_fields(&mut self, ty: Node, sym: usize, owner: &str) {
        let Some(list) = children(ty).find(|c| c.kind() == "field_declaration_list") else { return };
        for field in children(list).filter(|c| c.kind() == "field_declaration") {
            let Some(t) = field.child_by_field_name("type") else { continue };
            let names: Vec<Node> = children(field).filter(|c| c.kind() == "field_identifier").collect();
            let te = type_expr(self.src, t, &self.scopes.last().map(|s| s.type_params.clone()).unwrap_or_default())
                .map(|t| t.encode());
            if names.is_empty() {
                // An embedded field: named for its type, and its members promoted to the struct.
                let target = embedded_name(self.src, t);
                let fname = target.rsplit('.').next().unwrap_or(&target).to_string();
                if !fname.is_empty() {
                    let s = self.push(field, fname, SymbolKind::Property, Some(owner.to_string()), field.end_byte());
                    self.facts.symbols[s].type_name = te.clone();
                    self.facts.relations.push(Relation { from: sym, target, kind: RelationKind::Inherits, line: line(t) });
                }
            }
            for n in names {
                let fname = self.text(n).to_string();
                let s = self.push(field, fname, SymbolKind::Property, Some(owner.to_string()), field.end_byte());
                self.facts.symbols[s].type_name = te.clone();
            }
            self.visit(t);
        }
    }

    fn interface(&mut self, ty: Node, sym: usize, owner: &str) {
        for c in children(ty) {
            match c.kind() {
                "method_elem" => {
                    let Some(n) = c.child_by_field_name("name") else { continue };
                    let name = self.text(n).to_string();
                    let m = self.push(c, name, SymbolKind::Method, Some(owner.to_string()), c.end_byte());
                    self.facts.symbols[m].type_name = c.child_by_field_name("result").and_then(|r| result_type(self.src, r)).map(|t| t.encode());
                    self.scopes.push(Scope { sym: m, callable: true, locals: HashMap::new(), type_params: HashSet::new() });
                    for p in ["parameters", "result"] {
                        match c.child_by_field_name(p) {
                            Some(x) if x.kind() == "parameter_list" => self.params(x),
                            Some(x) => self.visit(x),
                            None => {}
                        }
                    }
                    self.scopes.pop();
                }
                "type_elem" => {
                    // An embedded interface (`fmt.Stringer`, `Reader`): its methods are the interface's too. A union
                    // or `~T` term is a type set, not an embedding.
                    let terms: Vec<Node> = children(c).filter(|t| t.is_named()).collect();
                    if let [t] = terms.as_slice()
                        && matches!(t.kind(), "type_identifier" | "qualified_type" | "generic_type")
                    {
                        let target = embedded_name(self.src, *t);
                        self.facts.relations.push(Relation { from: sym, target, kind: RelationKind::Inherits, line: line(*t) });
                    }
                    self.visit(c);
                }
                _ => {}
            }
        }
    }

    /// Declares a parameter list's names as locals, with their types, and visits the types.
    fn params(&mut self, list: Node) {
        for p in children(list).filter(|c| matches!(c.kind(), "parameter_declaration" | "variadic_parameter_declaration")) {
            let ty = p.child_by_field_name("type");
            let te = ty.and_then(|t| type_expr(self.src, t, &HashSet::new()));
            let te = if p.kind() == "variadic_parameter_declaration" { te.map(|t| TypeExpr::Array(Box::new(t))) } else { te };
            for n in children(p).filter(|c| c.kind() == "identifier") {
                let name = self.text(n).to_string();
                self.declare(&name, te.clone());
            }
            if let Some(t) = ty {
                self.visit(t);
            }
        }
    }

    fn function(&mut self, node: Node) {
        let Some(n) = node.child_by_field_name("name") else { return };
        let name = self.text(n).to_string();
        let receiver = node.child_by_field_name("receiver");
        let recv_type = receiver.and_then(|r| children(r).find(|c| c.kind() == "parameter_declaration")).and_then(|p| p.child_by_field_name("type"));
        let container = recv_type.map(|t| base_type_name(self.src, t)).filter(|c| !c.is_empty());
        let kind = if receiver.is_some() { SymbolKind::Method } else { SymbolKind::Function };
        let header_end = node.child_by_field_name("body").map_or(node.end_byte(), |b| b.start_byte());
        let sym = self.push(node, name, kind, container.clone(), header_end);
        self.facts.symbols[sym].type_name =
            node.child_by_field_name("result").and_then(|r| result_type(self.src, r)).map(|t| t.encode());
        // A method's receiver type's parameters (`func (l *List[T])`) are type parameters in its body.
        let mut type_params = self.type_params(node);
        if let Some(t) = recv_type {
            type_params.extend(receiver_type_params(self.src, t));
        }
        self.scopes.push(Scope { sym, callable: true, locals: HashMap::new(), type_params });
        if let Some(r) = receiver {
            self.params(r);
        }
        for f in ["type_parameters", "parameters", "result"] {
            if let Some(x) = node.child_by_field_name(f) {
                if matches!(x.kind(), "parameter_list") {
                    self.params(x);
                } else {
                    self.visit(x);
                }
            }
        }
        if let Some(b) = node.child_by_field_name("body") {
            self.visit(b);
        }
        self.scopes.pop();
    }

    /// A function literal: its parameters are locals of the enclosing function's scope for the literal's body, and
    /// what it references is the enclosing declaration's.
    fn func_literal(&mut self, node: Node) {
        let Some(sym) = self.current() else { return self.visit_children(node) };
        let outer: HashMap<String, Option<TypeExpr>> = self.scopes.iter().flat_map(|s| s.locals.clone()).collect();
        self.scopes.push(Scope { sym, callable: true, locals: outer, type_params: HashSet::new() });
        for f in ["parameters", "result"] {
            if let Some(x) = node.child_by_field_name(f) {
                if x.kind() == "parameter_list" {
                    self.params(x);
                } else {
                    self.visit(x);
                }
            }
        }
        if let Some(b) = node.child_by_field_name("body") {
            self.visit(b);
        }
        self.scopes.pop();
    }

    /// `const` and `var` declarations: package-level ones are declarations, others locals.
    fn values(&mut self, node: Node) {
        let specs: Vec<Node> = match children(node).find(|c| matches!(c.kind(), "var_spec_list" | "const_spec_list")) {
            Some(list) => children(list).filter(|c| matches!(c.kind(), "var_spec" | "const_spec")).collect(),
            None => children(node).filter(|c| matches!(c.kind(), "var_spec" | "const_spec")).collect(),
        };
        let top = !self.scopes.iter().any(|s| s.callable);
        // In a const group a spec without a type or value repeats the previous one's (`iota` enumerations).
        let mut last_type: Option<TypeExpr> = None;
        for spec in specs {
            let ty = spec.child_by_field_name("type");
            let value = spec.child_by_field_name("value");
            let declared = ty.and_then(|t| type_expr(self.src, t, &HashSet::new()));
            let names: Vec<Node> = children(spec).filter(|c| c.kind() == "identifier").collect();
            let values: Vec<Node> = value.map(|v| children(v).filter(|c| c.is_named()).collect()).unwrap_or_default();
            let spec_type = declared.clone().or_else(|| if values.is_empty() && spec.kind() == "const_spec" { last_type.clone() } else { None });
            if spec.kind() == "const_spec" && (ty.is_some() || value.is_some()) {
                last_type = declared.clone();
            }
            if top && names.iter().all(|n| self.text(*n) == "_") {
                // `var _ Iface = (*T)(nil)`: no declaration, but what it uses are references.
                for part in [ty, value].into_iter().flatten() {
                    self.visit(part);
                }
                continue;
            }
            for (i, n) in names.iter().enumerate() {
                let name = self.text(*n).to_string();
                if name == "_" {
                    continue;
                }
                let te = spec_type.clone().or_else(|| values.get(i).and_then(|v| self.expr_type(*v)));
                if top {
                    let s = self.push(spec, name, SymbolKind::Property, None, spec.end_byte());
                    self.facts.symbols[s].type_name = te.map(|t| t.encode());
                    self.scopes.push(Scope { sym: s, callable: false, locals: HashMap::new(), type_params: HashSet::new() });
                    if i == 0 {
                        if let Some(t) = ty {
                            self.visit(t);
                        }
                        if let Some(v) = value {
                            self.visit(v);
                        }
                    }
                    self.scopes.pop();
                } else {
                    self.declare(&name, te);
                }
            }
            if !top {
                if let Some(t) = ty {
                    self.visit(t);
                }
                if let Some(v) = value {
                    self.visit(v);
                }
            }
        }
    }

    fn short_var(&mut self, node: Node) {
        let left: Vec<Node> = node.child_by_field_name("left").map(|l| children(l).filter(|c| c.is_named()).collect()).unwrap_or_default();
        let right: Vec<Node> = node.child_by_field_name("right").map(|r| children(r).filter(|c| c.is_named()).collect()).unwrap_or_default();
        if let Some(r) = node.child_by_field_name("right") {
            self.visit(r);
        }
        for (i, l) in left.iter().enumerate() {
            if l.kind() != "identifier" {
                continue;
            }
            let te = if right.len() == left.len() {
                right.get(i).and_then(|v| self.expr_type(*v))
            } else if i == 0 && right.len() == 1 {
                // `v, err := f()`: the first result's type; `v, ok := m[k]`, `v, ok := x.(T)` likewise.
                self.expr_type(right[0])
            } else {
                None
            };
            let name = self.text(*l).to_string();
            if self.local(&name).is_some() && te.is_none() {
                continue; // reassigned in a multi-assignment: keep what is known
            }
            self.declare(&name, te);
        }
    }

    fn range(&mut self, node: Node) {
        let right = node.child_by_field_name("right");
        if let Some(r) = right {
            self.visit(r);
        }
        let over = right.and_then(|r| self.expr_type(r));
        let defines = children(node).any(|c| c.kind() == ":=");
        if let Some(left) = node.child_by_field_name("left") {
            let names: Vec<Node> = children(left).filter(|c| c.is_named()).collect();
            for (i, n) in names.iter().enumerate() {
                if !defines {
                    self.visit(*n);
                    continue;
                }
                if n.kind() != "identifier" {
                    continue;
                }
                let te = match (&over, i) {
                    (Some(TypeExpr::Dict(k, _)), 0) => Some((**k).clone()),
                    (Some(t), 1) => Some(TypeExpr::Elem(Box::new(t.clone()))),
                    _ => None,
                };
                let name = self.text(*n).to_string();
                self.declare(&name, te);
            }
        }
    }

    fn type_switch(&mut self, node: Node) {
        if let Some(v) = node.child_by_field_name("value") {
            self.visit(v);
        }
        let alias: Vec<String> = node
            .child_by_field_name("alias")
            .map(|a| children(a).filter(|c| c.kind() == "identifier").map(|c| self.text(c).to_string()).collect())
            .unwrap_or_default();
        for case in children(node).filter(|c| matches!(c.kind(), "type_case" | "default_case")) {
            // In a case naming one type the alias has that type; otherwise the switched value's.
            let types: Vec<Node> = case.children_by_field_name("type", &mut case.walk()).collect();
            let te = match types.as_slice() {
                [t] => type_expr(self.src, *t, &HashSet::new()),
                _ => None,
            };
            for a in &alias {
                self.declare(a, te.clone());
            }
            self.visit_children(case);
        }
    }

    fn call(&mut self, node: Node) {
        let Some(f) = node.child_by_field_name("function") else { return self.visit_children(node) };
        match f.kind() {
            "identifier" => self.identifier(f, RefKind::Call),
            "selector_expression" => self.selector(f, true),
            "generic_type" | "index_expression" => self.generic_call(f),
            "parenthesized_expression" => {
                // `(*T).M`-style method expressions and conversions: the parts are references of their own.
                self.visit(f);
            }
            _ => self.visit(f),
        }
        if let Some(t) = node.child_by_field_name("type_arguments") {
            self.visit(t);
        }
        if let Some(a) = node.child_by_field_name("arguments") {
            self.visit(a);
        }
    }

    /// `F[T](x)`: a call of the generic function `F`, which tree-sitter may parse as a conversion to a generic type.
    fn generic_call(&mut self, f: Node) {
        let callee = f.child_by_field_name("type").or_else(|| f.child_by_field_name("operand"));
        match callee {
            Some(c) if matches!(c.kind(), "identifier" | "type_identifier") => {
                let name = self.text(c);
                if self.local(name).is_none() {
                    self.add(name.to_string(), None, None, RefKind::Call, c);
                }
            }
            Some(c) if matches!(c.kind(), "selector_expression" | "qualified_type") => self.qualified_call(c),
            Some(c) => self.visit(c),
            None => {}
        }
        for args in ["type_arguments", "index", "indices"] {
            if let Some(a) = f.child_by_field_name(args) {
                self.visit(a);
            }
        }
        let mut cursor = f.walk();
        for a in f.children_by_field_name("index", &mut cursor) {
            self.visit(a);
        }
    }

    fn qualified_call(&mut self, c: Node) {
        if c.kind() == "selector_expression" {
            self.selector(c, true);
        } else if let (Some(p), Some(n)) = (c.child_by_field_name("package"), c.child_by_field_name("name")) {
            let (pkg, name) = (self.text(p).to_string(), self.text(n).to_string());
            self.add(name, Some(pkg), None, RefKind::Call, n);
        }
    }

    fn conversion(&mut self, node: Node) {
        match node.child_by_field_name("type") {
            Some(t) if t.kind() == "generic_type" => self.generic_call(t),
            Some(t) => self.visit(t),
            None => {}
        }
        if let Some(o) = node.child_by_field_name("operand") {
            self.visit(o);
        }
    }

    /// `x.f`: the qualifier and the field, method or package member. `called` when it is a call's function.
    fn selector(&mut self, node: Node, called: bool) {
        let (Some(operand), Some(field)) = (node.child_by_field_name("operand"), node.child_by_field_name("field")) else {
            return self.visit_children(node);
        };
        let name = self.text(field).to_string();
        let kind = if called { RefKind::Call } else { RefKind::Member };
        if operand.kind() == "identifier" {
            let q = self.text(operand);
            if let Some(t) = self.local(q) {
                let rt = t.as_ref().map(|t| t.encode());
                self.add(name, Some(q.to_string()), rt, kind, field);
                return;
            }
            if self.is_import(q) {
                self.add(name, Some(q.to_string()), None, kind, field);
                return;
            }
        }
        let receiver = collapse(self.text(operand), RECEIVER_CAP);
        let rt = self.expr_type(operand).map(|t| t.encode());
        self.add(name, Some(receiver), rt, kind, field);
        self.visit(operand);
    }

    fn composite(&mut self, node: Node) {
        let ty = node.child_by_field_name("type");
        let te = ty.and_then(|t| type_expr(self.src, t, &HashSet::new()));
        if let Some(t) = ty {
            match t.kind() {
                "type_identifier" => {
                    let name = self.text(t);
                    if !PREDECLARED_TYPES.contains(&name) && !self.is_type_param(name) {
                        self.add(name.to_string(), None, None, RefKind::Instantiate, t);
                    }
                }
                "qualified_type" => {
                    if let (Some(p), Some(n)) = (t.child_by_field_name("package"), t.child_by_field_name("name")) {
                        let (pkg, name) = (self.text(p).to_string(), self.text(n).to_string());
                        self.add(name, Some(pkg), None, RefKind::Instantiate, n);
                    }
                }
                "generic_type" => {
                    if let Some(g) = t.child_by_field_name("type") {
                        if g.kind() == "type_identifier" {
                            let name = self.text(g).to_string();
                            self.add(name, None, None, RefKind::Instantiate, g);
                        } else {
                            self.visit(g);
                        }
                    }
                    if let Some(a) = t.child_by_field_name("type_arguments") {
                        self.visit(a);
                    }
                }
                _ => self.visit(t),
            }
        }
        if let Some(body) = node.child_by_field_name("body") {
            self.literal_body(body, te);
        }
    }

    /// A composite literal's `{…}` of type `te`: a struct's keys are its fields; a slice's, array's or map's elements
    /// whose type is elided (`[]Item{{Name: "a"}}`) are literals of the element type.
    fn literal_body(&mut self, body: Node, te: Option<TypeExpr>) {
        let (fields, elem) = match &te {
            Some(TypeExpr::Array(e)) | Some(TypeExpr::Dict(_, e)) => (None, Some((**e).clone())),
            Some(TypeExpr::Named(_) | TypeExpr::Generic { .. }) => (te.clone(), None),
            _ => (None, None),
        };
        for c in children(body) {
            match c.kind() {
                "keyed_element" => self.keyed(c, fields.clone(), elem.clone()),
                "literal_element" => self.element(c, elem.clone()),
                _ => self.visit(c),
            }
        }
    }

    /// One element of a literal: an elided literal of type `elem`, or an expression.
    fn element(&mut self, node: Node, elem: Option<TypeExpr>) {
        match children(node).find(|c| c.is_named()) {
            Some(v) if v.kind() == "literal_value" => self.literal_body(v, elem),
            Some(v) => self.visit(v),
            None => {}
        }
    }

    /// A keyed element of a composite literal: `Field: value` when `owner` is the struct type, else `key: value`;
    /// `elem` is the type of a map's or slice's elided element literals.
    fn keyed(&mut self, node: Node, owner: Option<TypeExpr>, elem: Option<TypeExpr>) {
        let parts: Vec<Node> = children(node).filter(|c| c.is_named()).collect();
        let (key, rest) = match parts.split_first() {
            Some((k, r)) => (*k, r),
            None => return,
        };
        let key_ident = if key.kind() == "literal_element" { children(key).find(|c| c.is_named()) } else { Some(key) };
        match (owner, key_ident) {
            (Some(o), Some(k)) if k.kind() == "identifier" => {
                let name = self.text(k).to_string();
                self.add(name, None, Some(o.encode()), RefKind::Member, k);
            }
            _ if key.kind() == "literal_element" => self.element(key, None),
            _ => self.visit(key),
        }
        for r in rest {
            if r.kind() == "literal_element" {
                self.element(*r, elem.clone());
            } else {
                self.visit(*r);
            }
        }
    }

    /// What the syntax says about an expression's type.
    fn expr_type(&self, e: Node) -> Option<TypeExpr> {
        match e.kind() {
            "identifier" => {
                let name = self.text(e);
                match self.local(name) {
                    Some(t) => t.clone(),
                    None if self.is_import(name) => None,
                    None => Some(TypeExpr::root(PathRoot::Prop(name.to_string()))),
                }
            }
            "parenthesized_expression" => children(e).find(|c| c.is_named()).and_then(|c| self.expr_type(c)),
            "unary_expression" => {
                let op = e.child_by_field_name("operator").map(|o| self.text(o));
                match op {
                    Some("&" | "*") => e.child_by_field_name("operand").and_then(|o| self.expr_type(o)),
                    _ => None,
                }
            }
            "composite_literal" => e.child_by_field_name("type").and_then(|t| type_expr(self.src, t, &HashSet::new())),
            "type_assertion_expression" => e.child_by_field_name("type").and_then(|t| type_expr(self.src, t, &HashSet::new())),
            "index_expression" => e.child_by_field_name("operand").and_then(|o| self.expr_type(o)).map(|t| TypeExpr::Elem(Box::new(t))),
            "selector_expression" => {
                let operand = e.child_by_field_name("operand")?;
                let field = self.text(e.child_by_field_name("field")?).to_string();
                if operand.kind() == "identifier" && self.local(self.text(operand)).is_none() && self.is_import(self.text(operand)) {
                    // A package-level variable of an imported package.
                    return Some(TypeExpr::root(PathRoot::Prop(format!("{}.{field}", self.text(operand)))));
                }
                Some(self.expr_type(operand)?.then(Seg::Member(field)))
            }
            "call_expression" => {
                let f = e.child_by_field_name("function")?;
                match f.kind() {
                    "identifier" => {
                        let name = self.text(f);
                        match name {
                            "new" => e
                                .child_by_field_name("arguments")
                                .and_then(|a| children(a).find(|c| c.is_named()))
                                .and_then(|t| type_expr(self.src, t, &HashSet::new())),
                            "make" | "append" => e
                                .child_by_field_name("arguments")
                                .and_then(|a| children(a).find(|c| c.is_named()))
                                .and_then(|t| if name == "make" { type_expr(self.src, t, &HashSet::new()) } else { self.expr_type(t) }),
                            _ if self.local(name).is_some() => None,
                            _ => Some(TypeExpr::Call { owner: None, name: name.to_string() }),
                        }
                    }
                    "selector_expression" => {
                        let operand = f.child_by_field_name("operand")?;
                        let name = self.text(f.child_by_field_name("field")?).to_string();
                        if operand.kind() == "identifier" && self.local(self.text(operand)).is_none() && self.is_import(self.text(operand)) {
                            return Some(TypeExpr::Call { owner: Some(self.text(operand).to_string()), name });
                        }
                        Some(self.expr_type(operand)?.then(Seg::Call(name)))
                    }
                    "generic_type" | "index_expression" => {
                        let c = f.child_by_field_name("type").or_else(|| f.child_by_field_name("operand"))?;
                        (c.kind() == "identifier" || c.kind() == "type_identifier")
                            .then(|| TypeExpr::Call { owner: None, name: self.text(c).to_string() })
                    }
                    _ => None,
                }
            }
            "type_conversion_expression" => {
                let t = e.child_by_field_name("type")?;
                if t.kind() == "generic_type" {
                    let c = t.child_by_field_name("type")?;
                    return Some(TypeExpr::Call { owner: None, name: self.text(c).to_string() });
                }
                type_expr(self.src, t, &HashSet::new())
            }
            _ => None,
        }
    }
}

/// An imported package's usual name: its path's last element, without a major-version suffix (`.../chi/v5` is
/// `chi`) or a `gopkg.in` version (`yaml.v3` is `yaml`), with `-` dropped from `go-` and `-go` forms. The package
/// clause decides at query time; this only keeps qualifiers from being read as references.
pub fn package_name_guess(path: &str) -> &str {
    let mut parts = path.rsplit('/');
    let mut last = parts.next().unwrap_or(path);
    if last.len() > 1 && last.starts_with('v') && last[1..].bytes().all(|b| b.is_ascii_digit()) {
        last = parts.next().unwrap_or(last);
    }
    let last = last.split_once(".v").filter(|(_, v)| v.bytes().all(|b| b.is_ascii_digit())).map_or(last, |(n, _)| n);
    let last = last.strip_prefix("go-").unwrap_or(last);
    last.strip_suffix("-go").or_else(|| last.strip_suffix(".go")).unwrap_or(last)
}

fn is_test_name(name: &str, prefix: &str) -> bool {
    name.strip_prefix(prefix).is_some_and(|rest| rest.is_empty() || !rest.starts_with(|c: char| c.is_ascii_lowercase()))
}

/// A type's name as declarations are named: `T`, `pkg.T`, with pointers, generic arguments and parentheses removed.
fn embedded_name(src: &str, t: Node) -> String {
    match t.kind() {
        "pointer_type" | "parenthesized_type" | "type_elem" => children(t).find(|c| c.is_named()).map_or(String::new(), |c| embedded_name(src, c)),
        "generic_type" => t.child_by_field_name("type").map_or(String::new(), |c| embedded_name(src, c)),
        "qualified_type" | "type_identifier" => src[t.byte_range()].split_whitespace().collect(),
        _ => String::new(),
    }
}

/// A method receiver's base type name: `*List[T]` is `List`.
fn base_type_name(src: &str, t: Node) -> String {
    let n = embedded_name(src, t);
    n.rsplit('.').next().unwrap_or(&n).to_string()
}

/// The type parameters a receiver names: `*List[K, V]` names `K` and `V`.
fn receiver_type_params(src: &str, t: Node) -> Vec<String> {
    let g = match t.kind() {
        "pointer_type" => children(t).find(|c| c.kind() == "generic_type"),
        "generic_type" => Some(t),
        _ => None,
    };
    let Some(args) = g.and_then(|g| g.child_by_field_name("type_arguments")) else { return Vec::new() };
    let mut out = Vec::new();
    let mut stack = vec![args];
    while let Some(n) = stack.pop() {
        if n.kind() == "type_identifier" {
            out.push(src[n.byte_range()].to_string());
        }
        stack.extend(children(n));
    }
    out
}

/// A function's result type: the first result's, for a list (`(T, error)` is `T`).
fn result_type(src: &str, r: Node) -> Option<TypeExpr> {
    if r.kind() == "parameter_list" {
        let first = children(r).find(|c| c.kind() == "parameter_declaration")?;
        return type_expr(src, first.child_by_field_name("type")?, &HashSet::new());
    }
    type_expr(src, r, &HashSet::new())
}

/// A type as [`TypeExpr`]: a named type as written (`T`, `pkg.T`), pointers dropped; a slice or array as `Array`, a
/// map as `Dict`. `None` for function and channel types, type parameters and predeclared types.
fn type_expr(src: &str, t: Node, type_params: &HashSet<String>) -> Option<TypeExpr> {
    match t.kind() {
        "type_identifier" => {
            let name = &src[t.byte_range()];
            (!PREDECLARED_TYPES.contains(&name) && !type_params.contains(name)).then(|| TypeExpr::Named(name.to_string()))
        }
        "qualified_type" => Some(TypeExpr::Named(src[t.byte_range()].split_whitespace().collect())),
        "pointer_type" | "parenthesized_type" | "type_elem" => children(t).find(|c| c.is_named()).and_then(|c| type_expr(src, c, type_params)),
        "generic_type" => {
            let base = t.child_by_field_name("type")?;
            let name = match type_expr(src, base, type_params)? {
                TypeExpr::Named(n) => n,
                _ => return None,
            };
            let args: Vec<Option<TypeExpr>> = t
                .child_by_field_name("type_arguments")
                .map(|a| children(a).filter(|c| c.is_named()).map(|c| type_expr(src, c, type_params)).collect())
                .unwrap_or_default();
            Some(TypeExpr::Generic { name, args })
        }
        "slice_type" | "array_type" | "implicit_length_array_type" => {
            t.child_by_field_name("element").and_then(|e| type_expr(src, e, type_params)).map(|e| TypeExpr::Array(Box::new(e)))
        }
        "map_type" => {
            let k = t.child_by_field_name("key").and_then(|k| type_expr(src, k, type_params)).unwrap_or(TypeExpr::Named(String::new()));
            let v = type_expr(src, t.child_by_field_name("value")?, type_params)?;
            Some(TypeExpr::Dict(Box::new(k), Box::new(v)))
        }
        _ => None,
    }
}

/// The first line of the `//` comments directly above `node` (Go's doc comment).
fn doc_comment(src: &str, node: Node) -> Option<String> {
    let mut first = None;
    let mut at = node.start_position().row;
    let mut prev = node.prev_named_sibling();
    // A spec in a group (`var ( … )`) carries its own comment; a lone spec's is on its declaration.
    if prev.is_none() && let Some(p) = node.parent().filter(|p| matches!(p.kind(), "type_declaration" | "const_declaration" | "var_declaration")) {
        at = p.start_position().row;
        prev = p.prev_named_sibling();
    }
    while let Some(c) = prev {
        if c.kind() != "comment" || c.end_position().row + 1 != at {
            break;
        }
        first = Some(c);
        at = c.start_position().row;
        prev = c.prev_named_sibling();
    }
    let c = first?;
    let text = src[c.byte_range()].trim_start_matches("//").trim_start_matches("/*").trim();
    (!text.is_empty()).then(|| collapse(text.lines().next().unwrap_or(""), 160))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CART: &str = r#"package shop

import (
	"fmt"
	h "net/http"
	"github.com/example/kit/v2"
)

// Cart holds a customer's items.
type Cart struct {
	Items []Item
	*Base
	kit.Logger
	total int
}

// Store loads carts.
type Store interface {
	Load(id string) (*Cart, error)
	fmt.Stringer
}

type ID = string

type Celsius float64

const (
	Small Celsius = iota
	Large
)

var defaultCart = &Cart{total: 1}

func (c *Cart) Total() int { return c.total + Max }

func NewCart() *Cart { return &Cart{} }

func main() {
	c := NewCart()
	var s Store = c
	x, err := s.Load("a")
	c.Base.Run()
	fmt.Println(c.Total(), x, err, h.StatusOK, defaultCart.Items, kit.Version)
	for _, it := range c.Items {
		_ = it.Name
	}
}
"#;

    fn sym<'f>(f: &'f FileFacts, name: &str) -> &'f Symbol {
        f.symbols.iter().find(|s| s.name == name).unwrap_or_else(|| panic!("no symbol {name}"))
    }

    fn refs<'f>(f: &'f FileFacts, name: &str) -> Vec<&'f Ref> {
        f.refs.iter().filter(|r| r.name == name).collect()
    }

    #[test]
    fn declarations_and_package() {
        let f = extract("shop/cart.go", CART);
        assert_eq!(f.package.as_deref(), Some("shop"));
        assert_eq!(f.parse_errors, 0);
        assert_eq!(sym(&f, "Cart").kind, SymbolKind::Struct);
        assert_eq!(sym(&f, "Cart").doc.as_deref(), Some("Cart holds a customer's items."));
        assert_eq!(sym(&f, "Cart").signature, "Cart struct");
        assert_eq!(sym(&f, "Store").kind, SymbolKind::Protocol);
        assert_eq!(sym(&f, "ID").kind, SymbolKind::Typealias);
        assert_eq!(sym(&f, "Celsius").kind, SymbolKind::Struct);
        let total = sym(&f, "Total");
        assert_eq!((total.kind, total.container.as_deref()), (SymbolKind::Method, Some("Cart")));
        let load = sym(&f, "Load");
        assert_eq!((load.kind, load.container.as_deref(), load.type_name.as_deref()), (SymbolKind::Method, Some("Store"), Some("Cart")));
        assert_eq!(sym(&f, "Items").container.as_deref(), Some("Cart"));
        assert_eq!(sym(&f, "Base").container.as_deref(), Some("Cart"));
        assert_eq!(sym(&f, "Logger").type_name.as_deref(), Some("kit.Logger"));
        assert_eq!(sym(&f, "Large").type_name.as_deref(), Some("Celsius"));
        assert_eq!(sym(&f, "defaultCart").type_name.as_deref(), Some("Cart"));
        assert_eq!(sym(&f, "NewCart").type_name.as_deref(), Some("Cart"));
        assert!(sym(&f, "main").tags.contains(&"entrypoint") || f.package.as_deref() != Some("main"));
        let targets: Vec<&str> = f.relations.iter().map(|r| r.target.as_str()).collect();
        assert_eq!(targets, ["Base", "kit.Logger", "fmt.Stringer"]);
    }

    #[test]
    fn imports_are_bindings_and_qualifiers_are_not_references() {
        let f = extract("shop/cart.go", CART);
        let b: Vec<(&str, &str)> = f.bindings.iter().map(|b| (b.local.as_str(), b.module.as_str())).collect();
        assert_eq!(b, [("", "fmt"), ("h", "net/http"), ("", "github.com/example/kit/v2")]);
        for q in ["fmt", "h", "kit"] {
            assert!(refs(&f, q).is_empty(), "{q} is a qualifier");
        }
        let println = refs(&f, "Println");
        assert_eq!((println[0].kind, println[0].receiver.as_deref()), (RefKind::Call, Some("fmt")));
        assert_eq!(refs(&f, "StatusOK")[0].receiver.as_deref(), Some("h"));
        assert_eq!(refs(&f, "Version")[0].receiver.as_deref(), Some("kit"));
        assert_eq!(package_name_guess("github.com/go-chi/chi/v5"), "chi");
        assert_eq!(package_name_guess("gopkg.in/yaml.v3"), "yaml");
        assert_eq!(package_name_guess("github.com/mattn/go-sqlite3"), "sqlite3");
    }

    #[test]
    fn locals_type_receivers() {
        let f = extract("shop/cart.go", CART);
        let load = refs(&f, "Load");
        assert_eq!(load.iter().map(|r| r.receiver_type.as_deref()).collect::<Vec<_>>(), [Some("Store")]);
        let total: Vec<Option<&str>> = refs(&f, "Total").iter().map(|r| r.receiver_type.as_deref()).collect();
        assert!(total.contains(&Some(r#"{"call":{"owner":null,"name":"NewCart"}}"#)), "{total:?}");
        // `c.total` in the method: the receiver's declared type.
        assert!(refs(&f, "total").iter().any(|r| r.receiver_type.as_deref() == Some("Cart")));
        // A keyed field of a struct literal is the struct's member.
        assert!(refs(&f, "total").iter().any(|r| r.receiver.is_none() && r.receiver_type.as_deref() == Some("Cart")));
        // `c.Base.Run()`: a chain through the field.
        let run = refs(&f, "Run");
        assert!(run[0].receiver_type.as_deref().is_some_and(|t| t.contains(r#""member":"Base""#)), "{:?}", run[0].receiver_type);
        // `it` ranges over `c.Items`.
        assert!(refs(&f, "Name")[0].receiver_type.as_deref().is_some_and(|t| t.contains("elem")));
        // Package-level names used bare are references; locals and parameters aren't.
        assert_eq!(refs(&f, "Max").len(), 1);
        assert_eq!(refs(&f, "NewCart")[0].kind, RefKind::Call);
        assert_eq!(refs(&f, "defaultCart").len(), 1);
        for local in ["c", "s", "x", "err", "it", "id"] {
            assert!(refs(&f, local).is_empty(), "{local} is a local");
        }
    }

    #[test]
    fn generics_and_type_parameters() {
        let src = "package list\n\ntype List[T any] struct{ xs []T }\n\nfunc (l *List[T]) Push(x T) { l.xs = append(l.xs, x) }\n\nfunc New[T any](x T) *List[T] { return &List[T]{} }\n\nfunc use() { l := New[int](1); l.Push(2) }\n";
        let f = extract("list/list.go", src);
        assert_eq!(f.parse_errors, 0);
        assert!(refs(&f, "T").is_empty(), "type parameters aren't references");
        assert_eq!(sym(&f, "Push").container.as_deref(), Some("List"));
        assert_eq!(refs(&f, "New")[0].kind, RefKind::Call);
        assert!(refs(&f, "List").iter().any(|r| r.kind == RefKind::Instantiate));
        assert_eq!(refs(&f, "Push")[0].receiver_type.as_deref(), Some(r#"{"call":{"owner":null,"name":"New"}}"#));
    }

    #[test]
    fn builds_other_than_the_default_are_unsupported() {
        let area = |path: &str, src: &str| extract(path, src).unsupported.first().map(|u| u.construct.clone());
        assert_eq!(area("x/open_windows.go", "package x\n"), Some("file for `windows` only".into()));
        assert_eq!(area("x/open_linux.go", "package x\n"), None);
        assert_eq!(area("x/open_arm64.go", "package x\n"), None);
        assert_eq!(area("x/b.go", "//go:build nomsgpack\n\npackage x\n"), Some("build constraint `nomsgpack`".into()));
        assert_eq!(area("x/b.go", "//go:build !nomsgpack\n\npackage x\n"), None);
        assert_eq!(area("x/b.go", "//go:build (linux || darwin) && !appengine\n\npackage x\n"), None);
        assert_eq!(area("x/b.go", "//go:build windows && amd64\n\npackage x\n"), Some("build constraint `windows && amd64`".into()));
        assert_eq!(area("x/b.go", "//go:build amd64\n\npackage x\n"), None);
        assert_eq!(area("x/b.go", "// +build appengine\n\npackage x\n"), Some("build constraint `appengine`".into()));
        assert_eq!(area("x/b.go", "// +build !appengine linux\n\npackage x\n"), None);
    }

    #[test]
    fn cgo_is_unsupported() {
        let f = extract("x/x.go", "package x\n\n// #include <stdio.h>\nimport \"C\"\n\nfunc f() { C.puts(nil) }\n");
        assert_eq!(f.unsupported.iter().map(|u| (u.area, u.line)).collect::<Vec<_>>(), [("cgo", 4)]);
        assert!(f.bindings.is_empty());
    }

    #[test]
    fn elided_literals_and_blank_declarations() {
        let src = "package x\n\ntype Item struct{ Name string }\n\nvar items = []*Item{{Name: \"a\"}, {Name: \"b\"}}\nvar byKey = map[string]Item{\"k\": {Name: \"c\"}}\n\nvar _ Namer = (*Item)(nil)\n\ntype Namer interface{ Names() []Item }\n";
        let f = extract("x/x.go", src);
        let names: Vec<Option<&str>> = refs(&f, "Name").iter().map(|r| r.receiver_type.as_deref()).collect();
        assert_eq!(names, [Some("Item"), Some("Item"), Some("Item")]);
        assert!(refs(&f, "Namer").iter().any(|r| r.kind == RefKind::TypeRef));
        // `(*Item)(nil)` reads as a dereference: a bare name, which resolution also places among types.
        assert_eq!(refs(&f, "Item").len(), 4, "{:?}", refs(&f, "Item"));
    }

    #[test]
    fn tests_and_entrypoints() {
        let f = extract("shop/cart_test.go", "package shop\n\nfunc TestTotal(t *testing.T) {}\nfunc Testify() {}\nfunc BenchmarkTotal(b *testing.B) {}\n");
        assert!(sym(&f, "TestTotal").tags.contains(&"test"));
        assert!(sym(&f, "BenchmarkTotal").tags.contains(&"test"));
        assert!(!sym(&f, "Testify").tags.contains(&"test"));
        let m = extract("cmd/app/main.go", "package main\n\nfunc main() {}\nfunc init() {}\n");
        assert!(sym(&m, "main").tags.contains(&"entrypoint"));
        assert!(!sym(&m, "init").tags.contains(&"entrypoint"));
    }
}
