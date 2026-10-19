//! Concept search: files ranked for a plain-language query ("refund approval", "lock domains for transfer") by BM25
//! over the words of their entities, with no embeddings.
//!
//! Each file is one document with four fields, all identifier words split at camelCase, `snake_case` and digits,
//! lowercased and lightly stemmed: its path, the names of the symbols it defines, their doc comments, and the names
//! the symbols' bodies reference. The fields are weighted (a path word counts most, a referenced name least) and the
//! weighted term frequency saturates as in BM25. Length normalisation is weak: on replayed commits, files that
//! changed for a subject were more often the big ones, and full normalisation buried them.
//!
//! The words live in the `files_fts` FTS5 table (contentless: only the inverted index is kept), one row per file,
//! rewritten with the file's other facts; term frequencies are read back through the `files_fts_terms` vocabulary
//! table, so the ranking here, not FTS5's fixed `bm25()`, decides the weights.
//!
//! Two more sources join the ranking when the caller gives them ([`Extra`]):
//!
//! - Text files the index doesn't hold (YAML, JSON, Markdown, shell scripts, Dockerfiles, Terraform, unindexed
//!   ColdFusion pages) as documents of the same four fields: their path, their headings, keys and labels, their
//!   comments and prose, and their values and commands (see [`text_document`]).
//! - A fifth field, the words of the commit subjects that changed each file: the team's own words for what a file
//!   does, which a ticket-title query shares more often than the code's identifiers do.
//!
//! Test files count half unless the query mentions tests, so the implementation ranks above its tests.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::split_words;

/// Field weights, in the table's column order: path, names, docs, refs.
const WEIGHTS: [f64; 4] = [2.0, 1.0, 0.5, 0.5];
const COLUMNS: [&str; 4] = ["path", "names", "docs", "refs"];
const K1: f64 = 1.2;
/// Length normalisation, from 0 (none) to 1 (full).
const B: f64 = 0.1;
/// Weight of the commit-subject field (see [`Extra::history`]), per commit naming the word.
const HISTORY_WEIGHT: f64 = 0.5;
/// A text file's score is scaled by this: its words are looser than code's (a changelog names everything).
const TEXT_FACTOR: f64 = 0.5;
/// A test file's score is scaled by this unless the query mentions tests.
const TEST_FACTOR: f64 = 0.5;
/// Query words that ask for tests: with one, test files keep their full score.
const TEST_WORDS: &[&str] = &["test", "tests", "testing", "tested", "spec", "specs", "unittest", "unittests", "e2e", "mock", "mocks", "fixture", "fixtures"];
/// Words kept per field of a text file's document.
const TEXT_FIELD_CAP: usize = 400;
/// Bytes of a text file read for its document.
pub const TEXT_READ_CAP: usize = 64 << 10;

/// Query words that say nothing about where code is: English function words, the verbs of commit subjects, and
/// words every repo's paths share.
const STOP: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "been", "but", "by", "can", "could", "did", "do", "does", "done", "for",
    "from", "get", "got", "has", "have", "in", "into", "is", "it", "its", "of", "on", "or", "our", "so", "than", "that",
    "the", "their", "them", "then", "there", "these", "this", "those", "to", "too", "up", "use", "used", "uses",
    "using", "via", "was", "were", "when", "where", "which", "while", "will", "with", "without", "would", "add", "added",
    "adds", "adding", "fix", "fixed", "fixes", "fixing", "update", "updated", "updates", "updating", "remove", "removed",
    "removes", "removing", "change", "changed", "changes", "changing", "make", "made", "makes", "new", "now", "not",
    "no", "more", "less", "also", "only", "some", "all", "any", "should", "must", "may", "minor", "small", "cleanup",
    "clean", "code", "issue", "issues", "bug", "bugs", "work", "working", "merge", "branch", "version", "bump",
    "initial", "misc", "stuff", "test", "tests", "i", "we", "you", "me", "my", "your", "per", "e", "g", "eg", "ie",
    "vs", "etc", "just", "like", "onto", "out", "off", "over", "under", "again", "back", "already", "still", "yet",
    "one", "two", "three", "first", "last", "next", "same", "other", "another", "each", "every", "both", "either",
    "neither",
    // Code-specific: file extensions and words in nearly every path.
    "cs", "ts", "js", "html", "json", "xml", "api", "apis", "src", "lib", "dll", "app", "impl",
];

