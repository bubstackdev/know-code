//! Terraform (HCL `.tf` files) over tree-sitter-hcl.
//!
//! Symbols are named by Terraform's own addresses, the way configuration refers to them:
//!
//! - `resource "aws_s3_bucket" "x"` is `aws_s3_bucket.x` (kind `resource`);
//! - `data "aws_iam_policy" "y"` is `data.aws_iam_policy.y` (`data`);
//! - `module "z"` is `module.z` (`module`), with its `source` and `version` as facts (`tf_source`, `tf_version`);
//! - `variable "v"` is `var.v` (`variable`), its `description` as the doc;
//! - each attribute of a `locals` block is `local.l` (`local`);
//! - `output "o"` is `output.o` (`output`);
//! - `provider "aws"` is `provider.aws`, and with `alias = "east"`, `provider.aws.east` (`provider`).
//!
//! References, from the block (or local) they are written in:
//!
//! - `tf_ref`: an address an expression uses (`var.x`, `local.y`, `aws_s3_bucket.b.arn`, `module.m.out`,
//!   `data.t.n`), wherever it is written: arguments, nested blocks, `count`, `for_each`, string templates, heredocs,
//!   function arguments (`templatefile("…", { arn = aws_kms_key.k.arn })`), `replace_triggered_by`. A resource is
//!   a traversal whose root has an underscore (every resource type is `<provider>_<type>`); names a `for`
//!   expression, a template `for` or a `dynamic` block binds, and `count`, `each`, `self`, `path` and `terraform`,
//!   are never references;
//! - `depends_on`: each address in a `depends_on` list;
//! - `provider`: a resource's or data source's `provider = aws.east`, and the values of a module's `providers` map,
//!   as `provider.aws.east`;
//! - `tf_input`: a module block's argument (not `source`, `version`, `count`, `for_each`, `providers`,
//!   `depends_on`), named `var.<argument>` with the module's address as the receiver: the variable it sets in the
//!   module's configuration;
//! - `tf_output`: `module.m.out` also names `output.out`, with `module.m` as the receiver.
//!
//! Every name resolves within its root configuration, the file's directory (see `kc_store::terraform`).

use std::collections::HashSet;

use tree_sitter::{Node, Parser};

use crate::swift::{children, collapse, line};
use crate::{AnsibleFact, FileFacts, Ref, RefKind, Symbol, SymbolKind, Unsupported};

pub const VERSION: u32 = 3;

const SIGNATURE_CAP: usize = 200;

/// A Terraform configuration file. JSON configuration (`.tf.json`) isn't read.
pub fn is_terraform_path(path: &str) -> bool {
    path.ends_with(".tf")
}

/// Names that start a traversal but never name a block: `count.index`, `each.value`, `self.id`, `path.module`,
/// `terraform.workspace`.
const NOT_ADDRESSES: &[&str] = &["count", "each", "self", "path", "terraform"];

/// A module block's meta-arguments; every other argument sets one of the module's variables.
const MODULE_META: &[&str] = &["source", "version", "count", "for_each", "providers", "depends_on"];

pub fn extract(source: &str) -> FileFacts {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_hcl::LANGUAGE.into()).expect("tree-sitter-hcl ABI matches tree-sitter");
    let Some(tree) = parser.parse(source, None) else {
        return FileFacts { parse_errors: 1, ..Default::default() };
    };
    let mut cx = Cx { src: source, facts: FileFacts::default(), seen: HashSet::new() };
    cx.count_errors(tree.root_node());
    if let Some(body) = children(tree.root_node()).find(|c| c.kind() == "body") {
        for block in children(body).filter(|c| c.kind() == "block") {
            cx.top_block(block);
        }
    }
    cx.facts
}

struct Cx<'a> {
    src: &'a str,
    facts: FileFacts,
    seen: HashSet<(usize, String, RefKind, u32, u32)>,
}

/// The parts of a block: its type, labels (unquoted), and body.
struct Block<'t> {
    kind: String,
    labels: Vec<String>,
    body: Option<Node<'t>>,
}

