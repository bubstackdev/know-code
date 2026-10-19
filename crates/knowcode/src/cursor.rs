//! Continuing a cut list: an opaque cursor per list an answer cut (`cursor` for the answer's own `items`,
//! `<list>_cursor` beside a nested one), which `knowcode more <cursor>` and the MCP tool `kc_more` take to return the
//! next page of that list.
//!
//! Why a cursor rather than `offset` and `limit` per list: a page is only the rest of a list when it is computed from
//! the same index as the first, and for lists a walk cuts (impact's dependents, a path search's paths) the rest is
//! not a slice of a longer run. A cursor carries what the next page needs and checks both: the request itself (so a
//! nested list is continued without restating the call), the list, how far it was delivered, the index generation
//! of every repo the answer read (a cursor from an older generation is refused, "stale cursor; start again", never
//! mixed into a newer answer), and a hash of the rows delivered so far (so a list whose order changed meanwhile, a
//! ranking recomputed say, is refused too). `search` and `grep-ast` keep their `offset` as well.
//!
//! How each list continues ([`Mode`]): most lists are cut as a prefix of their order (references, a context's callers,
//! conformers, members, callees and accessors, trace nodes, search and structural hits, rename sites), and the next
//! page is the next slice of the same list computed with its limit raised. A walk's cut list (impact's `affected`, a
//! path search's `paths`) ranks what the walk reached; its continuation lists the rows of the walk without the limit
//! that the first answer didn't show, ranked the same way, so following every cursor gives every row exactly once. A
//! path search's candidate ends past the ones it searched from continue as the same search from the next ones.
//!
//! Operations make an answer continuable with [`offer`]; it keeps what the cursors are made from in the envelope's
//! private `_resume` field, which output removes ([`seal`]) and anything cutting the answer further (the MCP size
//! cap) updates through [`cut`], so a list it shortens gets the same kind of cursor.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::ops::{self, Scope};

/// The envelope field holding what cursors are made from; never output.
const FIELD: &str = "_resume";
/// What every cursor starts with: the format's version.
const PREFIX: &str = "kc1.";

/// A call whose lists can be continued: the operation and its parameters, as the operation was called.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Refs {
        name: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        kinds: Vec<String>,
        limit: usize,
    },
    Context {
        symbol: String,
        budget: usize,
        source: String,
        /// Rows per list of a repo's deploy answer.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<usize>,
    },
    Trace {
        symbol: String,
        direction: String,
        depth: u8,
        limit: usize,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        edges: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        kinds: Vec<String>,
        /// The evidence policy (see `kc_store::answer::EvidencePolicy`); empty for `default`.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        evidence: String,
    },
    Impact {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        symbol: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diff: Option<String>,
        depth: u8,
        limit: usize,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        edges: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        kinds: Vec<String>,
        order: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        as_of: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        since: Option<String>,
        /// The evidence policy (see `kc_store::answer::EvidencePolicy`); empty for `default`.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        evidence: String,
    },
    Path {
        from: String,
        to: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to_repo: Option<PathBuf>,
        max_hops: u8,
        max_nodes: usize,
        max_repos: usize,
        paths: usize,
        one_way: bool,
        allow_ambiguous: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        edges: Vec<String>,
        from_skip: usize,
        to_skip: usize,
        /// The evidence policy (see `kc_store::answer::EvidencePolicy`); empty for `default`.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        evidence: String,
    },
    Rename {
        symbol: String,
        new_name: String,
        allow_ambiguous: bool,
    },
    Search {
        pattern: String,
        regex: bool,
        case: String,
        word: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        langs: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        globs: Vec<String>,
        max_lines: usize,
        context: usize,
        files_only: bool,
        limit: usize,
    },
    GrepAst {
        pattern: String,
        lang: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        inside: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        has: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        not_inside: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        not_has: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        regex: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        globs: Vec<String>,
        max_matches: usize,
        limit: usize,
    },
}

/// A request and the repos it was asked of.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Query {
    #[serde(flatten)]
    req: Request,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    repos: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    no_sync: bool,
}

/// How a list's next page is made.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Mode {
    /// The list is cut as a prefix of its order: the next page is the next slice of it, computed with its limit
    /// raised to cover the page.
    Prefix,
    /// A walk's list, ranked after the walk stopped: the rows of the walk without the limit that the first answer
    /// didn't show, ranked.
    Rest,
    /// A path search's candidate ends: the same search from the next ones.
    Window,
}

fn mode(req: &Request, list: &str) -> Mode {
    let last = list.rsplit('/').next().unwrap_or("");
    match (req, last) {
        (Request::Impact { .. }, "affected") | (Request::Path { .. }, "paths") => Mode::Rest,
        (Request::Path { .. }, "from_candidates" | "to_candidates") => Mode::Window,
        _ => Mode::Prefix,
    }
}

/// One continuable list of an answer: where its rows sit in the request's own answer (`l`, a JSON pointer), how many
/// rows came before this answer's (`o0`), where the delivered rows' hash starts (`b`) and its value up to `o0`
/// (`h0`), the page size, how many rows the first answer showed (for [`Mode::Rest`]), and whether more rows follow.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ListState {
    l: String,
    o0: usize,
    b: usize,
    h0: u64,
    n: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    k: Option<usize>,
    more: bool,
}

/// What the cursors of one answer are made from (the envelope's `_resume`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Resume {
    q: Query,
    /// The generations the answer was computed at (see [`gens`]).
    g: Vec<String>,
    /// By the list's JSON pointer in this answer.
    lists: BTreeMap<String, ListState>,
}

/// A cursor's content.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Cursor {
    q: Query,
    g: Vec<String>,
    /// The list, as a JSON pointer into the request's answer.
    l: String,
    /// Rows delivered so far, the next page's start.
    o: usize,
    /// Where the delivered rows' hash starts (a search's first page may start past 0).
    #[serde(default, skip_serializing_if = "is_zero")]
    b: usize,
    /// The hash of the rows delivered from `b` to `o`.
    h: String,
    /// Page size.
    n: usize,
    /// The rows the first answer showed ([`Mode::Rest`]).
    #[serde(default, skip_serializing_if = "is_zero")]
    k: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// A list an operation offers to continue: its pointer in the answer, its page size (the operation's limit for it),
/// whether the operation cut it, and the rows that came before the answer's (a search's `offset`).
pub struct List {
    pub ptr: String,
    pub n: usize,
    pub more: bool,
    pub start: usize,
}

impl List {
    pub fn new(ptr: impl Into<String>, n: usize, more: bool) -> List {
        List { ptr: ptr.into(), n: n.max(1), more, start: 0 }
    }
}

