//! Full-text and regex search over the content index, ranked by what each hit is.
//!
//! A query compiles to a regex and a trigram formula ([`crate::grams`]). The formula picks candidate files from the
//! trigram lists, every candidate is read and matched line by line (like `git grep`), and each matching line is
//! classed with the symbol index:
//!
//! 1. `def`: the line declares a symbol and the match covers its name,
//! 2. `decl`: another part of a declaration line (a parameter or property type, an extension),
//! 3. `code`: any other code,
//! 4. `test`, `comment` and `doc`: test code, comments, and prose files (Markdown, text).
//!
//! Within a class, exact name matches and types come first, then files whose name matches, then path and line.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use anyhow::{Context, Result, bail};
use kc_store::Store;
use kc_store::content::{CFile, FileSymbol, INDEXED, PENDING, intersect_in_place, union};
use regex::bytes::{Regex, RegexBuilder};
use serde::Serialize;

use crate::grams::{self, Plan};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Case {
    Sensitive,
    Insensitive,
    /// Insensitive unless the pattern has an uppercase letter.
    Smart,
}

#[derive(Debug, Clone)]
pub struct SearchOptions {
    pub pattern: String,
    /// Treat `pattern` as a regular expression instead of a literal string.
    pub regex: bool,
    pub case: Case,
    /// Match whole words only.
    pub word: bool,
    /// Content languages to keep (`swift`, `python`, `markdown`, ...); empty keeps all.
    pub langs: Vec<String>,
    /// Path globs; a leading `!` excludes. A glob without `/` matches the file name.
    pub globs: Vec<String>,
    /// Stop collecting after this many matching lines.
    pub max_lines: usize,
}

/// A compiled query.
pub struct Matcher {
    re: Pattern,
    pub plan: Plan,
    langs: Vec<String>,
    include: Vec<String>,
    exclude: Vec<String>,
    pub ignore_case: bool,
}

pub fn compile(o: &SearchOptions) -> Result<Matcher> {
    if o.pattern.is_empty() {
        bail!("empty pattern");
    }
    let ignore_case = match o.case {
        Case::Sensitive => false,
        Case::Insensitive => true,
        Case::Smart => !has_uppercase(&o.pattern, o.regex),
    };
    let (behind, base, ahead) = if o.regex { split_lookaround(&o.pattern) } else { (None, regex::escape(&o.pattern), None) };
    let pattern = if o.word { format!(r"(?-u:\b)(?:{base})(?-u:\b)") } else { base };
    let build = |p: &str| {
        RegexBuilder::new(p)
            .case_insensitive(ignore_case)
            .multi_line(true)
            .size_limit(50 << 20)
            .build()
            .with_context(|| format!("invalid pattern {:?}", o.pattern))
    };
    let around = |side: Option<Around>, anchored: fn(&str) -> String| -> Result<Option<(bool, Regex)>> {
        side.map(|(negated, inner)| Ok((negated, build(&anchored(&inner))?))).transpose()
    };
    let re = Pattern {
        re: build(&pattern)?,
        ahead: around(ahead, |x| format!(r"\A(?:{x})"))?,
        behind: around(behind, |x| format!(r"(?:{x})\z"))?,
    };
    let hir = regex_syntax::ParserBuilder::new()
        .case_insensitive(ignore_case)
        .multi_line(true)
        .build()
        .parse(&pattern)
        .with_context(|| format!("invalid pattern {:?}", o.pattern))?;
    let (exclude, include): (Vec<String>, Vec<String>) = o.globs.iter().map(|g| g.trim().to_string()).filter(|g| !g.is_empty()).partition(|g| g.starts_with('!'));
    Ok(Matcher {
        re,
        plan: grams::plan(&hir),
        langs: o.langs.iter().map(|l| lang_name(l)).collect(),
        include,
        exclude: exclude.into_iter().map(|g| g[1..].to_string()).collect(),
        ignore_case,
    })
}

/// Uppercase letters outside escapes (`\S`, `\W`, `\p{Lu}` are not "uppercase in the pattern").
fn has_uppercase(pattern: &str, regex: bool) -> bool {
    if !regex {
        return pattern.chars().any(char::is_uppercase);
    }
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(e) = chars.next()
                && matches!(e, 'p' | 'P')
            {
                // Skip a `\p{...}` or `\pL` class name.
                if chars.clone().next() == Some('{') {
                    for d in chars.by_ref() {
                        if d == '}' {
                            break;
                        }
                    }
                } else {
                    chars.next();
                }
            }
        } else if c.is_uppercase() {
            return true;
        }
    }
    false
}

/// A search regex, with the look-ahead that ends it or the look-behind that starts it, which the regex engine lacks:
/// `\bSave\b(?!\()`, `(?<!\.)Save`. The regex finds a match and the look-around is checked on the rest of its line,
/// after it or before it; a match failing the check is passed over for the next.
pub struct Pattern {
    re: Regex,
    /// (negated, the look-ahead's pattern anchored at the match's end).
    ahead: Option<(bool, Regex)>,
    /// (negated, the look-behind's pattern anchored at the match's start).
    behind: Option<(bool, Regex)>,
}

impl Pattern {
    /// The first match at or after `pos` whose look-around holds.
    pub fn find_at<'h>(&self, data: &'h [u8], mut pos: usize) -> Option<regex::bytes::Match<'h>> {
        loop {
            let m = self.re.find_at(data, pos)?;
            if self.holds(data, m.start(), m.end()) {
                return Some(m);
            }
            if m.start() >= data.len() {
                return None;
            }
            pos = m.start() + 1;
        }
    }

    pub fn is_match(&self, data: &[u8]) -> bool {
        self.find_at(data, 0).is_some()
    }

    fn holds(&self, data: &[u8], start: usize, end: usize) -> bool {
        if let Some((negated, ahead)) = &self.ahead {
            let line_end = memchr::memchr(b'\n', &data[end..]).map_or(data.len(), |i| end + i);
            if ahead.is_match(&data[end..line_end]) == *negated {
                return false;
            }
        }
        if let Some((negated, behind)) = &self.behind {
            let line_start = memchr::memrchr(b'\n', &data[..start]).map_or(0, |i| i + 1);
            if behind.is_match(&data[line_start..start]) == *negated {
                return false;
            }
        }
        true
    }
}

/// A look-around group: (negated, its pattern).
type Around = (bool, String);

/// A regex split into the look-behind group that starts it, the rest, and the look-ahead group that ends it: each
/// group as (negated, its pattern). `(?<!\.)\bSave\b(?!\()` is ((true, `\.`), `\bSave\b`, (true, `\(`)). Look-around
/// anywhere else stays in the rest, for the regex engine to refuse.
fn split_lookaround(pattern: &str) -> (Option<Around>, String, Option<Around>) {
    let groups = top_groups(pattern);
    let mut core = (0, pattern.len());
    let mut behind = None;
    let mut ahead = None;
    if let Some(&(open, close)) = groups.first()
        && open == 0
        && let Some(negated) = lookaround_kind(&pattern[open..], &["(?<!", "(?<="])
        && close + 1 < pattern.len()
    {
        behind = Some((negated, pattern[open + 4..close].to_string()));
        core.0 = close + 1;
    }
    if let Some(&(open, close)) = groups.last()
        && close + 1 == pattern.len()
        && open >= core.0
        && let Some(negated) = lookaround_kind(&pattern[open..], &["(?!", "(?="])
        && open > core.0
    {
        ahead = Some((negated, pattern[open + 3..close].to_string()));
        core.1 = open;
    }
    (behind, pattern[core.0..core.1].to_string(), ahead)
}

/// Whether `rest` starts with a negated (first) or plain (second) look-around opener: Some(negated).
fn lookaround_kind(rest: &str, openers: &[&str; 2]) -> Option<bool> {
    if rest.starts_with(openers[0]) {
        Some(true)
    } else if rest.starts_with(openers[1]) {
        Some(false)
    } else {
        None
    }
}

/// The (open, close) byte offsets of a pattern's outermost parenthesised groups, skipping escapes and classes.
fn top_groups(pattern: &str) -> Vec<(usize, usize)> {
    let bytes = pattern.as_bytes();
    let (mut out, mut depth, mut open, mut class, mut i) = (Vec::new(), 0usize, 0usize, false, 0usize);
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 1,
            b'[' if !class => class = true,
            b']' if class => class = false,
            b'(' if !class => {
                if depth == 0 {
                    open = i;
                }
                depth += 1;
            }
            b')' if !class && depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    out.push((open, i));
                }
            }
            _ => {}
        }
        i += 1;
    }
    out
}

