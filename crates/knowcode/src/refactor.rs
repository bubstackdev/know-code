//! Refactor-safety previews: dead code (`unused`) and renames (`rename`). Both read the index and the files and
//! report; neither edits anything.
//!
//! Both start from the same two sources: references resolved with a confidence (see [`query`]), and every
//! whole-word occurrence of the names in the repo's text files, classed by where it sits (code, comment, doc
//! comment, string, markup, other text). An occurrence a resolved reference explains adds nothing; one nothing
//! explains (a bare identifier, `nameof`, a string, a template binding) is what makes a symbol only *possibly*
//! unused, or makes a rename site uncertain.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use kc_store::answer::Code;
use kc_store::query::{self, RefHit};
use rusqlite::Connection;
use serde::Serialize;
use serde_json::json;

use crate::ops::{Opened, Out, Scope, envelope, open_fresh, open_member, repo_name, target_repos};

// ---------------------------------------------------------------------------------------------------------------
// Occurrences: a small lexer that finds whole words and says where each sits.

/// Where an occurrence sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Ctx {
    Code,
    Comment,
    /// A doc comment (`///`, `/** */`) or a Markdown file.
    Doc,
    /// A string or character literal (C# interpolations and TypeScript template literals included).
    Str,
    /// HTML, Razor or XML: templates and config.
    Markup,
    /// Any other text file: JSON, YAML, config.
    Text,
}

/// One whole-word occurrence of a name.
#[derive(Debug, Clone)]
struct Occ {
    line: u32,
    /// 1-based byte column.
    col: u32,
    ctx: Ctx,
    /// Preceded by `.` (`x.Name`, `x?.Name`).
    dotted: bool,
    /// The identifier before that `.`, if any (`Type` in `Type.Name`).
    qual: Option<String>,
    /// The whole dotted path before it (`A.B` in `A.B.Name`).
    path: Option<String>,
    /// Followed by `.` (`Type.Member`).
    then_dot: bool,
    /// The whole content of a string literal (`"Name"`).
    whole_string: bool,
    /// Inside `nameof(…)`.
    nameof: bool,
    /// Inside a doc comment's `cref="…"`.
    cref: bool,
    /// On a `using` or `import` line (or inside an `import { … }` list).
    import: bool,
    /// On a `namespace` declaration line.
    namespace: bool,
    /// A member set in an object initializer: `new T { Name = … }`, unqualified though it names `T`'s member.
    init_assign: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FileKind {
    /// C-like syntax: `//` and `/* */` comments, quoted strings.
    CLike,
    Markup,
    Text,
    Doc,
}

fn file_kind(path: &str) -> Option<FileKind> {
    let ext = path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase())?;
    Some(match ext.as_str() {
        "cs" | "ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs" | "java" | "swift" | "rs" | "kt" | "go" | "scss"
        | "less" | "css" => FileKind::CLike,
        "html" | "htm" | "cshtml" | "razor" | "xml" | "config" | "csproj" | "props" | "targets" | "resx" | "xaml" | "svg" => {
            FileKind::Markup
        }
        "md" | "markdown" | "txt" | "rst" => FileKind::Doc,
        "json" | "yml" | "yaml" | "toml" | "ini" | "env" | "sql" | "cfm" | "cfc" | "properties" | "tf" | "sh" | "ps1" => {
            FileKind::Text
        }
        _ => return None,
    })
}

fn ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

/// Whole-word occurrences of `words` in `text`, each with the word it matched.
fn scan(text: &str, kind: FileKind, words: &HashSet<&str>) -> Vec<(String, Occ)> {
    #[derive(PartialEq)]
    enum St {
        Code,
        Line { doc: bool },
        Block { doc: bool },
        Str { q: u8, verbatim: bool, start: usize },
        MComment,
    }
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut st = St::Code;
    let (mut line, mut line_start) = (1u32, 0usize);
    let mut import_line = false;
    let mut import_block = false;
    let mut namespace_line = false;
    let is_namespace = |start: usize| text[start..].trim_start().starts_with("namespace ");
    let mut i = 0;
    let line_flags = |start: usize, block: &mut bool| -> bool {
        let rest = &text[start..];
        let t = rest.trim_start().trim_start_matches('\u{feff}');
        let end = t.find('\n').unwrap_or(t.len());
        let t = &t[..end];
        if *block {
            if t.contains('}') {
                *block = false;
            }
            return true;
        }
        // `using A.B;`, `using static A.B;`, `using X = A.B<C>;`; not a using declaration (`using IScope s = …;`).
        let using = t.trim_start_matches("global ").strip_prefix("using ").is_some_and(|rest| {
            let rest = rest.trim_start_matches("static ").trim_end();
            let Some(body) = rest.strip_suffix(';') else { return false };
            let (lhs, rhs) = body.split_once('=').unwrap_or((body, ""));
            let dotted = |x: &str| x.trim().chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '<' | '>' | ',' | ' '));
            !lhs.trim().contains(' ') && dotted(lhs) && dotted(rhs)
        });
        let import = t.starts_with("import ") || t.starts_with("export * from") || (t.starts_with("export {") && t.contains(" from "));
        if import && t.contains('{') && !t.contains('}') {
            *block = true;
        }
        using || import
    };
    if kind == FileKind::CLike {
        import_line = line_flags(0, &mut import_block);
        namespace_line = is_namespace(0);
    }
    while i < b.len() {
        let c = b[i];
        if c == b'\n' {
            line += 1;
            line_start = i + 1;
            if kind == FileKind::CLike {
                import_line = line_flags(line_start, &mut import_block);
                namespace_line = is_namespace(line_start);
            }
            match st {
                St::Line { .. } => st = St::Code,
                // Only verbatim strings and template literals span lines.
                St::Str { q, verbatim, .. } if !verbatim && q != b'`' => st = St::Code,
                _ => {}
            }
            i += 1;
            continue;
        }
        if kind == FileKind::CLike {
            match st {
                St::Code => {
                    if c == b'/' && b.get(i + 1) == Some(&b'/') {
                        let doc = b.get(i + 2) == Some(&b'/') && b.get(i + 3) != Some(&b'/');
                        st = St::Line { doc };
                        i += 2;
                        continue;
                    }
                    if c == b'/' && b.get(i + 1) == Some(&b'*') {
                        let doc = b.get(i + 2) == Some(&b'*') && b.get(i + 3) != Some(&b'/');
                        st = St::Block { doc };
                        i += 2;
                        continue;
                    }
                    if c == b'"' || c == b'\'' || c == b'`' {
                        let verbatim = c == b'"' && i > 0 && (b[i - 1] == b'@' || (b[i - 1] == b'$' && i > 1 && b[i - 2] == b'@'));
                        st = St::Str { q: c, verbatim, start: i + 1 };
                        i += 1;
                        continue;
                    }
                }
                St::Block { .. } => {
                    if c == b'*' && b.get(i + 1) == Some(&b'/') {
                        st = St::Code;
                        i += 2;
                        continue;
                    }
                }
                St::Str { q, verbatim, .. } => {
                    if c == b'\\' && !verbatim {
                        i += 2;
                        continue;
                    }
                    if c == q {
                        if verbatim && b.get(i + 1) == Some(&q) {
                            i += 2;
                            continue;
                        }
                        st = St::Code;
                        i += 1;
                        continue;
                    }
                }
                _ => {}
            }
        } else if kind == FileKind::Markup {
            if st == St::Code && b[i..].starts_with(b"<!--") {
                st = St::MComment;
                i += 4;
                continue;
            }
            if st == St::MComment && b[i..].starts_with(b"-->") {
                st = St::Code;
                i += 3;
                continue;
            }
        }
        if ident_byte(c) && (i == 0 || !ident_byte(b[i - 1])) {
            let mut j = i;
            while j < b.len() && ident_byte(b[j]) {
                j += 1;
            }
            let word = &text[i..j];
            if !c.is_ascii_digit() && words.contains(word) {
                let ctx = match (&st, kind) {
                    (_, FileKind::Doc) => Ctx::Doc,
                    (_, FileKind::Text) => Ctx::Text,
                    (St::MComment, _) => Ctx::Comment,
                    (_, FileKind::Markup) => Ctx::Markup,
                    (St::Line { doc: true } | St::Block { doc: true }, _) => Ctx::Doc,
                    (St::Line { .. } | St::Block { .. }, _) => Ctx::Comment,
                    (St::Str { .. }, _) => Ctx::Str,
                    _ => Ctx::Code,
                };
                let before = &text[line_start..i];
                let after = &text[j..];
                let trimmed = before.trim_end_matches([' ', '\t']);
                let dotted = trimmed.ends_with('.') && !trimmed.ends_with("..");
                let (qual, path) = if dotted {
                    let q = trimmed[..trimmed.len() - 1].trim_end_matches(['?', '!']);
                    let start = q.rfind(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '$')).map_or(0, |p| p + 1);
                    let pstart = q.rfind(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '$' || ch == '.')).map_or(0, |p| p + 1);
                    (Some(q[start..].to_string()).filter(|s| !s.is_empty()), Some(q[pstart..].trim_matches('.').to_string()).filter(|s| !s.is_empty()))
                } else {
                    (None, None)
                };
                let init_assign = {
                    let a = after.trim_start_matches([' ', '\t']);
                    let b = before.trim_end();
                    (b.is_empty() || b.ends_with('{') || b.ends_with(','))
                        && a.starts_with('=')
                        && !a.starts_with("==")
                        && !a.starts_with("=>")
                };
                let then_dot = {
                    let a = after.trim_start_matches([' ', '\t']);
                    a.starts_with('.') && !a.starts_with("..")
                };
                let whole_string = match st {
                    St::Str { q, start, .. } => start == i && b.get(j) == Some(&q),
                    _ => false,
                };
                // `nameof(A.B.Name)`: back over the qualification to `nameof(`.
                let path_start = before.trim_end_matches(|ch: char| ch.is_ascii_alphanumeric() || ch == '_' || ch == '.');
                let nameof = ctx == Ctx::Code && path_start.trim_end().strip_suffix('(').is_some_and(|p| p.trim_end().ends_with("nameof"));
                let cref_start = before.trim_end_matches(|ch: char| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '{' | '}' | '<' | '>' | ','));
                let cref = matches!(ctx, Ctx::Doc | Ctx::Comment) && (cref_start.ends_with("cref=\"") || cref_start.ends_with("cref='"));
                out.push((
                    word.to_string(),
                    Occ {
                        line,
                        col: (i - line_start + 1) as u32,
                        ctx,
                        dotted,
                        qual,
                        path,
                        then_dot,
                        whole_string,
                        nameof,
                        cref,
                        import: import_line && ctx == Ctx::Code,
                        namespace: namespace_line && ctx == Ctx::Code,
                        init_assign: init_assign && ctx == Ctx::Code,
                    },
                ));
            }
            i = j;
            continue;
        }
        i += 1;
    }
    out
}

/// The text of line `line` (1-based) of `text`, trimmed and capped.
fn line_text(text: &str, line: u32) -> String {
    let l = text.lines().nth(line.saturating_sub(1) as usize).unwrap_or("").trim();
    if l.len() > 160 {
        let mut end = 160;
        while !l.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &l[..end])
    } else {
        l.to_string()
    }
}

/// A C# file's namespace, the namespaces its `using`s import, and those its `global using`s import everywhere.
fn csharp_scopes(text: &str) -> (Option<String>, Vec<String>, Vec<String>) {
    let mut ns = None;
    let (mut usings, mut global) = (Vec::new(), Vec::new());
    for line in text.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("namespace ") {
            if ns.is_none() {
                ns = Some(rest.trim_end_matches(['{', ';', ' ']).trim().to_string());
            }
            continue;
        }
        let (is_global, rest) = match t.strip_prefix("global using ") {
            Some(r) => (true, r),
            None => match t.strip_prefix("using ") {
                Some(r) => (false, r),
                None => continue,
            },
        };
        let Some(name) = rest.strip_suffix(';').map(str::trim) else { continue };
        if name.contains('=') || name.contains('(') || name.contains(' ') && !name.starts_with("static ") {
            continue;
        }
        let name = name.trim_start_matches("static ").to_string();
        if is_global { global.push(name) } else { usings.push(name) }
    }
    (ns, usings, global)
}

/// The names a C# file's `using X = …;` aliases declare.
fn csharp_aliases(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.trim().trim_start_matches("global ").strip_prefix("using "))
        .filter_map(|rest| rest.split_once('='))
        .map(|(lhs, _)| lhs.trim().to_string())
        .filter(|lhs| !lhs.is_empty() && lhs.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        .collect()
}

/// Whether code in a file with namespace `ns` and these imports sees types of namespace `target`: its own
/// namespace or an enclosing one, or an imported one.
fn sees_namespace(ns: Option<&str>, usings: &[String], global: &[String], target: Option<&str>) -> bool {
    let Some(target) = target else { return true };
    let enclosing = ns.is_some_and(|n| n == target || n.starts_with(&format!("{target}.")));
    enclosing || usings.iter().chain(global).any(|u| u == target)
}

/// Every occurrence of `words` in the repo's searchable files, by word: (file, occurrence).
struct Occurrences {
    by_word: HashMap<String, Vec<(usize, Occ)>>,
    files: Vec<String>,
}

impl Occurrences {
    fn scan_repo(root: &Path, paths: &[String], words: &HashSet<&str>) -> Occurrences {
        let mut by_word: HashMap<String, Vec<(usize, Occ)>> = HashMap::new();
        let mut files = Vec::new();
        for path in paths {
            let Some(kind) = file_kind(path) else { continue };
            let Ok(text) = std::fs::read_to_string(root.join(path)) else { continue };
            // For a few words (a rename), most files hold none of them: skip lexing those.
            if words.len() <= 4 && !words.iter().any(|w| text.contains(w)) {
                continue;
            }
            let found = scan(&text, kind, words);
            if found.is_empty() {
                continue;
            }
            files.push(path.clone());
            let fi = files.len() - 1;
            for (w, o) in found {
                by_word.entry(w).or_default().push((fi, o));
            }
        }
        Occurrences { by_word, files }
    }

    fn of(&self, word: &str) -> &[(usize, Occ)] {
        self.by_word.get(word).map_or(&[], Vec::as_slice)
    }
}

/// Files the content index covers (searchable text: not binary, generated or over 1 MB).
fn text_files(o: &mut Opened, no_sync: bool) -> Result<Vec<String>> {
    let files = if no_sync {
        o.store.cfiles()?
    } else {
        kc_index::content::sync_content_files(&o.root, &mut o.store, kc_index::SyncOptions::default().threads)?.1
    };
    Ok(files.into_iter().filter(|f| f.state != kc_store::content::SKIPPED).map(|f| f.path).collect())
}

// ---------------------------------------------------------------------------------------------------------------
// The repo's symbols and type hierarchy.

#[derive(Debug, Clone)]
struct Sym {
    id: i64,
    name: String,
    kind: String,
    container: Option<String>,
    signature: String,
    attrs: Vec<String>,
    tags: String,
    file: String,
    line: u32,
    end_line: u32,
    parent: Option<i64>,
    lang: String,
}

impl Sym {
    fn qualified(&self) -> String {
        match &self.container {
            Some(c) => format!("{c}.{}", self.name),
            None => self.name.clone(),
        }
    }

    fn is_type(&self) -> bool {
        matches!(self.kind.as_str(), "class" | "struct" | "enum" | "protocol" | "actor")
    }

    fn contains(&self, file: &str, line: u32) -> bool {
        self.file == file && self.line <= line && line <= self.end_line
    }

    /// Whether an occurrence is the symbol using itself: inside its body. A one-line symbol (a field, a primary
    /// constructor's parameter) owns no occurrence but its declaration, which the caller skips.
    fn owns(&self, file: &str, o: &Occ) -> bool {
        self.end_line > self.line && self.contains(file, o.line)
    }

    fn csharp(&self) -> bool {
        self.lang == "csharp"
    }

    fn typescript(&self) -> bool {
        matches!(self.lang.as_str(), "typescript" | "tsx" | "javascript")
    }

    /// Declared `public` itself, whatever its type's access: what implementing an interface member takes. Members of
    /// a TypeScript class are public unless marked otherwise.
    fn declared_public(&self) -> bool {
        let sig = format!(" {}", self.signature);
        if self.typescript() {
            return !sig.contains(" private ") && !sig.contains(" protected ") && !self.name.starts_with('#');
        }
        sig.contains(" public ")
    }