/// Makes `lists` of the answer `env` to `req` (asked of `scope`) continuable: records what their cursors are made
/// from, and gives each one cut its cursor. `extra` adds generations the index's doesn't cover (a search's content
/// files).
pub fn offer(env: &mut Value, req: Request, scope: &Scope, lists: Vec<List>, extra: &[String]) {
    let mut g = gens(env);
    g.extend(extra.iter().cloned());
    let lists = lists
        .into_iter()
        .map(|l| (l.ptr.clone(), ListState { l: l.ptr, o0: l.start, b: l.start, h0: FNV_BASIS, n: l.n, k: None, more: l.more }))
        .collect();
    let r = Resume { q: Query { req, repos: scope.repos.clone(), no_sync: scope.no_sync }, g, lists };
    stamp(env, &r);
    env[FIELD] = json!(r);
}

/// The answer's [`Resume`], taken out of it (to measure the answer as it will be output).
pub fn take(env: &mut Value) -> Option<Resume> {
    let r = env.as_object_mut()?.remove(FIELD)?;
    serde_json::from_value(r).ok()
}

/// Removes what cursors are made from: for output.
pub fn seal(env: &mut Value) {
    if let Some(obj) = env.as_object_mut() {
        obj.remove(FIELD);
    }
}

/// The answer's list at `ptr` was cut further (by the size cap): marks it continued and renews its cursor. False when
/// the list isn't one that continues.
pub fn cut(env: &mut Value, r: &mut Resume, ptr: &str) -> bool {
    let Some(st) = r.lists.get_mut(ptr) else { return false };
    st.more = true;
    stamp(env, r);
    true
}

/// Gives each list that has more rows its cursor, made from its rows as they are now, and takes it from those that
/// don't.
fn stamp(env: &mut Value, r: &Resume) {
    for (ptr, st) in &r.lists {
        let Some(rows) = env.pointer(ptr).and_then(Value::as_array) else { continue };
        // A window of candidate ends moves on by the window, whatever its rows collapse to.
        let o = if mode(&r.q.req, &st.l) == Mode::Window { st.o0 + st.n } else { st.o0 + rows.len() };
        let c = st.more.then(|| Cursor {
            q: r.q.clone(),
            g: r.g.clone(),
            l: st.l.clone(),
            o,
            b: st.b,
            h: format!("{:016x}", chain(st.h0, rows)),
            n: st.n,
            // Only a walk's ranked list needs how many rows the first answer showed.
            k: if mode(&r.q.req, &st.l) == Mode::Rest { st.k.unwrap_or(st.o0 + rows.len()) } else { 0 },
        });
        let (parent, key) = cursor_field(ptr);
        if let Some(Value::Object(obj)) = env.pointer_mut(&parent) {
            match c {
                Some(c) => {
                    obj.insert(key, Value::from(encode(&c)));
                }
                None => {
                    obj.remove(&key);
                }
            }
        }
    }
}

/// Where the cursor of the list at `ptr` goes: the answer's `cursor` for its `items`, else `<list>_cursor` beside
/// the list.
fn cursor_field(ptr: &str) -> (String, String) {
    if ptr == "/items" {
        return (String::new(), "cursor".into());
    }
    let (parent, list) = ptr.rsplit_once('/').unwrap_or(("", ptr));
    (parent.to_string(), format!("{list}_cursor"))
}

/// The cursor continuing the list at `ptr` of `env`, if it has one.
pub fn cursor_at<'a>(env: &'a Value, ptr: &str) -> Option<&'a str> {
    let (parent, key) = cursor_field(ptr);
    env.pointer(&parent)?.get(&key)?.as_str()
}

/// The exact call continuing a list from `cursor`.
/// The exact call continuing a list whose cursor is in the answer's field `field` (`cursor`,
/// `items[0].callers_cursor`). Names the field rather than repeating the cursor: an answer cut to save room
/// shouldn't spend it on copies.
pub fn call(field: &str) -> String {
    format!("continue: kc_more {{cursor: <`{field}`>}}; CLI: knowcode more <`{field}`>")
}

/// The field holding the cursor of the list at `ptr`, readable (`items[0].callers_cursor`).
pub fn field_of(ptr: &str) -> String {
    let (parent, key) = cursor_field(ptr);
    let parent = readable(&parent);
    if parent.is_empty() { key } else { format!("{parent}.{key}") }
}

/// Where in `env` the cursor `value` is, as a readable field.
pub fn find(env: &Value, value: &str) -> Option<String> {
    fn go(v: &Value, at: &str, value: &str) -> Option<String> {
        match v {
            Value::String(s) if s == value => Some(at.to_string()),
            Value::Array(a) => a.iter().enumerate().find_map(|(i, x)| go(x, &format!("{at}[{i}]"), value)),
            Value::Object(m) => m
                .iter()
                .filter(|(k, _)| !matches!(k.as_str(), "incomplete" | "_resume"))
                .find_map(|(k, x)| go(x, &if at.is_empty() { k.clone() } else { format!("{at}.{k}") }, value)),
            _ => None,
        }
    }
    go(env, "", value)
}

