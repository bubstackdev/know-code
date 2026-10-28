//! A YAML tree with lines, read with yaml-rust2's event parser (which the binary already carries for XcodeGen), for
//! the Ansible and GitLab CI extractors.
//!
//! The events give every node's line. Duplicate keys are kept (Ansible and GitLab take the last); a syntax error keeps
//! what was read before it; `!vault` values read as `<vault>`; an alias is a copy of its anchor's node; a sequence
//! tagged `!reference` (GitLab CI) reads as the string `!reference a b`.
//!
//! Every YAML text KnowCode reads is checked by [`check`] first: at most [`MAX_BYTES`], and its aliases expanding to at
//! most [`MAX_ALIAS_NODES`] nodes in all. Anchors and aliases are ordinary in GitLab CI and compose files, so they are
//! read; but each alias is a copy of its anchor's node, and aliases of aliases nest, so a few hundred bytes could
//! otherwise stand for billions of nodes.

use std::collections::HashMap;

use yaml_rust2::parser::{Event, MarkedEventReceiver, Parser};
use yaml_rust2::scanner::{Marker, TScalarStyle};

#[derive(Debug, Clone)]
pub(crate) enum Val {
    Str(String),
    Map(Vec<(String, u32, Node)>),
    Seq(Vec<Node>),
    Null,
}

#[derive(Debug, Clone)]
pub(crate) struct Node {
    pub(crate) v: Val,
    pub(crate) line: u32,
    pub(crate) end: u32,
}

impl Node {
    pub(crate) fn str(&self) -> Option<&str> {
        match &self.v {
            Val::Str(s) => Some(s),
            _ => None,
        }
    }

    pub(crate) fn get(&self, key: &str) -> Option<&Node> {
        match &self.v {
            Val::Map(m) => m.iter().rev().find(|(k, ..)| k == key).map(|(_, _, n)| n),
            _ => None,
        }
    }

    pub(crate) fn map(&self) -> &[(String, u32, Node)] {
        match &self.v {
            Val::Map(m) => m,
            _ => &[],
        }
    }

    pub(crate) fn seq(&self) -> &[Node] {
        match &self.v {
            Val::Seq(s) => s,
            _ => &[],
        }
    }

    /// The strings of a scalar or a list of scalars.
    pub(crate) fn strings(&self) -> Vec<(&str, u32)> {
        match &self.v {
            Val::Str(s) => vec![(s.as_str(), self.line)],
            Val::Seq(items) => items.iter().filter_map(|i| i.str().map(|s| (s, i.line))).collect(),
            _ => Vec::new(),
        }
    }

    /// Every string scalar below, with its line.
    pub(crate) fn walk_strings<'a>(&'a self, out: &mut Vec<(&'a str, u32)>) {
        match &self.v {
            Val::Str(s) => out.push((s, self.line)),
            Val::Map(m) => {
                for (_, _, v) in m {
                    v.walk_strings(out);
                }
            }
            Val::Seq(items) => {
                for i in items {
                    i.walk_strings(out);
                }
            }
            Val::Null => {}
        }
    }
}

enum Frame {
    Map { line: u32, items: Vec<(String, u32, Node)>, key: Option<(String, u32)>, anchor: usize },
    Seq { line: u32, items: Vec<Node>, anchor: usize, reference: bool },
}

#[derive(Default)]
struct Builder {
    stack: Vec<Frame>,
    docs: Vec<Node>,
    anchors: HashMap<usize, Node>,
}

impl Builder {
    fn push(&mut self, node: Node) {
        match self.stack.last_mut() {
            None => self.docs.push(node),
            Some(Frame::Seq { items, .. }) => items.push(node),
            Some(Frame::Map { items, key, .. }) => match key.take() {
                None => {
                    // A key: only scalars are keys here; complex keys are kept as their text.
                    let text = match &node.v {
                        Val::Str(s) => s.clone(),
                        _ => String::new(),
                    };
                    *key = Some((text, node.line));
                }
                Some((k, line)) => items.push((k, line, node)),
            },
        }
    }

    fn anchored(&mut self, anchor: usize, node: &Node) {
        if anchor > 0 {
            self.anchors.insert(anchor, node.clone());
        }
    }