    /// Declared `public` or `protected` (C#), or exported and not private (TypeScript); interface members are
    /// public when their interface is.
    fn public(&self, model: &Model) -> bool {
        let sig = format!(" {}", self.signature);
        let parent = self.parent.and_then(|p| model.sym(p));
        if self.csharp() {
            if parent.is_some_and(|p| p.kind == "protocol") {
                return parent.is_some_and(|p| p.public(model));
            }
            let own = sig.contains(" public ") || sig.contains(" protected ");
            return own && parent.is_none_or(|p| p.public(model));
        }
        if self.typescript() {
            if sig.contains(" private ") || sig.contains(" protected ") || self.name.starts_with('#') {
                return false;
            }
            return match parent {
                Some(p) => p.public(model),
                None => sig.contains(" export "),
            };
        }
        false
    }
}

/// One repo's symbols, with lookups by id, name and file, and its type hierarchy as written in base lists.
struct Model {
    syms: Vec<Sym>,
    by_id: HashMap<i64, usize>,
    by_name: HashMap<String, Vec<usize>>,
    /// Qualified type name → its declarations (several for a partial type).
    types: HashMap<String, Vec<usize>>,
    /// Short type name → qualified names.
    short: HashMap<String, Vec<String>>,
    /// Qualified type name → its base types' names as written (generic arguments dropped).
    bases: HashMap<String, Vec<String>>,
    /// File → its type declarations.
    file_types: HashMap<String, Vec<usize>>,
}

impl Model {
    fn load(conn: &Connection) -> Result<Model> {
        let mut stmt = conn.prepare(
            "SELECT s.id, s.name, s.kind, s.container, s.signature, s.attrs, s.tags, f.path, s.start_line, s.end_line,
                    s.parent_id, f.lang
             FROM symbols s JOIN files f ON f.id = s.file_id ORDER BY f.path, s.start_line",
        )?;
        let syms: Vec<Sym> = stmt
            .query_map([], |r| {
                let attrs: String = r.get(5)?;
                Ok(Sym {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    kind: r.get(2)?,
                    container: r.get(3)?,
                    signature: r.get(4)?,
                    attrs: attrs.split(',').filter(|a| !a.is_empty()).map(str::to_string).collect(),
                    tags: r.get(6)?,
                    file: r.get(7)?,
                    line: r.get(8)?,
                    end_line: r.get(9)?,
                    parent: r.get(10)?,
                    lang: r.get(11)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut m = Model {
            by_id: HashMap::new(),
            by_name: HashMap::new(),
            types: HashMap::new(),
            short: HashMap::new(),
            bases: HashMap::new(),
            file_types: HashMap::new(),
            syms,
        };
        for (i, s) in m.syms.iter().enumerate() {
            m.by_id.insert(s.id, i);
            m.by_name.entry(s.name.clone()).or_default().push(i);
            if s.is_type() {
                let q = s.qualified();
                m.types.entry(q.clone()).or_default().push(i);
                let short = m.short.entry(s.name.clone()).or_default();
                if !short.contains(&q) {
                    short.push(q);
                }
                m.file_types.entry(s.file.clone()).or_default().push(i);
            }
        }
        let mut stmt = conn.prepare("SELECT from_id, target_name FROM relations")?;
        let rels: Vec<(i64, String)> = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
        for (from, target) in rels {
            if let Some(s) = m.sym(from) {
                let q = s.qualified();
                let base = target.rsplit('.').next().unwrap_or(&target).to_string();
                let list = m.bases.entry(q).or_default();
                if !list.contains(&base) {
                    list.push(base);
                }
            }
        }
        Ok(m)
    }

    fn sym(&self, id: i64) -> Option<&Sym> {
        self.by_id.get(&id).map(|&i| &self.syms[i])
    }

    fn named(&self, name: &str) -> impl Iterator<Item = &Sym> {
        self.by_name.get(name).into_iter().flatten().map(|&i| &self.syms[i])
    }

    /// The qualified names a base written as `name` may be, in this repo.
    fn resolve_type(&self, name: &str) -> &[String] {
        self.short.get(name).map_or(&[], Vec::as_slice)
    }

    /// `ty`'s supertypes in this repo, transitively (not `ty` itself).
    fn supertypes(&self, ty: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut todo = vec![ty.to_string()];
        while let Some(t) = todo.pop() {
            for b in self.bases.get(&t).into_iter().flatten() {
                for q in self.resolve_type(b) {
                    if q != ty && !out.contains(q) {
                        out.push(q.clone());
                        todo.push(q.clone());
                    }
                }
            }
        }
        out
    }

    /// Base types `ty` (or its supertypes) names that no type in this repo declares: framework or dependency types.
    fn external_bases(&self, ty: &str) -> Vec<String> {
        let mut out = Vec::new();
        for t in std::iter::once(ty.to_string()).chain(self.supertypes(ty)) {
            for b in self.bases.get(&t).into_iter().flatten() {
                if self.resolve_type(b).is_empty() && !out.contains(b) {
                    out.push(b.clone());
                }
            }
        }
        out
    }

    /// Types deriving from `ty`, transitively.
    fn subtypes(&self, ty: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut todo = vec![ty.to_string()];
        while let Some(t) = todo.pop() {
            let short = t.rsplit('.').next().unwrap_or(&t).to_string();
            for (q, bases) in &self.bases {
                if bases.contains(&short) && q != ty && !out.contains(q) && self.supertypes(q).contains(&t) {
                    out.push(q.clone());
                    todo.push(q.clone());
                }
            }
        }
        out
    }

    /// The innermost type declared around `line` of `file`, as members name their container.
    fn enclosing_type(&self, file: &str, line: u32) -> Option<String> {
        self.file_types
            .get(file)?
            .iter()
            .map(|&i| &self.syms[i])
            .filter(|s| s.line <= line && line <= s.end_line)
            .min_by_key(|s| s.end_line - s.line)
            .map(Sym::qualified)
    }

    /// Whether code in type `from` can name a member of `owner` without qualifying it (C#): `from` is `owner`, is
    /// nested in it, or derives from it (or an enclosing type of `from` does).
    fn sees_members_of(&self, from: &str, owner: &str) -> bool {
        let mut cur = Some(from.to_string());
        while let Some(t) = cur {
            if t == owner || self.supertypes(&t).iter().any(|s| s == owner) {
                return true;
            }
            cur = t.rsplit_once('.').map(|(p, _)| p.to_string()).filter(|p| self.types.contains_key(p));
        }
        false
    }

    /// How many types are named `name`: two classes `Order` in different namespaces count twice, the parts of a
    /// partial class once.
    fn type_count(&self, name: &str) -> usize {
        let types: Vec<&Sym> = self.named(name).filter(|t| t.is_type()).collect();
        let partial = types.iter().any(|t| is_partial(t));
        types.iter().filter(|t| !is_partial(t)).count() + partial as usize
    }

    /// Whether an occurrence of member `s`'s name can't mean it: qualified by another type of the repo (not `s`'s
    /// or a subtype of it), or, in C#, `nameof` used unqualified in a type that can't see `s`'s members.
    fn member_elsewhere(&self, s: &Sym, file: &str, o: &Occ) -> bool {
        let Some(owner) = &s.container else { return false };
        match &o.qual {
            Some(q) => {
                let ts = self.resolve_type(q);
                !ts.is_empty() && !ts.iter().any(|t| t == owner || self.supertypes(t).contains(owner))
            }
            None => s.csharp() && o.nameof && self.enclosing_type(file, o.line).is_some_and(|t| !self.sees_members_of(&t, owner)),
        }
    }

    /// Members of `ty` (by container name) named `name`.
    fn members_named(&self, ty: &str, name: &str) -> Vec<&Sym> {
        self.named(name).filter(|s| s.container.as_deref() == Some(ty)).collect()
    }

    /// The line and column of `s`'s own name: its first plain code occurrence inside its span.
    fn decl_site<'o>(&self, s: &Sym, occs: &'o Occurrences) -> Option<(usize, &'o Occ)> {
        let fi = occs.files.iter().position(|f| *f == s.file)?;
        let word = if s.kind == "init" { s.container.as_deref()?.rsplit('.').next()? } else { s.name.as_str() };
        occs.of(word)
            .iter()
            .filter(|(f, o)| *f == fi && o.ctx == Ctx::Code && !o.nameof && !o.dotted && s.line <= o.line && o.line <= s.end_line)
            .map(|(f, o)| (*f, o))
            .next()
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Repo facts the rules need: routes, DI, bus, Entity Framework, Angular templates.

#[derive(Default)]
struct Facts {
    /// (file, start line) of controller actions serving a route.
    actions: HashSet<(String, u32)>,
    /// Types named in DI registrations, as service or implementation.
    di_names: HashSet<String>,
    /// Name filters of assembly-scan registrations (`*Service`).
    di_scans: Vec<String>,
    /// Consumer classes.
    consumers: HashSet<i64>,
    /// Message types published or consumed, by bare name.
    messages: HashSet<String>,
    /// Entity types mapped to tables, and DbSet properties.
    entities: HashSet<String>,
    dbsets: HashSet<i64>,
    /// Component id → its template file, repo-relative.
    templates: HashMap<i64, String>,
    /// Symbols calling `createEffect` (NgRx effects).
    effects: HashSet<i64>,
}

impl Facts {
    fn load(conn: &Connection) -> Result<Facts> {
        let mut f = Facts::default();
        for r in query::http_routes(conn)? {
            if r.side == "server" {
                f.actions.insert((r.file, r.start_line));
            }
        }
        let mut stmt = conn.prepare("SELECT service_name, implementation, kind FROM di_bindings")?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, String>(2)?)))? {
            let (service, implementation, kind) = row?;
            if let Some(s) = service {
                f.di_names.insert(kc_store::di::bare_name(&s));
            }
            if let Some(i) = implementation {
                if kind == "scan" || i.contains('*') {
                    f.di_scans.push(i);
                } else {
                    f.di_names.insert(kc_store::di::bare_name(&i));
                }
            }
        }
        let mut stmt = conn.prepare("SELECT symbol_id, kind, message_name FROM bus_facts")?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?)))? {
            let (id, kind, message) = row?;
            if kind == "consume" {
                f.consumers.insert(id);
            }
            if kind != "message"
                && let Some(m) = message
            {
                f.messages.insert(m);
            }
        }
        let mut stmt = conn.prepare("SELECT symbol_id, kind, entity FROM db_facts")?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))? {
            let (id, kind, entity) = row?;
            if !entity.is_empty() {
                f.entities.insert(entity.rsplit('.').next().unwrap_or(&entity).to_string());
            }
            if kind == "dbset"
                && let Some(id) = id
            {
                f.dbsets.insert(id);
            }
        }
        let mut stmt = conn.prepare(
            "SELECT n.symbol_id, n.value, f.path FROM ng_facts n JOIN files f ON f.id = n.file_id WHERE n.kind = 'template' AND n.symbol_id IS NOT NULL",
        )?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))? {
            let (id, url, file) = row?;
            f.templates.insert(id, join_relative(&file, &url));
        }
        let mut stmt = conn.prepare("SELECT DISTINCT from_id FROM refs WHERE name = 'createEffect' AND kind = 'call' AND from_id IS NOT NULL")?;
        for id in stmt.query_map([], |r| r.get::<_, i64>(0))? {
            f.effects.insert(id?);
        }
        Ok(f)
    }

    fn di_scanned(&self, name: &str) -> bool {
        self.di_scans.iter().any(|p| glob_match(p, name))
    }
}

/// `*Service` against `OrderService`: `*` matches any run of characters.
fn glob_match(pattern: &str, name: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == name;
    }
    let mut rest = name;
    for (i, p) in parts.iter().enumerate() {
        if p.is_empty() {
            continue;
        }
        if i == 0 {
            let Some(r) = rest.strip_prefix(p) else { return false };
            rest = r;
        } else if i == parts.len() - 1 {
            return rest.ends_with(p);
        } else {
            let Some(at) = rest.find(p) else { return false };
            rest = &rest[at + p.len()..];
        }
    }
    true
}

