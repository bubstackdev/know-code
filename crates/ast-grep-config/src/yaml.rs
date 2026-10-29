//! Rule YAML read with yaml-rust2, in place of upstream's serde_yaml (KnowCode's change; see README.md).
//!
//! The text is parsed into nodes (scalars keep their text, style and tag; aliases are replaced by a copy of their
//! anchor's node), and the nodes are deserialized the way serde_yaml 0.9 deserializes events, so a rule means here
//! what it means to the ast-grep command line:
//!
//! - a plain scalar asked for as anything (`deserialize_any`, so untagged enums) resolves as serde_yaml resolves it:
//!   empty, `~` and `null` are null; `true`/`false` in three spellings are booleans; decimal, `0x`, `0o` and `0b`
//!   integers (no leading zeros) are integers; then floats (`.inf`, `.nan`); anything else is a string;
//! - a quoted or block scalar is a string, and so is any scalar asked for as a string;
//! - `!!str`, `!!int`, `!!float`, `!!bool` and `!!null` are honoured; another tag on a scalar, sequence or mapping
//!   names an enum variant, as in serde_yaml;
//! - a single-key mapping is an enum variant with its value (serde_yaml's `singleton_map_recursive`);
//! - `<<` is an ordinary key, since serde_yaml 0.9 merges nothing either.
//!
//! The text is expected to have passed KnowCode's YAML limits first (size, and the nodes aliases stand for), since
//! aliases are expanded here.

use std::collections::HashMap;
use std::fmt;

use serde::de::{self, DeserializeOwned, DeserializeSeed, IntoDeserializer, MapAccess, SeqAccess, Visitor};
use yaml_rust2::parser::{Event, MarkedEventReceiver, Parser, Tag};
use yaml_rust2::scanner::{Marker, TScalarStyle};

/// A YAML error: the text doesn't parse, or doesn't deserialize into what was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(String);

impl fmt::Display for Error {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.0)
  }
}

impl std::error::Error for Error {}

impl de::Error for Error {
  fn custom<T: fmt::Display>(msg: T) -> Self {
    Error(msg.to_string())
  }
}

type Result<T> = std::result::Result<T, Error>;

const CORE: &str = "tag:yaml.org,2002:";

/// The line of the node standing for a text with no document: as in serde_yaml, a struct or map read from it is
/// empty, and anything else reads it as null.
const VOID: usize = usize::MAX;

/// One YAML node.
#[derive(Debug, Clone)]
pub enum Node {
  Scalar { text: String, plain: bool, tag: Option<String>, line: usize },
  Seq { items: Vec<Node>, tag: Option<String>, line: usize },
  Map { entries: Vec<(Node, Node)>, tag: Option<String>, line: usize },
}

impl Node {
  fn line(&self) -> usize {
    match self {
      Node::Scalar { line, .. } | Node::Seq { line, .. } | Node::Map { line, .. } => *line,
    }
  }

  fn tag(&self) -> Option<&str> {
    match self {
      Node::Scalar { tag, .. } | Node::Seq { tag, .. } | Node::Map { tag, .. } => tag.as_deref(),
    }
  }

  /// A tag naming an enum variant: one that isn't one of YAML's core tags (`!Variant`).
  fn variant_tag(&self) -> Option<&str> {
    self.tag().filter(|t| !t.starts_with(CORE)).map(|t| t.strip_prefix('!').filter(|r| !r.is_empty()).unwrap_or(t))
  }

  fn at(&self, e: Error) -> Error {
    Error(format!("{} at line {}", e.0, self.line()))
  }
}

fn tag_name(t: Option<Tag>) -> Option<String> {
  let t = t?;
  Some(if t.handle == "!!" { format!("{CORE}{}", t.suffix) } else { format!("{}{}", t.handle, t.suffix) })
}

/// Builds the documents' nodes from parser events.
#[derive(Default)]
struct Builder {
  docs: Vec<Node>,
  /// Open collections: their node so far, anchor, and for a mapping the key waiting for its value.
  open: Vec<(Node, usize, Option<Node>)>,
  anchors: HashMap<usize, Node>,
  error: Option<Error>,
}

impl Builder {
  fn push(&mut self, node: Node, anchor: usize) {
    if anchor > 0 {
      self.anchors.insert(anchor, node.clone());
    }
    match self.open.last_mut() {
      None => self.docs.push(node),
      Some((Node::Seq { items, .. }, _, _)) => items.push(node),
      Some((Node::Map { entries, .. }, _, key)) => match key.take() {
        None => *key = Some(node),
        Some(k) => entries.push((k, node)),
      },
      Some((Node::Scalar { .. }, _, _)) => unreachable!("a scalar is never open"),
    }
  }
}

