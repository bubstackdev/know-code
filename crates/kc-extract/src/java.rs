//! Java extraction over tree-sitter-java.
//!
//! Types (classes, interfaces, enums, records, annotation types) and their members become symbols; an interface or
//! an annotation type is a protocol, a constructor is an `init`, a field, a record component or an enum constant's
//! value is a property, and an enum constant is a case. The file's package is each type's namespace (`TypeDecl::ns`),
//! as a C# namespace is, and every `import` is an [`Import`]: `a.b.C`, `a.b.*`, `static a.b.C.m` or `static a.b.C.*`.
//! References keep their receiver as written; a parameter's or local's declared type becomes the receiver's type.
//! Resolution against packages and imports happens at query time (kc-store's `jvm`).

use std::collections::{HashMap, HashSet};

use tree_sitter::{Node, Parser};

use crate::swift::{children, collapse, line, name_col};
use crate::types::{TypeDecl, TypeExpr};
use crate::{FileFacts, Import, Ref, RefKind, Relation, RelationKind, Symbol, SymbolKind};

pub const VERSION: u32 = 1;

const SIGNATURE_CAP: usize = 240;
const RECEIVER_CAP: usize = 48;

/// Branches, for [`Symbol::complexity`].
const BRANCH_KINDS: &[&str] = &[
    "if_statement", "for_statement", "enhanced_for_statement", "while_statement", "do_statement", "catch_clause",
    "switch_label", "ternary_expression",
];

/// Control-flow blocks that nest, for [`Symbol::nesting`].
const NEST_KINDS: &[&str] = &[
    "if_statement", "for_statement", "enhanced_for_statement", "while_statement", "do_statement", "switch_expression",
    "try_statement", "try_with_resources_statement",
];

/// `java.lang` types and primitives' boxes whose uses say nothing about the code base: they are never the repo's.
const COMMON_TYPES: &[&str] = &[
    "Object", "String", "Integer", "Long", "Short", "Byte", "Double", "Float", "Boolean", "Character", "Void",
    "Number", "CharSequence", "Iterable", "Comparable", "Runnable", "Override", "Deprecated", "SuppressWarnings",
    "FunctionalInterface", "SafeVarargs",
];

pub fn extract(source: &str) -> FileFacts {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_java::LANGUAGE.into()).expect("tree-sitter-java ABI matches tree-sitter");
    let Some(tree) = parser.parse(source, None) else {
        return FileFacts { parse_errors: 1, ..Default::default() };
    };
    let mut cx = Cx { src: source, facts: FileFacts::default(), package: None, scopes: Vec::new(), seen: HashSet::new() };
    cx.visit(tree.root_node());
    cx.facts
}

struct Scope {
    sym: usize,
    /// The dotted name of the type whose body this is (`Outer.Inner`), for a type scope.
    type_name: Option<String>,
    /// A method, constructor or initializer: its locals and parameters, with their declared types where written.
    callable: bool,
    locals: HashMap<String, Option<String>>,
}

