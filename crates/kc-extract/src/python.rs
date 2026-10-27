//! Python extraction over tree-sitter-python: syntactic, shallower than Swift.
//!
//! Classes, functions and methods (`__init__` as the initializer), class attributes and `self.x = …` attributes as
//! properties, base classes as relations, calls, attribute uses, imports and annotation types.

use std::collections::{HashMap, HashSet};

use tree_sitter::{Node, Parser};

use crate::swift::{children, collapse, line, name_col, starts_upper};
use crate::types::{PathRoot, Seg, TypeExpr};
use crate::{FileFacts, Import, ImportBinding, Ref, RefKind, Relation, RelationKind, Symbol, SymbolKind};

pub const VERSION: u32 = 5;

const SIGNATURE_CAP: usize = 240;
const RECEIVER_CAP: usize = 48;

const COMMON_TYPES: &[&str] = &[
    "Any", "Callable", "Dict", "Iterable", "Iterator", "List", "Optional", "Sequence", "Set", "Tuple", "Union", "bool",
    "bytes", "dict", "float", "int", "list", "object", "set", "str", "tuple", "type",
];

const BRANCH_KINDS: &[&str] = &[
    "if_statement", "elif_clause", "for_statement", "while_statement", "except_clause", "conditional_expression",
    "boolean_operator", "for_in_clause",
];

/// Control-flow blocks that nest, for [`Symbol::nesting`](crate::Symbol::nesting).
const NEST_KINDS: &[&str] = &[
    "if_statement", "for_statement", "while_statement", "try_statement", "with_statement", "match_statement",
];

pub fn extract(source: &str) -> FileFacts {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_python::LANGUAGE.into()).expect("tree-sitter-python ABI matches tree-sitter");
    let Some(tree) = parser.parse(source, None) else {
        return FileFacts { parse_errors: 1, ..Default::default() };
    };
    let mut cx = Cx { src: source, facts: FileFacts::default(), scopes: Vec::new(), seen: HashSet::new(), attrs_seen: HashSet::new() };
    cx.visit(tree.root_node());
    cx.facts
}

struct Scope {
    sym: usize,
    type_name: Option<String>,
    callable: bool,
    locals: HashMap<String, TypeExpr>,
}