impl Matcher {
    /// Whether a path passes the language and glob filters.
    pub fn keeps(&self, path: &str) -> bool {
        if !self.langs.is_empty() && !self.langs.iter().any(|l| l == content_lang(path)) {
            return false;
        }
        if self.exclude.iter().any(|g| path_glob(g, path)) {
            return false;
        }
        self.include.is_empty() || self.include.iter().any(|g| path_glob(g, path))
    }
}

/// `globs` as they apply in the repo at `root`, one of the repos a search covers (named `names`): a glob written after
/// this repo's name (`shop/src/**`) is relative to it, and one after another of the repos' names (`billing/**`) is
/// not for this one, unless the repo has a directory of that name (then the glob as written stays too); after a
/// leading `**/` too (`**/shop/src/**`), the way an agent scopes a glob to one repo of several. An absolute
/// path is relative to the repo it is in, and not for the others. `None` when globs to include were given and none
/// is for this repo: the search leaves the repo out.
pub fn globs_in(globs: &[String], root: &Path, names: &[String]) -> Option<Vec<String>> {
    let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let root_text = root.to_string_lossy();
    let mut out = Vec::new();
    let (mut includes, mut kept) = (false, false);
    for g in globs.iter().map(|g| g.trim()).filter(|g| !g.is_empty()) {
        let (negated, body) = match g.strip_prefix('!') {
            Some(b) => (true, b),
            None => (false, g),
        };
        includes |= !negated;
        let whole = |rest: &str| if rest.is_empty() { "**".to_string() } else { rest.to_string() };
        let mapped: Vec<String> = if Path::new(body).is_absolute() {
            match body.strip_prefix(root_text.as_ref()) {
                Some(rest) if rest.is_empty() || rest.starts_with('/') => vec![whole(rest.trim_start_matches('/'))],
                _ => Vec::new(),
            }
        } else {
            let body = body.trim_start_matches("./");
            // `**/shop/src/**`, a repo's name after a leading `**/`, names that repo as `shop/src/**` does.
            let body = body
                .strip_prefix("**/")
                .filter(|rest| rest.split('/').next().is_some_and(|first| names.iter().any(|n| n == first)))
                .unwrap_or(body);
            let (first, rest) = body.split_once('/').unwrap_or((body, ""));
            let own_dir = root.join(first).is_dir();
            if first == name {
                let mut v = vec![whole(rest)];
                if own_dir {
                    v.push(body.to_string());
                }
                v
            } else if !own_dir && names.iter().any(|n| n == first) {
                Vec::new()
            } else {
                vec![body.to_string()]
            }
        };
        kept |= !negated && !mapped.is_empty();
        out.extend(mapped.into_iter().map(|m| if negated { format!("!{m}") } else { m }));
    }
    (!includes || kept).then_some(out)
}

/// A content language as written (`typescript`, or an extension: `ts`, `cs`, `py`): its name in [`content_lang`].
pub fn lang_name(lang: &str) -> String {
    let l = lang.trim().trim_start_matches('.').to_ascii_lowercase();
    match content_lang(&format!("x.{l}")) {
        "other" => l,
        known => known.to_string(),
    }
}

/// A glob over a repo-relative path. `*` stays within a path component, `**` crosses them, `?` is one character.
/// A pattern without `/` matches the file name; one ending in `/` matches a directory prefix.
pub fn path_glob(pattern: &str, path: &str) -> bool {
    let pattern = pattern.trim_start_matches("./").trim_start_matches('/');
    if let Some(dir) = pattern.strip_suffix('/') {
        return path_glob(&format!("{dir}/**"), path);
    }
    if !pattern.contains('/') {
        let name = path.rsplit('/').next().unwrap_or(path);
        return glob_match(pattern.as_bytes(), name.as_bytes()) || glob_match(pattern.as_bytes(), path.as_bytes()) && pattern.contains("**");
    }
    glob_match(pattern.as_bytes(), path.as_bytes())
}

fn glob_match(p: &[u8], s: &[u8]) -> bool {
    // Keep ordinary globs allocation-free; abandon backtracking after a fixed amount of work.
    let mut budget = 64;
    glob_match_fast(p, s, &mut budget).unwrap_or_else(|| glob_match_bounded(p, s))
}

fn glob_match_fast(mut p: &[u8], mut s: &[u8], budget: &mut usize) -> Option<bool> {
    *budget = budget.checked_sub(1)?;
    loop {
        match p.first() {
            None => return Some(s.is_empty()),
            Some(b'*') => {
                let crosses_slash = p.get(1) == Some(&b'*');
                let skip = if crosses_slash { if p.get(2) == Some(&b'/') { 3 } else { 2 } } else { 1 };
                let rest = &p[skip..];
                if rest.is_empty() {
                    return Some(crosses_slash || !s.contains(&b'/'));
                }
                for i in 0..=s.len() {
                    if glob_match_fast(rest, &s[i..], budget)? {
                        return Some(true);
                    }
                    if !crosses_slash && s.get(i) == Some(&b'/') {
                        break;
                    }
                }
                return Some(false);
            }
            Some(&b) => {
                if s.first().is_none_or(|&c| if b == b'?' { c == b'/' } else { c != b }) {
                    return Some(false);
                }
                p = &p[1..];
                s = &s[1..];
            }
        }
    }
}

fn glob_match_bounded(p: &[u8], s: &[u8]) -> bool {
    // Strip the literal prefix before allocating the suffix-matching rows. Most paths fail here.
    let prefix = p.iter().position(|b| matches!(b, b'*' | b'?')).unwrap_or(p.len());
    if !s.starts_with(&p[..prefix]) {
        return false;
    }
    let (mut p, mut s) = (&p[prefix..], &s[prefix..]);
    if p.is_empty() {
        return s.is_empty();
    }
    // Tokenize forwards so runs such as `***/` have the same grouping as the recursive matcher.
    let mut tokens = Vec::new();
    while let Some(&b) = p.first() {
        if p.starts_with(b"**") {
            tokens.push((b'*', true));
            // Historically `**/` permits any prefix, including one with no slash. The alternative
            // that matches an initial slash is already included in that set of prefixes.
            p = &p[if p.get(2) == Some(&b'/') { 3 } else { 2 }..];
        } else {
            tokens.push((b, false));
            p = &p[1..];
        }
    }
    // Check the literal suffix once. Common globs such as `*.rs` then need no matching rows.
    while let Some(&(b, _)) = tokens.last() {
        if matches!(b, b'*' | b'?') {
            break;
        }
        if s.last() != Some(&b) {
            return false;
        }
        s = &s[..s.len() - 1];
        tokens.pop();
    }
    if let [(b'*', crosses_slash)] = tokens.as_slice() {
        return *crosses_slash || !s.contains(&b'/');
    }
    // Each row says which suffixes of s match the remaining tokens. O(pattern * path) work,
    // O(pattern + path) scratch, with no recursive backtracking or stack growth.
    let mut next = vec![false; s.len() + 1];
    let mut row = vec![false; s.len() + 1];
    next[s.len()] = true;
    for (b, crosses_slash) in tokens.into_iter().rev() {
        row[s.len()] = b == b'*' && next[s.len()];
        for i in (0..s.len()).rev() {
            row[i] = match b {
                b'*' => next[i] || (crosses_slash || s[i] != b'/') && row[i + 1],
                b'?' => s[i] != b'/' && next[i + 1],
                _ => s[i] == b && next[i + 1],
            };
        }
        std::mem::swap(&mut row, &mut next);
    }
    next[0]
}

