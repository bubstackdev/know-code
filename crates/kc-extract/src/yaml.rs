//! A YAML tree with lines, read with yaml-rust2's event parser (which the binary already carries for XcodeGen), for
//! the Ansible and GitLab CI extractors.
//!
//! The events give every node's line. Duplicate keys are kept (Ansible and GitLab take the last); a syntax error keeps
//! what was read before it; `!vault` values read as `<vault>`; an alias is a copy of its anchor's node; a sequence
//! tagged `!reference` (GitLab CI) reads as the string `!reference a b`.

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

/// The documents of a YAML text, and whether it had a syntax error (what came before it is kept).
pub(crate) fn parse(src: &str) -> (Vec<Node>, bool) {
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
}