struct Cx<'a> {
    src: &'a str,
    facts: FileFacts,
    scopes: Vec<Scope>,
    seen: HashSet<(Option<usize>, String, RefKind, u32, u32)>,
    /// `self.x` attributes already recorded per class.
    attrs_seen: HashSet<(String, String)>,
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
        self.scopes.last().is_some_and(|s| s.callable)
    }

    fn visit(&mut self, node: Node) {
        if node.is_error() || node.is_missing() {
            self.facts.parse_errors += 1;
        }
        if BRANCH_KINDS.contains(&node.kind())
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
        }
        match node.kind() {
            "class_definition" => self.class(node, &[]),
            "function_definition" => self.function(node, &[]),
            "decorated_definition" => {
                let decorators: Vec<String> = children(node)
                    .filter(|c| c.kind() == "decorator")
                    .filter_map(|d| children(d).find(|c| c.is_named()))
                    .map(|e| {
                        let t = self.text(e);
                        t.split('(').next().unwrap_or(t).to_string()
                    })
                    .collect();
                if let Some(def) = node.child_by_field_name("definition") {
                    match def.kind() {
                        "class_definition" => self.class(def, &decorators),
                        "function_definition" => self.function(def, &decorators),
                        _ => self.visit(def),
                    }
                }
            }
            "import_statement" | "import_from_statement" => {
                let module = node
                    .child_by_field_name("module_name")
                    .or_else(|| node.child_by_field_name("name"))
                    .map(|m| self.text(m).to_string());
                if let Some(m) = module {
                    self.facts.imports.push(Import { module: collapse(&m, 120), line: line(node) });
                }
                self.bindings(node);
            }
            "assignment" => self.assignment(node),
            "call" => {
                self.call(node);
                for (i, c) in children(node).enumerate() {
                    if i == 0 {
                        if let Some(o) = c.child_by_field_name("object") {
                            self.visit(o);
                        }
                    } else {
                        self.visit(c);
                    }
                }
            }
            "attribute" => {
                if let Some(a) = node.child_by_field_name("attribute") {
                    let obj = node.child_by_field_name("object");
                    let receiver = obj.map(|o| collapse(self.text(o), RECEIVER_CAP));
                    let rt = obj.and_then(|o| self.expr_type(o)).map(|t| t.encode());
                    let name = self.text(a).to_string();
                    self.add(name, receiver, rt, RefKind::Member, a);
                }
                if let Some(o) = node.child_by_field_name("object") {
                    self.visit(o);
                }
            }
            "type" => {
                for id in descendants(node).into_iter().filter(|n| n.kind() == "identifier") {
                    let name = self.text(id);
                    if !COMMON_TYPES.contains(&name) && starts_upper(name) {
                        self.add(name.to_string(), None, None, RefKind::TypeRef, id);
                    }
                }
            }
            _ => {
                for c in children(node) {
                    self.visit(c);
                }
            }
        }
    }

    /// The names an `import` or `from … import` binds (see [`ImportBinding`]).
    fn bindings(&mut self, node: Node) {
        let l = line(node);
        let from = node.child_by_field_name("module_name").filter(|_| node.kind() == "import_from_statement").map(|m| self.text(m).to_string());
        let mut cursor = node.walk();
        let names: Vec<Node> = node.children_by_field_name("name", &mut cursor).collect();
        let mut out: Vec<(String, String, String)> = Vec::new();
        for n in names {
            let (target, alias) = match n.kind() {
                "aliased_import" => (n.child_by_field_name("name"), n.child_by_field_name("alias")),
                _ => (Some(n), None),
            };
            let Some(target) = target else { continue };
            let target = self.text(target).to_string();
            let alias = alias.map(|a| self.text(a).to_string());
            match &from {
                // `from m import x as y`
                Some(m) => out.push((alias.unwrap_or_else(|| target.clone()), target, m.clone())),
                // `import a.b as c`, `import a.b`
                None => out.push((alias.unwrap_or_else(|| target.clone()), "*".to_string(), target)),
            }
        }
        if let Some(m) = &from
            && children(node).any(|c| c.kind() == "wildcard_import")
        {
            out.push(("*".to_string(), "*".to_string(), m.clone()));
        }
        for (local, name, module) in out {
            self.facts.bindings.push(ImportBinding { local, name, module: collapse(&module, 120), line: l, export: false });
        }
    }

    fn push(&mut self, node: Node, name: String, kind: SymbolKind, container: Option<String>, attrs: &[String]) -> usize {
        let end = node.child_by_field_name("body").map_or(node.end_byte(), |b| b.start_byte());
        let signature = collapse(self.src[node.start_byte()..end].trim_end().trim_end_matches(':'), SIGNATURE_CAP);
        let mut tags = Vec::new();
        if (kind == SymbolKind::Function || kind == SymbolKind::Method) && name.starts_with("test") {
            tags.push("test");
        }
        self.facts.symbols.push(Symbol {
            parent: self.current(),
            name,
            kind,
            container,
            signature,
            doc: docstring(self.src, node),
            attrs: attrs.to_vec(),
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

    fn class(&mut self, node: Node, decorators: &[String]) {
        let Some(n) = node.child_by_field_name("name") else { return };
        let name = self.text(n).to_string();
        let outer = self.enclosing_type();
        let qualified = match &outer {
            Some(o) => format!("{o}.{name}"),
            None => name.clone(),
        };
        let sym = self.push(node, name, SymbolKind::Class, outer, decorators);
        self.scopes.push(Scope { sym, type_name: Some(qualified), callable: false, locals: HashMap::new() });
        if let Some(bases) = node.child_by_field_name("superclasses") {
            for b in children(bases).filter(|c| matches!(c.kind(), "identifier" | "attribute")) {
                let target = self.text(b).rsplit('.').next().unwrap_or("").to_string();
                if target == "TestCase" {
                    self.facts.symbols[sym].tags.push("test");
                }
                // Also a type reference from the class, so `refs` finds subclasses.
                if !COMMON_TYPES.contains(&target.as_str()) {
                    self.add(target.clone(), None, None, RefKind::TypeRef, b);
                }
                self.facts.relations.push(Relation { from: sym, target, kind: RelationKind::Inherits, line: line(b) });
            }
        }
        if let Some(body) = node.child_by_field_name("body") {
            self.visit(body);
        }
        self.scopes.pop();
    }

    fn function(&mut self, node: Node, decorators: &[String]) {
        let Some(n) = node.child_by_field_name("name") else { return };
        let name = self.text(n).to_string();
        let in_type = self.scopes.last().is_some_and(|s| s.type_name.is_some());
        let container = if in_type { self.enclosing_type() } else { None };
        let kind = match (in_type, name.as_str()) {
            (true, "__init__") => SymbolKind::Init,
            (true, _) if decorators.iter().any(|d| d == "property") => SymbolKind::Property,
            (true, _) => SymbolKind::Method,
            _ => SymbolKind::Function,
        };
        let name = if kind == SymbolKind::Init { "init".to_string() } else { name };
        let sym = self.push(node, name, kind, container.clone(), decorators);
        self.facts.symbols[sym].type_name =
            node.child_by_field_name("return_type").and_then(|t| annotation(self.src, t)).map(|t| t.encode());
        let mut locals = HashMap::new();
        if let Some(params) = node.child_by_field_name("parameters") {
            for p in children(params) {
                if matches!(p.kind(), "typed_parameter" | "typed_default_parameter")
                    && let Some(id) = children(p).find(|c| c.kind() == "identifier")
                    && let Some(t) = p.child_by_field_name("type").and_then(|t| annotation(self.src, t))
                {
                    locals.insert(self.text(id).to_string(), t);
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

    fn assignment(&mut self, node: Node) {
        let left = node.child_by_field_name("left");
        let right = node.child_by_field_name("right");
        let annotated = node.child_by_field_name("type").and_then(|t| annotation(self.src, t));
        let ty = annotated.or_else(|| right.and_then(|r| self.expr_type(r)));
        match left.map(|l| (l.kind(), l)) {
            // Module- or class-level `NAME = …`: a property.
            Some(("identifier", l)) if !self.in_callable() => {
                let name = self.text(l).to_string();
                let container = self.enclosing_type();
                let sym = self.push(node, name, SymbolKind::Property, container, &[]);
                self.facts.symbols[sym].type_name = ty.map(|t| t.encode());
                self.scopes.push(Scope { sym, type_name: None, callable: true, locals: HashMap::new() });
                if let Some(r) = right {
                    self.visit(r);
                }
                self.scopes.pop();
                return;
            }
            Some(("identifier", l)) => {
                if let Some(t) = ty
                    && let Some(s) = self.scopes.iter_mut().rev().find(|s| s.callable)
                {
                    s.locals.insert(self.src[l.byte_range()].to_string(), t);
                }
            }
            // `self.x = …` in a method: an attribute of the class.
            Some(("attribute", l))
                if l.child_by_field_name("object").is_some_and(|o| self.text(o) == "self") =>
            {
                if let (Some(a), Some(class)) = (l.child_by_field_name("attribute"), self.enclosing_type()) {
                    let name = self.text(a).to_string();
                    if self.attrs_seen.insert((class.clone(), name.clone())) {
                        // Attach to the class, not the method: hang it off the class scope.
                        let class_sym = self.scopes.iter().rev().find(|s| s.type_name.is_some()).map(|s| s.sym);
                        let sym = self.push(node, name, SymbolKind::Property, Some(class), &[]);
                        self.facts.symbols[sym].parent = class_sym;
                        self.facts.symbols[sym].type_name = ty.map(|t| t.encode());
                    }
                }
            }
            _ => {}
        }
        for c in children(node) {
            self.visit(c);
        }
    }

    fn expr_type(&self, e: Node) -> Option<TypeExpr> {
        match e.kind() {
            "identifier" => {
                let name = self.text(e);
                if name == "self" {
                    return Some(TypeExpr::root(PathRoot::SelfType));
                }
                self.scopes
                    .iter()
                    .rev()
                    .take_while(|s| s.callable)
                    .find_map(|s| s.locals.get(name).cloned())
                    .or_else(|| (!starts_upper(name)).then(|| TypeExpr::root(PathRoot::Prop(name.to_string()))))
            }
            "attribute" => {
                let a = e.child_by_field_name("attribute")?;
                Some(self.expr_type(e.child_by_field_name("object")?)?.then(Seg::Member(self.text(a).to_string())))
            }
            "call" => {
                let f = e.child_by_field_name("function")?;
                match f.kind() {
                    "identifier" => {
                        let name = self.text(f).to_string();
                        if starts_upper(&name) { Some(TypeExpr::Named(name)) } else { Some(TypeExpr::Call { owner: None, name }) }
                    }
                    "attribute" => {
                        let name = self.text(f.child_by_field_name("attribute")?).to_string();
                        let obj = f.child_by_field_name("object")?;
                        if obj.kind() == "identifier" && starts_upper(self.text(obj)) {
                            Some(TypeExpr::Call { owner: Some(self.text(obj).to_string()), name })
                        } else {
                            Some(self.expr_type(obj)?.then(Seg::Call(name)))
                        }
                    }
                    _ => None,
                }
            }
            "await" | "parenthesized_expression" => children(e).filter(|c| c.is_named()).last().and_then(|c| self.expr_type(c)),
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
            "attribute" => {
                let Some(a) = f.child_by_field_name("attribute") else { return };
                let obj = f.child_by_field_name("object");
                let receiver = obj.map(|o| collapse(self.text(o), RECEIVER_CAP));
                let rt = obj.and_then(|o| self.expr_type(o)).map(|t| t.encode());
                let name = self.text(a).to_string();
                let kind = if starts_upper(&name) { RefKind::Instantiate } else { RefKind::Call };
                self.add(name, receiver, rt, kind, a);
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

fn descendants(node: Node) -> Vec<Node> {
    let mut out = Vec::new();
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        out.push(n);
        stack.extend(children(n));
    }
    out
}

/// The type an annotation names: `Foo`, `Optional[Foo]`, `list[Foo]`, `dict[K, V]`, `"Foo"`.
fn annotation(src: &str, t: Node) -> Option<TypeExpr> {
    let inner = if t.kind() == "type" { children(t).find(|c| c.is_named())? } else { t };
    match inner.kind() {
        "identifier" => Some(TypeExpr::Named(src[inner.byte_range()].to_string())),
        "attribute" => inner.child_by_field_name("attribute").map(|a| TypeExpr::Named(src[a.byte_range()].to_string())),
        "string" => {
            let s = src[inner.byte_range()].trim_matches(['"', '\'']);
            (!s.is_empty()).then(|| TypeExpr::Named(s.rsplit('.').next().unwrap_or(s).to_string()))
        }
        "generic_type" | "subscript" => {
            let base = children(inner).find(|c| c.is_named())?;
            let base_name = src[base.byte_range()].to_string();
            let args: Vec<TypeExpr> = children(inner)
                .skip(1)
                .flat_map(|c| if c.kind() == "type_parameter" { children(c).collect::<Vec<_>>() } else { vec![c] })
                .filter(|c| c.is_named())
                .filter_map(|c| annotation(src, c))
                .collect();
            match (base_name.as_str(), args.as_slice()) {
                ("list" | "List" | "set" | "Set" | "Sequence" | "Iterable" | "Iterator", [e]) => Some(TypeExpr::Array(Box::new(e.clone()))),
                ("dict" | "Dict" | "Mapping", [k, v]) => Some(TypeExpr::Dict(Box::new(k.clone()), Box::new(v.clone()))),
                ("Optional", [e]) => Some(e.clone()),
                _ => Some(TypeExpr::Named(base_name)),
            }
        }
        "binary_operator" => {
            // `Foo | None`
            inner.child_by_field_name("left").and_then(|l| annotation(src, l))
        }
        _ => None,
    }
}

/// First line of a function's or class's docstring.
fn docstring(src: &str, node: Node) -> Option<String> {
    let body = node.child_by_field_name("body")?;
    let first = children(body).find(|c| c.is_named())?;
    if first.kind() != "expression_statement" {
        return None;
    }
    let s = children(first).find(|c| c.kind() == "string")?;
    let text: String = children(s).filter(|c| c.kind() == "string_content").map(|c| &src[c.byte_range()]).collect();
    text.lines().map(str::trim).find(|l| !l.is_empty()).map(|l| collapse(l, 200))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_python_classes_attributes_and_calls() {
        let f = extract(
            r#"
import os
from a.b import c as d

class Store(Base):
    """The store."""
    LIMIT: int = 3
    def __init__(self, path: str):
        self.path = path
        self.conn = Conn(path)
    @property
    def size(self) -> int:
        return self.conn.count() + helper(self.path)

def main():
    s = Store("x")
    s.flush()
"#,
        );
        let sym = |name: &str, kind: SymbolKind| f.symbols.iter().find(|s| s.name == name && s.kind == kind).unwrap_or_else(|| panic!("{name}"));
        assert_eq!(sym("Store", SymbolKind::Class).doc.as_deref(), Some("The store."));
        assert_eq!(sym("init", SymbolKind::Init).container.as_deref(), Some("Store"));
        assert_eq!(sym("conn", SymbolKind::Property).type_name.as_deref(), Some("Conn"));
        assert_eq!(sym("size", SymbolKind::Property).container.as_deref(), Some("Store"));
        sym("LIMIT", SymbolKind::Property);
        sym("main", SymbolKind::Function);
        assert_eq!(f.relations[0].target, "Base");
        assert_eq!(f.imports.iter().map(|i| i.module.as_str()).collect::<Vec<_>>(), ["os", "a.b"]);
        let flush = f.refs.iter().find(|r| r.name == "flush").unwrap();
        assert_eq!(flush.receiver_type.as_deref(), Some("Store"));
        assert!(f.refs.iter().any(|r| r.name == "Store" && r.kind == RefKind::Instantiate));
        assert_eq!(f.parse_errors, 0);
    }

    #[test]
    fn base_classes_are_type_references_from_the_class() {
        let f = extract("class Store(Base, mixins.Loading):\n    pass\n");
        let refs: Vec<(&str, Option<&str>)> = f
            .refs
            .iter()
            .filter(|r| r.kind == RefKind::TypeRef)
            .map(|r| (r.name.as_str(), r.from.map(|i| f.symbols[i].name.as_str())))
            .collect();
        for name in ["Base", "Loading"] {
            assert!(refs.contains(&(name, Some("Store"))), "{name}: {refs:?}");
        }
    }

    #[test]
    fn records_what_imports_bind() {
        let f = extract("import os, a.b as ab\nimport x.y\nfrom .pkg import save as store, load\nfrom .. import mod\nfrom m import *\n");
        let b: Vec<(&str, &str, &str)> = f.bindings.iter().map(|b| (b.local.as_str(), b.name.as_str(), b.module.as_str())).collect();
        assert_eq!(
            b,
            [
                ("os", "*", "os"),
                ("ab", "*", "a.b"),
                ("x.y", "*", "x.y"),
                ("store", "save", ".pkg"),
                ("load", "load", ".pkg"),
                ("mod", "mod", ".."),
                ("*", "*", "m"),
            ]
        );
    }
}