/// A light English stemmer: plurals and `-ing` / `-ed` endings, so `refunds`, `refunding` and `refunded` meet
/// `refund`. Replay on held-out commits preferred it to no stemming.
pub fn stem(w: &str) -> String {
    let n = w.len();
    if n <= 3 || !w.is_ascii() || w.bytes().all(|b| b.is_ascii_digit()) {
        return w.to_string();
    }
    for (suf, rep) in [("ies", "y"), ("sses", "ss"), ("ches", "ch"), ("shes", "sh"), ("xes", "x")] {
        if w.ends_with(suf) && n - suf.len() >= 2 {
            return format!("{}{rep}", &w[..n - suf.len()]);
        }
    }
    let undouble = |b: &str| -> String {
        let bs = b.as_bytes();
        let k = bs.len();
        if k >= 3 && bs[k - 1] == bs[k - 2] && !b"lsz".contains(&bs[k - 1]) { b[..k - 1].to_string() } else { b.to_string() }
    };
    if w.ends_with("ing") && n >= 6 {
        return undouble(&w[..n - 3]);
    }
    if w.ends_with("ed") && n >= 5 {
        let b = &w[..n - 2];
        if let Some(s) = b.strip_suffix('i') {
            return format!("{s}y");
        }
        return undouble(b);
    }
    if w.ends_with('s') && !w.ends_with("ss") && !w.ends_with("us") && !w.ends_with("is") {
        return w[..n - 1].to_string();
    }
    w.to_string()
}

/// Stemmed words of free text or identifiers. With `whole`, an identifier of several words also gives its words
/// joined (`refundApproval` gives `refund`, `approval`, `refundapproval`), so a query naming it whole matches.
fn words_into(text: &str, whole: bool, out: &mut Vec<String>) {
    for raw in text.split(|c: char| !(c.is_alphanumeric() || c == '_')).filter(|s| !s.is_empty()) {
        let parts = split_words(raw);
        if whole && parts.len() > 1 {
            out.push(stem(&parts.concat()));
        }
        out.extend(parts.iter().map(|p| stem(p)));
    }
}

/// One file's row: the words of each field, space-separated, and the total word count.
pub(crate) fn file_document(path: &str, facts: &kc_extract::FileFacts) -> ([String; 4], i64) {
    let mut fields: [Vec<String>; 4] = Default::default();
    // The path without its extension; the file's stem also whole (`OrderRefundService`).
    let no_ext = path.rsplit_once('.').filter(|(_, ext)| !ext.contains('/')).map_or(path, |(p, _)| p);
    words_into(no_ext, false, &mut fields[0]);
    let file_name = path.rsplit('/').next().unwrap_or(path);
    let file_stem = file_name.split('.').next().unwrap_or(file_name);
    let stem_words = split_words(file_stem);
    if stem_words.len() > 1 {
        fields[0].push(stem(&stem_words.concat()));
    }
    for s in &facts.symbols {
        words_into(&s.name, true, &mut fields[1]);
        if let Some(d) = &s.doc {
            words_into(d, false, &mut fields[2]);
        }
    }
    // Names referenced from inside the file's symbols, once per symbol, kind and line: several occurrences on a line
    // are one use as far as concepts go.
    let mut seen = std::collections::HashSet::new();
    for r in facts.refs.iter().filter(|r| r.from.is_some() && seen.insert((r.from, r.name.as_str(), r.kind, r.line))) {
        words_into(&r.name, false, &mut fields[3]);
    }
    let len = fields.iter().map(|f| f.len() as i64).sum();
    (fields.map(|f| f.join(" ")), len)
}

/// The query's words: each word and, for an identifier of several words, the whole; minus stop words; stemmed.
pub fn query_terms(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in text.split(|c: char| !(c.is_alphanumeric() || c == '_')).filter(|s| !s.is_empty()) {
        let parts = split_words(raw);
        let mut words = Vec::new();
        if parts.len() > 1 {
            words.push(parts.concat());
        }
        words.extend(parts);
        for w in words {
            if w.chars().count() < 2 || w.chars().all(|c| c.is_ascii_digit()) || STOP.contains(&w.as_str()) {
                continue;
            }
            let s = stem(&w);
            if !out.contains(&s) {
                out.push(s);
            }
        }
    }
    out
}

