//! Code-health biomarkers from syntax, after CodeScene's code health: per function, Bumpy Road, Deep Nesting,
//! Complex Conditional and Brain Method; per file, Large File; per class, Low Cohesion (LCOM4). Each finding says why,
//! with lines. Functions' shapes are recorded at extraction (C# and TypeScript for bumps and conditions; nesting,
//! complexity and length for every language); references come from the index.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use anyhow::Result;
use kc_store::Store;
use serde::{Deserialize, Serialize};

/// Bumped when findings change for the same index, so cached hotspots are recomputed.
pub const VERSION: u32 = 2;

/// A function with at least this many bumps (top-level blocks nesting two or more deep) is a Bumpy Road.
pub const BUMPY_MIN_BUMPS: u32 = 2;
/// Control flow nested at least this deep is Deep Nesting.
pub const DEEP_NESTING: u32 = 4;
/// A branch condition with at least this many `&&`/`||` operators is a Complex Conditional.
pub const COMPLEX_CONDITION_OPS: u32 = 2;
/// A Brain Method is at least this long (lines)...
pub const BRAIN_LINES: u32 = 70;
/// ...this complex (branches + 1)...
pub const BRAIN_COMPLEXITY: u32 = 15;
/// ...nests this deep...
pub const BRAIN_NESTING: u32 = 3;
/// ...and references this many distinct names.
pub const BRAIN_REFERENCES: u32 = 20;
/// A source file of at least this many lines is a Large File.
pub const LARGE_FILE_LINES: u32 = 1000;
/// A class whose methods fall into at least this many groups sharing no field or call is Low Cohesion...
pub const LOW_COHESION_GROUPS: u32 = 3;
/// ...counting classes with at least this many methods.
pub const COHESION_MIN_METHODS: usize = 4;

/// Languages whose functions record bumps and conditions.
const SHAPED_LANGS: &[&str] = &["csharp", "typescript", "tsx", "javascript"];

/// One biomarker on a function, class or file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    /// `bumpy_road`, `deep_nesting`, `complex_conditional`, `brain_method`, `large_file` or `low_cohesion`.
    pub kind: String,
    pub file: String,
    /// The function or class, `Type.member` for members; none for a file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    pub line: u32,
    pub end_line: u32,
    /// The measure: bumps, depth, operators, lines (brain method and large file), or groups.
    pub value: u32,
    pub reason: String,
}

/// A function's or class's row.
struct Sym {
    id: i64,
    file: String,
    lang: String,
    name: String,
    container: Option<String>,
    kind: String,
    parent: Option<i64>,
    start: u32,
    end: u32,
    complexity: u32,
    nesting: u32,
    bumps: Option<String>,
    cond_ops: u32,
    cond_line: Option<u32>,
    signature: String,
    start_byte: usize,
    end_byte: usize,
}

impl Sym {
    fn qualified(&self) -> String {
        match &self.container {
            Some(c) if !matches!(self.kind.as_str(), "class" | "struct") => format!("{c}.{}", self.name),
            _ => self.name.clone(),
        }
    }
}

