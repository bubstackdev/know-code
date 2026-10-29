//! Kotlin extraction over tree-sitter-kotlin-ng.
//!
//! Kotlin is read on Java's model (see `java`), so the two resolve as one JVM family: classes, interfaces, objects
//! and companion objects, enums and type aliases become symbols (an interface is a protocol, an `object` a class
//! tagged `object`), and so do functions, properties (a primary constructor's `val`s and `var`s included),
//! secondary constructors (`init`) and enum entries (cases). An extension function or property keeps its receiver
//! type in its signature (`fun String.shout()`), which is how resolution tells it from a member. The file's
//! `package` is its types' namespace (`TypeDecl::ns`) and every `import` is an [`Import`]: `a.b.C`, `a.b.*`, or
//! `a.b.C as D` for an alias. References keep their receivers as written, typed where the code says: a parameter's or
//! local's declared type, and a path of fields and calls the query follows through declarations.

use std::collections::{HashMap, HashSet};

use tree_sitter::{Node, Parser};

use crate::swift::{children, collapse, line, name_col};
use crate::types::{PathRoot, Seg, TypeDecl, TypeExpr};
use crate::{FileFacts, Import, Ref, RefKind, Relation, RelationKind, Symbol, SymbolKind};

pub const VERSION: u32 = 1;

const SIGNATURE_CAP: usize = 240;
const RECEIVER_CAP: usize = 48;

const BRANCH_KINDS: &[&str] = &["if_expression", "when_entry", "for_statement", "while_statement", "do_while_statement", "catch_block"];
const NEST_KINDS: &[&str] = &["if_expression", "when_expression", "for_statement", "while_statement", "do_while_statement", "try_expression"];

/// Kotlin's own types and the platform's, whose uses say nothing about the code base.
const COMMON_TYPES: &[&str] = &[
    "Any", "Unit", "Nothing", "String", "Int", "Long", "Short", "Byte", "Double", "Float", "Boolean", "Char", "Number",
    "Array", "List", "MutableList", "Map", "MutableMap", "Set", "MutableSet", "Collection", "Iterable", "Sequence",
    "Pair", "Triple", "Comparable", "Throwable", "Exception", "Suppress", "JvmStatic", "JvmField", "JvmName",
    "JvmOverloads", "Deprecated",
];

pub fn extract(source: &str) -> FileFacts {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_kotlin_ng::LANGUAGE.into()).expect("tree-sitter-kotlin-ng ABI matches tree-sitter");
    let Some(tree) = parser.parse(source, None) else {
        return FileFacts { parse_errors: 1, ..Default::default() };
    };
    let mut nested = HashMap::new();
    member_types(source, tree.root_node(), None, &mut nested);
    let mut cx = Cx { src: source, facts: FileFacts::default(), package: None, scopes: Vec::new(), seen: HashSet::new(), nested };
    cx.visit(tree.root_node());
    cx.facts
}

struct Scope {
    sym: usize,
    type_name: Option<String>,
    callable: bool,
    locals: HashMap<String, Option<String>>,
}