/// The generations an answer was computed at: each repo it read (`provenance`), its index generation and HEAD,
/// hashed short.
fn gens(env: &Value) -> Vec<String> {
    let mut out: Vec<String> = env["provenance"]
        .as_array()
        .map(|list| {
            list.iter()
                .map(|p| {
                    let s = format!("{}|{}|{}", p["repo"].as_str().unwrap_or(""), p["generation"].as_str().unwrap_or(""), p["commit"].as_str().unwrap_or(""));
                    format!("{:08x}", fnv(FNV_BASIS, s.as_bytes()) as u32)
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out.dedup();
    out
}

/// A repo's content files, as a generation for cursors: a search reads files the index's generation doesn't cover.
pub fn content_gen(root: &std::path::Path, files: &[kc_store::content::CFile]) -> String {
    let mut h = fnv(FNV_BASIS, root.to_string_lossy().as_bytes());
    for f in files {
        h = fnv(h, f.path.as_bytes());
        h = fnv(h, &f.size.to_le_bytes());
        h = fnv(h, &f.mtime_ns.to_le_bytes());
    }
    format!("c{:08x}", h as u32)
}

const FNV_BASIS: u64 = 0xcbf2_9ce4_8422_2325;

fn fnv(mut h: u64, bytes: &[u8]) -> u64 {
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// What identifies a row across runs: the place and the thing at it, not how it was reached (an impact row's edge
/// and hop change when a longer walk finds a better way to it).
fn key(row: &Value) -> String {
    // A row that is a line of text (a deploy answer's `ci_skipped`, `own`): the line.
    if !row.is_object() {
        return row.to_string();
    }
    let mut out = String::new();
    for k in ["repo", "site_repo", "file", "line", "col", "column", "qualified", "name", "kind", "parent", "depth", "at", "run", "target"] {
        if let Some(v) = row.get(k).filter(|v| !v.is_null()) {
            out.push_str(&v.to_string());
        }
        out.push('\u{1f}');
    }
    if let Some(steps) = row.get("steps").and_then(Value::as_array) {
        for s in steps {
            out.push_str(&format!("{}@{}:{}>", s["qualified"], s["file"], s["line"]));
        }
    }
    out
}

/// A row's identity for [`Mode::Rest`]: the symbol or site, whatever way reached it.
fn subject(row: &Value) -> String {
    let mut out = String::new();
    for k in ["repo", "file", "line", "qualified", "kind"] {
        if let Some(v) = row.get(k).filter(|v| !v.is_null()) {
            out.push_str(&v.to_string());
        }
        out.push('\u{1f}');
    }
    if let Some(steps) = row.get("steps").and_then(Value::as_array) {
        for s in steps {
            out.push_str(&format!("{}@{}:{}>", s["qualified"], s["file"], s["line"]));
        }
    }
    out
}

/// The hash of `rows` delivered after those `h` covers.
fn chain(h: u64, rows: &[Value]) -> u64 {
    rows.iter().fold(h, |h, r| fnv(fnv(h, key(r).as_bytes()), b"\x1e"))
}

fn encode(c: &Cursor) -> String {
    format!("{PREFIX}{}", b64(serde_json::to_string(c).unwrap_or_default().as_bytes()))
}

fn decode(s: &str) -> Result<Cursor> {
    let bad = || anyhow::anyhow!("not a cursor this KnowCode made: pass the `cursor` (or `<list>_cursor`) of an answer as it is");
    let body = s.trim().strip_prefix(PREFIX).ok_or_else(bad)?;
    let bytes = unb64(body).ok_or_else(bad)?;
    serde_json::from_slice(&bytes).map_err(|_| bad())
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// URL-safe base64 without padding.
fn b64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 4 / 3 + 3);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

fn unb64(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let vals: Vec<u32> = s.bytes().map(|c| B64.iter().position(|x| *x == c).map(|p| p as u32)).collect::<Option<_>>()?;
    for chunk in vals.chunks(4) {
        if chunk.len() < 2 {
            return None;
        }
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, v)| n | v << (18 - 6 * i));
        for i in 0..chunk.len() - 1 {
            out.push((n >> (16 - 8 * i) & 255) as u8);
        }
    }
    Some(out)
}

/// How far a run of the request goes.
#[derive(Clone, Copy)]
enum Raise {
    /// As first asked.
    None,
    /// The cut lists' limits raised to at least this.
    Upto(usize),
    /// Without the walk's limit.
    All,
    /// A path search from candidate ends past these (from, to).
    Skip(usize, usize),
}

/// Runs the request, its limits raised as `raise` says. The envelope keeps its `_resume`.
fn run(q: &Query, raise: Raise) -> Result<Value> {
    let scope = Scope { repos: q.repos.clone(), no_sync: q.no_sync };
    let up = |limit: usize| match raise {
        Raise::Upto(n) => limit.max(n),
        Raise::All => usize::MAX,
        _ => limit,
    };
    Ok(match &q.req {
        Request::Refs { name, kinds, limit } => ops::refs(name, kinds, up(*limit), &scope)?.env,
        Request::Context { symbol, budget, source, limit } => {
            let raise = match raise {
                Raise::Upto(n) => Some(n),
                Raise::All => Some(usize::MAX),
                _ => None,
            };
            ops::context_raised(symbol, *budget, source.parse()?, *limit, raise, &scope)?.env
        }
        Request::Trace { symbol, direction, depth, limit, edges, kinds, evidence } => {
            let filter = ops::WalkFilter { edges: edges.clone(), kinds: kinds.clone(), evidence: evidence.clone() };
            ops::trace(symbol, direction, *depth, up(*limit), &filter, &scope)?.env
        }
        Request::Impact { symbol, file, diff, depth, limit, edges, kinds, order, as_of, since, evidence } => {
            let (limit, coupled_max) = match raise {
                Raise::All => (usize::MAX, None),
                Raise::Upto(n) => (*limit, Some(n)),
                _ => (*limit, None),
            };
            let p = ops::ImpactParams {
                symbol: symbol.clone(),
                file: file.clone(),
                diff: diff.clone(),
                depth: *depth,
                limit,
                filter: ops::WalkFilter { edges: edges.clone(), kinds: kinds.clone(), evidence: evidence.clone() },
                order: order.parse()?,
                cochange: kc_index::vitals::Span { as_of: as_of.clone(), since: since.clone() },
                coupled_max,
            };
            ops::impact(&p, &scope)?.env
        }
        Request::Path { from, to, to_repo, max_hops, max_nodes, max_repos, paths, one_way, allow_ambiguous, edges, from_skip, to_skip, evidence } => {
            let (from_skip, to_skip) = match raise {
                Raise::Skip(f, t) => (f, t),
                _ => (*from_skip, *to_skip),
            };
            let opts = crate::walk::PathOptions {
                max_hops: *max_hops,
                max_nodes: *max_nodes,
                max_repos: *max_repos,
                paths: up(*paths),
                one_way: *one_way,
                allow_ambiguous: *allow_ambiguous,
                edges: ops::edge_filter(edges, evidence)?,
                from_skip,
                to_skip,
            };
            crate::walk::path(from, to, to_repo.as_deref(), &opts, &scope)?.env
        }
        Request::Rename { symbol, new_name, allow_ambiguous } => {
            let p = crate::refactor::RenameParams { symbol: symbol.clone(), new_name: new_name.clone(), allow_ambiguous: *allow_ambiguous };
            crate::refactor::rename(&p, &scope)?.env
        }
        Request::Search { pattern, regex, case, word, langs, globs, max_lines, context, files_only, limit } => {
            let case = match case.as_str() {
                "insensitive" => kc_index::search::Case::Insensitive,
                "smart" => kc_index::search::Case::Smart,
                _ => kc_index::search::Case::Sensitive,
            };
            let opts = kc_index::search::SearchOptions {
                pattern: pattern.clone(),
                regex: *regex,
                case,
                word: *word,
                langs: langs.clone(),
                globs: globs.clone(),
                max_lines: *max_lines,
            };
            ops::search(&ops::SearchParams { opts, limit: up(*limit), offset: 0, context: *context, files_only: *files_only }, &scope)?.env
        }
        Request::GrepAst { pattern, lang, selector, inside, has, not_inside, not_has, regex, globs, max_matches, limit } => {
            let opts = kc_index::structural::AstOptions {
                pattern: pattern.clone(),
                lang: lang.clone(),
                selector: selector.clone(),
                inside: inside.clone(),
                has: has.clone(),
                not_inside: not_inside.clone(),
                not_has: not_has.clone(),
                regex: regex.clone(),
                globs: globs.clone(),
                max_matches: *max_matches,
            };
            ops::grep_ast(&ops::AstParams { opts, limit: up(*limit), offset: 0 }, &scope)?.env
        }
    })
}

/// The rows of the list at `ptr` of `env`.
fn rows_at(env: &Value, ptr: &str) -> Vec<Value> {
    env.pointer(ptr).and_then(Value::as_array).cloned().unwrap_or_default()
}

/// The generations of an answer as its `_resume` records them.
fn gens_of(env: &Value) -> Vec<String> {
    env[FIELD]["g"].as_array().map(|a| a.iter().filter_map(|g| g.as_str().map(str::to_string)).collect()).unwrap_or_default()
}

/// Refuses a cursor whose answer was computed at another generation than `now`'s.
fn check_gens(c: &Cursor, now: &Value) -> Result<()> {
    let have = gens_of(now);
    if let Some(gone) = c.g.iter().find(|g| !have.contains(g)) {
        let what = if gone.starts_with('c') { "a searched file" } else { "a repo the answer read" };
        bail!("stale cursor: {what} changed since this cursor was made (the index is at a newer generation); start again with the original call");
    }
    Ok(())
}

/// The list's known total in the request's answer: `<list>_total` (or `callers_summary.total`) beside it, the
/// answer's `total` for its items.
fn known_total(env: &Value, ptr: &str) -> Option<u64> {
    if ptr == "/items" {
        return env["total"].as_u64();
    }
    let (parent, list) = ptr.rsplit_once('/')?;
    let obj = env.pointer(parent)?;
    if list == "callers" {
        return obj["callers_summary"]["total"].as_u64();
    }
    obj.get(format!("{list}_total")).and_then(Value::as_u64)
}

/// The next page of the list a cursor continues, as an answer: `items` the page, `list` the list it continues, `offset`
/// its first row's place in the list, `total` when known, and `cursor` for the page after. Refuses a stale cursor.
pub fn more(cursor: &str) -> Result<Value> {
    let c = decode(cursor)?;
    let m = mode(&c.q.req, &c.l);
    if m == Mode::Window {
        return window(&c);
    }
    let (env, full, total) = match m {
        Mode::Prefix => {
            let env = run(&c.q, Raise::Upto(c.o + c.n + 1))?;
            check_gens(&c, &env)?;
            let full = rows_at(&env, &c.l);
            let total = known_total(&env, &c.l);
            (env, full, total)
        }
        _ => {
            let first = run(&c.q, Raise::None)?;
            check_gens(&c, &first)?;
            let shown: Vec<Value> = rows_at(&first, &c.l).into_iter().take(c.k).collect();
            let env = run(&c.q, Raise::All)?;
            check_gens(&c, &env)?;
            let seen: std::collections::HashSet<String> = shown.iter().map(subject).collect();
            let mut full = shown;
            full.extend(rows_at(&env, &c.l).into_iter().filter(|r| !seen.contains(&subject(r))));
            let total = Some(full.len() as u64);
            (env, full, total)
        }
    };
    let delivered = full.get(c.b.min(full.len())..c.o.min(full.len())).unwrap_or_default();
    if c.o > full.len() || format!("{:016x}", chain(FNV_BASIS, delivered)) != c.h {
        bail!("stale cursor: the list changed since this cursor was made (its order or rows differ); start again with the original call");
    }
    let page: Vec<Value> = full[c.o..].iter().take(c.n).cloned().collect();
    let more = full.len() > c.o + page.len();
    let mut out = json!({});
    // The evidence policy the request walked under holds for the page, and says what it left out.
    for k in ["repo", "commit", "dirty", "freshness", "repos", "knowcode", "provenance", "policy"] {
        if let Some(v) = env.get(k) {
            out[k] = v.clone();
        }
    }
    // What made the request's answer partial besides its own cuts (unreadable files, stale data) holds for the page.
    let carried: Vec<kc_store::answer::Incomplete> = crate::contract::incomplete_of(&env)
        .into_iter()
        .filter(|i| !matches!(i.reason, kc_store::answer::Reason::Capped | kc_store::answer::Reason::Budget))
        .collect();
    let n = page.len();
    out["list"] = Value::from(readable(&c.l));
    out["items"] = Value::Array(page);
    out["offset"] = json!(c.o);
    match total {
        Some(t) => out["total"] = json!(t.max((c.o + n) as u64)),
        None if !more => out["total"] = json!(c.o + n),
        None => {}
    }
    out["truncated"] = json!(more);
    // A page of a rename's sites is never a preview to apply.
    if matches!(c.q.req, Request::Rename { .. }) {
        out["refused"] = json!(true);
        out["reasons"] = json!(["a page of a rename preview's sites, not the whole preview: apply only a preview whose sites are all listed"]);
    }
    for i in carried {
        crate::contract::push_incomplete(&mut out, i);
    }
    let h = u64::from_str_radix(&c.h, 16).unwrap_or(FNV_BASIS);
    let st = ListState { l: c.l.clone(), o0: c.o, b: c.b, h0: h, n: c.n, k: Some(c.k), more };
    let r = Resume { q: c.q.clone(), g: c.g.clone(), lists: BTreeMap::from([("/items".to_string(), st)]) };
    stamp(&mut out, &r);
    out[FIELD] = json!(r);
    crate::contract::finish(&mut out);
    Ok(out)
}

/// A path search from the next candidate ends: the search's own answer, its cursors for the ends after these.
fn window(c: &Cursor) -> Result<Value> {
    let Request::Path { from_skip, to_skip, .. } = &c.q.req else { bail!("not a path search's cursor") };
    let skips = if c.l.ends_with("from_candidates") { (c.o, *to_skip) } else { (*from_skip, c.o) };
    let mut env = run(&c.q, Raise::Skip(skips.0, skips.1))?;
    check_gens(c, &env)?;
    env["list"] = Value::from(readable(&c.l));
    env["offset"] = json!(c.o);
    crate::contract::finish(&mut env);
    Ok(env)
}

/// `/items/0/callers` as `items[0].callers`.
fn readable(ptr: &str) -> String {
    let mut out = String::new();
    for part in ptr.split('/').filter(|p| !p.is_empty()) {
        if part.bytes().all(|b| b.is_ascii_digit()) {
            out.push_str(&format!("[{part}]"));
        } else {
            if !out.is_empty() {
                out.push('.');
            }
            out.push_str(part);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::tests::{repo, test_cache};
    use crate::ops::{ImpactParams, WalkFilter};
    use std::path::Path;

    fn scope(r: &Path) -> Scope {
        Scope { repos: vec![r.to_path_buf()], no_sync: false }
    }

    /// The list at `ptr` of `env` and every page its cursors give after it, in order; and how many pages.
    fn follow(env: &Value, ptr: &str) -> (Vec<Value>, usize) {
        let mut rows = rows_at(env, ptr);
        let mut next = cursor_at(env, ptr).map(str::to_string);
        let mut pages = 0;
        while let Some(c) = next {
            let page = more(&c).unwrap_or_else(|e| panic!("{e:#}: {c}"));
            // A page is cut while a cursor goes on; what holds for the whole answer (a walk's frontier) is carried.
            let cut = crate::contract::incomplete_of(&page).iter().any(|i| matches!(i.reason, kc_store::answer::Reason::Capped | kc_store::answer::Reason::Budget));
            assert_eq!(cut, page.get("cursor").is_some(), "{page}");
            rows.extend(rows_at(&page, "/items"));
            next = page["cursor"].as_str().map(str::to_string);
            pages += 1;
            assert!(pages < 100, "cursors never end");
        }
        (rows, pages)
    }

    /// Rows as their keys, for comparing lists.
    fn keys(rows: &[Value]) -> Vec<String> {
        rows.iter().map(key).collect()
    }

    fn sorted(mut v: Vec<String>) -> Vec<String> {
        v.sort();
        v
    }

    /// Every `capped` entry of an answer says how to get the rest: its cursor's call, when the list continues.
    fn capped_hints_continue(env: &Value) {
        for i in crate::contract::incomplete_of(env).iter().filter(|i| i.reason == kc_store::answer::Reason::Capped) {
            let h = i.hint.as_deref().unwrap_or("");
            assert!(h.starts_with("continue: kc_more {cursor: <`") && !h.contains("kc1."), "{}: {h}", i.detail);
            // Each field it names holds a cursor.
            for part in h.split("<`").skip(1) {
                let field = part.split("`>").next().unwrap();
                let ptr = format!("/{}", field.replace(['.', '['], "/").replace(']', ""));
                assert!(env.pointer(&ptr).and_then(Value::as_str).is_some_and(|c| c.starts_with("kc1.")), "{field} in {h}");
            }
        }
    }

    /// A C# repo: `Hub.Ping` called from 55 methods (each in its own class implementing `IThing`), `Hub.Fan` calling
    /// 55 methods, and an EF entity read by 12 methods.
    fn fixture(base: &Path) -> PathBuf {
        let mut files: Vec<(String, String)> = vec![
            ("src/Shop.csproj".into(), "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n".into()),
            ("src/IThing.cs".into(), "namespace N;\npublic interface IThing { void Go(); }\n".into()),
            (
                "src/Db.cs".into(),
                "namespace N;\n[Table(\"cursor_ledger\")]\npublic class Ledger { }\npublic class LedgerContext : DbContext\n{\n    public DbSet<Ledger> Ledgers { get; set; }\n}\n".into(),
            ),
        ];
        let fan: String = (0..55).map(|i| format!("        new T{i}().Go();\n")).collect();
        files.push(("src/Hub.cs".into(), format!("namespace N;\npublic class Hub\n{{\n    public static void Ping() {{ }}\n    public void Fan()\n    {{\n{fan}    }}\n}}\n")));
        for i in 0..55 {
            files.push((format!("src/T{i}.cs"), format!("namespace N;\npublic class T{i} : IThing\n{{\n    public void Go() {{ Hub.Ping(); }}\n}}\n")));
        }
        for i in 0..12 {
            files.push((
                format!("src/Reader{i}.cs"),
                format!("namespace N;\npublic class Reader{i}\n{{\n    private readonly LedgerContext _db;\n    public object Read() => _db.Ledgers.Where(o => o != null).ToList();\n}}\n"),
            ));
        }
        let files: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        repo(base, "app", &files)
    }

    #[test]
    fn invariant_following_every_cursor_gives_the_uncapped_list() {
        let cache = test_cache();
        let base = cache.dir("cursor-follow");
        let root = fixture(&base);
        let s = scope(&root);

        // References: a prefix of their order, page after page.
        let env = ops::refs("Ping", &[], 7, &s).unwrap().env;
        capped_hints_continue(&env);
        let all = ops::refs("Ping", &[], 10_000, &s).unwrap().env;
        let (rows, pages) = follow(&env, "/items");
        assert!(pages >= 4, "{pages}");
        assert_eq!(keys(&rows), keys(&rows_at(&all, "/items")));

        // A context's callers, conformers and accessors.
        let env = ops::context("Hub.Ping", 600, Default::default(), &s).unwrap().env;
        capped_hints_continue(&env);
        let all = ops::context_raised("Hub.Ping", 600, Default::default(), None, Some(10_000), &s).unwrap().env;
        let (rows, _) = follow(&env, "/items/0/callers");
        assert_eq!(rows.len(), 55);
        assert_eq!(keys(&rows), keys(&rows_at(&all, "/items/0/callers")));
        let env = ops::context("IThing", 600, Default::default(), &s).unwrap().env;
        capped_hints_continue(&env);
        assert_eq!(env["items"][0]["conformers"].as_array().map(Vec::len), Some(50));
        let all = ops::context_raised("IThing", 600, Default::default(), None, Some(10_000), &s).unwrap().env;
        let (rows, _) = follow(&env, "/items/0/conformers");
        assert_eq!(rows.len(), 55);
        assert_eq!(keys(&rows), keys(&rows_at(&all, "/items/0/conformers")));
        let env = ops::context("Ledger", 600, Default::default(), &s).unwrap().env;
        capped_hints_continue(&env);
        let all = ops::context_raised("Ledger", 600, Default::default(), None, Some(10_000), &s).unwrap().env;
        let (rows, _) = follow(&env, "/items/0/tables/0/accessors");
        assert_eq!(rows.len(), 12, "{env}");
        assert_eq!(keys(&rows), keys(&rows_at(&all, "/items/0/tables/0/accessors")));

        // A trace's nodes: a prefix of the walk's order, each page continuing it.
        let env = ops::trace("Hub.Fan", "out", 1, 6, &WalkFilter::default(), &s).unwrap().env;
        capped_hints_continue(&env);
        let all = ops::trace("Hub.Fan", "out", 1, 10_000, &WalkFilter::default(), &s).unwrap().env;
        let (rows, _) = follow(&env, "/items/0/nodes");
        assert!(rows.len() > 6);
        assert_eq!(keys(&rows), keys(&rows_at(&all, "/items/0/nodes")));

        // Impact's dependents: what the cut walk ranked, then the rest of the whole walk; each once.
        let p = |limit| ImpactParams { symbol: Some("Hub.Ping".into()), depth: 2, limit, ..Default::default() };
        let env = ops::impact(&p(7), &s).unwrap().env;
        capped_hints_continue(&env);
        let all = ops::impact(&p(10_000), &s).unwrap().env;
        let (rows, _) = follow(&env, "/items/0/affected");
        let subjects = |rows: &[Value]| sorted(rows.iter().map(subject).collect());
        assert_eq!(subjects(&rows), subjects(&rows_at(&all, "/items/0/affected")));
        assert_eq!(rows.len(), rows_at(&all, "/items/0/affected").len(), "each row once");

        // Search and structural search hits.
        let opts = kc_index::search::SearchOptions {
            pattern: "Ping".into(),
            regex: false,
            case: kc_index::search::Case::Sensitive,
            word: false,
            langs: Vec::new(),
            globs: Vec::new(),
            max_lines: 50_000,
        };
        let sp = |limit| ops::SearchParams { opts: opts.clone(), limit, offset: 0, context: 0, files_only: false };
        let env = ops::search(&sp(8), &s).unwrap().env;
        capped_hints_continue(&env);
        let (rows, _) = follow(&env, "/items");
        assert_eq!(keys(&rows), keys(&rows_at(&ops::search(&sp(10_000), &s).unwrap().env, "/items")));
        let ast = |limit| ops::AstParams {
            opts: kc_index::structural::AstOptions { pattern: "Hub.Ping()".into(), lang: "csharp".into(), max_matches: 50_000, ..Default::default() },
            limit,
            offset: 0,
        };
        let env = ops::grep_ast(&ast(4), &s).unwrap().env;
        let (rows, _) = follow(&env, "/items");
        assert_eq!(rows.len(), 55);
        assert_eq!(keys(&rows), keys(&rows_at(&ops::grep_ast(&ast(10_000), &s).unwrap().env, "/items")));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn invariant_path_cursors_give_every_path_and_every_candidate_end_once() {
        let cache = test_cache();
        let base = cache.dir("cursor-path");
        // Six ways from `Start.Run` to `End.Stop`, and 20 classes with a `Begin` method each calling it.
        let mut files: Vec<(String, String)> = vec![("src/App.csproj".into(), "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n".into())];
        let mids: String = (0..6).map(|i| format!("        M{i}.Hop();\n")).collect();
        files.push(("src/Start.cs".into(), format!("namespace N;\npublic class Start\n{{\n    public void Run()\n    {{\n{mids}    }}\n}}\n")));
        for i in 0..6 {
            files.push((format!("src/M{i}.cs"), format!("namespace N;\npublic static class M{i}\n{{\n    public static void Hop() {{ End.Stop(); }}\n}}\n")));
        }
        files.push(("src/End.cs".into(), "namespace N;\npublic static class End\n{\n    public static void Stop() { }\n}\n".into()));
        for i in 0..20 {
            files.push((format!("src/B{i:02}.cs"), format!("namespace N;\npublic class B{i:02}\n{{\n    public void Begin() {{ End.Stop(); }}\n}}\n")));
        }
        let files: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        let root = repo(&base, "app", &files);
        let s = scope(&root);
        let opts = |paths| crate::walk::PathOptions {
            max_hops: 6,
            max_nodes: 5000,
            max_repos: 4,
            paths,
            one_way: false,
            allow_ambiguous: true,
            edges: Default::default(),
            from_skip: 0,
            to_skip: 0,
        };
        let env = crate::walk::path("Start.Run", "End.Stop", None, &opts(2), &s).unwrap().env;
        let all = crate::walk::path("Start.Run", "End.Stop", None, &opts(usize::MAX), &s).unwrap().env;
        let total = rows_at(&all, "/items/0/paths").len();
        assert!(total >= 6, "{all}");
        let (rows, _) = follow(&env, "/items/0/paths");
        assert_eq!(sorted(rows.iter().map(subject).collect()), sorted(rows_at(&all, "/items/0/paths").iter().map(subject).collect()));
        // Candidate ends: the first 16 `begin` methods searched, the cursor searches from the rest.
        let env = crate::walk::path("Begin", "End.Stop", None, &opts(3), &s).unwrap().env;
        assert_eq!(env["items"][0]["from_candidates_truncated"], json!(true), "{env}");
        let mut names: Vec<String> = rows_at(&env, "/items/0/from_candidates").iter().map(|r| r["qualified"].as_str().unwrap().to_string()).collect();
        let c = cursor_at(&env, "/items/0/from_candidates").unwrap().to_string();
        let next = more(&c).unwrap();
        assert!(next["items"][0]["paths"].as_array().is_some_and(|p| !p.is_empty()), "{next}");
        names.extend(rows_at(&next, "/items/0/from_candidates").iter().map(|r| r["qualified"].as_str().unwrap().to_string()));
        assert!(cursor_at(&next, "/items/0/from_candidates").is_none());
        assert_eq!(sorted(names), (0..20).map(|i| format!("B{i:02}.Begin")).collect::<Vec<_>>());
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A page continues under the evidence policy the first one was asked under: each says it, and the pages together
    /// are that policy's whole list.
    #[test]
    fn a_cursor_continues_under_the_same_evidence_policy() {
        let cache = test_cache();
        let base = cache.dir("cursor-policy");
        let root = fixture(&base);
        let s = scope(&root);
        let policy = "confirmed,-name_unique";
        let walk = |_limit: usize| WalkFilter { evidence: policy.into(), ..Default::default() };
        // Every page after the first, each saying the policy (and incomplete for what it left out, if anything).
        let pages = |env: &Value, ptr: &str| {
            let mut rows = rows_at(env, ptr);
            let mut next = cursor_at(env, ptr).map(str::to_string);
            while let Some(c) = next {
                let page = more(&c).unwrap();
                assert_eq!(page["policy"]["evidence"], json!(policy), "{page}");
                let said = page["incomplete"].as_array().is_some_and(|a| a.iter().any(|i| i["reason"] == "policy"));
                assert_eq!(said, page["policy"]["excluded"].as_u64() > Some(0), "{page}");
                rows.extend(rows_at(&page, "/items"));
                next = page["cursor"].as_str().map(str::to_string);
            }
            rows
        };
        let env = ops::trace("Hub.Fan", "out", 1, 6, &walk(6), &s).unwrap().env;
        let all = ops::trace("Hub.Fan", "out", 1, 10_000, &walk(10_000), &s).unwrap().env;
        assert!(cursor_at(&env, "/items/0/nodes").is_some(), "{env}");
        assert!(env["policy"]["excluded"].as_u64() > Some(0), "{env}");
        let rows = pages(&env, "/items/0/nodes");
        assert_eq!(keys(&rows), keys(&rows_at(&all, "/items/0/nodes")));
        // The default policy's walk differs from no policy only by not saying so.
        assert!(ops::trace("Hub.Fan", "out", 1, 6, &WalkFilter::default(), &s).unwrap().env.get("policy").is_none());

        let p = |limit| ImpactParams { symbol: Some("Hub.Ping".into()), depth: 2, limit, filter: walk(limit), ..Default::default() };
        let env = ops::impact(&p(7), &s).unwrap().env;
        let all = ops::impact(&p(10_000), &s).unwrap().env;
        let rows = pages(&env, "/items/0/affected");
        assert_eq!(sorted(rows.iter().map(subject).collect()), sorted(rows_at(&all, "/items/0/affected").iter().map(subject).collect()));

        let opts = |paths| crate::walk::PathOptions {
            max_hops: 4,
            max_nodes: 5000,
            max_repos: 4,
            paths,
            one_way: false,
            allow_ambiguous: false,
            edges: ops::edge_filter(&[], policy).unwrap(),
            from_skip: 0,
            to_skip: 0,
        };
        let env = crate::walk::path("Hub.Fan", "Hub.Ping", None, &opts(1), &s).unwrap().env;
        assert_eq!(env["policy"]["evidence"], json!(policy));
        pages(&env, "/items/0/paths");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_cursor_from_before_the_index_changed_is_refused() {
        let cache = test_cache();
        let base = cache.dir("cursor-stale");
        let root = fixture(&base);
        let s = scope(&root);
        let env = ops::refs("Ping", &[], 5, &s).unwrap().env;
        let c = env["cursor"].as_str().unwrap().to_string();
        assert!(more(&c).is_ok());
        // An edit moves the index on: the cursor would mix two generations, so it is refused.
        std::fs::write(root.join("src/T0.cs"), "namespace N;\npublic class T0 : IThing\n{\n    public void Go() { Hub.Ping(); Hub.Ping(); }\n}\n").unwrap();
        let err = format!("{:#}", more(&c).unwrap_err());
        assert!(err.starts_with("stale cursor") && err.contains("start again"), "{err}");
        // A changed list under the same generation (a tampered cursor here) is refused too.
        let mut tampered = decode(ops::refs("Ping", &[], 5, &s).unwrap().env["cursor"].as_str().unwrap()).unwrap();
        tampered.h = "0".repeat(16);
        assert!(format!("{:#}", more(&encode(&tampered)).unwrap_err()).starts_with("stale cursor"));
        assert!(more("kc1.nonsense").is_err());
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The MCP server's compact answer keeps the contract at any size: `complete` exactly when nothing is said
    /// incomplete, a cut list's handle resolves to its cursor, and following it gives every row once.
    #[test]
    fn invariant_a_compact_answer_cut_to_fit_continues_to_every_row() {
        let cache = test_cache();
        let base = cache.dir("cursor-compact");
        let root = fixture(&base);
        let s = scope(&root);
        let all = keys(&rows_at(&crate::ops::refs("Hub.Ping", &[], 1000, &s).unwrap().env, "/items"));
        assert!(all.len() >= 55);
        for max in [1_000, 2_000, 4_000, 8_000] {
            let env = crate::ops::refs("Hub.Ping", &[], 25, &s).unwrap().env;
            let mut v: Value = serde_json::from_str(&crate::mcp::cap(env, max)).unwrap();
            assert_eq!(v["complete"].as_bool(), Some(v.get("incomplete").is_none()), "{v}");
            assert!(v.get("_resume").is_none() && v.get("knowcode").is_none(), "{v}");
            let handle = v["cursor"].as_str().expect("a cut list continues").to_string();
            assert!(handle.starts_with("kc~"), "{handle}");
            for i in crate::contract::incomplete_of(&v) {
                assert!(!i.hint.as_deref().unwrap_or("").contains("CLI:"), "{}", i.detail);
            }
            v["cursor"] = Value::from(crate::compact::resolve(&handle).unwrap());
            let (rows, _) = follow(&v, "/items");
            assert_eq!(keys(&rows), all, "max_chars {max}");
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_rename_preview_cut_to_fit_is_refused_and_continues() {
        let cache = test_cache();
        let base = cache.dir("cursor-rename");
        let root = fixture(&base);
        let s = scope(&root);
        let p = crate::refactor::RenameParams { symbol: "Hub.Ping".into(), new_name: "Pong".into(), allow_ambiguous: false };
        let env = crate::refactor::rename(&p, &s).unwrap().env;
        assert_eq!(env["refused"], json!(false), "{env}");
        let sites = rows_at(&env, "/items");
        assert!(sites.len() >= 31);
        // The size cap cuts the sites: the preview can't be applied as it is.
        let capped: Value = crate::mcp::cap_full(env.clone(), 2_000);
        assert!(capped.get("_resume").is_none());
        assert_eq!(capped["refused"], json!(true), "{capped}");
        assert!(capped["reasons"].as_array().unwrap().iter().any(|r| r.as_str().unwrap().starts_with("partial preview")));
        assert_eq!(capped["complete"], json!(false));
        assert!(capped["group"].as_array().is_some_and(|g| !g.is_empty()));
        capped_hints_continue(&capped);
        let (rows, _) = follow(&capped, "/items");
        assert_eq!(keys(&rows), keys(&sites));
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A repo whose pipeline includes a template repo's pipeline: 9 templates nested in turn (each including the next),
    /// 8 deploy jobs, and 2 includes whose rules leave them out. The app repo's root.
    fn deploy_fixture(base: &Path) -> PathBuf {
        let mut files: Vec<(String, String)> = Vec::new();
        let mut pipeline = String::from("include:\n  - local: /build/b0.yml\n  - local: /deploy.yml\n  - local: /tests/nightly.yml\n    rules:\n      - if: $CI_PIPELINE_SOURCE == \"schedule\"\n");
        pipeline.push_str("stages: [build, deploy]\n");
        files.push(("pipeline.yml".into(), pipeline));
        for i in 0..9 {
            let next = if i < 8 { format!("include:\n  - local: /build/b{}.yml\n", i + 1) } else { String::new() };
            files.push((format!("build/b{i}.yml"), format!("{next}build_step_{i}:\n  stage: build\n  script:\n    - dotnet build step{i}\n")));
        }
        let deploys: String = (0..8)
            .map(|i| format!("deploy_region_{i}:\n  stage: deploy\n  script:\n    - helm upgrade --install $CI_PROJECT_NAME-r{i} ./chart --namespace region-{i}\n"))
            .collect();
        files.push(("deploy.yml".into(), deploys));
        files.push(("tests/nightly.yml".into(), "nightly:\n  script:\n    - echo nightly\n".into()));
        let files: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        let add_origin = |root: &Path, url: &str| {
            assert!(std::process::Command::new("git").args(["remote", "add", "origin", url]).current_dir(root).status().unwrap().success());
            let opts = kc_index::SyncOptions { threads: 1, full_discovery: true, ..Default::default() };
            kc_index::sync(root, &mut kc_index::open(root).unwrap(), &opts).unwrap();
        };
        let templates = repo(base, "ci-templates", &files);
        add_origin(&templates, "git@gitlab.example.test:platform/ci-templates.git");
        let app = repo(
            base,
            "shop-api",
            &[
                (
                    ".gitlab-ci.yml",
                    "include:\n  - project: 'platform/ci-templates'\n    ref: main\n    file: '/pipeline.yml'\n  - local: /ci/manual.yml\n    rules:\n      - if: $CI_PIPELINE_SOURCE == \"web\"\n",
                ),
                ("ci/manual.yml", "manual:\n  script:\n    - echo manual\n"),
            ],
        );
        add_origin(&app, "git@gitlab.example.test:shop/shop-api.git");
        app
    }

    /// Every row of a compact answer's list at `ptr` and of the pages its cursors give, each page cut to `max` as the
    /// MCP server cuts it.
    fn follow_compact(v: &Value, ptr: &str, max: usize) -> Vec<Value> {
        let mut rows = rows_at(v, ptr);
        let mut next = cursor_at(v, ptr).map(str::to_string);
        let mut pages = 0;
        while let Some(handle) = next {
            let full = crate::compact::resolve(&handle).unwrap();
            let page: Value = serde_json::from_str(&crate::mcp::cap(more(&full).unwrap_or_else(|e| panic!("{e:#}")), max)).unwrap();
            assert_eq!(page["complete"].as_bool(), Some(page.get("incomplete").is_none()), "{page}");
            rows.extend(rows_at(&page, "/items"));
            next = page["cursor"].as_str().map(str::to_string);
            pages += 1;
            assert!(pages < 200, "cursors never end");
        }
        rows
    }

    /// A repo's deploy answer through the MCP server: said once (no `deploy` copy of the first item), and at any
    /// `max_chars` from the floor up (the smallest at which it fits), every list it cuts has a cursor, and following
    /// them gives exactly the CLI answer's rows, each once. `limit` cuts each list, and its cursors continue it.
    #[test]
    fn invariant_a_deploy_answer_at_any_size_continues_to_exactly_the_cli_answer() {
        let cache = test_cache();
        let base = cache.dir("cursor-deploy");
        let app = deploy_fixture(&base);
        let s = scope(&app);
        let ask = |limit| ops::context_limited("shop-api", 2000, Default::default(), limit, &s).unwrap().env;
        let cli = ask(None);
        let a = &cli["items"][0]["ansible"];
        assert_eq!(cli["deploy"], cli["items"][0], "the CLI keeps its top-level `deploy`");
        assert_eq!(a["deploy_chains"].as_array().map(Vec::len), Some(8), "{a:#}");
        let includes = a["ci_includes"].as_array().unwrap();
        assert!(includes.len() >= 11 && includes.iter().all(|i| i["status"] == "followed"), "{a:#}");
        assert_eq!(a["ci_skipped"], json!(["ci-templates:pipeline.yml:4: local /tests/nightly.yml", ".gitlab-ci.yml:5: local /ci/manual.yml"]), "{a:#}");
        // What the MCP server says of each row: the compact form of the CLI's.
        let names = crate::compact::Names::default();
        let mut sealed = cli.clone();
        seal(&mut sealed);
        let want: Value = serde_json::from_str(&crate::compact::measure(&sealed, &names)).unwrap();
        let lists: Vec<String> = ops::DEPLOY_LISTS.iter().filter(|k| a.get(**k).is_some()).map(|k| format!("/items/0/ansible/{k}")).collect();
        assert!(lists.len() >= 3);
        let fits = |max: usize| crate::mcp::cap(cli.clone(), max).len() <= max;
        let floor = (1_000..=40_000).step_by(250).find(|m| fits(*m)).expect("fits at 40000");
        assert!(floor < crate::mcp::cap(cli.clone(), 40_000).len(), "the floor cuts something: {floor}");
        for max in [1_000, floor, floor + 500, floor * 2, 8_000, 40_000] {
            let v: Value = serde_json::from_str(&crate::mcp::cap(cli.clone(), max)).unwrap();
            assert!(v.get("deploy").is_none(), "said twice at {max}: {v}");
            if max >= floor {
                assert!(v.to_string().len() <= max, "{max}");
            }
            assert_eq!(v["complete"].as_bool(), Some(v.get("incomplete").is_none()), "{v}");
            // The deploy steps go last: a chain is cut only once every tail is down to one row.
            let shown = |k: &str| v["items"][0]["ansible"][k].as_array().map_or(0, Vec::len);
            if shown("deploy_chains") < 8 {
                assert!(shown("ci_includes") <= 1 && shown("ci_skipped") <= 1, "max {max}: {v}");
            }
            for ptr in &lists {
                let rows = follow_compact(&v, ptr, max);
                assert_eq!(rows, rows_at(&want, ptr), "max_chars {max}, {ptr}");
            }
        }
        // `limit`: each list cut to it, its total said, and continued by its cursor to the whole list.
        let limited = ask(Some(2));
        // The lists the answer hasn't stay absent.
        assert!(ops::DEPLOY_LISTS.iter().all(|k| !limited["items"][0]["ansible"].get(*k).is_some_and(Value::is_null)), "{limited:#}");
        for ptr in &lists {
            let all = rows_at(&cli, ptr);
            assert!(rows_at(&limited, ptr).len() <= 2, "{ptr}");
            if all.len() > 2 {
                assert!(cursor_at(&limited, ptr).is_some(), "{ptr}");
                assert_eq!(limited.pointer(&format!("{ptr}_total")).and_then(Value::as_u64), Some(all.len() as u64));
            }
            let (rows, _) = follow(&limited, ptr);
            assert_eq!(rows, all, "{ptr}");
        }
        assert_eq!(limited["complete"], json!(false));
        capped_hints_continue(&limited);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn cursors_round_trip_and_reject_what_they_did_not_make() {
        for s in ["", "a", "ab", "abc", "abcd", "{\"q\":1}\u{e9}"] {
            assert_eq!(unb64(&b64(s.as_bytes())).unwrap(), s.as_bytes());
        }
        let c = Cursor {
            q: Query { req: Request::Refs { name: "go".into(), kinds: Vec::new(), limit: 5 }, repos: vec!["/r".into()], no_sync: false },
            g: vec!["0011aabb".into()],
            l: "/items".into(),
            o: 5,
            b: 0,
            h: "00".into(),
            n: 5,
            k: 5,
        };
        let back = decode(&encode(&c)).unwrap();
        assert_eq!((back.q, back.l, back.o), (c.q, c.l, c.o));
        assert!(decode("kc1.!!").is_err() && decode("abc").is_err());
        assert_eq!(readable("/items/0/callers"), "items[0].callers");
        assert_eq!(cursor_field("/items"), (String::new(), "cursor".to_string()));
        assert_eq!(cursor_field("/items/2/nodes"), ("/items/2".to_string(), "nodes_cursor".to_string()));
    }
}