    /// Closes open collections (after a syntax error), keeping what was read.
    fn finish(mut self) -> Vec<Node> {
        while let Some(f) = self.stack.pop() {
            let (node, _) = close(f);
            self.push(node);
        }
        self.docs
    }
}

fn close(f: Frame) -> (Node, usize) {
    match f {
        Frame::Map { line, items, anchor, .. } => {
            let end = items.iter().map(|(_, _, n)| n.end).max().unwrap_or(line);
            (Node { v: Val::Map(items), line, end }, anchor)
        }
        Frame::Seq { line, items, anchor, reference } => {
            let end = items.iter().map(|n| n.end).max().unwrap_or(line);
            if reference {
                let parts: Vec<&str> = items.iter().filter_map(Node::str).collect();
                return (Node { v: Val::Str(format!("!reference {}", parts.join(" "))), line, end }, anchor);
            }
            (Node { v: Val::Seq(items), line, end }, anchor)
        }
    }
}

impl MarkedEventReceiver for Builder {
    fn on_event(&mut self, ev: Event, mark: Marker) {
        let line = mark.line() as u32;
        match ev {
            Event::Scalar(s, style, anchor, tag) => {
                let vault = tag.as_ref().is_some_and(|t| t.suffix == "vault");
                let end = line + if matches!(style, TScalarStyle::Literal | TScalarStyle::Folded) { s.matches('\n').count() as u32 } else { 0 };
                let null = style == TScalarStyle::Plain && matches!(s.as_str(), "~" | "null" | "");
                let v = if vault {
                    Val::Str("<vault>".into())
                } else if null {
                    Val::Null
                } else {
                    Val::Str(s)
                };
                let node = Node { v, line, end };
                self.anchored(anchor, &node);
                self.push(node);
            }
            Event::SequenceStart(anchor, tag) => {
                let reference = tag.as_ref().is_some_and(|t| t.suffix == "reference");
                self.stack.push(Frame::Seq { line, items: Vec::new(), anchor, reference })
            }
            Event::MappingStart(anchor, _) => self.stack.push(Frame::Map { line, items: Vec::new(), key: None, anchor }),
            Event::SequenceEnd | Event::MappingEnd => {
                if let Some(f) = self.stack.pop() {
                    let (node, anchor) = close(f);
                    self.anchored(anchor, &node);
                    self.push(node);
                }
            }
            Event::Alias(id) => {
                let node = match self.anchors.get(&id) {
                    Some(n) => Node { v: n.v.clone(), line, end: line },
                    None => Node { v: Val::Null, line, end: line },
                };
                self.push(node);
            }
            _ => {}
        }
    }
}

/// The largest YAML text read, in bytes.
pub const MAX_BYTES: usize = 2 << 20;

/// The most nodes a YAML text's aliases may stand for in all, each alias counted as the size of its anchor's node.
/// Real files stay far below it: an anchor of a few dozen nodes used a few dozen times.
pub const MAX_ALIAS_NODES: u64 = 100_000;

/// Sizes the nodes of a YAML text, in its events, without building them: each alias as its anchor's node's size.
#[derive(Default)]
struct Sizer {
    /// Open collections: their anchor and size so far.
    open: Vec<(usize, u64)>,
    anchors: HashMap<usize, u64>,
    expanded: u64,
}

impl Sizer {
    fn add(&mut self, anchor: usize, size: u64) {
        if anchor > 0 {
            self.anchors.insert(anchor, size);
        }
        if let Some((_, n)) = self.open.last_mut() {
            *n = n.saturating_add(size);
        }
    }
}

impl MarkedEventReceiver for Sizer {
    fn on_event(&mut self, ev: Event, _: Marker) {
        if self.expanded > MAX_ALIAS_NODES {
            return;
        }
        match ev {
            Event::Scalar(_, _, anchor, _) => self.add(anchor, 1),
            Event::SequenceStart(anchor, _) | Event::MappingStart(anchor, _) => self.open.push((anchor, 1)),
            Event::SequenceEnd | Event::MappingEnd => {
                if let Some((anchor, size)) = self.open.pop() {
                    self.add(anchor, size);
                }
            }
            Event::Alias(id) => {
                let size = self.anchors.get(&id).copied().unwrap_or(1);
                self.expanded = self.expanded.saturating_add(size);
                self.add(0, size);
            }
            _ => {}
        }
    }
}