/// Every finding in the index of `root`, or in one file, sorted by file, line and kind.
pub fn findings(root: &Path, store: &Store, file: Option<&str>) -> Result<Vec<Finding>> {
    let conn = store.conn();
    let langs = kc_extract::Lang::SOURCE.map(|l| format!("'{l}'")).join(", ");
    let filter = if file.is_some() { "AND f.path = ?1" } else { "AND ?1 IS NULL" };
    let mut stmt = conn.prepare(&format!(
        "SELECT s.id, f.path, f.lang, s.name, s.container, s.kind, s.parent_id, s.start_line, s.end_line, s.complexity,
                s.nesting, s.bumps, s.cond_ops, s.cond_line, s.signature, s.start_byte, s.end_byte
         FROM symbols s JOIN files f ON f.id = s.file_id
         WHERE f.lang IN ({langs}) AND s.kind IN ('function', 'method', 'init', 'class', 'struct', 'property') {filter}"
    ))?;
    let syms: Vec<Sym> = stmt
        .query_map([file], |r| {
            Ok(Sym {
                id: r.get(0)?,
                file: r.get(1)?,
                lang: r.get(2)?,
                name: r.get(3)?,
                container: r.get(4)?,
                kind: r.get(5)?,
                parent: r.get(6)?,
                start: r.get(7)?,
                end: r.get(8)?,
                complexity: r.get(9)?,
                nesting: r.get(10)?,
                bumps: r.get(11)?,
                cond_ops: r.get(12)?,
                cond_line: r.get(13)?,
                signature: r.get(14)?,
                start_byte: r.get::<_, i64>(15)? as usize,
                end_byte: r.get::<_, i64>(16)? as usize,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;

    // References per function: distinct names, and the same-class names and receivers it uses (for LCOM4).
    let mut refs: HashMap<i64, HashSet<String>> = HashMap::new();
    let mut own_uses: HashMap<i64, HashSet<String>> = HashMap::new();
    {
        let mut stmt = conn.prepare(&format!(
            "SELECT r.from_id, r.name, r.receiver FROM refs r JOIN files f ON f.id = r.file_id
             WHERE r.from_id IS NOT NULL AND f.lang IN ({langs}) {filter}"
        ))?;
        let rows = stmt.query_map([file], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?)))?;
        for row in rows {
            let (from, name, receiver) = row?;
            let uses = own_uses.entry(from).or_default();
            match receiver.as_deref().map(|r| r.strip_prefix("this.").unwrap_or(r)) {
                None | Some("this") => {
                    uses.insert(name.clone());
                }
                // `_repo.Get()`: the receiver's first segment may be a field of the class.
                Some(r) => {
                    uses.insert(r.split(['.', '?', '(', '[', '!']).next().unwrap_or(r).to_string());
                }
            }
            refs.entry(from).or_default().insert(name);
        }
    }

    let mut out = Vec::new();
    for s in syms.iter().filter(|s| matches!(s.kind.as_str(), "function" | "method" | "init")) {
        let at = |kind: &str, line: u32, end_line: u32, value: u32, reason: String| Finding {
            kind: kind.to_string(),
            file: s.file.clone(),
            symbol: Some(s.qualified()),
            line,
            end_line,
            value,
            reason,
        };
        let q = s.qualified();
        let bumps: Vec<(u32, u32)> = s
            .bumps
            .as_deref()
            .unwrap_or("")
            .split(',')
            .filter_map(|r| r.split_once('-').and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?))))
            .collect();
        if bumps.len() as u32 >= BUMPY_MIN_BUMPS {
            let lines: Vec<String> = bumps.iter().map(|(a, b)| format!("{a}–{b}")).collect();
            out.push(at(
                "bumpy_road",
                s.start,
                s.end,
                bumps.len() as u32,
                format!("{q} has {} bumps (separate blocks nesting 2+ deep), lines {}", bumps.len(), lines.join(", ")),
            ));
        }
        if s.nesting >= DEEP_NESTING {
            out.push(at("deep_nesting", s.start, s.end, s.nesting, format!("{q} nests control flow {} deep, lines {}–{}", s.nesting, s.start, s.end)));
        }
        if s.cond_ops >= COMPLEX_CONDITION_OPS
            && let Some(l) = s.cond_line
        {
            out.push(at(
                "complex_conditional",
                l,
                l,
                s.cond_ops,
                format!("{q} has a condition with {} && / || operators, line {l}", s.cond_ops),
            ));
        }
        let lines = s.end - s.start + 1;
        let distinct = refs.get(&s.id).map_or(0, |r| r.len() as u32);
        if lines >= BRAIN_LINES && s.complexity >= BRAIN_COMPLEXITY && s.nesting >= BRAIN_NESTING && distinct >= BRAIN_REFERENCES {
            out.push(at(
                "brain_method",
                s.start,
                s.end,
                lines,
                format!(
                    "{q} is {lines} lines, complexity {}, nesting {}, and references {distinct} distinct names, lines {}–{}",
                    s.complexity, s.nesting, s.start, s.end
                ),
            ));
        }
    }

    // Large files.
    {
        let mut stmt = conn.prepare(&format!(
            "SELECT f.path, f.lines FROM files f WHERE f.lang IN ({langs}) AND f.lines >= {LARGE_FILE_LINES} {filter}"
        ))?;
        let rows = stmt.query_map([file], |r| Ok((r.get::<_, String>(0)?, r.get::<_, u32>(1)?)))?;
        for row in rows {
            let (path, lines) = row?;
            // A file that is mostly literals (a country list, fixtures) is data, not code to split.
            if std::fs::read_to_string(root.join(&path)).is_ok_and(|t| is_mostly_data(&t)) {
                continue;
            }
            out.push(Finding {
                kind: "large_file".into(),
                file: path,
                symbol: None,
                line: 1,
                end_line: lines,
                value: lines,
                reason: format!("{lines} lines (a Large File is {LARGE_FILE_LINES}+)"),
            });
        }
    }

    // Low cohesion: LCOM4 over each class's methods, linked when they use a common field or one calls the other.
    let mut members: HashMap<i64, Vec<&Sym>> = HashMap::new();
    for s in &syms {
        if let Some(p) = s.parent {
            members.entry(p).or_default().push(s);
        }
    }
    let mut texts: HashMap<String, String> = HashMap::new();
    // Test classes and their fakes are independent methods by design: no cohesion to measure.
    for class in syms.iter().filter(|s| {
        matches!(s.kind.as_str(), "class" | "struct") && SHAPED_LANGS.contains(&s.lang.as_str()) && !kc_store::is_test_path(&s.file)
    }) {
        // A class with a base class shares state through inherited members the index can't see as fields; mocks,
        // fakes and stubs are independent methods by design.
        if has_base_class(&class.signature) || is_double(&class.name, &class.file) {
            continue;
        }
        let Some(m) = members.get(&class.id) else { continue };
        // Static methods share no instance state by design; LCOM4 counts instance methods.
        let methods: Vec<&Sym> = m.iter().copied().filter(|s| s.kind == "method" && !is_static(&s.signature)).collect();
        if methods.len() < COHESION_MIN_METHODS {
            continue;
        }
        let fields: HashSet<&str> = m.iter().filter(|s| s.kind == "property").map(|s| s.name.as_str()).collect();
        if fields.is_empty() {
            continue;
        }
        // Bare field and property reads (`Flags`, `_count`) aren't references; find them in the methods' text.
        let text = texts.entry(class.file.clone()).or_insert_with(|| std::fs::read_to_string(root.join(&class.file)).unwrap_or_default());
        for m in &methods {
            if let Some(body) = text.get(m.start_byte..m.end_byte) {
                let u = own_uses.entry(m.id).or_default();
                let code = strip_strings_and_comments(body);
                u.extend(code.split(|c: char| !(c.is_alphanumeric() || c == '_')).filter(|w| fields.contains(w)).map(str::to_string));
            }
        }
        // A field initialized from another (`chips$ = this._chips.asObservable()`) is the same state.
        let mut same: HashMap<&str, &str> = HashMap::new();
        for f in m.iter().filter(|s| s.kind == "property") {
            let Some(decl) = text.get(f.start_byte..f.end_byte) else { continue };
            let code = strip_strings_and_comments(decl);
            for w in code.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
                if let Some(&wf) = fields.get(w)
                    && wf != f.name
                {
                    let (a, b) = (field_root(&same, &f.name), field_root(&same, wf));
                    if a != b {
                        same.insert(a, b);
                    }
                }
            }
        }
        let canon: HashMap<&str, &str> = fields.iter().map(|&f| (f, field_root(&same, f))).collect();
        let groups = lcom4(&methods, &canon, &own_uses);
        if groups.len() as u32 >= LOW_COHESION_GROUPS {
            let shown: Vec<String> = groups
                .iter()
                .take(4)
                .map(|g| {
                    // Overloads show once.
                    let mut names: Vec<&str> = Vec::new();
                    for &i in g {
                        if !names.contains(&methods[i].name.as_str()) {
                            names.push(&methods[i].name);
                        }
                    }
                    let more = names.len() > 4;
                    names.truncate(4);
                    format!("{{{}{}}}", names.join(", "), if more { ", …" } else { "" })
                })
                .collect();
            out.push(Finding {
                kind: "low_cohesion".into(),
                file: class.file.clone(),
                symbol: Some(class.qualified()),
                line: class.start,
                end_line: class.end,
                value: groups.len() as u32,
                reason: format!(
                    "{} splits into {} groups of methods sharing no field or call (LCOM4): {}{}",
                    class.qualified(),
                    groups.len(),
                    shown.join(" "),
                    if groups.len() > 4 { " …" } else { "" }
                ),
            });
        }
    }
    out.sort_by(|a, b| a.file.cmp(&b.file).then(a.line.cmp(&b.line)).then_with(|| a.kind.cmp(&b.kind)).then_with(|| a.symbol.cmp(&b.symbol)));
    Ok(out)
}