impl MarkedEventReceiver for Builder {
  fn on_event(&mut self, ev: Event, mark: Marker) {
    let line = mark.line();
    match ev {
      Event::Scalar(text, style, anchor, tag) => {
        let node = Node::Scalar { text, plain: matches!(style, TScalarStyle::Plain), tag: tag_name(tag), line };
        self.push(node, anchor);
      }
      Event::SequenceStart(anchor, tag) => self.open.push((Node::Seq { items: Vec::new(), tag: tag_name(tag), line }, anchor, None)),
      Event::MappingStart(anchor, tag) => {
        self.open.push((Node::Map { entries: Vec::new(), tag: tag_name(tag), line }, anchor, None))
      }
      Event::SequenceEnd | Event::MappingEnd => {
        if let Some((node, anchor, _)) = self.open.pop() {
          self.push(node, anchor);
        }
      }
      // An alias names an anchor of its own document only, as libyaml has it.
      Event::DocumentStart => self.anchors.clear(),
      Event::Alias(id) => match self.anchors.get(&id).cloned() {
        Some(node) => self.push(node, 0),
        None => self.error = Some(Error(format!("unknown anchor at line {line}"))),
      },
      _ => {}
    }
  }
}

/// The documents of a YAML text.
pub fn documents(src: &str) -> Result<Vec<Node>> {
  let mut b = Builder::default();
  Parser::new_from_str(src).load(&mut b, true).map_err(|e| Error(e.to_string()))?;
  if let Some(e) = b.error {
    return Err(e);
  }
  Ok(b.docs)
}

/// Deserializes a YAML text of one document; an empty text is null.
pub fn from_str<T: DeserializeOwned>(src: &str) -> Result<T> {
  let docs = documents(src)?;
  match docs.len() {
    0 => T::deserialize(Node::Scalar { text: String::new(), plain: true, tag: None, line: VOID }),
    1 => from_node(docs.into_iter().next().expect("one")),
    _ => Err(Error("deserializing from YAML containing more than one document is not supported".into())),
  }
}

pub fn from_node<T: DeserializeOwned>(node: Node) -> Result<T> {
  T::deserialize(node)
}

fn parse_null(s: &str) -> bool {
  matches!(s, "null" | "Null" | "NULL" | "~")
}

fn parse_bool(s: &str) -> Option<bool> {
  match s {
    "true" | "True" | "TRUE" => Some(true),
    "false" | "False" | "FALSE" => Some(false),
    _ => None,
  }
}

/// Leading zeros followed by digits are a string in YAML 1.2.
fn digits_but_not_number(s: &str) -> bool {
  let s = s.strip_prefix(['-', '+']).unwrap_or(s);
  s.len() > 1 && s.starts_with('0') && s[1..].bytes().all(|b| b.is_ascii_digit())
}

fn radix(s: &str) -> Option<(&str, u32)> {
  for (p, r) in [("0x", 16), ("0o", 8), ("0b", 2)] {
    if let Some(rest) = s.strip_prefix(p) {
      return (!rest.starts_with(['+', '-'])).then_some((rest, r));
    }
  }
  None
}

fn parse_unsigned(s: &str) -> Option<u128> {
  let un = s.strip_prefix('+').unwrap_or(s);
  if let Some((rest, r)) = radix(un)
    && let Ok(i) = u128::from_str_radix(rest, r)
  {
    return Some(i);
  }
  if un.starts_with(['+', '-']) || digits_but_not_number(s) {
    return None;
  }
  un.parse().ok()
}

fn parse_negative(s: &str) -> Option<i128> {
  for (p, r) in [("-0x", 16), ("-0o", 8), ("-0b", 2)] {
    if let Some(rest) = s.strip_prefix(p)
      && let Ok(i) = i128::from_str_radix(&format!("-{rest}"), r)
    {
      return Some(i);
    }
  }
  if digits_but_not_number(s) {
    return None;
  }
  s.parse().ok()
}

fn parse_f64(s: &str) -> Option<f64> {
  let un = match s.strip_prefix('+') {
    Some(u) if u.starts_with(['+', '-']) => return None,
    Some(u) => u,
    None => s,
  };
  match un {
    ".inf" | ".Inf" | ".INF" => return Some(f64::INFINITY),
    _ => {}
  }
  match s {
    "-.inf" | "-.Inf" | "-.INF" => return Some(f64::NEG_INFINITY),
    ".nan" | ".NaN" | ".NAN" => return Some(f64::NAN),
    _ => {}
  }
  un.parse::<f64>().ok().filter(|f| f.is_finite())
}

