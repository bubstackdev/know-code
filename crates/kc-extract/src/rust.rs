//! Rust extraction over tree-sitter-rust: syntactic, shallower than Swift.
//!
//! `impl Type` and `impl Trait for Type` become extensions of `Type`, so methods merge onto their type and
//! `Trait` shows up as a conformance. Traits are protocols, struct fields and consts are properties, enum variants
//! are cases.

use std::collections::{HashMap, HashSet};

use tree_sitter::{Node, Parser};

use crate::swift::{children, collapse, last_component, line, name_col, starts_upper};
use crate::types::{PathRoot, Seg, TypeExpr};
use crate::{FileFacts, Import, Ref, RefKind, Relation, RelationKind, Symbol, SymbolKind};

pub const VERSION: u32 = 4;

const SIGNATURE_CAP: usize = 240;
const RECEIVER_CAP: usize = 48;

/// Standard types whose uses say nothing about the code base.
const COMMON_TYPES: &[&str] = &[
    "Arc", "Box", "Cell", "Cow", "HashMap", "HashSet", "BTreeMap", "BTreeSet", "Mutex", "Option", "PathBuf", "Path",
    "Rc", "RefCell", "Result", "RwLock", "Self", "String", "Vec", "VecDeque",
];

const BRANCH_KINDS: &[&str] = &["if_expression", "match_arm", "for_expression", "while_expression", "loop_expression"];

/// Control-flow blocks that nest, for [`Symbol::nesting`](crate::Symbol::nesting).
const NEST_KINDS: &[&str] = &[
    "if_expression", "match_expression", "for_expression", "while_expression", "loop_expression",
];

pub fn extract(source: &str) -> FileFacts {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_rust::LANGUAGE.into()).expect("tree-sitter-rust ABI matches tree-sitter");
    let Some(tree) = parser.parse(source, None) else {
        return FileFacts { parse_errors: 1, ..Default::default() };
    };
    let mut cx = Cx { src: source, facts: FileFacts::default(), scopes: Vec::new(), seen: HashSet::new() };
    cx.visit(tree.root_node());
    cx.facts
}

struct Scope {
    sym: usize,
    /// The type whose members this scope declares (`impl T`, a trait, a struct body).
    type_name: Option<String>,
    callable: bool,
    locals: HashMap<String, TypeExpr>,
}