/// `./x.component.html` relative to `src/app/x.component.ts`.
fn join_relative(file: &str, rel: &str) -> String {
    let mut parts: Vec<&str> = file.rsplit_once('/').map_or(Vec::new(), |(d, _)| d.split('/').collect());
    for seg in rel.split('/') {
        match seg {
            "." | "" => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

// ---------------------------------------------------------------------------------------------------------------
// Rules: what is never reported unused.

/// Attributes that say nothing about how code is reached.
const BENIGN_ATTRS: &[&str] = &[
    "Obsolete", "Serializable", "Pure", "DebuggerStepThrough", "DebuggerDisplay", "DebuggerHidden", "DebuggerNonUserCode",
    "ExcludeFromCodeCoverage", "SuppressMessage", "MethodImpl", "CompilerGenerated", "GeneratedCode", "Description",
    "EditorBrowsable", "NotNull", "MaybeNull", "AllowNull", "DisallowNull", "NotNullWhen", "MaybeNullWhen", "CallerMemberName",
    "Injectable",
];

/// Base types whose subclasses a framework finds by itself (assembly scans, hosting, model binding) or calls
/// through overrides.
const FRAMEWORK_BASES: &[&str] = &[
    "Controller", "ControllerBase", "ApiController", "ODataController", "Hub", "Profile", "Module", "AbstractValidator",
    "IEntityTypeConfiguration", "EntityTypeConfiguration", "Migration", "DbMigration", "IHostedService", "BackgroundService",
    "IConsumer", "INotificationHandler", "IRequestHandler", "IStartupFilter", "IMiddleware", "JsonConverter", "Attribute",
    "ActionFilterAttribute", "ExceptionFilterAttribute", "IActionFilter", "IAsyncActionFilter", "IExceptionFilter",
    "IAuthorizationFilter", "IOperationFilter", "ISchemaFilter", "IDocumentFilter", "TagHelper", "ViewComponent",
    "IJob", "IHealthCheck", "IModelBinder", "IValueConverter", "TypeConverter", "Installer", "IWindsorInstaller",
    "IRegistrationSource", "IAutofacModule", "ServiceBase", "Startup",
];

/// Member names a framework or the language calls by convention.
const CONVENTION_NAMES: &[&str] = &[
    "Main", "OnModelCreating", "OnConfiguring", "Dispose", "DisposeAsync", "ToString", "Equals", "GetHashCode", "CompareTo",
    "GetEnumerator", "GetAwaiter", "Deconstruct", "Invoke", "InvokeAsync", "Seed", "Up", "Down", "BuildTargetModel",
    "Validate", "IsValid", "Consume", "Handle", "Execute", "ExecuteAsync", "Load", "OnStart", "OnStop", "Clone",
    "ngOnInit", "ngOnChanges", "ngDoCheck", "ngAfterContentInit", "ngAfterContentChecked", "ngAfterViewInit",
    "ngAfterViewChecked", "ngOnDestroy", "transform", "canActivate", "canActivateChild", "canDeactivate", "canLoad",
    "canMatch", "resolve", "intercept", "writeValue", "registerOnChange", "registerOnTouched", "setDisabledState",
    "validate", "registerOnValidatorChange", "handleError", "constructor", "ngDoBootstrap",
];

/// Angular interfaces whose members are all framework conventions (lifecycle hooks, forms, routing, HTTP): a class
/// implementing them implements nothing else by that.
const NG_INTERFACES: &[&str] = &[
    "OnInit", "OnDestroy", "OnChanges", "DoCheck", "AfterContentInit", "AfterContentChecked", "AfterViewInit",
    "AfterViewChecked", "ControlValueAccessor", "Validator", "AsyncValidator", "CanActivate", "CanActivateChild",
    "CanDeactivate", "CanLoad", "CanMatch", "Resolve", "HttpInterceptor", "PipeTransform", "ErrorHandler",
];

/// Angular decorators that make the framework instantiate a class or bind a member.
const NG_DECORATORS: &[&str] = &[
    "Component", "Directive", "Pipe", "NgModule", "Input", "Output", "HostListener", "HostBinding", "ViewChild",
    "ViewChildren", "ContentChild", "ContentChildren", "Effect",
];

const DATA_SUFFIXES: &[&str] = &[
    "Dto", "DTO", "Model", "Request", "Response", "ViewModel", "Contract", "Message", "Event", "Options", "Settings",
    "Config", "Configuration", "Result", "Entity", "Command", "Query", "Args", "Payload", "Data", "Info", "Details",
];

const DATA_DIRS: &[&str] = &[
    "Models", "Model", "Dto", "Dtos", "DTOs", "Contracts", "Entities", "Entity", "ViewModels", "Requests", "Responses",
    "Messages", "Events", "models", "dto", "dtos", "interfaces", "types",
];

/// Why a symbol is never reported unused, or `None` when it is a candidate.
fn exemption(s: &Sym, m: &Model, f: &Facts) -> Option<&'static str> {
    let stem = s.file.rsplit('/').next().unwrap_or(&s.file).to_ascii_lowercase();
    if kc_store::is_test_path(&s.file) || s.tags.split(',').any(|t| t == "test") || stem.contains("test") || stem.contains("mock") {
        return Some("test code");
    }
    if s.tags.split(',').any(|t| t == "entrypoint") || s.name == "Main" {
        return Some("entry point");
    }
    let generated = s.file.ends_with(".Designer.cs") || s.file.ends_with(".g.cs") || s.file.contains(".generated.");
    if generated || s.file.split('/').any(|d| d == "Migrations") {
        return Some("generated or migration code");
    }
    let parent = s.parent.and_then(|p| m.sym(p));
    let owner = if s.is_type() { Some(s) } else { parent.filter(|p| p.is_type()) };
    let owner_q = owner.map(Sym::qualified);
    if let Some(o) = owner
        && matches!(o.name.as_str(), "Program" | "Startup")
    {
        return Some("entry point");
    }
    if f.actions.contains(&(s.file.clone(), s.line)) || s.attrs.iter().any(|a| a.starts_with("Http") || a == "Route") {
        return Some("controller action");
    }
    let sig = format!(" {}", s.signature);
    if sig.contains(" override ") {
        return Some("override");
    }
    if s.attrs.iter().any(|a| NG_DECORATORS.contains(&a.as_str())) {
        return Some("framework-bound (Angular decorator)");
    }
    if f.effects.contains(&s.id) {
        return Some("NgRx effect");
    }
    if f.consumers.contains(&s.id) || owner.is_some_and(|o| f.consumers.contains(&o.id)) {
        return Some("message consumer");
    }
    if let Some(q) = &owner_q {
        // Besides those built in, the configured consumer bases and service hosts (see `kc_extract::conventions`).
        let conv = kc_index::config::conventions_now();
        let configured = |b: &String| conv.bus.consumer_bases.contains(b) || conv.csharp_http.service_host_bases.contains(b);
        let framework =
            |b: &String| configured(b) || FRAMEWORK_BASES.iter().any(|f| b == f || (b.starts_with(f) && b.len() > f.len() && f.ends_with("Controller")));
        let ext = m.external_bases(q);
        if ext.iter().any(framework) || owner.is_some_and(|o| o.name.ends_with("Controller") || o.attrs.iter().any(|a| a == "ApiController")) {
            // Controllers, consumers, validators, profiles, modules: the class and what the framework calls on it.
            if s.is_type() || s.public(m) || !s.csharp() {
                return Some("framework type (controller, consumer, module or other base the framework calls)");
            }
        }
    }
    if !s.is_type() && CONVENTION_NAMES.contains(&s.name.as_str()) {
        return Some("framework convention");
    }
    if s.kind == "method" && (s.name.starts_with("Configure") || s.signature.contains("(object sender,")) {
        return Some("framework convention");
    }
    if s.name.starts_with("operator ") {
        return Some("operator");
    }
    // Explicit interface implementation: `void IFoo.Bar()`.
    if s.csharp() && !s.is_type() {
        let head = s.signature.split('(').next().unwrap_or("");
        if head.contains(&format!(".{}", s.name)) {
            return Some("interface implementation");
        }
    }
    // A member that a supertype in the repo also declares: an implementation, or one hiding it.
    if !s.is_type()
        && let Some(q) = &owner_q
        && m.supertypes(q).iter().any(|t| !m.members_named(t, &s.name).is_empty())
    {
        return Some("interface implementation");
    }
    if s.is_type() && s.attrs.iter().any(|a| NG_DECORATORS.contains(&a.as_str())) {
        return Some("framework-bound (Angular decorator)");
    }
    if s.is_type() && f.di_scanned(&s.name) {
        return Some("registered by an assembly scan");
    }
    if s.kind == "property" {
        if f.dbsets.contains(&s.id) {
            return Some("Entity Framework DbSet");
        }
        if !s.attrs.is_empty() {
            return Some("data property (attributes bind or serialize it)");
        }
        if let Some(o) = owner
            && data_type(o, m, f)
        {
            return Some("data property (a serialized, bound or mapped type)");
        }
    }
    if s.csharp() && !s.is_type() && !s.attrs.iter().all(|a| BENIGN_ATTRS.contains(&a.as_str())) {
        return Some("framework attribute");
    }
    None
}

/// A type whose properties are read by serializers, binders or mappers rather than by name: an interface (in
/// TypeScript, a JSON shape), a type named or placed like a DTO, an entity, or one with only properties.
fn data_type(t: &Sym, m: &Model, f: &Facts) -> bool {
    if t.typescript() && t.kind == "protocol" {
        return true;
    }
    if t.kind == "struct" || t.signature.contains("record ") {
        return true;
    }
    if f.entities.contains(&t.name) || (t.csharp() && !t.attrs.iter().all(|a| BENIGN_ATTRS.contains(&a.as_str()))) {
        return true;
    }
    if DATA_SUFFIXES.iter().any(|s| t.name.ends_with(s)) || t.file.split('/').any(|d| DATA_DIRS.contains(&d)) {
        return true;
    }
    let q = t.qualified();
    !m.syms.iter().any(|s| s.container.as_deref() == Some(q.as_str()) && matches!(s.kind.as_str(), "method" | "function"))
}

// ---------------------------------------------------------------------------------------------------------------
// `unused`

/// One symbol reported unused, or possibly unused.
#[derive(Debug, Clone, Serialize)]
pub struct UnusedItem {
    pub qualified: String,
    pub kind: String,
    pub file: String,
    pub line: u32,
    pub end_line: u32,
    /// `unused` (nothing reaches it) or `possibly_unused` (only uncertain evidence does).
    pub tier: &'static str,
    /// Why it isn't confidently unused, for `possibly_unused`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UnusedRepo {
    pub repo: String,
    /// The repo has no entry point (no `Main`, `Program` or `Startup`, controller action or Angular component): a
    /// library, whose public API is never reported confidently unused.
    pub library: bool,
    /// Packages of the repo that indexed repos depend on: their public API is never reported confidently unused
    /// either, since repos outside the index may depend on them too.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub published: Vec<String>,
    /// Indexed repos depending on this one, whose references were checked.
    pub dependents: Vec<String>,
    /// Symbols considered.
    pub candidates: usize,
    /// Symbols never reported, by reason.
    pub exempt: BTreeMap<&'static str, usize>,
    pub unused: usize,
    pub possibly_unused: usize,
    pub items: Vec<UnusedItem>,
}

pub struct UnusedParams {
    pub kinds: Vec<String>,
    /// `unused`, `possibly` or `all`.
    pub tier: String,
    pub limit: usize,
}

/// The symbol kinds `--kind` selects.
fn kind_filter(kinds: &[String]) -> Result<Vec<&'static str>> {
    const DEFAULT: &[&str] = &["class", "struct", "enum", "protocol", "method", "function", "property"];
    if kinds.is_empty() {
        return Ok(DEFAULT.to_vec());
    }
    let mut out = Vec::new();
    for k in kinds {
        let add: &[&'static str] = match k.as_str() {
            "type" => &["class", "struct", "enum", "protocol"],
            "class" => &["class"],
            "struct" => &["struct"],
            "enum" => &["enum"],
            "interface" | "protocol" => &["protocol"],
            "method" => &["method"],
            "function" => &["function"],
            "func" | "callable" => &["method", "function"],
            "property" | "field" => &["property"],
            other => bail!("unknown kind `{other}`: use type, class, struct, enum, interface, method, function, func or property"),
        };
        out.extend_from_slice(add);
    }
    Ok(out)
}

/// What one candidate's evidence came to.
#[derive(Default)]
struct Evidence {
    used: bool,
    reasons: Vec<String>,
    tests: Vec<String>,
}

impl Evidence {
    fn maybe(&mut self, why: String) {
        if self.reasons.len() < 4 && !self.reasons.contains(&why) {
            self.reasons.push(why);
        }
    }
}

/// Dead code: symbols that nothing reaches, in each repo. A symbol is `unused` when every reference to its name, in
/// the repo and in indexed repos depending on it, resolves elsewhere (exact, external_likely or unresolved through a
/// known receiver), and no other occurrence of the name could reach it; `possibly_unused` when only ambiguous
/// references, tests, strings, `nameof`, templates or config name it, or when it is public API of a library.
pub fn unused(p: &UnusedParams, args: &Scope) -> Result<Out<Vec<UnusedRepo>>> {
    let kinds = kind_filter(&p.kinds)?;
    if !matches!(p.tier.as_str(), "unused" | "possibly" | "all") {
        bail!("tier must be unused, possibly or all, not {}", p.tier);
    }
    let mut opened = Vec::new();
    let mut repos = Vec::new();
    let roots = target_repos(&args.repos)?;
    for root in &roots {
        let Some(mut o) = open_member(&roots, root, args.no_sync)? else { continue };
        let files = text_files(&mut o, args.no_sync)?;
        repos.push(unused_in(&o, &files, &kinds, args.no_sync)?);
        opened.push(o);
    }
    let mut shown = 0;
    let mut total = 0;
    for r in &mut repos {
        r.items.retain(|i| match p.tier.as_str() {
            "unused" => i.tier == "unused",
            "possibly" => i.tier == "possibly_unused",
            _ => true,
        });
        total += r.items.len();
        r.items.truncate(p.limit.saturating_sub(shown));
        shown += r.items.len();
    }
    let env = envelope(&opened, serde_json::to_value(&repos)?, total, shown);
    Ok(Out { env, data: repos })
}

fn unused_in(o: &Opened, files: &[String], kinds: &[&str], no_sync: bool) -> Result<UnusedRepo> {
    let conn = o.store.conn();
    let m = Model::load(conn)?;
    let f = Facts::load(conn)?;
    let me = repo_name(&o.root);
    let dependents = dependent_repos(o, &[])?;
    let projects = Projects::load(o, conn, &m, &f)?;

    let mut exempt: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut candidates: Vec<&Sym> = Vec::new();
    for s in &m.syms {
        if !(s.csharp() || s.typescript()) || !kinds.contains(&s.kind.as_str()) {
            continue;
        }
        match exemption(s, &m, &f) {
            Some(why) => *exempt.entry(why).or_default() += 1,
            None => candidates.push(s),
        }
    }
    let mut names: Vec<&str> = candidates.iter().map(|s| s.name.as_str()).collect();
    names.sort();
    names.dedup();
    let words: HashSet<&str> = names.iter().copied().collect();
    let occs = Occurrences::scan_repo(&o.root, files, &words);
    // References to the candidates' names, and to the members of candidate types: a type is used when a member is.
    let type_ids: HashSet<i64> = candidates.iter().filter(|s| s.is_type()).map(|s| s.id).collect();
    let mut ref_names = names.clone();
    ref_names.extend(m.syms.iter().filter(|x| x.parent.is_some_and(|p| type_ids.contains(&p)) && !x.is_type()).map(|x| x.name.as_str()));
    ref_names.sort();
    ref_names.dedup();
    let hits = query::refs_named(conn, &ref_names)?;

    // Resolved uses per target id; and, per (file, line, name), that a reference there was resolved at all.
    let mut confident: HashMap<i64, Vec<usize>> = HashMap::new();
    let mut ambiguous: HashMap<i64, Vec<usize>> = HashMap::new();
    // Occurrences exactly-resolved references explain: by file, line, column (0 when the extractor gave none, which
    // explains the name's every occurrence on the line) and name.
    let mut ref_lines: HashSet<(&str, u32, u32, &str)> = HashSet::new();
    // Unresolved references that could still mean a candidate, by name.
    let mut unresolved: HashMap<&str, Vec<usize>> = HashMap::new();
    // Names of the repo's C# extension methods: a call on a receiver of any type, a framework's too, may mean one.
    let extensions: HashSet<&str> =
        m.syms.iter().filter(|x| x.csharp() && x.signature.contains("(this ")).map(|x| x.name.as_str()).collect();
    for (hi, h) in hits.iter().enumerate() {
        // Through a receiver of a framework type: a framework member, resolved elsewhere (unless the repo extends it).
        let foreign = h.receiver_type.as_deref().is_some_and(|t| m.resolve_type(t.rsplit('.').next().unwrap_or(t)).is_empty())
            && !(h.confidence == "unresolved" && extensions.contains(h.name.as_str()));
        // Only a reference resolved exactly (or to a framework member) explains its occurrence; one bound to another
        // definition by name alone might have meant a candidate the resolver's rules left out.
        if matches!(h.confidence, "exact" | "external_likely") || foreign {
            ref_lines.insert((h.file.as_str(), h.line, h.col, h.name.as_str()));
        } else if h.receiver.is_some() {
            // Through a receiver of a repo type that lacks the member (the resolver's guess may be wrong), or of an
            // unknown one (`ns.member` through a namespace import never reaches a free symbol).
            unresolved.entry(h.name.as_str()).or_default().push(hi);
        }
        let mut ids: Vec<i64> = h.target_ids.clone();
        if let Some(b) = &h.via_di {
            ids.push(b.target.id);
        }
        // One of several providers: a candidate, never certain.
        for b in &h.via_di_any {
            ambiguous.entry(b.target.id).or_default().push(hi);
        }
        for id in ids {
            match h.confidence {
                "exact" | "unique_name" => confident.entry(id).or_default().push(hi),
                "ambiguous" => ambiguous.entry(id).or_default().push(hi),
                _ => {}
            }
        }
    }
    // Declaration sites of every symbol with a candidate's name: occurrences there are declarations, not uses.
    let mut decls: HashSet<(usize, u32, u32)> = HashSet::new();
    for n in &names {
        for s in m.named(n) {
            if let Some((fi, occ)) = m.decl_site(s, &occs) {
                decls.insert((fi, occ.line, occ.col));
            }
        }
    }
    let dep_hits = dependent_hits(o, &dependents, &names, no_sync)?;

    let mut evidence: HashMap<i64, Evidence> = HashMap::new();
    for s in &candidates {
        let mut ev = Evidence::default();
        let self_use = |h: &RefHit| h.from.as_ref().is_some_and(|fr| fr.id == s.id) || s.contains(&h.file, h.line);
        for &hi in confident.get(&s.id).into_iter().flatten() {
            let h = &hits[hi];
            if self_use(h) {
                continue;
            }
            if kc_store::is_test_path(&h.file) {
                ev.tests.push(format!("{}:{}", h.file, h.line));
            } else {
                ev.used = true;
                break;
            }
        }
        if !ev.used {
            let extension = s.csharp() && s.signature.contains("(this ");
            for &hi in ambiguous.get(&s.id).into_iter().flatten() {
                let h = &hits[hi];
                if !self_use(h) {
                    ev.maybe(format!("ambiguous reference at {}:{}", h.file, h.line));
                }
            }
            for &hi in unresolved.get(s.name.as_str()).into_iter().flatten() {
                let h = &hits[hi];
                // An extension method is called like an instance member (`x.M()`) on a receiver of any type it extends.
                if !self_use(h) && ref_may_mean(&h.kind, &s.kind) && (h.receiver_type.is_some() || s.container.is_none() || extension) {
                    ev.maybe(format!("unresolved reference at {}:{}", h.file, h.line));
                }
            }
        }
        evidence.insert(s.id, ev);
    }
    // A type is used when one of its members is (an extension-method class is never named, and its methods'
    // container is the type they extend), whether or not that member was a candidate.
    let mut member_used_any: HashSet<String> = HashSet::new();
    for (id, list) in &confident {
        if let Some(t) = m.sym(*id)
            && !t.is_type()
            && let Some(owner) = t.parent.and_then(|p| m.sym(p)).filter(|p| p.is_type())
            && list.iter().any(|&hi| !owner.contains(&hits[hi].file, hits[hi].line) && !kc_store::is_test_path(&hits[hi].file))
        {
            member_used_any.insert(owner.qualified());
        }
    }

    // A member is used when a member renamed with it is: an interface member whose implementation is called, a
    // base member whose override is.
    let used_directly = |id: i64| {
        let Some(t) = m.sym(id) else { return false };
        confident.get(&id).into_iter().flatten().any(|&hi| !t.contains(&hits[hi].file, hits[hi].line) && !kc_store::is_test_path(&hits[hi].file))
    };
    let mut items = Vec::new();
    // Types whose member may be used, with why: such a type is at most possibly unused.
    let mut owners_maybe: HashMap<String, String> = HashMap::new();
    // Members first, so their types know.
    let ordered: Vec<&&Sym> = candidates.iter().filter(|s| !s.is_type()).chain(candidates.iter().filter(|s| s.is_type())).collect();
    for s in ordered {
        let mut ev = evidence.remove(&s.id).unwrap_or_default();
        if ev.used {
            continue;
        }
        let q = s.qualified();
        if !s.is_type() && rename_group(&m, s).iter().any(|g| g.id != s.id && used_directly(g.id)) {
            continue;
        }
        if s.is_type() {
            if let Some(why) = owners_maybe.get(&q) {
                ev.maybe(why.clone());
            }
            if member_used_any.contains(&q) {
                continue;
            }
            // Subtypes, DI registrations, bus messages and entities are uses the reference table doesn't hold.
            if m.bases.iter().any(|(t, b)| t != &q && b.contains(&s.name) && m.supertypes(t).contains(&q)) {
                continue;
            }
            if f.di_names.contains(&s.name) || f.messages.contains(&s.name) || f.entities.contains(&s.name) {
                continue;
            }
        }
        // References from dependents, resolved into this repo.
        for h in dep_hits.get(s.name.as_str()).into_iter().flatten() {
            let to_me = h.targets.iter().any(|t| t.id == s.id && t.repo.as_deref() == Some(me.as_str()));
            if to_me && matches!(h.confidence, "exact" | "unique_name") {
                ev.used = true;
            } else if to_me || h.confidence == "ambiguous" {
                ev.maybe(format!("{} reference in {} at {}:{}", h.confidence, h.repo.as_deref().unwrap_or("?"), h.file, h.line));
            }
        }
        if ev.used {
            // Its type is used too (an extension method's class is never named).
            if !s.is_type()
                && let Some(owner) = s.parent.and_then(|p| m.sym(p)).filter(|p| p.is_type())
            {
                member_used_any.insert(owner.qualified());
            }
            continue;
        }
        text_evidence(s, &m, &f, &occs, &decls, &ref_lines, &mut ev);
        if ev.used {
            continue;
        }
        if !ev.tests.is_empty() {
            ev.maybe(format!("used only by tests ({})", ev.tests[0]));
        }
        if let Some(q) = s.container.as_deref().filter(|_| !s.is_type()) {
            // C# interfaces are named `IFoo`; in TypeScript any base may be an interface the class implements.
            let ext: Vec<String> = m
                .external_bases(q)
                .into_iter()
                .filter(|b| if s.typescript() { !NG_INTERFACES.contains(&b.as_str()) } else { is_interface_name(b) })
                .collect();
            if !ext.is_empty() && s.declared_public() {
                ev.maybe(format!("may implement {} (declared outside the index)", ext.join(", ")));
            }
        }
        if s.is_type()
            && let Some(a) = s.attrs.iter().find(|a| !BENIGN_ATTRS.contains(&a.as_str()))
        {
            ev.maybe(format!("attribute [{a}] may register it"));
        }
        // A class implementing an interface declared outside the index may be found by an assembly scan.
        if s.is_type() && s.kind != "protocol" {
            let ext: Vec<String> = m.external_bases(&q).into_iter().filter(|b| is_interface_name(b)).collect();
            if !ext.is_empty() {
                ev.maybe(format!("implements {}, declared outside the index: a framework may create it", ext.join(", ")));
            }
        }
        if s.public(&m)
            && let Some(why) = projects.shared(&s.file)
        {
            ev.maybe(format!("public API: {why}"));
        }
        if !s.is_type()
            && !ev.reasons.is_empty()
            && let Some(owner) = s.parent.and_then(|p| m.sym(p)).filter(|p| p.is_type())
        {
            owners_maybe.entry(owner.qualified()).or_insert_with(|| format!("its member {} may be used", s.name));
        }
        items.push(UnusedItem {
            qualified: q,
            kind: s.kind.clone(),
            file: s.file.clone(),
            line: s.line,
            end_line: s.end_line,
            tier: if ev.reasons.is_empty() { "unused" } else { "possibly_unused" },
            reasons: ev.reasons,
        });
    }
    items.sort_by(|a, b| (a.tier != "unused", &a.file, a.line).cmp(&(b.tier != "unused", &b.file, b.line)));
    let unused = items.iter().filter(|i| i.tier == "unused").count();
    Ok(UnusedRepo {
        repo: me,
        library: !projects.any_app,
        published: projects.consumed.iter().map(|&i| projects.list[i].0.clone()).collect(),
        dependents: dependents.iter().map(|d| repo_name(d)).collect(),
        candidates: candidates.len(),
        exempt,
        unused,
        possibly_unused: items.len() - unused,
        items,
    })
}

/// Occurrences of `s`'s name that no resolved reference explains, as evidence: a bare identifier inside the type
/// (a use), `nameof`, a member access the extractor didn't resolve, strings, templates and config.
fn text_evidence(
    s: &Sym,
    m: &Model,
    f: &Facts,
    occs: &Occurrences,
    decls: &HashSet<(usize, u32, u32)>,
    ref_lines: &HashSet<(&str, u32, u32, &str)>,
    ev: &mut Evidence,
) {
    let template = s.parent.and_then(|p| f.templates.get(&p));
    let unique_type = s.is_type() && m.type_count(&s.name) == 1;
    for (fi, o) in occs.of(&s.name) {
        let file = occs.files[*fi].as_str();
        if s.owns(file, o) || decls.contains(&(*fi, o.line, o.col)) {
            continue;
        }
        let at = format!("{file}:{}", o.line);
        match o.ctx {
            // A Markdown file documenting the name: documented API.
            Ctx::Doc if !file.ends_with(".cs") && !file.ends_with(".ts") => ev.maybe(format!("documented at {at}")),
            Ctx::Comment | Ctx::Doc => {}
            Ctx::Code => {
                // Before references explain the line: `Name = other.Name` holds a reference to the other `Name`.
                if o.init_assign && s.kind == "property" && s.csharp() {
                    ev.maybe(format!("set in an object initializer at {at}"));
                    continue;
                }
                let explained = |col| ref_lines.contains(&(file, o.line, col, s.name.as_str()));
                if o.import || o.namespace || explained(o.col) || explained(0) {
                    continue;
                }
                if !s.is_type() && m.member_elsewhere(s, file, o) {
                    continue;
                }
                if o.nameof {
                    ev.maybe(format!("nameof at {at}"));
                    continue;
                }
                if s.is_type() {
                    if o.then_dot && unique_type && !kc_store::is_test_path(file) {
                        // `Type.Member`: a static use.
                        ev.used = true;
                        return;
                    }
                    ev.maybe(format!("named at {at}"));
                    continue;
                }
                if o.dotted {
                    ev.maybe(format!("member access at {at} not resolved to it exactly"));
                    continue;
                }
                // Unqualified: in C#, only code in the member's type (or a nested or derived type) can mean it; in
                // TypeScript a member always needs `this.`.
                if s.typescript() && s.container.is_some() {
                    continue;
                }
                if s.csharp()
                    && let Some(owner) = &s.container
                {
                    match m.enclosing_type(file, o.line) {
                        Some(t) if m.sees_members_of(&t, owner) => {
                            if kc_store::is_test_path(file) {
                                ev.tests.push(at);
                            } else {
                                ev.used = true;
                                return;
                            }
                        }
                        Some(_) => {}
                        None => ev.maybe(format!("named at {at}")),
                    }
                    continue;
                }
                ev.maybe(format!("named at {at}"));
            }
            Ctx::Markup => {
                if template.is_some_and(|t| t == file) {
                    ev.used = true;
                    return;
                }
                ev.maybe(format!("named in markup at {at}"));
            }
            Ctx::Str => ev.maybe(format!("string at {at} (reflection, routes or config)")),
            Ctx::Text => ev.maybe(format!("named in {at} (config)")),
        }
    }
}

/// `IFoo`: the C# naming convention for interfaces.
fn is_interface_name(name: &str) -> bool {
    name.len() > 1 && name.starts_with('I') && name[1..].starts_with(char::is_uppercase)
}

/// Whether a reference of `ref_kind` can name a symbol of `kind`, as resolution matches them.
fn ref_may_mean(ref_kind: &str, kind: &str) -> bool {
    match ref_kind {
        "call" => matches!(kind, "method" | "function" | "property"),
        "instantiate" => matches!(kind, "class" | "struct"),
        "type_ref" => matches!(kind, "class" | "struct" | "enum" | "protocol"),
        "member" => matches!(kind, "property" | "method"),
        _ => false,
    }
}

/// Package names whose public API is meant for other repos: models, contracts, clients.
const SHARED_SUFFIXES: &[&str] =
    &[".Models", ".Contracts", ".Client", ".Clients", ".Gateway", ".Sdk", ".Abstractions", ".Messages", ".Events", ".Dtos"];

/// The repo's projects (`.csproj`, `package.json`), for telling a library's public API from an app's.
struct Projects {
    /// Name, directory, and the directories of the projects it references by path.
    list: Vec<(String, String, Vec<String>)>,
    /// Projects an indexed repo depends on.
    consumed: Vec<usize>,
    /// Projects holding an entry point, and those they reach through project references.
    reachable: HashSet<usize>,
    /// Whether any project holds an entry point.
    any_app: bool,
}

impl Projects {
    fn load(o: &Opened, conn: &Connection, m: &Model, f: &Facts) -> Result<Projects> {
        let summaries = crate::ops::planned_summaries(o)?;
        let me = o.root.to_string_lossy().into_owned();
        let mut list = Vec::new();
        for p in kc_store::manifests::packages(conn)? {
            let dir = p.file.rsplit_once('/').map_or(String::new(), |(d, _)| d.to_string());
            let refs = p
                .deps
                .iter()
                .filter(|d| d.source == "path")
                .map(|d| {
                    let target = join_relative(&p.file, &d.location.replace('\\', "/"));
                    if target.ends_with("proj") { target.rsplit_once('/').map_or(String::new(), |(d, _)| d.to_string()) } else { target }
                })
                .collect();
            list.push((p.name.clone(), dir, refs));
        }
        let mut proj = Projects { list, consumed: Vec::new(), reachable: HashSet::new(), any_app: false };
        for (i, (name, dir, _)) in proj.list.iter().enumerate() {
            let last = dir.rsplit('/').next().unwrap_or(dir);
            let used = summaries.iter().filter(|s| s.root != o.root).flat_map(|s| &s.packages).flat_map(|q| &q.deps).any(|d| {
                (d.source == "registry" && d.name.eq_ignore_ascii_case(name))
                    || (d.resolved_repo.as_deref() == Some(me.as_str()) && !last.is_empty() && d.location.contains(last))
            });
            if used {
                proj.consumed.push(i);
            }
        }
        let mut todo: Vec<usize> = Vec::new();
        for s in &m.syms {
            let entry = s.tags.split(',').any(|t| t == "entrypoint")
                || (s.kind == "class" && matches!(s.name.as_str(), "Program" | "Startup"))
                || (s.kind == "class" && s.attrs.iter().any(|a| a == "Component"))
                || f.actions.contains(&(s.file.clone(), s.line));
            if !entry || kc_store::is_test_path(&s.file) {
                continue;
            }
            proj.any_app = true;
            if let Some(i) = proj.of(&s.file)
                && proj.reachable.insert(i)
            {
                todo.push(i);
            }
        }
        while let Some(i) = todo.pop() {
            for d in proj.list[i].2.clone() {
                if let Some(j) = proj.list.iter().position(|(_, dir, _)| *dir == d)
                    && proj.reachable.insert(j)
                {
                    todo.push(j);
                }
            }
        }
        Ok(proj)
    }

    /// The project whose directory holds `file`: the deepest.
    fn of(&self, file: &str) -> Option<usize> {
        self.list
            .iter()
            .enumerate()
            .filter(|(_, (_, dir, _))| dir.is_empty() || file.starts_with(&format!("{dir}/")))
            .max_by_key(|(_, (_, dir, _))| dir.len())
            .map(|(i, _)| i)
    }

    /// Why public API in `file` may be used by repos outside the index: the repo runs nothing (a library), or the
    /// file's project is a package indexed repos depend on, is named like a shared package (models, contracts, a
    /// client), or isn't referenced by the repo's own apps.
    fn shared(&self, file: &str) -> Option<String> {
        if !self.any_app {
            return Some("the repo is a library; repos outside the index may use it".into());
        }
        let i = self.of(file)?;
        let name = &self.list[i].0;
        if self.consumed.contains(&i) {
            return Some(format!("package {name} is used by indexed repos, and others may use it too"));
        }
        if SHARED_SUFFIXES.iter().any(|x| name.ends_with(x)) {
            return Some(format!("package {name} looks shared"));
        }
        if !self.reachable.contains(&i) && !kc_store::is_test_path(&self.list[i].1) {
            return Some(format!("package {name} isn't referenced by the repo's apps, so others may use it"));
        }
        None
    }
}

/// Indexed repos whose manifests depend on `root`. Those whose checkout is gone are left out, and named missing when
/// they may reference one of `names` (any name, when empty).
fn dependent_repos(o: &Opened, names: &[&str]) -> Result<Vec<PathBuf>> {
    let root = o.root.as_path();
    let summaries = crate::ops::planned_summaries(o)?;
    let (_, dependents) = kc_index::workspace::neighbours(root, &summaries);
    // A dependent whose checkout is gone can't be read for the uses a rename would break: the answer says it is missing.
    let (gone, there): (Vec<_>, Vec<_>) = dependents.into_iter().partition(|s| s.gone);
    kc_index::workspace::note_missing_if(gone, |s| names.is_empty() || names.iter().any(|n| s.may_reference(n)));
    Ok(there.into_iter().map(|s| s.root.clone()).collect())
}

/// References to `names` in the dependent repos, in files importing this repo's modules, resolved into it; by name.
fn dependent_hits<'n>(o: &Opened, dependents: &[PathBuf], names: &[&'n str], no_sync: bool) -> Result<HashMap<&'n str, Vec<RefHit>>> {
    let mut out: HashMap<&str, Vec<RefHit>> = HashMap::new();
    if dependents.is_empty() {
        return Ok(out);
    }
    let me = repo_name(&o.root);
    let modules = query::provided_modules(o.store.conn())?;
    let dep = query::Dependency { repo: me, conn: o.store.conn(), modules, assumptions: Vec::new() };
    for root in dependents {
        let mut store = kc_index::open(root)?;
        if !no_sync {
            kc_index::sync(root, &mut store, &kc_index::SyncOptions::default())?;
        }
        let referenced: HashSet<String> = {
            let mut stmt = store.conn().prepare("SELECT DISTINCT name FROM refs")?;
            stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
        };
        for n in names.iter().filter(|n| referenced.contains(**n)) {
            for mut h in query::refs_importing(store.conn(), n, &[], std::slice::from_ref(&dep))? {
                h.repo = Some(repo_name(root));
                out.entry(n).or_default().push(h);
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------------------------
// `rename`

/// One place a rename edits.
#[derive(Debug, Clone, Serialize)]
pub struct Site {
    /// Set for a site in another repo.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    pub file: String,
    pub line: u32,
    pub col: u32,
    /// definition, override, implementation, interface_member, constructor, reference, import, nameof, cref,
    /// string, template, config or route.
    pub kind: &'static str,
    /// exact, unique_name or ambiguous.
    pub confidence: &'static str,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// A symbol the new name would clash with.
#[derive(Debug, Clone, Serialize)]
pub struct Collision {
    pub qualified: String,
    pub kind: String,
    pub file: String,
    pub line: u32,
    pub why: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RenamePreview {
    pub symbol: String,
    pub kind: String,
    pub file: String,
    pub line: u32,
    pub new_name: String,
    /// Members renamed together: the symbol, the interface members it implements, their other implementations, the
    /// members it overrides and those overriding it.
    pub group: Vec<String>,
    pub sites: Vec<Site>,
    pub cross_repo: Vec<Site>,
    pub collisions: Vec<Collision>,
    /// Comments and prose strings mentioning the name: not edited, listed for review.
    pub mentions: Vec<Site>,
    /// True when the preview refuses: an ambiguous site, a collision, or a member the index can't see.
    pub refused: bool,
    pub reasons: Vec<String>,
    /// Each reason's code: `ambiguous`, `collision`, `unsupported` (a member the index can't see).
    pub codes: Vec<Code>,
}

pub struct RenameParams {
    pub symbol: String,
    pub new_name: String,
    pub allow_ambiguous: bool,
}

const KEYWORDS: &[&str] = &[
    "abstract", "as", "base", "bool", "break", "byte", "case", "catch", "char", "checked", "class", "const", "continue",
    "decimal", "default", "delegate", "do", "double", "else", "enum", "event", "explicit", "extern", "false", "finally",
    "fixed", "float", "for", "foreach", "goto", "if", "implicit", "in", "int", "interface", "internal", "is", "lock",
    "long", "namespace", "new", "null", "object", "operator", "out", "override", "params", "private", "protected",
    "public", "readonly", "ref", "return", "sbyte", "sealed", "short", "sizeof", "stackalloc", "static", "string",
    "struct", "switch", "this", "throw", "true", "try", "typeof", "uint", "ulong", "unchecked", "unsafe", "ushort",
    "using", "virtual", "void", "volatile", "while", "function", "let", "var", "export", "import", "extends",
    "implements", "super", "yield", "await", "async", "instanceof", "constructor",
];

/// What renaming a symbol to `new_name` would edit, in its repo and in indexed repos depending on it, each site
/// with a confidence; refused (with reasons) when any site is ambiguous or the new name collides.
pub fn rename(p: &RenameParams, args: &Scope) -> Result<Out<RenamePreview>> {
    let new = p.new_name.trim();
    if new.is_empty() || !new.bytes().all(ident_byte) || new.as_bytes()[0].is_ascii_digit() || KEYWORDS.contains(&new) {
        bail!("`{new}` is not a valid identifier");
    }
    let roots = target_repos(&args.repos)?;
    // Outside a repo and with no --repo: the one indexed repo defining the symbol.
    let roots = if roots.len() > 1 && args.repos.is_empty() { repos_defining(&roots, &p.symbol)? } else { roots };
    let root = match roots.as_slice() {
        [root] => root,
        [] => bail!("no indexed repo defines {}", p.symbol),
        many => bail!(
            "{} is defined in {} repos ({}): rename previews one repo at a time, pass --repo",
            p.symbol,
            many.len(),
            many.iter().take(8).map(|r| repo_name(r)).collect::<Vec<_>>().join(", ")
        ),
    };
    let mut o = open_fresh(root, args.no_sync)?;
    let files = text_files(&mut o, args.no_sync)?;
    let m = Model::load(o.store.conn())?;
    let target = pick_symbol(&m, &p.symbol)?;
    let preview = rename_in(&o, &m, &files, target, new, p.allow_ambiguous, args.no_sync)?;
    let shown = preview.sites.len() + preview.cross_repo.len();
    let mut env = envelope(std::slice::from_ref(&o), json!(preview.sites), shown, shown);
    for (k, v) in [
        ("symbol", json!(preview.symbol)),
        ("kind", json!(preview.kind)),
        ("new_name", json!(preview.new_name)),
        ("group", json!(preview.group)),
        ("cross_repo", json!(preview.cross_repo)),
        ("collisions", json!(preview.collisions)),
        ("mentions", json!(preview.mentions)),
        ("refused", json!(preview.refused)),
        ("reasons", json!(preview.reasons)),
        ("codes", json!(preview.codes)),
    ] {
        env[k] = v;
    }
    // The sites are never cut here; a size cap cutting them continues them, and refuses the preview (see the MCP
    // server's `cap`).
    let req = crate::cursor::Request::Rename { symbol: p.symbol.clone(), new_name: p.new_name.clone(), allow_ambiguous: p.allow_ambiguous };
    let lists = vec![
        crate::cursor::List::new("/items", shown.max(1), false),
        crate::cursor::List::new("/cross_repo", shown.max(1), false),
        crate::cursor::List::new("/mentions", shown.max(1), false),
    ];
    crate::cursor::offer(&mut env, req, args, lists, &[]);
    crate::ops::finish(&mut env);
    Ok(Out { env, data: preview })
}

/// The repos among `roots` whose index (as last synced) defines `symbol`.
fn repos_defining(roots: &[PathBuf], symbol: &str) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for r in roots {
        let Ok(store) = kc_index::open(r) else { continue };
        if !query::definitions(store.conn(), symbol)?.is_empty() {
            out.push(r.clone());
        }
    }
    Ok(out)
}

/// The one symbol `query` names: `Type.member`, a name, or `file:line` (the innermost symbol there). Partial
/// declarations of one type count as one; several other definitions are refused with their locations.
fn pick_symbol<'m>(m: &'m Model, query: &str) -> Result<&'m Sym> {
    let q = query.trim();
    if let Some((file, line)) = q.rsplit_once(':').and_then(|(f, l)| Some((f, l.parse::<u32>().ok()?))) {
        let file = file.trim_start_matches("./");
        return m
            .syms
            .iter()
            .filter(|s| s.file == file && s.line <= line && line <= s.end_line && s.kind != "extension")
            .min_by_key(|s| s.end_line - s.line)
            .ok_or_else(|| anyhow::anyhow!("no symbol at {file}:{line}"));
    }
    let (container, name) = query::split_dotted(q);
    let found: Vec<&Sym> = m
        .named(name)
        .filter(|s| s.name == name && s.kind != "extension")
        .filter(|s| match container {
            None => true,
            Some(c) => s.container.as_deref().is_some_and(|sc| sc == c || sc.ends_with(&format!(".{c}"))),
        })
        .collect();
    let Some(first) = found.first() else { bail!("no definition named {q}") };
    let same = found.iter().all(|s| s.qualified() == first.qualified() && s.kind == first.kind && first.is_type());
    if found.len() > 1 && !same {
        let list: Vec<String> = found.iter().map(|s| format!("{} {} at {}:{}", s.kind, s.qualified(), s.file, s.line)).collect();
        bail!("{q} names {} definitions; pass Type.member or file:line:\n  {}", found.len(), list.join("\n  "));
    }
    Ok(first)
}

fn is_partial(s: &Sym) -> bool {
    format!(" {}", s.signature).contains(" partial ")
}

/// The members a rename of `s` must rename with it: up to what it overrides or implements, down to what overrides or
/// implements those, until nothing new is found. Types: their partial declarations.
fn rename_group<'m>(m: &'m Model, s: &'m Sym) -> Vec<&'m Sym> {
    if s.is_type() {
        // Declarations sharing a qualified name are one type only when partial: the index has no namespaces, so
        // two `Models.Order` and `Entities.Order` classes both read `Order`.
        if !is_partial(s) {
            return vec![s];
        }
        return m.types.get(&s.qualified()).into_iter().flatten().map(|&i| &m.syms[i]).filter(|t| is_partial(t)).collect();
    }
    let mut group: Vec<&Sym> = vec![s];
    let mut i = 0;
    while i < group.len() {
        let g = group[i];
        i += 1;
        let Some(c) = &g.container else { continue };
        for t in m.supertypes(c).into_iter().chain(m.subtypes(c)) {
            for x in m.members_named(&t, &g.name) {
                if x.kind == g.kind && same_params(&x.signature, &g.signature) && !group.iter().any(|y| y.id == x.id) {
                    group.push(x);
                }
            }
        }
    }
    group
}

/// The parameter types of a signature, for telling overloads apart: `Foo(int a, string b = null)` gives `int` and
/// `string`; `foo(a: number)` gives `number`. `None` without a whole parameter list (a property, a capped signature).
fn param_types(sig: &str) -> Option<Vec<String>> {
    let open = sig.find('(')?;
    let mut depth = 0i32;
    let mut close = None;
    for (i, ch) in sig[open..].char_indices() {
        match ch {
            '(' | '<' | '[' | '{' => depth += 1,
            ')' | '>' | ']' | '}' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(open + i);
                    break;
                }
            }
            _ => {}
        }
    }
    let inner = &sig[open + 1..close?];
    let mut params = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    for ch in inner.chars() {
        match ch {
            '(' | '<' | '[' | '{' => depth += 1,
            ')' | '>' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                params.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(ch);
    }
    if !cur.trim().is_empty() {
        params.push(cur);
    }
    let types = params
        .iter()
        .map(|p| {
            let p = p.split('=').next().unwrap_or(p).trim();
            // TypeScript: `name?: Type`.
            if let Some((_, t)) = p.split_once(':') {
                return t.trim().to_string();
            }
            let words: Vec<&str> = p
                .split_whitespace()
                .filter(|w| !matches!(*w, "this" | "params" | "ref" | "out" | "in" | "scoped" | "readonly"))
                .collect();
            let ty = if words.len() > 1 { words[..words.len() - 1].join(" ") } else { words.join(" ") };
            // `System.String` and `String?` read as `String`.
            ty.rsplit('.').next().unwrap_or(&ty).trim_end_matches('?').to_string()
        })
        .collect();
    Some(types)
}

/// Whether two members can be one rename's override or implementation of each other: the same parameter types,
/// when both signatures show them.
fn same_params(a: &str, b: &str) -> bool {
    match (param_types(a), param_types(b)) {
        (Some(x), Some(y)) => x == y,
        _ => true,
    }
}

fn rename_in(o: &Opened, m: &Model, files: &[String], s: &Sym, new: &str, allow_ambiguous: bool, no_sync: bool) -> Result<RenamePreview> {
    let conn = o.store.conn();
    let f = Facts::load(conn)?;
    let me = repo_name(&o.root);
    let group = rename_group(m, s);
    let ids: HashSet<i64> = group.iter().map(|g| g.id).collect();
    // Members joined through a type whose name several types share: the index has no namespaces, so which of them
    // the hierarchy means is a guess.
    let uncertain: HashSet<i64> = group
        .iter()
        .filter(|g| g.id != s.id && g.container != s.container)
        .filter(|g| g.container.as_deref().is_some_and(|c| m.type_count(c.rsplit('.').next().unwrap_or(c)) > 1))
        .map(|g| g.id)
        .collect();
    let containers: HashSet<String> = group.iter().filter_map(|g| if g.is_type() { Some(g.qualified()) } else { g.container.clone() }).collect();
    let short_containers: HashSet<String> = containers.iter().map(|c| c.rsplit('.').next().unwrap_or(c).to_string()).collect();
    let word = s.name.as_str();
    let words: HashSet<&str> = HashSet::from([word]);
    let occs = Occurrences::scan_repo(&o.root, files, &words);
    let texts: HashMap<usize, String> =
        occs.files.iter().enumerate().filter_map(|(i, p)| std::fs::read_to_string(o.root.join(p)).ok().map(|t| (i, t))).collect();
    let text_at = |fi: usize, line: u32| texts.get(&fi).map(|t| line_text(t, line)).unwrap_or_default();
    // Another definition of the name anywhere in the repo makes an unqualified mention uncertain.
    let others = m.named(word).filter(|x| !ids.contains(&x.id) && x.kind != "init").count();
    let unique = |c: &'static str| if others == 0 { c } else { "ambiguous" };

    let mut sites: Vec<Site> = Vec::new();
    let mut mentions: Vec<Site> = Vec::new();
    let mut reasons: Vec<String> = Vec::new();
    let mut codes: Vec<Code> = Vec::new();
    let mut seen: HashSet<(usize, u32, u32)> = HashSet::new();
    let site = |fi: usize, occ: &Occ, kind: &'static str, confidence: &'static str, note: Option<String>, sites: &mut Vec<Site>| {
        sites.push(Site {
            repo: None,
            file: occs.files[fi].clone(),
            line: occ.line,
            col: occ.col,
            kind,
            confidence,
            text: text_at(fi, occ.line),
            note,
        });
    };

    // Definitions, and constructors of a renamed type.
    for g in &group {
        let Some((fi, occ)) = m.decl_site(g, &occs) else { continue };
        let kind = if g.id == s.id {
            "definition"
        } else if g.parent.and_then(|p| m.sym(p)).is_some_and(|p| p.kind == "protocol") {
            "interface_member"
        } else if format!(" {}", g.signature).contains(" override ") {
            "override"
        } else {
            "implementation"
        };
        seen.insert((fi, occ.line, occ.col));
        let (conf, note) = if uncertain.contains(&g.id) {
            ("ambiguous", format!("{}: several types share its type's name, so it may not be in the hierarchy", g.qualified()))
        } else {
            ("exact", g.qualified())
        };
        site(fi, occ, kind, conf, (g.id != s.id).then_some(note), &mut sites);
    }
    if s.is_type() {
        let q = s.qualified();
        for c in m.syms.iter().filter(|x| x.kind == "init" && x.container.as_deref() == Some(q.as_str())) {
            if let Some((fi, occ)) = m.decl_site(c, &occs)
                && seen.insert((fi, occ.line, occ.col))
            {
                site(fi, occ, "constructor", "exact", None, &mut sites);
            }
        }
    }
    // A member the index can't see may be part of the rename: an override of, or an implementation of, a type
    // declared outside the index.
    if !s.is_type()
        && let Some(c) = &s.container
    {
        let ext = m.external_bases(c);
        let sig = format!(" {}", s.signature);
        if sig.contains(" override ") && !group.iter().any(|g| g.id != s.id && !format!(" {}", g.signature).contains(" override ")) {
            reasons.push(format!("{} overrides a member of {}, declared outside the index", s.qualified(), ext.join(", ")));
            codes.push(Code::Unsupported);
        } else if s.declared_public()
            && let Some(i) = ext.iter().find(|b| s.typescript() || is_interface_name(b))
        {
            reasons.push(format!("{} may implement a member of {i}, declared outside the index", s.qualified()));
            codes.push(Code::Unsupported);
        }
    }

    // References, resolved.
    let hits = query::refs_named(conn, &[word])?;
    let mut by_line: HashMap<(String, u32), Vec<&RefHit>> = HashMap::new();
    for h in &hits {
        by_line.entry((h.file.clone(), h.line)).or_default().push(h);
    }
    let reaches = |h: &RefHit| {
        h.target_ids.iter().any(|t| ids.contains(t))
            || h.via_di.as_ref().is_some_and(|b| ids.contains(&b.target.id))
            || h.via_di_any.iter().any(|b| ids.contains(&b.target.id))
    };
    // For a C# type: namespaces settle which same-named type a name means (the index has none, so read them).
    let ns_of = |root: &Path, file: &str| std::fs::read_to_string(root.join(file)).map(|t| csharp_scopes(&t)).unwrap_or_default();
    let type_ns: Vec<(i64, Option<String>)> = if s.is_type() && s.csharp() {
        m.named(word).filter(|x| x.is_type() && x.csharp()).map(|x| (x.id, ns_of(&o.root, &x.file).0)).collect()
    } else {
        Vec::new()
    };
    let my_ns = type_ns.iter().find(|(id, _)| *id == s.id).and_then(|(_, n)| n.clone());
    let global_usings: Vec<String> = if type_ns.is_empty() {
        Vec::new()
    } else {
        occs.files.iter().filter(|f| f.ends_with(".cs")).flat_map(|f| ns_of(&o.root, f).2).collect()
    };
    // `Some(confidence)` when the occurrence means the renamed type, `Some(None)` when it can't, `None` when
    // namespaces don't apply.
    let ns_verdict = |root: &Path, file: &str, occ: &Occ, global: &[String]| -> Option<Option<&'static str>> {
        if type_ns.is_empty() || !file.ends_with(".cs") {
            return None;
        }
        if let Some(path) = &occ.path {
            // `A.B.Name`: a namespace path, or an enclosing type.
            let ns_match = |n: &Option<String>| n.as_deref().is_some_and(|n| n == path || n.ends_with(&format!(".{path}")));
            if ns_match(&my_ns) {
                return Some(Some("exact"));
            }
            if s.container.as_deref().is_some_and(|c| c == path || c.ends_with(&format!(".{path}")) || path.ends_with(&format!(".{c}"))) {
                return Some(Some("exact"));
            }
            // Another type's namespace: that type.
            if type_ns.iter().any(|(id, n)| *id != s.id && ns_match(n)) {
                return Some(None);
            }
            let outer_type = !m.resolve_type(path.rsplit('.').next().unwrap_or(path)).is_empty();
            return Some(if outer_type { Some("ambiguous") } else { None });
        }
        // `using Name = A.B.Name;`: the file's bare `Name` is the alias, which keeps its name.
        if std::fs::read_to_string(root.join(file)).is_ok_and(|t| csharp_aliases(&t).iter().any(|a| a == word)) {
            return Some(None);
        }
        let (ns, usings, own_global) = ns_of(root, file);
        // C# looks in the file's namespace and the enclosing ones first, innermost first, then in imported ones.
        if let Some(ns) = &ns {
            let mut cur = Some(ns.as_str());
            while let Some(n) = cur {
                let here: Vec<i64> = type_ns.iter().filter(|(_, t)| t.as_deref() == Some(n)).map(|(id, _)| *id).collect();
                if !here.is_empty() {
                    return Some(if here == [s.id] { Some("unique_name") } else if here.contains(&s.id) { Some("ambiguous") } else { None });
                }
                cur = n.rsplit_once('.').map(|(p, _)| p);
            }
        }
        let mut global: Vec<String> = global.to_vec();
        global.extend(own_global);
        let visible: Vec<i64> =
            type_ns.iter().filter(|(_, n)| sees_namespace(ns.as_deref(), &usings, &global, n.as_deref())).map(|(id, _)| *id).collect();
        Some(match (visible.contains(&s.id), visible.len()) {
            (false, _) => None,
            (true, 1) => Some("unique_name"),
            (true, _) => Some("ambiguous"),
        })
    };
    // Other types of the same name make an unexplained type name uncertain.
    let other_types = m.type_count(word).saturating_sub(1);
    // How sure an occurrence no reference explains is to mean the renamed symbol; `None` when it can't.
    let unexplained = |occ: &Occ, file: &str| -> Option<&'static str> {
        if s.is_type() {
            return Some(if other_types == 0 { "unique_name" } else { "ambiguous" });
        }
        let subtype_of_group = |q: &str| m.resolve_type(q).iter().any(|t| containers.iter().any(|c| m.supertypes(t).contains(c)));
        match &occ.qual {
            Some(q) if short_containers.contains(q) || subtype_of_group(q) => Some("exact"),
            // Another type of the repo: its own member of that name.
            Some(q) if !m.resolve_type(q).is_empty() => None,
            // A variable, or a type outside the index.
            Some(_) => Some(unique("unique_name")),
            // In TypeScript a class member always needs `this.`.
            None if s.typescript() && s.container.is_some() => None,
            None if s.container.is_none() => Some(unique("unique_name")),
            // In C#, only code inside the member's type (or a nested or derived one) names it unqualified.
            None => match m.enclosing_type(file, occ.line) {
                Some(t) if containers.iter().any(|c| m.sees_members_of(&t, c)) => Some("unique_name"),
                Some(_) => None,
                None => Some("ambiguous"),
            },
        }
    };
    let template = s.parent.and_then(|p| f.templates.get(&p)).cloned();
    let ng_input = s.attrs.iter().any(|a| matches!(a.as_str(), "Input" | "Output"));
    // Declarations of other symbols of the name (an overload, another type's member) are theirs.
    for x in m.named(word).filter(|x| !ids.contains(&x.id)) {
        if let Some((fi, occ)) = m.decl_site(x, &occs) {
            seen.insert((fi, occ.line, occ.col));
        }
    }
    // The columns of the name's occurrences in code, by file and line.
    let mut code_cols: HashMap<(usize, u32), Vec<u32>> = HashMap::new();
    for (fi, occ) in occs.of(word).iter().filter(|(_, o)| o.ctx == Ctx::Code) {
        code_cols.entry((*fi, occ.line)).or_default().push(occ.col);
    }
    let line_cols = |fi: usize, line: u32| code_cols.get(&(fi, line)).map_or(&[][..], Vec::as_slice);
    // Lines whose occurrences were read, kept or not: a reference elsewhere is one the lexer missed.
    let mut read_lines: HashSet<(String, u32)> = HashSet::new();
    for (fi, occ) in occs.of(word) {
        let file = occs.files[*fi].as_str();
        read_lines.insert((file.to_string(), occ.line));
        let key = (*fi, occ.line, occ.col);
        if seen.contains(&key) {
            continue;
        }
        let refs_here = by_line.get(&(file.to_string(), occ.line)).map(|rs| at_occurrence(rs, occ, line_cols(*fi, occ.line)));
        match occ.ctx {
            Ctx::Code if occ.namespace => {}
            // `using Alias = A.B.Name;`: the alias is a name of its own.
            Ctx::Code if occ.import && text_at(*fi, occ.line).split_once('=').is_some_and(|(lhs, _)| lhs.len() + 1 > occ.col as usize + word.len() - 1 && lhs.contains(word)) && !occ.dotted => {}
            Ctx::Code if s.is_type() && ns_verdict(&o.root, file, occ, &global_usings).is_some() => {
                let Some(Some(conf)) = ns_verdict(&o.root, file, occ, &global_usings) else { continue };
                let kind = if occ.import { "import" } else if occ.nameof { "nameof" } else { "reference" };
                site(*fi, occ, kind, conf, None, &mut sites);
            }
            Ctx::Code if occ.nameof => {
                if let Some(conf) = unexplained(occ, file) {
                    site(*fi, occ, "nameof", conf, None, &mut sites);
                }
            }
            Ctx::Code if occ.import => {
                if s.is_type() || s.container.is_none() {
                    site(*fi, occ, "import", unique("unique_name"), None, &mut sites);
                }
            }
            Ctx::Code => {
                // `new T { Name = … }`: which type's property only T says; sure when no other property has the name.
                if occ.init_assign && s.kind == "property" && s.csharp() {
                    let others = m.named(word).filter(|x| x.kind == "property" && !ids.contains(&x.id)).count();
                    site(*fi, occ, "reference", if others == 0 { "unique_name" } else { "ambiguous" }, Some("object initializer".into()), &mut sites);
                    continue;
                }
                if let Some((rs, by_col)) = &refs_here
                    && !rs.is_empty()
                {
                    let ours: Vec<&&RefHit> = rs.iter().filter(|h| reaches(h)).collect();
                    if ours.is_empty() {
                        // A reference resolved elsewhere, unless some other reference here was left unresolved.
                        if rs.iter().all(|h| h.confidence != "unresolved" || h.receiver_type.is_some()) {
                            continue;
                        }
                    } else {
                        let only_uncertain = ours.iter().all(|h| h.target_ids.iter().filter(|t| ids.contains(t)).all(|t| uncertain.contains(t)));
                        // A call that may mean an overload outside the group too.
                        let mixed = ours.iter().any(|h| h.target_ids.iter().any(|t| !ids.contains(t)));
                        let conf = if only_uncertain || mixed || ours.iter().any(|h| h.confidence == "ambiguous") {
                            "ambiguous"
                        } else if ours.iter().all(|h| h.confidence == "exact") {
                            "exact"
                        } else {
                            "unique_name"
                        };
                        // References without columns: two occurrences on a line with one reference, the other may be
                        // something else.
                        let n_occ = line_cols(*fi, occ.line).len();
                        let conf = if !by_col && n_occ > rs.len() && conf != "ambiguous" && rs.len() > ours.len() { "ambiguous" } else { conf };
                        site(*fi, occ, "reference", conf, None, &mut sites);
                        continue;
                    }
                }
                // No reference explains it: a bare identifier, or code the extractor doesn't model.
                if let Some(conf) = unexplained(occ, file) {
                    site(*fi, occ, "reference", conf, Some("not a resolved reference".into()), &mut sites);
                }
            }
            Ctx::Doc if occ.cref => {
                let conf = match &occ.qual {
                    Some(q) if short_containers.contains(q) => "exact",
                    Some(_) => "ambiguous",
                    None if group.iter().any(|g| g.file == file) => "unique_name",
                    None => unique("unique_name"),
                };
                site(*fi, occ, "cref", conf, None, &mut sites);
            }
            Ctx::Comment | Ctx::Doc => site(*fi, occ, "comment", "unique_name", None, &mut mentions),
            Ctx::Str if occ.whole_string && !kc_store::is_test_path(file) => site(*fi, occ, "string", unique("unique_name"), None, &mut sites),
            Ctx::Str => site(*fi, occ, "string", "ambiguous", None, &mut mentions),
            Ctx::Markup => {
                if template.as_deref() == Some(file) {
                    site(*fi, occ, "template", "exact", None, &mut sites);
                } else if ng_input || (s.typescript() && s.container.is_some()) {
                    site(*fi, occ, "template", "ambiguous", Some("another template binds this name".into()), &mut sites);
                } else {
                    site(*fi, occ, "config", "ambiguous", None, &mut mentions);
                }
            }
            Ctx::Text => {
                if s.kind == "property" && !s.typescript() {
                    site(*fi, occ, "config", "ambiguous", Some("a config or JSON key binding this property?".into()), &mut sites);
                } else {
                    site(*fi, occ, "config", "ambiguous", None, &mut mentions);
                }
            }
        }
    }
    // References no occurrence matched (a line the lexer read differently): keep them as sites. A target-typed
    // `new(…)` doesn't write the name: nothing there to rename.
    for h in hits.iter().filter(|h| reaches(h) && h.usage.as_deref() != Some("target_typed")) {
        if !read_lines.contains(&(h.file.clone(), h.line))
            && !sites.iter().any(|x| x.file == h.file && x.line == h.line && (h.col == 0 || x.col == 0 || x.col == h.col))
        {
            sites.push(Site {
                repo: None,
                file: h.file.clone(),
                line: h.line,
                col: h.col,
                kind: "reference",
                confidence: if h.confidence == "ambiguous" { "ambiguous" } else { h.confidence },
                text: String::new(),
                note: None,
            });
        }
    }
    // Routes: renaming an action whose route is built from its name moves the URL its clients request.
    for r in query::http_routes(conn)? {
        if r.side == "server"
            && group.iter().any(|g| g.file == r.file && g.line == r.start_line)
            && r.path.as_deref().is_some_and(|p| p.to_lowercase().split(['/', '{', '}']).any(|seg| seg == word.to_lowercase()))
        {
            sites.push(Site {
                repo: None,
                file: r.file.clone(),
                line: r.line,
                col: 0,
                kind: "route",
                confidence: "ambiguous",
                text: format!("{} /{}", r.verb, r.path.clone().unwrap_or_default()),
                note: Some("the route is built from the name: clients requesting it change too".into()),
            });
        }
    }

    // Repos depending on this one.
    let mut cross_repo = Vec::new();
    for dep_root in dependent_repos(o, &[word])? {
        let hits = dependent_hits(o, std::slice::from_ref(&dep_root), &[word], no_sync)?;
        // The dependent's `global using`s, for a C# type: from the files that usually hold them.
        let dep_global: Vec<String> = if type_ns.is_empty() || hits.is_empty() {
            Vec::new()
        } else {
            kc_index::open(&dep_root)?
                .cfiles()?
                .into_iter()
                .filter(|f| f.path.ends_with(".cs") && f.path.rsplit('/').next().is_some_and(|n| n.contains("Using")))
                .flat_map(|f| ns_of(&dep_root, &f.path).2)
                .collect()
        };
        for h in hits.into_values().flatten() {
            let to_us = h.targets.iter().any(|t| ids.contains(&t.id) && t.repo.as_deref() == Some(me.as_str()));
            if !to_us {
                continue;
            }
            let only_uncertain = h.targets.iter().filter(|t| ids.contains(&t.id) && t.repo.as_deref() == Some(me.as_str())).all(|t| uncertain.contains(&t.id));
            let text = std::fs::read_to_string(dep_root.join(&h.file)).map(|t| line_text(&t, h.line)).unwrap_or_default();
            let col = if h.col > 0 { h.col } else { text_col(&dep_root, &h.file, h.line, word) };
            let mut confidence = if only_uncertain { "ambiguous" } else { h.confidence };
            if !type_ns.is_empty() && h.file.ends_with(".cs") {
                if std::fs::read_to_string(dep_root.join(&h.file)).is_ok_and(|t| csharp_aliases(&t).iter().any(|a| a == word)) {
                    continue;
                }
                let (ns, usings, own_global) = ns_of(&dep_root, &h.file);
                let mut global = dep_global.clone();
                global.extend(own_global);
                let qualified = text.contains(&format!(".{word}"));
                if !qualified && !sees_namespace(ns.as_deref(), &usings, &global, my_ns.as_deref()) {
                    // Nothing here imports the type's namespace: another type of that name.
                    continue;
                }
                if qualified && !my_ns.as_deref().is_some_and(|n| text.contains(&format!("{}.{word}", n.rsplit('.').next().unwrap_or(n)))) {
                    confidence = "ambiguous";
                }
            }
            cross_repo.push(Site {
                repo: h.repo.clone(),
                file: h.file.clone(),
                line: h.line,
                col,
                kind: "reference",
                confidence,
                note: (confidence != h.confidence).then(|| "qualified by another namespace?".to_string()),
                text,
            });
        }
    }

    // Two references on one line (`new List<T>`: a type use and an instantiation) are one site.
    cross_repo.sort_by(|a, b| (&a.repo, &a.file, a.line, a.col).cmp(&(&b.repo, &b.file, b.line, b.col)));
    cross_repo.dedup_by(|a, b| a.repo == b.repo && a.file == b.file && a.line == b.line && a.col == b.col);
    let collisions = collisions(m, s, &group, new);
    sites.sort_by(|a, b| (&a.file, a.line, a.col).cmp(&(&b.file, b.line, b.col)));
    sites.dedup_by(|a, b| a.file == b.file && a.line == b.line && a.col == b.col && a.col != 0);
    mentions.sort_by(|a, b| (&a.file, a.line, a.col).cmp(&(&b.file, b.line, b.col)));
    let uncertain = sites.iter().chain(&cross_repo).filter(|x| x.confidence != "exact" && x.confidence != "unique_name").count();
    if uncertain > 0 {
        reasons.push(format!("{uncertain} ambiguous site(s)"));
        codes.push(Code::Ambiguous);
    }
    if !collisions.is_empty() {
        reasons.push(format!("{} collision(s) with `{new}`", collisions.len()));
        codes.push(Code::Collision);
    }
    Ok(RenamePreview {
        symbol: s.qualified(),
        kind: s.kind.clone(),
        file: s.file.clone(),
        line: s.line,
        new_name: new.to_string(),
        group: group.iter().map(|g| format!("{} ({}:{})", g.qualified(), g.file, g.line)).collect(),
        sites,
        cross_repo,
        collisions,
        mentions,
        refused: !reasons.is_empty() && !allow_ambiguous,
        reasons,
        codes,
    })
}