/// The words of a commit subject, as [`query_terms`] gives them, without what names the change rather than the
/// code: a Conventional Commits type (`fix(api):`), bracketed tags (`[WIP]`), ticket ids (`ABC-123`, `sc-123`,
/// `#123`) and URLs.
pub fn subject_terms(subject: &str) -> Vec<String> {
    const TYPES: &[&str] =
        &["feat", "feature", "fix", "chore", "refactor", "docs", "doc", "test", "tests", "style", "perf", "build", "ci", "revert", "wip", "hotfix", "bugfix"];
    let mut s = subject.trim().to_string();
    // `fix(scope)!: rest` → `scope rest`: the type names the change; the scope can name the code.
    if let Some((head, rest)) = s.split_once(':') {
        let (t, scope) = head.trim_end_matches('!').split_once('(').map_or((head.trim_end_matches('!'), ""), |(t, sc)| (t, sc.trim_end_matches(')')));
        if TYPES.contains(&t.to_ascii_lowercase().as_str()) {
            s = format!("{scope} {rest}");
        }
    }
    let mut kept = String::with_capacity(s.len());
    let mut depth = 0u32;
    for c in s.chars() {
        match c {
            '[' => depth += 1,
            ']' => depth = depth.saturating_sub(1),
            _ if depth == 0 => kept.push(c),
            _ => {}
        }
    }
    let words: Vec<&str> = kept
        .split_whitespace()
        .filter(|w| {
            let w = w.trim_matches(|c: char| !c.is_alphanumeric() && c != '#');
            let ticket = w.starts_with('#') && w.len() > 1 && w[1..].bytes().all(|b| b.is_ascii_digit())
                || w.split_once('-').is_some_and(|(a, n)| {
                    !a.is_empty() && a.bytes().all(|b| b.is_ascii_alphabetic()) && !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())
                });
            !ticket && !w.contains("://")
        })
        .collect();
    query_terms(&words.join(" "))
}

/// What sort of text file a path is, when `locate` reads it as a document: `markdown`, `yaml`, `json`, `shell`,
/// `docker`, `terraform` (`.tf`, `.tfvars`, `.hcl`), `template` (`.tftpl`, `.tpl`, Jinja `.j2`) or `cfml`.
pub fn text_kind(path: &str) -> Option<&'static str> {
    let file = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    if file.starts_with("dockerfile") || file.ends_with(".dockerfile") {
        return Some("docker");
    }
    if file.contains(".min.") || matches!(file.as_str(), "package-lock.json" | "composer.lock" | "npm-shrinkwrap.json") {
        return None;
    }
    match file.rsplit_once('.').map(|(_, e)| e)? {
        "md" | "markdown" => Some("markdown"),
        "yml" | "yaml" => Some("yaml"),
        "json" => Some("json"),
        "sh" | "bash" | "zsh" | "ps1" | "psm1" => Some("shell"),
        "tf" | "tfvars" | "hcl" => Some("terraform"),
        "tftpl" | "tpl" | "j2" | "jn2" | "jinja" | "jinja2" => Some("template"),
        "cfm" | "cfc" | "cfs" => Some("cfml"),
        _ => None,
    }
}

/// A text file's document for [`locate`]: its words by field (path; headings, keys and labels; comments and prose;
/// values and commands), each field capped, as weighted term frequencies.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TextDoc {
    pub path: String,
    /// (stemmed word, field-weighted count), sorted by word.
    pub terms: Vec<(String, f32)>,
    pub len: u32,
}