struct Cx<'a> {
    src: &'a str,
    facts: FileFacts,
    package: Option<String>,
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

    /// A local's or parameter's declared type in the innermost callable scope that has it. `Some(None)` for a local
    /// whose type isn't written (`var`): still a local, never a field of the same name.
    fn local(&self, name: &str) -> Option<Option<String>> {
        for s in self.scopes.iter().rev() {
            if let Some(t) = s.locals.get(name) {
                return Some(t.clone());
            }
            if s.type_name.is_some() {
                // A local class's body doesn't see its enclosing method's locals as its own receivers' types; it
                // does capture them, but a member of the class shadows them, so stop at the type.
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
            "package_declaration" => {
                if let Some(n) = children(node).find(|c| matches!(c.kind(), "scoped_identifier" | "identifier")) {
                    self.package = Some(self.text(n).split_whitespace().collect());
                }
            }
            "import_declaration" => self.import(node),
            "class_declaration" | "interface_declaration" | "enum_declaration" | "record_declaration"
            | "annotation_type_declaration" => self.type_decl(node),
            "method_declaration" | "constructor_declaration" | "compact_constructor_declaration"
            | "annotation_type_element_declaration" => self.method(node),
            "field_declaration" | "constant_declaration" => self.field(node),
            "enum_constant" => self.enum_constant(node),
            "static_initializer" | "block" if node.parent().is_some_and(|p| p.kind() == "class_body") => {
                self.with_callable(None, |cx| cx.visit_children(node));
            }
            "local_variable_declaration" => {
                let ty = node.child_by_field_name("type").and_then(|t| self.declared_type(t));
                for d in children(node).filter(|c| c.kind() == "variable_declarator") {
                    if let Some(n) = d.child_by_field_name("name") {
                        let name = self.text(n).to_string();
                        self.declare_local(&name, ty.clone());
                    }
                }
                self.visit_children(node);
            }
            "formal_parameter" | "spread_parameter" | "catch_formal_parameter" | "resource" => {
                let ty = node
                    .child_by_field_name("type")
                    .or_else(|| children(node).find(|c| c.kind() == "catch_type"))
                    .and_then(|t| self.declared_type(t));
                let name = node
                    .child_by_field_name("name")
                    .or_else(|| children(node).find(|c| c.kind() == "variable_declarator").and_then(|d| d.child_by_field_name("name")));
                if let Some(n) = name {
                    let name = self.text(n).to_string();
                    self.declare_local(&name, ty);
                }
                self.visit_children(node);
            }
            "enhanced_for_statement" => {
                let ty = node.child_by_field_name("type").and_then(|t| self.declared_type(t));
                if let Some(n) = node.child_by_field_name("name") {
                    let name = self.text(n).to_string();
                    self.declare_local(&name, ty);
                }
                self.visit_children(node);
            }
            "lambda_expression" => {
                if let Some(p) = node.child_by_field_name("parameters") {
                    if p.kind() == "identifier" {
                        let name = self.text(p).to_string();
                        self.declare_local(&name, None);
                    } else {
                        for c in children(p).filter(|c| c.kind() == "identifier") {
                            let name = self.text(c).to_string();
                            self.declare_local(&name, None);
                        }
                    }
                }
                self.visit_children(node);
            }
            "instanceof_expression" => {
                // `o instanceof Order placed`: `placed` is a local of the type tested.
                if let Some(n) = node.child_by_field_name("name") {
                    let ty = node.child_by_field_name("right").and_then(|t| self.declared_type(t));
                    let name = self.text(n).to_string();
                    self.declare_local(&name, ty);
                }
                for c in children(node) {
                    if Some(c) != node.child_by_field_name("name") {
                        self.visit(c);
                    }
                }
            }
            "type_pattern" | "record_pattern_component" => {
                let ty = children(node).find(|c| c.is_named() && c.kind() != "identifier").and_then(|t| self.declared_type(t));
                if let Some(n) = children(node).filter(|c| c.kind() == "identifier").last() {
                    let name = self.text(n).to_string();
                    self.declare_local(&name, ty);
                }
                self.visit_children(node);
            }
            "method_invocation" => self.call(node),
            "object_creation_expression" => {
                if let Some(t) = node.child_by_field_name("type") {
                    let args = node.child_by_field_name("arguments").map(arg_count);
                    self.type_use(t, RefKind::Instantiate, args);
                }
                for c in children(node) {
                    if c.kind() != "type" && Some(c) != node.child_by_field_name("type") {
                        self.visit(c);
                    }
                }
            }
            "method_reference" => self.method_reference(node),
            "field_access" => self.field_access(node),
            "class_literal" => {
                if let Some(t) = children(node).find(|c| c.is_named()) {
                    self.type_use(t, RefKind::TypeRef, None);
                }
            }
            "marker_annotation" | "annotation" => {
                if let Some(n) = node.child_by_field_name("name") {
                    self.type_use(n, RefKind::TypeRef, None);
                }
                if let Some(a) = node.child_by_field_name("arguments") {
                    self.visit(a);
                }
            }
            "type_identifier" | "scoped_type_identifier" | "generic_type" => self.type_use(node, RefKind::TypeRef, None),
            "identifier" => self.bare_identifier(node),
            _ => self.visit_children(node),
        }
    }

    fn visit_children(&mut self, node: Node) {
        for c in children(node) {
            self.visit(c);
        }
    }

    fn with_callable(&mut self, sym: Option<usize>, f: impl FnOnce(&mut Self)) {
        let sym = sym.or_else(|| self.current());
        match sym {
            Some(sym) => {
                self.scopes.push(Scope { sym, type_name: None, callable: true, locals: HashMap::new() });
                f(self);
                self.scopes.pop();
            }
            None => f(self),
        }
    }

    fn count_shape(&mut self, node: Node) {
        let callable = self.scopes.iter().rev().find(|s| s.callable).map(|s| s.sym);
        let Some(sym) = callable else { return };
        if BRANCH_KINDS.contains(&node.kind())
            || (node.kind() == "binary_expression"
                && node.child_by_field_name("operator").is_some_and(|o| matches!(self.text(o), "&&" | "||")))
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
        let is_static = children(node).any(|c| c.kind() == "static");
        let Some(name) = children(node).find(|c| matches!(c.kind(), "scoped_identifier" | "identifier")) else { return };
        let mut module: String = self.text(name).split_whitespace().collect();
        if children(node).any(|c| c.kind() == "asterisk") {
            module.push_str(".*");
        }
        if is_static {
            module.insert_str(0, "static ");
        }
        self.facts.imports.push(Import { module, line: line(node) });
    }

    fn push(&mut self, node: Node, name: String, kind: SymbolKind, header_stop: Option<Node>) -> usize {
        let end = node.child_by_field_name("body").or(header_stop).map_or(node.end_byte(), |b| b.start_byte());
        let signature = collapse(self.src[node.start_byte()..end].trim_end_matches(['=', ' ', '{', ';']), SIGNATURE_CAP);
        let attrs = annotations(self.src, node);
        let mut tags = Vec::new();
        if matches!(kind, SymbolKind::Method)
            && attrs.iter().any(|a| matches!(a.as_str(), "Test" | "ParameterizedTest" | "RepeatedTest" | "TestFactory" | "TestTemplate"))
        {
            tags.push("test");
        }
        if kind == SymbolKind::Method && name == "main" && signature.contains("static") && signature.contains("void") {
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
            complexity: if matches!(kind, SymbolKind::Method | SymbolKind::Init) { 1 } else { 0 },
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
            "interface_declaration" | "annotation_type_declaration" => SymbolKind::Protocol,
            "enum_declaration" => SymbolKind::Enum,
            _ => SymbolKind::Class,
        };
        let sym = self.push(node, name.clone(), kind, None);
        if node.kind() == "record_declaration" {
            self.facts.symbols[sym].tags.push("record");
        }
        let params = node
            .child_by_field_name("type_parameters")
            .map(|tp| {
                children(tp)
                    .filter(|c| c.kind() == "type_parameter")
                    .filter_map(|c| children(c).find(|n| matches!(n.kind(), "type_identifier" | "identifier")))
                    .map(|n| self.text(n).to_string())
                    .collect()
            })
            .unwrap_or_default();
        let decl = TypeDecl { ns: self.package.clone(), params, ..Default::default() };
        self.facts.symbols[sym].type_name = decl.encode();
        let full = self.enclosing_type().map_or_else(|| name.clone(), |o| format!("{o}.{name}"));
        // Supertypes: `extends` (a class's one, an interface's several) and `implements`.
        for field in ["superclass", "interfaces"] {
            if let Some(list) = node.child_by_field_name(field) {
                self.supertypes(sym, list);
            }
        }
        if let Some(ext) = children(node).find(|c| c.kind() == "extends_interfaces") {
            self.supertypes(sym, ext);
        }
        self.scopes.push(Scope { sym, type_name: Some(full), callable: false, locals: HashMap::new() });
        if let Some(tp) = node.child_by_field_name("type_parameters") {
            self.visit(tp);
        }
        for c in children(node).filter(|c| matches!(c.kind(), "modifiers")) {
            self.visit(c);
        }
        if node.kind() == "record_declaration"
            && let Some(params) = node.child_by_field_name("parameters")
        {
            for p in children(params).filter(|c| c.kind() == "formal_parameter") {
                if let Some(pn) = p.child_by_field_name("name") {
                    let pname = self.text(pn).to_string();
                    let ty = p.child_by_field_name("type").and_then(|t| self.declared_type(t));
                    let psym = self.push(p, pname, SymbolKind::Property, None);
                    self.facts.symbols[psym].type_name = ty;
                }
                if let Some(t) = p.child_by_field_name("type") {
                    self.visit(t);
                }
            }
        }
        if let Some(body) = node.child_by_field_name("body") {
            self.visit(body);
        }
        self.scopes.pop();
    }

    fn supertypes(&mut self, sym: usize, list: Node) {
        let mut stack = vec![list];
        while let Some(n) = stack.pop() {
            match n.kind() {
                "type_identifier" | "scoped_type_identifier" | "generic_type" => {
                    if let Some(target) = base_name(self.src, n) {
                        self.facts.relations.push(Relation { from: sym, target, kind: RelationKind::Inherits, line: line(n) });
                    }
                    self.type_use(n, RefKind::TypeRef, None);
                }
                _ => stack.extend(children(n).collect::<Vec<_>>().into_iter().rev()),
            }
        }
    }

    fn method(&mut self, node: Node) {
        let (name, kind) = match node.kind() {
            "constructor_declaration" | "compact_constructor_declaration" => ("init".to_string(), SymbolKind::Init),
            _ => match node.child_by_field_name("name") {
                Some(n) => (self.text(n).to_string(), SymbolKind::Method),
                None => return self.visit_children(node),
            },
        };
        let sym = self.push(node, name, kind, None);
        let params = node.child_by_field_name("parameters");
        if let Some(p) = params {
            let fixed = children(p).filter(|c| c.kind() == "formal_parameter").count() as u32;
            let spread = children(p).any(|c| c.kind() == "spread_parameter");
            self.facts.symbols[sym].arity = Some((fixed, (!spread).then_some(fixed)));
        } else if kind == SymbolKind::Method {
            self.facts.symbols[sym].arity = Some((0, Some(0)));
        }
        if let Some(t) = node.child_by_field_name("type") {
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

    fn field(&mut self, node: Node) {
        let ty_node = node.child_by_field_name("type");
        let ty = ty_node.and_then(|t| self.declared_type(t));
        if let Some(t) = ty_node {
            self.visit(t);
        }
        for c in children(node).filter(|c| c.kind() == "modifiers") {
            self.visit(c);
        }
        for d in children(node).filter(|c| c.kind() == "variable_declarator") {
            let Some(n) = d.child_by_field_name("name") else { continue };
            let name = self.text(n).to_string();
            let sym = self.push(d, name, SymbolKind::Property, d.child_by_field_name("value"));
            self.facts.symbols[sym].signature = collapse(
                &format!("{} {}", self.src[node.start_byte()..ty_node.map_or(node.start_byte(), |t| t.end_byte())].trim(), self.text(n)),
                SIGNATURE_CAP,
            );
            self.facts.symbols[sym].type_name = ty.clone();
            if let Some(v) = d.child_by_field_name("value") {
                self.with_callable(Some(sym), |cx| cx.visit(v));
            }
        }
    }

    fn enum_constant(&mut self, node: Node) {
        let Some(n) = node.child_by_field_name("name") else { return };
        let name = self.text(n).to_string();
        let sym = self.push(node, name, SymbolKind::Case, node.child_by_field_name("arguments"));
        self.facts.symbols[sym].type_name = self.enclosing_type();
        self.with_callable(Some(sym), |cx| {
            for c in children(node) {
                if Some(c) != node.child_by_field_name("name") {
                    cx.visit(c);
                }
            }
        });
    }

    fn call(&mut self, node: Node) {
        let Some(n) = node.child_by_field_name("name") else { return self.visit_children(node) };
        let name = self.text(n).to_string();
        let object = node.child_by_field_name("object");
        let args = node.child_by_field_name("arguments").map(arg_count);
        let (receiver, receiver_type) = match object {
            Some(o) => self.receiver_of(o),
            None => (None, None),
        };
        self.record(n, name, receiver, receiver_type, RefKind::Call, args);
        if let Some(o) = object {
            self.visit(o);
        }
        for c in children(node).filter(|c| matches!(c.kind(), "argument_list" | "type_arguments")) {
            self.visit(c);
        }
    }

    fn method_reference(&mut self, node: Node) {
        // `Type::method`, `expr::method`, `Type::new`.
        let parts: Vec<Node> = children(node).filter(|c| c.is_named()).collect();
        let Some(&target) = parts.first() else { return };
        let member = children(node).skip_while(|c| c.kind() != "::").nth(1);
        match member {
            Some(m) if m.kind() == "new" => self.type_use(target, RefKind::Instantiate, None),
            Some(m) if m.kind() == "identifier" => {
                let (receiver, receiver_type) = self.receiver_of(target);
                let name = self.text(m).to_string();
                self.record(m, name, receiver, receiver_type, RefKind::Call, None);
                self.visit(target);
            }
            _ => self.visit_children(node),
        }
    }

    fn field_access(&mut self, node: Node) {
        let (Some(object), Some(field)) = (node.child_by_field_name("object"), node.child_by_field_name("field")) else {
            return self.visit_children(node);
        };
        if field.kind() != "identifier" {
            return self.visit_children(node);
        }
        // A package-qualified name (`java.util.List` used as an expression) has a lower-case root that isn't a local
        // or a field; it is still recorded with its receiver, and resolution keeps it to the package it names.
        let name = self.text(field).to_string();
        let (receiver, receiver_type) = self.receiver_of(object);
        self.record(field, name, receiver, receiver_type, RefKind::Member, None);
        self.visit(object);
    }

    /// A name standing alone: a local, a parameter, or a field of the enclosing type (or a static import).
    fn bare_identifier(&mut self, node: Node) {
        let Some(parent) = node.parent() else { return };
        // Declarations and labels are names, not uses; qualified names are handled by their parents.
        if matches!(
            parent.kind(),
            "variable_declarator" | "formal_parameter" | "spread_parameter" | "catch_formal_parameter" | "lambda_expression"
                | "inferred_parameters" | "labeled_statement" | "break_statement" | "continue_statement"
                | "enhanced_for_statement" | "scoped_identifier" | "package_declaration" | "import_declaration"
                | "method_declaration" | "constructor_declaration" | "class_declaration" | "interface_declaration"
                | "enum_declaration" | "record_declaration" | "enum_constant" | "type_pattern" | "resource"
                | "element_value_pair" | "annotation_type_element_declaration" | "module_declaration"
        ) {
            if parent.kind() == "element_value_pair" && parent.child_by_field_name("key") == Some(node) {
                // `@Mark(level = 1)`: the annotation's element, a member of the annotation type.
                let annotation = parent.parent().and_then(|l| l.parent()).filter(|a| a.kind() == "annotation");
                let receiver = annotation.and_then(|a| a.child_by_field_name("name")).map(|n| collapse(self.text(n), RECEIVER_CAP));
                let name = self.text(node).to_string();
                self.record(node, name, receiver.clone(), receiver, RefKind::Call, Some(0));
            }
            return;
        }
        let name = self.text(node).to_string();
        if self.local(&name).is_some() {
            return;
        }
        self.record(node, name, None, None, RefKind::Member, None);
    }

    /// The receiver as written and, for a local or parameter with a declared type, that type.
    fn receiver_of(&self, object: Node) -> (Option<String>, Option<String>) {
        let text = collapse(self.text(object), RECEIVER_CAP);
        let ty = match object.kind() {
            "identifier" => self.local(self.text(object)).flatten(),
            "object_creation_expression" => object.child_by_field_name("type").and_then(|t| self.declared_type(t)),
            "this" => self.enclosing_type(),
            _ => None,
        };
        (Some(text), ty)
    }

    /// A type written somewhere: the name, with a qualifier as its receiver (`Map.Entry` is `Entry` on `Map`), and
    /// the types in its generic arguments. Primitive types and `var` aren't references.
    fn type_use(&mut self, node: Node, kind: RefKind, args: Option<u32>) {
        match node.kind() {
            "type_identifier" | "identifier" => {
                let name = self.text(node).to_string();
                if name == "var" {
                    return;
                }
                self.record(node, name, None, None, kind, args);
            }
            "scoped_type_identifier" | "scoped_identifier" => {
                let parts: Vec<Node> = children(node).filter(|c| c.is_named()).collect();
                if let (Some(q), Some(last)) = (parts.first(), parts.last()) {
                    let name = self.text(*last).to_string();
                    let receiver = collapse(self.text(*q), RECEIVER_CAP);
                    self.record(*last, name, Some(receiver), None, kind, args);
                    if parts.len() > 1 {
                        self.type_use(*q, RefKind::TypeRef, None);
                    }
                }
                for c in children(node).filter(|c| c.kind() == "type_arguments" || c.kind() == "annotation" || c.kind() == "marker_annotation") {
                    self.visit(c);
                }
            }
            "generic_type" => {
                let mut first = true;
                for c in children(node) {
                    if first && matches!(c.kind(), "type_identifier" | "scoped_type_identifier") {
                        self.type_use(c, kind, args);
                        first = false;
                    } else {
                        self.visit(c);
                    }
                }
            }
            "array_type" => {
                if let Some(e) = node.child_by_field_name("element") {
                    self.type_use(e, RefKind::TypeRef, None);
                }
            }
            "integral_type" | "floating_point_type" | "boolean_type" | "void_type" => {}
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

    /// A declared type, as an encoded type expression: the type's name with generic arguments dropped, an array's
    /// element type kept as an array. `None` for primitives and `var`.
    fn declared_type(&self, t: Node) -> Option<String> {
        type_expr(self.src, t).map(|e| e.encode())
    }
}

fn type_expr(src: &str, t: Node) -> Option<TypeExpr> {
    match t.kind() {
        "type_identifier" => {
            let n = &src[t.byte_range()];
            (n != "var").then(|| TypeExpr::Named(n.to_string()))
        }
        "scoped_type_identifier" => Some(TypeExpr::Named(src[t.byte_range()].split_whitespace().collect())),
        "generic_type" => {
            let base = children(t).find(|c| matches!(c.kind(), "type_identifier" | "scoped_type_identifier"))?;
            let name: String = src[base.byte_range()].split_whitespace().collect();
            let args: Vec<Option<TypeExpr>> = children(t)
                .find(|c| c.kind() == "type_arguments")
                .map(|a| children(a).filter(|c| c.is_named()).map(|c| type_expr(src, c)).collect())
                .unwrap_or_default();
            Some(if args.is_empty() { TypeExpr::Named(name) } else { TypeExpr::Generic { name, args } })
        }
        "array_type" => Some(TypeExpr::Array(Box::new(type_expr(src, t.child_by_field_name("element")?)?))),
        "catch_type" => {
            // `catch (A | B e)`: the type only when there is one.
            let alts: Vec<Node> = children(t).filter(|c| c.is_named()).collect();
            if alts.len() == 1 { type_expr(src, alts[0]) } else { None }
        }
        _ => None,
    }
}

/// A supertype's dotted name without generic arguments.
fn base_name(src: &str, n: Node) -> Option<String> {
    match n.kind() {
        "generic_type" => children(n)
            .find(|c| matches!(c.kind(), "type_identifier" | "scoped_type_identifier"))
            .map(|c| src[c.byte_range()].split_whitespace().collect()),
        _ => Some(src[n.byte_range()].split_whitespace().collect()),
    }
}

fn arg_count(list: Node) -> u32 {
    children(list).filter(|c| c.is_named() && !matches!(c.kind(), "line_comment" | "block_comment")).count() as u32
}

/// Annotation names on a declaration, without `@` and qualifiers: `Override`, `Test`, `GetMapping`.
fn annotations(src: &str, node: Node) -> Vec<String> {
    let Some(mods) = children(node).find(|c| c.kind() == "modifiers") else { return Vec::new() };
    children(mods)
        .filter(|c| matches!(c.kind(), "marker_annotation" | "annotation"))
        .filter_map(|a| a.child_by_field_name("name"))
        .map(|n| src[n.byte_range()].rsplit('.').next().unwrap_or_default().to_string())
        .collect()
}

/// The first line of the Javadoc comment right before a declaration.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn refs(f: &FileFacts, kind: RefKind) -> Vec<(String, Option<String>)> {
        f.refs.iter().filter(|r| r.kind == kind).map(|r| (r.name.clone(), r.receiver.clone())).collect()
    }

    #[test]
    fn types_members_package_and_imports() {
        let f = extract(
            "package org.example.shop;\n\
             import java.util.List;\n\
             import java.util.*;\n\
             import static org.junit.Assert.assertEquals;\n\
             import static java.util.Collections.*;\n\
             /** A cart. */\n\
             public class Cart<T> extends Base implements Iterable<T>, Sized {\n\
               private final List<Item> items;\n\
               static final int MAX = 3, MIN = 1;\n\
               public Cart(int n) { this.items = null; }\n\
               public int size() { return items.size(); }\n\
               interface Listener { void changed(Cart<?> c); }\n\
               enum State { OPEN, CLOSED }\n\
               record Line(String sku, int qty) {}\n\
             }\n",
        );
        let syms: Vec<_> = f.symbols.iter().map(|s| (s.name.as_str(), s.kind, s.container.as_deref())).collect();
        assert!(syms.contains(&("Cart", SymbolKind::Class, None)));
        assert!(syms.contains(&("items", SymbolKind::Property, Some("Cart"))));
        assert!(syms.contains(&("MAX", SymbolKind::Property, Some("Cart"))));
        assert!(syms.contains(&("MIN", SymbolKind::Property, Some("Cart"))));
        assert!(syms.contains(&("init", SymbolKind::Init, Some("Cart"))));
        assert!(syms.contains(&("size", SymbolKind::Method, Some("Cart"))));
        assert!(syms.contains(&("Listener", SymbolKind::Protocol, Some("Cart"))));
        assert!(syms.contains(&("changed", SymbolKind::Method, Some("Cart.Listener"))));
        assert!(syms.contains(&("OPEN", SymbolKind::Case, Some("Cart.State"))));
        assert!(syms.contains(&("sku", SymbolKind::Property, Some("Cart.Line"))));
        let cart = f.symbols.iter().find(|s| s.name == "Cart").unwrap();
        let decl = TypeDecl::decode(cart.type_name.as_deref());
        assert_eq!(decl.ns.as_deref(), Some("org.example.shop"));
        assert_eq!(decl.params, vec!["T".to_string()]);
        assert_eq!(cart.doc.as_deref(), Some("A cart."));
        let inner = f.symbols.iter().find(|s| s.name == "Listener").unwrap();
        assert_eq!(TypeDecl::decode(inner.type_name.as_deref()).ns.as_deref(), Some("org.example.shop"));
        let supers: Vec<_> = f.relations.iter().map(|r| r.target.as_str()).collect();
        assert_eq!(supers, vec!["Base", "Iterable", "Sized"]);
        let imports: Vec<_> = f.imports.iter().map(|i| i.module.as_str()).collect();
        assert_eq!(
            imports,
            vec!["java.util.List", "java.util.*", "static org.junit.Assert.assertEquals", "static java.util.Collections.*"]
        );
        let items = f.symbols.iter().find(|s| s.name == "items").unwrap();
        assert_eq!(
            TypeExpr::decode(items.type_name.as_deref().unwrap()),
            Some(TypeExpr::Generic { name: "List".into(), args: vec![Some(TypeExpr::Named("Item".into()))] })
        );
        assert_eq!(f.symbols.iter().find(|s| s.name == "init").unwrap().arity, Some((1, Some(1))));
        assert_eq!(f.parse_errors, 0);
    }

    #[test]
    fn calls_members_types_and_receivers() {
        let f = extract(
            "class A {\n\
               Store store;\n\
               void run(Order order, java.util.Map.Entry<String, Item> e) {\n\
                 Cart cart = new Cart(1, 2);\n\
                 cart.add(order.total());\n\
                 store.save(cart);\n\
                 Helpers.log(\"x\");\n\
                 this.store.flush();\n\
                 count++;\n\
                 list.forEach(Item::price);\n\
                 Supplier<Cart> s = Cart::new;\n\
                 if (o instanceof Order placed) { placed.ship(); }\n\
                 Class<?> k = Order.class;\n\
               }\n\
             }\n",
        );
        let calls = refs(&f, RefKind::Call);
        assert!(calls.contains(&("add".into(), Some("cart".into()))));
        assert!(calls.contains(&("total".into(), Some("order".into()))));
        assert!(calls.contains(&("save".into(), Some("store".into()))));
        assert!(calls.contains(&("log".into(), Some("Helpers".into()))));
        assert!(calls.contains(&("flush".into(), Some("this.store".into()))));
        assert!(calls.contains(&("price".into(), Some("Item".into()))));
        assert!(calls.contains(&("ship".into(), Some("placed".into()))));
        let add = f.refs.iter().find(|r| r.name == "add").unwrap();
        assert_eq!(add.receiver_type.as_deref(), Some("Cart"));
        assert_eq!(add.args, Some(1));
        assert_eq!(f.refs.iter().find(|r| r.name == "ship").unwrap().receiver_type.as_deref(), Some("Order"));
        let made = refs(&f, RefKind::Instantiate);
        assert_eq!(made, vec![("Cart".into(), None), ("Cart".into(), None)]);
        assert_eq!(f.refs.iter().find(|r| r.kind == RefKind::Instantiate).unwrap().args, Some(2));
        let types = refs(&f, RefKind::TypeRef);
        assert!(types.contains(&("Store".into(), None)));
        assert!(types.contains(&("Order".into(), None)));
        assert!(types.contains(&("Entry".into(), Some("java.util.Map".into()))));
        assert!(types.contains(&("Item".into(), None)));
        assert!(!types.iter().any(|(n, _)| n == "String"), "java.lang types aren't recorded");
        let members = refs(&f, RefKind::Member);
        assert!(members.contains(&("store".into(), Some("this".into()))));
        assert!(members.contains(&("count".into(), None)));
        assert!(!members.iter().any(|(n, _)| n == "cart" || n == "order"), "locals and parameters aren't members");
    }

    #[test]
    fn annotations_tests_and_entrypoints() {
        let f = extract(
            "class T {\n\
               @Test void adds() {}\n\
               @org.junit.jupiter.api.Disabled @Mark(level = 2) void skipped() {}\n\
               public static void main(String[] args) {}\n\
             }\n",
        );
        let adds = f.symbols.iter().find(|s| s.name == "adds").unwrap();
        assert_eq!(adds.tags, vec!["test"]);
        assert_eq!(f.symbols.iter().find(|s| s.name == "skipped").unwrap().attrs, vec!["Disabled", "Mark"]);
        assert_eq!(f.symbols.iter().find(|s| s.name == "main").unwrap().tags, vec!["entrypoint"]);
        let types = refs(&f, RefKind::TypeRef);
        assert!(types.contains(&("Test".into(), None)));
        assert!(types.contains(&("Disabled".into(), Some("org.junit.jupiter.api".into()))));
        assert!(refs(&f, RefKind::Call).contains(&("level".into(), Some("Mark".into()))));
    }
}