/// The references on an occurrence's line that are that occurrence. When each reference there has a column one of
/// the name's occurrences on the line has, those at this occurrence's column (`true`): another occurrence on the line
/// may be another symbol's, and never borrows this one's resolution. Otherwise (references without columns, or with
/// ones the lexer reads differently) all of them (`false`).
fn at_occurrence<'h>(rs: &[&'h RefHit], occ: &Occ, cols: &[u32]) -> (Vec<&'h RefHit>, bool) {
    if rs.iter().all(|h| h.col > 0 && cols.contains(&h.col)) {
        (rs.iter().copied().filter(|h| h.col == occ.col).collect(), true)
    } else {
        (rs.to_vec(), false)
    }
}

/// The 1-based column of `word` on `line` of a file, or 0.
fn text_col(root: &Path, file: &str, line: u32, word: &str) -> u32 {
    let Ok(text) = std::fs::read_to_string(root.join(file)) else { return 0 };
    let Some(l) = text.lines().nth(line.saturating_sub(1) as usize) else { return 0 };
    let b = l.as_bytes();
    let mut from = 0;
    while let Some(i) = l[from..].find(word).map(|i| i + from) {
        let j = i + word.len();
        if (i == 0 || !ident_byte(b[i - 1])) && (j >= b.len() || !ident_byte(b[j])) {
            return i as u32 + 1;
        }
        from = i + 1;
    }
    0
}