/// `text` with string literals and comments blanked out, so a field's name inside them isn't read as a use.
fn strip_strings_and_comments(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i += 2;
            while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                i += 1;
            }
            i += 2;
        } else if matches!(c, b'"' | b'\'' | b'`') {
            i += 1;
            while i < b.len() && b[i] != c {
                i += if b[i] == b'\\' { 2 } else { 1 };
            }
            i += 1;
            out.push(' ');
        } else {
            // Copy one whole character.
            let len = text[i..].chars().next().map_or(1, char::len_utf8);
            out.push_str(&text[i..i + len]);
            i += len;
            continue;
        }
        out.push(' ');
    }
    out
}

/// Whether at least 80% of a file's non-blank lines hold only literals: strings, numbers, `true`/`false`/`null`,
/// object keys (`name:`) and punctuation.
fn is_mostly_data(text: &str) -> bool {
    let code = strip_strings_and_comments(text);
    let (mut lines, mut data) = (0usize, 0usize);
    for line in code.lines() {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }
        lines += 1;
        let mut rest = l;
        let mut ok = true;
        while !rest.is_empty() {
            let c = rest.chars().next().unwrap();
            if c.is_alphabetic() || c == '_' || c == '$' {
                let end = rest.find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$')).unwrap_or(rest.len());
                let word = &rest[..end];
                rest = rest[end..].trim_start();
                if !(matches!(word, "true" | "false" | "null" | "undefined") || rest.starts_with(':')) {
                    ok = false;
                    break;
                }
            } else {
                rest = &rest[c.len_utf8()..];
            }
        }
        data += ok as usize;
    }
    lines > 0 && data * 5 >= lines * 4
}