/// The document of the text file at `path` (of [`text_kind`] `kind`) holding `text` (only its first
/// [`TEXT_READ_CAP`] bytes are read).
pub fn text_document(path: &str, kind: &str, text: &str) -> TextDoc {
    let mut fields: [Vec<String>; 4] = Default::default();
    let no_ext = path.rsplit_once('.').filter(|(_, ext)| !ext.contains('/')).map_or(path, |(p, _)| p);
    words_into(no_ext, false, &mut fields[0]);
    let mut cut = text.len().min(TEXT_READ_CAP);
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut in_comment = false;
    for raw in text[..cut].lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if kind == "cfml" {
            // `<!--- … --->` comments are prose; the rest is markup and code.
            let starts = line.contains("<!---");
            if starts || in_comment {
                in_comment = !line.contains("--->");
                put(&mut fields[2], line, false);
            } else {
                put(&mut fields[3], line, false);
            }
            continue;
        }
        let comment = match kind {
            "markdown" => None,
            "terraform" => line.strip_prefix('#').or_else(|| line.strip_prefix("//")),
            "json" => None,
            "template" => line.strip_prefix("{#").or_else(|| line.strip_prefix('#')).or_else(|| line.strip_prefix("//")),
            _ => line.strip_prefix('#').filter(|_| !line.starts_with("#!")),
        };
        if let Some(c) = comment {
            put(&mut fields[2], c, false);
            continue;
        }
        match kind {
            "markdown" => {
                if let Some(h) = line.strip_prefix('#') {
                    put(&mut fields[1], h.trim_start_matches('#'), true);
                } else {
                    put(&mut fields[2], line, false);
                }
            }
            "yaml" | "json" => {
                let l = line.trim_start_matches("- ").trim_start_matches('{').trim_start_matches('"');
                match l.split_once(':').or_else(|| l.split_once("\":")) {
                    Some((k, v)) if !k.contains(' ') || k.len() <= 40 => {
                        put(&mut fields[1], k.trim_end_matches('"'), true);
                        put(&mut fields[3], v, false);
                    }
                    _ => put(&mut fields[3], l, false),
                }
            }
            "terraform" => {
                let first = line.split_whitespace().next().unwrap_or("");
                if matches!(first, "resource" | "data" | "module" | "variable" | "output" | "locals" | "provider") {
                    put(&mut fields[1], line.trim_end_matches('{'), true);
                } else if let Some((k, v)) = line.split_once('=') {
                    put(&mut fields[1], k, true);
                    put(&mut fields[3], v, false);
                } else {
                    put(&mut fields[3], line, false);
                }
            }
            "template" => {
                // A rendered config: `key = value`, `key: value` or `key value;` (nginx); the key names, the rest refers.
                let split = line.find(['=', ':']).filter(|&i| !line[..i].trim().contains(' ')).map(|i| (&line[..i], &line[i + 1..]));
                match split {
                    Some((k, v)) => {
                        put(&mut fields[1], k, true);
                        put(&mut fields[3], v, false);
                    }
                    None => put(&mut fields[3], line, false),
                }
            }
            "shell" => {
                let f = line.strip_prefix("function ").or_else(|| line.split_once("()").map(|(n, _)| n).filter(|n| !n.contains(' ')));
                match f {
                    Some(name) => put(&mut fields[1], name, true),
                    None => put(&mut fields[3], line, false),
                }
            }
            "docker" => {
                let (instr, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
                if matches!(instr.to_ascii_uppercase().as_str(), "LABEL" | "ENV" | "ARG" | "FROM" | "EXPOSE" | "ENTRYPOINT") {
                    put(&mut fields[1], rest, false);
                } else {
                    put(&mut fields[3], rest, false);
                }
            }
            _ => put(&mut fields[3], line, false),
        }
    }
    let mut tf: HashMap<String, f32> = HashMap::new();
    let mut len = 0;
    for (i, f) in fields.iter().enumerate() {
        for w in f.iter().take(TEXT_FIELD_CAP) {
            *tf.entry(w.clone()).or_default() += WEIGHTS[i] as f32;
            len += 1;
        }
    }
    let mut terms: Vec<(String, f32)> = tf.into_iter().collect();
    terms.sort_by(|a, b| a.0.cmp(&b.0));
    TextDoc { path: path.to_string(), terms, len }
}

/// Adds a line's words to a field, dropping pure numbers and one-letter words.
fn put(field: &mut Vec<String>, text: &str, whole: bool) {
    if field.len() >= TEXT_FIELD_CAP {
        return;
    }
    let mut words = Vec::new();
    words_into(text, whole, &mut words);
    field.extend(words.into_iter().filter(|w| w.len() >= 2 && !w.bytes().all(|b| b.is_ascii_digit())));
}

/// What [`locate_with`] ranks besides the index's files.
#[derive(Default)]
pub struct Extra<'a> {
    /// Text files the index doesn't hold, as documents.
    pub texts: &'a [TextDoc],
    /// Per path, the stemmed words of the commit subjects that changed it, each with its number of commits.
    pub history: Option<&'a HashMap<String, Vec<(String, u32)>>>,
}