/// Symbols the new name clashes with: for a member, a member of that name in a renamed member's type or anywhere in
/// its hierarchy; for a type, another type of that name in the repo.
fn collisions(m: &Model, s: &Sym, group: &[&Sym], new: &str) -> Vec<Collision> {
    let mut out = Vec::new();
    let mut push = |x: &Sym, why: String| {
        if !out.iter().any(|c: &Collision| c.file == x.file && c.line == x.line) {
            out.push(Collision { qualified: x.qualified(), kind: x.kind.clone(), file: x.file.clone(), line: x.line, why });
        }
    };
    if s.is_type() {
        for x in m.named(new).filter(|x| x.is_type()) {
            push(x, format!("a type named {new} already exists"));
        }
        if let Some(c) = &s.container {
            for x in m.members_named(c, new) {
                push(x, format!("{c} already has a member named {new}"));
            }
        }
        return out;
    }
    for g in group {
        let Some(c) = &g.container else {
            for x in m.named(new).filter(|x| x.container.is_none() && x.file == g.file) {
                push(x, format!("{} already declares {new}", x.file));
            }
            continue;
        };
        for t in std::iter::once(c.clone()).chain(m.supertypes(c)).chain(m.subtypes(c)) {
            for x in m.members_named(&t, new) {
                let why = if &t == c { format!("{t} already has a member named {new}") } else { format!("{t}, in {c}'s hierarchy, has a member named {new}") };
                push(x, why);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------------------------
// Text output (kept here so the shared renderers stay untouched).

pub fn render_unused(repos: &[UnusedRepo]) {
    for r in repos {
        let exempt: usize = r.exempt.values().sum();
        // The answer first, in one line; then the confident list, compact; then the uncertain one with reasons.
        println!(
            "{} unused, {} possibly unused in {} (of {} candidates; {} exempt){}",
            r.unused,
            r.possibly_unused,
            r.repo,
            r.candidates,
            exempt,
            if r.library { "; a library: public API is at most possibly unused" } else { "" }
        );
        if !r.published.is_empty() {
            println!("  published packages (public API at most possibly unused): {}", r.published.join(", "));
        }
        if !r.dependents.is_empty() {
            println!("  checked dependents: {}", r.dependents.join(", "));
        }
        let unused: Vec<&UnusedItem> = r.items.iter().filter(|i| i.tier == "unused").collect();
        let possibly: Vec<&UnusedItem> = r.items.iter().filter(|i| i.tier != "unused").collect();
        if !unused.is_empty() {
            println!("\nunused ({}; nothing reaches them):", unused.len());
            for i in unused {
                println!("  {:<8} {}  {}:{}", crate::ops::kind_word(&i.kind, crate::ops::file_lang(&i.file)), i.qualified, i.file, i.line);
            }
        }
        if !possibly.is_empty() {
            println!("\npossibly unused ({}; only uncertain evidence reaches them):", possibly.len());
            for i in possibly {
                let more = if i.reasons.len() > 1 { format!(" (+{} more)", i.reasons.len() - 1) } else { String::new() };
                let why = i.reasons.first().map(|w| format!("  — {w}{more}")).unwrap_or_default();
                println!("  {:<8} {}  {}:{}{why}", crate::ops::kind_word(&i.kind, crate::ops::file_lang(&i.file)), i.qualified, i.file, i.line);
            }
        }
        if !r.exempt.is_empty() {
            println!("\nnever reported:");
            for (why, n) in &r.exempt {
                println!("  {n:>5}  {why}");
            }
        }
    }
}

pub fn render_rename(p: &RenamePreview) {
    println!("rename {} {} ({}:{}) to {}", crate::ops::kind_word(&p.kind, crate::ops::file_lang(&p.file)), p.symbol, p.file, p.line, p.new_name);
    if p.group.len() > 1 {
        println!("renamed together:");
        for g in &p.group {
            println!("  {g}");
        }
    }
    let show = |s: &Site| {
        let repo = s.repo.as_deref().map(|r| format!("{r}: ")).unwrap_or_default();
        let note = s.note.as_deref().map(|n| format!("  ({n})")).unwrap_or_default();
        println!("  {repo}{}:{}:{}  {} [{}]  {}{note}", s.file, s.line, s.col, s.kind, s.confidence, s.text);
    };
    println!("\n{} site(s):", p.sites.len());
    p.sites.iter().for_each(show);
    if !p.cross_repo.is_empty() {
        println!("\nin other repos, {} site(s):", p.cross_repo.len());
        p.cross_repo.iter().for_each(show);
    }
    if !p.collisions.is_empty() {
        println!("\ncollisions:");
        for c in &p.collisions {
            println!("  {} {}  {}:{}  {}", c.kind, c.qualified, c.file, c.line, c.why);
        }
    }
    if !p.mentions.is_empty() {
        println!("\nmentions (comments and prose strings, not edited): {}", p.mentions.len());
        for m in p.mentions.iter().take(10) {
            show(m);
        }
    }
    if p.refused {
        println!("\nrefused: {}. Pass --allow-ambiguous to preview anyway.", p.reasons.join("; "));
    } else if !p.reasons.is_empty() {
        println!("\nallowed despite: {}", p.reasons.join("; "));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::tests::{repo, test_cache};

    fn words(list: &[&'static str]) -> HashSet<&'static str> {
        list.iter().copied().collect()
    }

    fn ctxs(found: &[(String, Occ)]) -> Vec<(u32, Ctx)> {
        found.iter().map(|(_, o)| (o.line, o.ctx)).collect()
    }

    #[test]
    fn scan_classes_each_occurrence() {
        let text = "\u{feff}using A.Save;\nusing IScope s = Save();\n/// <see cref=\"Repo.Save\"/>\nvoid Save() { x.Save(); // Save\n  var n = nameof(Repo.Save); log(\"Save\"); log(@\"a \"\"Save\"\" b\"); }\n/* Save */ s = 'Save';\n";
        let found = scan(text, FileKind::CLike, &words(&["Save"]));
        assert_eq!(
            ctxs(&found),
            [
                (1, Ctx::Code),
                (2, Ctx::Code),
                (3, Ctx::Doc),
                (4, Ctx::Code),
                (4, Ctx::Code),
                (4, Ctx::Comment),
                (5, Ctx::Code),
                (5, Ctx::Str),
                (5, Ctx::Str),
                (6, Ctx::Comment),
                (6, Ctx::Str),
            ]
        );
        let o: Vec<&Occ> = found.iter().map(|(_, o)| o).collect();
        // `using A.Save;` imports; a using declaration doesn't.
        assert!(o[0].import && !o[1].import);
        assert!(o[2].cref && o[2].qual.as_deref() == Some("Repo"));
        assert!(!o[3].dotted && o[4].dotted && o[4].qual.as_deref() == Some("x"));
        assert!(o[6].nameof && o[6].path.as_deref() == Some("Repo"));
        assert!(o[7].whole_string && !o[8].whole_string && o[10].whole_string);
        assert_eq!((o[3].line, o[3].col), (4, 6));
    }

    #[test]
    fn scan_reads_markup_comments_and_import_lists() {
        let html = "<button (click)=\"save()\">save</button>\n<!-- save -->\n";
        let found = scan(html, FileKind::Markup, &words(&["save"]));
        assert_eq!(ctxs(&found), [(1, Ctx::Markup), (1, Ctx::Markup), (2, Ctx::Comment)]);
        let ts = "import {\n  save,\n} from './x';\nexport const y = save();\nnamespace save.x {}\n";
        let found = scan(ts, FileKind::CLike, &words(&["save"]));
        let flags: Vec<(bool, bool)> = found.iter().map(|(_, o)| (o.import, o.namespace)).collect();
        assert_eq!(flags, [(true, false), (false, false), (false, true)]);
    }

    #[test]
    fn scan_marks_object_initializer_assignments_and_aliases() {
        let text = "using Name = A.B.Name;\nvar o = new T { Name = x.Name,\n    Name = 1 };\nif (Name == 2) { }\n";
        let found = scan(text, FileKind::CLike, &words(&["Name"]));
        let init: Vec<(u32, bool)> = found.iter().map(|(_, o)| (o.line, o.init_assign)).collect();
        assert_eq!(init, [(1, false), (1, false), (2, true), (2, false), (3, true), (4, false)]);
        assert_eq!(csharp_aliases(text), ["Name"]);
    }

    #[test]
    fn parameter_types_tell_overloads_apart() {
        assert_eq!(param_types("public Task Save(int id, string name = null)").unwrap(), ["int", "string"]);
        assert_eq!(param_types("Task<R> Submit(Action<Builder> b, bool auto = true)").unwrap(), ["Action<Builder>", "bool"]);
        assert_eq!(param_types("static void Ext(this IList<T> xs, params object[] rest)").unwrap(), ["IList<T>", "object[]"]);
        assert_eq!(param_types("save(id: number, name?: string)").unwrap(), ["number", "string"]);
        assert!(param_types("public string Name").is_none());
        assert!(same_params("void A(System.String s)", "void A(String? t)"));
        assert!(!same_params("void A(int a)", "void A(int a, int b)"));
        assert!(same_params("public int Count", "int Count"));
    }

    #[test]
    fn helpers_match_globs_paths_and_namespaces() {
        assert!(glob_match("*Service", "OrderService") && !glob_match("*Service", "OrderRepo") && glob_match("Order*", "OrderRepo"));
        assert_eq!(join_relative("src/app/x/x.component.ts", "./x.component.html"), "src/app/x/x.component.html");
        assert_eq!(join_relative("src/A/A.csproj", "../B/B.csproj"), "src/B/B.csproj");
        let (ns, usings, global) = csharp_scopes("global using Shop.Common;\nusing Shop.Models;\nusing X = Shop.Y;\nnamespace Shop.Api.Orders;\n");
        assert_eq!((ns.as_deref(), usings.as_slice(), global.as_slice()), (Some("Shop.Api.Orders"), &["Shop.Models".to_string()][..], &["Shop.Common".to_string()][..]));
        assert!(sees_namespace(ns.as_deref(), &usings, &global, Some("Shop.Api")));
        assert!(sees_namespace(ns.as_deref(), &usings, &global, Some("Shop.Common")));
        assert!(!sees_namespace(ns.as_deref(), &usings, &global, Some("Shop.Entities")));
    }

    fn tiers(out: &[UnusedRepo]) -> Vec<(String, &'static str)> {
        let mut v: Vec<(String, &'static str)> = out.iter().flat_map(|r| r.items.iter().map(|i| (i.qualified.clone(), i.tier))).collect();
        v.sort();
        v
    }

    #[test]
    fn unused_reports_dead_code_and_holds_back_uncertain_and_framework_code() {
        let cache = test_cache();
        let base = cache.dir("unused");
        let root = repo(
            &base,
            "shop",
            &[
                ("src/Shop.csproj", "<Project Sdk=\"Microsoft.NET.Sdk.Web\"></Project>\n"),
                ("tests/Shop.Tests.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"><ItemGroup><ProjectReference Include=\"..\\src\\Shop.csproj\" /></ItemGroup></Project>\n"),
                (
                    "src/Program.cs",
                    "namespace Shop;\npublic static class Program\n{\n    public static void Main()\n    {\n        var s = new OrderService();\n        s.Place(1);\n        IRepo r = new Repo();\n        r.Save();\n    }\n}\n",
                ),
                (
                    "src/OrderService.cs",
                    "namespace Shop;\n\
                     public class OrderService\n{\n\
                         public void Place(int id) { Check(id); }\n\
                         private void Check(int id) { }\n\
                         public void Forgotten() { }\n\
                         public void TestedOnly() { }\n\
                         public void Reflected() { }\n\
                         public void Recurse() { Recurse(); }\n\
                         public override string ToString() => \"o\";\n\
                         public void Dispose() { }\n\
                     }\n\
                     public interface IRepo { void Save(); void Drop(); }\n\
                     public class Repo : IRepo { public void Save() { } public void Drop() { } }\n\
                     public class Lonely { }\n\
                     public static class Ext { public static int Twice(this int x) => x * 2; }\n\
                     public static class Usage { public static int Run() => 3.Twice(); }\n",
                ),
                ("src/Invoker.cs", "namespace Shop;\npublic class Invoker\n{\n    public object Go(object o) => o.GetType().GetMethod(\"Reflected\");\n}\n"),
                (
                    "src/OrdersController.cs",
                    "namespace Shop;\n[ApiController]\npublic class OrdersController : ControllerBase\n{\n    [HttpGet(\"orders\")]\n    public string Get() => \"x\";\n}\n",
                ),
                ("src/OrderDto.cs", "namespace Shop;\npublic class OrderDto\n{\n    public int Id { get; set; }\n    public string Note { get; set; }\n}\n"),
                ("tests/OrderTests.cs", "public class OrderTests\n{\n    [Fact]\n    public void Works() { new OrderService().TestedOnly(); }\n}\n"),
            ],
        );
        let scope = Scope { repos: vec![root.clone()], no_sync: false };
        let p = UnusedParams { kinds: Vec::new(), tier: "all".into(), limit: 100 };
        let out = unused(&p, &scope).unwrap();
        let got = tiers(&out.data);
        let want: Vec<(String, &str)> = vec![
            ("IRepo.Drop".into(), "unused"),
            ("Invoker".into(), "unused"),
            ("Invoker.Go".into(), "unused"),
            ("Lonely".into(), "unused"),
            ("OrderDto".into(), "unused"),
            ("OrderService.Forgotten".into(), "unused"),
            ("OrderService.Recurse".into(), "unused"),
            ("OrderService.Reflected".into(), "possibly_unused"),
            ("OrderService.TestedOnly".into(), "possibly_unused"),
            ("Usage".into(), "unused"),
            ("Usage.Run".into(), "unused"),
        ];
        assert_eq!(got, want, "{:#?}", out.data);
        let reasons = |q: &str| out.data[0].items.iter().find(|i| i.qualified == q).unwrap().reasons.clone();
        assert!(reasons("OrderService.Reflected")[0].starts_with("string at src/Invoker.cs:4"), "{:?}", reasons("OrderService.Reflected"));
        assert!(reasons("OrderService.TestedOnly")[0].starts_with("used only by tests"));
        let exempt = &out.data[0].exempt;
        assert!(exempt.contains_key("controller action") && exempt.contains_key("framework convention"));
        assert!(exempt.contains_key("interface implementation") && exempt.contains_key("data property (a serialized, bound or mapped type)"));
        assert_eq!(out.env["items"][0]["unused"], 9);

        // Only methods, only the confident tier.
        let p = UnusedParams { kinds: vec!["method".into()], tier: "unused".into(), limit: 100 };
        let got = tiers(&unused(&p, &scope).unwrap().data);
        assert!(got.iter().all(|(q, t)| *t == "unused" && q.contains('.')), "{got:?}");
        assert!(kind_filter(&["widget".into()]).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn unused_never_calls_an_extension_class_its_calls_may_reach_unused() {
        let cache = test_cache();
        let base = cache.dir("unused-ext");
        let root = repo(
            &base,
            "shop",
            &[
                ("src/Shop.csproj", "<Project Sdk=\"Microsoft.NET.Sdk.Web\"></Project>\n"),
                ("src/Status.cs", "namespace Shop.Models;\npublic enum Status { Open, Closed }\npublic class Order : EntityBase { public Status State { get; set; } }\n"),
                // An extension on `System.Enum`, called on a property of an enum type: resolved exactly.
                ("src/EnumExt.cs", "using System;\nnamespace Shop.Common;\npublic static class EnumExt\n{\n    public static string Describe(this Enum value) => value.ToString();\n}\n"),
                // An extension on a library interface, which the receiver's library base class may implement: the
                // call through a repo type is unresolved.
                ("src/NamedExt.cs", "namespace Shop.Common;\npublic static class NamedExt\n{\n    public static string Label(this INamed x) => x.Name;\n}\n"),
                (
                    "src/Program.cs",
                    "using Shop.Common;\nusing Shop.Models;\nnamespace Shop;\npublic static class Program\n{\n    public static void Main(Order o)\n    {\n        var d = o.State.Describe();\n        var t = o.Label();\n    }\n}\n",
                ),
            ],
        );
        let scope = Scope { repos: vec![root.clone()], no_sync: false };
        let p = UnusedParams { kinds: Vec::new(), tier: "all".into(), limit: 100 };
        let out = unused(&p, &scope).unwrap();
        let got = tiers(&out.data);
        // `EnumExt` and its method are used; `NamedExt` only possibly unused, for the unresolved `Label` call.
        assert_eq!(got, [("INamed.Label".to_string(), "possibly_unused"), ("NamedExt".to_string(), "possibly_unused")]);
        let reasons = &out.data[0].items.iter().find(|i| i.qualified == "INamed.Label").unwrap().reasons;
        assert!(reasons[0].starts_with("unresolved reference at src/Program.cs:9"), "{reasons:?}");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn extension_calls_resolve_to_the_extension_in_scope_across_repos() {
        let cache = test_cache();
        let base = cache.dir("ext-x");
        let core = repo(
            &base,
            "core",
            &[
                ("src/Core.Lib.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup><PackageId>Core.Lib</PackageId></PropertyGroup>\n</Project>\n"),
                ("src/EnumExtensions.cs", "using System;\nnamespace Core.Lib.Extensions;\npublic static class EnumExtensions\n{\n    public static string Describe(this Enum value) => value.ToString();\n}\n"),
            ],
        );
        // An API declaring its own copy of the extension, beside the models it publishes.
        let api = repo(
            &base,
            "api",
            &[
                ("src/Api.Models/Api.Models.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup><PackageId>Api.Models</PackageId></PropertyGroup>\n</Project>\n"),
                ("src/Api.Models/Events.cs", "namespace Api.Models.Events;\npublic class OrderPlaced { }\npublic enum Sha { Sha1, Sha256 }\npublic class Request { public Sha Sha { get; set; } }\n"),
                ("src/Api/Api.csproj", "<Project Sdk=\"Microsoft.NET.Sdk.Web\"></Project>\n"),
                ("src/Api/CommonExtensions.cs", "using System;\nnamespace Api.Service;\npublic static class CommonExtensions\n{\n    public static string Describe(this Enum value) => value.ToString();\n}\n"),
                (
                    "src/Api/Program.cs",
                    "using Api.Models.Events;\nnamespace Api.Service;\npublic static class Program\n{\n    public static void Main(Request request)\n    {\n        var d = request.Sha.Describe();\n    }\n}\n",
                ),
            ],
        );
        // A consumer of both, seeing only the core library's extension.
        let mail = repo(
            &base,
            "mail",
            &[
                (
                    "src/Mail.csproj",
                    "<Project Sdk=\"Microsoft.NET.Sdk.Web\">\n  <ItemGroup><PackageReference Include=\"Api.Models\" Version=\"1.*\" /><PackageReference Include=\"Core.Lib\" Version=\"1.*\" /></ItemGroup>\n</Project>\n",
                ),
                (
                    "src/Builder.cs",
                    "using Core.Lib.Extensions;\nusing Api.Models.Events;\nnamespace Mail;\npublic enum Flag { One }\npublic class Builder\n{\n    public string Build(Flag flag) => flag.Describe();\n}\n",
                ),
            ],
        );
        type Seen = (Option<String>, String, u32, &'static str, Vec<String>);
        let refs_in = |root: &PathBuf| -> Vec<Seen> {
            let d = crate::ops::refs("Describe", &[], 50, &Scope { repos: vec![root.clone()], no_sync: false }).unwrap().data;
            d.items
                .iter()
                .map(|(_, h)| {
                    let targets = h.targets.iter().map(|t| format!("{}{}", t.repo.as_deref().map(|r| format!("{r}/")).unwrap_or_default(), t.file)).collect();
                    (h.repo.clone(), h.file.clone(), h.line, h.confidence, targets)
                })
                .collect()
        };
        // In the API, the call on an enum property is its own extension's, exactly.
        assert_eq!(refs_in(&api), [(None, "src/Api/Program.cs".to_string(), 7, "exact", vec!["src/Api/CommonExtensions.cs".to_string()])]);
        // The consumer's call is the core library's (its `using`), never the API's same-named extension.
        assert_eq!(refs_in(&core), [(Some("mail".to_string()), "src/Builder.cs".to_string(), 7, "exact", vec!["core/src/EnumExtensions.cs".to_string()])]);
        let _ = mail;
        // So the API's extension class is used.
        let p = UnusedParams { kinds: Vec::new(), tier: "all".into(), limit: 100 };
        let out = unused(&p, &Scope { repos: vec![api], no_sync: false }).unwrap();
        assert!(out.data[0].items.iter().all(|i| !i.qualified.contains("CommonExtensions") && i.qualified != "Enum.Describe"), "{:#?}", out.data);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn unused_reads_angular_templates_and_namespace_imports() {
        let cache = test_cache();
        let base = cache.dir("unused-ng");
        let root = repo(
            &base,
            "web",
            &[
                ("package.json", "{ \"name\": \"web\", \"private\": true }\n"),
                (
                    "src/app/cart.component.ts",
                    "import * as fromCart from './cart.selectors';\n@Component({ selector: 'app-cart', templateUrl: './cart.component.html' })\nexport class CartComponent {\n  total = 0;\n  hidden = 1;\n  constructor(private store: Store) {}\n  save(): void { this.store.select(fromCart.selectItems); }\n  discard(): void { }\n}\n",
                ),
                ("src/app/cart.component.html", "<button (click)=\"save()\">{{ total }}</button>\n"),
                ("src/app/cart.selectors.ts", "export const selectItems = createSelector(s => s.items);\nexport const selectNothing = createSelector(s => s.none);\n"),
            ],
        );
        let p = UnusedParams { kinds: Vec::new(), tier: "all".into(), limit: 100 };
        let out = unused(&p, &Scope { repos: vec![root], no_sync: false }).unwrap();
        let got = tiers(&out.data);
        // `save` and `total` are bound in the template; `selectItems` is read through the namespace import, which
        // resolves into its module; `selectNothing`, `hidden` and `discard` are dead.
        assert_eq!(
            got,
            vec![
                ("CartComponent.discard".to_string(), "unused"),
                ("CartComponent.hidden".to_string(), "unused"),
                ("selectNothing".to_string(), "unused"),
            ],
            "{:#?}",
            out.data
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    fn sites(p: &RenamePreview) -> Vec<(String, u32, &'static str, &'static str)> {
        p.sites.iter().map(|s| (s.file.clone(), s.line, s.kind, s.confidence)).collect()
    }

    #[test]
    fn rename_lists_the_hierarchy_references_and_names_and_refuses_ambiguity() {
        let cache = test_cache();
        let base = cache.dir("rename");
        let root = repo(
            &base,
            "store",
            &[
                ("src/Store.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n"),
                (
                    "src/Repo.cs",
                    "namespace Store;\npublic interface IRepo\n{\n    /// <summary>Saves.</summary>\n    void Save(int id);\n    void Save(int id, bool now);\n}\n\
                     public class Repo : IRepo\n{\n    public void Save(int id) { }\n    public void Save(int id, bool now) { }\n    public void Load() { }\n}\n\
                     public class Other\n{\n    public void Save(int id) { }\n}\n",
                ),
                (
                    "src/Service.cs",
                    "namespace Store;\npublic class Service\n{\n    private readonly IRepo _repo;\n    /// Calls <see cref=\"IRepo.Save\"/>.\n    public void Run()\n    {\n        _repo.Save(1);\n        Log(nameof(IRepo.Save));\n        Log(\"Save\");\n        Log(\"Save failed\");\n    }\n    private void Log(string s) { }\n}\n",
                ),
            ],
        );
        let scope = Scope { repos: vec![root.clone()], no_sync: false };
        let p = RenameParams { symbol: "src/Repo.cs:5".into(), new_name: "Store".into(), allow_ambiguous: false };
        let out = rename(&p, &scope).unwrap();
        let d = &out.data;
        // The interface member and its one-argument implementation, not the overloads or `Other.Save`.
        assert_eq!(d.group.len(), 2, "{:?}", d.group);
        assert_eq!(
            sites(d),
            vec![
                ("src/Repo.cs".to_string(), 5, "definition", "exact"),
                ("src/Repo.cs".to_string(), 10, "implementation", "exact"),
                ("src/Service.cs".to_string(), 5, "cref", "exact"),
                ("src/Service.cs".to_string(), 8, "reference", "exact"),
                ("src/Service.cs".to_string(), 9, "nameof", "exact"),
                ("src/Service.cs".to_string(), 10, "string", "ambiguous"),
            ],
            "{d:#?}"
        );
        // The string is a guess (other symbols are named `Save`): refused, with the reason.
        assert!(d.refused && d.reasons.iter().any(|r| r.contains("ambiguous")), "{:?}", d.reasons);
        assert_eq!(d.mentions.iter().map(|m| (m.line, m.kind)).collect::<Vec<_>>(), [(11, "string")]);
        assert_eq!(out.env["refused"], true);

        // A name `Repo` already uses collides.
        let p = RenameParams { symbol: "src/Repo.cs:5".into(), new_name: "Load".into(), allow_ambiguous: true };
        let d = rename(&p, &scope).unwrap().data;
        assert_eq!(d.collisions.iter().map(|c| c.qualified.as_str()).collect::<Vec<_>>(), ["Repo.Load"]);
        assert!(!d.refused && !d.reasons.is_empty());

        // Several definitions: pick one.
        let p = RenameParams { symbol: "Save".into(), new_name: "Keep".into(), allow_ambiguous: false };
        assert!(rename(&p, &scope).is_err());
        let p = RenameParams { symbol: "Repo.Load".into(), new_name: "class".into(), allow_ambiguous: false };
        assert!(rename(&p, &scope).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn rename_finds_template_bindings_and_sites_in_dependent_repos() {
        let cache = test_cache();
        let base = cache.dir("rename-x");
        let lib = repo(
            &base,
            "lib",
            &[
                ("src/Shop.Models.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup><PackageId>Shop.Models</PackageId></PropertyGroup>\n</Project>\n"),
                ("src/Order.cs", "namespace Shop.Models;\npublic class Order\n{\n    public int Total() => 1;\n}\n"),
            ],
        );
        let app = repo(
            &base,
            "app",
            &[
                ("src/App.csproj", "<Project Sdk=\"Microsoft.NET.Sdk.Web\">\n  <ItemGroup><PackageReference Include=\"Shop.Models\" Version=\"1.*\" /></ItemGroup>\n</Project>\n"),
                ("src/Checkout.cs", "using Shop.Models;\nnamespace App;\npublic class Checkout\n{\n    public int Sum(Order o) => o.Total();\n}\n"),
            ],
        );
        let p = RenameParams { symbol: "Order.Total".into(), new_name: "Amount".into(), allow_ambiguous: false };
        let d = rename(&p, &Scope { repos: vec![lib.clone()], no_sync: false }).unwrap().data;
        let cross: Vec<(Option<&str>, &str, u32, &str)> = d.cross_repo.iter().map(|s| (s.repo.as_deref(), s.file.as_str(), s.line, s.confidence)).collect();
        assert_eq!(cross, [(Some("app"), "src/Checkout.cs", 5, "exact")], "{d:#?}");
        assert!(!d.refused, "{:?}", d.reasons);
        // The dependent's use keeps it from being unused; its other public API is held back.
        let p = UnusedParams { kinds: Vec::new(), tier: "all".into(), limit: 100 };
        let out = unused(&p, &Scope { repos: vec![lib], no_sync: false }).unwrap();
        assert_eq!(out.data[0].dependents, ["app"]);
        assert!(out.data[0].items.iter().all(|i| i.qualified != "Order.Total"), "{:#?}", out.data);

        let web = repo(
            &base,
            "web",
            &[
                (
                    "src/app/cart.component.ts",
                    "@Component({ selector: 'app-cart', templateUrl: './cart.component.html' })\nexport class CartComponent {\n  total = 0;\n  refresh(): void { this.total = 1; }\n}\n",
                ),
                ("src/app/cart.component.html", "<span>{{ total }}</span>\n"),
            ],
        );
        let p = RenameParams { symbol: "CartComponent.total".into(), new_name: "sum".into(), allow_ambiguous: false };
        let d = rename(&p, &Scope { repos: vec![web], no_sync: false }).unwrap().data;
        assert_eq!(
            sites(&d),
            vec![
                ("src/app/cart.component.html".to_string(), 1, "template", "exact"),
                ("src/app/cart.component.ts".to_string(), 3, "definition", "exact"),
                ("src/app/cart.component.ts".to_string(), 4, "reference", "exact"),
            ],
            "{d:#?}"
        );
        let _ = (app, std::fs::remove_dir_all(&base));
    }

    /// Sites in `file` with their columns: (line, column, kind, confidence).
    fn at(p: &RenamePreview, file: &str) -> Vec<(u32, u32, &'static str, &'static str)> {
        p.sites.iter().filter(|s| s.file == file).map(|s| (s.line, s.col, s.kind, s.confidence)).collect()
    }

    /// The 1-based column of the method name in the first `needle` (`first.Save`) on `line`.
    fn col_of(line: &str, needle: &str) -> u32 {
        (line.find(needle).unwrap() + needle.find('.').unwrap() + 2) as u32
    }

    #[test]
    fn rename_edits_only_the_occurrences_on_a_line_that_resolve_to_it() {
        let cache = test_cache();
        let base = cache.dir("rename-line");
        let line3 = "public class Caller { public void Run(FirstStore first, SecondStore second) { first.Save(); second.Save(); } }";
        let line4 = "public class Loose { public void Run(FirstStore first) { first.Save(); var x = Pick(); x.Save(); } }";
        let cs = format!(
            "public class FirstStore {{ public void Save() {{}} }}\npublic class SecondStore {{ public void Save() {{}} }}\n{line3}\n{line4}\n"
        );
        let ts = "export class First { save(): void {} }\nexport class Second { save(): void {} }\n\
                  export function run(a: First, b: Second): void { b.save(); a.save(); b.save(); }\n";
        let root = repo(&base, "app", &[("a.cs", cs.as_str()), ("run.ts", ts)]);
        let scope = Scope { repos: vec![root], no_sync: false };

        let p = RenameParams { symbol: "FirstStore.Save".into(), new_name: "Submit".into(), allow_ambiguous: false };
        let d = rename(&p, &scope).unwrap().data;
        // `second.Save()` beside `first.Save()` is `SecondStore.Save`'s: not a site.
        assert_eq!(
            at(&d, "a.cs"),
            [
                (1, 39, "definition", "exact"),
                (3, col_of(line3, "first.Save"), "reference", "exact"),
                (4, col_of(line4, "first.Save"), "reference", "exact"),
                (4, col_of(line4, "x.Save"), "reference", "ambiguous"),
            ],
            "{d:#?}"
        );
        // `x.Save()` through a receiver of unknown type may be either: the preview refuses rather than guess.
        assert!(d.refused, "{:?}", d.reasons);

        let p = RenameParams { symbol: "SecondStore.Save".into(), new_name: "Submit".into(), allow_ambiguous: true };
        let d = rename(&p, &scope).unwrap().data;
        let line_3: Vec<_> = at(&d, "a.cs").into_iter().filter(|s| s.0 == 3).collect();
        assert_eq!(line_3, [(3, col_of(line3, "second.Save"), "reference", "exact")], "{d:#?}");

        // TypeScript: the one `a.save()` between two `b.save()` calls.
        let p = RenameParams { symbol: "First.save".into(), new_name: "submit".into(), allow_ambiguous: false };
        let d = rename(&p, &scope).unwrap().data;
        let l3 = ts.lines().nth(2).unwrap();
        assert_eq!(at(&d, "run.ts"), [(1, 22, "definition", "exact"), (3, col_of(l3, "a.save"), "reference", "exact")], "{d:#?}");
        assert!(!d.refused, "{:?}", d.reasons);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Invariant: a rename preview never offers to edit an occurrence whose reference resolves, exactly, to a symbol
    /// outside the renamed group, however the calls are laid out on lines.
    #[test]
    fn invariant_rename_never_includes_an_occurrence_resolved_to_another_symbol() {
        let cache = test_cache();
        let base = cache.dir("rename-invariant");
        let types = ["Alpha", "Beta", "Gamma"];
        let mut cs = String::new();
        for t in types {
            cs.push_str(&format!("public class {t} {{ public void Save() {{}} public void Load() {{}} }}\n"));
        }
        // Each order of the three receivers written all on one line, then two on a line and one on the next.
        let orders = [["a", "b", "c"], ["c", "a", "b"], ["b", "c", "a"]];
        let mut body = Vec::new();
        for (i, o) in orders.iter().enumerate() {
            let m = if i % 2 == 0 { "Save" } else { "Load" };
            let c: Vec<String> = o.iter().map(|v| format!("{v}.{m}();")).collect();
            body.push(c.join(" "));
            body.push(format!("{} {}", c[0], c[1]));
            body.push(c[2].clone());
        }
        cs.push_str(&format!("public class User\n{{\n    public void Run(Alpha a, Beta b, Gamma c)\n    {{\n{}\n    }}\n}}\n", body.join("\n")));
        let root = repo(&base, "app", &[("a.cs", cs.as_str())]);
        let scope = Scope { repos: vec![root.clone()], no_sync: false };
        let opened = crate::ops::open_fresh(&root, false).unwrap();
        let hits = query::refs_named(opened.store.conn(), &["Save", "Load"]).unwrap();
        for t in types {
            for m in ["Save", "Load"] {
                let p = RenameParams { symbol: format!("{t}.{m}"), new_name: "Other".into(), allow_ambiguous: true };
                let d = rename(&p, &scope).unwrap().data;
                let refs: Vec<&Site> = d.sites.iter().filter(|s| s.kind == "reference").collect();
                // Each call of `m` through this type's variable, written twice per order calling `m`.
                let calling = orders.iter().enumerate().filter(|(i, _)| (i % 2 == 0) == (m == "Save")).count();
                assert_eq!(refs.len(), calling * 2, "{t}.{m}: {d:#?}");
                for s in refs {
                    let here: Vec<&RefHit> = hits.iter().filter(|h| h.file == s.file && h.line == s.line && h.col == s.col).collect();
                    assert!(!here.is_empty(), "{t}.{m}: a site no reference explains: {s:?}");
                    for h in here {
                        let elsewhere = h.confidence == "exact" && h.targets.iter().all(|x| x.qualified != format!("{t}.{m}"));
                        assert!(!elsewhere, "{t}.{m}: site {s:?} resolves to {:?}", h.targets);
                    }
                }
            }
        }
        let _ = std::fs::remove_dir_all(&base);
    }
}
