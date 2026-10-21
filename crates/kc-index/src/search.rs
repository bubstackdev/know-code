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
    re: Regex,
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
    let base = if o.regex { o.pattern.clone() } else { regex::escape(&o.pattern) };
    let pattern = if o.word { format!(r"(?-u:\b)(?:{base})(?-u:\b)") } else { base };
    let re = RegexBuilder::new(&pattern)
        .case_insensitive(ignore_case)
        .multi_line(true)
        .size_limit(50 << 20)
        .build()
        .with_context(|| format!("invalid pattern {:?}", o.pattern))?;
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
/// not for this one, unless the repo has a directory of that name (then the glob as written stays too). An absolute
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
pub fn match_lines(re: &Regex, data: &[u8], lang: &str, text: &mut String) -> Vec<LineHit> {
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
        let found = if m.end() <= le { Some((s - ls, m.end() - ls)) } else { re.find(line).map(|x| (x.start(), x.end())) };
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

/// The returned page: snippet, declared or enclosing symbol, and `context` lines around each hit (re-read from
/// the files, which the search just read).
pub fn finish(roots: &[PathBuf], scanned: &Scanned, files: &[FileData], hits: &[Hit], context: usize) -> Vec<PageHit> {
    let mut page: Vec<PageHit> = hits
        .iter()
        .map(|h| {
            let f = &files[h.file as usize];
            let (symbol, inside) = match h.declared {
                Some(i) => (Some(qualified(&f.symbols[i as usize])), None),
                None => {
                    let l = h.line;
                    let enclosing = f
                        .symbols
                        .iter()
                        .filter(|s| s.start_line <= l && l <= s.end_line)
                        .max_by_key(|s| (s.start_line, std::cmp::Reverse(s.end_line)));
                    (None, enclosing.map(qualified))
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
                before: Vec::new(),
                after: Vec::new(),
            }
        })
        .collect();
    add_context(roots, &mut page, context);
    page
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

    fn re(p: &str) -> Regex {
        RegexBuilder::new(p).multi_line(true).build().unwrap()
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
    fn snippets() {
        assert_eq!(snippet("    let x = 1", 8, 9), "let x = 1");
        let long = format!("{}needle{}", "a".repeat(300), "b".repeat(300));
        let s = snippet(&long, 300, 306);
        assert!(s.contains("needle") && s.starts_with('…') && s.ends_with('…'));
    }
}