/// Whether KnowCode reads the YAML text `src`: refused (saying why) when it is over [`MAX_BYTES`] or its aliases stand
/// for over [`MAX_ALIAS_NODES`] nodes. A syntax error isn't refused here; the reader says what it kept.
pub fn check(src: &str) -> Result<(), String> {
    if src.len() > MAX_BYTES {
        return Err(format!("larger than {} MiB", MAX_BYTES >> 20));
    }
    let mut sizer = Sizer::default();
    let _ = Parser::new_from_str(src).load(&mut sizer, true);
    if sizer.expanded > MAX_ALIAS_NODES {
        return Err(format!("its aliases stand for more than {MAX_ALIAS_NODES} nodes"));
    }
    Ok(())
}

/// The documents of a YAML text, and whether it had a syntax error (what came before it is kept). A text [`check`]
/// refuses has no documents, and counts as an error.
pub(crate) fn parse(src: &str) -> (Vec<Node>, bool) {
    if check(src).is_err() {
        return (Vec::new(), true);
    }
    let mut b = Builder::default();
    let mut parser = Parser::new_from_str(src);
    let failed = parser.load(&mut b, true).is_err();
    (b.finish(), failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchors_and_references() {
        let (docs, failed) = parse(".base: &base\n  image: x\njob:\n  <<: *base\n  script:\n    - !reference [.setup, script]\n    - make\n");
        assert!(!failed);
        let job = docs[0].get("job").unwrap();
        assert_eq!(job.get("<<").and_then(|b| b.get("image")).and_then(Node::str), Some("x"));
        let script: Vec<&str> = job.get("script").unwrap().strings().into_iter().map(|(s, _)| s).collect();
        assert_eq!(script, ["!reference .setup script", "make"]);
    }

    /// Nested anchors, each a list of ten aliases of the one before: `levels` of them stand for 10^levels nodes.
    fn nested(levels: usize) -> String {
        let mut s = String::from("a0: &a0 [x, x, x, x, x, x, x, x, x, x]\n");
        for i in 1..levels {
            s.push_str(&format!("a{i}: &a{i} [{}]\n", vec![format!("*a{}", i - 1); 10].join(", ")));
        }
        s
    }

    #[test]
    fn aliases_standing_for_too_many_nodes_are_refused() {
        assert!(check(&nested(3)).is_ok());
        let err = check(&nested(9)).unwrap_err();
        assert!(err.contains("aliases"), "{err}");
        let (docs, failed) = parse(&nested(9));
        assert!(docs.is_empty() && failed);
        let big = format!("k: {}\n", "x".repeat(MAX_BYTES));
        assert!(check(&big).unwrap_err().contains("larger"));
    }

    #[test]
    fn a_gitlab_ci_file_with_anchors_is_read_whole() {
        let src = "\
.defaults: &defaults
  image: ruby:3.3
  tags: [docker]
  before_script: &setup
    - bundle install
    - yarn install
.rules: &on_main
  rules:
    - if: $CI_COMMIT_BRANCH == \"main\"
stages: [test, deploy]
test:
  <<: *defaults
  stage: test
  script:
    - *setup
    - bundle exec rspec
lint:
  <<: *defaults
  stage: test
  script: [bundle exec rubocop]
deploy:
  <<: [*defaults, *on_main]
  stage: deploy
  script: [./deploy.sh]
";
        assert!(check(src).is_ok());
        let (docs, failed) = parse(src);
        assert!(!failed);
        let deploy = docs[0].get("deploy").unwrap();
        assert_eq!(deploy.get("stage").and_then(Node::str), Some("deploy"));
        let lint = docs[0].get("lint").unwrap();
        assert_eq!(lint.get("<<").and_then(|d| d.get("image")).and_then(Node::str), Some("ruby:3.3"));
    }
}