struct Cx<'a> {
    src: &'a str,
    facts: FileFacts,
    scopes: Vec<Scope>,
    seen: HashSet<(Option<usize>, String, RefKind, u32, u32)>,
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
                && node.child_by_field_name("operator").is_some_and(|o| matches!(self.text(o), "&&" | "||"))))
            && let Some(s) = self.scopes.iter().rev().find(|s| s.callable) {
                self.facts.symbols[s.sym].complexity += 1;
            }
        if NEST_KINDS.contains(&node.kind())
            && let Some(s) = self.scopes.iter().rev().find(|s| s.callable)
        {
            let sym = &mut self.facts.symbols[s.sym];
            let depth = crate::nesting_depth(node, NEST_KINDS, (sym.start_byte, sym.end_byte));
            sym.nesting = sym.nesting.max(depth);
        }
        match node.kind() {
            "struct_item" | "enum_item" | "union_item" | "trait_item" => self.type_item(node),
            "impl_item" => self.impl_item(node),
            "function_item" | "function_signature_item" => self.function(node),
            "type_item" => {
                if let Some(n) = node.child_by_field_name("name") {
                    let name = self.text(n).to_string();
                    let sym = self.push(node, name, SymbolKind::Typealias, self.enclosing_type(), None);
                    if let Some(t) = node.child_by_field_name("type") {
                        self.facts.symbols[sym].type_name = type_expr(self.src, t).map(|t| t.encode());
                    }
                }
            }
            "const_item" | "static_item" => {
                if let Some(n) = node.child_by_field_name("name") {
                    let name = self.text(n).to_string();
                    let sym = self.push(node, name, SymbolKind::Property, self.enclosing_type(), node.child_by_field_name("value"));
                    if let Some(t) = node.child_by_field_name("type") {
                        self.facts.symbols[sym].type_name = type_expr(self.src, t).map(|t| t.encode());
                    }
                    self.scopes.push(Scope { sym, type_name: None, callable: true, locals: HashMap::new() });
                    if let Some(v) = node.child_by_field_name("value") {
                        self.visit(v);
                    }
                    self.scopes.pop();
                }
            }
            "use_declaration" => {
                if let Some(arg) = node.child_by_field_name("argument") {
                    self.facts.imports.push(Import { module: collapse(self.text(arg), 120), line: line(node) });
                }
            }
            "let_declaration" => {
                if let (Some(p), Some(ty)) = (
                    node.child_by_field_name("pattern").filter(|p| p.kind() == "identifier"),
                    node.child_by_field_name("type")
                        .and_then(|t| type_expr(self.src, t))
                        .or_else(|| node.child_by_field_name("value").and_then(|v| self.expr_type(v))),
                ) {
                    let name = self.text(p).to_string();
                    if let Some(s) = self.scopes.iter_mut().rev().find(|s| s.callable) {
                        s.locals.insert(name, ty);
                    }
                }
                self.visit_children(node);
            }
            "call_expression" => {
                self.call(node);
                for (i, c) in children(node).enumerate() {
                    if i == 0 {
                        if let Some(v) = c.child_by_field_name("value") {
                            self.visit(v);
                        }
                    } else {
                        self.visit(c);
                    }
                }
            }
            "struct_expression" => {
                if let Some(n) = node.child_by_field_name("name") {
                    let name = last_component(&self.text(n).replace("::", ".")).to_string();
                    self.add(name, None, None, RefKind::Instantiate, n);
                }
                if let Some(b) = node.child_by_field_name("body") {
                    self.visit(b);
                }
            }
            "field_expression" => {
                if let Some(f) = node.child_by_field_name("field").filter(|f| f.kind() == "field_identifier") {
                    let value = node.child_by_field_name("value");
                    let receiver = value.map(|v| collapse(self.text(v), RECEIVER_CAP));
                    let rt = value.and_then(|v| self.expr_type(v)).map(|t| t.encode());
                    let name = self.text(f).to_string();
                    self.add(name, receiver, rt, RefKind::Member, f);
                }
                if let Some(v) = node.child_by_field_name("value") {
                    self.visit(v);
                }
            }
            "type_identifier" => {
                let name = self.text(node);
                if !COMMON_TYPES.contains(&name) && self.scopes.iter().any(|s| s.callable || s.type_name.is_some()) {
                    self.add(name.to_string(), None, None, RefKind::TypeRef, node);
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

    fn push(&mut self, node: Node, name: String, kind: SymbolKind, container: Option<String>, header_stop: Option<Node>) -> usize {
        let end = node
            .child_by_field_name("body")
            .or(header_stop)
            .map_or(node.end_byte(), |b| b.start_byte());
        let signature = collapse(self.src[node.start_byte()..end].trim_end_matches(['=', ' ', '{']), SIGNATURE_CAP);
        let attrs = attributes(self.src, node);
        let mut tags = Vec::new();
        if attrs.iter().any(|a| a == "test" || a == "tokio::test") {
            tags.push("test");
        }
        if kind == SymbolKind::Function && name == "main" {
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

    fn type_item(&mut self, node: Node) {
        let Some(n) = node.child_by_field_name("name") else { return self.visit_children(node) };
        let name = self.text(n).to_string();
        let kind = match node.kind() {
            "enum_item" => SymbolKind::Enum,
            "trait_item" => SymbolKind::Protocol,
            _ => SymbolKind::Struct,
        };
        let sym = self.push(node, name.clone(), kind, None, None);
        self.scopes.push(Scope { sym, type_name: Some(name.clone()), callable: false, locals: HashMap::new() });
        // Supertraits: `trait A: B + C`, also type references from the trait, so `refs` finds subtraits.
        if let Some(bounds) = node.child_by_field_name("bounds") {
            for b in children(bounds).filter(|c| c.kind() == "type_identifier" || c.kind() == "scoped_type_identifier") {
                let target = last_component(&self.text(b).replace("::", ".")).to_string();
                self.add(target.clone(), None, None, RefKind::TypeRef, b);
                self.facts.relations.push(Relation { from: sym, target, kind: RelationKind::Inherits, line: line(b) });
            }
        }
        if let Some(body) = node.child_by_field_name("body") {
            for c in children(body) {
                match c.kind() {
                    "field_declaration" => {
                        if let Some(f) = c.child_by_field_name("name") {
                            let fname = self.text(f).to_string();
                            let s = self.push(c, fname, SymbolKind::Property, Some(name.clone()), None);
                            if let Some(t) = c.child_by_field_name("type") {
                                self.facts.symbols[s].type_name = type_expr(self.src, t).map(|t| t.encode());
                                self.visit(t);
                            }
                        }
                    }
                    "enum_variant" => {
                        if let Some(v) = c.child_by_field_name("name") {
                            let vname = self.text(v).to_string();
                            self.push(c, vname, SymbolKind::Case, Some(name.clone()), None);
                        }
                    }
                    _ => self.visit(c),
                }
            }
        }
        self.scopes.pop();
    }

    fn impl_item(&mut self, node: Node) {
        let Some(ty) = node.child_by_field_name("type") else { return self.visit_children(node) };
        let name = base_type_name(self.src, ty);
        let sym = self.push(node, name.clone(), SymbolKind::Extension, None, None);
        self.add(name.clone(), None, None, RefKind::TypeRef, ty);
        self.scopes.push(Scope { sym, type_name: Some(name), callable: false, locals: HashMap::new() });
        // `impl Trait for Type`: the trait is a type reference from the impl, so `refs` finds implementations.
        if let Some(tr) = node.child_by_field_name("trait") {
            let target = base_type_name(self.src, tr);
            self.add(target.clone(), None, None, RefKind::TypeRef, tr);
            self.facts.relations.push(Relation { from: sym, target, kind: RelationKind::Inherits, line: line(tr) });
        }
        if let Some(body) = node.child_by_field_name("body") {
            self.visit(body);
        }
        self.scopes.pop();
    }

    fn function(&mut self, node: Node) {
        let Some(n) = node.child_by_field_name("name") else { return };
        let name = self.text(n).to_string();
        let in_type = self.scopes.last().is_some_and(|s| s.type_name.is_some());
        let container = if in_type { self.enclosing_type() } else { None };
        let kind = if in_type { SymbolKind::Method } else { SymbolKind::Function };
        let sym = self.push(node, name, kind, container.clone(), None);
        self.facts.symbols[sym].type_name = node.child_by_field_name("return_type").and_then(|t| {
            if self.text(t) == "Self" { container.clone().map(TypeExpr::Named) } else { type_expr(self.src, t) }
        }).map(|t| t.encode());
        let mut locals = HashMap::new();
        if let Some(params) = node.child_by_field_name("parameters") {
            for p in children(params).filter(|c| c.kind() == "parameter") {
                if let (Some(pat), Some(t)) = (p.child_by_field_name("pattern"), p.child_by_field_name("type"))
                    && pat.kind() == "identifier"
                    && let Some(te) = type_expr(self.src, t)
                {
                    locals.insert(self.text(pat).to_string(), te);
                }
            }
        }
        self.scopes.push(Scope { sym, type_name: None, callable: true, locals });
        for c in children(node) {
            if c.kind() != "identifier" {
                self.visit(c);
            }
        }
        self.scopes.pop();
    }

    fn expr_type(&self, e: Node) -> Option<TypeExpr> {
        match e.kind() {
            "self" => Some(TypeExpr::root(PathRoot::SelfType)),
            "identifier" => {
                let name = self.text(e);
                self.scopes
                    .iter()
                    .rev()
                    .take_while(|s| s.callable)
                    .find_map(|s| s.locals.get(name).cloned())
                    .or_else(|| (!starts_upper(name)).then(|| TypeExpr::root(PathRoot::Prop(name.to_string()))))
            }
            "reference_expression" | "try_expression" | "parenthesized_expression" | "await_expression" => {
                children(e).filter(|c| c.is_named()).last().and_then(|c| self.expr_type(c))
            }
            "struct_expression" => e.child_by_field_name("name").map(|n| TypeExpr::Named(last_component(&self.text(n).replace("::", ".")).to_string())),
            "field_expression" => {
                let f = e.child_by_field_name("field")?;
                Some(self.expr_type(e.child_by_field_name("value")?)?.then(Seg::Member(self.text(f).to_string())))
            }
            "call_expression" => {
                let f = e.child_by_field_name("function")?;
                match f.kind() {
                    "identifier" => {
                        let name = self.text(f).to_string();
                        if starts_upper(&name) { Some(TypeExpr::Named(name)) } else { Some(TypeExpr::Call { owner: None, name }) }
                    }
                    "scoped_identifier" => {
                        let path = f.child_by_field_name("path").map(|p| self.text(p).replace("::", "."))?;
                        let name = f.child_by_field_name("name").map(|n| self.text(n).to_string())?;
                        let owner = last_component(&path).to_string();
                        if owner == "Self" {
                            Some(TypeExpr::Call { owner: self.enclosing_type(), name })
                        } else if starts_upper(&owner) {
                            Some(TypeExpr::Call { owner: Some(owner), name })
                        } else {
                            Some(TypeExpr::Call { owner: None, name })
                        }
                    }
                    "field_expression" => {
                        let name = f.child_by_field_name("field").map(|n| self.text(n).to_string())?;
                        Some(self.expr_type(f.child_by_field_name("value")?)?.then(Seg::Call(name)))
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn call(&mut self, node: Node) {
        let Some(f) = node.child_by_field_name("function") else { return };
        match f.kind() {
            "identifier" => {
                let name = self.text(f).to_string();
                let kind = if starts_upper(&name) { RefKind::Instantiate } else { RefKind::Call };
                self.add(name, None, None, kind, f);
            }
            "scoped_identifier" => {
                let Some(n) = f.child_by_field_name("name") else { return };
                let name = self.text(n).to_string();
                let path = f.child_by_field_name("path").map(|p| self.text(p).replace("::", "."));
                let owner = path.as_deref().map(|p| last_component(p).to_string());
                match owner {
                    Some(o) if o == "Self" => {
                        let ty = self.enclosing_type().map(|t| TypeExpr::Named(t).encode());
                        self.add(name, Some("Self".into()), ty, RefKind::Call, n);
                    }
                    Some(o) if starts_upper(&o) => {
                        if !COMMON_TYPES.contains(&o.as_str()) {
                            self.add(o.clone(), None, None, RefKind::TypeRef, f);
                        }
                        let kind = if starts_upper(&name) { RefKind::Instantiate } else { RefKind::Call };
                        self.add(name, Some(o.clone()), Some(TypeExpr::Named(o).encode()), kind, n);
                    }
                    // `module::function()`: a free function.
                    _ => self.add(name, None, None, RefKind::Call, n),
                }
            }
            "field_expression" => {
                let Some(field) = f.child_by_field_name("field") else { return };
                let value = f.child_by_field_name("value");
                let receiver = value.map(|v| collapse(self.text(v), RECEIVER_CAP));
                let rt = value.and_then(|v| self.expr_type(v)).map(|t| t.encode());
                let name = self.text(field).to_string();
                self.add(name, receiver, rt, RefKind::Call, field);
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

/// `Foo`, `Foo<T>`, `&mut Foo`, `crate::a::Foo` → `Foo`.
fn base_type_name(src: &str, t: Node) -> String {
    match t.kind() {
        "generic_type" => t.child_by_field_name("type").map_or_else(|| src[t.byte_range()].to_string(), |b| base_type_name(src, b)),
        "reference_type" | "pointer_type" => t.child_by_field_name("type").map_or_else(String::new, |b| base_type_name(src, b)),
        "scoped_type_identifier" => t.child_by_field_name("name").map_or_else(|| src[t.byte_range()].to_string(), |n| src[n.byte_range()].to_string()),
        _ => src[t.byte_range()].to_string(),
    }
}

fn type_expr(src: &str, t: Node) -> Option<TypeExpr> {
    match t.kind() {
        "type_identifier" | "scoped_type_identifier" => Some(TypeExpr::Named(base_type_name(src, t))),
        "reference_type" | "pointer_type" => t.child_by_field_name("type").and_then(|b| type_expr(src, b)),
        "generic_type" => {
            let base = base_type_name(src, t);
            let args: Vec<TypeExpr> = t
                .child_by_field_name("type_arguments")
                .map(|a| children(a).filter(|c| c.is_named()).filter_map(|c| type_expr(src, c)).collect())
                .unwrap_or_default();
            match (base.as_str(), args.as_slice()) {
                ("Vec" | "VecDeque" | "HashSet" | "BTreeSet", [e]) => Some(TypeExpr::Array(Box::new(e.clone()))),
                ("HashMap" | "BTreeMap", [k, v]) => Some(TypeExpr::Dict(Box::new(k.clone()), Box::new(v.clone()))),
                ("Option" | "Box" | "Arc" | "Rc" | "Result", [inner, ..]) => Some(inner.clone()),
                _ => Some(TypeExpr::Named(base)),
            }
        }
        "array_type" | "slice_type" => {
            t.child_by_field_name("element").and_then(|e| type_expr(src, e)).map(|e| TypeExpr::Array(Box::new(e)))
        }
        _ => None,
    }
}

/// `#[test]`, `#[derive(...)]`: the attribute items right above an item.
fn attributes(src: &str, node: Node) -> Vec<String> {
    let mut out = Vec::new();
    let mut prev = node.prev_sibling();
    while let Some(p) = prev {
        match p.kind() {
            "attribute_item" => {
                if let Some(a) = children(p).find(|c| c.kind() == "attribute")
                    && let Some(path) = children(a).find(|c| c.is_named())
                {
                    out.push(src[path.byte_range()].to_string());
                }
            }
            "line_comment" | "block_comment" => {}
            _ => break,
        }
        prev = p.prev_sibling();
    }
    out.reverse();
    out
}

/// First line of the `///` doc comment above an item.
fn doc_comment(src: &str, node: Node) -> Option<String> {
    let mut lines = Vec::new();
    let mut prev = node.prev_sibling();
    while let Some(p) = prev {
        match p.kind() {
            "attribute_item" => {}
            "line_comment" => {
                let t = &src[p.byte_range()];
                if !t.starts_with("///") {
                    break;
                }
                lines.push(t.trim_start_matches('/').trim().to_string());
            }
            _ => break,
        }
        prev = p.prev_sibling();
    }
    lines.last().filter(|l| !l.is_empty()).map(|l| collapse(l, 200))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_rust_items_impls_and_calls() {
        let f = extract(
            r#"
use std::path::Path;

/// The index.
pub struct Store { conn: Connection, pub n: u32 }
pub enum Mode { A, B(u32) }
pub trait Run { fn run(&self) -> u32; }
impl Run for Store { fn run(&self) -> u32 { self.n } }
impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        let s = Store { conn: open(), n: 1 };
        s.check();
        Store::make();
        Ok(s)
    }
}
#[test]
fn t() { let s = Store::open(p).unwrap(); s.run(); }
"#,
        );
        let sym = |name: &str, kind: SymbolKind| f.symbols.iter().find(|s| s.name == name && s.kind == kind).unwrap_or_else(|| panic!("{name}"));
        assert_eq!(sym("Store", SymbolKind::Struct).doc.as_deref(), Some("The index."));
        assert_eq!(sym("n", SymbolKind::Property).container.as_deref(), Some("Store"));
        sym("B", SymbolKind::Case);
        sym("Run", SymbolKind::Protocol);
        assert_eq!(sym("open", SymbolKind::Method).container.as_deref(), Some("Store"));
        assert_eq!(sym("run", SymbolKind::Method).container.as_deref(), Some("Run"));
        assert!(sym("t", SymbolKind::Function).tags.contains(&"test"));
        assert!(f.relations.iter().any(|r| r.target == "Run"));
        let calls: Vec<(&str, Option<&str>)> =
            f.refs.iter().filter(|r| r.kind == RefKind::Call).map(|r| (r.name.as_str(), r.receiver.as_deref())).collect();
        assert!(calls.contains(&("check", Some("s"))));
        assert!(calls.contains(&("make", Some("Store"))));
        // `s` in `t` is typed from `Store::open(p).unwrap()`'s receiver chain; `s.run()` is a call on it.
        assert!(calls.contains(&("run", Some("s"))));
        assert!(f.refs.iter().any(|r| r.name == "Store" && r.kind == RefKind::Instantiate));
        assert_eq!(f.parse_errors, 0);
    }

    #[test]
    fn traits_are_type_references_from_their_implementations() {
        let f = extract("pub trait Run: Base + Send {}\nimpl Run for Store {}\nimpl fmt::Display for Store {}\n");
        let refs: Vec<(&str, Option<&str>)> = f
            .refs
            .iter()
            .filter(|r| r.kind == RefKind::TypeRef)
            .map(|r| (r.name.as_str(), r.from.map(|i| f.symbols[i].name.as_str())))
            .collect();
        assert!(refs.contains(&("Base", Some("Run"))), "{refs:?}");
        // An impl is an extension of its type; the trait it implements is referenced from it.
        let impls: Vec<&str> = refs.iter().filter(|(_, from)| *from == Some("Store")).map(|(n, _)| *n).collect();
        assert!(impls.contains(&"Run") && impls.contains(&"Display"), "{refs:?}");
    }
}