impl<'a> Cx<'a> {
    fn text(&self, n: Node) -> &'a str {
        &self.src[n.byte_range()]
    }

    fn count_errors(&mut self, node: Node) {
        if node.is_error() || node.is_missing() {
            self.facts.parse_errors += 1;
        }
        if node.has_error() {
            for c in children(node) {
                self.count_errors(c);
            }
        }
    }

    fn block<'t>(&self, node: Node<'t>) -> Block<'t> {
        let mut kind = String::new();
        let mut labels = Vec::new();
        let mut body = None;
        for c in children(node) {
            match c.kind() {
                "identifier" if kind.is_empty() => kind = self.text(c).to_string(),
                "identifier" => labels.push(self.text(c).to_string()),
                "string_lit" => labels.push(self.string(c).unwrap_or_default()),
                "body" => body = Some(c),
                _ => {}
            }
        }
        Block { kind, labels, body }
    }

    /// A string literal's text, or `None` when it interpolates.
    fn string(&self, node: Node) -> Option<String> {
        let mut out = String::new();
        for c in children(node) {
            match c.kind() {
                "template_literal" => out.push_str(self.text(c)),
                "quoted_template_start" | "quoted_template_end" => {}
                _ => return None,
            }
        }
        Some(out)
    }

    /// An expression that is a plain string (`"./modules/x"`), unquoted.
    fn literal(&self, expr: Node) -> Option<String> {
        let mut n = expr;
        loop {
            let named: Vec<Node> = children(n).filter(|c| c.is_named() && c.kind() != "comment").collect();
            match (n.kind(), named.as_slice()) {
                ("string_lit", _) => return self.string(n),
                ("template_expr", [q]) | ("quoted_template", [q]) if q.kind() == "quoted_template" => n = *q,
                ("quoted_template", _) => {
                    let mut out = String::new();
                    for c in children(n) {
                        match c.kind() {
                            "template_literal" => out.push_str(self.text(c)),
                            "quoted_template_start" | "quoted_template_end" => {}
                            _ => return None,
                        }
                    }
                    return Some(out);
                }
                (_, [one]) => n = *one,
                _ => return None,
            }
        }
    }

    fn attributes<'t>(&self, body: Node<'t>) -> impl Iterator<Item = (String, Node<'t>, Node<'t>)> + use<'t, '_> {
        children(body).filter(|c| c.kind() == "attribute").filter_map(|a| {
            let name = children(a).find(|c| c.kind() == "identifier")?;
            let value = children(a).find(|c| c.kind() == "expression")?;
            Some((self.text(name).to_string(), value, a))
        })
    }

    fn symbol(&mut self, node: Node, name: String, kind: SymbolKind, signature: String, doc: Option<String>) -> usize {
        self.facts.symbols.push(Symbol {
            parent: None,
            name,
            kind,
            container: None,
            signature: collapse(&signature, SIGNATURE_CAP),
            doc,
            attrs: Vec::new(),
            tags: Vec::new(),
            start_line: line(node),
            end_line: node.end_position().row as u32 + 1,
            start_byte: node.start_byte() as u32,
            end_byte: node.end_byte() as u32,
            complexity: 0,
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

    fn top_block(&mut self, node: Node) {
        let b = self.block(node);
        let header = self.text(node).split('{').next().unwrap_or("").to_string();
        let description = |cx: &Self| {
            b.body.and_then(|body| {
                cx.attributes(body).find(|(n, ..)| n == "description").and_then(|(_, v, _)| cx.literal(v)).map(|d| collapse(&d, 160))
            })
        };
        let (name, kind) = match (b.kind.as_str(), b.labels.as_slice()) {
            ("resource", [t, n]) => (format!("{t}.{n}"), SymbolKind::Resource),
            ("data", [t, n]) => (format!("data.{t}.{n}"), SymbolKind::Data),
            ("module", [n]) => (format!("module.{n}"), SymbolKind::Module),
            ("variable", [n]) => (format!("var.{n}"), SymbolKind::Variable),
            ("output", [n]) => (format!("output.{n}"), SymbolKind::Output),
            ("provider", [n]) => {
                let alias = b.body.and_then(|body| self.attributes(body).find(|(a, ..)| a == "alias").and_then(|(_, v, _)| self.literal(v)));
                (match alias {
                    Some(a) => format!("provider.{n}.{a}"),
                    None => format!("provider.{n}"),
                }, SymbolKind::Provider)
            }
            ("locals", []) => {
                if let Some(body) = b.body {
                    let locals: Vec<(String, Node, Node)> = self.attributes(body).collect();
                    for (n, value, attr) in locals {
                        let sig = format!("local.{n} = {}", self.text(value));
                        let sym = self.symbol(attr, format!("local.{n}"), SymbolKind::Local, sig, None);
                        self.expr_refs(value, sym, RefKind::TfRef, &[]);
                    }
                }
                return;
            }
            // State moves, imports and removals name addresses, and a check's assertions read them: not read here,
            // so said (`unsupported`): the uses they make of an address aren't counted.
            ("moved" | "import" | "removed" | "check", _) => {
                let construct = format!("{} block: {}", b.kind, collapse(self.text(node), 160));
                self.facts.unsupported.push(Unsupported { area: "terraform", construct, line: line(node) });
                return;
            }
            // `terraform`: settings, nothing to resolve.
            _ => return,
        };
        let doc = if matches!(kind, SymbolKind::Variable | SymbolKind::Output) { description(self) } else { None };
        let sig = match kind {
            SymbolKind::Variable => {
                let typ = b.body.and_then(|body| self.attributes(body).find(|(a, ..)| a == "type").map(|(_, v, _)| self.text(v).to_string()));
                match typ {
                    Some(t) => format!("{} {{ type = {t} }}", header.trim()),
                    None => header.trim().to_string(),
                }
            }
            _ => header.trim().to_string(),
        };
        let sym = self.symbol(node, name.clone(), kind, sig, doc);
        let Some(body) = b.body else { return };
        if kind == SymbolKind::Module {
            for (a, value, attr) in self.attributes(body).collect::<Vec<_>>() {
                match a.as_str() {
                    "source" | "version" => {
                        let v = self.literal(value).unwrap_or_else(|| self.text(value).to_string());
                        let kind = if a == "source" { "tf_source" } else { "tf_version" };
                        self.facts.ansible.push(AnsibleFact { kind, sym: Some(sym), name: name.clone(), value: v, line: line(attr) });
                    }
                    "providers" => self.provider_map(value, sym),
                    _ if MODULE_META.contains(&a.as_str()) => {}
                    _ => self.push(sym, format!("var.{a}"), Some(name.clone()), RefKind::TfInput, attr),
                }
            }
        }
        self.body_refs(body, sym, &[]);
    }

    /// References in a block body: its arguments' expressions and nested blocks (`dynamic` binding its iterator).
    fn body_refs(&mut self, body: Node, sym: usize, bound: &[String]) {
        let module = self.facts.symbols[sym].kind == SymbolKind::Module;
        for c in children(body) {
            match c.kind() {
                "attribute" => {
                    let Some(name) = children(c).find(|n| n.kind() == "identifier") else { continue };
                    let Some(value) = children(c).find(|n| n.kind() == "expression") else { continue };
                    match self.text(name) {
                        "depends_on" => self.expr_refs(value, sym, RefKind::DependsOn, bound),
                        "provider" if !module => self.provider_ref(value, sym),
                        "providers" if module => {}
                        // Attribute names, not references.
                        "ignore_changes" => {}
                        _ => self.expr_refs(value, sym, RefKind::TfRef, bound),
                    }
                }
                "block" => {
                    let b = self.block(c);
                    let Some(inner) = b.body else { continue };
                    if b.kind == "dynamic" {
                        // `dynamic "ingress" { for_each = …  iterator = rule  content { … rule.value … } }`
                        let iterator = self
                            .attributes(inner)
                            .find(|(a, ..)| a == "iterator")
                            .map(|(_, v, _)| self.text(v).trim().to_string())
                            .or_else(|| b.labels.first().cloned());
                        let mut more = bound.to_vec();
                        more.extend(iterator);
                        self.body_refs(inner, sym, &more);
                    } else {
                        self.body_refs(inner, sym, bound);
                    }
                }
                _ => {}
            }
        }
    }

    /// `provider = aws.east`: `provider.aws.east`.
    fn provider_ref(&mut self, value: Node, sym: usize) {
        let t: String = self.text(value).chars().filter(|c| !c.is_whitespace()).collect();
        if !t.is_empty() && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.') {
            self.push(sym, format!("provider.{t}"), None, RefKind::Provider, value);
        }
    }

    /// A module's `providers = { aws = aws.east }`: each value is a provider of this configuration.
    fn provider_map(&mut self, value: Node, sym: usize) {
        let mut stack = vec![value];
        while let Some(n) = stack.pop() {
            if n.kind() == "object_elem" {
                if let Some(v) = n.child_by_field_name("val") {
                    self.provider_ref(v, sym);
                }
                continue;
            }
            stack.extend(children(n));
        }
    }

    /// A reference written at `at` (its own start: names here are addresses, not the text written).
    fn push(&mut self, from: usize, name: String, receiver: Option<String>, kind: RefKind, at: Node) {
        let (line, col) = (line(at), at.start_position().column as u32 + 1);
        if self.seen.insert((from, name.clone(), kind, line, col)) {
            self.facts.refs.push(Ref { from: Some(from), name, receiver, receiver_type: None, kind, line, col, args: None });
        }
    }

    /// The addresses an expression uses, as references of `kind` from `sym`. `bound` are names a `for` expression,
    /// a template `for` or a `dynamic` block binds around it.
    fn expr_refs(&mut self, node: Node, sym: usize, kind: RefKind, bound: &[String]) {
        match node.kind() {
            "for_tuple_expr" | "for_object_expr" | "template_for" => {
                let intro = children(node).find(|c| matches!(c.kind(), "for_intro" | "template_for_start"));
                let mut more = bound.to_vec();
                if let Some(intro) = intro {
                    more.extend(children(intro).filter(|c| c.kind() == "identifier").map(|c| self.text(c).to_string()));
                    // The collection iterated is outside the loop's names.
                    for c in children(intro).filter(|c| c.kind() == "expression") {
                        self.expr_refs(c, sym, kind, bound);
                    }
                }
                for c in children(node).filter(|c| Some(*c) != intro) {
                    self.expr_refs(c, sym, kind, &more);
                }
                return;
            }
            // `{ key = value }`: a bare key is a name, not a reference.
            "object_elem" => {
                if let Some(k) = node.child_by_field_name("key")
                    && !self.text(k).trim_start().starts_with('(')
                {
                    if let Some(v) = node.child_by_field_name("val") {
                        self.expr_refs(v, sym, kind, bound);
                    }
                    return;
                }
            }
            _ => {}
        }
        // A traversal is a `variable_expr` and the `get_attr`, `index` and `splat` siblings after it, inside an
        // `expression` or among an operation's operands. Index expressions in it (`aws_x.y[var.i]`) are visited as
        // children.
        let kids: Vec<Node> = children(node).collect();
        for (i, c) in kids.iter().enumerate() {
            if c.kind() == "variable_expr" {
                // The grammar reads `!local.off` as `(!local).off`: the steps follow the operation.
                let mut steps = kids[i..].to_vec();
                if steps.len() == 1
                    && node.kind() == "unary_operation"
                    && let Some(op) = node.parent()
                {
                    let mut next = op.next_sibling();
                    while let Some(n) = next.filter(|n| matches!(n.kind(), "get_attr" | "index" | "splat")) {
                        steps.push(n);
                        next = n.next_sibling();
                    }
                }
                self.traversal(&steps, sym, kind, bound);
            } else {
                self.expr_refs(*c, sym, kind, bound);
            }
        }
    }

    /// A traversal: `root.attr[index].attr…`, from its `variable_expr` on (siblings past it are ignored).
    fn traversal(&mut self, steps: &[Node], sym: usize, kind: RefKind, bound: &[String]) {
        let mut parts = steps.iter().copied();
        let Some(root) = parts.next().and_then(|v| children(v).find(|c| c.kind() == "identifier")) else { return };
        let root_name = self.text(root);
        if NOT_ADDRESSES.contains(&root_name) || bound.iter().any(|b| b == root_name) {
            return;
        }
        // Attribute names after the root, skipping index steps (`aws_x.y[0].arn`), up to a splat.
        let mut attrs = Vec::new();
        for p in parts {
            match p.kind() {
                "get_attr" => {
                    if let Some(id) = children(p).find(|c| c.kind() == "identifier") {
                        attrs.push(self.text(id).to_string());
                    }
                }
                "index" => {}
                _ => break,
            }
            if attrs.len() == 2 {
                break;
            }
        }
        let at = steps[0];
        let address = match (root_name, attrs.as_slice()) {
            ("var" | "local", [n, ..]) => format!("{root_name}.{n}"),
            ("module", [m, rest @ ..]) => {
                let module = format!("module.{m}");
                if let [out] = rest
                    && kind == RefKind::TfRef
                {
                    self.push(sym, format!("output.{out}"), Some(module.clone()), RefKind::TfOutput, at);
                }
                module
            }
            ("data", [t, n]) => format!("data.{t}.{n}"),
            (t, [n, ..]) if t.contains('_') && t.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') => {
                format!("{t}.{n}")
            }
            _ => return,
        };
        // A local using itself is a cycle Terraform rejects; a resource naming itself is its own `self`.
        if self.facts.symbols[sym].name != address {
            self.push(sym, address, None, kind, at);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refs(f: &FileFacts, from: &str) -> Vec<(String, &'static str, Option<String>)> {
        let i = f.symbols.iter().position(|s| s.name == from).expect(from);
        f.refs.iter().filter(|r| r.from == Some(i)).map(|r| (r.name.clone(), r.kind.as_str(), r.receiver.clone())).collect()
    }

    fn has(f: &FileFacts, from: &str, name: &str, kind: &str) -> bool {
        refs(f, from).iter().any(|(n, k, _)| n == name && *k == kind)
    }

    const SRC: &str = r#"
terraform {
  required_providers {
    aws = { source = "hashicorp/aws" }
  }
}

provider "aws" {
  region = var.region
}

provider "aws" {
  alias  = "east"
  region = "us-east-1"
}

variable "region" {
  type        = string
  description = "Where it runs"
  default     = "us-west-2"
}

variable "names" {}

locals {
  prefix = "${var.region}-app"
  arns   = [for b in aws_s3_bucket.b : b.arn]
  ids    = { for k, v in var.names : k => v.id }
}

data "aws_iam_policy_document" "p" {
  statement {
    resources = ["${aws_s3_bucket.b[0].arn}/*"]
  }
}

resource "aws_s3_bucket" "b" {
  count    = length(var.names)
  bucket   = "${local.prefix}-${count.index}"
  provider = aws.east
  policy   = templatefile("${path.module}/policy.json.tpl", { key = aws_kms_key.k.arn, sub = module.net.subnet_ids[0] })

  dynamic "rule" {
    for_each = local.ids
    content {
      id = rule.value
    }
  }

  lifecycle {
    ignore_changes       = [tags_all.x]
    replace_triggered_by = [aws_kms_key.k.id]
  }

  depends_on = [aws_iam_role.r, data.aws_iam_policy_document.p, module.net]
}

resource "aws_kms_key" "k" {
  for_each = toset(var.names)
  is_enabled = var.region == "PRD" ? !local.off : true
  description = <<-EOT
    key for ${local.prefix}
  EOT
}

module "net" {
  source    = "./modules/net"
  version   = "1.2.0"
  cidr      = var.region
  providers = { aws = aws.east }
}

output "bucket_arn" {
  description = "The bucket"
  value       = aws_s3_bucket.b[*].arn
}
"#;

    #[test]
    fn definitions_are_named_by_address() {
        let f = extract(SRC);
        assert_eq!(f.parse_errors, 0);
        let names: Vec<(&str, &str)> = f.symbols.iter().map(|s| (s.name.as_str(), s.kind.as_str())).collect();
        assert_eq!(
            names,
            [
                ("provider.aws", "provider"),
                ("provider.aws.east", "provider"),
                ("var.region", "variable"),
                ("var.names", "variable"),
                ("local.prefix", "local"),
                ("local.arns", "local"),
                ("local.ids", "local"),
                ("data.aws_iam_policy_document.p", "data"),
                ("aws_s3_bucket.b", "resource"),
                ("aws_kms_key.k", "resource"),
                ("module.net", "module"),
                ("output.bucket_arn", "output"),
            ]
        );
        let region = &f.symbols[2];
        assert_eq!(region.doc.as_deref(), Some("Where it runs"));
        assert_eq!(region.signature, r#"variable "region" { type = string }"#);
        assert_eq!(f.symbols[4].signature, r#"local.prefix = "${var.region}-app""#);
        let facts: Vec<(&str, &str)> = f.ansible.iter().map(|a| (a.kind, a.value.as_str())).collect();
        assert_eq!(facts, [("tf_source", "./modules/net"), ("tf_version", "1.2.0")]);
    }

    #[test]
    fn references_by_kind() {
        let f = extract(SRC);
        let b = refs(&f, "aws_s3_bucket.b");
        // count, template, templatefile arguments, for_each of a dynamic block, replace_triggered_by.
        for name in ["var.names", "local.prefix", "aws_kms_key.k", "module.net", "local.ids"] {
            assert!(has(&f, "aws_s3_bucket.b", name, "tf_ref"), "{name} in {b:?}");
        }
        assert!(b.contains(&("output.subnet_ids".into(), "tf_output", Some("module.net".into()))));
        assert!(has(&f, "aws_s3_bucket.b", "provider.aws.east", "provider"));
        for name in ["aws_iam_role.r", "data.aws_iam_policy_document.p", "module.net"] {
            assert!(has(&f, "aws_s3_bucket.b", name, "depends_on"), "{name}");
        }
        // Not references: count.index, path.module, the dynamic iterator, ignore_changes, object keys.
        assert!(!b.iter().any(|(n, ..)| n.starts_with("rule") || n.starts_with("tags_all") || n == "key" || n.starts_with("count")), "{b:?}");
        // for-expression names are not references; the collection is.
        assert_eq!(refs(&f, "local.arns"), [("aws_s3_bucket.b".into(), "tf_ref", None)]);
        assert_eq!(refs(&f, "local.ids"), [("var.names".into(), "tf_ref", None)]);
        assert!(has(&f, "aws_kms_key.k", "var.names", "tf_ref"));
        assert!(has(&f, "aws_kms_key.k", "var.region", "tf_ref") && has(&f, "aws_kms_key.k", "local.off", "tf_ref"), "operands");
        assert!(has(&f, "aws_kms_key.k", "local.prefix", "tf_ref"), "heredoc template");
        assert!(has(&f, "data.aws_iam_policy_document.p", "aws_s3_bucket.b", "tf_ref"));
        assert!(has(&f, "provider.aws", "var.region", "tf_ref"));
        assert!(has(&f, "output.bucket_arn", "aws_s3_bucket.b", "tf_ref"));
        let net = refs(&f, "module.net");
        assert!(net.contains(&("var.cidr".into(), "tf_input", Some("module.net".into()))), "{net:?}");
        assert!(net.contains(&("var.region".into(), "tf_ref", None)));
        assert!(net.contains(&("provider.aws.east".into(), "provider", None)));
        assert!(!net.iter().any(|(n, ..)| n == "var.source" || n == "var.providers" || n == "var.version"));
    }

    /// A heredoc holding `name = value` lines (a shell script) defines no locals; its interpolations are references.
    #[test]
    fn heredoc_lines_are_not_locals() {
        let f = extract("locals {\n  script = <<SCRIPT\n#!/bin/bash\ntoken=$(curl -s ${var.url})\nhost = x\nSCRIPT\n}\n");
        let names: Vec<&str> = f.symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["local.script"]);
        assert_eq!(refs(&f, "local.script"), [("var.url".into(), "tf_ref", None)]);
    }

    #[test]
    fn a_broken_file_keeps_what_parsed() {
        let f = extract("variable \"a\" {}\nresource \"x_y\" \"z\" {\n  v = var.a\n");
        assert!(f.parse_errors > 0);
        assert!(f.symbols.iter().any(|s| s.name == "var.a"));
    }
}