struct Cx<'a> {
    src: &'a str,
    facts: FileFacts,
    package: Option<String>,
    scopes: Vec<Scope>,
    seen: HashSet<(Option<usize>, String, RefKind, u32, u32)>,
    /// The file's member types: per type (dotted, within the package), the simple names declared in it.
    nested: HashMap<String, HashSet<String>>,
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

    fn local(&self, name: &str) -> Option<Option<String>> {
        for s in self.scopes.iter().rev() {
            if let Some(t) = s.locals.get(name) {
                return Some(t.clone());
            }
            if s.type_name.is_some() {
                return None;
            }
        }
        None
    }

    fn declare_local(&mut self, name: &str, ty: Option<String>) {
        if let Some(s) = self.scopes.iter_mut().rev().find(|s| s.callable) {
            s.locals.insert(name.to_string(), ty);
        }
    }

    fn visit(&mut self, node: Node) {
        if node.is_error() || node.is_missing() {
            self.facts.parse_errors += 1;
        }
        self.count_shape(node);
        match node.kind() {
            "package_header" => {
                if let Some(q) = children(node).find(|c| c.kind() == "qualified_identifier" || c.kind() == "identifier") {
                    self.package = Some(self.text(q).split_whitespace().collect());
                }
            }
            "import" => self.import(node),
            "class_declaration" | "object_declaration" | "companion_object" => self.type_decl(node),
            "type_alias" => self.type_alias(node),
            "function_declaration" => self.function(node),
            "secondary_constructor" => self.secondary_constructor(node),
            "property_declaration" => self.property(node),
            "enum_entry" => self.enum_entry(node),
            "anonymous_initializer" => self.with_callable(None, |cx| cx.visit_children(node)),
            "parameter" | "class_parameter" | "lambda_parameter" | "variable_declaration" if self.in_callable() => {
                // A local's or parameter's name and declared type; its type is a reference too.
                let ty = children(node).find(|c| is_type_node(c.kind())).and_then(|t| self.declared_type(t));
                if let Some(n) = children(node).find(|c| c.kind() == "identifier") {
                    let name = self.text(n).to_string();
                    self.declare_local(&name, ty);
                }
                for c in children(node).filter(|c| c.kind() != "identifier") {
                    self.visit(c);
                }
            }
            "lambda_literal" => {
                // `it`, and a lambda's own parameters, are locals.
                self.declare_local("it", None);
                self.visit_children(node);
            }
            "for_statement" | "catch_block" => self.visit_children(node),
            "call_expression" => self.call(node),
            "navigation_expression" => self.navigation(node),
            "callable_reference" => self.callable_reference(node),
            "annotation" => {
                if let Some(t) = descendant_of_kind(node, "user_type") {
                    self.type_use(t, RefKind::TypeRef, None);
                }
                if let Some(args) = descendant_of_kind(node, "value_arguments") {
                    self.visit(args);
                }
            }
            "user_type" => self.type_use(node, RefKind::TypeRef, None),
            "identifier" => self.bare_identifier(node),
            _ => self.visit_children(node),
        }
    }

    fn visit_children(&mut self, node: Node) {
        for c in children(node) {
            self.visit(c);
        }
    }

    fn in_callable(&self) -> bool {
        self.scopes.last().is_some_and(|s| s.callable)
    }

    fn with_callable(&mut self, sym: Option<usize>, f: impl FnOnce(&mut Self)) {
        match sym.or_else(|| self.current()) {
            Some(sym) => {
                self.scopes.push(Scope { sym, type_name: None, callable: true, locals: HashMap::new() });
                f(self);
                self.scopes.pop();
            }
            None => f(self),
        }
    }

    fn count_shape(&mut self, node: Node) {
        let Some(sym) = self.scopes.iter().rev().find(|s| s.callable).map(|s| s.sym) else { return };
        if BRANCH_KINDS.contains(&node.kind())
            || (matches!(node.kind(), "conjunction_expression" | "disjunction_expression"))
        {
            self.facts.symbols[sym].complexity += 1;
        }
        if NEST_KINDS.contains(&node.kind()) {
            let s = &mut self.facts.symbols[sym];
            let depth = crate::nesting_depth(node, NEST_KINDS, (s.start_byte, s.end_byte));
            s.nesting = s.nesting.max(depth);
        }
    }

    fn import(&mut self, node: Node) {
        let Some(q) = children(node).find(|c| c.kind() == "qualified_identifier" || c.kind() == "identifier") else { return };
        let mut module: String = self.text(q).split_whitespace().collect();
        let text = self.text(node);
        if text.trim_end().ends_with('*') {
            module.push_str(".*");
        } else if let Some(alias) = children(node).filter(|c| c.kind() == "identifier").last() {
            module.push_str(" as ");
            module.push_str(self.text(alias));
        }
        self.facts.imports.push(Import { module, line: line(node) });
    }

    fn push(&mut self, node: Node, name: String, kind: SymbolKind, header_stop: Option<Node>) -> usize {
        let end = children(node)
            .find(|c| matches!(c.kind(), "class_body" | "enum_class_body" | "function_body" | "block"))
            .or(header_stop)
            .map_or(node.end_byte(), |b| b.start_byte());
        let signature = collapse(self.src[node.start_byte()..end].trim_end_matches(['=', ' ', '{']), SIGNATURE_CAP);
        let attrs = annotations(self.src, node);
        let mut tags = Vec::new();
        if kind == SymbolKind::Method && attrs.iter().any(|a| matches!(a.as_str(), "Test" | "ParameterizedTest" | "RepeatedTest")) {
            tags.push("test");
        }
        if kind == SymbolKind::Function && name == "main" {
            tags.push("entrypoint");
        }
        self.facts.symbols.push(Symbol {
            parent: self.current(),
            name,
            kind,
            container: self.enclosing_type(),
            signature,
            doc: doc_comment(self.src, node),
            attrs,
            tags,
            start_line: line(node),
            end_line: node.end_position().row as u32 + 1,
            start_byte: node.start_byte() as u32,
            end_byte: node.end_byte() as u32,
            complexity: if matches!(kind, SymbolKind::Method | SymbolKind::Function | SymbolKind::Init) { 1 } else { 0 },
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
        let name = match node.child_by_field_name("name") {
            Some(n) => self.text(n).to_string(),
            None if node.kind() == "companion_object" => "Companion".to_string(),
            None => return self.visit_children(node),
        };
        let keyword_interface = children(node).any(|c| c.kind() == "interface");
        let modifiers = children(node).find(|c| c.kind() == "modifiers").map(|m| self.text(m)).unwrap_or("");
        let kind = if keyword_interface || modifiers.split_whitespace().any(|m| m == "annotation") {
            SymbolKind::Protocol
        } else if modifiers.split_whitespace().any(|m| m == "enum") {
            SymbolKind::Enum
        } else {
            SymbolKind::Class
        };
        let sym = self.push(node, name.clone(), kind, None);
        match node.kind() {
            "object_declaration" => self.facts.symbols[sym].tags.push("object"),
            "companion_object" => self.facts.symbols[sym].tags.push("companion"),
            _ => {}
        }
        let params = children(node)
            .find(|c| c.kind() == "type_parameters")
            .map(|tp| {
                children(tp)
                    .filter(|c| c.kind() == "type_parameter")
                    .filter_map(|c| children(c).find(|n| n.kind() == "identifier"))
                    .map(|n| self.text(n).to_string())
                    .collect()
            })
            .unwrap_or_default();
        self.facts.symbols[sym].type_name = TypeDecl { ns: self.package.clone(), params, ..Default::default() }.encode();
        let full = self.enclosing_type().map_or_else(|| name.clone(), |o| format!("{o}.{name}"));
        if let Some(list) = children(node).find(|c| c.kind() == "delegation_specifiers") {
            for spec in children(list).filter(|c| c.kind() == "delegation_specifier") {
                let t = children(spec)
                    .find(|c| c.kind() == "constructor_invocation")
                    .and_then(|ci| children(ci).find(|c| c.kind() == "user_type"))
                    .or_else(|| children(spec).find(|c| c.kind() == "user_type"))
                    .or_else(|| descendant_of_kind(spec, "user_type"));
                if let Some(t) = t {
                    if let Some(target) = base_name(self.src, t) {
                        self.facts.relations.push(Relation { from: sym, target, kind: RelationKind::Inherits, line: line(t) });
                    }
                    self.type_use(t, RefKind::TypeRef, None);
                }
                // Arguments to the superclass constructor are code in the class.
                if let Some(args) = descendant_of_kind(spec, "value_arguments") {
                    self.scopes.push(Scope { sym, type_name: Some(full.clone()), callable: false, locals: HashMap::new() });
                    self.with_callable(Some(sym), |cx| cx.visit(args));
                    self.scopes.pop();
                }
            }
        }
        self.scopes.push(Scope { sym, type_name: Some(full), callable: false, locals: HashMap::new() });
        for c in children(node).filter(|c| matches!(c.kind(), "modifiers" | "type_parameters" | "type_constraints")) {
            self.visit(c);
        }
        if let Some(pc) = children(node).find(|c| c.kind() == "primary_constructor") {
            self.primary_constructor(pc);
        }
        if let Some(body) = children(node).find(|c| matches!(c.kind(), "class_body" | "enum_class_body")) {
            self.visit(body);
        }
        self.scopes.pop();
    }

    /// A primary constructor's `val`/`var` parameters are the class's properties; the rest are its parameters.
    fn primary_constructor(&mut self, pc: Node) {
        let Some(list) = children(pc).find(|c| c.kind() == "class_parameters") else { return };
        let params: Vec<Node> = children(list).filter(|c| c.kind() == "class_parameter").collect();
        let init = self.push(pc, "init".to_string(), SymbolKind::Init, Some(list));
        let defaults = params.iter().filter(|p| children(**p).any(|c| is_expression(c.kind()))).count() as u32;
        self.facts.symbols[init].arity = Some((params.len() as u32 - defaults, Some(params.len() as u32)));
        for p in &params {
            let holds = self.text(*p).split_whitespace().any(|w| w == "val" || w == "var");
            let ty_node = children(*p).find(|c| is_type_node(c.kind()));
            let ty = ty_node.and_then(|t| self.declared_type(t));
            if holds && let Some(n) = children(*p).find(|c| c.kind() == "identifier") {
                let name = self.text(n).to_string();
                let sym = self.push(*p, name, SymbolKind::Property, ty_node);
                self.facts.symbols[sym].type_name = ty.clone();
            }
            if let Some(t) = ty_node {
                self.visit(t);
            }
            for c in children(*p).filter(|c| c.kind() == "modifiers" || is_expression(c.kind())) {
                self.with_callable(Some(init), |cx| cx.visit(c));
            }
        }
    }

    fn type_alias(&mut self, node: Node) {
        let Some(n) = node.child_by_field_name("type") else { return };
        let name = self.text(n).to_string();
        let sym = self.push(node, name, SymbolKind::Typealias, None);
        if let Some(t) = children(node).filter(|c| is_type_node(c.kind())).last() {
            self.facts.symbols[sym].type_name = self.declared_type(t);
            self.visit(t);
        }
    }

    fn function(&mut self, node: Node) {
        let Some(n) = node.child_by_field_name("name") else { return self.visit_children(node) };
        let name = self.text(n).to_string();
        let in_type = self.scopes.last().is_some_and(|s| s.type_name.is_some());
        let kind = if in_type { SymbolKind::Method } else { SymbolKind::Function };
        let sym = self.push(node, name, kind, None);
        let params = children(node).find(|c| c.kind() == "function_value_parameters");
        if let Some(p) = params {
            let all: Vec<Node> = children(p).filter(|c| c.kind() == "parameter").collect();
            let vararg = self.text(p).contains("vararg");
            // A parameter with a default may be left out.
            let defaults = children(p).filter(|c| is_expression(c.kind())).count() as u32;
            let n = all.len() as u32;
            self.facts.symbols[sym].arity = Some((n.saturating_sub(defaults), (!vararg).then_some(n)));
        }
        // The return type follows the parameters; a type before the name is an extension's receiver.
        let ret = params.and_then(|p| {
            let mut after = false;
            children(node).find(|c| {
                if *c == p {
                    after = true;
                    return false;
                }
                after && is_type_node(c.kind())
            })
        });
        if let Some(t) = ret {
            self.facts.symbols[sym].type_name = self.declared_type(t);
        }
        self.with_callable(Some(sym), |cx| {
            for c in children(node) {
                if Some(c) != node.child_by_field_name("name") {
                    cx.visit(c);
                }
            }
        });
    }

    fn secondary_constructor(&mut self, node: Node) {
        let sym = self.push(node, "init".to_string(), SymbolKind::Init, None);
        if let Some(p) = children(node).find(|c| c.kind() == "function_value_parameters") {
            let n = children(p).filter(|c| c.kind() == "parameter").count() as u32;
            self.facts.symbols[sym].arity = Some((n, Some(n)));
        }
        self.with_callable(Some(sym), |cx| cx.visit_children(node));
    }

    fn property(&mut self, node: Node) {
        if self.in_callable() {
            // A local `val`/`var`: its name and declared (or constructed) type.
            if let Some(v) = children(node).find(|c| c.kind() == "variable_declaration") {
                let ty = children(v)
                    .find(|c| is_type_node(c.kind()))
                    .and_then(|t| self.declared_type(t))
                    .or_else(|| children(node).find(|c| c.kind() == "call_expression").and_then(|e| self.constructed(e)));
                if let Some(n) = children(v).find(|c| c.kind() == "identifier") {
                    let name = self.text(n).to_string();
                    self.declare_local(&name, ty);
                }
            }
            for c in children(node).filter(|c| c.kind() != "variable_declaration") {
                self.visit(c);
            }
            if let Some(v) = children(node).find(|c| c.kind() == "variable_declaration") {
                for c in children(v).filter(|c| is_type_node(c.kind())) {
                    self.visit(c);
                }
            }
            return;
        }
        let Some(v) = children(node).find(|c| c.kind() == "variable_declaration") else { return self.visit_children(node) };
        let Some(n) = children(v).find(|c| c.kind() == "identifier") else { return };
        let name = self.text(n).to_string();
        let in_type = self.scopes.last().is_some_and(|s| s.type_name.is_some());
        let sym = self.push(node, name, SymbolKind::Property, None);
        let _ = in_type;
        let ty = children(v).find(|c| is_type_node(c.kind())).and_then(|t| self.declared_type(t));
        self.facts.symbols[sym].type_name = ty;
        self.with_callable(Some(sym), |cx| {
            for c in children(node) {
                if c != v {
                    cx.visit(c);
                }
            }
            for c in children(v).filter(|c| is_type_node(c.kind())) {
                cx.visit(c);
            }
        });
    }

    fn enum_entry(&mut self, node: Node) {
        let Some(n) = children(node).find(|c| c.kind() == "identifier") else { return };
        let name = self.text(n).to_string();
        let sym = self.push(node, name, SymbolKind::Case, children(node).find(|c| c.kind() == "value_arguments"));
        self.facts.symbols[sym].type_name = self.enclosing_type();
        self.with_callable(Some(sym), |cx| {
            for c in children(node).filter(|c| c.kind() != "identifier") {
                cx.visit(c);
            }
        });
    }

    fn call(&mut self, node: Node) {
        let Some(callee) = children(node).find(|c| c.is_named()) else { return };
        let args = children(node).find(|c| c.kind() == "value_arguments").map(arg_count);
        let lambda = children(node).any(|c| c.kind() == "annotated_lambda");
        let args = args.map(|a| a + u32::from(lambda)).or(lambda.then_some(1));
        match callee.kind() {
            "identifier" => {
                let name = self.text(callee).to_string();
                if self.local(&name).is_none() {
                    // `Cart(…)`: a constructor call names its type.
                    let kind = if type_like(&name) { RefKind::Instantiate } else { RefKind::Call };
                    self.record(callee, name, None, None, kind, args);
                }
            }
            "navigation_expression" => {
                let parts: Vec<Node> = children(callee).filter(|c| c.is_named()).collect();
                if let (Some(object), Some(last)) = (parts.first(), parts.last())
                    && last.kind() == "identifier"
                    && parts.len() >= 2
                {
                    let name = self.text(*last).to_string();
                    let (receiver, receiver_type) = self.receiver_of(*object);
                    let kind = if type_like(&name) && is_qualifier(self.text(*object)) { RefKind::Instantiate } else { RefKind::Call };
                    self.record(*last, name, receiver, receiver_type, kind, args);
                    self.visit(*object);
                }
            }
            _ => self.visit(callee),
        }
        for c in children(node).filter(|c| c.kind() != callee.kind() || *c != callee) {
            if c != callee {
                self.visit(c);
            }
        }
    }

    fn navigation(&mut self, node: Node) {
        let parts: Vec<Node> = children(node).filter(|c| c.is_named()).collect();
        let (Some(object), Some(last)) = (parts.first(), parts.last()) else { return };
        if parts.len() < 2 || last.kind() != "identifier" {
            return self.visit_children(node);
        }
        let name = self.text(*last).to_string();
        let (receiver, receiver_type) = self.receiver_of(*object);
        // `Item::price`, a member reference, is read as navigation with `::`: it names a function to call.
        let reference = self.src[object.end_byte()..last.start_byte()].contains("::");
        let kind = if reference { RefKind::Call } else { RefKind::Member };
        self.record(*last, name, receiver, receiver_type, kind, None);
        self.visit(*object);
    }

    fn callable_reference(&mut self, node: Node) {
        // `Type::member`, `x::member`, `::function`, `::Type` (a constructor).
        let name_node = children(node).filter(|c| c.kind() == "identifier").last();
        let target = children(node).find(|c| c.is_named() && Some(*c) != name_node);
        let Some(n) = name_node else { return self.visit_children(node) };
        let name = self.text(n).to_string();
        match target {
            Some(t) => {
                let (receiver, receiver_type) = self.receiver_of(t);
                self.record(n, name, receiver, receiver_type, RefKind::Call, None);
                self.visit(t);
            }
            None if type_like(&name) => self.record(n, name, None, None, RefKind::Instantiate, None),
            None => self.record(n, name, None, None, RefKind::Call, None),
        }
    }

    fn bare_identifier(&mut self, node: Node) {
        let Some(parent) = node.parent() else { return };
        if !matches!(
            parent.kind(),
            "statement" | "value_argument" | "binary_expression" | "comparison_expression" | "equality_expression"
                | "conjunction_expression" | "disjunction_expression" | "additive_expression" | "multiplicative_expression"
                | "return_expression" | "assignment" | "if_expression" | "when_subject" | "when_condition"
                | "string_template" | "interpolated_expression" | "parenthesized_expression" | "prefix_expression"
                | "postfix_expression" | "function_body" | "property_declaration" | "elvis_expression"
                | "range_expression" | "index_expression" | "as_expression" | "check_expression" | "spread_expression"
                | "statements" | "block" | "control_structure_body" | "jump_expression" | "in_expression"
        ) {
            return;
        }
        let name = self.text(node).to_string();
        if self.local(&name).is_some() || matches!(name.as_str(), "it" | "field") {
            return;
        }
        self.record(node, name, None, None, RefKind::Member, None);
    }

    fn receiver_of(&self, object: Node) -> (Option<String>, Option<String>) {
        let text = collapse(self.text(object), RECEIVER_CAP);
        let ty = match object.kind() {
            "identifier" => self.local(self.text(object)).flatten(),
            "this_expression" => self.enclosing_type(),
            _ => None,
        };
        (Some(text), ty.or_else(|| self.expr_type(object).map(|t| t.encode())))
    }

    fn expr_type(&self, e: Node) -> Option<TypeExpr> {
        match e.kind() {
            "this_expression" => Some(TypeExpr::root(PathRoot::SelfType)),
            "identifier" => {
                let name = self.text(e);
                match self.local(name) {
                    Some(t) => t.and_then(|t| TypeExpr::decode(&t)),
                    None if type_like(name) => None,
                    None => Some(TypeExpr::root(PathRoot::Prop(name.to_string()))),
                }
            }
            "parenthesized_expression" => children(e).find(|c| c.is_named()).and_then(|c| self.expr_type(c)),
            "as_expression" => children(e).filter(|c| is_type_node(c.kind())).last().and_then(|t| type_expr(self.src, t)),
            "navigation_expression" => {
                let parts: Vec<Node> = children(e).filter(|c| c.is_named()).collect();
                let (object, last) = (*parts.first()?, *parts.last()?);
                if parts.len() < 2 {
                    return None;
                }
                self.member_of(object, Seg::Member(self.text(last).to_string()))
            }
            "call_expression" => {
                if let Some(t) = self.constructed(e) {
                    return TypeExpr::decode(&t);
                }
                let callee = children(e).find(|c| c.is_named())?;
                match callee.kind() {
                    "identifier" => Some(TypeExpr::Call { owner: None, name: self.text(callee).to_string() }),
                    "navigation_expression" => {
                        let parts: Vec<Node> = children(callee).filter(|c| c.is_named()).collect();
                        let (object, last) = (*parts.first()?, *parts.last()?);
                        (parts.len() >= 2).then_some(())?;
                        self.member_of(object, Seg::Call(self.text(last).to_string()))
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn member_of(&self, object: Node, seg: Seg) -> Option<TypeExpr> {
        match self.expr_type(object) {
            Some(t) => Some(t.then(seg)),
            None if object.kind() == "identifier" && type_like(self.text(object)) && self.local(self.text(object)).is_none() => {
                Some(TypeExpr::root(PathRoot::Static(self.text(object).to_string())).then(seg))
            }
            None => None,
        }
    }

    /// `Cart(…)` (a capitalised call): the type it constructs.
    fn constructed(&self, call: Node) -> Option<String> {
        let callee = children(call).find(|c| c.is_named())?;
        (callee.kind() == "identifier" && type_like(self.text(callee)) && self.local(self.text(callee)).is_none())
            .then(|| self.qualify(TypeExpr::Named(self.text(callee).to_string())).encode())
    }

    fn type_use(&mut self, node: Node, kind: RefKind, args: Option<u32>) {
        match node.kind() {
            "user_type" => {
                let parts: Vec<Node> = children(node).filter(|c| c.kind() == "identifier").collect();
                if let Some(last) = parts.last() {
                    let name = self.text(*last).to_string();
                    let receiver = (parts.len() > 1).then(|| {
                        let q: Vec<&str> = parts[..parts.len() - 1].iter().map(|p| self.text(*p)).collect();
                        collapse(&q.join("."), RECEIVER_CAP)
                    });
                    self.record(*last, name, receiver, None, kind, args);
                }
                for c in children(node).filter(|c| c.kind() == "type_arguments") {
                    self.visit(c);
                }
            }
            _ => self.visit_children(node),
        }
    }

    fn record(&mut self, at: Node, name: String, receiver: Option<String>, receiver_type: Option<String>, kind: RefKind, args: Option<u32>) {
        if kind == RefKind::TypeRef && receiver.is_none() && COMMON_TYPES.contains(&name.as_str()) {
            return;
        }
        let ln = line(at);
        let col = name_col(self.src, at, &name);
        let from = self.current();
        if !self.seen.insert((from, name.clone(), kind, ln, col)) {
            return;
        }
        self.facts.refs.push(Ref { from, name, receiver, receiver_type, kind, line: ln, col, args });
    }

    fn declared_type(&self, t: Node) -> Option<String> {
        type_expr(self.src, t).map(|e| self.qualify(e).encode())
    }

    /// A simple type name inside a type that declares a member type of that name means the member type.
    fn qualify(&self, e: TypeExpr) -> TypeExpr {
        let inner = |n: String| -> String {
            if n.contains('.') {
                return n;
            }
            let Some(t) = self.enclosing_type() else { return n };
            let parts: Vec<&str> = t.split('.').collect();
            for k in (1..=parts.len()).rev() {
                let scope = parts[..k].join(".");
                if self.nested.get(&scope).is_some_and(|names| names.contains(&n)) {
                    return format!("{scope}.{n}");
                }
            }
            n
        };
        match e {
            TypeExpr::Named(n) => TypeExpr::Named(inner(n)),
            TypeExpr::Generic { name, args } => TypeExpr::Generic { name: inner(name), args },
            TypeExpr::Array(e) => TypeExpr::Array(Box::new(self.qualify(*e))),
            other => other,
        }
    }
}

fn is_type_node(kind: &str) -> bool {
    matches!(kind, "user_type" | "nullable_type" | "function_type" | "parenthesized_type" | "non_nullable_type")
}

fn is_expression(kind: &str) -> bool {
    !matches!(kind, "identifier" | "modifiers" | "user_type" | "nullable_type" | "function_type" | "parenthesized_type" | ":" | "=" | "," | "val" | "var" | "vararg")
        && !kind.is_empty()
        && kind.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && !matches!(kind, "parameter_modifiers" | "type_modifiers" | "line_comment" | "block_comment")
}

fn descendant_of_kind<'t>(node: Node<'t>, kind: &str) -> Option<Node<'t>> {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if n.kind() == kind && n != node {
            return Some(n);
        }
        stack.extend(children(n).collect::<Vec<_>>().into_iter().rev());
    }
    None
}

fn type_expr(src: &str, t: Node) -> Option<TypeExpr> {
    match t.kind() {
        "nullable_type" | "non_nullable_type" | "parenthesized_type" => children(t).find(|c| is_type_node(c.kind())).and_then(|c| type_expr(src, c)),
        "user_type" => {
            let parts: Vec<&str> = children(t).filter(|c| c.kind() == "identifier").map(|c| &src[c.byte_range()]).collect();
            let name = parts.join(".");
            let args: Vec<Option<TypeExpr>> = children(t)
                .find(|c| c.kind() == "type_arguments")
                .map(|a| {
                    children(a)
                        .filter(|c| c.kind() == "type_projection")
                        .map(|p| children(p).find(|c| is_type_node(c.kind())).and_then(|c| type_expr(src, c)))
                        .collect()
                })
                .unwrap_or_default();
            if name.is_empty() {
                None
            } else if args.is_empty() {
                Some(TypeExpr::Named(name))
            } else {
                Some(TypeExpr::Generic { name, args })
            }
        }
        _ => None,
    }
}

fn base_name(src: &str, n: Node) -> Option<String> {
    let parts: Vec<&str> = children(n).filter(|c| c.kind() == "identifier").map(|c| &src[c.byte_range()]).collect();
    (!parts.is_empty()).then(|| parts.join("."))
}

fn type_like(name: &str) -> bool {
    name.chars().next().is_some_and(char::is_uppercase) && name.chars().any(char::is_lowercase)
}

/// A dotted qualifier that names a package or types (`a.b`, `Outer`), not an expression.
fn is_qualifier(s: &str) -> bool {
    s.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.')
}

fn arg_count(list: Node) -> u32 {
    children(list).filter(|c| c.kind() == "value_argument").count() as u32
}

fn annotations(src: &str, node: Node) -> Vec<String> {
    let Some(mods) = children(node).find(|c| c.kind() == "modifiers") else { return Vec::new() };
    children(mods)
        .filter(|c| c.kind() == "annotation")
        .filter_map(|a| descendant_of_kind(a, "user_type"))
        .filter_map(|t| children(t).filter(|c| c.kind() == "identifier").last().map(|n| src[n.byte_range()].to_string()))
        .collect()
}

fn doc_comment(src: &str, node: Node) -> Option<String> {
    let prev = node.prev_sibling().filter(|p| p.kind() == "block_comment")?;
    let text = &src[prev.byte_range()];
    if !text.starts_with("/**") {
        return None;
    }
    text.trim_start_matches("/**")
        .trim_end_matches("*/")
        .lines()
        .map(|l| l.trim().trim_start_matches('*').trim())
        .find(|l| !l.is_empty() && !l.starts_with('@'))
        .map(|l| collapse(l, 160))
}

/// The member types `node`'s declarations declare, by the dotted name of the declaring type.
fn member_types(src: &str, node: Node, outer: Option<&str>, out: &mut HashMap<String, HashSet<String>>) {
    for c in children(node) {
        let is_type = matches!(c.kind(), "class_declaration" | "object_declaration" | "companion_object");
        let name = c.child_by_field_name("name").map(|n| src[n.byte_range()].to_string())
            .or_else(|| (c.kind() == "companion_object").then(|| "Companion".to_string()));
        match (is_type, name) {
            (true, Some(name)) => {
                if let Some(o) = outer {
                    out.entry(o.to_string()).or_default().insert(name.clone());
                }
                let full = outer.map_or_else(|| name.clone(), |o| format!("{o}.{name}"));
                if let Some(body) = children(c).find(|b| matches!(b.kind(), "class_body" | "enum_class_body")) {
                    member_types(src, body, Some(&full), out);
                }
            }
            _ if !matches!(c.kind(), "function_declaration" | "function_body" | "lambda_literal") => member_types(src, c, outer, out),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refs(f: &FileFacts, kind: RefKind) -> Vec<(String, Option<String>)> {
        f.refs.iter().filter(|r| r.kind == kind).map(|r| (r.name.clone(), r.receiver.clone())).collect()
    }

    #[test]
    fn declarations_package_and_imports() {
        let f = extract(
            "package shop.cart\n\
             import shop.model.Item as It\n\
             import shop.util.*\n\
             class Cart(private val owner: String, items: List<It>) : Base(), Sized {\n\
               val size: Int get() = 1\n\
               companion object {\n\
                 const val MAX = 3\n\
                 fun of(): Cart = Cart(\"x\", listOf())\n\
               }\n\
               fun add(item: It): Cart { store.save(item); return this }\n\
             }\n\
             fun String.shout(): String = uppercase()\n\
             val Cart.big: Boolean get() = size > 1\n\
             object Registry {\n\
               fun find(id: Int) = Cart.of()\n\
             }\n\
             interface Shape {\n\
               fun area(): Double\n\
             }\n\
             enum class State { OPEN, CLOSED }\n\
             typealias Carts = List<Cart>\n",
        );
        let syms: Vec<_> = f.symbols.iter().map(|s| (s.name.as_str(), s.kind, s.container.as_deref())).collect();
        for want in [
            ("Cart", SymbolKind::Class, None),
            ("owner", SymbolKind::Property, Some("Cart")),
            ("size", SymbolKind::Property, Some("Cart")),
            ("Companion", SymbolKind::Class, Some("Cart")),
            ("MAX", SymbolKind::Property, Some("Cart.Companion")),
            ("of", SymbolKind::Method, Some("Cart.Companion")),
            ("add", SymbolKind::Method, Some("Cart")),
            ("shout", SymbolKind::Function, None),
            ("big", SymbolKind::Property, None),
            ("Registry", SymbolKind::Class, None),
            ("find", SymbolKind::Method, Some("Registry")),
            ("Shape", SymbolKind::Protocol, None),
            ("area", SymbolKind::Method, Some("Shape")),
            ("State", SymbolKind::Enum, None),
            ("OPEN", SymbolKind::Case, Some("State")),
            ("Carts", SymbolKind::Typealias, None),
        ] {
            assert!(syms.contains(&want), "{want:?} in {syms:?}");
        }
        assert!(!syms.iter().any(|(n, _, _)| *n == "items"), "a plain constructor parameter isn't a property");
        assert_eq!(TypeDecl::decode(f.symbols.iter().find(|s| s.name == "Cart").unwrap().type_name.as_deref()).ns.as_deref(), Some("shop.cart"));
        let imports: Vec<_> = f.imports.iter().map(|i| i.module.as_str()).collect();
        assert_eq!(imports, vec!["shop.model.Item as It", "shop.util.*"]);
        let supers: Vec<_> = f.relations.iter().map(|r| r.target.as_str()).collect();
        assert_eq!(supers, vec!["Base", "Sized"]);
        assert!(f.symbols.iter().find(|s| s.name == "shout").unwrap().signature.contains("String.shout"));
        assert_eq!(f.parse_errors, 0);
    }

    #[test]
    fn calls_constructors_members_and_receivers() {
        let f = extract(
            "class A {\n\
               val store: Store = Store()\n\
               fun run(order: Order) {\n\
                 val cart = Cart(1)\n\
                 cart.add(order.total())\n\
                 store.save(cart)\n\
                 Helpers.log(\"x\")\n\
                 xs.forEach { it.go() }\n\
                 val r = Item::price\n\
                 count += 1\n\
               }\n\
             }\n",
        );
        let calls = refs(&f, RefKind::Call);
        assert!(calls.contains(&("add".into(), Some("cart".into()))));
        assert!(calls.contains(&("total".into(), Some("order".into()))));
        assert!(calls.contains(&("save".into(), Some("store".into()))));
        assert!(calls.contains(&("log".into(), Some("Helpers".into()))));
        assert!(calls.contains(&("price".into(), Some("Item".into()))));
        assert_eq!(f.refs.iter().find(|r| r.name == "add").unwrap().receiver_type.as_deref(), Some("Cart"));
        assert_eq!(f.refs.iter().find(|r| r.name == "total").unwrap().receiver_type.as_deref(), Some("Order"));
        let made = refs(&f, RefKind::Instantiate);
        assert!(made.contains(&("Cart".into(), None)));
        assert!(made.contains(&("Store".into(), None)));
        let members = refs(&f, RefKind::Member);
        assert!(members.contains(&("count".into(), None)));
        assert!(!members.iter().any(|(n, _)| n == "cart" || n == "it"), "locals aren't members");
    }
}