/// Whether a class header names a base class: TypeScript `extends`, or a C# base list whose first type isn't an
/// interface (`IName`).
fn has_base_class(signature: &str) -> bool {
    if signature.split_whitespace().any(|w| w == "extends") {
        return true;
    }
    let head = signature.split(" where ").next().unwrap_or(signature);
    let Some((_, bases)) = head.split_once(':') else { return false };
    let first = bases.trim_start().split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.')).next().unwrap_or("");
    let name = first.rsplit('.').next().unwrap_or(first);
    let b = name.as_bytes();
    !(b.len() >= 2 && b[0] == b'I' && b[1].is_ascii_uppercase()) && !name.is_empty()
}

/// Test doubles outside test directories: `*.mock.ts`, classes named `…Mock`, `…Fake`, `…Stub`, `Fake…`.
fn is_double(name: &str, file: &str) -> bool {
    file.contains(".mock.") || file.contains(".stub.") || name.ends_with("Mock") || name.ends_with("Fake") || name.ends_with("Stub") || name.starts_with("Fake")
}

/// Whether a declaration header declares a static member (C# `static`, TypeScript `static`).
fn is_static(signature: &str) -> bool {
    let head = signature.split('(').next().unwrap_or(signature);
    head.split(|c: char| !c.is_alphanumeric()).any(|w| w == "static")
}

/// The connected components of `methods` (indices), linked when two use a common field of `fields` (each mapped to
/// the field it stands for) or one calls the other by name; largest first.
/// The field `f` stands for, following `same` links.
fn field_root<'a>(same: &HashMap<&'a str, &'a str>, f: &'a str) -> &'a str {
    let mut r = f;
    while let Some(&n) = same.get(r) {
        r = n;
    }
    r
}