fn visit_int<'de, V: Visitor<'de>>(v: V, s: &str) -> std::result::Result<Result<V::Value>, V> {
  if let Some(i) = parse_unsigned(s) {
    return Ok(match u64::try_from(i) {
      Ok(u) => v.visit_u64(u),
      Err(_) => v.visit_u128(i),
    });
  }
  if let Some(i) = parse_negative(s) {
    return Ok(match i64::try_from(i) {
      Ok(n) => v.visit_i64(n),
      Err(_) => v.visit_i128(i),
    });
  }
  Err(v)
}

/// A plain scalar, resolved as serde_yaml resolves an untagged one.
fn visit_plain<'de, V: Visitor<'de>>(v: V, s: &str) -> Result<V::Value> {
  if s.is_empty() || parse_null(s) {
    return v.visit_unit();
  }
  if let Some(b) = parse_bool(s) {
    return v.visit_bool(b);
  }
  let v = match visit_int(v, s) {
    Ok(r) => return r,
    Err(v) => v,
  };
  if !digits_but_not_number(s)
    && let Some(f) = parse_f64(s)
  {
    return v.visit_f64(f);
  }
  v.visit_str(s)
}

fn visit_scalar<'de, V: Visitor<'de>>(v: V, text: &str, plain: bool, tag: Option<&str>) -> Result<V::Value> {
  match tag.and_then(|t| t.strip_prefix(CORE)) {
    Some("bool") => parse_bool(text).map_or_else(|| Err(Error(format!("invalid value: {text:?}, expected a boolean"))), |b| v.visit_bool(b)),
    Some("int") => visit_int(v, text).unwrap_or_else(|_| Err(Error(format!("invalid value: {text:?}, expected an integer")))),
    Some("float") => parse_f64(text).map_or_else(|| Err(Error(format!("invalid value: {text:?}, expected a float"))), |f| v.visit_f64(f)),
    Some("null") if parse_null(text) => v.visit_unit(),
    Some("null") => Err(Error(format!("invalid value: {text:?}, expected null"))),
    Some(_) => v.visit_str(text),
    None if tag.is_some() && plain => visit_plain(v, text),
    None if plain => visit_plain(v, text),
    None => v.visit_str(text),
  }
}

/// Whether a scalar is read as the core type `expected` names: a plain one, or one tagged with it.
fn plain_or_tagged(plain: bool, tag: Option<&str>, expected: &str) -> bool {
  plain || tag.is_some_and(|t| t.strip_prefix(CORE) == Some(expected))
}

macro_rules! number {
  ($method:ident, $core:literal) => {
    fn $method<V: Visitor<'de>>(self, v: V) -> Result<V::Value> {
      match &self {
        Node::Scalar { text, plain, tag, .. } if plain_or_tagged(*plain, tag.as_deref(), $core) => {
          match visit_int(v, text) {
            Ok(r) => r,
            Err(v) => match parse_f64(text) {
              Some(f) if $core == "float" => v.visit_f64(f),
              _ => Err(self.at(de::Error::invalid_type(de::Unexpected::Str(text), &v))),
            },
          }
        }
        _ => self.deserialize_any(v),
      }
    }
  };
}

impl<'de> de::Deserializer<'de> for Node {
  type Error = Error;