/// The content language of a path, from its extension or name.
pub fn content_lang(path: &str) -> &'static str {
    let name = path.rsplit('/').next().unwrap_or(path);
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    match ext.as_deref() {
        Some("swift") => "swift",
        Some("py" | "pyi") => "python",
        Some("rs") => "rust",
        Some("cs") => "csharp",
        Some("md" | "markdown") => "markdown",
        Some("txt" | "rst" | "adoc") => "text",
        Some("json" | "jsonl" | "ndjson" | "geojson") => "json",
        Some("yml" | "yaml") => "yaml",
        Some("toml") => "toml",
        Some("sh" | "bash" | "zsh" | "fish") => "shell",
        Some("c" | "h") => "c",
        Some("m" | "mm") => "objc",
        Some("cc" | "cpp" | "cxx" | "hpp" | "hh") => "cpp",
        Some("js" | "mjs" | "cjs" | "jsx") => "javascript",
        Some("ts" | "tsx" | "mts") => "typescript",
        Some("html" | "htm") => "html",
        Some("css" | "scss") => "css",
        Some("xml" | "plist" | "xcscheme" | "storyboard" | "xib" | "entitlements") => "xml",
        Some("pbxproj") => "pbxproj",
        Some("metal") => "metal",
        Some("go") => "go",
        Some("java") => "java",
        Some("kt" | "kts") => "kotlin",
        Some("rb") => "ruby",
        Some("sql") => "sql",
        Some("cfm" | "cfc" | "cfml" | "cfs") => "cfml",
        Some("ini" | "cfg" | "conf") => "config",
        _ => match name {
            "Makefile" | "makefile" | "GNUmakefile" => "make",
            "Dockerfile" => "docker",
            "Gemfile" | "Podfile" | "Fastfile" => "ruby",
            _ => "other",
        },
    }
}

/// Comment syntax for a language: whole-line and trailing comment markers.
fn comment_markers(lang: &str) -> &'static [&'static str] {
    match lang {
        "swift" | "rust" | "csharp" | "c" | "objc" | "cpp" | "javascript" | "typescript" | "go" | "java" | "kotlin" | "metal" => &["//", "/*"],
        "css" => &["/*"],
        "python" | "shell" | "yaml" | "toml" | "ruby" | "make" | "docker" | "config" => &["#"],
        "sql" => &["--"],
        "html" | "xml" => &["<!--"],
        "cfml" => &["<!---", "//", "/*"],
        _ => &[],
    }
}

fn is_prose(lang: &str) -> bool {
    matches!(lang, "markdown" | "text")
}

/// Whether the match at `start` in `line` sits in a comment: the line starts with a comment marker (or a `*`
/// continuation of a block comment), or a marker outside a string literal precedes the match.
fn in_comment(lang: &str, line: &[u8], start: usize) -> bool {
    let markers = comment_markers(lang);
    if markers.is_empty() {
        return false;
    }
    let trimmed = line.trim_ascii_start();
    if markers.iter().any(|m| trimmed.starts_with(m.as_bytes())) {
        return true;
    }
    if markers.contains(&"/*") && (trimmed.starts_with(b"* ") || trimmed == b"*" || trimmed.starts_with(b"*/")) {
        return true;
    }
    let before = &line[..start.min(line.len())];
    let mut in_str = false;
    let mut i = 0;
    while i < before.len() {
        let c = before[i];
        if c == b'\\' {
            i += 2;
            continue;
        }
        if c == b'"' {
            in_str = !in_str;
        } else if !in_str && markers.iter().any(|m| before[i..].starts_with(m.as_bytes())) {
            // `#` inside an identifier-ish run (`a#b`, Swift `#if`) isn't a comment.
            let prev = i.checked_sub(1).map(|j| before[j]);
            if markers[0] != "#" || prev.is_none_or(|p| p == b' ' || p == b'\t') {
                return true;
            }
        }
        i += 1;
    }
    false
}

pub use kc_store::is_test_path;