fn lcom4(methods: &[&Sym], fields: &HashMap<&str, &str>, uses: &HashMap<i64, HashSet<String>>) -> Vec<Vec<usize>> {
    let names: HashMap<&str, Vec<usize>> = methods.iter().enumerate().fold(HashMap::new(), |mut m, (i, s)| {
        m.entry(s.name.as_str()).or_default().push(i);
        m
    });
    let mut parent: Vec<usize> = (0..methods.len()).collect();
    fn find(p: &mut [usize], i: usize) -> usize {
        let mut r = i;
        while p[r] != r {
            r = p[r];
        }
        p[i] = r;
        r
    }
    // Overloads are one method.
    for same in names.values() {
        for &j in &same[1..] {
            let (a, b) = (find(&mut parent, same[0]), find(&mut parent, j));
            parent[a] = b;
        }
    }
    let mut by_field: HashMap<&str, usize> = HashMap::new();
    for (i, m) in methods.iter().enumerate() {
        for u in uses.get(&m.id).into_iter().flatten() {
            if let Some(&f) = fields.get(u.as_str()) {
                match by_field.get(f) {
                    Some(&j) => {
                        let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                        parent[a] = b;
                    }
                    None => {
                        by_field.insert(f, i);
                    }
                }
            }
            for &j in names.get(u.as_str()).into_iter().flatten() {
                let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                parent[a] = b;
            }
        }
    }
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..methods.len() {
        let r = find(&mut parent, i);
        groups.entry(r).or_default().push(i);
    }
    let mut out: Vec<Vec<usize>> = groups.into_values().collect();
    out.sort_by(|a, b| b.len().cmp(&a.len()).then(a[0].cmp(&b[0])));
    out
}

/// Findings per file, counted by kind.
pub fn counts_by_file(findings: &[Finding]) -> HashMap<String, BTreeMap<String, u32>> {
    let mut out: HashMap<String, BTreeMap<String, u32>> = HashMap::new();
    for f in findings {
        *out.entry(f.file.clone()).or_default().entry(f.kind.clone()).or_default() += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_and_comments_hide_names() {
        let code = strip_strings_and_comments("x = account; // account\ny = \"my account\" + 'a\\'ccount' /* account */ + `account`;");
        assert_eq!(code.matches("account").count(), 1, "{code}");
    }

    #[test]
    fn data_files_are_not_code() {
        let data = "export const COUNTRIES = [\n  { code: 'AD', name: 'Andorra', dial: 376 },\n  { code: 'AE', name: 'UAE', dial: 971 },\n  { code: 'AF', name: 'Afghanistan', dial: 93 },\n  { code: 'AG', name: 'Antigua', dial: 1 },\n];\n";
        assert!(is_mostly_data(data));
        assert!(!is_mostly_data("export function f(x) {\n  if (x) {\n    return g(x);\n  }\n  return null;\n}\n"));
    }

    #[test]
    fn a_field_from_another_field_is_the_same_state() {
        let same = HashMap::from([("chips$", "_chips")]);
        assert_eq!(field_root(&same, "chips$"), "_chips");
        assert_eq!(field_root(&same, "_chips"), "_chips");
    }

    #[test]
    fn base_classes_and_doubles() {
        assert!(has_base_class("public class JobController : JobExecutionController"));
        assert!(has_base_class("public class Repo : Base.Repo<T>, IRepo where T : class"));
        assert!(!has_base_class("public class Orders : IOrders, IDisposable"));
        assert!(!has_base_class("public class Orders"));
        assert!(!has_base_class("public class Box<T> where T : new()"));
        assert!(has_base_class("export class A extends B implements C"));
        assert!(!has_base_class("export class A implements C"));
        assert!(is_double("DomainServiceMock", "src/a.ts") && is_double("X", "src/x.mock.ts") && !is_double("Mockery", "src/m.cs"));
        assert!(is_static("public static int Parse(string s)") && !is_static("public int Parse(string staticValue)"));
    }
}
