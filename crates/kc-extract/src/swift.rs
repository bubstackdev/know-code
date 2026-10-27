//! Swift extraction over tree-sitter-swift.

use std::collections::{HashMap, HashSet};

use tree_sitter::{Node, Parser};

use crate::types::{PathRoot, Seg, TypeExpr};
use crate::{FileFacts, Import, Ref, RefKind, Relation, RelationKind, Symbol, SymbolKind};

pub const VERSION: u32 = 15;

/// Collection methods whose closure argument receives the receiver's elements: `$0` (or the first named parameter)
/// is an element.
const ELEMENT_CLOSURES: &[&str] = &[
    "allSatisfy", "compactMap", "contains", "drop", "filter", "first", "firstIndex", "flatMap", "forEach", "last",
    "lastIndex", "map", "partition", "prefix", "removeAll", "split",
];

/// Collection methods whose closure receives two elements (`$0`, `$1`).
const PAIR_CLOSURES: &[&str] = &["sorted", "sort", "min", "max", "elementsEqual"];

const SIGNATURE_CAP: usize = 240;
const RECEIVER_CAP: usize = 48;
const DOC_CAP: usize = 200;

/// Standard-library and Foundation types that would otherwise dominate type references without telling an agent
/// anything about the code base.
const COMMON_TYPES: &[&str] = &[
    "Any", "AnyObject", "Array", "Bool", "CGFloat", "CGPoint", "CGRect", "CGSize", "Character", "Data", "Date",
    "Dictionary", "Double", "Error", "Float", "Int", "Int16", "Int32", "Int64", "Int8", "Never", "Optional", "Self",
    "Set", "String", "Substring", "TimeInterval", "UInt", "UInt16", "UInt32", "UInt64", "UInt8", "URL", "UUID",
    "Void",
];

const BRANCH_KINDS: &[&str] = &[
    "if_statement",
    "guard_statement",
    "switch_entry",
    "for_statement",
    "while_statement",
    "repeat_while_statement",
    "catch_block",
    "ternary_expression",
    "conjunction_expression",
    "disjunction_expression",
];

/// Control-flow blocks that nest, for [`Symbol::nesting`](crate::Symbol::nesting).
const NEST_KINDS: &[&str] = &[
    "if_statement", "for_statement", "while_statement", "repeat_while_statement", "switch_statement", "do_statement",
];

pub fn extract(source: &str) -> FileFacts {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_swift::LANGUAGE.into()).expect("tree-sitter-swift ABI matches tree-sitter");
    let Some(tree) = parser.parse(source, None) else {
        return FileFacts { parse_errors: 1, ..Default::default() };
    };
    let mut cx = Cx {
        src: source,
        facts: FileFacts::default(),
        scopes: Vec::new(),
        seen_refs: HashSet::new(),
        closure_locals: HashMap::new(),
        seen_bare: HashSet::new(),
    };
    cx.visit(tree.root_node());
    cx.facts.script = children(tree.root_node()).any(|c| {
        c.is_named()
            && !matches!(
                c.kind(),
                "import_declaration" | "class_declaration" | "protocol_declaration" | "function_declaration"
                    | "property_declaration" | "typealias_declaration" | "comment" | "multiline_comment"
                    | "operator_declaration" | "precedence_group_declaration" | "macro_declaration" | "directive"
                    | "shebang_line" | "associatedtype_declaration"
            )
    });
    cx.facts
}

struct Scope {
    sym: usize,
    /// Dotted type name when this scope is a type or extension body.
    type_name: Option<String>,
    /// Function, initializer or property-accessor body: locals here aren't symbols.
    callable: bool,
    /// Parameters, locals and closure parameters with a known (or resolvable) type, for receiver typing.
    locals: HashMap<String, TypeExpr>,
    /// Every name bound in this body (typed or not), so bare identifiers that aren't locals can be recorded as
    /// member reads.
    bound: HashSet<String>,
}

struct Cx<'a> {
    src: &'a str,
    facts: FileFacts,
    scopes: Vec<Scope>,
    seen_refs: HashSet<(Option<usize>, String, RefKind, u32, u32)>,
    /// Parameter types for closures passed to collection methods, keyed by the closure node's id.
    closure_locals: HashMap<usize, HashMap<String, TypeExpr>>,
    /// Bare identifiers already recorded per enclosing symbol.
    seen_bare: HashSet<(Option<usize>, String)>,
}