/// A file ranked for a concept query.
#[derive(Debug, Clone, Serialize)]
pub struct LocateHit {
    pub file: String,
    pub score: f64,
    /// Query words (stemmed) the file matched.
    pub terms: Vec<String>,
    /// The file's symbols whose names or doc comments match, best first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub symbols: Vec<LocateSymbol>,
    /// How many of its symbols match, when more than the few listed do.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbols_total: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LocateSymbol {
    pub name: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    pub line: u32,
    pub end_line: u32,
}

/// Symbols listed per file.
const SYMBOLS_PER_FILE: usize = 3;

/// Files ranked for `text`, best first, at most `limit`, and the number of files matching any query word.
pub fn locate(conn: &Connection, text: &str, limit: usize) -> Result<(Vec<LocateHit>, usize)> {
    locate_with(conn, text, limit, &Extra::default())
}

/// Whether a query asks for tests.
fn mentions_tests(text: &str) -> bool {
    text.split(|c: char| !c.is_alphanumeric()).flat_map(split_words).any(|w| TEST_WORDS.contains(&w.as_str()))
}

/// [`locate`] over the index's files and `extra`'s text files, with its commit-subject field.
pub fn locate_with(conn: &Connection, text: &str, limit: usize, extra: &Extra) -> Result<(Vec<LocateHit>, usize)> {
    let terms = query_terms(text);
    if terms.is_empty() {
        return Ok((Vec::new(), 0));
    }
    // A config file indexed only for its hosts is a text document, as an unindexed one is.
    let mut paths: HashMap<i64, String> = conn
        .prepare_cached("SELECT id, path FROM files WHERE lang != 'hostconfig'")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    // Each text file's document: its indexed file's (an Ansible template's words join its row), else `-1 - i`.
    let text_ids: Vec<i64> = {
        let by_path: HashMap<&str, i64> = paths.iter().map(|(id, p)| (p.as_str(), *id)).collect();
        extra.texts.iter().enumerate().map(|(i, t)| by_path.get(t.path.as_str()).copied().unwrap_or(-1 - i as i64)).collect()
    };
    for (t, &id) in extra.texts.iter().zip(&text_ids) {
        if id < 0 {
            paths.insert(id, t.path.clone());
        }
    }
    // Document lengths, their mean and the document count.
    let mut lens: HashMap<i64, f64> = HashMap::new();
    {
        let mut stmt = conn.prepare_cached("SELECT rowid, len FROM files_fts")?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))? {
            let (id, len) = row?;
            lens.insert(id, len as f64);
        }
    }
    for (t, &id) in extra.texts.iter().zip(&text_ids) {
        *lens.entry(id).or_default() += t.len as f64;
    }
    let n = lens.len() as f64;
    if n == 0.0 {
        return Ok((Vec::new(), 0));
    }
    // Summed in a fixed order, so the mean doesn't vary in its last bits.
    let mut all: Vec<f64> = lens.values().copied().collect();
    all.sort_by(f64::total_cmp);
    let avg = (all.iter().sum::<f64>() / n).max(1.0);
    // The history field's postings for the query's words: word → (document, commits).
    let mut said: HashMap<&str, Vec<(i64, u32)>> = HashMap::new();
    if let Some(history) = extra.history {
        let ids: HashMap<&str, i64> = paths.iter().map(|(id, p)| (p.as_str(), *id)).collect();
        let wanted: HashSet<&str> = terms.iter().map(String::as_str).collect();
        for (path, words) in history {
            let Some(&id) = ids.get(path.as_str()) else { continue };
            for (w, c) in words {
                if let Some(t) = wanted.get(w.as_str()) {
                    said.entry(t).or_default().push((id, *c));
                }
            }
        }
    }

    let mut scores: HashMap<i64, (f64, Vec<usize>)> = HashMap::new();
    let mut stmt = conn.prepare_cached("SELECT doc, col, count(*) FROM files_fts_terms WHERE term = ?1 GROUP BY doc, col")?;
    let mut idfs = Vec::with_capacity(terms.len());
    for (ti, t) in terms.iter().enumerate() {
        let mut tf: HashMap<i64, f64> = HashMap::new();
        for row in stmt.query_map([t], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?)))? {
            let (doc, col, count) = row?;
            let w = COLUMNS.iter().position(|c| *c == col).map_or(0.0, |i| WEIGHTS[i]);
            *tf.entry(doc).or_default() += w * count as f64;
        }
        for (d, &id) in extra.texts.iter().zip(&text_ids) {
            if let Ok(k) = d.terms.binary_search_by(|(w, _)| w.as_str().cmp(t)) {
                *tf.entry(id).or_default() += d.terms[k].1 as f64;
            }
        }
        for (doc, c) in said.get(t.as_str()).into_iter().flatten() {
            *tf.entry(*doc).or_default() += HISTORY_WEIGHT * *c as f64;
        }
        let df = tf.len() as f64;
        let idf = (0.5 + (n - df + 0.5) / (df + 0.5)).ln().max(1e-6);
        idfs.push(idf);
        for (doc, f) in tf {
            let len = lens.get(&doc).copied().unwrap_or(avg);
            let s = idf * f * (K1 + 1.0) / (f + K1 * (1.0 - B + B * len / avg));
            let e = scores.entry(doc).or_default();
            e.0 += s;
            e.1.push(ti);
        }
    }
    let total = scores.len();
    let tests_wanted = mentions_tests(text);
    let mut ranked: Vec<(i64, f64, Vec<usize>)> = scores
        .into_iter()
        .filter_map(|(id, (s, ts))| {
            let path = paths.get(&id)?;
            let mut s = s;
            if id < 0 {
                s *= TEXT_FACTOR;
            }
            if !tests_wanted && crate::is_test_path(path) {
                s *= TEST_FACTOR;
            }
            Some((id, s, ts))
        })
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| paths[&a.0].cmp(&paths[&b.0])));
    ranked.truncate(limit);

    let mut hits = Vec::with_capacity(ranked.len());
    for (id, score, ts) in ranked {
        let matched: Vec<&String> = ts.iter().map(|&i| &terms[i]).collect();
        let mut symbols = if id < 0 { Vec::new() } else { matching_symbols(conn, id, &terms, &idfs)? };
        let symbols_total = (symbols.len() > SYMBOLS_PER_FILE).then_some(symbols.len());
        symbols.truncate(SYMBOLS_PER_FILE);
        hits.push(LocateHit {
            file: paths.remove(&id).unwrap_or_default(),
            score: (score * 1000.0).round() / 1000.0,
            terms: matched.iter().map(|t| t.to_string()).collect(),
            symbols,
            symbols_total,
        });
    }
    Ok((hits, total))
}