/// A file to scan.
#[derive(Debug, Clone)]
pub struct ScanFile {
    pub repo: usize,
    pub path: String,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct CandidateStats {
    /// Searchable files in scope.
    pub files: usize,
    /// Files the trigram formula left to read.
    pub candidates: usize,
}

/// Searchable files of one repo that can match: the trigram formula's picks plus every pending file, filtered.
pub fn candidates(store: &Store, files: Vec<CFile>, m: &Matcher, repo: usize) -> Result<(Vec<ScanFile>, CandidateStats)> {
    let ids = eval(store, &m.plan)?;
    let mut stats = CandidateStats::default();
    let mut out = Vec::new();
    for f in files {
        if !matches!(f.state, INDEXED | PENDING) || !m.keeps(&f.path) {
            continue;
        }
        stats.files += 1;
        let pick = f.state == PENDING || ids.as_ref().is_none_or(|ids| ids.binary_search(&(f.id as u32)).is_ok());
        if pick {
            out.push(ScanFile { repo, path: f.path });
        }
    }
    stats.candidates = out.len();
    Ok((out, stats))
}

/// The file ids a formula admits, ascending; `None` admits every file.
pub(crate) fn eval(store: &Store, plan: &Plan) -> Result<Option<Vec<u32>>> {
    Ok(match plan {
        Plan::All => None,
        Plan::Grams(gs) => Some(store.intersect_postings(gs)?),
        Plan::And(ps) => {
            let mut acc: Option<Vec<u32>> = None;
            for p in ps {
                if let Some(ids) = eval(store, p)? {
                    acc = Some(match acc {
                        Some(mut a) => {
                            intersect_in_place(&mut a, &ids);
                            a
                        }
                        None => ids,
                    });
                }
            }
            acc
        }
        Plan::Or(ps) => {
            let mut acc = Vec::new();
            for p in ps {
                match eval(store, p)? {
                    Some(ids) => acc = union(&acc, &ids),
                    None => return Ok(None),
                }
            }
            Some(acc)
        }
    })
}

/// One matching line. Its text sits in its scan thread's text arena, so thousands of hits cost a few large
/// allocations rather than many small ones: the long-running server would keep the pages small ones touched.
#[derive(Debug, Clone, Copy)]
pub struct LineHit {
    pub line: u32,
    /// Byte range of the first match within the line.
    pub start: u32,
    pub end: u32,
    off: u32,
    len: u32,
    pub comment: bool,
}

/// Lines longer than this are cut for snippets and classification.
const MAX_LINE: usize = 2000;

#[derive(Debug, Clone)]
pub struct FileHits {
    pub repo: usize,
    pub path: String,
    pub lang: &'static str,
    pub lines: Vec<LineHit>,
    /// Which of [`Scanned::arenas`] holds the line texts.
    arena: usize,
}

/// What a scan found: the files with hits, and the text arenas their lines point into.
pub struct Scanned {
    pub files: Vec<FileHits>,
    pub arenas: Vec<String>,
    pub stats: ScanStats,
}

impl Scanned {
    /// A hit's line (cut to [`MAX_LINE`] bytes, without its newline).
    pub fn text(&self, fh: &FileHits, h: &LineHit) -> &str {
        &self.arenas[fh.arena][h.off as usize..(h.off + h.len) as usize]
    }
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct ScanStats {
    pub files_read: usize,
    pub bytes_read: u64,
    /// Stopped early at the line cap.
    pub capped: bool,
    /// Files among those read that could not be read (repo index, path): their matches are missing.
    #[serde(skip)]
    pub failed: Vec<(usize, String)>,
}

/// Where a parallel scan stops, the same whatever the thread timing: files are claimed in their order and every
/// claimed file is finished, so the files done are always a prefix of the list; the cut is the first file (in that
/// order) at which the count so far passes `max`, and only it and the files before it are kept. `done` holds the
/// files processed, by index, each with its count; returns how many files to keep (a prefix) and whether the cap
/// cut the list.
pub fn deterministic_cut(done: &mut [(usize, usize)], max: usize) -> (usize, bool) {
    done.sort_unstable_by_key(|d| d.0);
    let mut sum = 0usize;
    for (i, &(k, n)) in done.iter().enumerate() {
        debug_assert_eq!(k, i, "the files done are a prefix of the list");
        sum += n;
        if sum > max {
            return (k + 1, true);
        }
    }
    (done.len(), false)
}

/// What one file of a scan gave: its index in the list, bytes read (`None`: unreadable) and hits.
struct Done<T> {
    k: usize,
    bytes: Option<usize>,
    hits: Vec<T>,
}

/// Runs `work` on `files` in parallel until the hits found pass `max`, then keeps the deterministic prefix (see
/// [`deterministic_cut`]): the files kept with their hits in file order, the bytes read and unreadable files among
/// them, and whether the cap cut the list. `work` returns `None` for a file it could not read.
pub fn scan_prefix<F, T, S>(files: &[F], threads: usize, max: usize, init: impl Fn() -> S + Sync, work: impl Fn(&mut S, &F) -> Option<(usize, Vec<T>)> + Sync) -> ScanPrefix<T, S>
where
    F: Sync,
    T: Send,
    S: Send,
{
    let next = AtomicUsize::new(0);
    let found = AtomicUsize::new(0);
    let capped = AtomicBool::new(false);
    type Collected<T, S> = (Vec<(usize, Done<T>)>, Vec<S>);
    let out: Mutex<Collected<T, S>> = Mutex::new((Vec::new(), Vec::new()));
    std::thread::scope(|scope| {
        for _ in 0..threads.clamp(1, files.len().max(1)) {
            scope.spawn(|| {
                let mut state = init();
                let mut local = Vec::new();
                loop {
                    if capped.load(Ordering::Relaxed) {
                        break;
                    }
                    let k = next.fetch_add(1, Ordering::Relaxed);
                    let Some(f) = files.get(k) else { break };
                    let (bytes, hits) = match work(&mut state, f) {
                        Some((b, hits)) => (Some(b), hits),
                        None => (None, Vec::new()),
                    };
                    if !hits.is_empty() && found.fetch_add(hits.len(), Ordering::Relaxed) + hits.len() > max {
                        capped.store(true, Ordering::Relaxed);
                    }
                    local.push(Done { k, bytes, hits });
                }
                let mut o = out.lock().unwrap();
                let which = o.1.len();
                o.0.extend(local.into_iter().map(|d| (which, d)));
                o.1.push(state);
            });
        }
    });
    let (mut done, states) = out.into_inner().unwrap();
    let mut counts: Vec<(usize, usize)> = done.iter().map(|(_, d)| (d.k, d.hits.len())).collect();
    let (keep, capped) = deterministic_cut(&mut counts, max);
    done.retain(|(_, d)| d.k < keep);
    done.sort_unstable_by_key(|(_, d)| d.k);
    let mut bytes_read = 0u64;
    let mut failed = Vec::new();
    let mut kept = Vec::new();
    for (which, d) in done {
        match d.bytes {
            Some(b) => bytes_read += b as u64,
            None => failed.push(d.k),
        }
        kept.push((d.k, which, d.hits));
    }
    ScanPrefix { files: kept, states, files_read: keep, bytes_read, capped, failed }
}

/// What [`scan_prefix`] kept.
pub struct ScanPrefix<T, S> {
    /// (file index, the worker state that holds its data, its hits), in file order.
    pub files: Vec<(usize, usize, Vec<T>)>,
    /// Each worker's state, by the index in `files`.
    pub states: Vec<S>,
    pub files_read: usize,
    pub bytes_read: u64,
    pub capped: bool,
    /// Indexes of the files kept that could not be read.
    pub failed: Vec<usize>,
}

/// Reads and matches files in parallel. Files come back sorted by repo and path. At `max_lines` matching lines the
/// scan stops at the same file whatever the thread timing (see [`deterministic_cut`]); files it could not read are
/// in `stats.failed`.
pub fn scan(roots: &[PathBuf], files: &[ScanFile], m: &Matcher, threads: usize, max_lines: usize) -> Scanned {
    // One read buffer and one text arena per thread, reused across files.
    let init = || (Vec::new(), String::new());
    let work = |(data, text): &mut (Vec<u8>, String), f: &ScanFile| -> Option<(usize, Vec<LineHit>)> {
        if !readable(read_into(&roots[f.repo].join(&f.path), data))? {
            return Some((0, Vec::new()));
        }
        Some((data.len(), match_lines(&m.re, data, content_lang(&f.path), text)))
    };
    let p = scan_prefix(files, threads, max_lines, init, work);
    let mut hits: Vec<FileHits> = p
        .files
        .into_iter()
        .filter(|(_, _, lines)| !lines.is_empty())
        .map(|(k, arena, lines)| FileHits { repo: files[k].repo, path: files[k].path.clone(), lang: content_lang(&files[k].path), lines, arena })
        .collect();
    hits.sort_by(|a, b| (a.repo, &a.path).cmp(&(b.repo, &b.path)));
    let arenas = p.states.into_iter().map(|(_, text)| text).collect();
    let failed = p.failed.iter().map(|&k| (files[k].repo, files[k].path.clone())).collect();
    let stats = ScanStats { files_read: p.files_read, bytes_read: p.bytes_read, capped: p.capped, failed };
    Scanned { files: hits, arenas, stats }
}

/// What a scan makes of a read: `Some(true)` read, `Some(false)` nothing to read (gone since the index listed it, or
/// a directory: a submodule's placeholder), `None` a file that couldn't be read (permissions, I/O), whose matches are
/// missing.
pub fn readable(read: std::io::Result<()>) -> Option<bool> {
    match read {
        Ok(()) => Some(true),
        Err(e) if matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::IsADirectory) => Some(false),
        Err(_) => None,
    }
}

/// Reads a whole file into `buf`, reusing its allocation.
pub fn read_into(path: &Path, buf: &mut Vec<u8>) -> std::io::Result<()> {
    use std::io::Read as _;
    buf.clear();
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len() as usize;
    // reserve() is relative to len (zero after clear), rather than the existing capacity.
    buf.reserve(len.saturating_add(1));
    f.read_to_end(buf)?;
    Ok(())
}

/// Every line with a match, like `git grep`: matches never span lines. The whole buffer is searched for the next
/// match start; the line holding it is then matched on its own, which finds that line's first match whether or not
/// the buffer match ran past the newline. Matching lines are appended to `text`.
pub fn match_lines(re: &Pattern, data: &[u8], lang: &str, text: &mut String) -> Vec<LineHit> {
    let mut out = Vec::new();
    let len = data.len();
    let (mut pos, mut line_no, mut counted) = (0usize, 1u32, 0usize);
    // Python: triple quotes seen so far, to tell docstrings from code.
    let python = lang == "python";
    let mut quotes = [0usize; 2];
    let count_quotes = |seg: &[u8], q: &mut [usize; 2]| {
        q[0] += memchr::memmem::find_iter(seg, b"\"\"\"").count();
        q[1] += memchr::memmem::find_iter(seg, b"'''").count();
    };
    while pos <= len {
        let Some(m) = re.find_at(data, pos) else { break };
        let s = m.start();
        let ls = memchr::memrchr(b'\n', &data[..s]).map_or(0, |i| i + 1);
        if ls == len && len > 0 {
            break; // the empty "line" after a final newline
        }
        let le = memchr::memchr(b'\n', &data[s..]).map_or(len, |i| s + i);
        line_no += memchr::memchr_iter(b'\n', &data[counted..ls]).count() as u32;
        if python {
            count_quotes(&data[counted..ls], &mut quotes);
        }
        counted = ls;
        let line = &data[ls..le];
        let found = if m.end() <= le { Some((s - ls, m.end() - ls)) } else { re.find_at(line, 0).map(|x| (x.start(), x.end())) };
        if let Some((start, end)) = found {
            let cut = &line[..line.len().min(MAX_LINE)];
            let cut = cut.strip_suffix(b"\r").unwrap_or(cut);
            let off = text.len();
            text.push_str(&String::from_utf8_lossy(cut));
            out.push(LineHit {
                line: line_no,
                start: start as u32,
                end: end as u32,
                off: off as u32,
                len: (text.len() - off) as u32,
                comment: in_comment(lang, cut, start)
                    || python && {
                        let mut q = quotes;
                        count_quotes(&line[..start], &mut q);
                        q[0] % 2 == 1 || q[1] % 2 == 1
                    },
            });
        }
        pos = le + 1;
    }
    out
}

/// A matching file after classing: the symbols around its hits, kept once for all of them.
#[derive(Debug)]
pub struct FileData {
    pub repo: usize,
    pub path: String,
    arena: usize,
    symbols: Vec<FileSymbol>,
}

/// Buffers [`classify`] reuses from file to file.
#[derive(Default)]
pub struct Scratch {
    buf: Vec<u8>,
    starts: Vec<usize>,
}

/// A classified, rankable hit: small and copyable. [`finish`] turns the returned page into [`PageHit`]s.
#[derive(Debug, Clone, Copy)]
pub struct Hit {
    /// Index into the [`FileData`] list.
    pub file: u32,
    pub line: u32,
    /// `def`, `decl`, `code`, `test`, `comment` or `doc`.
    pub kind: &'static str,
    pub key: (u8, u8, u8),
    raw: LineHit,
    /// The declared symbol (index into the file's symbols), for `def` and `decl`.
    declared: Option<u32>,
}

/// A hit as returned.
#[derive(Debug, Clone, Serialize)]
pub struct PageHit {
    #[serde(skip)]
    pub repo: usize,
    pub file: String,
    pub line: u32,
    pub kind: &'static str,
    /// The line, trimmed, cut around the match.
    pub text: String,
    /// For `def` and `decl`: the declared symbol.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    /// Otherwise: the innermost symbol containing the line.
    #[serde(rename = "in", skip_serializing_if = "Option::is_none")]
    pub inside: Option<String>,
    /// With `in` a method or function: its parameter names, from its signature (`(id, data)`), on the page's first
    /// code hit inside it (see [`add_params`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_params: Option<String>,
    /// With `in` a method or function: its name and first line, to look its signature up.
    #[serde(skip)]
    pub member: Option<(String, u32)>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub before: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub after: Vec<String>,
}

/// Snippet width in characters.
const SNIPPET: usize = 160;

fn qualified(s: &FileSymbol) -> String {
    match &s.container {
        Some(c) if !c.is_empty() => format!("{c}.{}", s.name),
        _ => s.name.clone(),
    }
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Byte ranges where `name` occurs in `line` as a whole word.
fn word_spans(line: &str, name: &str) -> Vec<(usize, usize)> {
    let (l, n) = (line.as_bytes(), name.as_bytes());
    let mut out = Vec::new();
    if n.is_empty() {
        return out;
    }
    for i in memchr::memmem::find_iter(l, n) {
        let before = i.checked_sub(1).map(|j| l[j]);
        let after = l.get(i + n.len()).copied();
        if before.is_none_or(|b| !is_word_byte(b)) && after.is_none_or(|b| !is_word_byte(b)) {
            out.push((i, i + n.len()));
        }
    }
    out
}

const TYPE_KINDS: &[&str] = &["class", "struct", "enum", "actor", "protocol", "typealias"];

/// A declaration's name is looked for on its first lines (attributes and decorators come first).
const DECL_LINES: u32 = 4;

/// Classes one file's hits with its symbols. `root` is the file's repo, read again only when a declaration starts
/// above the hit line (attributes, decorators, multi-line signatures) to find the line that names the symbol.
/// `file` is the index the returned [`FileData`] will have.
pub fn classify(
    root: &Path,
    store: &Store,
    scanned: &Scanned,
    fh: &FileHits,
    file: u32,
    m: &Matcher,
    scratch: &mut Scratch,
) -> Result<(FileData, Vec<Hit>)> {
    let mut symbols = if kc_extract::Lang::SOURCE.contains(&fh.lang) {
        let lines: Vec<u32> = fh.lines.iter().map(|h| h.line).collect();
        store.file_symbols(&fh.path, &lines)?
    } else {
        Vec::new()
    };
    symbols.sort_by_key(|s| s.start_line);
    let tests: Vec<(u32, u32)> = symbols.iter().filter(|s| s.test).map(|s| (s.start_line, s.end_line)).collect();
    let test_file = is_test_path(&fh.path);
    let name = fh.path.rsplit('/').next().unwrap_or(&fh.path);
    let name_hit = m.re.is_match(name.as_bytes()) as u8;
    let mut out = Vec::with_capacity(fh.lines.len());
    let mut reread = false;
    for h in &fh.lines {
        let (l, text) = (h.line, scanned.text(fh, h));
        let (start, end) = (h.start as usize, h.end as usize);
        // The innermost symbol whose declaration names it on this line: the first of its first lines that has the
        // name as a word.
        let lo = symbols.partition_point(|s| s.start_line + DECL_LINES < l);
        let hi = symbols.partition_point(|s| s.start_line <= l);
        let mut decl: Option<(usize, Vec<(usize, usize)>)> = None;
        for (i, s) in symbols.iter().enumerate().take(hi).skip(lo) {
            if l > s.end_line {
                continue;
            }
            let spans = word_spans(text, &s.name);
            if spans.is_empty() {
                continue;
            }
            if s.start_line < l {
                if !reread {
                    reread = true;
                    scratch.starts.clear();
                    if read_into(&root.join(&fh.path), &mut scratch.buf).is_err() {
                        scratch.buf.clear();
                    }
                    scratch.starts.push(0);
                    scratch.starts.extend(memchr::memchr_iter(b'\n', &scratch.buf).map(|i| i + 1));
                }
                let line = |n: u32| {
                    let (a, b) = (scratch.starts.get(n as usize - 1).copied(), scratch.starts.get(n as usize).copied());
                    a.map(|a| String::from_utf8_lossy(&scratch.buf[a..b.unwrap_or(scratch.buf.len())]))
                };
                if (s.start_line..l).any(|n| line(n).is_some_and(|t| !word_spans(&t, &s.name).is_empty())) {
                    continue;
                }
            }
            // Later starts are more deeply nested.
            decl = Some((i, spans));
        }
        let in_test = test_file || tests.iter().any(|&(a, b)| a <= l && l <= b);
        let (kind, declared, sub) = match decl {
            Some((i, spans)) if !h.comment => {
                let s = &symbols[i];
                let covers = spans.iter().any(|&(a, b)| start < b && a < end);
                let is_type = TYPE_KINDS.contains(&s.kind.as_str());
                if covers && s.kind != "extension" {
                    // The whole name matched (`SettingsStore`, `struct SettingsStore`), or only part of it.
                    let whole = spans.iter().any(|&(a, b)| start <= a && b <= end);
                    ("def", Some(i as u32), (!whole) as u8 * 2 + (!is_type) as u8 + 8 * in_test as u8)
                } else {
                    ("decl", Some(i as u32), (!is_type) as u8 + 8 * in_test as u8)
                }
            }
            _ if is_prose(fh.lang) => ("doc", None, 2),
            _ if h.comment => ("comment", None, 1),
            _ if in_test => ("test", None, 0),
            _ => ("code", None, 0),
        };
        let tier = match kind {
            "def" => 0,
            "decl" => 1,
            "code" => 2,
            _ => 3,
        };
        out.push(Hit { file, line: l, kind, key: (tier, sub, 1 - name_hit), raw: *h, declared });
    }
    let data = FileData { repo: fh.repo, path: fh.path.clone(), arena: fh.arena, symbols };
    Ok((data, out))
}

/// Sorts hits by class, then exactness, then file-name match, then repo order, path and line.
pub fn rank(hits: &mut [Hit], files: &[FileData], repo_names: &[String]) {
    hits.sort_unstable_by(|a, b| {
        let (fa, fb) = (&files[a.file as usize], &files[b.file as usize]);
        (a.key, &repo_names[fa.repo], &fa.path, a.line).cmp(&(b.key, &repo_names[fb.repo], &fb.path, b.line))
    });
}

/// The symbol kinds whose parameters a hit inside them is given (see [`add_params`]).
const MEMBER_KINDS: &[&str] = &["method", "function", "init", "subscript", "procedure"];

/// The returned page: snippet, declared or enclosing symbol, and `context` lines around each hit (re-read from
/// the files, which the search just read). Without `context`, a hit in a statement that goes on over other lines
/// gets just those (see [`add_statement`]).
pub fn finish(roots: &[PathBuf], scanned: &Scanned, files: &[FileData], hits: &[Hit], context: usize) -> Vec<PageHit> {
    let mut page: Vec<PageHit> = hits
        .iter()
        .map(|h| {
            let f = &files[h.file as usize];
            let (symbol, inside, member) = match h.declared {
                Some(i) => (Some(qualified(&f.symbols[i as usize])), None, None),
                None => {
                    let l = h.line;
                    let enclosing = f
                        .symbols
                        .iter()
                        .filter(|s| s.start_line <= l && l <= s.end_line)
                        .max_by_key(|s| (s.start_line, std::cmp::Reverse(s.end_line)));
                    let member = enclosing.filter(|s| MEMBER_KINDS.contains(&s.kind.as_str())).map(|s| (s.name.clone(), s.start_line));
                    (None, enclosing.map(qualified), member)
                }
            };
            PageHit {
                repo: f.repo,
                file: f.path.clone(),
                line: h.line,
                kind: h.kind,
                text: snippet(
                    &scanned.arenas[f.arena][h.raw.off as usize..(h.raw.off + h.raw.len) as usize],
                    h.raw.start as usize,
                    h.raw.end as usize,
                ),
                symbol,
                inside,
                in_params: None,
                member,
                before: Vec::new(),
                after: Vec::new(),
            }
        })
        .collect();
    if context == 0 {
        add_statement(roots, &mut page);
    } else {
        add_context(roots, &mut page, context);
    }
    page
}

/// Parameter names said at most, in characters.
const PARAMS_CHARS: usize = 50;
/// Lines of a statement quoted at most on each side of a hit's line.
const STATEMENT_LINES: usize = 2;

/// Gives the page's first code hit inside each method or function its parameter names (`in_params`), read from the
/// index of the hit's repo (`stores`, by the hits' `repo`): the enclosing member's name says which member, not which
/// overload or with what. Later hits inside the same member name it only; a test's parameters aren't given, nor
/// those a hit's own line already shows (a line of the signature).
pub fn add_params(stores: &[&Store], page: &mut [PageHit]) -> Result<()> {
    let mut seen = std::collections::HashSet::new();
    for h in page.iter_mut() {
        let Some((name, start)) = &h.member else { continue };
        if h.kind != "code" || h.line == *start || seen.contains(&(h.repo, h.file.clone(), *start)) {
            continue;
        }
        let Some(store) = stores.get(h.repo) else { continue };
        let Some((params, whole)) = store.symbol_signature(&h.file, *start, name)?.and_then(|sig| params_of(&sig, name)) else { continue };
        let line = h.text.trim().trim_end_matches(',');
        if !line.is_empty() && whole.contains(line) {
            continue;
        }
        seen.insert((h.repo, h.file.clone(), *start));
        h.in_params = Some(params);
    }
    Ok(())
}

/// Brackets around text that may hold commas, colons or `=`: `(`, `<`, `[`, `{` open one, their pairs close it; the
/// `>` of an arrow (`=>`, `->`) doesn't.
fn bracket(prev: Option<char>, c: char) -> i8 {
    match c {
        '(' | '<' | '[' | '{' => 1,
        '>' if matches!(prev, Some('=' | '-')) => 0,
        ')' | '>' | ']' | '}' => -1,
        _ => 0,
    }
}

/// The parameter names of a signature, `(eventName, payload)`: of the list from the `(` after `name` (else the
/// first after any leading attributes) to its match, each parameter's name without its type, modifiers or default
/// (`{…}` for a destructured one). `None` for no list or an empty one. Second, the whole list on one line, to tell
/// whether a hit's line is part of it.
fn params_of(sig: &str, name: &str) -> Option<(String, String)> {
    let mut rest = sig.trim_start();
    // Leading attributes (`[HttpGet("x")]`) and decorators (`@Input()`) have parentheses of their own.
    loop {
        if let Some(r) = rest.strip_prefix('[') {
            rest = r.split_once(']').map(|(_, r)| r.trim_start())?;
        } else if rest.starts_with('@') {
            let end = rest.find(char::is_whitespace)?;
            rest = rest[end..].trim_start();
        } else {
            break;
        }
    }
    let open = word_spans(rest, name).into_iter().find_map(|(_, end)| rest[end..].find('(').map(|i| end + i)).or_else(|| rest.find('('))?;
    // Each parameter, split at the commas outside brackets, up to the bracket closing the list.
    let (mut depth, mut from, mut close, mut parts, mut prev) = (0i32, open + 1, None, Vec::new(), None);
    for (i, c) in rest[open..].char_indices() {
        let i = open + i;
        depth += bracket(prev, c) as i32;
        prev = Some(c);
        if depth == 0 {
            parts.push(&rest[from..i]);
            close = Some(i);
            break;
        }
        if c == ',' && depth == 1 {
            parts.push(&rest[from..i]);
            from = i + 1;
        }
    }
    let whole = rest[open + 1..close?].split_whitespace().collect::<Vec<_>>().join(" ");
    let names: Vec<&str> = parts.into_iter().filter_map(param_name).collect();
    if names.is_empty() {
        return None;
    }
    let list = names.join(", ");
    let list = if list.chars().count() <= PARAMS_CHARS { list } else { format!("{}…", list.chars().take(PARAMS_CHARS).collect::<String>()) };
    Some((format!("({list})"), whole))
}

/// A parameter's name: `string id` (C#, Java), `id: string` (TypeScript, Python, Swift, Rust), `id = 1`, `...ids`;
/// `{…}` for a destructured one. `None` for a receiver (`self`), said by the member's container.
fn param_name(p: &str) -> Option<&str> {
    // After its attributes (`[FromBody]`); an array destructured is all brackets.
    let mut p = p.trim();
    if p.is_empty() {
        return None;
    }
    while let Some(r) = p.strip_prefix('[') {
        p = r.split_once(']').map_or("", |(_, r)| r.trim_start());
    }
    if p.is_empty() {
        return Some("[…]");
    }
    // Up to its type annotation's `:` or its default's `=`, outside brackets.
    let (mut depth, mut prev, mut end) = (0i32, None, p.len());
    for (i, c) in p.char_indices() {
        let next = p[i + c.len_utf8()..].chars().next();
        depth += bracket(prev, c) as i32;
        prev = Some(c);
        if depth == 0 && (c == ':' || (c == '=' && !matches!(next, Some('>' | '=')))) {
            end = i;
            break;
        }
    }
    let p = p[..end].trim();
    if p.starts_with('{') {
        return Some("{…}");
    }
    let word = p.split_whitespace().last()?.trim_end_matches('?').trim_start_matches("...");
    (!word.is_empty() && !matches!(word, "self" | "&self" | "cls")).then_some(word)
}

/// A line that goes on the statement before it: a chained call (`.then(`, `?.map(`), not a spread (`...rest`).
fn continues(line: &str) -> bool {
    let t = line.trim_start();
    (t.starts_with('.') && !t.starts_with("..")) || t.starts_with("?.")
}

/// A `case` or `default` label: what the line under it runs for.
fn label(line: &str) -> bool {
    let t = line.trim();
    (t.starts_with("case ") || t == "default:") && t.ends_with(':')
}

/// Without `context`, the lines a hit's statement goes on over, at most [`STATEMENT_LINES`] each side: before a
/// line continuing a chained call, the lines back to the call it continues; after a line whose next continues it,
/// those lines; before the first line under a `case` label, the label. Only what the hit's line can't be read
/// without: a code or test hit, never a declaration or comment, and nothing for a line that stands alone.
fn add_statement(roots: &[PathBuf], hits: &mut [PageHit]) {
    let mut order: Vec<usize> = (0..hits.len()).filter(|&i| matches!(hits[i].kind, "code" | "test")).collect();
    order.sort_by(|&a, &b| (hits[a].repo, &hits[a].file).cmp(&(hits[b].repo, &hits[b].file)));
    let (mut buf, mut starts, mut cached): (Vec<u8>, Vec<usize>, Option<(usize, String)>) = (Vec::new(), Vec::new(), None);
    for i in order {
        let h = &hits[i];
        if cached.as_ref().is_none_or(|(r, f)| *r != h.repo || *f != h.file) {
            if read_into(&roots[h.repo].join(&h.file), &mut buf).is_err() {
                buf.clear();
            }
            starts.clear();
            starts.push(0);
            starts.extend(memchr::memchr_iter(b'\n', &buf).map(|i| i + 1));
            cached = Some((h.repo, h.file.clone()));
        }
        let line = |n: usize| -> Option<String> {
            let (a, b) = (*starts.get(n)?, starts.get(n + 1).map_or(buf.len(), |b| b - 1));
            (a <= b).then(|| String::from_utf8_lossy(&buf[a..b]).trim().chars().take(SNIPPET).collect())
        };
        let at = h.line as usize - 1;
        let Some(this) = line(at) else { continue };
        let mut before = Vec::new();
        if continues(&this) {
            for n in (at.saturating_sub(STATEMENT_LINES)..at).rev() {
                let Some(l) = line(n).filter(|l| !l.is_empty()) else { break };
                let more = continues(&l);
                before.insert(0, l);
                if !more {
                    break;
                }
            }
        } else if let Some(l) = at.checked_sub(1).and_then(line).filter(|l| label(l) && !this.starts_with("case ") && !this.starts_with("default")) {
            before.push(l);
        }
        let mut after = Vec::new();
        for n in at + 1..=at + STATEMENT_LINES {
            match line(n) {
                Some(l) if continues(&l) => after.push(l),
                _ => break,
            }
        }
        let h = &mut hits[i];
        h.before = before;
        h.after = after;
    }
}

/// The line trimmed, cut to about [`SNIPPET`] characters around the match.
fn snippet(line: &str, start: usize, end: usize) -> String {
    let lead = line.len() - line.trim_start().len();
    let text = line.trim();
    if text.chars().count() <= SNIPPET {
        return text.to_string();
    }
    let (start, end) = (start.saturating_sub(lead).min(text.len()), end.saturating_sub(lead).min(text.len()));
    let mut from = start.saturating_sub(40);
    while !text.is_char_boundary(from) {
        from -= 1;
    }
    let mut to = (from + SNIPPET * 2).min(text.len()).max(end.min(text.len()));
    while !text.is_char_boundary(to) {
        to -= 1;
    }
    let cut: String = text[from..to].chars().take(SNIPPET).collect();
    format!("{}{cut}{}", if from > 0 { "…" } else { "" }, if from + cut.len() < text.len() { "…" } else { "" })
}

/// Fills `before` and `after` with up to `n` lines around each hit, dedented together with the hit's line.
fn add_context(roots: &[PathBuf], hits: &mut [PageHit], n: usize) {
    if n == 0 {
        return;
    }
    let mut cache: Option<(usize, String, Vec<String>)> = None;
    let mut order: Vec<usize> = (0..hits.len()).collect();
    order.sort_by(|&a, &b| (hits[a].repo, &hits[a].file).cmp(&(hits[b].repo, &hits[b].file)));
    for i in order {
        let h = &hits[i];
        if cache.as_ref().is_none_or(|(r, f, _)| *r != h.repo || *f != h.file) {
            let text = std::fs::read(roots[h.repo].join(&h.file)).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
            let lines = text.lines().map(|l| l.trim_end().chars().take(SNIPPET).collect()).collect();
            cache = Some((h.repo, h.file.clone(), lines));
        }
        let lines = &cache.as_ref().unwrap().2;
        let at = h.line as usize - 1;
        if at >= lines.len() {
            continue;
        }
        let lo = at.saturating_sub(n);
        let hi = (at + n + 1).min(lines.len());
        let block = &lines[lo..hi];
        let indent = block.iter().filter(|l| !l.trim().is_empty()).map(|l| l.len() - l.trim_start().len()).min().unwrap_or(0);
        let dedent = |l: &String| if l.len() >= indent { l[indent..].to_string() } else { l.trim_start().to_string() };
        let (before, after): (Vec<String>, Vec<String>) = (block[..at - lo].iter().map(dedent).collect(), block[at - lo + 1..].iter().map(dedent).collect());
        let h = &mut hits[i];
        h.text = dedent(&lines[at]);
        h.before = before;
        h.after = after;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directory_or_a_gone_file_has_nothing_to_read_and_is_not_a_failure() {
        let dir = std::env::temp_dir().join(format!("kc-readable-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("submodule")).unwrap();
        std::fs::write(dir.join("a.txt"), "x").unwrap();
        let mut buf = Vec::new();
        assert_eq!(readable(read_into(&dir.join("a.txt"), &mut buf)), Some(true));
        assert_eq!(readable(read_into(&dir.join("submodule"), &mut buf)), Some(false));
        assert_eq!(readable(read_into(&dir.join("gone.txt"), &mut buf)), Some(false));
        assert_eq!(readable(Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn re(p: &str) -> Pattern {
        Pattern { re: RegexBuilder::new(p).multi_line(true).build().unwrap(), ahead: None, behind: None }
    }

    fn opts(pattern: &str) -> SearchOptions {
        SearchOptions { pattern: pattern.into(), regex: true, case: Case::Sensitive, word: false, langs: Vec::new(), globs: Vec::new(), max_lines: 100 }
    }

    #[test]
    fn a_lookaround_at_either_end_is_checked_on_each_match() {
        let data = b"item.SetParent(a);\nvar f = item.SetParent;\nSetParent(b);\n";
        let lines = |p: &str| match_lines(&compile(&opts(p)).unwrap().re, data, "text", &mut String::new()).iter().map(|h| h.line).collect::<Vec<_>>();
        assert_eq!(lines(r"\bSetParent\b(?!\()"), [2], "a use that isn't a call");
        assert_eq!(lines(r"\bSetParent\b(?=\()"), [1, 3]);
        assert_eq!(lines(r"(?<!\.)\bSetParent\("), [3], "a call not through a member access");
        assert_eq!(lines(r"(?<=\.)SetParent(?!\()"), [2]);
        // A match failing its look-ahead gives way to a later one on the same line.
        let data = b"Save(); Save;\n";
        let m = compile(&opts(r"Save(?!\()")).unwrap();
        assert_eq!(match_lines(&m.re, data, "text", &mut String::new())[0].start, 8);
        assert!(!m.re.is_match(b"Save();"));
        // Case and whole-word options apply inside the look-around too.
        let m = compile(&SearchOptions { case: Case::Insensitive, word: true, ..opts(r"save(?!\s*\()") }).unwrap();
        assert!(m.re.is_match(b"x = SAVE;") && !m.re.is_match(b"SAVE ()") && !m.re.is_match(b"saved;"));
    }

    #[test]
    fn only_lookaround_at_the_ends_is_split_off() {
        assert_eq!(split_lookaround(r"a(?!b)"), (None, "a".into(), Some((true, "b".into()))));
        assert_eq!(split_lookaround(r"(?<=x)a(?=b)"), (Some((false, "x".into())), "a".into(), Some((false, "b".into()))));
        assert_eq!(split_lookaround(r"a(b)(?!c[)])"), (None, "a(b)".into(), Some((true, "c[)]".into()))));
        assert_eq!(split_lookaround(r"a\(?!b\)"), (None, r"a\(?!b\)".into(), None), "escaped parentheses are no group");
        assert_eq!(split_lookaround(r"(?!b)"), (None, "(?!b)".into(), None), "nothing left to match");
        // Look-around elsewhere is left for the regex engine to refuse, as before.
        assert!(compile(&opts(r"a(?!b)c")).is_err());
    }

    #[test]
    fn lines_like_git_grep() {
        let data = b"alpha beta\ngamma\nbeta beta\n";
        let hits = match_lines(&re("beta"), data, "text", &mut String::new());
        assert_eq!(hits.iter().map(|h| (h.line, h.start)).collect::<Vec<_>>(), vec![(1, 6), (3, 0)]);
        // A buffer match that crosses a newline never counts; the line's own match does.
        let hits = match_lines(&re(r"a\s+g"), data, "text", &mut String::new());
        assert!(hits.is_empty());
        let hits = match_lines(&re(r"^beta$|a[\s\S]*gamma"), b"xa\ngamma\n", "text", &mut String::new());
        assert_eq!(hits.len(), 0);
        let hits = match_lines(&re(r"gamma$"), data, "text", &mut String::new());
        assert_eq!(hits.iter().map(|h| h.line).collect::<Vec<_>>(), vec![2]);
        // `^` matches every line but not the empty tail after the last newline.
        assert_eq!(match_lines(&re("^"), data, "text", &mut String::new()).len(), 3);
        assert_eq!(match_lines(&re("x*"), b"a\n\nb", "text", &mut String::new()).len(), 3);
    }

    #[test]
    fn comments() {
        assert!(in_comment("swift", b"    // load the store", 10));
        assert!(in_comment("swift", b"let x = 1 // store", 14));
        assert!(!in_comment("swift", b"let url = \"https://store\"", 20));
        assert!(!in_comment("swift", b"let store = 1", 4));
        assert!(in_comment("python", b"x = 1  # store", 9));
        assert!(!in_comment("swift", b"#if DEBUG", 1));
        assert!(in_comment("rust", b" * store", 3));
        let doc = b"def f():\n    \"\"\"Load the store.\n\n    The store is cached.\n    \"\"\"\n    return store\n";
        let hits = match_lines(&re("store"), doc, "python", &mut String::new());
        assert_eq!(hits.iter().map(|h| (h.line, h.comment)).collect::<Vec<_>>(), vec![(2, true), (4, true), (6, false)]);
    }

    #[test]
    fn globs() {
        assert!(path_glob("*.swift", "Sources/App/A.swift"));
        assert!(path_glob("Sources/**/*.swift", "Sources/App/A.swift"));
        assert!(path_glob("Sources/**/*.swift", "Sources/A.swift"));
        assert!(!path_glob("Sources/*.swift", "Sources/App/A.swift"));
        assert!(path_glob("Tests/", "Tests/X/A.swift"));
        assert!(path_glob("*Tests*", "Tests/ATests.swift"));
        assert!(!path_glob("*.py", "a.pyc"));
        assert!(path_glob("crates/*/src/*.rs", "crates/kc-index/src/lib.rs"));
    }

    /// The bounded matcher must preserve even unusual wildcard runs and slash behavior.
    #[test]
    fn invariant_glob_matches_recursive_definition() {
        fn reference(p: &[u8], s: &[u8]) -> bool {
            match p.first() {
                None => s.is_empty(),
                Some(b'*') if p.get(1) == Some(&b'*') => {
                    // `**/` also matches nothing.
                    let rest = if p.get(2) == Some(&b'/') { &p[3..] } else { &p[2..] };
                    (0..=s.len()).any(|i| reference(rest, &s[i..])) || (p.get(2) == Some(&b'/') && reference(&p[2..], s))
                }
                Some(b'*') => {
                    let rest = &p[1..];
                    for i in 0..=s.len() {
                        if reference(rest, &s[i..]) {
                            return true;
                        }
                        if i < s.len() && s[i] == b'/' {
                            break;
                        }
                    }
                    false
                }
                Some(b'?') => !s.is_empty() && s[0] != b'/' && reference(&p[1..], &s[1..]),
                Some(&c) => !s.is_empty() && s[0] == c && reference(&p[1..], &s[1..]),
            }
        }

        fn words(alphabet: &[u8], max: usize) -> Vec<Vec<u8>> {
            let mut all = vec![Vec::new()];
            let mut level = vec![Vec::new()];
            for _ in 0..max {
                let mut next = Vec::new();
                for word in &level {
                    for &b in alphabet {
                        let mut w = word.clone();
                        w.push(b);
                        next.push(w);
                    }
                }
                all.extend(next.iter().cloned());
                level = next;
            }
            all
        }
        let patterns = words(b"a/*?", 5);
        let paths = words(b"ab/", 5);
        for pattern in &patterns {
            for path in &paths {
                assert_eq!(glob_match(pattern, path), reference(pattern, path), "pattern {pattern:?}, path {path:?}");
                assert_eq!(glob_match_bounded(pattern, path), reference(pattern, path), "pattern {pattern:?}, path {path:?}");
            }
        }
        for pattern in ["**/é?", "*é*", "?", "**/*/", "***a"] {
            for path in ["é", "a/éa", "é/", "日本語", "a/b/"] {
                assert_eq!(glob_match(pattern.as_bytes(), path.as_bytes()), reference(pattern.as_bytes(), path.as_bytes()));
            }
        }
        // Previously exponential rejection; its running time should be bounded by pattern * path length.
        assert!(!glob_match(b"*a*a*a*a*a*a*a*a*b*", &[b'a'; 1024]));
    }

    #[test]
    fn globs_after_a_repo_name_apply_in_that_repo_only() {
        let base = std::env::temp_dir().join(format!("knowcode-test-globs-in-{}", std::process::id()));
        let (web, server) = (base.join("web"), base.join("server"));
        std::fs::create_dir_all(server.join("web")).unwrap(); // the server repo has a `web` directory of its own
        std::fs::create_dir_all(&web).unwrap();
        let names = vec!["web".to_string(), "server".to_string()];
        let g = |globs: &[&str], root: &Path| globs_in(&globs.iter().map(|g| g.to_string()).collect::<Vec<_>>(), root, &names);
        // After the repo's name: relative to it; the other repo is left out.
        assert_eq!(g(&["web/src/**"], &web), Some(vec!["src/**".to_string()]));
        assert_eq!(g(&["server/**"], &web), None);
        assert_eq!(g(&["web"], &web), Some(vec!["**".to_string()]));
        // After `**/` too, the way an agent scopes a glob to one repo; `**/` before any other name stays as written.
        assert_eq!(g(&["**/web/src/**"], &web), Some(vec!["src/**".to_string()]));
        assert_eq!(g(&["**/server/src/**"], &web), None);
        assert_eq!(g(&["**/src/**"], &web), Some(vec!["**/src/**".to_string()]));
        // A repo holding a directory named like the other repo keeps the glob as written.
        assert_eq!(g(&["web/**"], &server), Some(vec!["web/**".to_string()]));
        // Exclusions and plain globs apply everywhere; an exclusion alone leaves no repo out.
        assert_eq!(g(&["*.ts", "!web/dist/**"], &web), Some(vec!["*.ts".to_string(), "!dist/**".to_string()]));
        assert_eq!(g(&["!server/bin/**"], &web), Some(vec![]));
        // An absolute path: relative to the repo it is in, and for no other.
        let abs = format!("{}/src/**", web.display());
        assert_eq!(g(&[&abs], &web), Some(vec!["src/**".to_string()]));
        assert_eq!(g(&[&abs], &server), None);
        assert_eq!(g(&["/elsewhere/**"], &web), None);
        // Extensions name languages.
        assert_eq!((lang_name("ts"), lang_name(".cs"), lang_name("Python"), lang_name("cobol")), ("typescript".into(), "csharp".into(), "python".into(), "cobol".into()));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn smart_case() {
        assert!(!has_uppercase(r"\bfoo\S+\p{Lu}", true));
        assert!(has_uppercase("Foo", true));
        assert!(has_uppercase("Foo", false));
    }

    #[test]
    fn test_paths() {
        assert!(is_test_path("Tests/AppTests/StoreTests.swift"));
        assert!(is_test_path("tests/test_api.py"));
        assert!(is_test_path("src/foo_test.py"));
        assert!(!is_test_path("Sources/App/Testimonial.swift"));
        assert!(!is_test_path("Sources/App/Latest.swift"));
    }

    #[test]
    fn parameter_names_from_signatures() {
        let names = |sig: &str, name: &str| params_of(sig, name).map(|(n, _)| n);
        let some = |s: &str| Some(s.to_string());
        // C#: after attributes, past an explicit interface's generics; `this`, attributes and defaults dropped.
        assert_eq!(names("protected virtual void Emit(string eventName, object? payload)", "Emit"), some("(eventName, payload)"));
        assert_eq!(names("[HttpPost(\"{id}\")] public async Task<IActionResult> Put(Guid id, [FromBody] Model m = null)", "Put"), some("(id, m)"));
        assert_eq!(names("void IObserver<KeyValuePair<string, object?>>.OnError(Exception error)", "OnError"), some("(error)"));
        assert_eq!(names("public static void Run(this Listener l, Func<int, string> f, params object[] rest)", "Run"), some("(l, f, rest)"));
        // TypeScript and JavaScript: annotations, optional marks, defaults, arrows in types, destructuring, spreads.
        assert_eq!(names("const loadOrder = async ( client: Client, orderId?: string, cb: (x: number) => void = noop )", "loadOrder"), some("(client, orderId, cb)"));
        assert_eq!(names("export const OrderPanel = ({ mode = 'view' }: Props, ...rest: unknown[])", "OrderPanel"), some("({…}, rest)"));
        assert_eq!(names("reset(clearAll = false)", "reset"), some("(clearAll)"));
        // Python, Swift, Rust: the receiver is the container's.
        assert_eq!(names("def load(self, path: str, *, strict: bool = True)", "load"), some("(path, *, strict)"));
        assert_eq!(names("func load(from url: URL, _ retries: Int = 3) throws -> Data", "load"), some("(url, retries)"));
        assert_eq!(names("pub fn scan(&self, roots: &[PathBuf], threads: usize) -> Scanned", "scan"), some("(roots, threads)"));
        // None to give: no list, an empty one, only a receiver.
        assert_eq!(names("refresh()", "refresh"), None);
        assert_eq!(names("public int Count { get; }", "Count"), None);
        assert_eq!(names("def close(self)", "close"), None);
        // Long lists are cut; the whole list comes back too, to tell a hit on a signature line.
        let long = names("function f(alpha, bravo, charlie, delta, echo, foxtrot, golf, hotel, india)", "f").unwrap();
        assert!(long.ends_with("…)") && long.chars().count() == PARAMS_CHARS + 3, "{long}");
        assert_eq!(params_of("f(a: string | number)", "f").unwrap().1, "a: string | number");
    }

    #[test]
    fn statement_continuations_and_labels() {
        assert!(continues("    .then(x => x)") && continues("?.map(f)") && !continues("...rest,") && !continues("a.b()"));
        assert!(label("case 404:") && label("  default:") && label("case Keys.Start:") && !label("case 'x': {") && !label("default: break;"));
    }

    #[test]
    fn snippets() {
        assert_eq!(snippet("    let x = 1", 8, 9), "let x = 1");
        let long = format!("{}needle{}", "a".repeat(300), "b".repeat(300));
        let s = snippet(&long, 300, 306);
        assert!(s.contains("needle") && s.starts_with('…') && s.ends_with('…'));
    }
}