  fn deserialize_any<V: Visitor<'de>>(self, v: V) -> Result<V::Value> {
    if self.variant_tag().is_some() {
      return self.deserialize_enum("", &[], v);
    }
    let line = self.line();
    match self {
      Node::Scalar { text, plain, tag, .. } => visit_scalar(v, &text, plain, tag.as_deref()),
      Node::Seq { items, .. } => v.visit_seq(Seq(items.into_iter())),
      Node::Map { entries, .. } => v.visit_map(Map { entries: entries.into_iter(), value: None }),
    }
    .map_err(|e| if e.0.contains(" at line ") { e } else { Error(format!("{} at line {line}", e.0)) })
  }

  fn deserialize_bool<V: Visitor<'de>>(self, v: V) -> Result<V::Value> {
    match &self {
      Node::Scalar { text, plain, tag, .. } if plain_or_tagged(*plain, tag.as_deref(), "bool") => match parse_bool(text) {
        Some(b) => v.visit_bool(b),
        None => Err(self.at(de::Error::invalid_type(de::Unexpected::Str(text), &v))),
      },
      _ => self.deserialize_any(v),
    }
  }

  number!(deserialize_i8, "int");
  number!(deserialize_i16, "int");
  number!(deserialize_i32, "int");
  number!(deserialize_i64, "int");
  number!(deserialize_i128, "int");
  number!(deserialize_u8, "int");
  number!(deserialize_u16, "int");
  number!(deserialize_u32, "int");
  number!(deserialize_u64, "int");
  number!(deserialize_u128, "int");
  number!(deserialize_f32, "float");
  number!(deserialize_f64, "float");

  fn deserialize_str<V: Visitor<'de>>(self, v: V) -> Result<V::Value> {
    match &self {
      Node::Scalar { text, .. } => v.visit_str(text),
      _ => self.deserialize_any(v),
    }
  }

  fn deserialize_string<V: Visitor<'de>>(self, v: V) -> Result<V::Value> {
    self.deserialize_str(v)
  }

  fn deserialize_char<V: Visitor<'de>>(self, v: V) -> Result<V::Value> {
    self.deserialize_str(v)
  }

  fn deserialize_identifier<V: Visitor<'de>>(self, v: V) -> Result<V::Value> {
    self.deserialize_str(v)
  }

  fn deserialize_option<V: Visitor<'de>>(self, v: V) -> Result<V::Value> {
    match &self {
      Node::Scalar { text, plain, tag, .. }
        if (*plain && tag.is_none() && (text.is_empty() || parse_null(text)))
          || tag.as_deref().is_some_and(|t| t.strip_prefix(CORE) == Some("null")) =>
      {
        v.visit_none()
      }
      _ => v.visit_some(self),
    }
  }

  fn deserialize_enum<V: Visitor<'de>>(self, _name: &'static str, _variants: &'static [&'static str], v: V) -> Result<V::Value> {
    if let Some(tag) = self.variant_tag().map(str::to_string) {
      let untagged = match self {
        Node::Scalar { text, plain, line, .. } => Node::Scalar { text, plain, tag: None, line },
        Node::Seq { items, line, .. } => Node::Seq { items, tag: None, line },
        Node::Map { entries, line, .. } => Node::Map { entries, tag: None, line },
      };
      return v.visit_enum(Enum { variant: tag, value: Some(untagged) });
    }
    let line = self.line();
    match self {
      Node::Scalar { text, .. } => v.visit_enum(Enum { variant: text, value: None }),
      Node::Map { entries, .. } if entries.len() == 1 => {
        let (k, value) = entries.into_iter().next().expect("one");
        let Node::Scalar { text, .. } = k else { return Err(Error(format!("invalid enum variant at line {line}"))) };
        v.visit_enum(Enum { variant: text, value: Some(value) })
      }
      _ => Err(Error(format!("invalid type: expected an enum variant (a string or a mapping of one key) at line {line}"))),
    }
  }

  fn deserialize_map<V: Visitor<'de>>(self, v: V) -> Result<V::Value> {
    if self.line() == VOID {
      return v.visit_map(Map { entries: Vec::new().into_iter(), value: None });
    }
    self.deserialize_any(v)
  }

  fn deserialize_struct<V: Visitor<'de>>(self, _name: &'static str, _fields: &'static [&'static str], v: V) -> Result<V::Value> {
    self.deserialize_map(v)
  }

  fn deserialize_newtype_struct<V: Visitor<'de>>(self, _name: &'static str, v: V) -> Result<V::Value> {
    v.visit_newtype_struct(self)
  }

  fn deserialize_ignored_any<V: Visitor<'de>>(self, v: V) -> Result<V::Value> {
    v.visit_unit()
  }

  serde::forward_to_deserialize_any! {
    bytes byte_buf unit unit_struct seq tuple tuple_struct
  }
}

impl<'de> IntoDeserializer<'de, Error> for Node {
  type Deserializer = Node;
  fn into_deserializer(self) -> Node {
    self
  }
}

struct Seq(std::vec::IntoIter<Node>);

impl<'de> SeqAccess<'de> for Seq {
  type Error = Error;
  fn next_element_seed<T: DeserializeSeed<'de>>(&mut self, seed: T) -> Result<Option<T::Value>> {
    self.0.next().map(|n| seed.deserialize(n)).transpose()
  }
}

struct Map {
  entries: std::vec::IntoIter<(Node, Node)>,
  value: Option<Node>,
}