/// The file's symbols whose names (or, at half weight, doc comments) contain query words, by the summed idf of the
/// words they contain; ties go to types before members, then to shorter names.
fn matching_symbols(conn: &Connection, file_id: i64, terms: &[String], idfs: &[f64]) -> Result<Vec<LocateSymbol>> {
    let mut stmt = conn.prepare_cached(
        "SELECT name, kind, container, doc, start_line, end_line FROM symbols WHERE file_id = ?1 ORDER BY start_line",
    )?;
    let mut scored: Vec<(f64, u8, LocateSymbol)> = Vec::new();
    for row in stmt.query_map(params![file_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, u32>(4)?,
            r.get::<_, u32>(5)?,
        ))
    })? {
        let (name, kind, container, doc, line, end_line) = row?;
        let mut name_words = Vec::new();
        words_into(&name, true, &mut name_words);
        let name_words: HashSet<String> = name_words.into_iter().collect();
        let mut doc_words = Vec::new();
        words_into(doc.as_deref().unwrap_or(""), false, &mut doc_words);
        let doc_words: HashSet<String> = doc_words.into_iter().collect();
        let score: f64 = terms
            .iter()
            .zip(idfs)
            .map(|(t, idf)| {
                if name_words.contains(t) {
                    *idf
                } else if doc_words.contains(t) {
                    idf * 0.5
                } else {
                    0.0
                }
            })
            .sum();
        if score > 0.0 {
            let type_first = u8::from(!crate::query::is_type_or_extension(&kind));
            scored.push((score, type_first, LocateSymbol { name, kind, container, line, end_line }));
        }
    }
    // Equal scores: the shorter name says less else; then file order (the sort is stable).
    scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.name.len().cmp(&b.2.name.len())));
    Ok(scored.into_iter().map(|(_, _, s)| s).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stems_plurals_and_verb_endings() {
        for (w, s) in [
            ("refunds", "refund"),
            ("approvals", "approval"),
            ("entries", "entry"),
            ("addresses", "address"),
            ("batches", "batch"),
            ("locking", "lock"),
            ("running", "run"),
            ("locked", "lock"),
            ("applied", "apply"),
            ("status", "status"),
            ("class", "class"),
            ("analysis", "analysis"),
            ("api", "api"),
            ("2024", "2024"),
        ] {
            assert_eq!(stem(w), s, "{w}");
        }
    }

    #[test]
    fn a_file_document_holds_path_names_docs_and_referenced_names() {
        let src = "namespace Shop;\n/// <summary>Approves pending refunds.</summary>\npublic class RefundApprover\n{\n    public bool Approve(Order order) => order.CanRefund();\n}\n";
        let facts = kc_extract::extract(kc_extract::Lang::CSharp, src);
        let (fields, len) = file_document("src/Billing/RefundApprover.cs", &facts);
        assert_eq!(fields[0], "src bill refund approver refundapprover");
        assert_eq!(fields[1], "refundapprover refund approver approve");
        assert_eq!(fields[2], "approve pend refund");
        assert_eq!(fields[3], "order can refund");
        assert_eq!(len, fields.iter().map(|f| f.split(' ').count() as i64).sum::<i64>());
    }

    #[test]
    fn subject_words_leave_out_tickets_and_types() {
        assert_eq!(subject_terms("fix(api): sc-1234 [WIP] Refund approvals for ORD-77 #12 https://x.example/y"), ["refund", "approval"]);
        assert_eq!(subject_terms("Lock domains: transfer"), ["lock", "domain", "transfer"]);
    }

    #[test]
    fn text_documents_read_keys_headings_comments_and_values() {
        let d = text_document("deploy/consul.yml", "yaml", "# Registers the service\nservice_name: orders-api\nchecks:\n  - http: /health\n");
        let has = |w: &str| d.terms.iter().find(|(t, _)| t == w).map(|(_, f)| *f);
        assert_eq!(has("deploy"), Some(2.0));
        assert_eq!(has("register"), Some(0.5));
        assert_eq!(has("service"), Some(1.5), "a key word (1) and a comment word (0.5)");
        assert_eq!(has("servicename"), Some(1.0));
        assert_eq!(has("health"), Some(0.5));
        let md = text_document("docs/RUNBOOK.md", "markdown", "# Failover runbook\nSwitch the primary database.\n");
        assert!(md.terms.iter().any(|(t, f)| t == "failover" && *f == 1.0));
        assert!(md.terms.iter().any(|(t, f)| t == "primary" && *f == 0.5));
        let tf = text_document("infra/main.tf", "terraform", "resource \"aws_s3_bucket\" \"invoices\" {\n  acl = \"private\"\n}\n");
        assert!(tf.terms.iter().any(|(t, f)| t == "invoice" && *f == 1.0));
        assert_eq!(text_kind("app/Dockerfile.prod"), Some("docker"));
        for (p, k) in [("infra/user_data.tftpl", "template"), ("deploy/app.conf.j2", "template"), ("x/policy.tpl", "template"), ("env/prod.hcl", "terraform")] {
            assert_eq!(text_kind(p), Some(k), "{p}");
        }
        let t = text_document("templates/nginx.conf.j2", "template", "{# Serves the storefront #}\nserver_name {{ shop_host }};\nproxy_pass = http://upstream_cart;\n");
        assert!(t.terms.iter().any(|(w, f)| w == "storefront" && *f == 0.5));
        assert!(t.terms.iter().any(|(w, f)| w == "proxypass" && *f == 1.0));
        assert!(t.terms.iter().any(|(w, _)| w == "cart"));
        assert_eq!(text_kind("package-lock.json"), None);
        assert_eq!(text_kind("src/a.cs"), None);
    }

    #[test]
    fn tests_are_asked_for_by_name() {
        assert!(mentions_tests("flaky unit tests for refunds"));
        assert!(mentions_tests("RefundSpec"));
        assert!(!mentions_tests("latest refunds"));
    }

    #[test]
    fn query_words_drop_stop_words_and_keep_identifiers_whole() {
        assert_eq!(query_terms("Fix the refund approvals in OrderService"), ["refund", "approval", "orderservice", "order", "service"]);
        assert_eq!(query_terms("add new api"), Vec::<String>::new());
        assert_eq!(query_terms("lock domains for transfer"), ["lock", "domain", "transfer"]);
    }
}