impl<'a> Cx<'a> {
    fn text(&self, node: Node) -> &'a str {
        &self.src[node.byte_range()]
    }

    fn visit(&mut self, node: Node) {
        if node.is_error() || node.is_missing() {
            self.facts.parse_errors += 1;
        }
        if BRANCH_KINDS.contains(&node.kind()) {
            self.bump_complexity();
        }
        if NEST_KINDS.contains(&node.kind())
            && let Some(s) = self.scopes.iter().rev().find(|s| s.callable)
        {
            let sym = &mut self.facts.symbols[s.sym];
            let depth = crate::nesting_depth(node, NEST_KINDS, (sym.start_byte, sym.end_byte));
            sym.nesting = sym.nesting.max(depth);
        }
        match node.kind() {
            "class_declaration" | "protocol_declaration" => self.type_decl(node),
            "function_declaration" | "protocol_function_declaration" => self.func_decl(node, SymbolKind::Function),
            "init_declaration" => self.func_decl(node, SymbolKind::Init),
            "subscript_declaration" => self.func_decl(node, SymbolKind::Subscript),
            "property_declaration" | "protocol_property_declaration" if !self.in_callable() => {
                self.property_decl(node)
            }
            "property_declaration" => {
                self.local_binding(node);
                self.visit_children(node);
            }
            "for_statement" => {
                // `for x in xs`: x is an element of xs.
                if let (Some(item), Some(coll)) = (node.child_by_field_name("item"), node.child_by_field_name("collection"))
                    && let Some(ident) = item.child_by_field_name("bound_identifier")
                    && let Some(ty) = self.expr_type(coll)
                {
                    let name = self.text(ident).to_string();
                    self.bind_local(name, TypeExpr::Elem(Box::new(ty)));
                }
                self.visit_children(node);
            }
            "lambda_literal" => match (self.closure_locals.remove(&node.id()), self.current_sym()) {
                (Some(locals), Some(sym)) => {
                    self.scopes.push(Scope { sym, type_name: None, callable: true, locals, bound: HashSet::new() });
                    self.bind_lambda_params(node);
                    self.visit_children(node);
                    self.scopes.pop();
                }
                _ => {
                    self.bind_lambda_params(node);
                    self.visit_children(node);
                }
            },
            "pattern" | "capture_list_item" if self.in_callable() => {
                // Bindings (`let x`, `case .a(let y)`, `[weak z]`): names, plus any expressions inside.
                self.bind_pattern(node);
            }
            "catch_block" if self.in_callable() => {
                self.bind("error".to_string());
                self.visit_children(node);
            }
            "simple_identifier" if self.in_callable() => self.bare_identifier(node),
            "enum_entry" => self.enum_entry(node),
            "typealias_declaration" => self.typealias_decl(node),
            "import_declaration" => self.import_decl(node),
            "call_expression" => {
                self.call(node);
                for (i, child) in children(node).enumerate() {
                    match (i, child.kind()) {
                        // The callee was recorded by `call`; only its receiver still needs a visit.
                        (0, "navigation_expression") => {
                            if let Some(t) = child.child_by_field_name("target") {
                                self.visit(t);
                            }
                        }
                        (0, "prefix_expression" | "simple_identifier") => {}
                        _ => self.visit(child),
                    }
                }
            }
            "navigation_expression" => self.member_access(node),
            "prefix_expression" => {
                if let Some(target) = node.child_by_field_name("target")
                    && target.kind() == "simple_identifier"
                    && node.child_by_field_name("operation").is_some_and(|o| self.text(o) == ".")
                {
                    let name = self.text(target).to_string();
                    self.add_ref(name, Some(".".into()), RefKind::Member, target);
                } else {
                    self.visit_children(node);
                }
            }
            "modifiers" => {
                // Attribute arguments (`@Environment(\.theme)`) are code; the attribute names aren't references.
                for attr in children(node).filter(|c| c.kind() == "attribute") {
                    for arg in children(attr).filter(|c| c.is_named() && c.kind() != "user_type") {
                        self.visit(arg);
                    }
                }
            }
            "constructor_expression" => {
                if let Some(ty) = node.child_by_field_name("constructed_type")
                    && ty.kind() == "user_type"
                {
                    let name = last_component(&type_name(self.src, ty)).to_string();
                    self.add_ref(name, None, RefKind::Instantiate, ty);
                }
                for child in children(node) {
                    if child.kind() == "constructor_suffix" {
                        self.visit(child);
                    }
                }
            }
            "user_type" => {
                let name = type_name(self.src, node);
                let name = last_component(&name);
                if !COMMON_TYPES.contains(&name) {
                    self.add_ref(name.to_string(), None, RefKind::TypeRef, node);
                }
                for child in children(node) {
                    if child.kind() == "type_arguments" {
                        self.visit(child);
                    }
                }
            }
            _ => self.visit_children(node),
        }
    }

    fn visit_children(&mut self, node: Node) {
        for child in children(node) {
            self.visit(child);
        }
    }

    fn in_callable(&self) -> bool {
        self.scopes.last().is_some_and(|s| s.callable)
    }

    fn enclosing_type(&self) -> Option<String> {
        self.scopes.iter().rev().find_map(|s| s.type_name.clone())
    }

    fn current_sym(&self) -> Option<usize> {
        self.scopes.last().map(|s| s.sym)
    }

    fn bump_complexity(&mut self) {
        if let Some(scope) = self.scopes.iter().rev().find(|s| s.callable) {
            self.facts.symbols[scope.sym].complexity += 1;
        }
    }

    fn push_symbol(&mut self, node: Node, name: String, kind: SymbolKind, container: Option<String>) -> usize {
        let header_end = ["body", "computed_value"]
            .iter()
            .find_map(|f| node.child_by_field_name(f))
            .or_else(|| first_named_of(node, "willset_didset_block"))
            .map_or(node.end_byte(), |b| b.start_byte());
        let signature = collapse(&self.src[node.start_byte()..header_end], SIGNATURE_CAP);
        let attrs = attributes(self.src, node);
        let complexity = if matches!(kind, SymbolKind::Function | SymbolKind::Method | SymbolKind::Init) { 1 } else { 0 };
        self.facts.symbols.push(Symbol {
            parent: self.current_sym(),
            name,
            kind,
            container,
            signature,
            doc: doc_comment(self.src, node),
            attrs,
            tags: Vec::new(),
            start_line: line(node),
            end_line: node.end_position().row as u32 + 1,
            start_byte: node.start_byte() as u32,
            end_byte: node.end_byte() as u32,
            complexity,
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
        let kind = match node.child_by_field_name("declaration_kind").map(|k| self.text(k)) {
            Some("class") => SymbolKind::Class,
            Some("struct") => SymbolKind::Struct,
            Some("enum") => SymbolKind::Enum,
            Some("actor") => SymbolKind::Actor,
            Some("extension") => SymbolKind::Extension,
            Some("protocol") => SymbolKind::Protocol,
            _ => return self.visit_children(node),
        };
        let Some(name_node) = node.child_by_field_name("name") else { return self.visit_children(node) };
        let dotted = type_name(self.src, name_node);
        let outer = self.enclosing_type();
        let (name, container, qualified) = if kind == SymbolKind::Extension {
            // `extension A.B` extends the type B nested in A; the extension's own container is A.
            let (prefix, last) = match dotted.rsplit_once('.') {
                Some((p, l)) => (Some(p.to_string()), l.to_string()),
                None => (outer.clone(), dotted.clone()),
            };
            (last, prefix, dotted.clone())
        } else {
            let qualified = match &outer {
                Some(o) => format!("{o}.{dotted}"),
                None => dotted.clone(),
            };
            (dotted, outer, qualified)
        };
        let sym = self.push_symbol(node, name, kind, container);
        if kind == SymbolKind::Extension {
            // Extending a type uses it; across repos this is often the only trace of the dependency.
            let extended = last_component(&qualified).to_string();
            self.scopes.push(Scope { sym, type_name: None, callable: false, locals: HashMap::new(), bound: HashSet::new() });
            self.add_ref(extended, None, RefKind::TypeRef, name_node);
            self.scopes.pop();
        }

        let mut targets = Vec::new();
        // Inherited types are also type references from this type, so `refs` finds subclasses and conformers.
        self.scopes.push(Scope { sym, type_name: None, callable: false, locals: HashMap::new(), bound: HashSet::new() });
        for child in children(node) {
            if child.kind() == "inheritance_specifier"
                && let Some(target) = child.child_by_field_name("inherits_from")
            {
                let target_name = type_name(self.src, target);
                self.facts.relations.push(Relation {
                    from: sym,
                    target: target_name.clone(),
                    kind: RelationKind::Inherits,
                    line: line(target),
                });
                self.add_ref(last_component(&target_name).to_string(), None, RefKind::TypeRef, target);
                targets.push(target_name);
            }
        }
        self.scopes.pop();
        self.facts.symbols[sym].tags = type_tags(kind, &self.facts.symbols[sym].attrs, &targets);

        self.scopes.push(Scope { sym, type_name: Some(qualified), callable: false, locals: HashMap::new(), bound: HashSet::new() });
        if let Some(body) = node.child_by_field_name("body") {
            self.visit(body);
        }
        self.scopes.pop();
    }

    fn func_decl(&mut self, node: Node, kind: SymbolKind) {
        let name = match kind {
            SymbolKind::Init => "init".to_string(),
            SymbolKind::Subscript => "subscript".to_string(),
            _ => match node.child_by_field_name("name") {
                Some(n) => self.text(n).to_string(),
                None => return self.visit_children(node),
            },
        };
        let in_type = self.scopes.last().is_some_and(|s| s.type_name.is_some());
        let kind = match kind {
            SymbolKind::Function if in_type => SymbolKind::Method,
            k => k,
        };
        let container = if in_type { self.enclosing_type() } else { None };
        let sym = self.push_symbol(node, name, kind, container.clone());
        // A function's type_name is its return type (`Self` resolved to the enclosing type).
        self.facts.symbols[sym].type_name = node
            .child_by_field_name("return_type")
            .and_then(|t| {
                if self.text(t) == "Self" {
                    container.as_deref().map(|c| TypeExpr::Named(last_component(c).to_string()))
                } else {
                    type_expr(self.src, t)
                }
            })
            .map(|t| t.encode());
        let attrs = &self.facts.symbols[sym].attrs;
        if attrs.iter().any(|a| a == "Test") || self.parent_is_test() && self.facts.symbols[sym].name.starts_with("test")
        {
            self.facts.symbols[sym].tags.push("test");
        }
        let mut locals = HashMap::new();
        for param in children(node).filter(|c| c.kind() == "parameter") {
            if let Some((name, ty)) = self.parameter_binding(param) {
                locals.insert(name, ty);
            }
        }
        let bound: HashSet<String> = children(node)
            .filter(|c| c.kind() == "parameter")
            .flat_map(|p| {
                children(p)
                    .enumerate()
                    .filter(|(i, c)| c.kind() == "simple_identifier" && p.field_name_for_child(*i as u32) == Some("name"))
                    .map(|(_, c)| self.text(c).to_string())
                    .collect::<Vec<_>>()
            })
            .collect();
        self.scopes.push(Scope { sym, type_name: None, callable: true, locals, bound });
        for child in children(node) {
            self.visit(child);
        }
        self.scopes.pop();
    }

    /// `label name: Type` gives (`name`, its type).
    fn parameter_binding(&self, param: Node) -> Option<(String, TypeExpr)> {
        let mut name = None;
        let mut ty = None;
        for (i, child) in children(param).enumerate() {
            if param.field_name_for_child(i as u32) != Some("name") {
                continue;
            }
            match child.kind() {
                "simple_identifier" => name = Some(self.text(child).to_string()),
                _ => ty = type_expr(self.src, child),
            }
        }
        Some((name?, ty?))
    }

    /// Each name a property or local declaration binds, with the type it is declared or initialized with.
    /// `let a = A(), b = B()` binds a to A and b to B.
    fn bindings<'t>(&self, decl: Node<'t>) -> Vec<(Node<'t>, Option<TypeExpr>)> {
        let mut out: Vec<(Node, Option<TypeExpr>, bool)> = Vec::new(); // (identifier, type, annotated)
        for (i, child) in children(decl).enumerate() {
            let field = decl.field_name_for_child(i as u32);
            match (child.kind(), field) {
                ("pattern", Some("name")) => {
                    if let Some(ident) =
                        child.child_by_field_name("bound_identifier").or_else(|| first_named_of(child, "simple_identifier"))
                    {
                        out.push((ident, None, false));
                    }
                }
                ("type_annotation", _) => {
                    if let Some(last) = out.last_mut() {
                        last.1 = children(child).find(|c| c.is_named()).and_then(|t| type_expr(self.src, t));
                        last.2 = last.1.is_some();
                    }
                }
                (_, Some("value")) => {
                    if let Some(last) = out.last_mut()
                        && !last.2
                    {
                        last.1 = self.expr_type(child);
                    }
                }
                _ => {}
            }
        }
        out.into_iter().map(|(n, t, _)| (n, t)).collect()
    }

    fn local(&self, name: &str) -> Option<TypeExpr> {
        self.scopes.iter().rev().take_while(|s| s.callable).find_map(|s| s.locals.get(name).cloned())
    }

    fn bind_local(&mut self, name: String, ty: TypeExpr) {
        if let Some(scope) = self.scopes.iter_mut().rev().find(|s| s.callable) {
            scope.bound.insert(name.clone());
            scope.locals.insert(name, ty);
        }
    }

    fn bind(&mut self, name: String) {
        if let Some(scope) = self.scopes.iter_mut().rev().find(|s| s.callable) {
            scope.bound.insert(name);
        }
    }

    fn is_bound(&self, name: &str) -> bool {
        self.scopes.iter().rev().take_while(|s| s.callable).any(|s| s.bound.contains(name) || s.locals.contains_key(name))
    }

    /// Binds every identifier a pattern introduces, and visits expressions inside it (`case .a(x + 1)` is rare,
    /// `if let x = compute()` keeps its value outside the pattern node).
    fn bind_pattern(&mut self, node: Node) {
        let mut stack = vec![node];
        while let Some(n) = stack.pop() {
            for (i, c) in children(n).enumerate() {
                let field = n.field_name_for_child(i as u32);
                match c.kind() {
                    "simple_identifier" if field != Some("suffix") => self.bind(self.text(c).to_string()),
                    "navigation_expression" | "call_expression" | "prefix_expression" => self.visit(c),
                    _ => stack.push(c),
                }
            }
        }
    }

    fn bind_lambda_params(&mut self, lambda: Node) {
        let names: Vec<String> = lambda
            .child_by_field_name("type")
            .and_then(|t| first_named_of(t, "lambda_function_type_parameters"))
            .map(|ps| {
                children(ps)
                    .filter(|p| p.kind() == "lambda_parameter")
                    .filter_map(|p| p.child_by_field_name("name"))
                    .map(|n| self.text(n).to_string())
                    .collect()
            })
            .unwrap_or_default();
        for n in names {
            self.bind(n);
        }
    }

    /// A bare identifier read inside a body that isn't a local: a member of the enclosing type (or a global).
    /// Recorded once per enclosing symbol and name, to keep the index small.
    fn bare_identifier(&mut self, node: Node) {
        let name = self.text(node);
        let field = node.parent().and_then(|p| {
            let i = children(p).position(|c| c.id() == node.id())?;
            p.field_name_for_child(i as u32)
        });
        match field {
            // `if let x = …`, `guard let x = …`: a binding.
            Some("bound_identifier") => return self.bind(name.to_string()),
            // A declaration's own name.
            Some("name") => return,
            _ => {}
        }
        let parent = node.parent().map(|p| p.kind()).unwrap_or("");
        let skip_parent = matches!(
            parent,
            "value_argument_label" | "parameter" | "lambda_parameter" | "navigation_suffix" | "statement_label"
                | "availability_condition" | "enum_entry" | "import_declaration" | "identifier" | "directive"
                | "macro_invocation" | "key_path_string_expression" | "selector_expression"
        );
        if skip_parent
            || !name.chars().all(|c| c.is_alphanumeric() || c == '_')
            || starts_upper(name)
            || name.starts_with('$')
            || matches!(name, "newValue" | "oldValue" | "error" | "_" | "super")
            || self.is_bound(name)
        {
            return;
        }
        let from = self.current_sym();
        if self.seen_bare.insert((from, name.to_string())) {
            self.add_ref(name.to_string(), None, RefKind::Member, node);
        }
    }

    /// What can be said syntactically about an expression's type: a nominal type, a call whose return type is
    /// looked up later, or a member chain from a local, `self`, a property or a type.
    fn expr_type(&self, expr: Node) -> Option<TypeExpr> {
        match expr.kind() {
            "try_expression" | "await_expression" | "tuple_expression" => {
                let named: Vec<Node> = children(expr).filter(|c| c.is_named()).collect();
                if expr.kind() == "tuple_expression" && named.len() != 1 {
                    return None;
                }
                named.last().and_then(|e| self.expr_type(*e))
            }
            "postfix_expression" => self.expr_type(expr.child_by_field_name("target")?),
            "self_expression" => Some(TypeExpr::root(PathRoot::SelfType)),
            "simple_identifier" => {
                let name = self.text(expr);
                if let Some(t) = self.local(name) {
                    Some(t)
                } else if starts_upper(name) {
                    Some(TypeExpr::root(PathRoot::Static(name.to_string())))
                } else {
                    Some(TypeExpr::root(PathRoot::Prop(name.to_string())))
                }
            }
            "constructor_expression" => {
                let ty = expr.child_by_field_name("constructed_type")?;
                type_expr(self.src, ty)
            }
            "navigation_expression" => {
                let suffix = expr.child_by_field_name("suffix")?.child_by_field_name("suffix")?;
                if suffix.kind() != "simple_identifier" {
                    return None;
                }
                let base = self.expr_type(expr.child_by_field_name("target")?)?;
                Some(base.then(Seg::Member(self.text(suffix).to_string())))
            }
            "call_expression" => {
                let callee = expr.child(0)?;
                if is_subscript(expr) {
                    return Some(self.expr_type(callee)?.then(Seg::Index));
                }
                match callee.kind() {
                    "simple_identifier" => {
                        let name = self.text(callee).to_string();
                        if name == "XCTUnwrap" {
                            // `try XCTUnwrap(x)` has x's (unwrapped) type.
                            let arg = children(children(expr).find(|c| c.kind() == "call_suffix")?)
                                .find(|c| c.kind() == "value_arguments")
                                .and_then(|a| children(a).find(|c| c.kind() == "value_argument"))
                                .and_then(|a| a.child_by_field_name("value"))?;
                            self.expr_type(arg)
                        } else if starts_upper(&name) {
                            Some(TypeExpr::Named(name))
                        } else {
                            Some(TypeExpr::Call { owner: None, name })
                        }
                    }
                    "navigation_expression" => {
                        let suffix = callee.child_by_field_name("suffix")?.child_by_field_name("suffix")?;
                        let target = callee.child_by_field_name("target")?;
                        let name = self.text(suffix).to_string();
                        let target_text = collapse(self.text(target), 120);
                        let static_target = starts_upper(&target_text) && !target_text.contains(['(', ' ', '[']);
                        if static_target && name == "init" {
                            Some(TypeExpr::Named(last_component(&target_text).to_string()))
                        } else if static_target && target.kind() == "simple_identifier" {
                            Some(TypeExpr::Call { owner: Some(target_text), name })
                        } else {
                            Some(self.expr_type(target)?.then(Seg::Call(name)))
                        }
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn parent_is_test(&self) -> bool {
        self.current_sym().is_some_and(|p| self.facts.symbols[p].tags.contains(&"test"))
    }

    fn property_decl(&mut self, node: Node) {
        let in_type = self.scopes.last().is_some_and(|s| s.type_name.is_some());
        let container = if in_type { self.enclosing_type() } else { None };
        let mut first = None;
        for (ident, ty) in self.bindings(node) {
            let name = self.text(ident).to_string();
            let sym = self.push_symbol(node, name, SymbolKind::Property, container.clone());
            self.facts.symbols[sym].type_name = ty.map(|t| t.encode());
            first.get_or_insert(sym);
        }
        let Some(sym) = first else { return self.visit_children(node) };
        // Calls inside initial values and computed bodies (a SwiftUI `body`, say) belong to the property.
        self.scopes.push(Scope { sym, type_name: None, callable: true, locals: HashMap::new(), bound: HashSet::new() });
        for child in children(node) {
            if !matches!(child.kind(), "pattern" | "value_binding_pattern") {
                self.visit(child);
            }
        }
        self.scopes.pop();
    }

    /// `let x: T = ...` or `let x = T(...)` inside a body: remember x's type for receiver typing.
    fn local_binding(&mut self, node: Node) {
        for (ident, ty) in self.bindings(node) {
            let name = self.text(ident).to_string();
            match ty {
                Some(ty) => self.bind_local(name, ty),
                None => self.bind(name),
            }
        }
    }

    fn enum_entry(&mut self, node: Node) {
        let container = self.enclosing_type();
        for (i, child) in children(node).enumerate() {
            if node.field_name_for_child(i as u32) == Some("name") {
                let name = self.text(child).to_string();
                let sym = self.push_symbol(node, name, SymbolKind::Case, container.clone());
                self.facts.symbols[sym].start_line = line(child);
            } else {
                self.visit(child);
            }
        }
    }

    fn typealias_decl(&mut self, node: Node) {
        let Some(n) = node.child_by_field_name("name") else { return };
        let name = type_name(self.src, n);
        let sym = self.push_symbol(node, name, SymbolKind::Typealias, self.enclosing_type());
        if let Some(v) = node.child_by_field_name("value") {
            self.facts.symbols[sym].type_name = type_expr(self.src, v).map(|t| t.encode());
            self.scopes.push(Scope { sym, type_name: None, callable: false, locals: HashMap::new(), bound: HashSet::new() });
            self.visit(v);
            self.scopes.pop();
        }
    }

    fn import_decl(&mut self, node: Node) {
        if let Some(ident) = first_named_of(node, "identifier") {
            self.facts.imports.push(Import { module: collapse(self.text(ident), 120), line: line(node) });
        }
    }

    fn call(&mut self, node: Node) {
        let Some(callee) = node.child(0) else { return };
        match callee.kind() {
            "simple_identifier" => {
                let name = self.text(callee).to_string();
                let kind = if starts_upper(&name) { RefKind::Instantiate } else { RefKind::Call };
                self.add_ref(name, None, kind, callee);
            }
            "navigation_expression" => {
                let Some(suffix) = callee
                    .child_by_field_name("suffix")
                    .and_then(|s| s.child_by_field_name("suffix"))
                    .filter(|s| s.kind() == "simple_identifier")
                else {
                    return;
                };
                let name = self.text(suffix).to_string();
                let target = callee.child_by_field_name("target");
                if let Some(t) = target
                    && name != "init"
                {
                    self.static_type_ref(t);
                }
                let receiver = target.map(|t| collapse(self.text(t), RECEIVER_CAP));
                let receiver_type = target.and_then(|t| self.expr_type(t));
                if let Some(t) = &receiver_type {
                    self.type_closures(node, &name, t);
                }
                let receiver_type = receiver_type.map(|t| t.encode());
                if name == "init" {
                    // `Foo.init(...)` instantiates Foo; `self.init` and `super.init` delegate.
                    if let Some(r) = receiver.as_deref().filter(|r| starts_upper(r)) {
                        let ty = last_component(r).to_string();
                        self.add_ref(ty, None, RefKind::Instantiate, suffix);
                    } else {
                        self.add_ref(name, receiver, RefKind::Call, suffix);
                    }
                } else {
                    let kind = if starts_upper(&name) { RefKind::Instantiate } else { RefKind::Call };
                    self.add_typed_ref(name, receiver, receiver_type, kind, suffix);
                }
            }
            "prefix_expression" => {
                // Implicit member: `.shared.load()` is handled above; this is `.load()` or `.init()`.
                if let Some(target) = callee.child_by_field_name("target")
                    && target.kind() == "simple_identifier"
                {
                    let name = self.text(target).to_string();
                    if name != "init" {
                        self.add_ref(name, Some(".".into()), RefKind::Call, target);
                    }
                }
            }
            _ => {}
        }
    }

    /// `Type.member`: the type is used too.
    fn static_type_ref(&mut self, target: Node) {
        if target.kind() == "simple_identifier" {
            let name = self.text(target);
            if starts_upper(name) && name != "Self" && !COMMON_TYPES.contains(&name) && self.local(name).is_none() {
                self.add_ref(name.to_string(), None, RefKind::TypeRef, target);
            }
        }
    }

    /// `x.member` used as a value (not called): a member reference, typed like a call's receiver.
    fn member_access(&mut self, node: Node) {
        let Some(suffix) = node.child_by_field_name("suffix").and_then(|s| s.child_by_field_name("suffix")) else {
            return self.visit_children(node);
        };
        let target = node.child_by_field_name("target");
        if let Some(t) = target {
            self.static_type_ref(t);
        }
        if suffix.kind() == "simple_identifier" {
            let name = self.text(suffix).to_string();
            let receiver = target.map(|t| {
                if t.kind() == "key_path_expression" { "\\".to_string() } else { collapse(self.text(t), RECEIVER_CAP) }
            });
            let receiver_type = target.filter(|t| t.kind() != "key_path_expression").and_then(|t| self.expr_type(t)).map(|t| t.encode());
            self.add_typed_ref(name, receiver, receiver_type, RefKind::Member, suffix);
        }
        if let Some(t) = target {
            self.visit(t);
        }
    }

    /// For `xs.filter { $0.ok }` and `xs.forEach { x in ... }`: types the closure's parameters as elements of xs.
    fn type_closures(&mut self, call: Node, method: &str, receiver: &TypeExpr) {
        let pair = PAIR_CLOSURES.contains(&method);
        let reduce = method == "reduce";
        if !pair && !reduce && !ELEMENT_CLOSURES.contains(&method) {
            return;
        }
        let elem = TypeExpr::Elem(Box::new(receiver.clone()));
        let Some(suffix) = children(call).find(|c| c.kind() == "call_suffix") else { return };
        let mut lambdas: Vec<Node> = children(suffix).filter(|c| c.kind() == "lambda_literal").collect();
        if let Some(args) = children(suffix).find(|c| c.kind() == "value_arguments") {
            for arg in children(args).filter(|c| c.kind() == "value_argument") {
                if let Some(v) = arg.child_by_field_name("value").filter(|v| v.kind() == "lambda_literal") {
                    lambdas.push(v);
                }
            }
        }
        for lambda in lambdas {
            let params: Vec<String> = lambda
                .child_by_field_name("type")
                .and_then(|t| first_named_of(t, "lambda_function_type_parameters"))
                .map(|ps| {
                    children(ps)
                        .filter(|p| p.kind() == "lambda_parameter")
                        .filter_map(|p| p.child_by_field_name("name"))
                        .map(|n| self.text(n).to_string())
                        .collect()
                })
                .unwrap_or_default();
            let mut locals = HashMap::new();
            let element_slots: &[usize] = if pair { &[0, 1] } else if reduce { &[1] } else { &[0] };
            for &i in element_slots {
                locals.insert(format!("${i}"), elem.clone());
                if let Some(p) = params.get(i) {
                    locals.insert(p.clone(), elem.clone());
                }
            }
            self.closure_locals.insert(lambda.id(), locals);
        }
    }

    fn add_ref(&mut self, name: String, receiver: Option<String>, kind: RefKind, at: Node) {
        self.add_typed_ref(name, receiver, None, kind, at);
    }

    fn add_typed_ref(&mut self, name: String, receiver: Option<String>, receiver_type: Option<String>, kind: RefKind, at: Node) {
        let from = self.current_sym();
        let (l, col) = (line(at), name_col(self.src, at, &name));
        if self.seen_refs.insert((from, name.clone(), kind, l, col)) {
            self.facts.refs.push(Ref { from, name, receiver, receiver_type, kind, line: l, col, args: None });
        }
    }
}

fn type_tags(kind: SymbolKind, attrs: &[String], targets: &[String]) -> Vec<&'static str> {
    let has_attr = |a: &str| attrs.iter().any(|x| x == a);
    let has_target = |t: &str| targets.iter().any(|x| x == t || x.ends_with(&format!(".{t}")));
    let mut tags = Vec::new();
    if has_attr("main") {
        tags.push("entrypoint");
    }
    if kind != SymbolKind::Protocol && has_target("View") {
        tags.push("swiftui_view");
    }
    if has_attr("Model") {
        tags.push("swiftdata_model");
    }
    if has_attr("Observable") || has_target("ObservableObject") {
        tags.push("observable");
    }
    if has_target("AppIntent") {
        tags.push("app_intent");
    }
    if has_target("XCTestCase") || has_attr("Suite") {
        tags.push("test");
    }
    tags
}

pub(crate) fn children(node: Node) -> impl Iterator<Item = Node> {
    (0..node.child_count()).filter_map(move |i| node.child(i))
}

pub(crate) fn first_named_of<'t>(node: Node<'t>, kind: &str) -> Option<Node<'t>> {
    children(node).find(|c| c.kind() == kind)
}

pub(crate) fn line(node: Node) -> u32 {
    node.start_position().row as u32 + 1
}

/// The 1-based byte column where a reference to `name` recorded at `node` is written: the first leaf on `node`'s
/// first line that spells `name` as a whole word (`Save` in `first.Save`, `x` in `#x`), else where `node` starts. Two
/// paths recording one occurrence through different nodes (the call, its name) agree on it.
pub(crate) fn name_col(src: &str, node: Node, name: &str) -> u32 {
    let row = node.start_position().row;
    let start = node.start_position().column as u32 + 1;
    if name.is_empty() {
        return start;
    }
    let mut c = node.walk();
    loop {
        let n = c.node();
        if n.start_position().row > row {
            return start;
        }
        if n.child_count() == 0 {
            if let Some(i) = word_in(&src[n.byte_range()], name) {
                return (n.start_position().column + i) as u32 + 1;
            }
        } else if c.goto_first_child() {
            continue;
        }
        while !c.goto_next_sibling() {
            if !c.goto_parent() || c.node() == node {
                return start;
            }
        }
    }
}

/// Where `name` first appears in `text` as a whole identifier.
fn word_in(text: &str, name: &str) -> Option<usize> {
    let ident = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    text.match_indices(name)
        .map(|(i, _)| i)
        .find(|&i| !text[..i].chars().next_back().is_some_and(ident) && !text[i + name.len()..].chars().next().is_some_and(ident))
}

/// Dotted type name with generic arguments dropped: `Foo.Bar<Int>` becomes `Foo.Bar`.
fn type_name(src: &str, node: Node) -> String {
    match node.kind() {
        "type_identifier" => src[node.byte_range()].to_string(),
        "user_type" => {
            let parts: Vec<&str> =
                children(node).filter(|c| c.kind() == "type_identifier").map(|c| &src[c.byte_range()]).collect();
            if parts.is_empty() { collapse(&src[node.byte_range()], 120) } else { parts.join(".") }
        }
        _ => collapse(&src[node.byte_range()], 120),
    }
}

/// The type a type node names: `Foo`, `Foo?`, `Module.Foo<T>`, `[T]`, `Array<T>`, `Set<T>`, `[K: V]`.
/// Function and tuple types give `None`.
fn type_expr(src: &str, node: Node) -> Option<TypeExpr> {
    match node.kind() {
        "type_identifier" => Some(TypeExpr::Named(src[node.byte_range()].to_string())),
        "user_type" => {
            let name = type_name(src, node);
            let last = last_component(&name).to_string();
            let args: Vec<TypeExpr> = first_named_of(node, "type_arguments")
                .map(|a| children(a).filter(|c| c.is_named()).filter_map(|c| type_expr(src, c)).collect())
                .unwrap_or_default();
            match (last.as_str(), args.as_slice()) {
                ("Array" | "Set" | "ContiguousArray" | "ArraySlice", [t]) => Some(TypeExpr::Array(Box::new(t.clone()))),
                ("Dictionary", [k, v]) => Some(TypeExpr::Dict(Box::new(k.clone()), Box::new(v.clone()))),
                ("Optional", [t]) => Some(t.clone()),
                _ => Some(TypeExpr::Named(last)),
            }
        }
        // `T?`, `T!`, `any P`, `some P`: the underlying type.
        "optional_type" | "implicitly_unwrapped_type" | "existential_type" | "opaque_type" => {
            children(node).find(|c| c.is_named()).and_then(|c| type_expr(src, c))
        }
        "array_type" => {
            let elem = node.child_by_field_name("name").or_else(|| children(node).find(|c| c.is_named()))?;
            Some(TypeExpr::Array(Box::new(type_expr(src, elem)?)))
        }
        "dictionary_type" => {
            let named: Vec<Node> = children(node).filter(|c| c.is_named()).collect();
            let [k, v] = named.as_slice() else { return None };
            Some(TypeExpr::Dict(Box::new(type_expr(src, *k)?), Box::new(type_expr(src, *v)?)))
        }
        _ => None,
    }
}

/// `xs[i]` parses as a call whose arguments are in brackets.
fn is_subscript(call: Node) -> bool {
    children(call)
        .find(|c| c.kind() == "call_suffix")
        .and_then(|s| children(s).find(|c| c.kind() == "value_arguments"))
        .and_then(|a| a.child(0))
        .is_some_and(|open| open.kind() == "[")
}

pub(crate) fn last_component(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

pub(crate) fn starts_upper(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_uppercase())
}

/// Attribute names on a declaration, from its `modifiers` child and any leading `attribute` children.
fn attributes(src: &str, node: Node) -> Vec<String> {
    let mut out = Vec::new();
    let mut collect = |attr: Node| {
        if let Some(ty) = first_named_of(attr, "user_type") {
            out.push(type_name(src, ty));
        }
    };
    for child in children(node) {
        match child.kind() {
            "attribute" => collect(child),
            "modifiers" => children(child).filter(|c| c.kind() == "attribute").for_each(&mut collect),
            _ => {}
        }
    }
    out
}

/// First line of the `///` or `/** */` comment directly above a declaration.
fn doc_comment(src: &str, node: Node) -> Option<String> {
    let mut lines = Vec::new();
    let mut prev = node.prev_sibling();
    let mut expected_row = node.start_position().row;
    while let Some(p) = prev {
        if !matches!(p.kind(), "comment" | "multiline_comment") || p.end_position().row + 1 < expected_row {
            break;
        }
        let text = &src[p.byte_range()];
        if !(text.starts_with("///") || text.starts_with("/**")) {
            break;
        }
        lines.push(text);
        expected_row = p.start_position().row;
        prev = p.prev_sibling();
    }
    let first = lines.last()?;
    let cleaned = first.trim_start_matches('/').trim_start_matches('*').trim_end_matches("*/").trim();
    (!cleaned.is_empty()).then(|| collapse(cleaned, DOC_CAP))
}

pub(crate) fn collapse(s: &str, cap: usize) -> String {
    let mut out = String::with_capacity(s.len().min(cap + 4));
    for word in s.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
        if out.len() >= cap {
            let mut end = cap;
            while !out.is_char_boundary(end) {
                end -= 1;
            }
            out.truncate(end);
            out.push('…');
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
import SwiftUI
import BubstackKit

/// The app.
@main struct DemoApp: App {
    var body: some Scene { WindowGroup { ContentView() } }
}

@Observable final class Store: NSObject, Codable {
    @Published private(set) var items: [Item] = []
    init(x: Int) { super.init(); load() }
    func load() async throws -> Int {
        let s = Loader.make(1)
        self.items.append(Item(id: 1))
        if s > 0 { helper.run(a: 1) { $0 } }
        return items.count
    }
}

extension Store: Sendable {
    static func make() -> Store { .init(x: 1) }
}

protocol Loading: AnyObject {
    func load() async throws -> Int
    var progress: Double { get }
}

enum Mode { case light, dark(Int) }

struct ContentView: View {
    var body: some View { Text("hi") }
}
"#;

    fn sym<'f>(f: &'f FileFacts, name: &str, kind: SymbolKind) -> &'f Symbol {
        f.symbols.iter().find(|s| s.name == name && s.kind == kind).unwrap_or_else(|| panic!("no {kind:?} {name}"))
    }

    #[test]
    fn extracts_types_members_and_tags() {
        let f = extract(SAMPLE);
        assert_eq!(f.parse_errors, 0);
        let app = sym(&f, "DemoApp", SymbolKind::Struct);
        assert!(app.tags.contains(&"entrypoint"));
        assert_eq!(app.doc.as_deref(), Some("The app."));
        let store = sym(&f, "Store", SymbolKind::Class);
        assert!(store.tags.contains(&"observable"));
        assert!(sym(&f, "ContentView", SymbolKind::Struct).tags.contains(&"swiftui_view"));
        assert_eq!(sym(&f, "items", SymbolKind::Property).attrs, vec!["Published"]);
        assert_eq!(sym(&f, "load", SymbolKind::Method).container.as_deref(), Some("Store"));
        assert_eq!(sym(&f, "make", SymbolKind::Method).container.as_deref(), Some("Store"));
        sym(&f, "Store", SymbolKind::Extension);
        sym(&f, "Loading", SymbolKind::Protocol);
        sym(&f, "progress", SymbolKind::Property);
        sym(&f, "dark", SymbolKind::Case);
        sym(&f, "init", SymbolKind::Init);
        // `let s` is a local, not a property.
        assert!(!f.symbols.iter().any(|s| s.name == "s"));
        assert_eq!(f.imports.iter().map(|i| i.module.as_str()).collect::<Vec<_>>(), ["SwiftUI", "BubstackKit"]);
    }

    #[test]
    fn extracts_calls_and_relations() {
        let f = extract(SAMPLE);
        let load = f.symbols.iter().position(|s| s.name == "load" && s.kind == SymbolKind::Method).unwrap();
        let calls: Vec<_> = f.refs.iter().filter(|r| r.from == Some(load)).map(|r| (r.name.as_str(), r.kind)).collect();
        assert!(calls.contains(&("make", RefKind::Call)));
        assert!(calls.contains(&("append", RefKind::Call)));
        assert!(calls.contains(&("Item", RefKind::Instantiate)));
        assert!(calls.contains(&("run", RefKind::Call)));
        assert_eq!(f.symbols[load].complexity, 2);
        let body_refs: Vec<_> = f.refs.iter().filter(|r| r.name == "ContentView").collect();
        assert_eq!(body_refs.len(), 1);
        assert_eq!(body_refs[0].kind, RefKind::Instantiate);
        let targets: Vec<_> = f.relations.iter().map(|r| r.target.as_str()).collect();
        assert_eq!(targets, ["App", "NSObject", "Codable", "Sendable", "AnyObject", "View"]);
        let make = f.refs.iter().find(|r| r.name == "make").unwrap();
        assert_eq!(make.receiver.as_deref(), Some("Loader"));
    }

    #[test]
    fn types_loops_closures_and_chains() {
        let f = extract(
            r#"
func f(tiles: [Tile], game: Game) {
    let a = Alpha(), b = Beta()
    b.spin()
    for t in tiles { t.spin() }
    tiles.forEach { tile in tile.spin() }
    _ = tiles.sorted { $0.rank < $1.rank }.first?.spin()
    game.factory.tiles[c]!.spin()
}
"#,
        );
        let types: Vec<TypeExpr> =
            f.refs.iter().filter(|r| r.name == "spin").map(|r| TypeExpr::decode(r.receiver_type.as_deref().unwrap()).unwrap()).collect();
        let elem = TypeExpr::Elem(Box::new(TypeExpr::Array(Box::new(TypeExpr::Named("Tile".into())))));
        assert_eq!(types[0], TypeExpr::Named("Beta".into())); // second binding of a multi-binding `let`
        let types = &types[1..];
        assert_eq!(types[0], elem); // for-in
        assert_eq!(types[1], elem); // named closure parameter
        let chain = TypeExpr::root(PathRoot::Expr(Box::new(TypeExpr::Named("Game".into()))))
            .then(Seg::Member("factory".into()))
            .then(Seg::Member("tiles".into()))
            .then(Seg::Index);
        assert_eq!(types[3], chain);
    }

    #[test]
    fn records_bare_member_reads_but_not_locals() {
        let f = extract(
            r#"
struct Rows {
    static let core = [1]
    static let all = core + nav
    var count = 0
    func f(limit: Int, items: [Int]) -> Int {
        let local = 1
        if let first = items.first { _ = first }
        guard let last = items.last else { return count }
        for item in items { _ = item + limit + local + last }
        items.forEach { value in _ = value + count }
        do { try g() } catch { print(error) }
        switch items.count { case let n: _ = n }
        return count + total
    }
}
"#,
        );
        let mut bare: Vec<(String, String)> = f
            .refs
            .iter()
            .filter(|r| r.kind == RefKind::Member && r.receiver.is_none())
            .map(|r| (f.symbols[r.from.unwrap()].name.clone(), r.name.clone()))
            .collect();
        bare.sort();
        assert_eq!(
            bare,
            [("all".into(), "core".into()), ("all".into(), "nav".into()), ("f".into(), "count".into()), ("f".into(), "total".into())]
        );
    }

    #[test]
    fn types_receivers() {
        let f = extract(
            r#"
final class Model {
    let archive: QueueArchive
    private let pairing = PairingArchive(url: .temp)
    func run(store: SettingsStore?, n: Int) throws {
        store?.save()
        let cache = Cache.init(limit: n)
        cache.flush()
        try Loader(url: u).load()
        archive.load()
    }
}
"#,
        );
        let ty = |name: &str| f.refs.iter().find(|r| r.name == name).unwrap().receiver_type.clone();
        assert_eq!(ty("flush").as_deref(), Some("Cache"));
        assert_eq!(ty("save").as_deref(), Some("SettingsStore"));
        // `Loader(...).load()` is typed here; `archive.load()` is a property, typed at query time.
        let loads: Vec<_> = f.refs.iter().filter(|r| r.name == "load").map(|r| r.receiver_type.clone().unwrap()).collect();
        assert_eq!(loads[0], "Loader");
        assert_eq!(TypeExpr::decode(&loads[1]), Some(TypeExpr::root(PathRoot::Prop("archive".into()))));
        let props: Vec<_> =
            f.symbols.iter().filter(|s| s.kind == SymbolKind::Property).map(|s| s.type_name.clone()).collect();
        assert_eq!(props, [Some("QueueArchive".to_string()), Some("PairingArchive".to_string())]);
    }

    #[test]
    fn inherited_types_are_type_references_from_the_declaring_type() {
        let f = extract("final class QueueArchive: BaseArchive, Archiving {}\nprotocol Store: Sendable, Loading {}\nextension Engine: Describing {}\n");
        let refs: Vec<(&str, Option<&str>)> = f
            .refs
            .iter()
            .filter(|r| r.kind == RefKind::TypeRef)
            .map(|r| (r.name.as_str(), r.from.map(|i| f.symbols[i].name.as_str())))
            .collect();
        for (name, from) in [("BaseArchive", "QueueArchive"), ("Archiving", "QueueArchive"), ("Loading", "Store"), ("Describing", "Engine")] {
            assert!(refs.contains(&(name, Some(from))), "{name}: {refs:?}");
        }
    }
}