impl<'de> MapAccess<'de> for Map {
  type Error = Error;
  fn next_key_seed<K: DeserializeSeed<'de>>(&mut self, seed: K) -> Result<Option<K::Value>> {
    let Some((k, v)) = self.entries.next() else { return Ok(None) };
    self.value = Some(v);
    seed.deserialize(k).map(Some)
  }
  fn next_value_seed<T: DeserializeSeed<'de>>(&mut self, seed: T) -> Result<T::Value> {
    seed.deserialize(self.value.take().expect("a value after its key"))
  }
}

struct Enum {
  variant: String,
  value: Option<Node>,
}

impl<'de> de::EnumAccess<'de> for Enum {
  type Error = Error;
  type Variant = Variant;
  fn variant_seed<T: DeserializeSeed<'de>>(self, seed: T) -> Result<(T::Value, Variant)> {
    let name = seed.deserialize(self.variant.into_deserializer())?;
    Ok((name, Variant(self.value)))
  }
}

struct Variant(Option<Node>);

impl<'de> de::VariantAccess<'de> for Variant {
  type Error = Error;
  fn unit_variant(self) -> Result<()> {
    match self.0 {
      None => Ok(()),
      Some(n) => de::Deserialize::deserialize(n),
    }
  }
  fn newtype_variant_seed<T: DeserializeSeed<'de>>(self, seed: T) -> Result<T::Value> {
    match self.0 {
      Some(n) => seed.deserialize(n),
      None => Err(Error("expected a value for the variant".into())),
    }
  }
  fn tuple_variant<V: Visitor<'de>>(self, _len: usize, v: V) -> Result<V::Value> {
    match self.0 {
      Some(n) => de::Deserializer::deserialize_seq(n, v),
      None => Err(Error("expected a sequence for the variant".into())),
    }
  }
  fn struct_variant<V: Visitor<'de>>(self, _fields: &'static [&'static str], v: V) -> Result<V::Value> {
    match self.0 {
      Some(n) => de::Deserializer::deserialize_map(n, v),
      None => Err(Error("expected a mapping for the variant".into())),
    }
  }
}

#[cfg(test)]
mod test {
  use super::*;
  use serde::Deserialize;
  use std::collections::BTreeMap;

  #[derive(Deserialize, Debug, PartialEq)]
  #[serde(untagged)]
  enum Any {
    Null(()),
    Bool(bool),
    U(u64),
    I(i64),
    F(f64),
    S(String),
  }

  #[test]
  fn plain_scalars_resolve_as_serde_yaml_resolves_them() {
    let m: BTreeMap<String, Any> =
      from_str("a: ~\nb: true\nc: 0x1F\nd: -3\ne: 1.5\nf: 012\ng: '12'\nh: .inf\ni: yes\nj: !!str 7\nk:\n").unwrap();
    assert_eq!(m["a"], Any::Null(()));
    assert_eq!(m["b"], Any::Bool(true));
    assert_eq!(m["c"], Any::U(31));
    assert_eq!(m["d"], Any::I(-3));
    assert_eq!(m["e"], Any::F(1.5));
    assert_eq!(m["f"], Any::S("012".into()));
    assert_eq!(m["g"], Any::S("12".into()));
    assert_eq!(m["h"], Any::F(f64::INFINITY));
    assert_eq!(m["i"], Any::S("yes".into()));
    assert_eq!(m["j"], Any::S("7".into()));
    assert_eq!(m["k"], Any::Null(()));
  }

  #[test]
  fn anchors_are_copied_and_merge_keys_stay_keys() {
    let m: BTreeMap<String, BTreeMap<String, String>> = from_str("a: &x {k: v}\nb: *x\n").unwrap();
    assert_eq!(m["b"]["k"], "v");
    // `<<` merges nothing: its value is a mapping, not a string.
    let r: Result<BTreeMap<String, BTreeMap<String, String>>> = from_str("c: {<<: {k: v}}\n");
    assert!(r.is_err());
  }

  #[test]
  fn single_key_maps_and_tags_are_enum_variants() {
    #[derive(Deserialize, Debug, PartialEq)]
    enum E {
      A,
      B(String),
    }
    let v: Vec<E> = from_str("- A\n- {B: x}\n- !B y\n").unwrap();
    assert_eq!(v, [E::A, E::B("x".into()), E::B("y".into())]);
    assert!(from_str::<Vec<String>>("- a\n---\n- b\n").is_err());
  }
}
