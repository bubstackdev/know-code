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
use crate::types::{PathRoot, Seg, TypeDecl, TypeExpr};
use crate::urls::{self, Piece};
use crate::spring::{self, Mapping, Side};
use crate::{FileFacts, HttpRoute, Import, Ref, RefKind, Relation, RelationKind, StringConst, Symbol, SymbolKind, Unsupported};

pub const VERSION: u32 = 5;

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
    let mut nested = HashMap::new();
    member_types(source, tree.root_node(), None, &mut nested);
    let fields = field_types(source, tree.root_node());
    let mut cx =
        Cx { src: source, facts: FileFacts::default(), package: None, scopes: Vec::new(), seen: HashSet::new(), nested, fields };
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
    /// A type whose methods' Spring mappings are routes it serves or requests it sends (a controller's, a Feign
    /// client's): its own mapping (`@RequestMapping`, a Feign client's `path`), or why it can't be read.
    spring: Option<(Side, Result<Option<Mapping>, String>)>,
}

struct Cx<'a> {
    src: &'a str,
    facts: FileFacts,
    package: Option<String>,
    scopes: Vec<Scope>,
    seen: HashSet<(Option<usize>, String, RefKind, u32, u32)>,
    /// The file's member types: per type (dotted, within the package), the simple names of the types declared in it.
    nested: HashMap<String, HashSet<String>>,
    /// The file's fields' declared types (simple names), by field name, for telling an HTTP client's calls.
    fields: HashMap<String, String>,
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
                self.scopes.push(Scope { sym, type_name: None, callable: true, locals: HashMap::new(), spring: None });
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
            scope: None,
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
        let abstract_type = node.kind() == "interface_declaration"
            || children(node).find(|c| c.kind() == "modifiers").is_some_and(|m| children(m).any(|c| c.kind() == "abstract"));
        let spring = spring::side(&self.facts.symbols[sym].attrs, abstract_type).map(|side| (side, self.type_mapping(node)));
        self.scopes.push(Scope { sym, type_name: Some(full), callable: false, locals: HashMap::new(), spring });
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
        if kind == SymbolKind::Method {
            self.spring_routes(node, sym);
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
                self.string_const(node, d, v);
            }
        }
    }

    /// A `static final String` field (any `String` field of an interface, which is one), as a constant a request
    /// mapping may name: `Container.NAME`, its value encoded as a mapping's path is.
    fn string_const(&mut self, field: Node, declarator: Node, value: Node) {
        let mods: Vec<&str> = children(field).filter(|c| c.kind() == "modifiers").flat_map(children).map(|c| c.kind()).collect();
        let in_interface = self.scopes.last().is_some_and(|s| !s.callable && self.facts.symbols[s.sym].kind == SymbolKind::Protocol);
        let is_string = field.child_by_field_name("type").is_some_and(|t| self.text(t) == "String");
        if !is_string || !(in_interface || mods.contains(&"static") && mods.contains(&"final")) {
            return;
        }
        let (Some(container), Some(n)) = (self.enclosing_type(), declarator.child_by_field_name("name")) else { return };
        if let Ok(v) = self.path_expr(value) {
            let qualified = format!("{container}.{}", self.text(n));
            self.facts.constants.push(StringConst { qualified, value: v, line: line(declarator) });
        }
    }

    /// A type's own mapping, its methods' prefix: its `@RequestMapping` (or `@HttpExchange`) under a Feign client's
    /// `path`, each path of one joined with each of the other's.
    fn type_mapping(&self, node: Node) -> Result<Option<Mapping>, String> {
        let own = match self.mappings(node).into_iter().find(|(_, verb, _)| verb.is_none()) {
            Some((_, _, m)) => Some(m?),
            None => None,
        };
        let Some(feign) = self.feign_path(node)? else { return Ok(own) };
        let mut m = own.unwrap_or_default();
        let under = if m.paths.is_empty() { vec![String::new()] } else { std::mem::take(&mut m.paths) };
        m.paths = under.iter().map(|p| format!("{}/{}", feign.trim_end_matches('/'), p.trim_start_matches('/'))).collect();
        Ok(Some(m))
    }

    /// A `@FeignClient`'s `path`, the prefix of its methods' paths (its `value` and `name` are the service's).
    fn feign_path(&self, node: Node) -> Result<Option<String>, String> {
        let Some(mods) = children(node).find(|c| c.kind() == "modifiers") else { return Ok(None) };
        for a in children(mods).filter(|c| c.kind() == "annotation") {
            if a.child_by_field_name("name").is_none_or(|n| self.text(n).rsplit('.').next() != Some("FeignClient")) {
                continue;
            }
            let Some(args) = a.child_by_field_name("arguments") else { return Ok(None) };
            for pair in children(args).filter(|c| c.kind() == "element_value_pair") {
                if let (Some(k), Some(v)) = (pair.child_by_field_name("key"), pair.child_by_field_name("value"))
                    && self.text(k) == "path"
                {
                    return self.path_expr(v).map(Some);
                }
            }
        }
        Ok(None)
    }

    /// The request mappings annotating a declaration: each annotation's name, the verb it fixes (`None` for
    /// `@RequestMapping`) and what its arguments say, or why they can't be read.
    fn mappings<'t>(&self, node: Node<'t>) -> Vec<(Node<'t>, Option<&'static str>, Result<Mapping, String>)> {
        let Some(mods) = children(node).find(|c| c.kind() == "modifiers") else { return Vec::new() };
        let mut out = Vec::new();
        for a in children(mods).filter(|c| matches!(c.kind(), "marker_annotation" | "annotation")) {
            let Some(name) = a.child_by_field_name("name").map(|n| self.text(n).rsplit('.').next().unwrap_or_default()) else { continue };
            let Some(verb) = spring::annotation_verb(name) else { continue };
            out.push((a, verb, self.mapping(a)));
        }
        out
    }

    /// A mapping annotation's paths (`value`, `path`, or its one unnamed argument) and methods (`method`).
    fn mapping<'t>(&self, annotation: Node<'t>) -> Result<Mapping, String> {
        let mut m = Mapping::default();
        let Some(args) = annotation.child_by_field_name("arguments") else { return Ok(m) };
        let items = |v: Node<'t>| -> Vec<Node<'t>> {
            if v.kind() == "element_value_array_initializer" { children(v).filter(|c| c.is_named() && !c.kind().ends_with("comment")).collect() } else { vec![v] }
        };
        for arg in children(args).filter(|c| c.is_named() && !c.kind().ends_with("comment")) {
            let (key, value) = if arg.kind() == "element_value_pair" {
                match (arg.child_by_field_name("key"), arg.child_by_field_name("value")) {
                    (Some(k), Some(v)) => (self.text(k), v),
                    _ => continue,
                }
            } else {
                ("value", arg)
            };
            match key {
                "value" | "path" | "url" => {
                    for p in items(value) {
                        m.paths.push(self.path_expr(p)?);
                    }
                }
                "method" => {
                    for v in items(value) {
                        m.verbs.push(spring::request_method(self.text(v)).ok_or_else(|| format!("method `{}`", self.text(v)))?);
                    }
                }
                _ => {}
            }
        }
        Ok(m)
    }

    /// A path written in a mapping or a constant: string literals, constants (`{=NAME}`, `{=Paths.API}`) and their
    /// concatenation with `+`.
    fn path_expr(&self, e: Node) -> Result<String, String> {
        match e.kind() {
            "string_literal" => {
                let t = self.text(e);
                if t.starts_with("\"\"\"") || t.contains('\\') {
                    return Err(format!("path `{}`", collapse(t, 60)));
                }
                Ok(t.trim_matches('"').to_string())
            }
            "identifier" | "field_access" => Ok(format!("{{={}}}", self.text(e).split_whitespace().collect::<String>())),
            "binary_expression" if e.child_by_field_name("operator").is_some_and(|o| self.text(o) == "+") => {
                let (Some(l), Some(r)) = (e.child_by_field_name("left"), e.child_by_field_name("right")) else {
                    return Err("path expression".to_string());
                };
                Ok(self.path_expr(l)? + &self.path_expr(r)?)
            }
            "parenthesized_expression" => children(e).find(|c| c.is_named()).map_or(Err("path expression".to_string()), |c| self.path_expr(c)),
            _ => Err(format!("path `{}`", collapse(self.text(e), 60))),
        }
    }

    /// A handler method's routes, or a client method's requests (see [`spring::routes`]); a mapping that can't be read
    /// is `unsupported`, never a route with a wrong path.
    fn spring_routes(&mut self, node: Node, sym: usize) {
        let Some((side, of_type)) = self.scopes.last().filter(|s| !s.callable).and_then(|s| s.spring.clone()) else { return };
        for (at, verb, mapping) in self.mappings(node) {
            let ty = match &of_type {
                Ok(ty) => ty.as_ref(),
                Err(why) => {
                    self.facts.unsupported.push(Unsupported { area: "http", construct: format!("RequestMapping on the type: {why}"), line: line(at) });
                    continue;
                }
            };
            match mapping {
                Ok(m) => {
                    for (verb, path) in spring::routes(side, ty, &m, verb) {
                        // A handler's route is at the method (its owner's line, annotations included); a client's request at the
                        // method's name, where callers call it.
                        let at = match side {
                            Side::Serves => line(node),
                            Side::Requests => node.child_by_field_name("name").map_or(line(node), line),
                        };
                        let route = HttpRoute { sym: Some(sym), side: side.as_str(), verb, path, path_ref: None, service: None, service_ref: None, line: at };
                        if !self.facts.http_routes.contains(&route) {
                            self.facts.http_routes.push(route);
                        }
                    }
                }
                Err(why) => self.facts.unsupported.push(Unsupported { area: "http", construct: format!("request mapping: {why}"), line: line(at) }),
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
        self.client_call(node, n, &name, object);
        self.record(n, name, receiver, receiver_type, RefKind::Call, args);
        if let Some(o) = object {
            self.visit(o);
        }
        for c in children(node).filter(|c| matches!(c.kind(), "argument_list" | "type_arguments")) {
            self.visit(c);
        }
    }

    /// A Spring HTTP client's request, as a client route of the calling method: a `RestTemplate` method
    /// (`getForObject(url, …)`, `exchange(url, HttpMethod.PUT, …)`) on a receiver of that type (or named for it), or
    /// the `uri(url, …)` after a verb in a `WebClient` or `RestClient` chain (`client.get().uri(…)`,
    /// `.method(HttpMethod.PATCH).uri(…)`), at the `uri` call's line.
    fn client_call(&mut self, call: Node, name_node: Node, name: &str, object: Option<Node>) {
        let args: Vec<Node> = call.child_by_field_name("arguments").map(|a| children(a).filter(|c| c.is_named() && !c.kind().ends_with("comment")).collect()).unwrap_or_default();
        let (verb, url, at) = if name == "uri" {
            // The verb call before `.uri(…)`: `get()`, …, or `method(HttpMethod.X)`.
            let Some(inner) = object.filter(|o| o.kind() == "method_invocation") else { return };
            let Some(m) = inner.child_by_field_name("name") else { return };
            let inner_args: Vec<Node> =
                inner.child_by_field_name("arguments").map(|a| children(a).filter(|c| c.is_named()).collect()).unwrap_or_default();
            let verb = match (self.text(m), inner_args.as_slice()) {
                ("method", [v]) => Err(*v),
                (v @ ("get" | "post" | "put" | "patch" | "delete" | "head" | "options"), []) => Ok(v.to_ascii_uppercase()),
                _ => return,
            };
            let Some(url) = args.first() else { return };
            (verb, *url, name_node)
        } else {
            let verb = match name {
                "getForObject" | "getForEntity" => Ok("GET".to_string()),
                "postForObject" | "postForEntity" | "postForLocation" => Ok("POST".to_string()),
                "put" => Ok("PUT".to_string()),
                "delete" => Ok("DELETE".to_string()),
                "patchForObject" => Ok("PATCH".to_string()),
                "headForHeaders" => Ok("HEAD".to_string()),
                "optionsForAllow" => Ok("OPTIONS".to_string()),
                "exchange" | "execute" if args.len() >= 2 => Err(args[1]),
                _ => return,
            };
            if !object.is_some_and(|o| self.is_rest_template(o)) {
                return;
            }
            let Some(url) = args.first() else { return };
            (verb, *url, name_node)
        };
        let verb = match verb {
            Ok(v) => Some(v),
            Err(e) => self.text(e).rsplit('.').next().and_then(urls::verb),
        };
        let Some(verb) = verb else {
            self.facts.unsupported.push(Unsupported { area: "http", construct: "a request whose method is chosen at run time".to_string(), line: line(at) });
            return;
        };
        let mut pieces = self.url_pieces(url);
        // A call first is the base address a method gives (`serviceUri() + "/owners"`), not a path segment.
        if let Some(first @ Piece::Call) = pieces.first_mut() {
            *first = Piece::Unknown;
        }
        let path = urls::encode(&pieces, true).filter(|p| urls::looks_like_path(p)).unwrap_or_default();
        self.facts.http_routes.push(HttpRoute { sym: self.current(), side: "client", verb, path, path_ref: None, service: None, service_ref: None, line: line(at) });
    }

    /// Whether a receiver is a `RestTemplate` (or `RestOperations`, `TestRestTemplate`): by its declared type when a
    /// local, parameter or field of the file says it, by its name otherwise (`restTemplate`, `getRestTemplate()`).
    fn is_rest_template(&self, o: Node) -> bool {
        let simple = |t: &str| matches!(t.rsplit('.').next().unwrap_or(t), "RestTemplate" | "RestOperations" | "TestRestTemplate" | "OAuth2RestTemplate");
        let name = match o.kind() {
            "identifier" => self.text(o),
            "field_access" if o.child_by_field_name("object").is_some_and(|x| x.kind() == "this") => o.child_by_field_name("field").map_or("", |f| self.text(f)),
            _ => "",
        };
        if !name.is_empty() {
            let declared = match self.local(name) {
                Some(local) => local,
                None => self.fields.get(name).cloned(),
            };
            if let Some(t) = declared {
                return simple(&t);
            }
        }
        let lower = self.text(o).to_ascii_lowercase();
        lower.contains("resttemplate") || lower.contains("restoperations")
    }

    /// A request URL's pieces (see [`urls`]): string literals (a URI template's `{var}` a value), constants, `+`.
    fn url_pieces(&self, e: Node) -> Vec<Piece> {
        match e.kind() {
            "string_literal" => {
                let t = self.text(e);
                let body = t.trim_start_matches('"').trim_end_matches('"');
                let mut out = Vec::new();
                let mut rest = body;
                while let Some(i) = rest.find('{') {
                    let Some(j) = rest[i..].find('}') else { break };
                    out.push(Piece::Text(rest[..i].to_string()));
                    out.push(Piece::Unknown);
                    rest = &rest[i + j + 1..];
                }
                out.push(Piece::Text(rest.to_string()));
                out
            }
            "parenthesized_expression" => children(e).find(|c| c.is_named()).map(|c| self.url_pieces(c)).unwrap_or_default(),
            "binary_expression" if e.child_by_field_name("operator").is_some_and(|o| self.text(o) == "+") => {
                let mut out = e.child_by_field_name("left").map(|l| self.url_pieces(l)).unwrap_or_default();
                out.extend(e.child_by_field_name("right").map(|r| self.url_pieces(r)).unwrap_or_default());
                out
            }
            "identifier" if self.local(self.text(e)).is_none() => vec![Piece::Ref(self.text(e).to_string())],
            "field_access" => vec![Piece::Ref(self.text(e).trim_start_matches("this.").split_whitespace().collect())],
            "method_invocation" => vec![Piece::Call],
            _ => vec![Piece::Unknown],
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
        // The object of a call or a field access written as a type is (`ObjectUtils.isEmpty(x)`,
        // `JsonScope.EMPTY_DOCUMENT`): a static member's type, a type reference. A constant (`INSTANCE.run()`) isn't.
        let receiver = matches!(parent.kind(), "method_invocation" | "field_access" | "method_reference")
            && parent.child_by_field_name("object").or_else(|| parent.child(0)) == Some(node);
        let kind = if receiver && type_like(&name) { RefKind::TypeRef } else { RefKind::Member };
        self.record(node, name, None, None, kind, None);
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
        // Otherwise how its type follows from what the repo declares (a field's, a method's return), for the query.
        (Some(text), ty.or_else(|| self.expr_type(object).map(|t| t.encode())))
    }

    /// An expression's type as a path the query evaluates against declarations: `this`, a field of the caller's type,
    /// `Type.member`, and chains of member uses and calls on them (`store.items().first()`).
    fn expr_type(&self, e: Node) -> Option<TypeExpr> {
        match e.kind() {
            "this" => Some(TypeExpr::root(PathRoot::SelfType)),
            "identifier" => {
                let name = self.text(e);
                match self.local(name) {
                    Some(t) => t.and_then(|t| TypeExpr::decode(&t)),
                    None if type_like(name) => None,
                    None => Some(TypeExpr::root(PathRoot::Prop(name.to_string()))),
                }
            }
            "parenthesized_expression" => children(e).find(|c| c.is_named()).and_then(|c| self.expr_type(c)),
            "cast_expression" | "object_creation_expression" => e.child_by_field_name("type").and_then(|t| type_expr(self.src, t)),
            "field_access" => {
                let object = e.child_by_field_name("object")?;
                let field = self.text(e.child_by_field_name("field")?).to_string();
                self.member_of(object, Seg::Member(field))
            }
            "method_invocation" => {
                let name = self.text(e.child_by_field_name("name")?).to_string();
                match e.child_by_field_name("object") {
                    Some(object) => self.member_of(object, Seg::Call(name)),
                    None => Some(TypeExpr::Call { owner: None, name }),
                }
            }
            _ => None,
        }
    }

    /// `object.seg`'s type: on the object's type, or on a type written by name (`Type.member`).
    fn member_of(&self, object: Node, seg: Seg) -> Option<TypeExpr> {
        match self.expr_type(object) {
            Some(t) => Some(t.then(seg)),
            None if object.kind() == "identifier" && type_like(self.text(object)) && self.local(self.text(object)).is_none() => {
                Some(TypeExpr::root(PathRoot::Static(self.text(object).to_string())).then(seg))
            }
            None => None,
        }
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
        type_expr(self.src, t).map(|e| self.qualify(e).encode())
    }

    /// A simple type name written inside a type that declares (or whose outer types declare) a member type of that
    /// name means the member type (JLS 6.4.1): `Builder` in `ReflectionDiffBuilder` is `ReflectionDiffBuilder.Builder`,
    /// never a top-level `Builder` elsewhere. Qualified so the query looks it up there.
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

/// The member types `node`'s type declarations declare, by the dotted name of the declaring type (`outer`).
fn member_types(src: &str, node: Node, outer: Option<&str>, out: &mut HashMap<String, HashSet<String>>) {
    for c in children(node) {
        let is_type = matches!(
            c.kind(),
            "class_declaration" | "interface_declaration" | "enum_declaration" | "record_declaration" | "annotation_type_declaration"
        );
        match (is_type, c.child_by_field_name("name")) {
            (true, Some(n)) => {
                let name = &src[n.byte_range()];
                if let Some(o) = outer {
                    out.entry(o.to_string()).or_default().insert(name.to_string());
                }
                let full = outer.map_or_else(|| name.to_string(), |o| format!("{o}.{name}"));
                if let Some(body) = c.child_by_field_name("body") {
                    member_types(src, body, Some(&full), out);
                }
            }
            // A body's members, and anything else but a method's body (a local class is no member type).
            _ if !matches!(c.kind(), "method_declaration" | "constructor_declaration" | "block" | "lambda_expression") => {
                member_types(src, c, outer, out)
            }
            _ => {}
        }
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

/// `Name`, written as a type is: an upper-case first letter and a lower-case letter (not a constant like `MAX`).
fn type_like(name: &str) -> bool {
    name.chars().next().is_some_and(char::is_uppercase) && name.chars().any(char::is_lowercase)
}

fn arg_count(list: Node) -> u32 {
    children(list).filter(|c| c.is_named() && !matches!(c.kind(), "line_comment" | "block_comment")).count() as u32
}

/// Every field's declared type in the file, by the field's name: the type's base name (`RestTemplate`).
fn field_types(src: &str, root: Node) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == "field_declaration" {
            if let Some(t) = n.child_by_field_name("type").and_then(|t| base_name(src, t)) {
                for d in children(n).filter(|c| c.kind() == "variable_declarator") {
                    if let Some(name) = d.child_by_field_name("name") {
                        out.insert(src[name.byte_range()].to_string(), t.clone());
                    }
                }
            }
            continue;
        }
        stack.extend(children(n));
    }
    out
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
        assert!(refs(&f, RefKind::TypeRef).contains(&("Helpers".into(), None)), "a static call's type");
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
    fn member_types_and_receiver_paths() {
        let f = extract(
            "class Report {\n\
               static Builder builder() { return null; }\n\
               static final class Builder { Builder title() { return this; } Report build() { return null; } }\n\
               Store store;\n\
               void run() { Report.builder().title().build(); this.store.items().first(); store.save(); }\n\
             }\n",
        );
        let builder = f.symbols.iter().find(|s| s.name == "builder").unwrap();
        assert_eq!(builder.type_name.as_deref(), Some("Report.Builder"), "a member type's name is qualified");
        let build = f.refs.iter().find(|r| r.name == "build" && r.kind == RefKind::Call).unwrap();
        assert_eq!(
            TypeExpr::decode(build.receiver_type.as_deref().unwrap()),
            Some(TypeExpr::root(PathRoot::Static("Report".into())).then(Seg::Call("builder".into())).then(Seg::Call("title".into())))
        );
        let first = f.refs.iter().find(|r| r.name == "first").unwrap();
        assert_eq!(
            TypeExpr::decode(first.receiver_type.as_deref().unwrap()),
            Some(TypeExpr::root(PathRoot::SelfType).then(Seg::Member("store".into())).then(Seg::Call("items".into())))
        );
        let save = f.refs.iter().find(|r| r.name == "save").unwrap();
        assert_eq!(TypeExpr::decode(save.receiver_type.as_deref().unwrap()), Some(TypeExpr::root(PathRoot::Prop("store".into()))));
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

    #[test]
    fn spring_handler_methods_serve_their_mappings() {
        let src = "package org.example.shop;\n\
                   \n\
                   @RestController\n\
                   @RequestMapping(Paths.API + \"/orders\")\n\
                   class OrderController {\n\
                       static final String ITEMS = \"/{id}/items\";\n\
                       @GetMapping(\"/{id}\")\n\
                       Order find(long id) { return null; }\n\
                       @PostMapping\n\
                       Order create() { return null; }\n\
                       @RequestMapping(value = {\"a\", ITEMS}, method = {RequestMethod.PUT, RequestMethod.PATCH})\n\
                       void update() {}\n\
                       @RequestMapping(\"/any\")\n\
                       void any() {}\n\
                       @GetMapping(path = pathOf(\"x\"))\n\
                       void computed() {}\n\
                       void helper() {}\n\
                   }\n\
                   interface Paths { String API = \"/api\"; }\n\
                   @FeignClient(\"orders\")\n\
                   interface OrderClient { @GetMapping(\"/api/orders/{id}\") Order find(long id); }\n\
                   @Service\n\
                   class NotAController { @GetMapping(\"/no\") void no() {} }\n";
        let f = extract(src);
        let routes: Vec<(String, &str, &str)> =
            f.http_routes.iter().filter(|r| r.side == "server").map(|r| (f.symbols[r.sym.unwrap()].name.clone(), r.verb.as_str(), r.path.as_str())).collect();
        assert_eq!(
            routes,
            [
                ("find".to_string(), "GET", "{=Paths.API}/orders/{id}"),
                ("create".to_string(), "POST", "{=Paths.API}/orders"),
                ("update".to_string(), "PUT", "{=Paths.API}/orders/a"),
                ("update".to_string(), "PATCH", "{=Paths.API}/orders/a"),
                ("update".to_string(), "PUT", "{=Paths.API}/orders/{=ITEMS}"),
                ("update".to_string(), "PATCH", "{=Paths.API}/orders/{=ITEMS}"),
                ("any".to_string(), "*", "{=Paths.API}/orders/any"),
            ]
        );
        assert!(f.http_routes.iter().filter(|r| r.side == "server").all(|r| r.line == f.symbols[r.sym.unwrap()].start_line));
        assert_eq!(f.unsupported.len(), 1, "{:?}", f.unsupported);
        assert!(f.unsupported[0].construct.contains("pathOf"));
        let consts: Vec<(&str, &str)> = f.constants.iter().map(|c| (c.qualified.as_str(), c.value.as_str())).collect();
        assert_eq!(consts, [("OrderController.ITEMS", "/{id}/items"), ("Paths.API", "/api")]);
    }

    #[test]
    fn spring_clients_request_routes() {
        let src = "package org.example.shop;\n\
                   \n\
                   @FeignClient(name = \"orders\", path = \"/api\")\n\
                   interface OrderClient {\n\
                       @RequestMapping(method = RequestMethod.PUT, value = \"/orders/{id}\")\n\
                       void update(long id);\n\
                       @GetMapping(\"/orders\")\n\
                       List<Order> all();\n\
                       @RequestMapping(\"/plain\")\n\
                       void plain();\n\
                   }\n\
                   @HttpExchange(\"/stock\")\n\
                   interface StockClient {\n\
                       @GetExchange(\"/{sku}\")\n\
                       Stock find(String sku);\n\
                       @HttpExchange(method = \"PATCH\", url = \"/{sku}\")\n\
                       void patch(String sku);\n\
                   }\n\
                   class Calls {\n\
                       static final String OWNERS = \"/owners\";\n\
                       private final RestTemplate rest;\n\
                       private final Map<String, String> cache;\n\
                       private final WebClient web;\n\
                       void run(String base, long id) {\n\
                           rest.getForObject(\"http://customers/owners/{id}\", Owner.class, id);\n\
                           rest.exchange(base + \"/pets\", HttpMethod.DELETE, null, Void.class);\n\
                           cache.put(\"/not/a/request\", \"x\");\n\
                           web.get()\n\
                               .uri(OWNERS + \"/{id}\", id)\n\
                               .retrieve();\n\
                           web.method(HttpMethod.PATCH).uri(serviceUri() + \"/visits\").retrieve();\n\
                           rest.exchange(\"/x\", method, null, Void.class);\n\
                       }\n\
                   }\n";
        let f = extract(src);
        let routes: Vec<(String, &str, &str, &str, u32)> = f
            .http_routes
            .iter()
            .map(|r| (f.symbols[r.sym.unwrap()].name.clone(), r.side, r.verb.as_str(), r.path.as_str(), r.line))
            .collect();
        assert_eq!(
            routes,
            [
                ("update".to_string(), "client", "PUT", "api/orders/{id}", 6),
                ("all".to_string(), "client", "GET", "api/orders", 8),
                ("plain".to_string(), "client", "GET", "api/plain", 10),
                ("find".to_string(), "client", "GET", "stock/{sku}", 15),
                ("patch".to_string(), "client", "PATCH", "stock/{sku}", 17),
                ("run".to_string(), "client", "GET", "http://customers/owners/{}", 25),
                ("run".to_string(), "client", "DELETE", "/pets", 26),
                ("run".to_string(), "client", "GET", "{=OWNERS}/{}", 29),
                ("run".to_string(), "client", "PATCH", "/visits", 31),
            ]
        );
        assert_eq!(f.unsupported.len(), 1, "{:?}", f.unsupported);
    }
}
