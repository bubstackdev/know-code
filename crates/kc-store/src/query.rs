//! Read queries and query-time name resolution.
//!
//! References are stored by name. Resolution happens here, against the definitions in the index, and every
//! resolved edge carries a confidence tag:
//!
//! - `exact`: narrowed to one container through the receiver (`self`, `Type.`) or the caller's own type,
//! - `unique_name`: only one container defines a compatible symbol with that name,
//! - `ambiguous`: several candidates remain; all are listed and none is picked,
//! - `external_likely`: the receiver's type is unknown and the name is a common standard-library member (`map`,
//!   `append`, …); in-repo candidates are listed but are probably not the target,
//! - `unresolved`: no definition in this repo that the call can reach (usually a framework symbol).
//!
//! When the repo has a SCIP index (see [`crate::scip`]), a reference it covers takes the compiler's answer instead
//! and is marked `precise`; one whose heuristic answer the compiler contradicts also says what that was
//! (`corrected_from`).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use anyhow::Result;
use crate::answer::{Assumption, Evidence, Mechanism};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::Serialize;

use kc_extract::{PathRoot, Seg, TypeDecl, TypeExpr};

use crate::split_words;

const TYPE_KINDS: &[&str] = &["class", "struct", "enum", "actor", "protocol"];

#[derive(Debug, Clone, Serialize)]
pub struct SymbolRow {
    pub id: i64,
    pub name: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    pub signature: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub attrs: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub tags: String,
    pub file: String,
    pub line: u32,
    pub end_line: u32,
    #[serde(skip)]
    pub start_byte: usize,
    #[serde(skip)]
    pub end_byte: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub complexity: u32,
    /// The namespace or module it is declared in (see [`crate::identity`]), set where another definition listed with
    /// it has the same qualified name, or a selector named it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

impl SymbolRow {
    /// The dotted name members of this symbol use as their container: `Outer.Inner` for a nested type, the
    /// extended type for an extension, and the container itself for members.
    pub fn self_type(&self) -> Option<String> {
        if is_type_or_extension(&self.kind) {
            Some(join(self.container.as_deref(), &self.name))
        } else {
            self.container.clone()
        }
    }

    /// `Container.name`; a role named for its own container (a repo that is one role, named by the repo) is just its
    /// name, not `Configure.Orders.Configure.Orders`.
    pub fn qualified(&self) -> String {
        if self.kind == "role" && self.container.as_deref() == Some(self.name.as_str()) {
            return self.name.clone();
        }
        join(self.container.as_deref(), &self.name)
    }
}

fn join(container: Option<&str>, name: &str) -> String {
    match container {
        Some(c) => format!("{c}.{name}"),
        None => name.to_string(),
    }
}

pub fn is_type_or_extension(kind: &str) -> bool {
    TYPE_KINDS.contains(&kind) || kind == "extension"
}

pub(crate) const SYMBOL_COLS: &str = "s.id, s.name, s.kind, s.container, s.signature, s.doc, s.attrs, s.tags, f.path, s.start_line,
    s.end_line, s.start_byte, s.end_byte, s.complexity";

pub(crate) fn symbol_row(r: &Row) -> rusqlite::Result<SymbolRow> {
    Ok(SymbolRow {
        id: r.get(0)?,
        name: r.get(1)?,
        kind: r.get(2)?,
        container: r.get(3)?,
        signature: r.get(4)?,
        doc: r.get(5)?,
        attrs: r.get(6)?,
        tags: r.get(7)?,
        file: r.get(8)?,
        line: r.get(9)?,
        end_line: r.get(10)?,
        start_byte: r.get::<_, i64>(11)? as usize,
        end_byte: r.get::<_, i64>(12)? as usize,
        complexity: r.get(13)?,
        namespace: None,
    })
}

/// Lower ranks first: types, then callables, then properties, then the rest.
fn kind_rank(kind: &str) -> u8 {
    match kind {
        "class" | "struct" | "enum" | "actor" | "protocol" => 0,
        "function" | "method" => 1,
        "typealias" => 2,
        "property" => 3,
        "init" => 4,
        "case" => 5,
        "extension" => 6,
        "template" => 1,
        _ => 7,
    }
}

/// Whether a symbol kind passes a kind filter: a kind, `type` (any type) or `func`/`callable`. The words other
/// languages use for Swift's kinds name them too: `interface` and `trait` are `protocol`, `constructor` is `init`.
pub fn kind_matches(filter: Option<&str>, kind: &str) -> bool {
    match filter {
        None => true,
        Some("type") => TYPE_KINDS.contains(&kind),
        Some("func") | Some("callable") => matches!(kind, "function" | "method" | "init"),
        Some("interface") | Some("trait") => kind == "protocol",
        Some("constructor") => kind == "init",
        Some(f) => f == kind,
    }
}

/// Splits `Outer.name` into (`Some("Outer")`, `"name"`).
pub fn split_dotted(query: &str) -> (Option<&str>, &str) {
    match query.rsplit_once('.') {
        Some((c, n)) if !c.is_empty() && !n.is_empty() => (Some(c), n),
        _ => (None, query),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct FindHit {
    #[serde(flatten)]
    pub symbol: SymbolRow,
    /// `exact`, `prefix` or `token`.
    pub matched: &'static str,
}

/// Ranked definitions matching a name, a prefix, or camelCase word prefixes. Returns the hits and the total
/// number of matches before `limit`.
pub fn find(conn: &Connection, query: &str, kind: Option<&str>, limit: usize) -> Result<(Vec<FindHit>, usize)> {
    find_ranked(conn, query, kind, limit, &crate::rank::FileRank::default())
}

/// [`find`], breaking ties (same match, kind and name length) by the rank of the hit's file, highest first.
pub fn find_ranked(conn: &Connection, query: &str, kind: Option<&str>, limit: usize, rank: &crate::rank::FileRank) -> Result<(Vec<FindHit>, usize)> {
    let f = find_counted(conn, query, kind, limit, rank)?;
    Ok((f.hits, f.total))
}

/// Candidates [`find_counted`] reads at most per way of matching (by name, by words), after the container and kind
/// filters: past that, the total is a lower bound.
pub const FIND_CANDIDATES: usize = 2000;

/// What [`find_counted`] found.
#[derive(Debug)]
pub struct Found {
    pub hits: Vec<FindHit>,
    /// Matches before `limit`: all of them, or with `capped`, those among the candidates read.
    pub total: usize,
    /// More than [`FIND_CANDIDATES`] candidates matched one way: `total` is a lower bound and a match may be missing
    /// from `hits`.
    pub capped: bool,
}

/// [`find_ranked`], saying whether the candidate cap made `total` a lower bound. The container (`Outer.name`) and
/// kind filters apply before the cap, so a match is never lost to candidates the filters drop.
pub fn find_counted(conn: &Connection, query: &str, kind: Option<&str>, limit: usize, rank: &crate::rank::FileRank) -> Result<Found> {
    let mut found = find_unqualified(conn, query, kind, limit, rank)?;
    // Qualified by a namespace or module (`B.Worker.Run`): the definitions of the rest it names, exact matches only.
    if found.total == 0 && query.contains('.') {
        let rows = crate::identity::namespaced(conn, query, |rest| {
            let f = find_unqualified(conn, rest, kind, usize::MAX, rank)?;
            Ok(f.hits.into_iter().filter(|h| h.matched == "exact").map(|h| h.symbol).collect())
        })?;
        found = Found { total: rows.len(), hits: rows.into_iter().take(limit).map(|symbol| FindHit { symbol, matched: "exact" }).collect(), capped: false };
    }
    let mut rows: Vec<SymbolRow> = found.hits.iter().map(|h| h.symbol.clone()).collect();
    crate::identity::disambiguate(conn, &mut rows)?;
    for (h, r) in found.hits.iter_mut().zip(rows) {
        h.symbol.namespace = r.namespace;
    }
    Ok(found)
}

fn find_unqualified(conn: &Connection, query: &str, kind: Option<&str>, limit: usize, rank: &crate::rank::FileRank) -> Result<Found> {
    let (container, name) = split_dotted(query.trim());
    if name.is_empty() {
        return Ok(Found { hits: Vec::new(), total: 0, capped: false });
    }
    let mut hits: HashMap<i64, (u8, FindHit)> = HashMap::new();
    let mut capped = false;
    let like = |t: &str| t.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
    // `Outer.name`: in a container named `Outer`, or ending in `.Outer`.
    let in_container = "(?2 IS NULL OR s.container = ?2 OR s.container LIKE ?3 ESCAPE '\\')";
    let suffix = container.map(|c| format!("%.{}", like(c)));
    // The SQL narrows by container ignoring case (`LIKE`); this decides, with case.
    let wanted = |s: &SymbolRow| {
        kind_matches(kind, &s.kind)
            && container.is_none_or(|c| s.container.as_deref().is_some_and(|hc| hc == c || hc.ends_with(&format!(".{c}"))))
    };

    let pattern = format!("{}%", like(name));
    let sql = format!(
        "SELECT {SYMBOL_COLS} FROM symbols s JOIN files f ON f.id = s.file_id
         WHERE s.name LIKE ?1 ESCAPE '\\' AND {in_container} ORDER BY length(s.name), s.name, f.path, s.start_byte"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let mut read = 0;
    for row in stmt.query_map(params![&pattern, container, suffix], symbol_row)? {
        let row = row?;
        if !wanted(&row) {
            continue;
        }
        if read == FIND_CANDIDATES {
            capped = true;
            break;
        }
        read += 1;
        let tier = if row.name == name {
            0
        } else if row.name.eq_ignore_ascii_case(name) {
            1
        } else {
            2
        };
        let matched = if tier < 2 { "exact" } else { "prefix" };
        hits.insert(row.id, (tier, FindHit { symbol: row, matched }));
    }

    let words = split_words(name);
    if !words.is_empty() {
        let fts_query = words.iter().map(|w| format!("\"{w}\"*")).collect::<Vec<_>>().join(" ");
        let sql = format!(
            "SELECT {SYMBOL_COLS} FROM symbols_fts t JOIN symbols s ON s.id = t.rowid JOIN files f ON f.id = s.file_id
             WHERE symbols_fts MATCH ?1 AND {in_container} ORDER BY t.rank, f.path, s.start_byte"
        );
        let mut stmt = conn.prepare_cached(&sql)?;
        let mut read = 0;
        for row in stmt.query_map(params![&fts_query, container, suffix], symbol_row)? {
            let row = row?;
            if !wanted(&row) {
                continue;
            }
            if read == FIND_CANDIDATES {
                capped = true;
                break;
            }
            read += 1;
            hits.entry(row.id).or_insert((3, FindHit { symbol: row, matched: "token" }));
        }
    }

    let mut ranked: Vec<(u8, FindHit)> = hits.into_values().collect();
    ranked.sort_by(|(ta, a), (tb, b)| {
        (ta, kind_rank(&a.symbol.kind), a.symbol.name.len())
            .cmp(&(tb, kind_rank(&b.symbol.kind), b.symbol.name.len()))
            .then_with(|| rank.score(&b.symbol.file).total_cmp(&rank.score(&a.symbol.file)))
            .then_with(|| (&a.symbol.file, a.symbol.line).cmp(&(&b.symbol.file, b.symbol.line)))
    });
    let total = ranked.len();
    Ok(Found { hits: ranked.into_iter().take(limit).map(|(_, h)| h).collect(), total, capped })
}

/// Definitions with exactly this name (case-sensitive), optionally inside a container, best kind first, then live
/// code before tests and before copies (backups, old versions, template variants: see [`crate::path_rank`]).
///
/// One of several same-named definitions is picked by place: `path#name` (a file path, or its end, and the name),
/// `file:line` (the innermost definition spanning that line), or a file's path alone when the file is a page
/// defining one template (a CFML page).
pub fn definitions(conn: &Connection, query: &str) -> Result<Vec<SymbolRow>> {
    if let Some(rows) = located(conn, query.trim())? {
        return Ok(rows);
    }
    let mut rows = definitions_split(conn, query, split_dotted(query.trim()))?;
    // A name with a dot in it (an Ansible playbook `deploy.yml`, a task `Install v1.2`): the whole query.
    if rows.is_empty() && query.contains('.') {
        rows = definitions_split(conn, query, (None, query.trim()))?;
    }
    // Qualified by a namespace or module (`B.Worker.Run`, `src/worker.Worker.run`).
    if rows.is_empty() && query.contains('.') {
        rows = crate::identity::namespaced(conn, query, |rest| definitions_split(conn, rest, split_dotted(rest)))?;
    }
    crate::identity::disambiguate(conn, &mut rows)?;
    Ok(rows)
}

fn definitions_split(conn: &Connection, _query: &str, (container, name): (Option<&str>, &str)) -> Result<Vec<SymbolRow>> {
    let sql = format!(
        "SELECT {SYMBOL_COLS} FROM symbols s JOIN files f ON f.id = s.file_id WHERE s.name = ?1 AND s.name = ?1 COLLATE BINARY
         ORDER BY f.path, s.start_byte"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let mut rows: Vec<SymbolRow> = stmt
        .query_map([name], symbol_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .filter(|s| match container {
            None => true,
            Some(c) => s.container.as_deref().is_some_and(|sc| sc == c || sc.ends_with(&format!(".{c}"))),
        })
        .collect();
    if rows.is_empty() {
        // SQL names match without case (`Orders` finds `orders`); a schema too (`Shop.orders`).
        let sql = format!(
            "SELECT {SYMBOL_COLS} FROM symbols s JOIN files f ON f.id = s.file_id WHERE s.name = ?1 AND f.lang = 'sql'
             ORDER BY f.path, s.start_byte"
        );
        let mut stmt = conn.prepare_cached(&sql)?;
        rows = stmt
            .query_map([name], symbol_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .filter(|s| match container {
                None => true,
                Some(c) => s.container.as_deref().is_some_and(|sc| sc.eq_ignore_ascii_case(c) || sc.to_lowercase().ends_with(&format!(".{}", c.to_lowercase()))),
            })
            .collect();
    }
    // Code before tests and copies, whatever its kind (a test helper sharing the name of the code it tests never comes
    // first), then by kind; an Ansible role's meta symbol after its main tasks.
    let meta = |s: &SymbolRow| s.kind == "role" && s.tags == "meta";
    let key = |s: &SymbolRow| (crate::path_rank(&s.file), kind_rank(&s.kind), meta(s), s.file.clone(), s.line);
    rows.sort_by_cached_key(key);
    Ok(rows)
}

/// The definitions a query naming a place picks (see [`definitions`]); `None` when the query names no place.
fn located(conn: &Connection, query: &str) -> Result<Option<Vec<SymbolRow>>> {
    let in_file = |path: &str, wanted: &str| {
        let wanted = wanted.trim_start_matches("./");
        path == wanted || path.strip_suffix(wanted).is_some_and(|head| head.ends_with('/'))
    };
    // `path/to/page.cfm#name`.
    if let Some((file, name)) = query.rsplit_once('#')
        && !file.is_empty()
        && !name.is_empty()
        && (file.contains('/') || file.contains('.'))
    {
        let rows: Vec<SymbolRow> = definitions(conn, name)?.into_iter().filter(|s| in_file(&s.file, file)).collect();
        return Ok(Some(rows));
    }
    // `file:line`.
    if let Some((file, line)) = query.rsplit_once(':')
        && let Ok(line) = line.parse::<u32>()
        && (file.contains('/') || file.contains('.'))
    {
        let file = file.trim_start_matches("./");
        let sql = format!(
            "SELECT {SYMBOL_COLS} FROM symbols s JOIN files f ON f.id = s.file_id
             WHERE (f.path = ?1 OR f.path LIKE '%/' || ?1) AND s.start_line <= ?2 AND s.end_line >= ?2
             ORDER BY f.path = ?1 DESC, s.end_line - s.start_line, s.start_line DESC, f.path LIMIT 1"
        );
        let rows: Vec<SymbolRow> = conn.prepare_cached(&sql)?.query_map(params![file, line], symbol_row)?.collect::<rusqlite::Result<_>>()?;
        if !rows.is_empty() {
            return Ok(Some(rows));
        }
        return Ok(None);
    }
    // A page's path: the template it defines.
    if query.contains('.') {
        let sql = format!(
            "SELECT {SYMBOL_COLS} FROM symbols s JOIN files f ON f.id = s.file_id
             WHERE f.path = ?1 AND s.kind = 'template' AND s.parent_id IS NULL"
        );
        let rows: Vec<SymbolRow> =
            conn.prepare_cached(&sql)?.query_map([query.trim_start_matches("./")], symbol_row)?.collect::<rusqlite::Result<_>>()?;
        if rows.len() == 1 {
            return Ok(Some(rows));
        }
    }
    Ok(None)
}

/// A minimal definition used for resolution.
#[derive(Debug, Clone)]
struct Def {
    id: i64,
    kind: String,
    container: Option<String>,
    /// `static` or `class` member, reachable through `.member` and `Type.member`.
    is_static: bool,
    file: String,
    /// The container is a type the repo only extends (`extension View`), never defines.
    external_container: bool,
    /// Defined in a script file: a free symbol there is visible only within that file.
    in_script: bool,
    /// Fewest and most arguments a call passes (C#); `None` fewest when unknown, `None` most for `params`.
    min_args: Option<u32>,
    max_args: Option<u32>,
    /// A C# extension method (`this T x`).
    extension: bool,
    /// For a C# extension method, the static class declaring it (`Ext` in `Ext.M(x)`).
    declaring: Option<String>,
    /// The namespace of a C# type, or of a member's type.
    ns: Option<String>,
    /// A C# method's parameter types, as JSON (see `FileFacts::param_types`).
    params: Option<String>,
    /// The defining file's language (`files.lang`).
    lang: String,
}

impl Def {
    /// Whether a call passing `args` arguments can reach this definition. An extension method called as a static
    /// method also passes its receiver.
    fn accepts(&self, args: u32) -> bool {
        let Some(min) = self.min_args else { return true };
        let fits = |n: u32| n >= min && self.max_args.is_none_or(|m| n <= m);
        fits(args) || (self.extension && args > 0 && fits(args - 1))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct FromSymbol {
    pub id: i64,
    pub name: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Target {
    pub id: i64,
    pub qualified: String,
    pub kind: String,
    pub file: String,
    pub line: u32,
    /// Set when the target lives in another repo (a dependency).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RefHit {
    pub name: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receiver: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receiver_type: Option<String>,
    pub file: String,
    pub line: u32,
    /// The 1-based byte column of the name on `line`; 0 when not known. Two references on a line are told apart by it.
    #[serde(skip_serializing_if = "is_zero")]
    pub col: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<FromSymbol>,
    pub confidence: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<Target>,
    /// Set when the reference itself is in another repo (a dependent).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// The receiver's type and its supertypes as this repo knows them, for resolving into dependencies.
    #[serde(skip)]
    pub receiver_chain: Vec<String>,
    /// Every candidate's id; `targets` shows at most [`TARGETS_SHOWN`].
    #[serde(skip)]
    pub target_ids: Vec<i64>,
    /// How many candidates there are, when `targets` lists fewer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub targets_total: Option<usize>,
    /// For a call through a service type the repo's container binds to one implementation: that
    /// implementation's member (see [`crate::di`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via_di: Option<crate::di::Bound>,
    /// For a TypeScript call through a service several Angular providers in scope bind (one per route, say): each
    /// implementation's member, any of which may run; `via_di` is then unset.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub via_di_any: Vec<crate::di::Bound>,
    /// How many arguments a call passes (C#), for telling overloads apart.
    #[serde(skip)]
    pub args: Option<u32>,
    /// The arguments' types as the call site tells them (C#), as JSON: see `FileFacts::arg_types`.
    #[serde(skip)]
    pub arg_types: Option<String>,
    /// A member reference that assigns the member (C#).
    #[serde(skip)]
    pub assigns: bool,
    /// Resolved from a SCIP index (a compiler's answer) rather than by the heuristics.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub precise: bool,
    /// For a precise reference the heuristics got wrong: the confidence they had given it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub corrected_from: Option<&'static str>,
    /// The reference's source line, trimmed, where an answer shows call sites (`context`'s callers).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// A construction through a factory the container injects (see `factory_constructions`), not a `new`.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub factory: bool,
    /// For `context`'s callers of an implementation: the interface (or base type) the call goes through, calling the
    /// member this one implements (or overrides).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub through: Option<String>,
    /// Why the confidence is what it is, when the evidence that would decide it was missing: [`GUESSED_MODULE`],
    /// [`UNPLACED_MODULE`], [`AMBIGUOUS_DEPENDENCY`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub basis: Option<&'static str>,
    /// What the resolution rests on: how the target was found, and what that did not check. `None` for a reference
    /// that resolved to nothing (`unresolved`, `external_likely`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<crate::answer::Evidence>,
    /// How a type reference or creation uses the type, when it isn't a plain use (C#): `type_argument`
    /// (`Get<IOrders>()`, a DI registration's `AddScoped<IFoo, Foo>()`), `typeof`, `nameof`, `constraint` (`where T :
    /// IFoo`), `target_typed` (`new(…)`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<String>,
    /// For a C# creation (`new T(…)`) of a type declaring constructors: the constructors it may call, told apart by
    /// the arguments (see [`constructors_called`]), the type's own id standing for its primary constructor. Empty when
    /// it calls the implicit parameterless one; `None` when not decided (no single type, or none declared).
    #[serde(skip)]
    pub ctors: Option<Vec<i64>>,
    /// The reference's line, quoted (see [`crate::answer::Cite`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cite: Option<crate::answer::Cite>,
}

/// A reference lists at most this many of its candidate targets (`targets_total` counts them all).
pub const TARGETS_SHOWN: usize = 8;

/// [`RefHit::basis`]: the name's module was placed by its path's tail or a directory above the file, not by a config.
pub const GUESSED_MODULE: &str = "module placed by file name: no tsconfig paths or baseUrl maps the import";
/// [`RefHit::basis`]: a config maps the name's module to no indexed file.
pub const UNPLACED_MODULE: &str = "tsconfig maps the import to no indexed file";
/// [`RefHit::basis`]: several dependencies the file imports declare the name, and the import doesn't say which.
pub const AMBIGUOUS_DEPENDENCY: &str = "several imported dependencies declare it";

impl RefHit {
    /// Whether the reference can point at symbol `id`, directly or as the implementation the container binds.
    pub fn may_target(&self, id: i64) -> bool {
        self.target_ids.contains(&id) || self.targets.iter().any(|t| t.id == id && t.repo.is_none()) || self.bound_to(id)
    }

    /// Whether the reference reaches `id` only through a dependency-injection binding (one of several, for
    /// `via_di_any`).
    pub fn bound_to(&self, id: i64) -> bool {
        self.via_di.as_ref().is_some_and(|b| b.target.id == id) || self.via_di_any.iter().any(|b| b.target.id == id)
    }
}

impl RefHit {
    fn caller_type(&self) -> Option<String> {
        let f = self.from.as_ref()?;
        if is_type_or_extension(&f.kind) { Some(join(f.container.as_deref(), &f.name)) } else { f.container.clone() }
    }
}

pub(crate) const REF_COLS: &str =
    "r.name, r.kind, r.receiver, f.path, r.line, fs.id, fs.name, fs.kind, fs.container, r.receiver_type, r.args, r.arg_types, r.assigns, r.col, r.usage";

pub(crate) fn ref_row(r: &Row) -> rusqlite::Result<RefHit> {
    let from_id: Option<i64> = r.get(5)?;
    let from = match from_id {
        Some(id) => Some(FromSymbol { id, name: r.get(6)?, kind: r.get(7)?, container: r.get(8)? }),
        None => None,
    };
    Ok(RefHit {
        name: r.get(0)?,
        kind: r.get(1)?,
        receiver: r.get(2)?,
        receiver_type: r.get(9)?,
        file: r.get(3)?,
        line: r.get(4)?,
        col: r.get(13)?,
        from,
        confidence: "unresolved",
        targets: Vec::new(),
        repo: None,
        receiver_chain: Vec::new(),
        target_ids: Vec::new(),
        targets_total: None,
        via_di: None,
        via_di_any: Vec::new(),
        args: r.get(10)?,
        arg_types: r.get(11)?,
        assigns: r.get(12)?,
        precise: false,
        corrected_from: None,
        code: None,
        factory: false,
        through: None,
        basis: None,
        evidence: None,
        usage: r.get(14)?,
        ctors: None,
        cite: None,
    })
}

fn compatible(ref_kind: &str, def_kind: &str) -> bool {
    match ref_kind {
        "call" => matches!(def_kind, "function" | "method" | "property" | "case" | "init"),
        "instantiate" => matches!(def_kind, "class" | "struct" | "enum" | "actor" | "typealias" | "function"),
        "type_ref" => matches!(def_kind, "class" | "struct" | "enum" | "actor" | "protocol" | "typealias"),
        "member" => matches!(def_kind, "property" | "case" | "method" | "subscript"),
        _ => false,
    }
}

/// Generic parameter names: a receiver of one of these types tells nothing about where its members live.
const GENERIC_NAMES: &[&str] = &["Element", "Value", "Key", "Content", "Wrapped", "Base", "Output", "Input", "Failure", "Success", "Label", "Root"];

/// Library members that return a value of the receiver's type argument (`FormGroup<T>.controls`,
/// `ComponentFixture<T>.componentInstance`).
const GENERIC_PROJECTIONS: &[&str] =
    &["componentInstance", "controls", "data", "getRawValue", "injector", "instance", "value"];

/// Standard-library types whose members are never defined in the repo (unless it extends them, which lookup finds).
const STDLIB_TYPES: &[&str] = &[
    "Array", "Bool", "CGFloat", "Character", "Data", "Date", "Dictionary", "Double", "Float", "Int", "Int64", "Set",
    "String", "Substring", "UInt", "URL", "UUID",
];

/// .NET collection types a sequence (`TypeExpr::Array`) may be, for extension methods declared on them.
const DOTNET_SEQUENCES: &[&str] = &[
    "List", "IList", "ICollection", "IEnumerable", "IReadOnlyList", "IReadOnlyCollection", "IQueryable", "HashSet",
    "ISet",
];

/// LINQ, Entity Framework and `List<T>` methods returning a sequence of the same elements.
const LINQ_SEQUENCE: &[&str] = &[
    "AsEnumerable", "AsNoTracking", "AsQueryable", "Concat", "Distinct", "DistinctBy", "Except", "FindAll", "Include",
    "Intersect", "OrderBy", "OrderByDescending", "Reverse", "Skip", "SkipWhile", "Take", "TakeWhile", "ThenBy",
    "ThenByDescending", "ToArray", "ToArrayAsync", "ToList", "ToListAsync", "Union", "Where",
];

/// LINQ, Entity Framework and `List<T>` methods returning one element.
const LINQ_ELEMENT: &[&str] = &[
    "ElementAt", "ElementAtOrDefault", "Find", "First", "FirstAsync", "FirstOrDefault", "FirstOrDefaultAsync", "Last",
    "LastOrDefault", "MaxBy", "MinBy", "Single", "SingleAsync", "SingleOrDefault", "SingleOrDefaultAsync",
];

/// Evaluates the extractor's type expressions against the index, with caches: member types, call return types,
/// typealiases and supertypes (superclasses, conformed protocols, and protocols an extension adds).
struct TypeOracle<'c> {
    conn: &'c Connection,
    /// A member's container, encoded type and file.
    members: HashMap<MemberKey, Option<(String, String, String)>>,
    /// A function's container, encoded return type and file.
    returns: HashMap<ReturnKey, Option<(Option<String>, String, String)>>,
    /// The TypeScript file being resolved: a free function it calls is its own or an imported one, never another
    /// file's of the same name.
    file: Option<String>,
    supers: HashMap<String, Vec<String>>,
    /// Per name, `None` when no type, extension or typealias has it, else its typealiases (see [`Self::alias`]).
    defined: HashMap<String, Option<Vec<Alias>>>,
    lambdas: HashMap<String, Vec<LambdaSig>>,
    /// C# type declarations by name (see [`TypeDecl`]), merged over partial classes.
    decls: HashMap<String, TypeDecl>,
    /// The repo's C# types by name: (container, namespace) of each.
    type_ns: HashMap<String, Vec<TypeAt>>,
    /// [`Self::seen`]'s answers by type name as written, file it was written in, and the type it was written inside.
    seen_cache: HashMap<(String, Option<String>, Option<String>), Seen>,
    /// Where the type the last evaluation reached was written, for C# namespace visibility.
    origin: Origin,
    /// The C# file being resolved and the caller's type there, and what its code and other files see.
    here: Option<String>,
    caller: Option<String>,
    vis: crate::csharp::Visibility,
    /// Whether the repo declares an enum of this name.
    enums: HashMap<String, bool>,
}

/// A typealias the repo declares: its file, the type it is nested in, and its target, encoded.
struct Alias {
    file: String,
    container: Option<String>,
    target: Option<String>,
}

/// Which of the repo's same-named C# types a written type name means.
#[derive(Debug, Clone, PartialEq)]
enum Seen {
    /// Can't be told, or all of them.
    Any,
    /// Those declared in these namespaces.
    Only(Vec<String>),
    /// None: a library's type of the same name.
    Library,
}

/// Where a type name was written, which decides the namespaces it may come from.
#[derive(Debug, Clone, PartialEq)]
enum Origin {
    /// In the file of the reference being resolved.
    Here,
    /// In a member's or function's declaration in that file, inside that type.
    In(String, Option<String>),
    /// Not known (substituted for a type parameter, a lambda's delegate): no judgement.
    Unknown,
}

/// A member looked up: its container, name, whether it's called, and the namespaces its container is narrowed to.
type MemberKey = (String, String, bool, Option<String>);

/// Where a C# type is declared: its container (for a nested type) and its namespace.
type TypeAt = (Option<String>, Option<String>);

/// A call whose return type is looked up: owner, caller's type, name, and (TypeScript) the calling file.
type ReturnKey = (Option<String>, Option<String>, String, Option<String>);

/// A method taking a delegate: its container, fewest and most arguments, and per parameter the delegate's input types.
struct LambdaSig {
    container: Option<String>,
    min_args: Option<u32>,
    max_args: Option<u32>,
    inputs: Vec<Option<Vec<Option<String>>>>,
}

impl<'c> TypeOracle<'c> {
    fn new(conn: &'c Connection) -> Self {
        TypeOracle {
            conn,
            members: HashMap::new(),
            returns: HashMap::new(),
            file: None,
            supers: HashMap::new(),
            defined: HashMap::new(),
            lambdas: HashMap::new(),
            decls: HashMap::new(),
            type_ns: HashMap::new(),
            seen_cache: HashMap::new(),
            origin: Origin::Here,
            here: None,
            caller: None,
            vis: crate::csharp::Visibility::default(),
            enums: HashMap::new(),
        }
    }

    /// Whether the repo declares an enum named `name` (its last segment).
    fn is_enum(&mut self, name: &str) -> Result<bool> {
        let name = last_segment(name);
        if let Some(&v) = self.enums.get(name) {
            return Ok(v);
        }
        let v: bool = self
            .conn
            .prepare_cached("SELECT EXISTS(SELECT 1 FROM symbols WHERE name = ?1 AND name = ?1 COLLATE BINARY AND kind = 'enum')")?
            .query_row([name], |r| r.get(0))?;
        self.enums.insert(name.to_string(), v);
        Ok(v)
    }

    /// Which of the repo's C# types named `written` (`Policy`, or qualified `Data.Policy`) the code where it was
    /// written (see [`Origin`]) means.
    fn seen(&mut self, written: &str) -> Result<Seen> {
        let Some(here) = self.here.as_deref() else { return Ok(Seen::Any) };
        let (at, within) = match &self.origin {
            Origin::Here => (Some(here), self.caller.as_deref()),
            Origin::In(f, scope) => (Some(f.as_str()), scope.as_deref()),
            Origin::Unknown => (None, self.caller.as_deref()),
        };
        let key = (written.to_string(), at.map(str::to_string), within.map(str::to_string));
        if let Some(v) = self.seen_cache.get(&key) {
            return Ok(v.clone());
        }
        let v = self.seen_uncached(written)?;
        self.seen_cache.insert(key, v.clone());
        Ok(v)
    }

    fn seen_uncached(&mut self, written: &str) -> Result<Seen> {
        let Some(here) = self.here.clone() else { return Ok(Seen::Any) };
        let (qualifier, n) = match written.rsplit_once('.') {
            Some((q, n)) => (Some(q), n),
            None => (None, written),
        };
        let decls = self.type_namespaces(n)?;
        // A nested type of that name is what code inside its container means.
        let nested: Vec<&String> = decls.iter().filter_map(|(c, _)| c.as_ref()).collect();
        let inside = |c: &String, k: &str| k == c || k.strip_prefix(c.as_str()).is_some_and(|r| r.starts_with('.'));
        let within = match &self.origin {
            Origin::In(_, scope) => scope.clone(),
            _ => self.caller.clone(),
        };
        if within.as_deref().is_some_and(|k| nested.iter().any(|c| inside(c, k))) {
            return Ok(Seen::Any);
        }
        let top: Vec<&(Option<String>, Option<String>)> = decls.iter().filter(|(c, _)| c.is_none()).collect();
        if top.is_empty() || top.iter().any(|(_, ns)| ns.is_none()) {
            return Ok(Seen::Any);
        }
        let mut all: Vec<String> = top.iter().filter_map(|(_, ns)| ns.clone()).collect();
        all.sort();
        all.dedup();
        let seen: Vec<String> = match qualifier {
            Some(q) => {
                let seen: Vec<String> = all.iter().filter(|ns| crate::csharp::Visibility::qualifies(Some(ns), None, q)).cloned().collect();
                if seen.is_empty() {
                    // `Inventory.Models.Domain` names a namespace the repo doesn't declare: a package's type. A
                    // single-word qualifier may be an alias.
                    let nested_match = nested.iter().any(|c| crate::csharp::Visibility::qualifies(None, Some(c), q));
                    return Ok(if q.contains('.') && !nested_match { Seen::Library } else { Seen::Any });
                }
                seen
            }
            None => {
                let at = match &self.origin {
                    Origin::Here => here,
                    Origin::In(f, _) => f.clone(),
                    Origin::Unknown => return Ok(Seen::Any),
                };
                let seen = self.vis.seen_among(self.conn, &at, n, &all)?;
                if seen.is_empty() {
                    return Ok(Seen::Library);
                }
                seen
            }
        };
        Ok(if seen.len() < all.len() { Seen::Only(seen) } else { Seen::Any })
    }

    /// Evaluates an encoded type expression in the scope of `scope` (the caller's type) to a concrete type.
    fn eval_encoded(&mut self, encoded: &str, scope: Option<&str>) -> Result<Option<TypeExpr>> {
        match TypeExpr::decode(encoded) {
            Some(t) => self.eval(&t, scope, 0),
            None => Ok(None),
        }
    }

    /// Reduces a type expression to `Named`, `Array` or `Dict`, or `None` when it can't be known syntactically.
    fn eval(&mut self, t: &TypeExpr, scope: Option<&str>, depth: u8) -> Result<Option<TypeExpr>> {
        if depth > 8 {
            return Ok(None);
        }
        Ok(match t {
            TypeExpr::Named(n) if n == "Self" => scope.map(|s| TypeExpr::Named(s.to_string())),
            // An Entity Framework owned type's builder (`Order#Payments`, see the C# extractor): the owned type.
            TypeExpr::Named(n) if n.contains('#') => self.owned_type(n, depth)?,
            // A C# name written qualified (`Data.Policy`) keeps its qualifier, which tells same-named types apart;
            // what reads the type uses its last segment.
            TypeExpr::Named(n) => match self.unalias(n, scope, depth)? {
                TypeExpr::Named(u) if u == last_segment(n) => Some(t.clone()),
                other => Some(other),
            },
            TypeExpr::Array(_) | TypeExpr::Dict(..) | TypeExpr::Delegate(_) | TypeExpr::Tuple(_) => Some(t.clone()),
            TypeExpr::Generic { name, .. } => match self.unalias(name, scope, depth)? {
                TypeExpr::Named(u) if u == last_segment(name) => Some(t.clone()),
                other => Some(other),
            },
            TypeExpr::Call { owner, name } => match self.return_type(owner.as_deref(), scope, name)? {
                Some((container, ret)) => self.eval_encoded_at(&ret, container.as_deref(), depth)?,
                // A property or field holding a delegate, called (`_factory()`): what the delegate returns.
                None if owner.is_none() => match self.property_in_scope(name, scope, depth)? {
                    Some(t) => self.invoked(&t, depth)?,
                    None => None,
                },
                None => None,
            },
            TypeExpr::Elem(inner) => match self.eval(inner, scope, depth + 1)? {
                Some(TypeExpr::Array(e)) => self.eval(&e, scope, depth + 1)?,
                _ => None,
            },
            TypeExpr::Path { root, segs } => {
                let mut cur = match root {
                    PathRoot::SelfType => scope.map(|s| TypeExpr::Named(s.to_string())),
                    PathRoot::Static(n) if n == "Self" => scope.map(|s| TypeExpr::Named(s.to_string())),
                    PathRoot::Static(n) => Some(self.unalias(n, scope, depth)?),
                    PathRoot::Expr(e) => self.eval(e, scope, depth + 1)?,
                    PathRoot::Prop(p) => self.property_in_scope(p, scope, depth)?,
                };
                let mut segs = &segs[..];
                // A capitalised root is a property of the caller's type first (C# names them so: `Context.Orders`,
                // and a member hides a type of the same name), then a type; with no type of its name, a namespace or
                // an alias qualifying the type that follows (`Models.Contracts.Order.Name`).
                if let PathRoot::Static(n) = root
                    && n != "Self"
                    && (self.here.is_some() || !self.is_defined(n)?)
                    && let Some(t) = self.property_in_scope(n, scope, depth)?
                {
                    cur = Some(t);
                } else if let PathRoot::Static(n) = root
                    && n != "Self"
                    && !self.is_defined(n)?
                {
                    let mut qualifier = n.clone();
                    for (i, seg) in segs.iter().enumerate() {
                        let Seg::Member(m) = seg else { break };
                        if !m.starts_with(char::is_uppercase) {
                            break;
                        }
                        if self.is_defined(m)? {
                            cur = Some(TypeExpr::Named(format!("{qualifier}.{m}")));
                            segs = &segs[i + 1..];
                            break;
                        }
                        qualifier = format!("{qualifier}.{m}");
                    }
                }
                for seg in segs {
                    let Some(c) = cur else { break };
                    cur = self.step(&c, seg, depth)?;
                }
                cur
            }
            TypeExpr::LambdaParam { recv, name, args, arg, param } => {
                // The types the method may be declared on: the receiver's and its supertypes, or with no receiver the
                // caller's type, its enclosing types and their supertypes.
                let mut owners = Vec::new();
                match recv {
                    Some(r) => match self.eval(r, scope, depth + 1)? {
                        Some(TypeExpr::Named(n) | TypeExpr::Generic { name: n, .. }) => owners = self.chain(last_segment(&n))?,
                        // A library method on a collection taking a lambda first (`xs.Exists(x => …)`,
                        // `xs.ShouldAllBe(x => …)`) passes it the elements.
                        Some(TypeExpr::Array(e)) if *arg == 0 && *param == 0 => return self.eval(&e, scope, depth + 1),
                        _ => {}
                    },
                    None => {
                        let mut s = scope.map(str::to_string);
                        while let Some(cur) = s {
                            for t in self.chain(&cur)? {
                                if !owners.contains(&t) {
                                    owners.push(t);
                                }
                            }
                            s = cur.rsplit_once('.').map(|(p, _)| p.to_string());
                        }
                    }
                }
                match self.lambda_input(&owners, recv.is_none(), name, *args, *arg as usize, *param as usize)? {
                    Some((container, ty)) => {
                        let t = self.eval_encoded_at(&ty, container.as_deref(), depth)?;
                        self.origin = Origin::Unknown;
                        t
                    }
                    None => None,
                }
            }
        })
    }

    /// The type of the property `name` of the caller's type `scope`, an enclosing type, or one of their supertypes.
    fn property_in_scope(&mut self, name: &str, scope: Option<&str>, depth: u8) -> Result<Option<TypeExpr>> {
        // In C#, the caller's own type is the one declared in the file's namespace, not a same-named type elsewhere.
        let own = match (self.here.clone(), scope) {
            (Some(here), Some(s)) if Some(s) == self.caller.as_deref() && self.type_namespaces(last_segment(s))?.len() > 1 => {
                Some(self.vis.namespaces(self.conn, &here)?)
            }
            _ => None,
        };
        let mut s = scope.map(str::to_string);
        while let Some(cur) = s {
            let only = if Some(cur.as_str()) == scope { own.as_deref().filter(|o| !o.is_empty()) } else { None };
            if let Some((container, ty)) = self.member_in_chain_seen(&cur, name, false, only)? {
                return self.eval_encoded_at(&ty, Some(&container), depth);
            }
            s = cur.rsplit_once('.').map(|(p, _)| p.to_string());
        }
        Ok(None)
    }

    /// The type of parameter `param` of a lambda passed as argument `arg` of `args` to a method `name` declared on the
    /// first of `owners` that declares one (or, when `free`, a local function), with the method's container. `None`
    /// when no overload taking that many arguments has a delegate there, or overloads disagree.
    fn lambda_input(
        &mut self,
        owners: &[String],
        free: bool,
        name: &str,
        args: u32,
        arg: usize,
        param: usize,
    ) -> Result<Option<(Option<String>, String)>> {
        if !self.lambdas.contains_key(name) {
            let mut stmt = self.conn.prepare_cached(
                "SELECT s.container, s.min_args, s.max_args, s.lambda_types FROM symbols s JOIN files f ON f.id = s.file_id
                 WHERE s.name = ?1 AND s.name = ?1 COLLATE BINARY AND s.kind IN ('function', 'method') AND s.lambda_types IS NOT NULL
                 ORDER BY f.path, s.start_byte",
            )?;
            let sigs = stmt
                .query_map([name], |r| {
                    Ok(LambdaSig {
                        container: r.get(0)?,
                        min_args: r.get(1)?,
                        max_args: r.get(2)?,
                        inputs: serde_json::from_str(&r.get::<_, String>(3)?).unwrap_or_default(),
                    })
                })?
                .collect::<rusqlite::Result<_>>()?;
            self.lambdas.insert(name.to_string(), sigs);
        }
        let sigs = &self.lambdas[name];
        let pick = |on: &mut dyn Iterator<Item = &LambdaSig>| -> Option<Option<(Option<String>, String)>> {
            let mut any = false;
            let mut found: Option<(Option<String>, String)> = None;
            for sig in on {
                any = true;
                let fits = sig.min_args.is_none_or(|m| args >= m) && sig.max_args.is_none_or(|m| args <= m);
                let Some(ty) = sig.inputs.get(arg).and_then(|p| p.as_ref()?.get(param)?.clone()).filter(|_| fits) else {
                    continue;
                };
                match &found {
                    Some((_, t)) if *t != ty => return Some(None),
                    Some(_) => {}
                    None => found = Some((sig.container.clone(), ty)),
                }
            }
            any.then_some(found)
        };
        for o in owners {
            let suffix = format!(".{o}");
            let mut on = sigs.iter().filter(|s| s.container.as_deref().is_some_and(|c| c == o || c.ends_with(&suffix)));
            if let Some(found) = pick(&mut on) {
                return Ok(found);
            }
        }
        if free && let Some(found) = pick(&mut sigs.iter().filter(|s| s.container.is_none())) {
            return Ok(found);
        }
        Ok(None)
    }

    /// For a member chain that can't be evaluated (`fixture.debugElement.query`): the type it last reached, when the
    /// next step failed because that type isn't the repo's (`ComponentFixture`), so neither is anything after it.
    fn foreign_prefix(&mut self, encoded: &str, scope: Option<&str>) -> Result<Option<String>> {
        let Some(TypeExpr::Path { root, segs }) = TypeExpr::decode(encoded) else { return Ok(None) };
        let mut cur = self.eval(&TypeExpr::root(root), scope, 0)?;
        for (i, seg) in segs.iter().enumerate() {
            let Some(c) = cur else { return Ok(None) };
            let next = self.step(&c, seg, 0)?;
            // Members that hand out the type argument (`form.controls`, `fixture.componentInstance`): what follows
            // is that type's, which the type's name alone doesn't say; but one of a form's controls is a control.
            if next.is_none()
                && let Seg::Member(m) | Seg::Call(m) = seg
                && GENERIC_PROJECTIONS.contains(&m.as_str())
            {
                return Ok((m == "controls" && i + 2 == segs.len()).then(|| "AbstractControl".to_string()));
            }
            // A library method may return its type argument (`adapter.removeMany(ids, state)` gives the state):
            // only a property's type says the value stays in the library.
            if next.is_none()
                && matches!(seg, Seg::Member(_))
                && let TypeExpr::Named(n) | TypeExpr::Generic { name: n, .. } = &c
                && let n = &last_segment(n).to_string()
                && !GENERIC_NAMES.contains(&n.as_str())
                && n.chars().count() > 1
                && !self.is_defined(n)?
            {
                return Ok(Some(n.clone()));
            }
            cur = next;
        }
        Ok(None)
    }

    fn eval_encoded_at(&mut self, encoded: &str, scope: Option<&str>, depth: u8) -> Result<Option<TypeExpr>> {
        match TypeExpr::decode(encoded) {
            Some(t) => self.eval(&t, scope, depth + 1),
            None => Ok(None),
        }
    }

    fn step(&mut self, cur: &TypeExpr, seg: &Seg, depth: u8) -> Result<Option<TypeExpr>> {
        Ok(match (cur, seg) {
            // A value of a C# delegate type invoked: what the delegate returns.
            (TypeExpr::Named(_), Seg::Call(m)) if m == "Invoke" && self.here.is_some() && self.delegate(cur)?.is_some() => {
                self.invoked(cur, depth)?
            }
            (TypeExpr::Named(n), Seg::Member(m)) if m == "Length" && keyword_type(last_segment(n)) == "string" => {
                Some(TypeExpr::Named("int".to_string()))
            }
            (TypeExpr::Named(_), Seg::Call(m)) if m == "ToString" => Some(TypeExpr::Named("string".to_string())),
            (TypeExpr::Named(t), Seg::Member(m) | Seg::Call(m)) => {
                let callable = matches!(seg, Seg::Call(_));
                let only = match self.seen(t)? {
                    Seen::Library => return Ok(None),
                    Seen::Only(ns) => Some(ns),
                    Seen::Any => None,
                };
                match self.member_in_chain_seen(last_segment(t), m, callable, only.as_deref())? {
                    Some((container, ty)) => self.eval_encoded_at(&ty, Some(&container), depth)?,
                    None => None,
                }
            }
            // Every value's `ToString()` is a string; a string's or an array's `Length` and a sequence's `Count` an int.
            (_, Seg::Call(m)) if m == "ToString" => Some(TypeExpr::Named("string".to_string())),
            (TypeExpr::Array(_), Seg::Member(m)) if matches!(m.as_str(), "Length" | "Count" | "LongLength") => {
                Some(TypeExpr::Named(if m == "LongLength" { "long" } else { "int" }.to_string()))
            }
            (TypeExpr::Array(_), Seg::Call(m)) if matches!(m.as_str(), "Count" | "LongCount") => {
                Some(TypeExpr::Named(if m == "LongCount" { "long" } else { "int" }.to_string()))
            }
            // `_factory.Invoke()`, `lazy.Value`, `options.CurrentValue`, `pair.Amount` on a tuple.
            (TypeExpr::Delegate(r), Seg::Call(m)) if m == "Invoke" => self.eval(r, None, depth + 1)?,
            (TypeExpr::Generic { name, args }, Seg::Member(m) | Seg::Call(m)) => {
                let callable = matches!(seg, Seg::Call(_));
                let name = last_segment(name).to_string();
                match self.member_in_chain(&name, m, callable)? {
                    Some((container, ty)) => {
                        // The type's own parameters stand for its arguments.
                        let params = self.decl(&name)?.params;
                        let map: HashMap<String, TypeExpr> =
                            params.into_iter().zip(args).filter_map(|(p, a)| Some((p, a.clone()?))).collect();
                        match TypeExpr::decode(&ty) {
                            Some(decoded) if !map.is_empty() => {
                                let replaced = substitute(&decoded, &map);
                                if replaced != decoded {
                                    let t = self.eval(&replaced, Some(&container), depth + 1)?;
                                    self.origin = Origin::Unknown;
                                    return Ok(t);
                                }
                                self.eval_encoded_at(&ty, Some(&container), depth)?
                            }
                            _ => self.eval_encoded_at(&ty, Some(&container), depth)?,
                        }
                    }
                    // A library's generic type hands out its one argument through the usual accessors.
                    None => match (args.as_slice(), m.as_str()) {
                        ([Some(a)], "Value" | "CurrentValue" | "Current" | "Instance" | "Message" | "Data" | "Result" | "Model" | "Entity"
                            | "CreateDbContext" | "CreateDbContextAsync") => {
                            self.origin = Origin::Unknown;
                            self.eval(a, None, depth + 1)?
                        }
                        ([Some(a)], "Items" | "Results" | "Records" | "Entities") => {
                            self.origin = Origin::Unknown;
                            Some(TypeExpr::Array(Box::new(a.clone())))
                        }
                        _ => None,
                    },
                }
            }
            (TypeExpr::Tuple(elements), Seg::Member(m)) => match elements.iter().find(|(n, _)| n == m) {
                Some((_, Some(t))) => self.eval(&t.clone(), None, depth + 1)?,
                _ => None,
            },
            (TypeExpr::Array(e), Seg::Index) => Some((**e).clone()),
            (TypeExpr::Dict(_, v), Seg::Index) => Some((**v).clone()),
            (TypeExpr::Dict(_, v), Seg::Call(m)) if m == "get" => Some((**v).clone()),
            (TypeExpr::Dict(k, _), Seg::Member(m)) if m == "keys" => Some(TypeExpr::Array(k.clone())),
            (TypeExpr::Dict(_, v), Seg::Member(m)) if m == "values" => Some(TypeExpr::Array(v.clone())),
            (TypeExpr::Array(e), Seg::Member(m)) if matches!(m.as_str(), "first" | "last") => Some((**e).clone()),
            (TypeExpr::Array(e), Seg::Call(m)) => match m.as_str() {
                "filter" | "sorted" | "reversed" | "shuffled" | "prefix" | "suffix" | "dropFirst" | "dropLast" => {
                    Some(cur.clone())
                }
                "first" | "last" | "min" | "max" | "randomElement" | "popLast" | "removeFirst" | "removeLast" => {
                    Some((**e).clone())
                }
                m if LINQ_SEQUENCE.contains(&m) => Some(cur.clone()),
                m if LINQ_ELEMENT.contains(&m) => Some((**e).clone()),
                _ => None,
            },
            _ => None,
        })
    }

    /// Follows a typealias defined in the repo (see [`Self::alias`]) for a name written in `scope`; other names pass
    /// through.
    fn unalias(&mut self, name: &str, scope: Option<&str>, depth: u8) -> Result<TypeExpr> {
        let last = name.rsplit('.').next().unwrap_or(name);
        if depth < 6
            && let Some(target) = self.alias(name, scope)?
            && let Some(TypeExpr::Named(n)) = TypeExpr::decode(&target)
            && n != last
        {
            return self.unalias(&n, scope, depth + 1);
        }
        Ok(TypeExpr::Named(last.to_string()))
    }

    /// For a name in capitals that no type has (an injection token, `USER_SETTINGS`): the declared type of the
    /// top-level constant of that name, when there's one.
    fn token_type(&mut self, name: &str, scope: Option<&str>) -> Result<Option<String>> {
        if !name.chars().any(|c| c.is_ascii_uppercase())
            || name.chars().any(|c| c.is_ascii_lowercase())
            || self.is_defined(name)?
        {
            return Ok(None);
        }
        let mut stmt = self.conn.prepare_cached(
            "SELECT DISTINCT type_name FROM symbols WHERE name = ?1 AND name = ?1 COLLATE BINARY AND kind = 'property'
             AND container IS NULL AND type_name IS NOT NULL",
        )?;
        let types: Vec<String> = stmt.query_map([name], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        let [t] = types.as_slice() else { return Ok(None) };
        Ok(match self.eval_encoded(t, scope)? {
            Some(TypeExpr::Named(n)) if last_segment(&n) != name => Some(last_segment(&n).to_string()),
            _ => None,
        })
    }

    /// The repo's C# types named `name`: each one's container (for a nested type) and namespace.
    fn type_namespaces(&mut self, name: &str) -> Result<Vec<TypeAt>> {
        if let Some(v) = self.type_ns.get(name) {
            return Ok(v.clone());
        }
        let mut stmt = self.conn.prepare_cached(
            "SELECT DISTINCT s.container, CASE WHEN json_valid(s.type_name) THEN json_extract(s.type_name, '$.ns') END
             FROM symbols s JOIN files f ON f.id = s.file_id
             WHERE s.name = ?1 AND s.name = ?1 COLLATE BINARY AND f.lang = 'csharp'
             AND s.kind IN ('class', 'struct', 'protocol', 'enum', 'typealias')",
        )?;
        let v: Vec<TypeAt> = stmt.query_map([name], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
        self.type_ns.insert(name.to_string(), v.clone());
        Ok(v)
    }

    /// Loads the types named `name` (see [`Self::defined`]).
    fn load_defined(&mut self, name: &str) -> Result<()> {
        if self.defined.contains_key(name) {
            return Ok(());
        }
        // A C# delegate is a type of its own, never an alias of what it returns; a C# `using` alias is spelled out
        // where it's written, when extracted.
        let mut stmt = self.conn.prepare_cached(
            "SELECT s.kind = 'typealias' AND f.lang != 'csharp', f.path, s.container, s.type_name
             FROM symbols s JOIN files f ON f.id = s.file_id
             WHERE s.name = ?1 AND s.name = ?1 COLLATE BINARY
             AND s.kind IN ('class', 'struct', 'enum', 'actor', 'protocol', 'extension', 'typealias')
             ORDER BY f.path, s.start_byte",
        )?;
        let rows: Vec<(bool, Alias)> = stmt
            .query_map([name], |r| Ok((r.get(0)?, Alias { file: r.get(1)?, container: r.get(2)?, target: r.get(3)? })))?
            .collect::<rusqlite::Result<_>>()?;
        let v = (!rows.is_empty()).then(|| rows.into_iter().filter(|(alias, _)| *alias).map(|(_, a)| a).collect());
        self.defined.insert(name.to_string(), v);
        Ok(())
    }

    /// Whether a type, extension or typealias has this name.
    fn is_defined(&mut self, name: &str) -> Result<bool> {
        self.load_defined(name)?;
        Ok(self.defined[name].is_some())
    }

    /// The type `Order#Payments` names: the type of `Order`'s `Payments`, a collection's element.
    fn owned_type(&mut self, spec: &str, depth: u8) -> Result<Option<TypeExpr>> {
        let mut parts = spec.split('#');
        let Some(owner) = parts.next() else { return Ok(None) };
        let mut cur = TypeExpr::Named(owner.to_string());
        for nav in parts {
            let next = match self.step(&cur, &Seg::Member(nav.to_string()), depth)? {
                Some(TypeExpr::Array(e)) => Some(*e),
                other => other,
            };
            cur = match next {
                Some(t @ TypeExpr::Named(_)) => t,
                Some(TypeExpr::Generic { name, args }) => match (last_segment(&name), args.as_slice()) {
                    ("List" | "IList" | "ICollection" | "IEnumerable" | "HashSet" | "ISet" | "IReadOnlyCollection" | "IReadOnlyList"
                    | "Collection", [Some(e)]) => e.clone(),
                    _ => TypeExpr::Generic { name, args },
                },
                _ => return Ok(None),
            };
        }
        Ok(Some(cur))
    }

    /// What invoking a value of type `t` gives: a `Func`'s result, or a C# delegate type's return type.
    fn invoked(&mut self, t: &TypeExpr, depth: u8) -> Result<Option<TypeExpr>> {
        match t {
            TypeExpr::Delegate(ret) => self.eval(ret, None, depth + 1),
            TypeExpr::Named(_) => match self.delegate(t)? {
                Some((container, ret)) => self.eval_encoded_at(&ret, container.as_deref(), depth),
                None => Ok(None),
            },
            _ => Ok(None),
        }
    }

    /// For a C# delegate type written `t` (`Factory`, or qualified `Repo.Factory`): its container and encoded return
    /// type. A delegate nested in a type is that type's: it is meant written qualified by the type, or inside it.
    /// Same-named delegates returning different types are told apart only so; otherwise none is picked. Sets
    /// [`Self::origin`] to the delegate's declaration, where its return type was written.
    fn delegate(&mut self, t: &TypeExpr) -> Result<Option<(Option<String>, String)>> {
        let TypeExpr::Named(written) = t else { return Ok(None) };
        let (qualifier, name) = match written.rsplit_once('.') {
            Some((q, n)) => (Some(q), n),
            None => (None, written.as_str()),
        };
        if self.here.is_none() || !self.is_defined(name)? {
            return Ok(None);
        }
        let mut stmt = self.conn.prepare_cached(
            "SELECT s.container, s.type_name, f.path FROM symbols s JOIN files f ON f.id = s.file_id
             WHERE s.name = ?1 AND s.name = ?1 COLLATE BINARY AND s.kind = 'typealias' AND f.lang = 'csharp'
             AND s.type_name IS NOT NULL ORDER BY f.path, s.start_byte",
        )?;
        let rows: Vec<(Option<String>, String, String)> =
            stmt.query_map([name], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
        if rows.is_empty() {
            return Ok(None);
        }
        let within = match &self.origin {
            Origin::In(_, scope) => scope.clone(),
            _ => self.caller.clone(),
        };
        let names = |a: &str, b: &str| a == b || a.ends_with(&format!(".{b}")) || b.ends_with(&format!(".{a}"));
        let applies = |c: &Option<String>| match (c, qualifier) {
            (Some(c), Some(q)) => names(c, q),
            // Written qualified by a namespace: a top-level delegate.
            (None, Some(_)) => true,
            (None, None) => true,
            (Some(c), None) => std::iter::successors(within.as_deref(), |s| s.rsplit_once('.').map(|(p, _)| p)).any(|s| names(s, c)),
        };
        let found: Vec<&(Option<String>, String, String)> = rows.iter().filter(|(c, _, _)| applies(c)).collect();
        let Some((container, ret, file)) = found.first().copied() else { return Ok(None) };
        if found.iter().any(|(_, r, _)| r != ret) {
            return Ok(None);
        }
        self.origin = Origin::In(file.clone(), container.clone());
        Ok(Some((container.clone(), ret.clone())))
    }

    /// The encoded target of the typealias a type name written in `scope` (the type it was written in) means.
    ///
    /// An alias nested in a type applies inside that type, or written qualified by it. Of those that apply, the one
    /// declared in the file being resolved wins, else the one they all agree on; aliases that disagree alias nothing,
    /// since which one is meant isn't known.
    fn alias(&mut self, written: &str, scope: Option<&str>) -> Result<Option<String>> {
        let (qualifier, name) = match written.rsplit_once('.') {
            Some((q, n)) => (Some(q), n),
            None => (None, written),
        };
        self.load_defined(name)?;
        let Some(all) = &self.defined[name] else { return Ok(None) };
        let names = |a: &str, b: &str| a == b || a.ends_with(&format!(".{b}")) || b.ends_with(&format!(".{a}"));
        let inside = |c: &str| {
            qualifier.is_some_and(|q| names(c, q))
                || std::iter::successors(scope, |s| s.rsplit_once('.').map(|(p, _)| p)).any(|s| names(s, c))
        };
        let applicable: Vec<&Alias> = all.iter().filter(|a| a.container.as_deref().is_none_or(inside)).collect();
        let here = self.file.as_deref().or(self.here.as_deref());
        if let Some(a) = applicable.iter().find(|a| Some(a.file.as_str()) == here) {
            return Ok(a.target.clone());
        }
        Ok(match applicable.as_slice() {
            [first, rest @ ..] if rest.iter().all(|a| a.target == first.target) => first.target.clone(),
            _ => None,
        })
    }

    /// A property (or, with `callable`, a method) named `name` declared directly on `container`: its container and
    /// encoded type. An enum case counts as a member whose type is the enum.
    fn member(&mut self, container: &str, name: &str, callable: bool) -> Result<Option<(String, String, String)>> {
        self.member_seen(container, name, callable, None)
    }

    /// [`Self::member`] on the types named `container` declared in the namespaces `only` (all, for `None`); members
    /// other types add (C# extension methods) count for each.
    fn member_seen(&mut self, container: &str, name: &str, callable: bool, only: Option<&[String]>) -> Result<Option<(String, String, String)>> {
        let key = (container.to_string(), name.to_string(), callable, only.map(|o| o.join("|")));
        if let Some(v) = self.members.get(&key) {
            return Ok(v.clone());
        }
        let kinds = if callable { "('function', 'method')" } else { "('property')" };
        // The parent's namespace only matters when narrowing.
        let (parent, ns, limit) = match only {
            Some(_) => ("p.name", "CASE WHEN json_valid(p.type_name) THEN json_extract(p.type_name, '$.ns') END", ""),
            None => ("NULL", "NULL", "LIMIT 1"),
        };
        let sql = format!(
            "SELECT s.container, s.type_name, s.kind, f.path, {parent}, {ns}
             FROM symbols s JOIN files f ON f.id = s.file_id LEFT JOIN symbols p ON p.id = s.parent_id
             WHERE s.name = ?2 AND s.name = ?2 COLLATE BINARY
             AND (s.container = ?1 OR s.container LIKE '%.' || ?1) AND (s.kind IN {kinds} OR s.kind = 'case')
             ORDER BY s.type_name IS NULL, f.path, s.start_byte {limit}"
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        type Row = (String, Option<String>, String, String, Option<String>, Option<String>);
        let rows: Vec<Row> = stmt
            .query_map(params![container, name], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let v = rows
            .into_iter()
            .find(|(c, _, _, _, parent, ns)| {
                let own = parent.as_deref() == Some(last_segment(c));
                !own || only.is_none_or(|o| ns.as_ref().is_some_and(|n| o.contains(n)))
            })
            .and_then(|(c, t, kind, file, _, _)| {
                if kind == "case" {
                    let ty = c.rsplit('.').next().unwrap_or(&c).to_string();
                    Some((c, ty, file))
                } else {
                    t.map(|t| (c, t, file))
                }
            });
        self.members.insert(key, v.clone());
        Ok(v)
    }

    /// A member of `ty` or a supertype: its container and encoded type, with a generic base's type parameters
    /// replaced by the arguments `ty`'s chain gives them (`Context` declared `TContext` on `BaseRepo<TContext>`, read
    /// from a `Repo : BaseRepo<ShopContext>`, is a `ShopContext`).
    fn member_in_chain(&mut self, ty: &str, name: &str, callable: bool) -> Result<Option<(String, String)>> {
        self.member_in_chain_seen(ty, name, callable, None)
    }

    /// [`Self::member_in_chain`] with `ty` itself narrowed to the types declared in the namespaces `only`.
    fn member_in_chain_seen(&mut self, ty: &str, name: &str, callable: bool, only: Option<&[String]>) -> Result<Option<(String, String)>> {
        for t in self.chain(ty)? {
            let found = if t == ty { self.member_seen(&t, name, callable, only)? } else { self.member(&t, name, callable)? };
            if let Some((container, enc, file)) = found {
                self.origin = Origin::In(file, Some(container.clone()));
                if t != ty
                    && let Some(decoded) = TypeExpr::decode(&enc)
                {
                    let map = self.generic_map(ty, &t)?;
                    if !map.is_empty() {
                        let replaced = substitute(&decoded, &map);
                        if replaced != decoded {
                            self.origin = Origin::Unknown;
                            return Ok(Some((container, replaced.encode())));
                        }
                    }
                }
                return Ok(Some((container, enc)));
            }
        }
        Ok(None)
    }

    /// A C# type's declaration info (namespace, type parameters, generic bases), merged over its partial parts.
    fn decl(&mut self, name: &str) -> Result<TypeDecl> {
        if let Some(d) = self.decls.get(name) {
            return Ok(d.clone());
        }
        let mut stmt = self.conn.prepare_cached(
            "SELECT s.type_name FROM symbols s JOIN files f ON f.id = s.file_id WHERE s.name = ?1 AND s.name = ?1 COLLATE BINARY
             AND s.kind IN ('class', 'struct', 'protocol', 'enum') AND s.type_name IS NOT NULL ORDER BY f.path, s.start_byte",
        )?;
        let rows: Vec<String> = stmt.query_map([name], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        let mut d = TypeDecl::default();
        for r in rows {
            let part = TypeDecl::decode(Some(&r));
            if d.params.is_empty() {
                d.params = part.params;
            }
            for b in part.bases {
                if !d.bases.iter().any(|x| x.0 == b.0) {
                    d.bases.push(b);
                }
            }
        }
        self.decls.insert(name.to_string(), d.clone());
        Ok(d)
    }

    /// What `base`'s type parameters stand for, seen from `from` through the generic bases on the way.
    fn generic_map(&mut self, from: &str, base: &str) -> Result<HashMap<String, TypeExpr>> {
        let mut queue: Vec<(String, HashMap<String, TypeExpr>)> = vec![(from.to_string(), HashMap::new())];
        let mut seen = vec![from.to_string()];
        let mut i = 0;
        while i < queue.len() && i < 16 {
            let (cur, map) = queue[i].clone();
            i += 1;
            for (b, args) in self.decl(&cur)?.bases {
                let params = self.decl(&b)?.params;
                let mut next = HashMap::new();
                for (p, a) in params.iter().zip(&args) {
                    if let Some(t) = a.as_deref().and_then(TypeExpr::decode) {
                        next.insert(p.clone(), substitute(&t, &map));
                    }
                }
                if b == base {
                    return Ok(next);
                }
                if !seen.contains(&b) {
                    seen.push(b.clone());
                    queue.push((b, next));
                }
            }
        }
        Ok(HashMap::new())
    }

    /// The declared return type of a function, as (its container, encoded type).
    fn return_type(&mut self, owner: Option<&str>, scope: Option<&str>, name: &str) -> Result<Option<(Option<String>, String)>> {
        let file = if owner.is_none() { self.file.clone() } else { None };
        let key = (owner.map(str::to_string), scope.map(str::to_string), name.to_string(), file.clone());
        if let Some(v) = self.returns.get(&key) {
            let v = v.clone();
            return Ok(v.map(|(c, t, f)| {
                self.origin = Origin::In(f, c.clone());
                (c, t)
            }));
        }
        let mut stmt = self.conn.prepare_cached(
            "SELECT s.container, s.type_name, f.path FROM symbols s JOIN files f ON f.id = s.file_id
             WHERE s.name = ?1 AND s.name = ?1 COLLATE BINARY AND s.kind IN ('function', 'method') AND s.type_name IS NOT NULL
             ORDER BY f.path, s.start_byte",
        )?;
        let rows: Vec<(Option<String>, String, String)> =
            stmt.query_map([name], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
        let own = file.as_ref().and_then(|f| rows.iter().find(|(c, _, p)| c.is_none() && p == f)).cloned();
        let defs = rows;
        let in_container = |c: &Option<String>, want: &str| c.as_deref().is_some_and(|c| c == want || c.ends_with(&format!(".{want}")));
        let picked = match owner {
            Some(o) => {
                let mut found = None;
                for t in self.chain(o)? {
                    let on: Vec<&(Option<String>, String, String)> = defs.iter().filter(|(c, _, _)| in_container(c, &t)).collect();
                    if let Some(d) = on.first() {
                        // C# overloads returning different types: which one is called isn't known.
                        if d.2.ends_with(".cs") && on.iter().any(|x| x.1 != d.1) {
                            break;
                        }
                        found = Some((*d).clone());
                        break;
                    }
                }
                found
            }
            None => {
                let mut found = None;
                let mut s = scope.map(str::to_string);
                while let Some(cur) = s {
                    if let Some(d) = defs.iter().find(|(c, _, _)| c.as_deref() == Some(cur.as_str())) {
                        found = Some(d.clone());
                        break;
                    }
                    s = cur.rsplit_once('.').map(|(p, _)| p.to_string());
                }
                let free = || {
                    if file.is_some() {
                        // Another file's function is one only if it's the only kind there is.
                        let mut types: Vec<&String> = defs.iter().filter(|(c, _, _)| c.is_none()).map(|(_, t, _)| t).collect();
                        types.sort();
                        types.dedup();
                        return (types.len() == 1).then(|| defs.iter().find(|(c, _, _)| c.is_none()).cloned()).flatten();
                    }
                    defs.iter().find(|(c, _, _)| c.is_none()).cloned()
                };
                found.or(own).or_else(free).or_else(|| {
                    let mut types: Vec<&String> = defs.iter().map(|(_, t, _)| t).collect();
                    types.dedup();
                    (types.len() == 1).then(|| defs[0].clone())
                })
            }
        };
        self.returns.insert(key, picked.clone());
        Ok(picked.map(|(c, t, f)| {
            self.origin = Origin::In(f, c.clone());
            (c, t)
        }))
    }

    /// `ty` followed by its supertypes, breadth first, up to a small depth.
    fn chain(&mut self, ty: &str) -> Result<Vec<String>> {
        let mut out = vec![ty.to_string()];
        let mut i = 0;
        while i < out.len() && out.len() < 16 {
            let cur = out[i].clone();
            for s in self.direct_supers(&cur)? {
                if !out.contains(&s) {
                    out.push(s);
                }
            }
            i += 1;
        }
        Ok(out)
    }

    fn direct_supers(&mut self, ty: &str) -> Result<Vec<String>> {
        if let Some(v) = self.supers.get(ty) {
            return Ok(v.clone());
        }
        let (container, name) = match ty.rsplit_once('.') {
            Some((c, n)) => (Some(c), n),
            None => (None, ty),
        };
        // In declaration order: by file, then as written there (a file's rows are stored in the order extracted).
        let mut stmt = self.conn.prepare_cached(
            "SELECT r.target_name FROM relations r JOIN symbols s ON s.id = r.from_id JOIN files f ON f.id = r.file_id
             WHERE s.name = ?1 AND s.name = ?1 COLLATE BINARY AND IFNULL(s.container, '') = IFNULL(?2, '')
             ORDER BY f.path, r.line, r.rowid",
        )?;
        let mut v: Vec<String> = Vec::new();
        for t in stmt.query_map(params![name, container], |r| r.get::<_, String>(0))? {
            let t = t?;
            if !v.contains(&t) {
                v.push(t);
            }
        }
        self.supers.insert(ty.to_string(), v.clone());
        Ok(v)
    }
}

/// `Policy` in `Data.Policy`.
fn last_segment(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

/// `t` with the type parameters `map` names replaced by what they stand for.
fn substitute(t: &TypeExpr, map: &HashMap<String, TypeExpr>) -> TypeExpr {
    let sub = |e: &TypeExpr| Box::new(substitute(e, map));
    match t {
        TypeExpr::Named(n) => map.get(n).cloned().unwrap_or_else(|| t.clone()),
        TypeExpr::Array(e) => TypeExpr::Array(sub(e)),
        TypeExpr::Dict(k, v) => TypeExpr::Dict(sub(k), sub(v)),
        TypeExpr::Elem(e) => TypeExpr::Elem(sub(e)),
        TypeExpr::Delegate(e) => TypeExpr::Delegate(sub(e)),
        TypeExpr::Generic { name, args } => TypeExpr::Generic {
            name: map.get(name).and_then(|t| if let TypeExpr::Named(n) = t { Some(n.clone()) } else { None }).unwrap_or_else(|| name.clone()),
            args: args.iter().map(|a| a.as_ref().map(|a| substitute(a, map))).collect(),
        },
        TypeExpr::Tuple(es) => TypeExpr::Tuple(es.iter().map(|(n, e)| (n.clone(), e.as_ref().map(|e| substitute(e, map)))).collect()),
        _ => t.clone(),
    }
}

/// What resolution knows about a receiver's type.
enum Receiver {
    /// Not known syntactically.
    Unknown,
    /// Known, and the type is defined in the repo or is a standard-library type: members not found on it (or its
    /// supertypes) aren't in the repo.
    Known(Vec<String>),
    /// Known by name only (a framework type such as `Text`): its own members aren't in the repo, but members the repo
    /// adds to framework protocols (`extension View`) may apply, since the type may conform to them.
    Foreign(Vec<String>),
}

fn receiver(hit: &RefHit, oracle: &mut TypeOracle) -> Result<Receiver> {
    let recv = typed_receiver(hit, oracle)?;
    if matches!(recv, Receiver::Unknown)
        && let Some(root) = external_chain_root(hit, oracle)?
    {
        return Ok(Receiver::Foreign(vec![root]));
    }
    Ok(recv)
}

/// In C#, a call chain off a name the repo doesn't define at all (`Policy.Handle<E>().RetryAsync(3).ExecuteAsync(…)`,
/// `System.IO.File.Open(…)`): the chain is a library's, so its members aren't the repo's same-named ones. Returns
/// that root name.
fn external_chain_root(hit: &RefHit, oracle: &mut TypeOracle) -> Result<Option<String>> {
    let Some(r) = hit.receiver.as_deref() else { return Ok(None) };
    if !hit.file.ends_with(".cs") || !r.contains('(') {
        return Ok(None);
    }
    let root: String = r.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
    if !root.starts_with(char::is_uppercase) {
        return Ok(None);
    }
    let mut stmt = oracle.conn.prepare_cached("SELECT EXISTS(SELECT 1 FROM symbols WHERE name = ?1)")?;
    let defined: bool = stmt.query_row([&root], |row| row.get(0))?;
    Ok((!defined).then_some(root))
}

fn typed_receiver(hit: &RefHit, oracle: &mut TypeOracle) -> Result<Receiver> {
    let Some(enc) = &hit.receiver_type else { return Ok(Receiver::Unknown) };
    let ty = oracle.eval_encoded(enc, hit.caller_type().as_deref())?;
    receiver_from(hit, ty, oracle)
}

/// What resolution knows about a receiver of the evaluated type `ty`.
fn receiver_from(hit: &RefHit, ty: Option<TypeExpr>, oracle: &mut TypeOracle) -> Result<Receiver> {
    let caller = hit.caller_type();
    let ty = match ty {
        Some(TypeExpr::Named(n)) => last_segment(&n).to_string(),
        Some(TypeExpr::Array(_)) => "Array".to_string(),
        Some(TypeExpr::Dict(..)) => "Dictionary".to_string(),
        // A tuple's elements and a delegate's own members are never the repo's.
        Some(TypeExpr::Tuple(_)) => return Ok(Receiver::Foreign(vec!["ValueTuple".to_string()])),
        Some(TypeExpr::Delegate(_)) => return Ok(Receiver::Foreign(vec!["Func".to_string()])),
        Some(TypeExpr::Generic { name, .. }) => last_segment(&name).to_string(),
        _ => return Ok(Receiver::Unknown),
    };
    if GENERIC_NAMES.contains(&ty.as_str()) || ty.chars().count() == 1 {
        return Ok(Receiver::Unknown);
    }
    // An injection token (`inject(USER_SETTINGS)`): the type it was declared for.
    let ty = match oracle.token_type(&ty, caller.as_deref())? {
        Some(t) => t,
        None => ty,
    };
    let mut chain = oracle.chain(&ty)?;
    // A C# sequence also reaches extension methods the repo declares on the collection interfaces.
    if ty == "Array" && hit.file.ends_with(".cs") {
        chain.extend(DOTNET_SEQUENCES.iter().map(|s| s.to_string()));
    }
    // A C# enum is a `System.Enum`, which extension methods often extend (`this Enum e`).
    if hit.file.ends_with(".cs") && !chain.iter().any(|t| t == "Enum") && oracle.is_enum(&ty)? {
        chain.push("Enum".to_string());
    }
    // Every C# value is an `object`, which extension methods may extend (`this object o`).
    if hit.file.ends_with(".cs") && !chain.iter().any(|t| t == "object") {
        chain.push("object".to_string());
    }
    if STDLIB_TYPES.contains(&ty.as_str()) || oracle.is_defined(&ty)? {
        Ok(Receiver::Known(chain))
    } else {
        Ok(Receiver::Foreign(chain))
    }
}

/// .NET types used through their static members (`DateTime.UtcNow`), never a property a class inherits.
const DOTNET_STATICS: &[&str] = &[
    "Activator", "Array", "BitConverter", "Console", "Convert", "CultureInfo", "DateOnly", "DateTime", "DateTimeOffset",
    "Decimal", "Directory", "Encoding", "Enum", "Enumerable", "Environment", "File", "Guid", "HttpMethod",
    "HttpStatusCode", "Interlocked", "JsonConvert", "JsonSerializer", "Math", "Path", "Random", "Regex", "Stopwatch",
    "StringComparer", "StringComparison", "Task", "Thread", "TimeOnly", "TimeSpan", "TimeZoneInfo", "Uri",
];

/// A C# receiver, with the namespaces its type may be declared in when the repo declares that type in several and this
/// code sees only some (`None`: no narrowing; none at all: a library's type of the same name).
///
/// - `base.M()` reaches the caller's supertypes only.
/// - A bare name (`Context.Domains`, `DateTime.UtcNow`, `Ext.M(x)`) is a property of the caller's type or a supertype
///   first, then a type: the repo's, or else a library's.
/// - A type the repo declares only in namespaces the code where the type is written can't see is a library's of the
///   same name.
/// - A member chain stepping through a library's type stays in the library.
fn cs_receiver(hit: &RefHit, oracle: &mut TypeOracle) -> Result<(Receiver, Option<Vec<String>>)> {
    let caller = hit.caller_type();
    let r = hit.receiver.as_deref();
    if r == Some("base") {
        let supers = match &caller {
            Some(c) => oracle.chain(c)?.into_iter().skip(1).collect(),
            None => Vec::new(),
        };
        return Ok((Receiver::Known(supers), None));
    }
    oracle.origin = Origin::Here;
    let mut ty = None;
    if let Some(r) = r
        && let Some(name) = bare_name(r)
        && hit.receiver_type.as_deref().is_none_or(|rt| rt == name || rt == r)
    {
        let generic = r.contains('<');
        if !generic {
            ty = oracle.property_in_scope(name, caller.as_deref(), 0)?;
        }
        if ty.is_none() {
            if !oracle.is_defined(name)? && !STDLIB_TYPES.contains(&name) {
                // Not the repo's type: a library's, unless the name may be a property inherited from a library type
                // (`AppSettings` on a library's generic base class), which only a well-known .NET type's name is not.
                let inherited = match &caller {
                    Some(c) => {
                        let mut any = false;
                        for t in oracle.chain(c)?.into_iter().skip(1) {
                            any |= !oracle.is_defined(&t)?;
                        }
                        any
                    }
                    None => false,
                };
                // A call (`Helpers.Run()`) was always read as one on a type.
                if generic || !inherited || DOTNET_STATICS.contains(&name) || hit.kind == "call" {
                    return Ok((Receiver::Foreign(vec![name.to_string()]), None));
                }
                return Ok((Receiver::Unknown, None));
            }
            oracle.origin = Origin::Here;
            ty = Some(TypeExpr::Named(name.to_string()));
        }
    } else if let Some(enc) = &hit.receiver_type {
        ty = oracle.eval_encoded(enc, caller.as_deref())?;
    }
    let origin = oracle.origin.clone();
    let named = match &ty {
        Some(TypeExpr::Named(n) | TypeExpr::Generic { name: n, .. }) => Some(n.clone()),
        _ => None,
    };
    let recv = receiver_from(hit, ty, oracle)?;
    if matches!(recv, Receiver::Unknown) {
        if let Some(root) = external_chain_root(hit, oracle)? {
            return Ok((Receiver::Foreign(vec![root]), None));
        }
        if let Some(enc) = &hit.receiver_type
            && let Some(t) = oracle.foreign_prefix(enc, caller.as_deref())?
        {
            return Ok((Receiver::Foreign(vec![t]), None));
        }
        return Ok((recv, None));
    }
    // Which of the repo's types of that name the code where it was written means.
    let (Some(written), Receiver::Known(_)) = (named, &recv) else { return Ok((recv, None)) };
    oracle.origin = origin;
    Ok(match oracle.seen(&written)? {
        Seen::Any => (recv, None),
        Seen::Only(ns) => (recv, Some(ns)),
        // A library's type: never the same-named repo type's members (extension methods still apply).
        Seen::Library => (Receiver::Foreign(vec![last_segment(&written).to_string()]), Some(Vec::new())),
    })
}

/// `Name` for a receiver written `Name` or `Name<T, …>` with `Name` capitalised: a property or a type.
fn bare_name(receiver: &str) -> Option<&str> {
    let head = match receiver.split_once('<') {
        Some((head, rest)) if rest.ends_with('>') => head,
        Some(_) => return None,
        None => receiver,
    };
    (head.starts_with(char::is_uppercase) && head.chars().all(|c| c.is_alphanumeric() || c == '_')).then_some(head)
}

/// Standard-library and Foundation members that in-repo types often also define. A call to one of these on a
/// receiver of unknown type is far more likely to be the library's.
const STDLIB_MEMBERS: &[&str] = &[
    "append", "allSatisfy", "compactMap", "contains", "count", "decode", "dropFirst", "dropLast", "encode",
    "enumerated", "filter", "first", "firstIndex", "flatMap", "forEach", "formUnion", "index", "insert",
    "intersection", "isEmpty", "joined", "last", "lastIndex", "lowercased", "map", "mapValues", "max", "merge",
    "min", "prefix", "reduce", "remove", "removeAll", "removeFirst", "removeLast", "removeValue", "replacingOccurrences",
    "reserveCapacity", "reversed", "sort", "sorted", "split", "starts", "subtract", "suffix", "trimmingCharacters",
    "union", "updateValue", "uppercased", "write", "zip",
];

/// .NET collection, string, LINQ and object members, for calls from C#.
const DOTNET_MEMBERS: &[&str] = &[
    "Add", "AddRange", "Any", "All", "Clear", "Concat", "Contains", "ContainsKey", "Count", "Dispose", "Distinct",
    "EndsWith", "Equals", "First", "FirstOrDefault", "ForEach", "Format", "GetHashCode", "GetType", "GetValueOrDefault",
    "IndexOf", "Insert", "Join", "Last", "LastOrDefault", "Max", "Min", "OrderBy", "OrderByDescending", "Remove",
    "RemoveAll", "RemoveAt", "Replace", "Select", "SelectMany", "Single", "SingleOrDefault", "Skip", "Split",
    "StartsWith", "Substring", "Sum", "Take", "ToArray", "ToDictionary", "ToHashSet", "ToList", "ToLower",
    "ToLowerInvariant", "ToString", "ToUpper", "ToUpperInvariant", "Trim", "TryAdd", "TryGetValue", "Where",
];

/// JavaScript array, string, Map/Set, Promise and RxJS members, for calls from TypeScript.
const JS_MEMBERS: &[&str] = &[
    "add", "catch", "clear", "complete", "concat", "delete", "emit", "endsWith", "entries", "every", "filter", "find",
    "findIndex", "flatMap", "forEach", "get", "has", "includes", "indexOf", "join", "keys", "length", "map", "next", "pipe", "pop",
    "push", "reduce", "replace", "set", "shift", "slice", "some", "sort", "splice", "split", "startsWith", "subscribe",
    "then", "toLowerCase", "toString", "toUpperCase", "trim", "unshift", "unsubscribe", "values",
];

/// Whether `name` is a common standard-library member in the language of `file`.
fn stdlib_member(file: &str, name: &str) -> bool {
    match file.rsplit_once('.').map(|(_, e)| e) {
        Some("cs") => DOTNET_MEMBERS.contains(&name),
        Some("ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs") => JS_MEMBERS.contains(&name),
        _ => STDLIB_MEMBERS.contains(&name),
    }
}

/// `unique_name` when the candidates share one container, `ambiguous` for several, `unresolved` for none.
fn group_confidence(cands: &[&Def]) -> (&'static str, Vec<i64>) {
    let mut groups: Vec<Option<&str>> = cands.iter().map(|d| d.container.as_deref()).collect();
    groups.sort();
    groups.dedup();
    let ids = cands.iter().map(|d| d.id).collect();
    match groups.len() {
        0 => ("unresolved", Vec::new()),
        1 => ("unique_name", ids),
        _ => ("ambiguous", ids),
    }
}

/// The module a file most likely belongs to: `Sources/<Name>` or `Tests/<Name>` for SwiftPM layouts, else its
/// directory.
fn module_of(path: &str) -> &str {
    for root in ["Sources/", "Tests/"] {
        if let Some(i) = path.find(root) {
            let rest = &path[i + root.len()..];
            if let Some(end) = rest.find('/') {
                return &path[..i + root.len() + end];
            }
        }
    }
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

/// `Type`, `Module.Type` or `Type.shared`: a receiver that names a type rather than an expression.
fn type_like(receiver: &str) -> bool {
    receiver.starts_with(char::is_uppercase) && receiver.chars().all(|c| c.is_alphanumeric() || c == '.' || c == '_')
}

/// Picks the definitions a reference can point to and tags the result with a confidence.
///
/// `recv_chain` is the receiver's type followed by its supertypes; `self_chain` is the caller's type, its enclosing
/// types, then its supertypes.
///
/// A call's overloads are first narrowed to those taking as many arguments as it passes; when that leaves nothing to
/// point to, all of them are considered.
fn resolve(hit: &RefHit, recv: &Receiver, self_chain: &[String], defs: &[&Def]) -> (&'static str, Vec<i64>) {
    let all: Vec<&Def> = defs.to_vec();
    if hit.kind == "call"
        && let Some(n) = hit.args
    {
        let fitting: Vec<&Def> = defs.iter().copied().filter(|d| d.accepts(n)).collect();
        if fitting.len() < all.len() {
            let (confidence, ids) = resolve_among(hit, recv, self_chain, &fitting);
            if !ids.is_empty() {
                return (confidence, ids);
            }
        }
    }
    resolve_among(hit, recv, self_chain, &all)
}

fn resolve_among(hit: &RefHit, recv: &Receiver, self_chain: &[String], defs: &[&Def]) -> (&'static str, Vec<i64>) {
    // An enum case is only reached as `.case(...)` or `Type.case(...)`, never through an instance.
    let case_ok = match hit.receiver.as_deref() {
        None | Some(".") => true,
        Some(r) => type_like(r),
    };
    // `.member(...)` reaches only cases, static members and initializers of the contextual type; a key path
    // (`\.member`) only properties.
    let implicit = hit.receiver.as_deref() == Some(".");
    let key_path = hit.receiver.as_deref() == Some("\\");
    let cands: Vec<&Def> = defs
        .iter()
        .copied()
        .filter(|d| compatible(&hit.kind, &d.kind) && (case_ok || d.kind != "case"))
        .filter(|d| !implicit || d.kind == "case" || d.is_static)
        .filter(|d| !key_path || matches!(d.kind.as_str(), "property" | "subscript"))
        // Anything a script defines is visible only inside that script.
        .filter(|d| !d.in_script || d.file == hit.file)
        // Through a receiver (`x.f()`, `T.f()`) only members are reachable, never free functions or globals.
        .filter(|d| hit.receiver.is_none() || d.container.is_some() || matches!(hit.kind.as_str(), "instantiate" | "type_ref"))
        .collect();
    // A member used without a call is a property or case; a method only when nothing else has the name, or when the
    // receiver's type has the method (passed or spied on: `service.load.mockReturnValue(…)`).
    let typed = matches!(recv, Receiver::Known(_) | Receiver::Foreign(_));
    let cands: Vec<&Def> = if hit.kind == "member" && !typed && cands.iter().any(|d| d.kind != "method") {
        cands.into_iter().filter(|d| d.kind != "method").collect()
    } else {
        cands
    };
    if cands.is_empty() {
        return ("unresolved", Vec::new());
    }
    let in_container = |c: &str| -> Vec<&Def> {
        // An exact container match beats a nested type that shares the name (`Song` vs `Migration.Song`). A C#
        // extension method is reachable through its declaring class too (`Ext.M(x)`).
        let exact: Vec<&Def> = cands
            .iter()
            .copied()
            .filter(|d| d.container.as_deref() == Some(c) || (d.extension && d.declaring.as_deref() == Some(c)))
            .collect();
        if !exact.is_empty() {
            return exact;
        }
        cands.iter().copied().filter(|d| d.container.as_deref().is_some_and(|dc| dc.ends_with(&format!(".{c}")))).collect()
    };

    let first_in = |chain: &[String], exact_only: bool| -> Vec<&Def> {
        for c in chain {
            let found: Vec<&Def> = if exact_only {
                cands.iter().copied().filter(|d| d.container.as_deref() == Some(c.as_str())).collect()
            } else {
                in_container(c)
            };
            if !found.is_empty() {
                return found;
            }
        }
        Vec::new()
    };
    let narrowed: Vec<&Def> = if let Receiver::Known(chain) | Receiver::Foreign(chain) = recv {
        let mut found = first_in(chain, false);
        if hit.kind == "member" && found.iter().any(|d| d.kind != "method") {
            found.retain(|d| d.kind != "method");
        }
        if found.is_empty() {
            if matches!(recv, Receiver::Foreign(_)) {
                // A framework type may conform to a framework protocol the repo extends.
                let ext: Vec<&Def> = cands.iter().copied().filter(|d| d.external_container).collect();
                return group_confidence(&ext);
            }
            // The receiver's type is known and nothing on it or its supertypes has this name: the member lives
            // outside the repo (a framework or standard-library member).
            return ("unresolved", Vec::new());
        }
        found
    } else {
        match hit.receiver.as_deref() {
            Some("self") | Some("Self") => first_in(self_chain, false),
            // TypeScript's `super.m()`: the nearest supertype defining it, never the caller's own override.
            Some("super") if crate::ts::is_ts_path(&hit.file) => first_in(self_chain.get(1..).unwrap_or_default(), false),
            Some(r) if type_like(r) => {
                // `Type.member`, `Module.Type.member`, or `Type.shared.member` (a static instance of Type).
                let mut found = in_container(r);
                if found.is_empty()
                    && let Some((head, _)) = r.split_once('.')
                {
                    found = in_container(head);
                }
                found
            }
            Some(_) => Vec::new(),
            // Unqualified: a member of the caller's type, an enclosing type or a supertype.
            None => {
                let own = first_in(self_chain, true);
                if !own.is_empty() {
                    return ("exact", own.iter().map(|d| d.id).collect());
                }
                // Otherwise only a free (or nested) function can be called this way; a method of an unrelated
                // type can't. With none, it's a local closure or a library function.
                // A script's globals and free functions are visible only inside that script.
                let free: Vec<&Def> =
                    cands.iter().copied().filter(|d| d.container.is_none() && (!d.in_script || d.file == hit.file)).collect();
                if free.len() > 1 {
                    // Several free functions share the name (common across standalone scripts): one in the
                    // caller's own file wins, then one in the caller's module.
                    let same_file: Vec<&Def> = free.iter().copied().filter(|d| d.file == hit.file).collect();
                    if !same_file.is_empty() {
                        return ("exact", same_file.iter().map(|d| d.id).collect());
                    }
                    let module = module_of(&hit.file);
                    let same_module: Vec<&Def> = free.iter().copied().filter(|d| module_of(&d.file) == module).collect();
                    if same_module.len() == 1 {
                        return ("unique_name", vec![same_module[0].id]);
                    }
                }
                return match free.len() {
                    0 => ("unresolved", Vec::new()),
                    1 => ("unique_name", vec![free[0].id]),
                    _ => ("ambiguous", free.iter().map(|d| d.id).collect()),
                };
            }
        }
    };
    if !narrowed.is_empty() {
        return ("exact", narrowed.iter().map(|d| d.id).collect());
    }
    let instance_receiver = hit.receiver.as_deref().is_some_and(|r| r != "." && !type_like(r));
    if matches!(recv, Receiver::Unknown) && instance_receiver && stdlib_member(&hit.file, &hit.name) {
        return ("external_likely", cands.iter().map(|d| d.id).collect());
    }
    let mut groups: Vec<Option<&str>> = cands.iter().map(|d| d.container.as_deref()).collect();
    groups.sort();
    groups.dedup();
    let ids = cands.iter().map(|d| d.id).collect();
    if groups.len() == 1 { ("unique_name", ids) } else { ("ambiguous", ids) }
}

/// C# predefined types by their .NET names, for comparing a parameter's type with an argument's.
fn keyword_type(name: &str) -> &str {
    match name {
        "String" => "string",
        "Int32" => "int",
        "Int64" => "long",
        "Int16" => "short",
        "UInt32" => "uint",
        "UInt64" => "ulong",
        "UInt16" => "ushort",
        "Byte" => "byte",
        "SByte" => "sbyte",
        "Boolean" => "bool",
        "Double" => "double",
        "Single" => "float",
        "Decimal" => "decimal",
        "Char" => "char",
        "Object" => "object",
        other => other,
    }
}

/// The types C#'s implicit numeric conversions take a value of type `from` to.
fn numeric_targets(from: &str) -> &'static [&'static str] {
    match from {
        "sbyte" => &["short", "int", "long", "float", "double", "decimal"],
        "byte" => &["short", "ushort", "int", "uint", "long", "ulong", "float", "double", "decimal"],
        "short" => &["int", "long", "float", "double", "decimal"],
        "ushort" => &["int", "uint", "long", "ulong", "float", "double", "decimal"],
        "int" => &["long", "float", "double", "decimal"],
        "uint" => &["long", "ulong", "float", "double", "decimal"],
        "long" | "ulong" => &["float", "double", "decimal"],
        "char" => &["ushort", "int", "uint", "long", "ulong", "float", "double", "decimal"],
        "float" => &["double"],
        // An integer literal converts to any numeric type it fits.
        "@int" => &["int", "sbyte", "byte", "short", "ushort", "uint", "long", "ulong", "float", "double", "decimal"],
        _ => &[],
    }
}

/// The interfaces and classes a repo type implementing a sequence derives from.
const SEQUENCE_TYPES: &[&str] = &[
    "IEnumerable", "ICollection", "IList", "IReadOnlyCollection", "IReadOnlyList", "List", "Collection", "HashSet", "ISet",
    "IDictionary", "Dictionary", "IAsyncEnumerable", "IQueryable",
];

/// What a `string` is: its base types and interfaces, and the spans it converts to.
const STRING_SUPERTYPES: &[&str] = &[
    "IEnumerable", "IComparable", "ICloneable", "IConvertible", "IEquatable", "ReadOnlySpan", "ReadOnlyMemory",
    "IFormattable", "Nullable",
];

/// Library types with a user-defined conversion from `string`.
const STRING_CONVERSIONS: &[&str] = &["XName", "XNamespace", "StringValues", "JToken", "PathString", "HtmlString", "FormattableString"];

/// C#'s value keywords and `string`: types whose conversions are known exactly.
const KEYWORD_TYPES: &[&str] =
    &["string", "bool", "char", "sbyte", "byte", "short", "ushort", "int", "uint", "long", "ulong", "float", "double", "decimal", "@int"];

impl TypeOracle<'_> {
    /// Whether an argument of type `arg` (evaluated) may be passed for a parameter declared `param`: `Some(false)`
    /// only when C# certainly rejects it (a `string` for an `int`, an unrelated repo type, a repo type for a
    /// `string`); `Some(true)` when it certainly accepts it; `None` when it can't be told (library types whose
    /// supertypes the repo doesn't declare, generics, delegates).
    fn accepts_arg(&mut self, arg: &TypeExpr, param: &TypeExpr) -> Result<Option<bool>> {
        let name = |t: &TypeExpr| match t {
            TypeExpr::Named(n) => Some(keyword_type(last_segment(n)).to_string()),
            TypeExpr::Generic { name, .. } => Some(keyword_type(last_segment(name)).to_string()),
            _ => None,
        };
        let (Some(a), Some(p)) = (name(arg), name(param)) else {
            return Ok(match (arg, param) {
                // An array is no keyword type and no repo type.
                (TypeExpr::Array(_), _) => match name(param) {
                    Some(p) if KEYWORD_TYPES.contains(&p.as_str()) => Some(false),
                    Some(p) if self.is_defined(&p)? => Some(false),
                    _ => None,
                },
                // A sequence parameter (`IEnumerable<T>`, `List<T>`, `T[]`): a keyword-typed value is none, but a
                // `string` is a sequence of `char`s; a repo type is one only if it implements a collection interface.
                (_, TypeExpr::Array(element)) => match name(arg) {
                    Some(a) if a == "string" => Some(name(element).is_none_or(|e| e == "char")),
                    Some(a) if KEYWORD_TYPES.contains(&a.as_str()) => Some(false),
                    Some(a) if self.is_defined(&a)? => {
                        let chain = self.chain(&a)?;
                        if chain.iter().any(|t| SEQUENCE_TYPES.contains(&last_segment(t))) { None } else { Some(false) }
                    }
                    _ => None,
                },
                _ => None,
            });
        };
        if a == p || p == "object" || p == "dynamic" || (a == "@int" && p == "int") {
            return Ok(Some(true));
        }
        // What an `object` or `dynamic` holds isn't known.
        if matches!(a.as_str(), "object" | "dynamic") {
            return Ok(None);
        }
        if numeric_targets(&a).contains(&p.as_str()) {
            return Ok(Some(true));
        }
        let a_keyword = KEYWORD_TYPES.contains(&a.as_str());
        let p_keyword = KEYWORD_TYPES.contains(&p.as_str());
        if a_keyword && p_keyword {
            return Ok(Some(false));
        }
        let a_repo = !a_keyword && self.is_defined(&a)?;
        let p_repo = !p_keyword && self.is_defined(&p)?;
        // `0` converts to any enum.
        if a == "@int" && p_repo && self.is_enum(&p)? {
            return Ok(None);
        }
        if a_keyword {
            // A value of a keyword type is never a repo type. A `string` is a sequence of `char`s and little else.
            if a == "string" && !p_repo {
                let element_ok = match param {
                    TypeExpr::Generic { args, .. } => match args.as_slice() {
                        [Some(TypeExpr::Named(e))] => keyword_type(last_segment(e)) == "char",
                        _ => true,
                    },
                    _ => true,
                };
                return Ok(if STRING_SUPERTYPES.contains(&p.as_str()) && element_ok {
                    Some(true)
                } else if STRING_CONVERSIONS.contains(&p.as_str()) {
                    None
                } else {
                    Some(false)
                });
            }
            return Ok(if p_repo { Some(false) } else { None });
        }
        if a_repo {
            if self.chain(&a)?.iter().any(|t| keyword_type(last_segment(t)) == p) {
                return Ok(Some(true));
            }
            // A repo type converts to a keyword type or an unrelated repo type only through a user-defined
            // conversion, which isn't read; its library supertypes may still accept a library parameter.
            return Ok(if p_keyword || p_repo { Some(false) } else { None });
        }
        // A library type is never a repo type, nor a keyword type.
        Ok(if p_repo || (p_keyword && a != "Nullable") { Some(false) } else { None })
    }
}

/// Of a C# call's overloads (definitions sharing a container), those whose parameters accept the argument types the
/// call site gives, when some do; overloads the types can't decide stay. Definitions in different containers are
/// left to receiver resolution.
fn narrow_by_argument_types<'d>(oracle: &mut TypeOracle, hit: &RefHit, defs: Vec<&'d Def>) -> Result<Vec<&'d Def>> {
    let Some(args) = hit.arg_types.as_deref().and_then(|a| serde_json::from_str::<Vec<serde_json::Value>>(a).ok()) else {
        return Ok(defs);
    };
    let mut groups: BTreeMap<Option<&str>, Vec<usize>> = BTreeMap::new();
    for (i, d) in defs.iter().enumerate() {
        if d.params.is_some() {
            groups.entry(d.container.as_deref()).or_default().push(i);
        }
    }
    if !groups.values().any(|g| g.len() > 1) {
        return Ok(defs);
    }
    // Each argument: its name when written `name: value`, and its type evaluated.
    let caller = hit.caller_type();
    let mut evaluated: Vec<(Option<String>, Option<TypeExpr>)> = Vec::with_capacity(args.len());
    for a in &args {
        let (name, enc) = match a {
            serde_json::Value::Object(o) => (o.get("n").and_then(|n| n.as_str()).map(str::to_string), o.get("t").and_then(|t| t.as_str())),
            serde_json::Value::String(t) => (None, Some(t.as_str())),
            _ => (None, None),
        };
        oracle.origin = Origin::Here;
        let t = match enc {
            Some(enc) => oracle.eval_encoded(enc, caller.as_deref())?,
            None => None,
        };
        evaluated.push((name, t));
    }
    let mut drop = vec![false; defs.len()];
    for members in groups.values().filter(|g| g.len() > 1) {
        let mut rejected = Vec::new();
        for &i in members {
            let Some(params) = defs[i].params.as_deref().and_then(Params::decode) else { continue };
            let mut fits = true;
            for (k, (name, arg)) in evaluated.iter().enumerate() {
                // A named argument goes to the parameter of that name, which the overload must have.
                let at = match name {
                    Some(n) => match params.names.iter().position(|p| p == n) {
                        Some(at) => at,
                        None => {
                            fits = false;
                            break;
                        }
                    },
                    None => k,
                };
                // Past the declared parameters: a `params` array's elements, which aren't read.
                let Some(Some(p)) = params.types.get(at) else { continue };
                let (Some(arg), Some(p)) = (arg, TypeExpr::decode(p)) else { continue };
                if defs[i].max_args.is_none() && at + 1 >= params.types.len() {
                    continue;
                }
                if oracle.accepts_arg(arg, &p)? == Some(false) {
                    fits = false;
                    break;
                }
            }
            if !fits {
                rejected.push(i);
            }
        }
        // When every overload is rejected the types are misread: keep them all.
        if rejected.len() < members.len() {
            for &i in &rejected {
                drop[i] = true;
            }
            // Of those left, C# calls one taking every argument as is (`int` for an `int`) over one converting it.
            let left: Vec<usize> = members.iter().copied().filter(|i| !rejected.contains(i)).collect();
            let identical: Vec<bool> = left.iter().map(|&i| identical_parameters(defs[i], &evaluated)).collect();
            if identical.iter().any(|x| *x) && !identical.iter().all(|x| *x) {
                for (&i, same) in left.iter().zip(&identical) {
                    if !same {
                        drop[i] = true;
                    }
                }
            }
        }
    }
    Ok(defs.into_iter().zip(drop).filter(|(_, d)| !d).map(|(d, _)| d).collect())
}

/// The constructors a C# type declares, for telling which one a creation calls (see [`constructors_called`]).
pub(crate) struct CtorSet {
    /// The declared constructors, and a primary constructor (`class Order(int id)`) as the type itself.
    defs: Vec<Def>,
    /// A struct: `new S()` calls its implicit parameterless constructor when none declared takes no arguments.
    value_type: bool,
}

impl CtorSet {
    /// The constructors of the type `ty`, in every part of a partial type; `None` when it declares none (only the
    /// implicit one: every creation calls it).
    pub(crate) fn of(conn: &Connection, ty: i64) -> Result<Option<CtorSet>> {
        let sql = format!("SELECT {DEF_COLS} FROM symbols s JOIN files f ON f.id = s.file_id LEFT JOIN symbols p ON p.id = s.parent_id WHERE s.id = ?1");
        let Some(def) = conn.prepare_cached(&sql)?.query_map([ty], def_row)?.next().transpose()? else { return Ok(None) };
        if !matches!(def.kind.as_str(), "class" | "struct") {
            return Ok(None);
        }
        let (name, qualified): (String, String) = conn
            .prepare_cached("SELECT name, CASE WHEN container IS NULL THEN name ELSE container || '.' || name END FROM symbols WHERE id = ?1")?
            .query_row([ty], |r| Ok((r.get(0)?, r.get(1)?)))?;
        let sql = format!(
            "SELECT {DEF_COLS} FROM symbols s JOIN files f ON f.id = s.file_id LEFT JOIN symbols p ON p.id = s.parent_id
             WHERE s.kind = 'init' AND s.container = ?1 AND p.name = ?2 AND p.name = ?2 COLLATE BINARY AND f.lang = 'csharp'
             ORDER BY f.path, s.start_byte"
        );
        let mut defs: Vec<Def> =
            conn.prepare_cached(&sql)?.query_map(params![qualified, name], def_row)?.collect::<rusqlite::Result<Vec<Def>>>()?;
        // A same-named type of another namespace has constructors of its own.
        defs.retain(|d| d.ns == def.ns);
        if def.min_args.is_some() {
            let mut primary = def.clone();
            primary.container = Some(qualified);
            defs.push(primary);
        }
        if defs.is_empty() {
            return Ok(None);
        }
        Ok(Some(CtorSet { defs, value_type: def.kind == "struct" }))
    }

    /// Every constructor's id.
    fn ids(&self) -> Vec<i64> {
        self.defs.iter().map(|d| d.id).collect()
    }
}

/// A C# constructor chained to another, `: this(…)` or `: base(…)` (a primary constructor's base type too): the
/// constructors of the caller's own type or of its base class its arguments may call, `exact` when they pick one.
/// `None` when the call isn't one (a method named `init`); unresolved when the base class isn't in the repo.
fn chained_constructor(
    conn: &Connection,
    oracle: &mut TypeOracle,
    hit: &RefHit,
    sets: &mut HashMap<i64, Option<CtorSet>>,
) -> Result<Option<(&'static str, Vec<i64>)>> {
    let (Some(from), Some(receiver)) = (&hit.from, hit.receiver.as_deref()) else { return Ok(None) };
    if !matches!(receiver, "this" | "base") {
        return Ok(None);
    }
    // The type whose constructor chains: an initializer's type, or the type itself for a primary constructor.
    let own: Option<i64> = if from.kind == "init" {
        conn.prepare_cached("SELECT parent_id FROM symbols WHERE id = ?1")?.query_row([from.id], |r| r.get(0))?
    } else if matches!(from.kind.as_str(), "class" | "struct") {
        Some(from.id)
    } else {
        return Ok(None);
    };
    let Some(own) = own else { return Ok(Some(("unresolved", Vec::new()))) };
    let ty = if receiver == "this" {
        Some(own)
    } else {
        // The base class: the class among the bases any part of the type lists.
        let bases: Vec<String> = conn
            .prepare_cached(
                "SELECT r.target_name FROM relations r JOIN symbols s ON s.id = r.from_id JOIN symbols o ON o.id = ?1
                 WHERE s.name = o.name AND s.container IS o.container AND s.kind IN ('class', 'struct') ORDER BY r.line",
            )?
            .query_map([own], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let mut found = None;
        for b in bases {
            if let Some(d) = definitions(conn, &b)?.into_iter().find(|d| d.kind == "class") {
                found = Some(d.id);
                break;
            }
        }
        found
    };
    let Some(ty) = ty else { return Ok(Some(("unresolved", Vec::new()))) };
    if let std::collections::hash_map::Entry::Vacant(e) = sets.entry(ty) {
        e.insert(CtorSet::of(conn, ty)?);
    }
    let Some(set) = &sets[&ty] else { return Ok(Some(("unresolved", Vec::new()))) };
    oracle.here = Some(hit.file.clone());
    oracle.caller = hit.caller_type();
    let mut ids = constructors_called(oracle, hit, set)?;
    // The type itself stands for its primary constructor, which has no symbol a call can name.
    ids.retain(|&i| i != ty);
    Ok(Some(match ids.len() {
        0 => ("unresolved", ids),
        1 => ("exact", ids),
        _ => ("ambiguous", ids),
    }))
}

/// The constructors of `set` a creation `hit` may call: those taking as many arguments as it passes (named and
/// optional parameters and `params` counted as for methods), narrowed by the argument types it gives, as a method
/// call's overloads are. One left is the one called; several are an ambiguous creation; none is the implicit
/// parameterless constructor of a struct. When the arguments rule every constructor out they were misread: all of
/// them may be called.
fn constructors_called(oracle: &mut TypeOracle, hit: &RefHit, set: &CtorSet) -> Result<Vec<i64>> {
    let all: Vec<&Def> = set.defs.iter().collect();
    let fitting: Vec<&Def> = match hit.args {
        Some(n) => all.iter().copied().filter(|d| d.accepts(n)).collect(),
        None => all.clone(),
    };
    if fitting.is_empty() {
        return Ok(if hit.args == Some(0) && set.value_type { Vec::new() } else { set.ids() });
    }
    let narrowed = if fitting.len() > 1 && hit.arg_types.is_some() { narrow_by_argument_types(oracle, hit, fitting)? } else { fitting };
    Ok(narrowed.iter().map(|d| d.id).collect())
}

/// Whether `def` takes each of the call's positional arguments whose type is known as a parameter of exactly that type
/// (an integer literal for an `int`), every one known, with no named argument.
fn identical_parameters(def: &Def, args: &[(Option<String>, Option<TypeExpr>)]) -> bool {
    let Some(params) = def.params.as_deref().and_then(Params::decode) else { return false };
    let name = |t: &TypeExpr| match t {
        TypeExpr::Named(n) => Some(keyword_type(last_segment(n)).to_string()),
        _ => None,
    };
    args.iter().enumerate().all(|(k, (named, arg))| {
        let p = params.types.get(k).cloned().flatten().and_then(|p| TypeExpr::decode(&p));
        match (named, arg.as_ref().and_then(name), p.as_ref().and_then(name)) {
            (None, Some(a), Some(p)) => a == p || (a == "@int" && p == "int"),
            _ => false,
        }
    })
}

/// A C# method's parameters as stored (see `FileFacts::param_types`): each one's declared type, encoded (`None` for a
/// type parameter), and name.
#[derive(serde::Deserialize)]
struct Params {
    types: Vec<Option<String>>,
    #[serde(default)]
    names: Vec<String>,
}

impl Params {
    fn decode(json: &str) -> Option<Params> {
        serde_json::from_str(json).ok()
    }
}

/// The columns [`def_row`] reads, from `symbols s JOIN files f` and the parent symbol `p`.
const DEF_COLS: &str = "s.id, s.kind, s.container, s.signature LIKE '%static %' OR s.signature LIKE '%class func %', f.path,
     s.container IS NOT NULL AND NOT EXISTS (SELECT 1 FROM symbols t WHERE t.name = substr(s.container, length(rtrim(s.container, replace(s.container, '.', ''))) + 1) AND t.kind IN ('class', 'struct', 'enum', 'actor', 'protocol', 'typealias')),
     f.script, s.min_args, s.max_args, s.signature LIKE '%(this %', p.name,
     CASE WHEN f.lang = 'csharp' THEN coalesce(
       CASE WHEN json_valid(s.type_name) THEN json_extract(s.type_name, '$.ns') END,
       CASE WHEN json_valid(p.type_name) THEN json_extract(p.type_name, '$.ns') END) END, s.param_types, f.lang";

fn def_row(r: &Row) -> rusqlite::Result<Def> {
    let extension: bool = r.get(9)?;
    Ok(Def {
        id: r.get(0)?,
        kind: r.get(1)?,
        container: r.get(2)?,
        is_static: r.get(3)?,
        file: r.get(4)?,
        external_container: r.get(5)?,
        in_script: r.get(6)?,
        min_args: r.get(7)?,
        max_args: r.get(8)?,
        extension,
        declaring: if extension { r.get(10)? } else { None },
        ns: r.get::<_, Option<String>>(11)?.filter(|n| !n.is_empty()),
        params: r.get(12)?,
        lang: r.get(13)?,
    })
}

fn defs_named(conn: &Connection, name: &str) -> Result<Vec<Def>> {
    let sql = format!(
        "SELECT {DEF_COLS} FROM symbols s JOIN files f ON f.id = s.file_id LEFT JOIN symbols p ON p.id = s.parent_id
         WHERE s.name = ?1 AND s.name = ?1 COLLATE BINARY ORDER BY f.path, s.start_byte"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map([name], def_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn target(conn: &Connection, id: i64) -> Result<Target> {
    let mut stmt = conn.prepare_cached(
        "SELECT s.id, s.name, s.container, s.kind, f.path, s.start_line FROM symbols s JOIN files f ON f.id = s.file_id
         WHERE s.id = ?1",
    )?;
    Ok(stmt.query_row([id], |r| {
        Ok(Target {
            id: r.get(0)?,
            qualified: join(r.get::<_, Option<String>>(2)?.as_deref(), &r.get::<_, String>(1)?),
            kind: r.get(3)?,
            file: r.get(4)?,
            line: r.get(5)?,
            repo: None,
        })
    })?)
}

/// A dependency repo's index, for resolving references that leave the repo.
pub struct Dependency<'c> {
    pub repo: String,
    pub conn: &'c Connection,
    /// Module names the dependency provides (its targets and products), matched against `import` statements.
    pub modules: Vec<String>,
    /// What resolving into it assumes (`revision_unverified`: the version asked for isn't known to be the checkout's).
    pub assumptions: Vec<crate::answer::Assumption>,
}

/// Every import in the repo: `(path, module, line)`, ordered by path.
pub fn imports(conn: &Connection) -> Result<Vec<(String, String, u32)>> {
    let mut stmt =
        conn.prepare("SELECT f.path, i.module, i.line FROM imports i JOIN files f ON f.id = i.file_id ORDER BY f.path, i.line")?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[derive(Debug, Clone, Serialize)]
pub struct SymbolAt {
    /// `Container.name`, as `context` takes it.
    pub qualified: String,
    pub name: String,
    pub kind: String,
    pub signature: String,
    pub file: String,
    pub start_line: u32,
    pub end_line: u32,
}

/// The symbols whose span contains `line` of `file` (repo-relative; a leading `./` is ignored), innermost first.
pub fn symbols_at(conn: &Connection, file: &str, line: u32) -> Result<Vec<SymbolAt>> {
    let file = file.trim_start_matches("./");
    let mut stmt = conn.prepare_cached(
        "SELECT s.name, s.container, s.kind, s.signature, s.start_line, s.end_line FROM symbols s
         JOIN files f ON f.id = s.file_id
         WHERE f.path = ?1 AND s.start_line <= ?2 AND s.end_line >= ?2
         ORDER BY s.end_line - s.start_line, s.start_line DESC",
    )?;
    let rows = stmt.query_map(params![file, line], |r| {
        let (name, container): (String, Option<String>) = (r.get(0)?, r.get(1)?);
        Ok(SymbolAt {
            qualified: container.map_or_else(|| name.clone(), |c| format!("{c}.{name}")),
            name,
            kind: r.get(2)?,
            signature: r.get(3)?,
            file: file.to_string(),
            start_line: r.get(4)?,
            end_line: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// An HTTP route with its path resolved: what a controller action or minimal API endpoint serves, or what an API
/// contract requests.
#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
pub struct RouteRow {
    /// `server`, `client`, `service`, `commented`, or `unsupported` (a registration whose route can't be placed:
    /// `verb` names its style, `detail` why).
    pub side: String,
    /// Upper-case; `*` for an endpoint answering every verb.
    pub verb: String,
    /// The full path without leading or trailing `/`, or `None` when it names a constant that isn't found.
    pub path: Option<String>,
    /// The service a gateway contract is sent to, or that a `service` registration hosts, resolved.
    #[serde(default)]
    pub service: Option<String>,
    /// The qualified name of the action or contract (the file's name for top-level statements).
    pub symbol: String,
    pub kind: String,
    pub file: String,
    pub start_line: u32,
    pub end_line: u32,
    /// The line of the attribute, call or comment the route comes from.
    #[serde(default)]
    pub line: u32,
    /// The host a client route is sent to, when its URL names one or its typed client's base address does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// Where `host` was read (`file:line`), when not at the route itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_at: Option<String>,
    /// For an `unsupported` registration: why its route can't be placed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// An `http_routes` row as [`http_routes`] reads it: (side, verb, path, path_ref), (service, service_ref), (symbol
/// name, container, kind, file), (start line, end line, line).
type RawRoute = (
    (String, String, String, Option<String>),
    (Option<String>, Option<String>),
    (Option<String>, Option<String>, Option<String>, String),
    (Option<u32>, Option<u32>, u32),
);

/// How many helpers deep a route group is followed (`{@Helper#i}` in a group passed on).
const GROUP_DEPTH: u32 = 4;

/// Every HTTP route in the repo, with constant references resolved against the repo's constants, and minimal API
/// endpoints under the prefixes of the route groups their helpers are called with.
pub fn http_routes(conn: &Connection) -> Result<Vec<RouteRow>> {
    let mut stmt = conn.prepare(
        "SELECT r.side, r.verb, r.path, r.path_ref, r.service, r.service_ref, s.name, s.container, s.kind, f.path,
                s.start_line, s.end_line, r.line
         FROM http_routes r LEFT JOIN symbols s ON s.id = r.symbol_id JOIN files f ON f.id = r.file_id
         ORDER BY f.path, r.line",
    )?;
    let mut consts = Constants::new(conn)?;
    let rows = stmt.query_map([], |r| {
        Ok((
            (r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, Option<String>>(3)?),
            (r.get::<_, Option<String>>(4)?, r.get::<_, Option<String>>(5)?),
            (r.get::<_, Option<String>>(6)?, r.get::<_, Option<String>>(7)?, r.get::<_, Option<String>>(8)?, r.get::<_, String>(9)?),
            (r.get::<_, Option<u32>>(10)?, r.get::<_, Option<u32>>(11)?, r.get::<_, u32>(12)?),
        ))
    })?;
    let rows: Vec<RawRoute> = rows.collect::<rusqlite::Result<_>>()?;
    // The prefixes each helper parameter is passed (`MapOrders#0`), or why one can't be known.
    let mut groups: HashMap<String, Vec<std::result::Result<String, String>>> = HashMap::new();
    for ((side, verb, path, _), (service, _), _, _) in &rows {
        if side == "group"
            && let Some(key) = service
        {
            groups.entry(key.clone()).or_default().push(if verb == "?" { Err(path.clone()) } else { Ok(path.clone()) });
        }
    }
    // Typed HTTP clients' base addresses, by client class.
    let mut clients: HashMap<String, (String, String)> = HashMap::new();
    let mut hstmt = conn.prepare_cached(
        "SELECT h.target, h.host, f.path, h.line FROM host_facts h JOIN files f ON f.id = h.file_id
         WHERE h.role = 'configures' AND h.kind = 'http_client' AND h.target IS NOT NULL ORDER BY f.path, h.line",
    )?;
    for row in hstmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, u32>(3)?)))? {
        let (target, host, file, line) = row?;
        clients.entry(target).or_insert((host, format!("{file}:{line}")));
    }
    let controllers = if rows.iter().any(|((side, _, path, _), ..)| side == "server" && path.starts_with("{^")) { Some(Controllers::new(conn, &rows)?) } else { None };
    let mut out: Vec<RouteRow> = Vec::with_capacity(rows.len());
    for ((side, verb, path, path_ref), (service, service_ref), (name, container, kind, file), (start_line, end_line, line)) in rows {
        if matches!(side.as_str(), "group" | "prefix" | "conventional") {
            continue;
        }
        // Top-level statements: the file stands for the symbol.
        let symbol = match &name {
            Some(n) => container.as_ref().map_or_else(|| n.clone(), |c| format!("{c}.{n}")),
            None => file.rsplit('/').next().unwrap_or(&file).split('.').next().unwrap_or_default().to_string(),
        };
        let kind = kind.unwrap_or_else(|| "script".to_string());
        let (start_line, end_line) = (start_line.unwrap_or(line), end_line.unwrap_or(line));
        let row = |side: String, verb: String, path: Option<String>, detail: Option<String>| RouteRow {
            side,
            verb,
            path,
            service: None,
            symbol: symbol.clone(),
            kind: kind.clone(),
            file: file.clone(),
            start_line,
            end_line,
            line,
            host: None,
            host_at: None,
            detail,
        };
        if side == "unsupported" {
            out.push(row(side, verb, None, Some(path)));
            continue;
        }
        // A controller action whose prefix comes from its class's bases, or from conventional routes.
        if side == "server"
            && let Some(ctl) = &controllers
            && path.starts_with("{^")
        {
            let class = container.as_deref().map(|c| c.rsplit('.').next().unwrap_or(c)).unwrap_or_default();
            for p in ctl.paths(&mut consts, class, &path)? {
                let r = row(side.clone(), verb.clone(), Some(p), None);
                if !out.contains(&r) {
                    out.push(r);
                }
            }
            continue;
        }
        // A minimal API endpoint: under each prefix its helper is passed, its constants resolved.
        if side == "server" && path_ref.is_none() && (path.contains("{@") || path.contains("{=")) {
            for p in under_groups(&path, &groups, 0) {
                match p {
                    Ok(p) => {
                        let r = row(side.clone(), verb.clone(), server_path(&mut consts, &p)?, None);
                        if !out.contains(&r) {
                            out.push(r);
                        }
                    }
                    Err(why) => {
                        let r = row("unsupported".to_string(), verb.clone(), None, Some(why));
                        if !out.contains(&r) {
                            out.push(r);
                        }
                    }
                }
            }
            continue;
        }
        let mut host = None;
        let path = match path_ref {
            None if side == "client" => {
                let url = consts.expand(&path, 0)?;
                host = kc_extract::urls::host_of(&url);
                let p = client_path(&url);
                // A request a method makes (a gateway or HTTP client call) to a URL that can't be known links nothing:
                // it is kept as `unresolved`, so an answer finding no caller can say where requests weren't read.
                if p.is_none() && !matches!(kind.as_str(), "class" | "struct" | "protocol" | "enum") {
                    out.push(row("unresolved".to_string(), verb, None, None));
                    continue;
                }
                p
            }
            None => Some(path),
            Some(r) => consts.resolve(&r)?.map(|v| {
                [path.as_str(), v.trim_matches('/')].iter().filter(|p| !p.is_empty()).copied().collect::<Vec<_>>().join("/")
            }),
        };
        let service = match service_ref {
            Some(r) => consts.resolve(&r)?,
            None => service,
        };
        let mut r = row(side, verb, path, None);
        r.service = service;
        if r.side == "client" {
            let class = if matches!(r.kind.as_str(), "class" | "struct") { name.clone() } else { container.as_deref().map(|c| c.rsplit('.').next().unwrap_or(c).to_string()) };
            match (host, class.and_then(|c| clients.get(&c))) {
                (Some(h), _) => r.host = Some(h),
                (None, Some((h, at))) => {
                    r.host = Some(h.clone());
                    r.host_at = Some(at.clone());
                }
                (None, None) => {}
            }
        }
        out.push(r);
    }
    // An operation of a client the repo itself declares (a generated client checked in) is requested by the client's
    // own method, with its URL: the operation adds nothing.
    let declared: std::collections::HashSet<String> =
        out.iter().filter(|r| r.side == "client" && r.kind == "method").filter_map(|r| r.symbol.rsplit('.').next().map(str::to_ascii_lowercase)).collect();
    out.retain(|r| r.side != "operation" || r.path.as_deref().is_some_and(|op| !declared.contains(&op.to_ascii_lowercase())));
    Ok(out)
}

/// What places a controller action whose class writes no `[Route]` out (`{^Action}/rest`, `{^*Action}` for a public
/// method with no route attribute; see `kc_extract::csharp`): the classes' `[Route]` templates, their bases, and
/// the repo's conventional route templates.
struct Controllers {
    /// Class name → its `[Route]` template, or the constant naming it.
    prefixes: HashMap<String, (String, Option<String>)>,
    /// Class name → the names of its bases.
    bases: HashMap<String, Vec<String>>,
    conventional: Vec<String>,
}

/// How many bases up a class's route prefix is looked for.
const BASE_DEPTH: u32 = 8;

impl Controllers {
    fn new(conn: &Connection, rows: &[RawRoute]) -> Result<Self> {
        let mut prefixes = HashMap::new();
        let mut conventional = Vec::new();
        for ((side, _, path, path_ref), _, (name, ..), _) in rows {
            match side.as_str() {
                "prefix" => {
                    if let Some(n) = name {
                        prefixes.entry(n.clone()).or_insert((path.clone(), path_ref.clone()));
                    }
                }
                "conventional" if !conventional.contains(path) => conventional.push(path.clone()),
                _ => {}
            }
        }
        let mut bases: HashMap<String, Vec<String>> = HashMap::new();
        let mut stmt = conn.prepare_cached(
            "SELECT DISTINCT s.name, r.target_name FROM relations r JOIN symbols s ON s.id = r.from_id WHERE s.kind = 'class' ORDER BY s.name, r.line",
        )?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
            let (class, base) = row?;
            bases.entry(class).or_default().push(base);
        }
        Ok(Controllers { prefixes, bases, conventional })
    }

    /// The `[Route]` template nearest `class` up its bases (its own first), as ASP.NET takes it.
    fn prefix(&self, class: &str) -> Option<&(String, Option<String>)> {
        let mut level = vec![class.to_string()];
        let mut seen = std::collections::HashSet::new();
        for _ in 0..=BASE_DEPTH {
            let mut next = Vec::new();
            for c in &level {
                if let Some(p) = self.prefixes.get(c) {
                    return Some(p);
                }
                if seen.insert(c.clone()) {
                    next.extend(self.bases.get(c).into_iter().flatten().cloned());
                }
            }
            if next.is_empty() {
                break;
            }
            level = next;
        }
        None
    }

    /// The paths of an action of `class` encoded `{^Action}rest` (see [`Controllers`]): under the inherited prefix,
    /// else, for an action without a template, each conventional route's, else the action's own template. None for a
    /// method no route serves.
    fn paths(&self, consts: &mut Constants, class: &str, encoded: &str) -> Result<Vec<String>> {
        let Some((marker, rest)) = encoded.strip_prefix("{^").and_then(|e| e.split_once('}')) else { return Ok(vec![encoded.to_string()]) };
        let (implicit, action) = match marker.strip_prefix('*') {
            Some(a) => (true, a),
            None => (false, marker),
        };
        let rest = rest.trim_matches('/');
        let controller = class.strip_suffix("Controller").unwrap_or(class);
        let fill = |t: &str| -> String {
            let t = t.replace("[controller]", controller).replace("[action]", action);
            // Any other token (`[area]` from a base) stands for whatever it is.
            t.split('/')
                .filter(|s| !s.is_empty())
                .map(|s| if s.starts_with('[') && s.ends_with(']') { "{}" } else { s })
                .collect::<Vec<_>>()
                .join("/")
        };
        if let Some((template, r)) = self.prefix(class) {
            // An inherited prefix routes the actions written with attributes only.
            if implicit {
                return Ok(Vec::new());
            }
            let template = match r {
                Some(r) => match consts.resolve(r)? {
                    Some(v) => v,
                    None => return Ok(vec![rest.to_string()]),
                },
                None => template.clone(),
            };
            let joined = [fill(&template), rest.to_string()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("/");
            return Ok(vec![joined]);
        }
        if rest.is_empty() && !self.conventional.is_empty() {
            let mut out: Vec<String> = Vec::new();
            for t in &self.conventional {
                for p in conventional_paths(t, controller, action) {
                    if !out.contains(&p) {
                        out.push(p);
                    }
                }
            }
            return Ok(out);
        }
        Ok(if implicit { Vec::new() } else { vec![rest.to_string()] })
    }
}

/// A conventional route template filled for one action: `{controller=Home}/{action=Index}/{id?}` is `Orders/List`
/// and `Orders/List/{id}` (trailing parameters may be left out). A template without `{action}` (Web API's
/// `api/{controller}/{id}`) picks the action by verb: `api/Orders`, `api/Orders/{id}`.
fn conventional_paths(template: &str, controller: &str, action: &str) -> Vec<String> {
    let mut segs: Vec<(String, bool)> = Vec::new();
    for s in template.split('/').filter(|s| !s.is_empty()) {
        let inner = s.strip_prefix('{').and_then(|s| s.strip_suffix('}'));
        let name = inner.map(|i| i.trim_start_matches('*').split(['=', ':', '?']).next().unwrap_or_default());
        segs.push(match name {
            Some("controller") => (controller.to_string(), false),
            Some("action") => (action.to_string(), false),
            Some(n) => (format!("{{{n}}}"), true),
            None => (s.to_string(), false),
        });
    }
    let fixed = segs.iter().rposition(|(_, p)| !p).map_or(0, |i| i + 1);
    let mut out = Vec::new();
    for end in fixed..=segs.len() {
        out.push(segs[..end].iter().map(|(s, _)| s.as_str()).collect::<Vec<_>>().join("/"));
    }
    out
}

/// An endpoint's encoded path under the prefixes its helper is passed (see [`http_routes`]): `{@MapOrders#0}/{id}`
/// with `MapOrders` called on `app.MapGroup("api/orders")` is `api/orders/{id}`. A helper no call passes a group to
/// maps on the application itself.
fn under_groups(path: &str, groups: &HashMap<String, Vec<std::result::Result<String, String>>>, depth: u32) -> Vec<std::result::Result<String, String>> {
    let Some(rest) = path.strip_prefix("{@") else { return vec![Ok(path.to_string())] };
    let Some((key, rest)) = rest.split_once('}') else { return vec![Ok(path.to_string())] };
    if depth >= GROUP_DEPTH {
        return vec![Err(format!("its route group is passed through more than {GROUP_DEPTH} helpers"))];
    }
    let none = vec![Ok(String::new())];
    let prefixes = groups.get(key).filter(|p| !p.is_empty()).unwrap_or(&none);
    let mut out = Vec::new();
    for p in prefixes {
        match p {
            Ok(p) => {
                let joined = [p.as_str(), rest].iter().flat_map(|s| s.split('/')).filter(|s| !s.is_empty()).collect::<Vec<_>>().join("/");
                for x in under_groups(&joined, groups, depth + 1) {
                    if !out.contains(&x) {
                        out.push(x);
                    }
                }
            }
            Err(why) => out.push(Err(why.clone())),
        }
    }
    out
}

/// A server route's encoded path with its constants replaced: `None` when one isn't found (the route can't be known).
fn server_path(consts: &mut Constants, encoded: &str) -> Result<Option<String>> {
    let mut out = String::new();
    let mut rest = encoded;
    while let Some(i) = rest.find("{=") {
        let Some(j) = rest[i..].find('}') else { break };
        out.push_str(&rest[..i]);
        match consts.resolve(&rest[i + 2..i + j])? {
            Some(v) => out.push_str(&v),
            None => return Ok(None),
        }
        rest = &rest[i + j + 1..];
    }
    out.push_str(rest);
    Ok(Some(out.trim_start_matches('~').split('/').filter(|s| !s.is_empty()).collect::<Vec<_>>().join("/")))
}

/// A host fact (see `kc_extract::HostFact`) with the file it was read from.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, serde::Deserialize)]
pub struct HostRow {
    pub role: String,
    pub kind: String,
    pub host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    pub file: String,
    pub line: u32,
}

/// Every host the repo's config and code say it serves or its clients send to.
pub fn host_facts(conn: &Connection) -> Result<Vec<HostRow>> {
    let mut stmt = conn.prepare_cached(
        "SELECT h.role, h.kind, h.host, h.target, f.path, h.line FROM host_facts h JOIN files f ON f.id = h.file_id ORDER BY f.path, h.line, h.host",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(HostRow { role: r.get(0)?, kind: r.get(1)?, host: r.get(2)?, target: r.get(3)?, file: r.get(4)?, line: r.get(5)? })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The repo's string constants, for resolving routes.
struct Constants<'c> {
    exact: rusqlite::CachedStatement<'c>,
    suffix: rusqlite::CachedStatement<'c>,
}

impl<'c> Constants<'c> {
    fn new(conn: &'c Connection) -> Result<Self> {
        Ok(Constants {
            exact: conn.prepare_cached("SELECT DISTINCT value FROM constants WHERE qualified = ?1")?,
            suffix: conn.prepare_cached("SELECT DISTINCT value FROM constants WHERE qualified LIKE '%.' || ?1")?,
        })
    }

    /// The value of a constant: `Api.x` names `Api.x`, or failing that `Outer.Api.x`. A name with several values is
    /// unknown, unless they agree once expanded and stripped of a base address (`'/api'` and
    /// `'http://localhost:5000/api'` are both `api`).
    fn resolve(&mut self, name: &str) -> Result<Option<String>> {
        self.resolve_at(name, 0)
    }

    fn resolve_at(&mut self, name: &str, depth: u32) -> Result<Option<String>> {
        let mut values: Vec<String> = self.exact.query_map([name], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        if values.is_empty() {
            values = self.suffix.query_map([name], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        }
        if let [v] = values.as_slice()
            && !v.contains("{=")
        {
            return Ok(Some(v.clone()));
        }
        let mut seen = Vec::new();
        for v in &values {
            let v = strip_base(&self.expand(v, depth + 1)?);
            if !seen.contains(&v) {
                seen.push(v);
            }
        }
        Ok(match seen.as_slice() {
            [v] => Some(v.clone()),
            _ => None,
        })
    }

    /// An encoded path (`{=API}/orders/{}`) with its constants replaced by their values. An unknown constant is a
    /// route parameter (`{}`), or nothing at the start of the path, where it is taken for a base address.
    fn expand(&mut self, encoded: &str, depth: u32) -> Result<String> {
        let mut out = String::new();
        let mut rest = encoded;
        while let Some(i) = rest.find("{=") {
            let Some(j) = rest[i..].find('}') else { break };
            out.push_str(&rest[..i]);
            let name = &rest[i + 2..i + j];
            match if depth < 4 { self.resolve_at(name, depth)? } else { None } {
                Some(v) => out.push_str(&v),
                None if out.trim_matches('/').is_empty() => {}
                None => out.push_str("{}"),
            }
            rest = &rest[i + j + 1..];
        }
        out.push_str(rest);
        Ok(out)
    }
}

/// `https://host/api/orders/{}?x=1` → `api/orders/{}`: a URL's path without its base address or query.
fn strip_base(url: &str) -> String {
    let url = url.split(['?', '#']).next().unwrap_or_default();
    let path = match url.find("://") {
        Some(i) => url[i + 3..].find('/').map_or("", |j| &url[i + 3 + j..]),
        None => url,
    };
    path.split('/').filter(|s| !s.is_empty()).collect::<Vec<_>>().join("/")
}

/// The path a client request names, or `None` when only parameters are left.
fn client_path(url: &str) -> Option<String> {
    let path = strip_base(url);
    path.split('/').any(|s| s != "{}").then_some(path).filter(|p| !p.is_empty())
}

/// A custom element or directive attribute used in a template, and the component or directive it resolves to
/// (`None` for an element no indexed component declares, like a library's).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NgUse {
    /// The element's tag (`nb-badge`), or the attribute in brackets (`[nbBadge]`).
    pub tag: String,
    pub component: Option<String>,
    /// The repo of the component (for `renders`) or of the template (for `rendered_by`) when it's another repo.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    pub file: String,
    pub line: u32,
    /// The element's tag and attribute names, for matching selectors in other repos.
    #[serde(skip)]
    pub on: Vec<String>,
}

impl NgUse {
    /// The template fact this use was read from: (`renders`, tag) or (`attr`, attribute).
    pub fn key(&self) -> (&'static str, &str) {
        match self.tag.strip_prefix('[').and_then(|t| t.strip_suffix(']')) {
            Some(attr) => ("attr", attr),
            None => ("renders", self.tag.as_str()),
        }
    }

    /// Whether `part` selects this use.
    pub fn matched_by(&self, part: &SelectorPart) -> bool {
        let (kind, value) = self.key();
        let (tag, attrs) = self.on.split_first().map_or((value, &[][..]), |(t, a)| (t.as_str(), a));
        part.key().is_some_and(|(k, v)| k == kind && v == value) && part.matches(tag, attrs)
    }
}

/// What Angular says about a component.
#[derive(Debug, Clone, Serialize)]
pub struct Angular {
    pub selector: Option<String>,
    /// The template file, repo-relative.
    pub template: Option<String>,
    /// Full route paths that show the component, lazy prefixes included.
    pub routes: Vec<String>,
    /// Components whose templates use it: one of its element selectors, or as a directive, one of its attributes.
    pub rendered_by: Vec<NgUse>,
    /// Custom elements its template uses, and the directives its attributes select.
    pub renders: Vec<NgUse>,
    /// Attributes in its template that no component or directive of the repo selects, to be looked up in the repos
    /// it depends on (most are plain inputs and never resolve).
    #[serde(skip)]
    pub unresolved_attrs: Vec<NgUse>,
}

/// One compound selector of a component's or directive's selector list: `nb-badge`, `[nbBadge]`,
/// `button[uiButton]`, `ui-hint:not([validationFor])`.
#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
pub struct SelectorPart {
    /// The element name, lower case.
    pub element: Option<String>,
    /// Attributes the element must have, as written.
    pub attrs: Vec<String>,
    /// Attributes it must not have (`:not([x])`).
    #[serde(default)]
    pub not: Vec<String>,
}

impl SelectorPart {
    /// The template fact a use of this selector is recorded under: a custom element's `renders` row, or else an
    /// `attr` row for its first attribute. `None` for a selector no template fact records (a plain `div`).
    pub fn key(&self) -> Option<(&'static str, &str)> {
        match (&self.element, self.attrs.first()) {
            (Some(e), _) if kc_extract::html::is_custom(e) => Some(("renders", e.as_str())),
            (_, Some(a)) => Some(("attr", a.as_str())),
            _ => None,
        }
    }

    /// Whether an element with this tag and these attributes matches.
    pub fn matches(&self, tag: &str, attrs: &[String]) -> bool {
        self.element.as_deref().is_none_or(|e| e == tag)
            && self.attrs.iter().all(|a| attrs.contains(a))
            && !self.not.iter().any(|a| attrs.contains(a))
    }

    /// How the use is shown: the tag for an element, the attribute in brackets for a directive.
    fn shown(&self) -> String {
        match self.key() {
            Some(("renders", t)) => t.to_string(),
            Some((_, a)) => format!("[{a}]"),
            None => String::new(),
        }
    }
}

/// The parts of a selector list that a template can be matched against. Parts with classes, ids, attribute values
/// or combinators are left out: templates are scanned for tags and attribute names only.
pub fn selector_parts(selector: &str) -> Vec<SelectorPart> {
    selector.split(',').filter_map(|s| parse_part(s.trim())).filter(|p| p.key().is_some()).collect()
}

fn parse_part(s: &str) -> Option<SelectorPart> {
    let ident = |s: &str| s.find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_')).unwrap_or(s.len());
    let mut part = SelectorPart { element: None, attrs: Vec::new(), not: Vec::new() };
    let n = ident(s);
    if n > 0 {
        part.element = Some(s[..n].to_ascii_lowercase());
    }
    let mut rest = &s[n..];
    // `[name]` with no value: the attribute's name.
    let attr = |r: &str| -> Option<(String, usize)> {
        let end = r.find(']')?;
        let name = &r[1..end];
        (!name.is_empty() && ident(name) == name.len()).then(|| (name.to_string(), end + 1))
    };
    while !rest.is_empty() {
        if rest.starts_with('[') {
            let (name, len) = attr(rest)?;
            part.attrs.push(name);
            rest = &rest[len..];
        } else {
            // `:not([name])`; any other pseudo-class, class or id isn't matched.
            let inner = rest.strip_prefix(":not(").filter(|i| i.starts_with('['))?;
            let (name, len) = attr(inner)?;
            if !inner[len..].starts_with(')') {
                return None;
            }
            part.not.push(name);
            rest = &inner[len + 1..];
        }
    }
    Some(part)
}

/// A selector as recorded: written out, or `{=Name}` for a constant (`selector: flexSelectors`), looked up.
fn selector_value(conn: &Connection, value: String) -> Result<Option<String>> {
    let Some(name) = value.strip_prefix("{=").and_then(|v| v.strip_suffix('}')) else { return Ok(Some(value)) };
    let mut stmt = conn.prepare_cached("SELECT DISTINCT value FROM constants WHERE qualified = ?1")?;
    let values: Vec<String> = stmt.query_map([name], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    Ok(match values.as_slice() {
        [v] => Some(v.clone()),
        _ => None,
    })
}

/// A template use row: (owning component's id and name, for an inline template; file; kind; value; extra; line).
type UseRow = (Option<i64>, Option<String>, String, String, String, Option<String>, u32);

fn use_of(kind: &str, value: &str, extra: Option<&str>, file: &str, line: u32) -> NgUse {
    let on: Vec<String> = extra.map(|e| e.split_whitespace().map(str::to_string).collect()).unwrap_or_default();
    let tag = if kind == "attr" { format!("[{value}]") } else { value.to_string() };
    NgUse { tag, component: None, repo: None, file: file.to_string(), line, on }
}

/// The Angular facts about a component (or directive): `None` when the symbol isn't one.
pub fn angular(conn: &Connection, symbol_id: i64) -> Result<Option<Angular>> {
    // Every component in the repo: name, file, selector, resolved template.
    let mut stmt = conn.prepare(
        "SELECT n.symbol_id, s.name, f.path, n.kind, n.value FROM ng_facts n JOIN symbols s ON s.id = n.symbol_id
         JOIN files f ON f.id = n.file_id WHERE n.kind IN ('selector', 'template')",
    )?;
    let mut comps: HashMap<i64, (String, String, Option<String>, Option<String>)> = HashMap::new();
    for row in stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?, r.get::<_, String>(4)?)))? {
        let (id, name, file, kind, value) = row?;
        let e = comps.entry(id).or_insert_with(|| (name, file.clone(), None, None));
        match kind.as_str() {
            "selector" => e.2 = selector_value(conn, value)?,
            _ => e.3 = Some(join_relative(&file, &value)),
        }
    }
    let Some((name, _, selector, template)) = comps.get(&symbol_id).cloned() else { return Ok(None) };
    // Selectors map template uses to components. Test doubles that copy a component's selector never answer: they
    // aren't what runs, and the real one may live in a library repo.
    let mut ordered: Vec<&(String, String, Option<String>, Option<String>)> =
        comps.values().filter(|(n, f, _, _)| !is_test_double(n, f)).collect();
    ordered.sort_by_key(|(n, f, _, _)| (f.clone(), n.clone()));
    let parts: Vec<(SelectorPart, &String)> =
        ordered.iter().flat_map(|(n, _, sel, _)| selector_parts(sel.as_deref().unwrap_or("")).into_iter().map(move |p| (p, n))).collect();
    let mut renders_stmt = conn.prepare(
        "SELECT n.symbol_id, s.name, f.path, n.kind, n.value, n.extra, n.line FROM ng_facts n JOIN files f ON f.id = n.file_id
         LEFT JOIN symbols s ON s.id = n.symbol_id WHERE n.kind IN ('renders', 'attr') ORDER BY f.path, n.line",
    )?;
    let rows: Vec<UseRow> = renders_stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let rendered_by = template_uses(conn, &selector_parts(selector.as_deref().unwrap_or("")))?;
    let (mut renders, mut unresolved_attrs) = (Vec::new(), Vec::new());
    for (sid, _, f, kind, value, extra, l) in &rows {
        if !(*sid == Some(symbol_id) || (sid.is_none() && template.as_deref() == Some(f.as_str()))) {
            continue;
        }
        let mut u = use_of(kind, value, extra.as_deref(), f, *l);
        u.component = parts.iter().find(|(p, _)| u.matched_by(p)).map(|(_, n)| (*n).clone());
        match (kind.as_str(), &u.component) {
            ("attr", None) => unresolved_attrs.push(u),
            _ => renders.push(u),
        }
    }
    Ok(Some(Angular { selector, template, routes: ng_routes(conn, &name)?, rendered_by, renders, unresolved_attrs }))
}

/// A template use row: (owning component's id, for an inline template; file; kind; value; extra; line).
type OwnedUse = (Option<i64>, String, String, String, Option<String>, u32);

/// A component or directive another's template uses: its symbol id, and where the template uses it.
#[derive(Debug, Clone, PartialEq)]
pub struct NgChild {
    /// The repo's component or directive; `None` for a use no selector of the repo matches (a library's, maybe).
    pub id: Option<i64>,
    pub file: String,
    pub line: u32,
    /// The element or attribute used, for matching other repos' selectors.
    pub used: NgUse,
}

/// For every component of the repo, what its template uses (by element selector or directive attribute), in template
/// order: each component and directive of the repo matching a use, once (every directive an element's attributes
/// select, not only the first), and the uses none matches. Test doubles never answer (see [`angular`]).
pub fn ng_children(conn: &Connection) -> Result<HashMap<i64, Vec<NgChild>>> {
    let mut stmt = conn.prepare(
        "SELECT n.symbol_id, s.name, f.path, n.kind, n.value FROM ng_facts n JOIN symbols s ON s.id = n.symbol_id
         JOIN files f ON f.id = n.file_id WHERE n.kind IN ('selector', 'template') ORDER BY f.path, s.name, n.line",
    )?;
    let rows: Vec<(i64, String, String, String, String)> =
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?.collect::<rusqlite::Result<_>>()?;
    let mut parts: Vec<(SelectorPart, i64)> = Vec::new();
    let mut by_template: HashMap<String, i64> = HashMap::new();
    for (id, name, file, kind, value) in rows {
        match kind.as_str() {
            "selector" if !is_test_double(&name, &file) => {
                let selector = selector_value(conn, value)?.unwrap_or_default();
                parts.extend(selector_parts(&selector).into_iter().map(|p| (p, id)));
            }
            "template" => {
                by_template.entry(join_relative(&file, &value)).or_insert(id);
            }
            _ => {}
        }
    }
    let mut stmt = conn.prepare(
        "SELECT n.symbol_id, f.path, n.kind, n.value, n.extra, n.line FROM ng_facts n JOIN files f ON f.id = n.file_id
         WHERE n.kind IN ('renders', 'attr') ORDER BY f.path, n.line",
    )?;
    let uses: Vec<OwnedUse> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut out: HashMap<i64, Vec<NgChild>> = HashMap::new();
    for (sid, file, kind, value, extra, line) in uses {
        let Some(owner) = sid.or_else(|| by_template.get(&file).copied()) else { continue };
        let u = use_of(&kind, &value, extra.as_deref(), &file, line);
        let list = out.entry(owner).or_default();
        let matched: Vec<i64> = parts.iter().filter(|(p, _)| u.matched_by(p)).map(|(_, id)| *id).collect();
        if matched.is_empty() {
            list.push(NgChild { id: None, file, line, used: u });
            continue;
        }
        for child in matched {
            if child != owner && !list.iter().any(|c| c.id == Some(child)) {
                list.push(NgChild { id: Some(child), file: file.clone(), line, used: u.clone() });
            }
        }
    }
    Ok(out)
}

/// The component or directive whose selector is `selector` as a whole (`app-order-list`, `[appTooltip]`), leaving out
/// test doubles; the first by file when several declare it.
pub fn selector_entry(conn: &Connection, selector: &str) -> Result<Option<SymbolRow>> {
    let want = selector_parts(selector);
    let [want] = want.as_slice() else { return Ok(None) };
    for s in ng_selectors(conn)? {
        if s.part.element == want.element && s.part.attrs == want.attrs
            && let Some(d) = definitions(conn, &s.component)?.into_iter().find(|d| d.file == s.file && is_type_or_extension(&d.kind))
        {
            return Ok(Some(d));
        }
    }
    Ok(None)
}

/// The type a file stands for as an entry point: for an Angular template, the component whose `templateUrl` it is;
/// otherwise its first top-level component, or else its first top-level type. `file` is repo-relative, or its tail.
pub fn file_entry(conn: &Connection, file: &str) -> Result<Option<SymbolRow>> {
    let file = file.trim_start_matches("./");
    let path: Option<String> = conn
        .prepare_cached("SELECT path FROM files WHERE path = ?1 OR path LIKE '%/' || ?1 ORDER BY path = ?1 DESC, length(path), path LIMIT 1")?
        .query_row([file], |r| r.get(0))
        .optional()?;
    let Some(path) = path else { return Ok(None) };
    let sql = format!(
        "SELECT {SYMBOL_COLS}, n.value FROM ng_facts n JOIN symbols s ON s.id = n.symbol_id JOIN files f ON f.id = s.file_id
         WHERE n.kind = 'template' ORDER BY f.path, s.start_line"
    );
    if !crate::ts::is_ts_path(&path) {
        let mut stmt = conn.prepare_cached(&sql)?;
        let comps = stmt.query_map([], |r| Ok((symbol_row(r)?, r.get::<_, String>(SYMBOL_COLS.split(',').count())?)))?;
        for c in comps {
            let (s, url) = c?;
            if join_relative(&s.file, &url) == path {
                return Ok(Some(s));
            }
        }
    }
    let sql = format!(
        "SELECT {SYMBOL_COLS} FROM symbols s JOIN files f ON f.id = s.file_id
         WHERE f.path = ?1 AND s.parent_id IS NULL AND s.kind IN ('class', 'struct', 'enum', 'actor', 'protocol')
         ORDER BY NOT EXISTS(SELECT 1 FROM ng_facts n WHERE n.symbol_id = s.id AND n.kind IN ('selector', 'template')),
           s.kind != 'class', s.start_line LIMIT 1"
    );
    Ok(conn.prepare_cached(&sql)?.query_map([&path], symbol_row)?.next().transpose()?)
}

/// The components `sym_id` opens (`dialog.open(EditComponent)`): the class each `opens` fact names, resolved as the
/// reference at the same place is, kept when it is a component.
pub fn ng_opened(conn: &Connection, sym_id: i64) -> Result<Vec<RefHit>> {
    let sql = format!(
        "SELECT {REF_COLS} FROM refs r JOIN files f ON f.id = r.file_id LEFT JOIN symbols fs ON fs.id = r.from_id
         WHERE r.from_id = ?1 AND r.kind = 'type_ref' AND EXISTS(
           SELECT 1 FROM ng_facts n WHERE n.symbol_id = ?1 AND n.kind = 'opens' AND n.value = r.name AND n.line = r.line)
         ORDER BY r.line"
    );
    let mut hits: Vec<RefHit> = conn.prepare_cached(&sql)?.query_map([sym_id], ref_row)?.collect::<rusqlite::Result<_>>()?;
    if hits.is_empty() {
        return Ok(hits);
    }
    resolve_all(conn, &mut hits)?;
    let mut component = conn.prepare_cached("SELECT EXISTS(SELECT 1 FROM ng_facts WHERE symbol_id = ?1 AND kind IN ('selector', 'template'))")?;
    for h in &mut hits {
        let mut keep = Vec::new();
        for &id in &h.target_ids {
            if component.query_row([id], |r| r.get::<_, bool>(0))? {
                keep.push(id);
            }
        }
        h.target_ids = keep;
    }
    hits.retain(|h| !h.target_ids.is_empty());
    Ok(hits)
}

/// Uses of the given selector parts in this repo's templates, each with the component owning the template.
pub fn template_uses(conn: &Connection, parts: &[SelectorPart]) -> Result<Vec<NgUse>> {
    if parts.is_empty() {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT s.name, f.path, n.value FROM ng_facts n JOIN symbols s ON s.id = n.symbol_id JOIN files f ON f.id = n.file_id
         WHERE n.kind = 'template' ORDER BY f.path, s.start_line",
    )?;
    let mut by_template: HashMap<String, String> = HashMap::new();
    for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))? {
        let (name, file, url) = row?;
        by_template.entry(join_relative(&file, &url)).or_insert(name);
    }
    let mut stmt = conn.prepare_cached(
        "SELECT s.name, f.path, n.extra, n.line FROM ng_facts n JOIN files f ON f.id = n.file_id LEFT JOIN symbols s ON s.id = n.symbol_id
         WHERE n.kind = ?1 AND n.value = ?2 ORDER BY f.path, n.line",
    )?;
    let mut out: Vec<NgUse> = Vec::new();
    for part in parts {
        let Some((kind, value)) = part.key() else { continue };
        for row in stmt.query_map([kind, value], |r| {
            Ok((r.get::<_, Option<String>>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, u32>(3)?))
        })? {
            let (owner, file, extra, line) = row?;
            let mut u = use_of(kind, value, extra.as_deref(), &file, line);
            if !u.matched_by(part) || out.iter().any(|o| o.file == u.file && o.line == u.line && o.tag == u.tag) {
                continue;
            }
            u.tag = part.shown();
            // An inline template's owner is recorded; an .html file belongs to the component whose templateUrl it is.
            u.component = owner.or_else(|| by_template.get(&file).cloned());
            out.push(u);
        }
    }
    Ok(out)
}

/// A selector part a component or directive declares, for resolving template uses from other repos.
#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
pub struct NgSelector {
    pub part: SelectorPart,
    pub component: String,
    pub file: String,
    pub line: u32,
}

/// Every selector part of the repo's components and directives, leaving out test doubles.
pub fn ng_selectors(conn: &Connection) -> Result<Vec<NgSelector>> {
    let mut stmt = conn.prepare(
        "SELECT s.name, f.path, n.value, s.start_line FROM ng_facts n JOIN symbols s ON s.id = n.symbol_id
         JOIN files f ON f.id = n.file_id WHERE n.kind = 'selector'",
    )?;
    let mut out: Vec<NgSelector> = Vec::new();
    for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, u32>(3)?)))? {
        let (component, file, selector, line) = row?;
        let Some(selector) = selector_value(conn, selector)? else { continue };
        for part in selector_parts(&selector) {
            out.push(NgSelector { part, component: component.clone(), file: file.clone(), line });
        }
    }
    out.retain(|s| !is_test_double(&s.component, &s.file));
    out.sort_by_key(|s| (s.file.clone(), s.component.clone()));
    Ok(out)
}

/// Full paths of the routes showing `component`: each route's path in its file, under the paths of lazy routes that
/// load a module or routes declared in that file's directory.
fn ng_routes(conn: &Connection, component: &str) -> Result<Vec<String>> {
    let mut out: Vec<String> = ng_route_table(conn)?.into_iter().filter(|r| r.component == component).map(|r| r.path).collect();
    out.sort();
    out.dedup();
    Ok(out)
}

/// An Angular route showing a component: its full path (lazy prefixes included), the component's name, and where
/// the route is declared.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NgRoute {
    pub path: String,
    pub component: String,
    pub file: String,
    pub line: u32,
}

/// Every route of the repo that shows a component, one per full path: each route's path in its file, under the
/// paths of lazy routes that load a module or routes declared in that file's directory.
pub fn ng_route_table(conn: &Connection) -> Result<Vec<NgRoute>> {
    let mut stmt =
        conn.prepare("SELECT f.path, n.value, n.extra, n.line FROM ng_facts n JOIN files f ON f.id = n.file_id WHERE n.kind = 'route' ORDER BY f.path, n.line")?;
    let routes: Vec<(String, String, Option<String>, u32)> =
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<rusqlite::Result<_>>()?;
    let mut def = conn.prepare_cached("SELECT f.path FROM symbols s JOIN files f ON f.id = s.file_id WHERE s.name = ?1 ORDER BY f.path")?;
    // Lazy routes: (file, path, directory of what they load).
    let mut lazy = Vec::new();
    for (file, path, extra, _) in &routes {
        if let Some(target) = extra.as_deref().and_then(|e| e.strip_prefix("loads:")) {
            // `routes@./login/login.routes`: a routes constant, in the module imported.
            let (target, from) = match target.split_once('@') {
                Some((t, m)) => (t, Some(join_relative(file, m))),
                None => (target, None),
            };
            let mut files: Vec<String> = def.query_map([target], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
            if let Some(m) = from {
                files.retain(|f| f.rsplit_once('.').is_some_and(|(stem, _)| stem == m));
            }
            for f in files {
                lazy.push((file.clone(), path.clone(), dir_of(&f).to_string()));
            }
        }
    }
    fn prefixes(file: &str, lazy: &[(String, String, String)], depth: usize) -> Vec<String> {
        let parents: Vec<&(String, String, String)> = lazy.iter().filter(|(_, _, d)| d == dir_of(file)).collect();
        if parents.is_empty() || depth > 8 {
            return vec![String::new()];
        }
        parents
            .iter()
            .flat_map(|(f, p, _)| prefixes(f, lazy, depth + 1).into_iter().map(move |pre| join_route(&pre, p)))
            .collect()
    }
    let mut out: Vec<NgRoute> = Vec::new();
    for (file, path, extra, line) in &routes {
        let Some(component) = extra.as_deref().filter(|e| !e.starts_with("loads:")) else { continue };
        for pre in prefixes(file, &lazy, 0) {
            let r = NgRoute { path: join_route(&pre, path), component: component.to_string(), file: file.clone(), line: *line };
            if !out.contains(&r) {
                out.push(r);
            }
        }
    }
    Ok(out)
}

fn join_route(a: &str, b: &str) -> String {
    [a, b].iter().filter(|p| !p.is_empty()).copied().collect::<Vec<_>>().join("/")
}

fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

/// `rel` (`./x.html`, `../shared/y.html`) resolved against the directory of `file`.
fn join_relative(file: &str, rel: &str) -> String {
    let mut parts: Vec<&str> = dir_of(file).split('/').filter(|p| !p.is_empty()).collect();
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// A stub or mock: named so, or declared in a test file.
fn is_test_double(name: &str, file: &str) -> bool {
    ["Stub", "Mock", "Fake"].iter().any(|w| name.contains(w)) || file.contains(".spec.") || file.contains("/testing/")
}

/// Distinct modules imported by files that reference `name`.
pub fn imports_of_files_referencing(conn: &Connection, name: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare_cached(
        "SELECT DISTINCT i.module FROM imports i WHERE i.file_id IN (SELECT file_id FROM refs WHERE name = ?1) ORDER BY i.module",
    )?;
    let rows = stmt.query_map([name], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// One repo's imports, from [`imports`], grouped by module, to find those naming any of several module lists: each
/// list is matched against the distinct modules imported rather than against every import.
pub struct ImportSites<'a> {
    imports: &'a [(String, String, u32)],
    by_module: Vec<(&'a str, Vec<usize>)>,
}

impl<'a> ImportSites<'a> {
    pub fn new(imports: &'a [(String, String, u32)]) -> Self {
        let mut index: HashMap<&str, usize> = HashMap::new();
        let mut by_module: Vec<(&str, Vec<usize>)> = Vec::new();
        for (i, (_, m, _)) in imports.iter().enumerate() {
            let slot = *index.entry(m.as_str()).or_insert_with(|| {
                by_module.push((m.as_str(), Vec::new()));
                by_module.len() - 1
            });
            by_module[slot].1.push(i);
        }
        ImportSites { imports, by_module }
    }

    /// The imports that name one of `modules` or something inside it, in their original order.
    pub fn matching(&self, modules: &[String]) -> impl Iterator<Item = &'a (String, String, u32)> + use<'a> {
        let mut hits: Vec<usize> = self
            .by_module
            .iter()
            .filter(|(m, _)| modules.iter().any(|d| imports_module(m, d)))
            .flat_map(|(_, at)| at.iter().copied())
            .collect();
        hits.sort_unstable();
        let imports = self.imports;
        hits.into_iter().map(move |i| &imports[i])
    }
}

/// Whether an import names `module` or something inside it: `Acme.Common.Data.Models` is in `Acme.Common.Data`, and
/// `@acme/ui/button` is in `@acme/ui`.
pub fn imports_module(import: &str, module: &str) -> bool {
    import.strip_prefix(module).is_some_and(|rest| rest.is_empty() || rest.starts_with(['.', '/']))
}

/// Modules a repo's manifests declare (targets and products).
pub fn provided_modules(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT DISTINCT name COLLATE BINARY AS n FROM package_targets ORDER BY n")?;
    let rows = stmt.query_map([], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Resolves references that found nothing in their own repo against the public symbols of dependencies whose
/// modules the referencing file imports. Hits that resolve get targets tagged with the dependency's repo.
///
/// In TypeScript the import that brought in the name, or its receiver's type, names the one module it can come from:
/// only a dependency providing that module is searched. Otherwise every dependency the file imports is: when more
/// than one declares the name (and no single one matches the receiver's type exactly), the reference is ambiguous
/// between them rather than the first one's.
pub fn resolve_external(conn: &Connection, hits: &mut [RefHit], deps: &[Dependency]) -> Result<()> {
    if deps.is_empty() {
        return Ok(());
    }
    let mut imports: HashMap<String, Vec<String>> = HashMap::new();
    let mut oracles: Vec<TypeOracle> = deps.iter().map(|d| TypeOracle::new(d.conn)).collect();
    let mut own = TypeOracle::new(conn);
    let mut defs: HashMap<(usize, String), Vec<Def>> = HashMap::new();
    let mut scope = crate::scope::Scope::default();
    let mut modules = crate::ts::Modules::default();
    for hit in hits.iter_mut().filter(|h| matches!(h.confidence, "unresolved" | "external_likely")) {
        if !imports.contains_key(&hit.file) {
            let mut stmt = conn.prepare_cached("SELECT i.module FROM imports i JOIN files f ON f.id = i.file_id WHERE f.path = ?1")?;
            let mods: Vec<String> = stmt.query_map([&hit.file], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
            imports.insert(hit.file.clone(), mods);
        }
        let file_imports = imports[&hit.file].clone();
        if !deps.iter().any(|d| file_imports.iter().any(|m| d.modules.iter().any(|dm| imports_module(m, dm)))) {
            continue;
        }
        // A dependency's definitions in another language are never what a name means.
        let own_family = scope.lang(conn, &hit.file)?.map(|l| crate::scope::family(l).to_string());
        let own_family = own_family.as_deref();
        // Hits that found no in-repo definition skipped receiver evaluation; do it now.
        if hit.receiver_chain.is_empty() && hit.receiver_type.is_some() {
            match receiver(hit, &mut own)? {
                Receiver::Known(c) | Receiver::Foreign(c) => {
                    hit.receiver_type = c.first().cloned();
                    hit.receiver_chain = c;
                }
                Receiver::Unknown => hit.receiver_type = None,
            }
        }
        // TypeScript: the package the name, or its receiver's type, is imported from.
        let via = if crate::ts::is_ts_path(&hit.file) {
            let local = match (hit.receiver.as_deref(), hit.receiver_type.as_deref()) {
                (None, _) => hit.name.as_str(),
                (Some(_), Some(t)) => t,
                (Some(r), None) => r,
            };
            let local = local.split(['.', '<']).next().unwrap_or(local);
            modules.module_of(conn, &hit.file, local)?.filter(|m| !m.starts_with('.'))
        } else {
            None
        };
        // Per dependency declaring it: the confidence and the definitions picked.
        let mut found: Vec<(usize, &'static str, Vec<i64>)> = Vec::new();
        for (di, dep) in deps.iter().enumerate() {
            let provides = |m: &String| dep.modules.iter().any(|d| imports_module(m, d));
            let eligible = match &via {
                Some(m) => provides(m),
                None => file_imports.iter().any(provides),
            };
            if !eligible {
                continue;
            }
            let key = (di, hit.name.clone());
            if !defs.contains_key(&key) {
                defs.insert(key.clone(), public_defs_named(dep.conn, &hit.name)?);
            }
            let mut cands: Vec<&Def> = defs[&key]
                .iter()
                .filter(|d| compatible(&hit.kind, &d.kind) && own_family.is_none_or(|f| crate::scope::family(&d.lang) == f))
                .collect();
            // A C# extension method is in scope only where its declaring class's namespace is (a `using`, or the
            // file's own namespace or a parent of it).
            if hit.file.ends_with(".cs") {
                let mut visible = Vec::with_capacity(cands.len());
                for d in cands {
                    if !d.extension || own.vis.sees(conn, &hit.file, d.ns.as_deref(), d.declaring.as_deref().unwrap_or(""))? {
                        visible.push(d);
                    }
                }
                cands = visible;
            }
            if cands.is_empty() {
                continue;
            }
            let in_container = |c: &str| -> Vec<&Def> {
                // An exact container match beats a nested type that shares the name (`Song` vs `Migration.Song`).
        let exact: Vec<&Def> = cands.iter().copied().filter(|d| d.container.as_deref() == Some(c)).collect();
        if !exact.is_empty() {
            return exact;
        }
        cands.iter().copied().filter(|d| d.container.as_deref().is_some_and(|dc| dc.ends_with(&format!(".{c}")))).collect()
            };
            // A C# type written qualified (`EP.DeleteAccount`) keeps the qualifier as its receiver: a top-level type
            // whose namespace ends with it.
            let qualifier = (hit.file.ends_with(".cs") && matches!(hit.kind.as_str(), "type_ref" | "instantiate"))
                .then(|| hit.receiver.clone())
                .flatten();
            let receiver = if qualifier.is_some() { None } else { hit.receiver.as_deref() };
            let (confidence, picked): (&'static str, Vec<&Def>) = match (receiver, hit.receiver_type.as_deref()) {
                (_, Some(t)) => {
                    // This repo's view of the receiver's supertypes (`InboxView: View`), widened by the
                    // dependency's own (a dependency type's protocols).
                    let mut chain: Vec<String> = if hit.receiver_chain.is_empty() { vec![t.to_string()] } else { hit.receiver_chain.clone() };
                    for c in chain.clone() {
                        for sup in oracles[di].chain(&c)? {
                            if !chain.contains(&sup) {
                                chain.push(sup);
                            }
                        }
                    }
                    let mut found = Vec::new();
                    for c in chain {
                        found = in_container(&c);
                        if !found.is_empty() {
                            break;
                        }
                    }
                    if found.is_empty() {
                        // A framework type (`NavigationStack`) may conform to a framework protocol the dependency
                        // extends (`extension View`).
                        let ext: Vec<&Def> = cands.iter().copied().filter(|d| d.external_container).collect();
                        let (conf, _) = group_confidence(&ext);
                        (conf, ext)
                    } else {
                        ("exact", found)
                    }
                }
                (None, None) => {
                    let mut free: Vec<&Def> = cands.iter().copied().filter(|d| d.container.is_none()).collect();
                    if let Some(q) = &qualifier {
                        let named: Vec<&Def> =
                            free.iter().copied().filter(|d| crate::csharp::Visibility::qualifies(d.ns.as_deref(), None, q)).collect();
                        if !named.is_empty() {
                            free = named;
                        }
                    }
                    // Imported by name from this package (TypeScript): its one declaration of the name is the one.
                    let conf = match free.len() {
                        1 if via.is_some() && hit.receiver.is_none() => "exact",
                        1 => "unique_name",
                        _ => "ambiguous",
                    };
                    (conf, free)
                }
                (Some(r), None) if type_like(r) => {
                    let mut found = in_container(r);
                    if found.is_empty()
                        && let Some((head, _)) = r.split_once('.')
                    {
                        found = in_container(head);
                    }
                    ("exact", found)
                }
                _ => {
                    let implicit = hit.receiver.as_deref() == Some(".");
                    let key_path = hit.receiver.as_deref() == Some("\\");
                    let picked: Vec<&Def> = cands
                        .iter()
                        .copied()
                        .filter(|d| !implicit || d.kind == "case" || d.is_static)
                        .filter(|d| !key_path || matches!(d.kind.as_str(), "property" | "subscript"))
                        .collect();
                    let mut groups: Vec<Option<&str>> = picked.iter().map(|d| d.container.as_deref()).collect();
                    groups.sort();
                    groups.dedup();
                    (if groups.len() == 1 { "unique_name" } else { "ambiguous" }, picked)
                }
            };
            // Overloads: those taking as many arguments as the call passes, when any does.
            let mut picked = picked;
            if hit.kind == "call"
                && let Some(n) = hit.args
            {
                let fitting: Vec<&Def> = picked.iter().copied().filter(|d| d.accepts(n)).collect();
                if !fitting.is_empty() {
                    picked = fitting;
                }
            }
            if picked.is_empty() {
                continue;
            }
            found.push((di, confidence, picked.iter().map(|d| d.id).collect()));
        }
        // One dependency's answer; of several, the one matching the receiver's type exactly, else all of them.
        let exact: Vec<usize> = (0..found.len()).filter(|&i| found[i].1 == "exact").collect();
        let chosen: Vec<usize> = if found.len() > 1 && exact.len() == 1 { exact } else { (0..found.len()).collect() };
        let confidence = match chosen.as_slice() {
            [] => continue,
            [one] => found[*one].1,
            _ => {
                hit.basis = Some(AMBIGUOUS_DEPENDENCY);
                "ambiguous"
            }
        };
        hit.confidence = confidence;
        // Exact across a dependency: the name imported from its package, else the receiver's (or a named) type.
        let exact_by = if hit.receiver_type.is_none() && hit.receiver.is_none() { Mechanism::Import } else { Mechanism::ReceiverType };
        let assumed: Vec<crate::answer::Assumption> = chosen.iter().flat_map(|&i| deps[found[i].0].assumptions.iter().copied()).collect();
        hit.evidence = Evidence::by_confidence(confidence, exact_by).map(|e| e.assuming_all(&assumed));
        crate::answer::debug_check(|| format!("{}:{} {}", hit.file, hit.line, hit.name), confidence, hit.evidence.as_ref());
        hit.targets.clear();
        // Round-robin over the dependencies, so each shows among the first eight.
        let mut round = 0;
        let candidates: usize = chosen.iter().map(|&i| found[i].2.len()).sum();
        hit.targets_total = (candidates > TARGETS_SHOWN).then_some(candidates);
        while hit.targets.len() < TARGETS_SHOWN && chosen.iter().any(|&i| round < found[i].2.len()) {
            for &i in &chosen {
                let (di, _, ids) = &found[i];
                if let Some(&id) = ids.get(round)
                    && hit.targets.len() < TARGETS_SHOWN
                {
                    let mut t = target(deps[*di].conn, id)?;
                    t.repo = Some(deps[*di].repo.clone());
                    hit.targets.push(t);
                }
            }
            round += 1;
        }
    }
    Ok(())
}

/// Definitions visible from other modules: declared `public` or `open`, or members of a `public extension`. In
/// TypeScript, `export`ed declarations and the members of exported types that aren't `private` or `protected`.
fn public_defs_named(conn: &Connection, name: &str) -> Result<Vec<Def>> {
    let sql = format!(
        "SELECT {DEF_COLS}
         FROM symbols s JOIN files f ON f.id = s.file_id LEFT JOIN symbols p ON p.id = s.parent_id
         WHERE s.name = ?1 AND s.name = ?1 COLLATE BINARY
         AND (' ' || s.signature LIKE '% public %' OR ' ' || s.signature LIKE '% open %'
              OR (p.kind = 'extension' AND ' ' || p.signature LIKE '% public extension %')
              OR (f.lang = 'csharp' AND p.kind = 'protocol' AND ' ' || p.signature LIKE '% public %')
              OR (f.lang IN ('typescript', 'tsx', 'javascript')
                  AND ' ' || s.signature NOT LIKE '% private %' AND ' ' || s.signature NOT LIKE '% protected %'
                  AND (' ' || s.signature LIKE '% export %' OR ' ' || p.signature LIKE '% export %')))
         ORDER BY f.path, s.start_byte"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map([name], def_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Of `named`, the definitions code in `caller` can bind to by name: those in a language it sees, and in C#, in a
/// project it reaches (see [`crate::scope`]).
fn in_scope(conn: &Connection, scope: &mut crate::scope::Scope, caller: &str, named: &[Def]) -> Result<Vec<Def>> {
    let mut out = Vec::with_capacity(named.len());
    for d in named {
        if scope.sees(conn, caller, &d.file, &d.lang)? != crate::scope::Sight::No {
            out.push(d.clone());
        }
    }
    Ok(out)
}

/// Whether a C# binding of `caller` to `ids` rests on an assumption the project files can't confirm: the caller or a
/// target is in no project, or past a project reference the index can't place.
fn assumed(conn: &Connection, scope: &mut crate::scope::Scope, caller: &str, named: &[Def], ids: &[i64]) -> Result<bool> {
    for d in named.iter().filter(|d| ids.contains(&d.id)) {
        if scope.sees(conn, caller, &d.file, &d.lang)? == crate::scope::Sight::Assumed {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether a C# type reference resolved to one type (or its arities, `Request` and `Request<T>`) is the compiler's
/// pick because the scope ruled out the other types of that name (another language's, a project the caller doesn't
/// reference), as [`cs_namespaces`] narrowing to one is: the type is top-level and the file sees its namespace (or
/// names it, qualified).
fn scoped_out_type(
    all: &[Def],
    visible: &[Def],
    ids: &[i64],
    conn: &Connection,
    hit: &RefHit,
    qualifier: Option<&str>,
    vis: &mut crate::csharp::Visibility,
) -> Result<bool> {
    let is_type = |d: &&Def| TYPE_KINDS.contains(&d.kind.as_str()) || d.kind == "typealias";
    if ids.is_empty() || all.iter().filter(is_type).count() <= visible.iter().filter(is_type).count() {
        return Ok(false);
    }
    // One type, or its arities (`Request`, `Request<T>`): top-level, in one namespace.
    let picked: Vec<&Def> = visible.iter().filter(|d| ids.contains(&d.id)).filter(is_type).collect();
    if picked.len() != ids.len() || picked.iter().any(|d| d.container.is_some() || d.ns != picked[0].ns) {
        return Ok(false);
    }
    let Some(ns) = picked[0].ns.as_deref() else { return Ok(false) };
    Ok(match qualifier {
        Some(q) => crate::csharp::Visibility::qualifies(Some(ns), None, q),
        None => vis.seen_among(conn, &hit.file, &hit.name, &[ns.to_string()])?.iter().any(|n| n == ns),
    })
}

pub(crate) fn resolve_all(conn: &Connection, hits: &mut [RefHit]) -> Result<()> {
    resolve_hits(conn, hits, true)
}

/// [`resolve_all`] by the heuristics alone, ignoring any SCIP index: for comparing the two.
pub(crate) fn resolve_heuristic(conn: &Connection, hits: &mut [RefHit]) -> Result<()> {
    resolve_hits(conn, hits, false)
}

fn resolve_hits(conn: &Connection, hits: &mut [RefHit], use_scip: bool) -> Result<()> {
    let mut precise = if use_scip { Some(crate::scip::Precise::new(conn)?) } else { None };
    let mut defs: HashMap<String, Vec<Def>> = HashMap::new();
    let mut targets: HashMap<i64, Target> = HashMap::new();
    let mut oracle = TypeOracle::new(conn);
    let mut self_chains: HashMap<Option<String>, Vec<String>> = HashMap::new();
    // Per service, member, argument count and registration: the implementation's member it is bound to.
    type BindingKey = (String, String, Option<u32>, Option<String>);
    let mut bound: HashMap<BindingKey, Option<crate::di::Bound>> = HashMap::new();
    let mut nearest: HashMap<(String, String), Option<crate::di::Registration>> = HashMap::new();
    // Per C# service identity (name, type arguments, namespace): its registrations.
    let mut registered: HashMap<String, Vec<crate::di::Registration>> = HashMap::new();
    // TypeScript: per service and directory, the Angular provider in scope.
    let mut provided: HashMap<(String, String), Vec<crate::di::Registration>> = HashMap::new();
    // Per scope key and name: the definitions code there can see, and each one's position among them, by id.
    let mut scope = crate::scope::Scope::default();
    let mut visible: HashMap<(String, String), Vec<Def>> = HashMap::new();
    let mut positions: HashMap<(String, String), HashMap<i64, usize>> = HashMap::new();
    let mut cfml: Option<crate::cfml::Files> = None;
    let mut sql = crate::sql::Resolver::default();
    let mut ansible = crate::ansible::Resolver::default();
    let mut terraform = crate::terraform::Resolver::default();
    let mut modules = crate::ts::Modules::default();
    let mut py_modules = crate::py::Modules::default();
    // Per C# type, the constructors a creation of it may call.
    let mut ctor_sets: HashMap<i64, Option<CtorSet>> = HashMap::new();
    for hit in hits.iter_mut() {
        hit.ctors = None;
        hit.via_di = None;
        hit.via_di_any.clear();
        hit.precise = false;
        hit.corrected_from = None;
        hit.basis = None;
        hit.evidence = None;
        hit.targets_total = None;
        // The receiver's type as recorded, before resolution narrows it to a bare name: a generic service's arguments.
        let written_receiver = hit.receiver_type.clone();
        // CFML, SQL, Ansible and Terraform resolve by their own rules: paths and includes, schemas, roles and scopes,
        // configuration directories.
        let own = if crate::ansible::handles(&hit.kind) {
            Some((ansible.resolve(conn, hit)?, Mechanism::Convention))
        } else if crate::terraform::handles(&hit.kind) {
            Some((terraform.resolve(conn, hit)?, Mechanism::Convention))
        } else if crate::cfml::handles(&hit.file) {
            if cfml.is_none() {
                cfml = Some(crate::cfml::Files::load(conn)?);
            }
            let files = cfml.as_mut().expect("loaded");
            Some((files.resolve(conn, &hit.file, &hit.name, &hit.kind, hit.receiver.as_deref(), hit.receiver_type.as_deref())?, Mechanism::Import))
        } else if crate::sql::is_sql_path(&hit.file) {
            Some((sql.resolve(conn, hit)?, Mechanism::SqlName))
        } else if hit.kind == "call" && hit.name == "init" && hit.file.ends_with(".cs") {
            chained_constructor(conn, &mut oracle, hit, &mut ctor_sets)?.map(|r| (r, Mechanism::ReceiverType))
        } else {
            None
        };
        if let Some(((confidence, ids), mechanism)) = own {
            hit.confidence = confidence;
            // Ansible roles and Terraform scopes are placed by their directories.
            hit.evidence = Evidence::by_confidence(confidence, mechanism).map(|e| match mechanism {
                Mechanism::Convention => e.assuming(Assumption::DirectoryConvention),
                _ => e,
            });
            hit.targets.clear();
            hit.target_ids = ids.clone();
            hit.targets_total = (ids.len() > TARGETS_SHOWN).then_some(ids.len());
            for id in ids.into_iter().take(TARGETS_SHOWN) {
                if let std::collections::hash_map::Entry::Vacant(e) = targets.entry(id) {
                    e.insert(target(conn, id)?);
                }
                hit.targets.push(targets[&id].clone());
            }
            continue;
        }
        if !defs.contains_key(&hit.name) {
            // A SQL function is never what code in another language calls by name.
            let named: Vec<Def> = defs_named(conn, &hit.name)?.into_iter().filter(|d| !crate::sql::is_sql_path(&d.file)).collect();
            defs.insert(hit.name.clone(), named);
        }
        let ts = crate::ts::is_ts_path(&hit.file);
        let cs = hit.file.ends_with(".cs");
        // In TypeScript an imported name comes from its module, and a bare one from the file itself; in Python a name
        // `from m import`ed, or a member of an `import`ed module, comes from that module.
        let scoped = if ts {
            ts_scoped(conn, hit, &mut modules, &mut defs)?
        } else if hit.file.ends_with(".py") {
            py_scoped(conn, hit, &mut py_modules, &mut defs)?
        } else {
            None
        };
        let seen_key = (scope.key(conn, &hit.file)?, hit.name.clone());
        if !visible.contains_key(&seen_key) {
            let v = in_scope(conn, &mut scope, &hit.file, &defs[&hit.name])?;
            visible.insert(seen_key.clone(), v);
        }
        let named = &visible[&seen_key];
        // A C# type written qualified (`Models.Domain`) keeps its qualifier as the receiver; it is resolved as the bare
        // name, then narrowed by the qualifier.
        let cs_type = cs && matches!(hit.kind.as_str(), "type_ref" | "instantiate");
        let qualifier = if cs_type { hit.receiver.take() } else { None };
        let mut seen_ns: Option<Vec<String>> = None;
        let by_import = scoped.is_some();
        let mut receiver_known = false;
        let (mut confidence, mut ids) = if let Some(r) = scoped {
            r
        } else if !named.iter().any(|d| compatible(&hit.kind, &d.kind)) {
            ("unresolved", Vec::new())
        } else {
            oracle.file = ts.then(|| hit.file.clone());
            oracle.here = cs.then(|| hit.file.clone());
            oracle.caller = hit.caller_type();
            let mut recv = if cs && !cs_type {
                let (recv, ns) = cs_receiver(hit, &mut oracle)?;
                seen_ns = ns;
                recv
            } else {
                receiver(hit, &mut oracle)?
            };
            if ts {
                recv = ts_receiver(conn, hit, recv, &mut oracle, &mut modules)?;
            }
            hit.receiver_type = match &recv {
                Receiver::Known(c) | Receiver::Foreign(c) => c.first().cloned(),
                Receiver::Unknown => None,
            };
            receiver_known = !matches!(recv, Receiver::Unknown);
            if let Receiver::Known(c) | Receiver::Foreign(c) = &recv {
                hit.receiver_chain = c.clone();
            }
            let caller = hit.caller_type();
            if !self_chains.contains_key(&caller) {
                let mut chain = Vec::new();
                let mut s = caller.clone();
                while let Some(cur) = s {
                    s = cur.rsplit_once('.').map(|(p, _)| p.to_string());
                    chain.push(cur);
                }
                if let Some(c) = &caller {
                    for sup in oracle.chain(c)?.into_iter().skip(1) {
                        if !chain.contains(&sup) {
                            chain.push(sup);
                        }
                    }
                }
                self_chains.insert(caller.clone(), chain);
            }
            let usable: Vec<&Def> = match (&seen_ns, &recv) {
                // The receiver's type is one of several same-named ones: never the others' members.
                (Some(seen), Receiver::Known(chain) | Receiver::Foreign(chain)) if !chain.is_empty() => {
                    let ty = chain[0].as_str();
                    named
                        .iter()
                        .filter(|d| {
                            let on_type = d.container.as_deref().is_some_and(|c| last_segment(c) == ty) && !d.extension;
                            !on_type || d.ns.as_ref().is_some_and(|n| seen.contains(n))
                        })
                        .collect()
                }
                _ => named.iter().collect(),
            };
            // Of same-named C# extension methods, those whose declaring class's namespace the file sees. When it
            // sees none, they all stay: the file may rely on a `global using`, which isn't read.
            let usable = if cs && hit.receiver.is_some() && usable.iter().filter(|d| d.extension).nth(1).is_some() {
                let mut visible = Vec::with_capacity(usable.len());
                for d in &usable {
                    if !d.extension || oracle.vis.sees(conn, &hit.file, d.ns.as_deref(), d.declaring.as_deref().unwrap_or(""))? {
                        visible.push(*d);
                    }
                }
                if visible.iter().any(|d| d.extension) { visible } else { usable }
            } else {
                usable
            };
            // Overloads the argument types rule out.
            let usable = if cs && hit.kind == "call" && hit.arg_types.is_some() && usable.len() > 1 {
                narrow_by_argument_types(&mut oracle, hit, usable)?
            } else {
                usable
            };
            resolve(hit, &recv, &self_chains[&caller], &usable)
        };
        // How an exact answer was reached, recorded where it is decided.
        let mut exact_by = if by_import {
            Mechanism::Import
        } else if receiver_known
            || matches!(hit.receiver.as_deref(), Some("self" | "Self" | "super" | "this" | "base"))
            || hit.receiver.as_deref().is_some_and(type_like)
            || qualifier.is_some()
        {
            Mechanism::ReceiverType
        } else {
            Mechanism::LexicalScope
        };
        if cs && ids.len() > 1 || cs_type && !ids.is_empty() {
            if !positions.contains_key(&seen_key) {
                positions.insert(seen_key.clone(), named.iter().enumerate().map(|(i, d)| (d.id, i)).collect());
            }
            let before = confidence;
            (confidence, ids) =
                cs_namespaces(conn, hit, qualifier.as_deref(), named, &positions[&seen_key], (confidence, ids), &mut oracle.vis)?;
            if before != "exact" && confidence == "exact" {
                exact_by = Mechanism::Namespace;
            }
        }
        if cs_type && matches!(confidence, "unique_name" | "ambiguous") && scoped_out_type(&defs[&hit.name], named, &ids, conn, hit, qualifier.as_deref(), &mut oracle.vis)? {
            confidence = "exact";
            exact_by = Mechanism::ProjectScope;
        }
        // Members of same-named types in different namespaces (a receiver typed by a name its file's imports don't
        // settle): one of them, not every one.
        if cs && confidence == "exact" && ids.len() > 1 {
            let owners: HashSet<(Option<&str>, Option<&str>)> =
                named.iter().filter(|d| ids.contains(&d.id)).map(|d| (d.container.as_deref(), d.ns.as_deref())).collect();
            if owners.len() > 1 {
                confidence = "ambiguous";
            }
        }
        let mut evidence = Evidence::by_confidence(confidence, exact_by);
        if hit.basis == Some(GUESSED_MODULE) {
            evidence = evidence.map(|_| Evidence::new(Mechanism::FilenameGuess));
        }
        if cs && confidence == "exact" && assumed(conn, &mut scope, &hit.file, named, &ids)? {
            confidence = "unique_name";
            // The binding's own evidence stands; the project files couldn't confirm the caller sees the target.
            evidence = evidence.map(|e| e.assuming(Assumption::ProjectUnverified));
        }
        if cs_type {
            hit.receiver = qualifier;
        }
        // A SCIP index covering the site has the compiler's answer: it wins.
        if let Some(p) = precise.as_mut()
            && let Some(v) = p.verdict(conn, &hit.file, hit.line, &hit.name)?
        {
            hit.precise = true;
            hit.basis = None;
            evidence = None;
            match v {
                crate::scip::Verdict::Target(t) => {
                    if matches!(crate::scip::classify(&ids, v), "other" | "missed") {
                        hit.corrected_from = Some(confidence);
                    }
                    (confidence, ids) = ("exact", vec![t]);
                    evidence = Some(Evidence::new(Mechanism::Scip));
                }
                // Already said to be outside the repo.
                crate::scip::Verdict::External if confidence == "external_likely" => {}
                crate::scip::Verdict::External => {
                    if !ids.is_empty() {
                        hit.corrected_from = Some(confidence);
                    }
                    (confidence, ids) = ("unresolved", Vec::new());
                }
            }
        }
        hit.confidence = confidence;
        hit.evidence = if matches!(confidence, "unresolved" | "external_likely") { None } else { evidence };
        crate::answer::debug_check(|| format!("{}:{} {}", hit.file, hit.line, hit.name), confidence, hit.evidence.as_ref());
        hit.targets.clear();
        hit.target_ids = ids.clone();
        hit.targets_total = (ids.len() > TARGETS_SHOWN).then_some(ids.len());
        for id in ids.into_iter().take(TARGETS_SHOWN) {
            if let std::collections::hash_map::Entry::Vacant(e) = targets.entry(id) {
                e.insert(target(conn, id)?);
            }
            hit.targets.push(targets[&id].clone());
        }
        // A creation of one C# type: the constructors its arguments may call. A SCIP index names the one called.
        if cs && hit.kind == "instantiate" && hit.target_ids.len() == 1 {
            let ty = hit.target_ids[0];
            if let std::collections::hash_map::Entry::Vacant(e) = ctor_sets.entry(ty) {
                e.insert(CtorSet::of(conn, ty)?);
            }
            if let Some(set) = &ctor_sets[&ty] {
                let precise_ctor = match precise.as_mut() {
                    Some(p) if hit.precise => p.constructor(conn, &hit.file, hit.line, &hit.name)?,
                    _ => None,
                };
                hit.ctors = Some(match precise_ctor {
                    Some(c) => vec![c],
                    None => constructors_called(&mut oracle, hit, set)?,
                });
            }
        }
        // A call through a service type: the implementation the container binds it to.
        if hit.kind == "call"
            && matches!(confidence, "exact" | "unique_name")
            && hit.target_ids.len() == 1
            && hit.file.ends_with(".cs")
            && let Some((service, _)) = hit.targets[0].qualified.rsplit_once('.')
        {
            let service = service.rsplit('.').next().unwrap_or(service).to_string();
            // A generic service is one service per type arguments (`IHandler<Order>`, `IHandler<Invoice>`), and
            // same-named services in two namespaces are two services.
            let args = service_args(&mut oracle, hit, written_receiver.as_deref(), &service)?;
            let ns = named.iter().find(|d| d.id == hit.target_ids[0]).and_then(|d| d.ns.clone());
            let identity = match &args {
                Some(a) => format!("{service}<{}>@{}", a.iter().map(|a| a.as_deref().unwrap_or("?")).collect::<Vec<_>>().join(","), ns.as_deref().unwrap_or("")),
                None => format!("{service}@{}", ns.as_deref().unwrap_or("")),
            };
            if !registered.contains_key(&identity) {
                let regs = if crate::di::any(conn)? {
                    service_registrations(conn, &mut oracle.vis, &service, args.as_deref(), ns.as_deref())?
                } else {
                    Vec::new()
                };
                registered.insert(identity.clone(), regs);
            }
            // A repo hosting several applications registers a service in each: the registration of the application
            // the call is in, whose project decides which of same-named implementations it binds.
            let near_key = (identity.clone(), hit.file.clone());
            if !nearest.contains_key(&near_key) {
                nearest.insert(near_key.clone(), crate::di::nearest(registered[&identity].clone(), &hit.file));
            }
            let near = nearest[&near_key].clone();
            let at = near.as_ref().map(|r| format!("{}:{}", r.file, r.line));
            let key = (identity.clone(), hit.name.clone(), hit.args, at);
            if !bound.contains_key(&key) {
                let b = match crate::di::single(registered[&identity].clone(), &service) {
                    Some(reg) => {
                        let reg = near.clone().unwrap_or(reg);
                        bound_member(conn, &mut oracle, &reg, &service, &hit.name, hit.args, hit.target_ids[0])?
                    }
                    None => None,
                };
                bound.insert(key.clone(), b);
            }
            hit.via_di = bound[&key].clone();
        } else if ts
            && hit.kind == "call"
            // A call on the object itself runs its own class's member: no injector is involved.
            && !matches!(hit.receiver.as_deref(), None | Some("this" | "super"))
            && let Some((service, library)) = ts_service(hit, confidence)
            && (!library || crate::di::provides(conn, &service)?)
        {
            // An abstract class, an interface an `InjectionToken` is typed with, a class re-provided, or a library's
            // type the repo provides (`{ provide: ErrorHandler, useClass: … }`): the implementation the Angular
            // provider in scope hands out.
            let dir = hit.file.rsplit_once('/').map_or("", |(d, _)| d).to_string();
            let caller = hit.caller_type().map(|c| c.rsplit('.').next().unwrap_or(&c).to_string());
            let near_key = (service.clone(), format!("{dir}|{}", caller.as_deref().unwrap_or("")));
            if !provided.contains_key(&near_key) {
                let r = if crate::di::any(conn)? { crate::di::ts_bound(conn, &service, &hit.file, caller.as_deref())? } else { Vec::new() };
                provided.insert(near_key.clone(), r);
            }
            let mut members: Vec<crate::di::Bound> = Vec::new();
            for reg in provided[&near_key].clone() {
                let at = format!("{}:{} {}", reg.file, reg.line, reg.implementation.as_deref().unwrap_or(""));
                let key = (service.clone(), hit.name.clone(), hit.args, Some(at));
                if !bound.contains_key(&key) {
                    let b = bound_member(conn, &mut oracle, &reg, &key.0, &hit.name, hit.args, hit.target_ids.first().copied().unwrap_or(-1))?;
                    bound.insert(key.clone(), b);
                }
                if let Some(mut b) = bound[&key].clone().filter(|b| !members.iter().any(|m| m.target.id == b.target.id)) {
                    // Placed here by its directory, not by the injector tree the code declares.
                    if reg.by_directory {
                        b.assumptions = vec![crate::answer::Assumption::DirectoryConvention];
                    }
                    members.push(b);
                }
            }
            // A library's member the repo implements: what the call reaches is the implementation.
            if library && !members.is_empty() {
                hit.confidence = if members.len() == 1 { "unique_name" } else { "ambiguous" };
                hit.evidence = Some(Evidence::new(Mechanism::DiBinding));
            }
            // Providers in several scopes: any of them may run, none is picked.
            if members.len() == 1 {
                hit.via_di = members.pop();
            } else {
                hit.via_di_any = members;
            }
        }
    }
    Ok(())
}

/// The type arguments of the generic C# service `service` a call's receiver has (`["Order"]` for an
/// `IHandler<Order> handler`), by bare name, `None` each where not known; `None` when the receiver's type isn't known
/// to be that service closed over arguments.
fn service_args(oracle: &mut TypeOracle, hit: &RefHit, written: Option<&str>, service: &str) -> Result<Option<Vec<Option<String>>>> {
    let Some(enc) = written else { return Ok(None) };
    Ok(match oracle.eval_encoded(enc, hit.caller_type().as_deref())? {
        Some(TypeExpr::Generic { name, args }) if last_segment(&name) == service => Some(
            args.iter()
                .map(|a| match a {
                    Some(TypeExpr::Named(n) | TypeExpr::Generic { name: n, .. }) => Some(last_segment(n).to_string()),
                    _ => None,
                })
                .collect(),
        ),
        _ => None,
    })
}

/// The registrations of the C# service a call goes through: `service` (a bare name) closed over `args` when they are
/// known (see [`crate::di::closes`]), and, when the repo declares types of that name in several namespaces, the one in
/// `ns`: a registration writing it qualified names that namespace, else its file sees it.
fn service_registrations(
    conn: &Connection,
    vis: &mut crate::csharp::Visibility,
    service: &str,
    args: Option<&[Option<String>]>,
    ns: Option<&str>,
) -> Result<Vec<crate::di::Registration>> {
    let mut regs = crate::di::registrations_of(conn, service)?;
    if let Some(args) = args {
        regs.retain(|r| r.service.as_deref().is_none_or(|w| crate::di::closes(w, args)));
    }
    let several: bool = conn
        .prepare_cached(
            "SELECT COUNT(DISTINCT json_extract(type_name, '$.ns')) > 1 FROM symbols
             WHERE name = ?1 AND name = ?1 COLLATE BINARY AND kind IN ('class', 'struct', 'protocol') AND json_valid(type_name)",
        )?
        .query_row([service], |r| r.get(0))?;
    let Some(ns) = ns.filter(|_| several) else { return Ok(regs) };
    let mut kept = Vec::with_capacity(regs.len());
    for r in regs {
        let head = r.service.as_deref().map(|w| w.split('<').next().unwrap_or(w).trim());
        let ours = match head.and_then(|h| h.rsplit_once('.')) {
            Some((qualifier, _)) => crate::csharp::Visibility::qualifies(Some(ns), None, qualifier),
            None => vis.sees(conn, &r.file, Some(ns), service)?,
        };
        if ours {
            kept.push(r);
        }
    }
    Ok(kept)
}

/// The service a TypeScript call goes through, by bare name, and whether it is a library's: the type of its one
/// confident target, or else, for a call no definition in the repo answers, its receiver's type (`ErrorHandler`).
fn ts_service(hit: &RefHit, confidence: &str) -> Option<(String, bool)> {
    if matches!(confidence, "exact" | "unique_name") && hit.target_ids.len() == 1 {
        let (service, _) = hit.targets[0].qualified.rsplit_once('.')?;
        return Some((service.rsplit('.').next().unwrap_or(service).to_string(), false));
    }
    if hit.target_ids.is_empty() && matches!(confidence, "unresolved" | "external_likely") {
        let t = hit.receiver_type.as_deref().filter(|t| !t.starts_with(['{', '"']))?;
        return Some((crate::di::bare_name(t), true));
    }
    None
}

/// Narrows a C# reference's candidates by namespace, as the compiler would:
///
/// - a type written unqualified is one the file sees (its own namespace or a parent, or a `using`), and a type the
///   repo declares only where the file can't see is a library's of the same name (`unresolved`); written qualified,
///   one whose namespace or enclosing type ends with the qualifier;
/// - an unqualified call matching the caller's own type by name (two `Parser` classes in two namespaces) is the one
///   in the caller's namespace.
fn cs_namespaces(
    conn: &Connection,
    hit: &RefHit,
    qualifier: Option<&str>,
    named: &[Def],
    positions: &HashMap<i64, usize>,
    (confidence, ids): (&'static str, Vec<i64>),
    vis: &mut crate::csharp::Visibility,
) -> Result<(&'static str, Vec<i64>)> {
    let picked: Vec<&Def> = ids.iter().filter_map(|id| positions.get(id).map(|&i| &named[i])).collect();
    if picked.len() != ids.len() {
        return Ok((confidence, ids));
    }
    let same_type = |ds: &[&Def]| ds.windows(2).all(|w| w[0].ns == w[1].ns && w[0].container == w[1].container);
    let narrowed = |kept: Vec<&Def>, confidence: &'static str| -> (&'static str, Vec<i64>) {
        let conf = if same_type(&kept) { "exact" } else { confidence };
        (conf, kept.iter().map(|d| d.id).collect())
    };
    if matches!(hit.kind.as_str(), "type_ref" | "instantiate") {
        let types: Vec<&Def> = picked.iter().copied().filter(|d| TYPE_KINDS.contains(&d.kind.as_str()) || d.kind == "typealias").collect();
        if types.len() != picked.len() {
            return Ok((confidence, ids));
        }
        if let Some(q) = qualifier {
            let kept: Vec<&Def> =
                types.iter().copied().filter(|d| crate::csharp::Visibility::qualifies(d.ns.as_deref(), d.container.as_deref(), q)).collect();
            if !kept.is_empty() && kept.len() < types.len() {
                return Ok(narrowed(kept, confidence));
            }
            // `Inventory.Models.Domain` names a namespace the repo doesn't declare: a package's type.
            if kept.is_empty() && q.contains('.') {
                return Ok(("unresolved", Vec::new()));
            }
            return Ok((confidence, ids));
        }
        let mut namespaces: Vec<String> = types.iter().filter(|d| d.container.is_none()).filter_map(|d| d.ns.clone()).collect();
        namespaces.sort();
        namespaces.dedup();
        let seen = vis.seen_among(conn, &hit.file, &hit.name, &namespaces)?;
        let kept: Vec<&Def> = types
            .iter()
            .copied()
            .filter(|d| d.container.is_some() || d.ns.as_ref().is_none_or(|n| seen.contains(n)))
            .collect();
        if kept.is_empty() {
            return Ok(if types.iter().all(|d| d.ns.is_some()) { ("unresolved", Vec::new()) } else { (confidence, ids) });
        }
        if kept.len() < types.len() {
            return Ok(narrowed(kept, confidence));
        }
        return Ok((confidence, ids));
    }
    if matches!(hit.receiver.as_deref(), None | Some("this")) && !same_type(&picked) {
        let own = vis.namespaces(conn, &hit.file)?;
        let kept: Vec<&Def> = picked.iter().copied().filter(|d| d.ns.as_ref().is_some_and(|n| own.contains(n))).collect();
        if !kept.is_empty() && kept.len() < picked.len() {
            return Ok(narrowed(kept, confidence));
        }
    }
    Ok((confidence, ids))
}

/// Whether a top-level TypeScript declaration of kind `def_kind` can be what a `ref_kind` reference names: a module's
/// constants and functions are values (`SetCart(…)`, `ofType(SetCart)`, `inject(TOKEN)`) as much as its classes.
fn ts_top_level_compatible(ref_kind: &str, def_kind: &str) -> bool {
    match ref_kind {
        "type_ref" | "member" => !matches!(def_kind, "init" | "case" | "method"),
        "instantiate" => matches!(def_kind, "class" | "function" | "property"),
        "call" => matches!(def_kind, "function" | "property"),
        _ => false,
    }
}

/// The TypeScript import rules' answer for `hit`, when they have one: an imported name resolves in its module's file
/// (or outside the repo, for a package), and a name neither imported nor a member resolves in its own file.
fn ts_scoped(
    conn: &Connection,
    hit: &mut RefHit,
    modules: &mut crate::ts::Modules,
    defs: &mut HashMap<String, Vec<Def>>,
) -> Result<Option<(&'static str, Vec<i64>)>> {
    if !matches!(hit.kind.as_str(), "call" | "type_ref" | "instantiate" | "member") {
        return Ok(None);
    }
    let typed = hit.receiver_type.is_some() && !hit.receiver.as_deref().is_some_and(type_like);
    let top = |d: &&Def, name_kind: &str| d.container.is_none() && ts_top_level_compatible(name_kind, &d.kind);
    match modules.scope(conn, &hit.file, &hit.name, hit.receiver.as_deref(), typed)? {
        Some(crate::ts::Scope::External) => Ok(Some(("unresolved", Vec::new()))),
        Some(crate::ts::Scope::Unplaced) => {
            hit.basis = Some(UNPLACED_MODULE);
            Ok(Some(("unresolved", Vec::new())))
        }
        Some(crate::ts::Scope::Module { files, guessed, name }) => {
            if !defs.contains_key(&name) {
                defs.insert(name.clone(), defs_named(conn, &name)?);
            }
            let named = &defs[&name];
            let found: Vec<&Def> = named.iter().filter(|d| top(d, &hit.kind) && files.contains(&d.file)).collect();
            let placed: Vec<i64> = found.iter().filter(|d| !guessed.contains(&d.file)).map(|d| d.id).collect();
            if !placed.is_empty() {
                return Ok(Some(("exact", placed)));
            }
            // Only a module placed by guesswork declares it: never exact.
            if !found.is_empty() {
                hit.basis = Some(GUESSED_MODULE);
                let ids: Vec<i64> = found.iter().map(|d| d.id).collect();
                return Ok(Some((if ids.len() == 1 { "unique_name" } else { "ambiguous" }, ids)));
            }
            // A re-export the index can't follow: any top-level declaration of the name.
            let free: Vec<&Def> = named.iter().filter(|d| top(d, &hit.kind)).collect();
            Ok(Some(match free.len() {
                0 => ("unresolved", Vec::new()),
                1 => ("unique_name", vec![free[0].id]),
                _ => ("ambiguous", free.iter().map(|d| d.id).collect()),
            }))
        }
        None if hit.receiver.is_none() && hit.kind != "member" => {
            let own: Vec<i64> = defs[&hit.name].iter().filter(|d| top(d, &hit.kind) && d.file == hit.file).map(|d| d.id).collect();
            Ok((!own.is_empty()).then_some(("exact", own)))
        }
        None => Ok(None),
    }
}

/// The Python import rules' answer for `hit`, when they have one: a name `from m import`ed, or a member of an
/// `import`ed module, is a top-level declaration of that module (exact when the module is certain), or outside the repo
/// when the module is.
fn py_scoped(
    conn: &Connection,
    hit: &RefHit,
    modules: &mut crate::py::Modules,
    defs: &mut HashMap<String, Vec<Def>>,
) -> Result<Option<(&'static str, Vec<i64>)>> {
    if !matches!(hit.kind.as_str(), "call" | "type_ref" | "instantiate" | "member") {
        return Ok(None);
    }
    match modules.scope(conn, &hit.file, &hit.name, hit.receiver.as_deref())? {
        Some(crate::py::Scope::External) => Ok(Some(("unresolved", Vec::new()))),
        Some(crate::py::Scope::Module { files, name, sure }) => {
            if !defs.contains_key(&name) {
                defs.insert(name.clone(), defs_named(conn, &name)?);
            }
            let top = |d: &&Def| d.container.is_none() && compatible(&hit.kind, &d.kind);
            let found: Vec<i64> = defs[&name].iter().filter(|d| top(d) && files.contains(&d.file)).map(|d| d.id).collect();
            Ok(match found.len() {
                // Not declared where the import says (assigned, or re-exported in a way not followed): the name rules.
                0 => None,
                1 if !sure => Some(("unique_name", found)),
                _ if !sure => Some(("ambiguous", found)),
                _ => Some(("exact", found)),
            })
        }
        None => Ok(None),
    }
}

/// A TypeScript receiver's type, corrected by the file's imports: a type imported from a package is the package's even
/// when the repo has one of that name, and a member chain through a package's type stays in the package.
fn ts_receiver(
    conn: &Connection,
    hit: &RefHit,
    recv: Receiver,
    oracle: &mut TypeOracle,
    modules: &mut crate::ts::Modules,
) -> Result<Receiver> {
    Ok(match recv {
        Receiver::Known(chain) if modules.imports_external(conn, &hit.file, &chain[0])? => Receiver::Foreign(vec![chain[0].clone()]),
        Receiver::Unknown => match hit.receiver_type.as_deref() {
            Some(enc) => match oracle.foreign_prefix(enc, hit.caller_type().as_deref())? {
                Some(t) => Receiver::Foreign(vec![t]),
                None => Receiver::Unknown,
            },
            None => Receiver::Unknown,
        },
        r => r,
    })
}

/// The member named `name` that `reg`'s implementation (or a supertype of it other than the service) defines: of its
/// overloads, the first taking `args` arguments. When several projects declare an implementation of that name, the
/// one the registering project can use (itself or a project it references).
fn bound_member(
    conn: &Connection,
    oracle: &mut TypeOracle,
    reg: &crate::di::Registration,
    service: &str,
    name: &str,
    args: Option<u32>,
    service_member: i64,
) -> Result<Option<crate::di::Bound>> {
    let Some(implementation) = reg.implementation.as_deref() else { return Ok(None) };
    let defs = defs_named(conn, name)?;
    for ty in oracle.chain(implementation)? {
        if ty == service {
            continue;
        }
        let mut on_type: Vec<&Def> = defs
            .iter()
            .filter(|d| {
                d.id != service_member
                    && matches!(d.kind.as_str(), "method" | "function" | "property")
                    && d.container.as_deref().is_some_and(|c| c == ty || c.ends_with(&format!(".{ty}")))
            })
            .collect();
        if on_type.iter().any(|d| d.file != on_type[0].file)
            && let Some(reachable) = oracle.vis.reachable_projects(conn, &reg.file)?
        {
            let mut usable = Vec::new();
            for d in &on_type {
                if oracle.vis.project_of(conn, &d.file)?.is_some_and(|p| reachable.contains(&p)) {
                    usable.push(*d);
                }
            }
            if !usable.is_empty() {
                on_type = usable;
            }
        }
        let found = on_type.iter().find(|d| args.is_none_or(|n| d.accepts(n))).or(on_type.first());
        if let Some(d) = found {
            return Ok(Some(crate::di::Bound {
                target: target(conn, d.id)?,
                lifetime: reg.lifetime.clone(),
                registered: format!("{}:{}", reg.file, reg.line),
                assumptions: Vec::new(),
            }));
        }
    }
    Ok(None)
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RefSummary {
    pub total: usize,
    pub by_confidence: BTreeMap<&'static str, usize>,
    /// Calls through a service type that also reach the implementation the container binds (`via_di`).
    #[serde(skip_serializing_if = "is_zero_usize")]
    pub via_di: usize,
    /// References a SCIP index resolved (`precise`), and those of them it corrected.
    #[serde(skip_serializing_if = "is_zero_usize")]
    pub precise: usize,
    #[serde(skip_serializing_if = "is_zero_usize")]
    pub corrected: usize,
}

fn is_zero_usize(n: &usize) -> bool {
    *n == 0
}

/// References to a name, resolved. `kinds` filters by reference kind (`call`, `instantiate`, `type_ref`).
pub fn refs(conn: &Connection, query: &str, kinds: &[&str], limit: usize) -> Result<(Vec<RefHit>, RefSummary)> {
    refs_with(conn, query, kinds, limit, &[])
}

/// [`refs`], also resolving references that leave the repo against `deps`.
pub fn refs_with(
    conn: &Connection,
    query: &str,
    kinds: &[&str],
    limit: usize,
    deps: &[Dependency],
) -> Result<(Vec<RefHit>, RefSummary)> {
    refs_filtered(conn, query, kinds, limit, deps, false)
}

/// [`refs_with`], keeping only references in files that import one of `deps`' modules: the only ones that can
/// resolve into `deps`. The others are never resolved, which is most of the work when the name is common.
pub fn refs_importing(conn: &Connection, query: &str, kinds: &[&str], deps: &[Dependency]) -> Result<Vec<RefHit>> {
    Ok(refs_filtered(conn, query, kinds, usize::MAX, deps, true)?.0)
}

fn refs_filtered(
    conn: &Connection,
    query: &str,
    kinds: &[&str],
    limit: usize,
    deps: &[Dependency],
    importing_deps: bool,
) -> Result<(Vec<RefHit>, RefSummary)> {
    let (mut container, mut name) = split_dotted(query.trim());
    // A name with a dot in it (an Ansible file name, `deploy.yml`) is the whole query when references use it whole.
    if container.is_some() {
        let whole: bool = conn
            .prepare_cached("SELECT EXISTS(SELECT 1 FROM refs WHERE name = ?1)")?
            .query_row([query.trim()], |r| r.get(0))?;
        if whole {
            (container, name) = (None, query.trim());
        }
    }
    // References in SQL files are stored lower-cased, as SQL matches names.
    let sql = format!(
        "SELECT {REF_COLS} FROM refs r JOIN files f ON f.id = r.file_id LEFT JOIN symbols fs ON fs.id = r.from_id
         WHERE r.name = ?1 OR (?2 <> ?1 AND r.name = ?2 AND f.lang IN ('cfml', 'sql')) ORDER BY f.path, r.line"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    // CFML references are stored lower case: CFML ignores case.
    let mut hits: Vec<RefHit> = stmt
        .query_map([name, &name.to_lowercase()], ref_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .filter(|h| kinds.is_empty() || kinds.contains(&h.kind.as_str()))
        .collect();
    if importing_deps {
        let mut stmt = conn.prepare_cached("SELECT i.module FROM imports i JOIN files f ON f.id = i.file_id WHERE f.path = ?1")?;
        let mut imports_deps: HashMap<String, bool> = HashMap::new();
        let mut keep = Vec::with_capacity(hits.len());
        for h in hits {
            if !imports_deps.contains_key(&h.file) {
                let mods: Vec<String> = stmt.query_map([&h.file], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
                let any = deps.iter().any(|d| mods.iter().any(|m| d.modules.iter().any(|dm| imports_module(m, dm))));
                imports_deps.insert(h.file.clone(), any);
            }
            if imports_deps[&h.file] {
                keep.push(h);
            }
        }
        hits = keep;
    }
    resolve_all(conn, &mut hits)?;
    resolve_external(conn, &mut hits, deps)?;
    if kinds.is_empty() || kinds.contains(&"instantiate") {
        add_factory_constructions(conn, name, &mut hits)?;
    }
    if let Some(c) = container {
        // `Type.member`: keep references that can point at that container's member.
        let (lc, ln) = (c.to_lowercase(), name.to_lowercase());
        hits.retain(|h| {
            h.confidence != "external_likely"
                && h.targets.iter().any(|t| {
                    t.qualified == format!("{c}.{name}")
                        || t.qualified.ends_with(&format!(".{c}.{name}"))
                        || (crate::sql::is_sql_path(&t.file) && t.qualified.to_lowercase() == format!("{lc}.{ln}"))
                })
        });
    }
    let summary = summarize(&hits);
    hits.truncate(limit);
    Ok((hits, summary))
}

/// Where C# code constructs a type through a factory the container injects, by type name: a call of an Autofac
/// delegate factory (`Widget.Factory make` called `make("x")`, `make.Invoke(…)`, `make?.Invoke(…)`) or of a
/// `Func<…, T>` (`Func<Owned<T>>` too), as `instantiate` references of the type it makes. A factory returning a class
/// constructs it (`exact` for the one class of that name); one returning a service the container binds to a single
/// implementation constructs that (`unique_name`). The binding shown in `via_di` is the delegate type's own
/// registration when it has one, else the service's.
///
/// Computed once per index generation for every site, and kept for the process: `refs` asks it for every type name.
/// Adds the factory calls constructing `name` (see [`factory_constructions`]) to `hits`, in file and line order.
fn add_factory_constructions(conn: &Connection, name: &str, hits: &mut Vec<RefHit>) -> Result<()> {
    let made = factory_constructions(conn)?;
    if let Some(more) = made.get(name) {
        hits.extend(more.iter().cloned());
        hits.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
    }
    Ok(())
}

fn factory_constructions(conn: &Connection) -> Result<std::sync::Arc<HashMap<String, Vec<RefHit>>>> {
    type Cache = HashMap<(String, String), std::sync::Arc<HashMap<String, Vec<RefHit>>>>;
    static CACHE: std::sync::Mutex<Option<Cache>> = std::sync::Mutex::new(None);
    let generation: String =
        conn.query_row("SELECT value FROM meta WHERE key = 'tree_gen'", [], |r| r.get(0)).optional()?.unwrap_or_default();
    let key = (conn.path().unwrap_or_default().to_string(), generation);
    if !key.0.is_empty()
        && let Some(v) = CACHE.lock().expect("cache lock").as_ref().and_then(|c| c.get(&key))
    {
        return Ok(v.clone());
    }
    let v = std::sync::Arc::new(factory_constructions_uncached(conn)?);
    if !key.0.is_empty() {
        let mut cache = CACHE.lock().expect("cache lock");
        let c = cache.get_or_insert_with(HashMap::new);
        c.retain(|(path, _), _| *path != key.0);
        c.insert(key, v.clone());
    }
    Ok(v)
}

fn factory_constructions_uncached(conn: &Connection) -> Result<HashMap<String, Vec<RefHit>>> {
    let mut out: HashMap<String, Vec<RefHit>> = HashMap::new();
    // Calls of a delegate (`Invoke`, written or of a value holding one), calls of a member a constructor was given,
    // and Entity Framework context factories' `CreateDbContext`.
    let sql = format!(
        "SELECT {REF_COLS} FROM refs r JOIN files f ON f.id = r.file_id LEFT JOIN symbols fs ON fs.id = r.from_id
         WHERE f.lang = 'csharp' AND r.kind = 'call'
         AND (r.name IN ('Invoke', 'CreateDbContext', 'CreateDbContextAsync') OR (r.receiver IS NULL AND r.name IN (
           SELECT p.name FROM symbols p JOIN files pf ON pf.id = p.file_id
           WHERE pf.lang = 'csharp' AND p.kind = 'property' AND ',' || p.tags || ',' LIKE '%,injected,%')))
         ORDER BY f.path, r.line, r.name"
    );
    let sites: Vec<RefHit> = conn.prepare(&sql)?.query_map([], ref_row)?.collect::<rusqlite::Result<_>>()?;
    if sites.is_empty() {
        return Ok(out);
    }
    let mut oracle = TypeOracle::new(conn);
    // The repo's classes by name: id, container and file.
    let mut classes: HashMap<String, Vec<(i64, Option<String>, String)>> = HashMap::new();
    let mut class_ids = |name: &str| -> Result<Vec<(i64, Option<String>, String)>> {
        if !classes.contains_key(name) {
            let ids = conn
                .prepare_cached(
                    "SELECT s.id, s.container, f.path FROM symbols s JOIN files f ON f.id = s.file_id
                     WHERE s.name = ?1 AND s.name = ?1 COLLATE BINARY AND s.kind = 'class' AND f.lang = 'csharp'
                     ORDER BY f.path, s.start_byte",
                )?
                .query_map([name], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<_>>()?;
            classes.insert(name.to_string(), ids);
        }
        Ok(classes[name].clone())
    };
    let di = crate::di::any(conn)?;
    for site in sites {
        let caller = site.caller_type();
        oracle.here = Some(site.file.clone());
        oracle.file = None;
        oracle.caller = caller.clone();
        oracle.origin = Origin::Here;
        // `contextFactory.CreateDbContext()` makes the context, whoever made the factory.
        if site.name != "Invoke" && site.receiver.is_some() {
            let made = match site.receiver_type.as_deref().map(|enc| oracle.eval_encoded(enc, caller.as_deref())).transpose()?.flatten() {
                Some(TypeExpr::Generic { name, args }) if matches!(last_segment(&name), "IDbContextFactory" | "PooledDbContextFactory") => {
                    match args.as_slice() {
                        [Some(TypeExpr::Named(t) | TypeExpr::Generic { name: t, .. })] => last_segment(t).to_string(),
                        _ => continue,
                    }
                }
                _ => continue,
            };
            let ids: Vec<i64> = class_ids(&made)?.into_iter().map(|c| c.0).collect();
            if !ids.is_empty() {
                let confidence = if ids.len() == 1 { "exact" } else { "ambiguous" };
                out.entry(made.clone()).or_default().push(construction(conn, &site, &made, &ids, confidence, None)?);
            }
            continue;
        }
        // The member holding the factory must hold what a constructor was given: the container's factory. One a test
        // fills with its own lambda or a mock makes nothing the container would.
        let member = match &site.receiver {
            Some(r) => r.strip_prefix("this.").unwrap_or(r),
            None => site.name.as_str(),
        };
        if !member.chars().all(|c| c.is_alphanumeric() || c == '_') || !injected_member(conn, &mut oracle, caller.as_deref(), member)? {
            continue;
        }
        // The factory: the receiver of an `Invoke`, or the property called.
        let factory = if site.name == "Invoke" {
            match &site.receiver_type {
                Some(enc) => oracle.eval_encoded(enc, caller.as_deref())?,
                None => None,
            }
        } else {
            oracle.property_in_scope(&site.name, caller.as_deref(), 0)?
        };
        let Some(factory) = factory else { continue };
        // A delegate type: its name, qualified by the type declaring it, and that type.
        let (delegate, declaring) = match &factory {
            TypeExpr::Named(n) => match oracle.delegate(&factory)? {
                Some((container, _)) => {
                    let file = match &oracle.origin {
                        Origin::In(f, _) => Some(f.clone()),
                        _ => None,
                    };
                    (Some(join(container.as_deref(), last_segment(n))), container.zip(file))
                }
                None => continue,
            },
            TypeExpr::Delegate(_) => (None, None),
            _ => continue,
        };
        let made = match oracle.invoked(&factory, 0)? {
            // `Func<Owned<T>>`, `Func<Lazy<T>>`: the container makes the `T`.
            Some(TypeExpr::Generic { name, args }) if matches!(last_segment(&name), "Owned" | "Lazy") => match args.as_slice() {
                [Some(t)] => oracle.eval(t, None, 0)?,
                _ => None,
            },
            other => other,
        };
        let made = match made {
            Some(TypeExpr::Named(n) | TypeExpr::Generic { name: n, .. }) => last_segment(&n).to_string(),
            _ => continue,
        };
        let mut own = class_ids(&made)?;
        // A delegate factory nested in the class it makes (`CartInstance.Factory`) makes that one.
        if let Some((container, file)) = &declaring
            && last_segment(container) == made
        {
            let parent = container.rsplit_once('.').map(|(p, _)| p);
            own.retain(|(_, c, f)| f == file && c.as_deref() == parent);
        }
        let own: Vec<i64> = own.into_iter().map(|c| c.0).collect();
        // A `Func` makes a class only when the container registers it.
        let made_by_container =
            delegate.is_some() || (di && crate::di::registered_as(conn, &made)?.iter().any(|r| !r.test));
        let (class, ids, confidence, binding) = if !own.is_empty() && made_by_container {
            (made, own, "exact", None)
        } else if !own.is_empty() {
            continue;
        } else if di && let Some(reg) = crate::di::bound(conn, &made)? {
            // A service the container binds to one implementation: that implementation.
            let Some(imp) = reg.concrete().map(crate::di::bare_name) else { continue };
            let ids: Vec<i64> = class_ids(&imp)?.into_iter().map(|c| c.0).collect();
            (imp, ids, "unique_name", Some(reg))
        } else {
            continue;
        };
        if ids.is_empty() {
            continue;
        }
        let confidence = if ids.len() > 1 { "ambiguous" } else { confidence };
        let explicit = match &delegate {
            Some(d) if di => {
                let written = |r: &crate::di::Registration| {
                    r.service.as_deref().is_some_and(|w| w == d || d.ends_with(&format!(".{w}")) || w.ends_with(&format!(".{d}")))
                };
                crate::di::registrations_of(conn, last_segment(d))?.into_iter().find(|r| !r.test && written(r))
            }
            _ => None,
        };
        let hit = construction(conn, &site, &class, &ids, confidence, explicit.or(binding))?;
        out.entry(class).or_default().push(hit);
    }
    Ok(out)
}

/// A factory call `site` as an `instantiate` reference of the class `class` (`ids` its definitions), with the
/// registration binding it when one does.
fn construction(
    conn: &Connection,
    site: &RefHit,
    class: &str,
    ids: &[i64],
    confidence: &'static str,
    binding: Option<crate::di::Registration>,
) -> Result<RefHit> {
    let mut hit = site.clone();
    hit.name = class.to_string();
    hit.kind = "instantiate".to_string();
    if hit.receiver.is_none() {
        hit.receiver = Some(site.name.clone());
    }
    hit.receiver_type = None;
    hit.confidence = confidence;
    // The container makes the class: a factory it injects.
    hit.evidence = Some(Evidence::new(Mechanism::DiBinding));
    hit.factory = true;
    hit.targets = ids.iter().take(TARGETS_SHOWN).map(|&id| target(conn, id)).collect::<Result<_>>()?;
    hit.targets_total = (ids.len() > TARGETS_SHOWN).then_some(ids.len());
    hit.target_ids = ids.to_vec();
    if let Some(reg) = binding
        && let [id] = ids
    {
        hit.via_di = Some(crate::di::Bound {
            target: target(conn, *id)?,
            lifetime: reg.lifetime.clone(),
            registered: format!("{}:{}", reg.file, reg.line),
            assumptions: Vec::new(),
        });
    }
    Ok(hit)
}

/// Whether `member`, seen from code in `caller`, is a field, property or primary constructor parameter of the caller's
/// type, an enclosing type or a supertype that holds what a constructor was given (tagged `injected`).
fn injected_member(conn: &Connection, oracle: &mut TypeOracle, caller: Option<&str>, member: &str) -> Result<bool> {
    let mut stmt = conn.prepare_cached(
        "SELECT EXISTS(SELECT 1 FROM symbols WHERE name = ?1 AND name = ?1 COLLATE BINARY AND kind = 'property'
         AND (container = ?2 OR container LIKE '%.' || ?2) AND ',' || tags || ',' LIKE '%,injected,%')",
    )?;
    let mut s = caller.map(str::to_string);
    while let Some(cur) = s {
        for t in oracle.chain(&cur)? {
            if stmt.query_row(params![member, t], |r| r.get::<_, bool>(0))? {
                return Ok(true);
            }
        }
        s = cur.rsplit_once('.').map(|(p, _)| p.to_string());
    }
    Ok(false)
}

/// Every reference using one of `names`, resolved in one pass that shares its caches: for whole-repo questions
/// (unused symbols) that would otherwise resolve name by name.
pub fn refs_named(conn: &Connection, names: &[&str]) -> Result<Vec<RefHit>> {
    let sql = format!(
        "SELECT {REF_COLS} FROM refs r JOIN files f ON f.id = r.file_id LEFT JOIN symbols fs ON fs.id = r.from_id
         WHERE r.name = ?1 ORDER BY f.path, r.line"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let mut hits = Vec::new();
    for name in names {
        hits.extend(stmt.query_map([name], ref_row)?.collect::<rusqlite::Result<Vec<_>>>()?);
    }
    resolve_all(conn, &mut hits)?;
    for name in names {
        add_factory_constructions(conn, name, &mut hits)?;
    }
    Ok(hits)
}

fn summarize(hits: &[RefHit]) -> RefSummary {
    let mut s = RefSummary { total: hits.len(), ..Default::default() };
    for h in hits {
        *s.by_confidence.entry(h.confidence).or_default() += 1;
        s.via_di += h.via_di.is_some() as usize;
        s.precise += h.precise as usize;
        s.corrected += h.corrected_from.is_some() as usize;
    }
    s
}

#[derive(Debug, Clone, Serialize)]
pub struct RelationHit {
    /// `inherits`, `conforms_to`, or `inherits_or_conforms` when the target isn't defined in this repo.
    pub kind: &'static str,
    pub target: String,
    pub external: bool,
    pub file: String,
    pub line: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct Context {
    pub symbol: SymbolRow,
    pub source: String,
    pub source_truncated: bool,
    /// How `source` shows the symbol: `full`, `skeleton` (signatures and key lines, bodies elided) or `none`.
    pub source_mode: &'static str,
    /// For a skeleton, the file lines it keeps (`10-14,20,33-35`); the rest of the span is elided.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_lines: Option<String>,
    /// The file's content differs from what the index holds (an edit that kept its mtime and size): `source` is the
    /// file as it is now, the rest of the answer as last indexed. A query that syncs indexes the file again and
    /// answers from it instead.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub source_changed: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub relations: Vec<RelationHit>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<SymbolRow>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<SymbolRow>,
    pub members_total: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub conformers: Vec<SymbolRow>,
    /// How many conformers there are, when more than are listed ([`CONFORMERS_SHOWN`] by default).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conformers_total: Option<usize>,
    /// Conformers and subclasses in repos depending on this one (a library's interface implemented by its users).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub conformers_elsewhere: Vec<Elsewhere>,
    /// How many there are, when more are found than listed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conformers_elsewhere_total: Option<usize>,
    pub callers: Vec<RefHit>,
    pub callers_summary: RefSummary,
    pub callees: Vec<RefHit>,
    pub callees_total: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<SymbolRow>,
    /// How many other definitions have the name, when more than [`ALTERNATIVES_SHOWN`] are listed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alternatives_total: Option<usize>,
    /// For a C# type: its dependency-injection registrations and, for a service, the implementations bound to it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub di: Option<crate::di::DiSection>,
    /// The symbol's lines checked against the index (see [`crate::answer::Cite`]); `source` quotes them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cite: Option<crate::answer::Cite>,
}

/// At most this many of a type's conformers are listed in its context.
pub const CONFORMERS_SHOWN: usize = 50;
/// At most this many other definitions of a name are listed in a context.
pub const ALTERNATIVES_SHOWN: usize = 10;

/// A symbol in another indexed repo.
#[derive(Debug, Clone, Serialize)]
pub struct Elsewhere {
    pub repo: String,
    #[serde(flatten)]
    pub symbol: SymbolRow,
}

pub struct ContextLimits {
    pub source_chars: usize,
    pub members: usize,
    pub callers: usize,
    pub callees: usize,
    /// Conformers of a type listed (by default [`CONFORMERS_SHOWN`]).
    pub conformers: usize,
    /// How to show the source.
    pub source: crate::skeleton::SourceMode,
}

impl ContextLimits {
    /// Splits a token budget (about 4 characters per token) across the sections.
    pub fn from_budget(tokens: usize) -> ContextLimits {
        let chars = tokens * 4;
        ContextLimits {
            source_chars: chars * 2 / 5,
            members: (tokens / 60).clamp(5, 500),
            callers: (tokens / 120).clamp(3, 400),
            callees: (tokens / 120).clamp(3, 400),
            conformers: CONFORMERS_SHOWN,
            source: crate::skeleton::SourceMode::Full,
        }
    }
}

/// Everything an agent usually reads next about a symbol, in one bundle.
pub fn context(conn: &Connection, root: &Path, query: &str, limits: &ContextLimits) -> Result<Option<Context>> {
    context_with(conn, root, query, limits, &[])
}

/// [`context`], also resolving callees that leave the repo against `deps`.
/// The imports [`context_with`] may consult to resolve `query`'s callees into dependencies: those of the file
/// declaring the symbol it picks. None when it picks a type (types list no callees) or finds nothing. Dependencies
/// providing none of these modules can't change the context, so needn't be opened.
pub fn context_imports(conn: &Connection, query: &str) -> Result<Vec<String>> {
    let Some(symbol) = definitions(conn, query)?.into_iter().next() else { return Ok(Vec::new()) };
    if TYPE_KINDS.contains(&symbol.kind.as_str()) || symbol.kind == "extension" {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare_cached("SELECT i.module FROM imports i JOIN files f ON f.id = i.file_id WHERE f.path = ?1")?;
    let rows = stmt.query_map([&symbol.file], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn context_with(
    conn: &Connection,
    root: &Path,
    query: &str,
    limits: &ContextLimits,
    deps: &[Dependency],
) -> Result<Option<Context>> {
    let mut defs = definitions(conn, query)?;
    if defs.is_empty() {
        return Ok(None);
    }
    // Prefer a type over its extensions, and a definition over a same-named property.
    let symbol = defs.remove(0);

    let is_type = TYPE_KINDS.contains(&symbol.kind.as_str()) || symbol.kind == "extension";

    let mut extensions = Vec::new();
    let mut relations = Vec::new();
    let mut members = Vec::new();
    let mut members_total = 0;
    let mut conformers = Vec::new();
    let mut conformers_total = None;
    if is_type {
        let qualified = symbol.qualified();
        let sql = format!(
            "SELECT {SYMBOL_COLS} FROM symbols s JOIN files f ON f.id = s.file_id
             WHERE s.kind = 'extension' AND s.name = ?1 AND s.name = ?1 COLLATE BINARY AND s.id != ?2 ORDER BY f.path, s.start_line"
        );
        let mut stmt = conn.prepare_cached(&sql)?;
        extensions = stmt
            .query_map(params![symbol.name, symbol.id], symbol_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .filter(|e| e.qualified() == qualified)
            .collect();

        let from_ids: Vec<i64> = std::iter::once(symbol.id).chain(extensions.iter().map(|e| e.id)).collect();
        let mut stmt = conn.prepare_cached(
            "SELECT r.target, r.target_name, f.path, r.line FROM relations r JOIN files f ON f.id = r.file_id
             WHERE r.from_id = ?1 ORDER BY r.line",
        )?;
        for id in &from_ids {
            let rows: Vec<(String, String, String, u32)> = stmt
                .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                .collect::<rusqlite::Result<_>>()?;
            for (target, target_name, file, line) in rows {
                let target_defs = defs_named(conn, &target_name)?;
                let types: Vec<&Def> = target_defs.iter().filter(|d| TYPE_KINDS.contains(&d.kind.as_str())).collect();
                let kind = match types.first().map(|d| d.kind.as_str()) {
                    Some("protocol") => "conforms_to",
                    Some(_) => "inherits",
                    None => "inherits_or_conforms",
                };
                relations.push(RelationHit { kind, target, external: types.is_empty(), file, line });
            }
        }

        let sql = format!(
            "SELECT {SYMBOL_COLS} FROM symbols s JOIN files f ON f.id = s.file_id
             WHERE s.container = ?1 AND s.kind != 'extension' ORDER BY f.path, s.start_line"
        );
        let mut stmt = conn.prepare_cached(&sql)?;
        let all: Vec<SymbolRow> = stmt.query_map([&qualified], symbol_row)?.collect::<rusqlite::Result<_>>()?;
        members_total = all.len();
        members = all.into_iter().take(limits.members).collect();

        let sql = format!(
            "SELECT {SYMBOL_COLS} FROM relations r JOIN symbols s ON s.id = r.from_id JOIN files f ON f.id = s.file_id
             WHERE r.target_name = ?1 COLLATE BINARY ORDER BY f.path, s.start_line, s.id, r.line LIMIT ?2"
        );
        let mut stmt = conn.prepare_cached(&sql)?;
        let limit = i64::try_from(limits.conformers).unwrap_or(i64::MAX);
        conformers = stmt.query_map(params![symbol.name, limit], symbol_row)?.collect::<rusqlite::Result<_>>()?;
        // At the limit, say how many there are.
        if conformers.len() == limits.conformers {
            let n: i64 = conn
                .prepare_cached("SELECT COUNT(*) FROM relations r JOIN symbols s ON s.id = r.from_id WHERE r.target_name = ?1 COLLATE BINARY")?
                .query_row([&symbol.name], |r| r.get(0))?;
            conformers_total = Some(n as usize).filter(|&n| n > limits.conformers);
        }
    }

    // Callers: references that resolve to this symbol. An initializer is called through its type's name.
    let (caller_name, caller_kinds): (String, &[&str]) = if symbol.kind == "init" {
        let ty = symbol.container.as_deref().unwrap_or("");
        (ty.rsplit('.').next().unwrap_or(ty).to_string(), &["instantiate"])
    } else if is_type {
        (symbol.name.clone(), &["instantiate", "type_ref"])
    } else if let Some(kinds) = crate::sql::relation_ref_kinds(&symbol.kind, &symbol.file) {
        (symbol.name.clone(), kinds)
    } else if let Some(kinds) = terraform_ref_kinds(&symbol) {
        (symbol.name.clone(), kinds)
    } else if let Some(kinds) = ansible_ref_kinds(&symbol) {
        (symbol.name.clone(), kinds)
    } else {
        (symbol.name.clone(), &["call", "member"])
    };
    let (all_callers, _) = refs(conn, &caller_name, caller_kinds, usize::MAX)?;
    let target_id = if symbol.kind == "init" {
        definitions(conn, symbol.container.as_deref().unwrap_or(""))?.first().map(|d| d.id)
    } else {
        Some(symbol.id)
    };
    let mut callers: Vec<RefHit> = all_callers
        .iter()
        .filter(|h| h.confidence != "external_likely")
        .filter(|h| target_id.is_some_and(|id| h.may_target(id)))
        .cloned()
        .collect();
    if symbol.kind == "init" {
        // A constructor's callers are the creations its arguments pick, and the constructors chaining to it.
        let several = definitions(conn, &symbol.qualified())?.iter().filter(|d| d.kind == "init").count() > 1;
        callers.retain_mut(|h| match &h.ctors {
            Some(called) if !called.contains(&symbol.id) => false,
            Some(called) => {
                if called.len() > 1 {
                    h.confidence = "ambiguous";
                }
                true
            }
            None => {
                if several {
                    h.confidence = "ambiguous";
                }
                true
            }
        });
        let (chained, _) = refs(conn, "init", &["call"], usize::MAX)?;
        callers.extend(chained.into_iter().filter(|h| h.may_target(symbol.id)));
    }
    // An implementation is also called through the interface member it implements, or the base member it overrides.
    if matches!(symbol.kind.as_str(), "method" | "property") && symbol.container.is_some() {
        let members = implemented_members(conn, &symbol)?;
        if !members.is_empty() {
            let direct: HashSet<(&str, u32)> = callers.iter().map(|c| (c.file.as_str(), c.line)).collect();
            let through: Vec<RefHit> = all_callers
                .iter()
                .filter(|h| h.confidence != "external_likely" && !direct.contains(&(h.file.as_str(), h.line)))
                .filter_map(|h| members.iter().find_map(|m| m.through(h, symbol.id)))
                .collect();
            callers.extend(through);
        }
    }
    // Most certain first, then by location.
    callers.sort_by_key(|h| (confidence_rank(h.confidence), h.file.clone(), h.line));
    let callers_summary = summarize(&callers);
    callers.truncate(limits.callers);

    let mut callees = Vec::new();
    let mut callees_total = 0;
    let mut key_lines = Vec::new();
    if !is_type {
        let sql = format!(
            "SELECT {REF_COLS} FROM refs r JOIN files f ON f.id = r.file_id LEFT JOIN symbols fs ON fs.id = r.from_id
             WHERE r.from_id = ?1 AND r.kind != 'type_ref' ORDER BY r.line"
        );
        let mut stmt = conn.prepare_cached(&sql)?;
        let mut hits: Vec<RefHit> = stmt.query_map([symbol.id], ref_row)?.collect::<rusqlite::Result<_>>()?;
        resolve_all(conn, &mut hits)?;
        resolve_external(conn, &mut hits, deps)?;
        // In-repo targets first; framework calls are listed after.
        hits.sort_by_key(|h| (h.confidence == "unresolved", h.line));
        callees_total = hits.len();
        key_lines = crate::skeleton::reference_lines(&hits);
        hits.truncate(limits.callees);
        callees = hits;
    }
    let (source, source_truncated, source_mode, source_lines, source_changed) = read_source(conn, root, &symbol, limits, &key_lines)?;

    let others: Vec<SymbolRow> = defs.into_iter().filter(|d| !extensions.iter().any(|e| e.id == d.id)).collect();
    let alternatives_total = (others.len() > ALTERNATIVES_SHOWN).then_some(others.len());
    let alternatives: Vec<SymbolRow> = others.into_iter().take(ALTERNATIVES_SHOWN).collect();
    let di = if is_type { crate::di::section(conn, &symbol)? } else { None };

    Ok(Some(Context {
        symbol,
        source,
        source_truncated,
        source_mode,
        source_lines,
        source_changed,
        relations,
        extensions,
        members,
        members_total,
        conformers,
        conformers_total,
        conformers_elsewhere: Vec::new(),
        conformers_elsewhere_total: None,
        callers,
        callers_summary,
        callees,
        callees_total,
        alternatives,
        alternatives_total,
        di,
        cite: None,
    }))
}

/// A member an implementation implements (an interface's) or overrides (a base class's).
#[derive(Debug, Clone)]
pub struct Implemented {
    pub id: i64,
    /// The interface or base type declaring it, as the implementation's type names it.
    pub owner: String,
    /// Calls through it may reach another implementation: an interface several types implement, or a base member
    /// (which runs itself, or another override, for other instances).
    pub shared: bool,
}

impl Implemented {
    /// `hit` as a call of the implementation `id` through this member: unless it targets `id` already, or is the
    /// override calling its base (`base.M()`). `ambiguous` when it may reach another implementation instead.
    pub fn through(&self, hit: &RefHit, id: i64) -> Option<RefHit> {
        if hit.may_target(id) {
            return None;
        }
        self.through_member(hit)
    }

    /// `hit` as a call through this member, for an implementation outside the repo `hit` was resolved in (a
    /// dependency's base class calling its own virtual member). `None` for the override calling its base.
    pub fn through_member(&self, hit: &RefHit) -> Option<RefHit> {
        if !hit.may_target(self.id) || hit.receiver.as_deref() == Some("base") {
            return None;
        }
        let mut h = hit.clone();
        h.through = Some(self.owner.clone());
        // Another implementation may run, or the call may be another overload its argument types didn't rule out.
        if (self.shared || hit.target_ids.len() > 1) && matches!(h.confidence, "exact" | "unique_name") {
            h.confidence = "ambiguous";
        }
        Some(h)
    }
}

/// The interface members `sym` (a method or property of a type) implements, and for an `override`, the base class
/// members it overrides: same-named members of the same kind, taking the same arguments, in the type's supertypes,
/// transitively.
pub fn implemented_members(conn: &Connection, sym: &SymbolRow) -> Result<Vec<Implemented>> {
    let Some(container) = sym.container.as_deref() else { return Ok(Vec::new()) };
    let short = container.rsplit('.').next().unwrap_or(container).to_string();
    let parents = supertypes(conn, &short)?;
    implemented_in(conn, conn, sym, parents, HashSet::from([short]), false)
}

/// [`implemented_members`] declared in a dependency `dep` (a package's base class or interface): the supertypes
/// `sym`'s type names in its own repo (`conn`) that no type there defines, looked up in `dep`, and theirs there,
/// transitively. Any repo's implementation may run for a call in `dep`, so each is `shared`.
pub fn implemented_members_in(conn: &Connection, dep: &Connection, sym: &SymbolRow) -> Result<Vec<Implemented>> {
    let Some(container) = sym.container.as_deref() else { return Ok(Vec::new()) };
    let short = container.rsplit('.').next().unwrap_or(container).to_string();
    let mut seen = HashSet::from([short.clone()]);
    let mut queue = vec![short];
    let mut outside = Vec::new();
    while let Some(ty) = queue.pop() {
        for p in supertypes(conn, &ty)? {
            if seen.len() > 32 || !seen.insert(p.clone()) {
                continue;
            }
            if definitions(conn, &p)?.iter().any(|d| TYPE_KINDS.contains(&d.kind.as_str())) {
                queue.push(p);
            } else {
                outside.push(p);
            }
        }
    }
    if outside.is_empty() {
        return Ok(Vec::new());
    }
    seen.retain(|n| !outside.contains(n));
    implemented_in(conn, dep, sym, outside, seen, true)
}

/// The type a DI factory registered at `file`:`line` makes, from `made` (an encoded type expression, see
/// `kc_extract::DiBinding::made`) evaluated as the code there sees it: its bare name, when it is a named type.
pub(crate) fn made_type(conn: &Connection, file: &str, line: u32, made: &str) -> Result<Option<String>> {
    // The type the registration is written in.
    let caller = symbols_at(conn, file, line)?.into_iter().find(|s| TYPE_KINDS.contains(&s.kind.as_str())).map(|s| s.qualified);
    let mut oracle = TypeOracle::new(conn);
    oracle.here = Some(file.to_string());
    oracle.caller = caller.clone();
    oracle.origin = Origin::Here;
    Ok(match oracle.eval_encoded(made, caller.as_deref())? {
        Some(TypeExpr::Named(n) | TypeExpr::Generic { name: n, .. }) => Some(last_segment(&n).to_string()).filter(|n| !n.is_empty()),
        _ => None,
    })
}

/// The namespace a C# type is declared in, when known.
pub fn type_namespace(conn: &Connection, id: i64) -> Result<Option<String>> {
    let enc: Option<String> = conn.prepare_cached("SELECT type_name FROM symbols WHERE id = ?1")?.query_row([id], |r| r.get(0))?;
    Ok(enc.and_then(|e| kc_extract::TypeDecl::decode(Some(&e)).ns))
}

/// The types of `conn` (a repo using a library) declaring the library's type `name`, of namespace `ns`, as a
/// supertype: those whose file sees `ns` (declaring a type in it or below, importing it, aliasing the name), or
/// writing the name qualified by it. Where the repo declares a type of that name its files see too, theirs wins.
/// In file order.
pub fn conformers_named(conn: &Connection, name: &str, ns: Option<&str>) -> Result<Vec<SymbolRow>> {
    let sql = format!(
        "SELECT {SYMBOL_COLS}, r.target FROM relations r JOIN symbols s ON s.id = r.from_id JOIN files f ON f.id = s.file_id
         WHERE r.target_name = ?1 COLLATE BINARY ORDER BY f.path, s.start_line"
    );
    let rows: Vec<(SymbolRow, String)> =
        conn.prepare_cached(&sql)?.query_map([name], |r| Ok((symbol_row(r)?, r.get(14)?)))?.collect::<rusqlite::Result<_>>()?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    // The repo's own types of the name, by namespace.
    let mut own: Vec<String> = Vec::new();
    for d in definitions(conn, name)?.iter().filter(|d| TYPE_KINDS.contains(&d.kind.as_str())) {
        own.push(type_namespace(conn, d.id)?.unwrap_or_default());
    }
    let mut vis = crate::csharp::Visibility::default();
    let mut out: Vec<SymbolRow> = Vec::new();
    for (s, written) in rows {
        let bare = written.split('<').next().unwrap_or(&written);
        let means = match bare.rsplit_once('.') {
            Some((qualifier, _)) => crate::csharp::Visibility::qualifies(ns, None, qualifier),
            None if own.is_empty() => vis.sees(conn, &s.file, ns, name)?,
            None => {
                let mut among = own.clone();
                among.extend(ns.map(str::to_string));
                let seen = vis.seen_among(conn, &s.file, name, &among)?;
                ns.is_some_and(|n| seen.iter().any(|x| x == n)) && seen.len() == 1
            }
        };
        if means && !out.iter().any(|o| o.id == s.id) {
            out.push(s);
        }
    }
    Ok(out)
}

/// The names of the supertypes types named `ty` declare in `conn`.
fn supertypes(conn: &Connection, ty: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare_cached(
        "SELECT DISTINCT r.target_name FROM relations r JOIN symbols s ON s.id = r.from_id
         WHERE s.name = ?1 COLLATE BINARY AND s.kind IN ('class', 'struct', 'protocol') ORDER BY r.target_name",
    )?;
    Ok(stmt.query_map([ty], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?)
}

/// The members `sym` (of `conn`) implements or overrides among those of `start` (supertype names, looked up in
/// `types`) and their supertypes there, transitively, never visiting `seen`. `dependency` when `types` is another
/// repo's: each is then marked as possibly reaching another implementation.
fn implemented_in(
    conn: &Connection,
    types: &Connection,
    sym: &SymbolRow,
    start: Vec<String>,
    mut seen: HashSet<String>,
    dependency: bool,
) -> Result<Vec<Implemented>> {
    let same_repo = !dependency;
    let Some(container) = sym.container.as_deref() else { return Ok(Vec::new()) };
    let arity = |c: &Connection, id: i64| -> Result<(Option<u32>, Option<u32>)> {
        Ok(c.prepare_cached("SELECT min_args, max_args FROM symbols WHERE id = ?1")?.query_row([id], |r| Ok((r.get(0)?, r.get(1)?)))?)
    };
    let param_types = |c: &Connection, id: i64| -> Result<Option<Vec<Option<String>>>> {
        let enc: Option<String> = c.prepare_cached("SELECT param_types FROM symbols WHERE id = ?1")?.query_row([id], |r| r.get(0))?;
        Ok(enc.as_deref().and_then(Params::decode).map(|p| p.types))
    };
    let own = arity(conn, sym.id)?;
    let own_params = param_types(conn, sym.id)?;
    let overrides = sym.signature.split_whitespace().any(|w| w == "override");
    let mut out = Vec::new();
    let mut queue = Vec::new();
    let mut parents = start;
    loop {
        for p in parents {
            if seen.len() > 32 || !seen.insert(p.clone()) {
                continue;
            }
            queue.push(p.clone());
            let interface = definitions(types, &p)?.iter().any(|d| d.kind == "protocol");
            if !interface && !overrides {
                continue;
            }
            let mut members: Vec<SymbolRow> = definitions(types, &format!("{p}.{}", sym.name))?
                .into_iter()
                .filter(|d| {
                    !(same_repo && d.id == sym.id)
                        && d.kind == sym.kind
                        && d.container.as_deref().is_some_and(|c| c == p || c.ends_with(&format!(".{p}")))
                })
                .collect();
            // An implementation takes the arguments the member does (a WCF client's `XAsync()` doesn't implement
            // the channel's `XAsync(request)`).
            let mut same = Vec::new();
            for m in members {
                let theirs = arity(types, m.id)?;
                if theirs == own || theirs.0.is_none() || own.0.is_none() {
                    same.push(m);
                }
            }
            // And the same parameter types, where both declare them (a type parameter matches anything): an
            // implementation of `Create(Uri)` doesn't implement `Create(string)`. When several overloads match by
            // arity and none by type, the types are misread: keep them all.
            if let Some(mine) = &own_params {
                let mut typed = Vec::new();
                for m in &same {
                    if param_types(types, m.id)?.is_none_or(|theirs| params_match(mine, &theirs)) {
                        typed.push(m.clone());
                    }
                }
                if !typed.is_empty() || same.len() == 1 {
                    same = typed;
                }
            }
            members = same;
            for m in members {
                // Implemented elsewhere too: by another type (this type's other overloads don't count, nor do test
                // doubles beside one real implementation).
                let mut shared = dependency || !interface;
                for i in crate::db::running_implementations(types, m.id)? {
                    let c: Option<String> = types.prepare_cached("SELECT container FROM symbols WHERE id = ?1")?.query_row([i], |r| r.get(0))?;
                    shared |= c.as_deref() != Some(container);
                }
                out.push(Implemented { id: m.id, owner: p.clone(), shared });
            }
        }
        let Some(ty) = queue.pop() else { break };
        parents = supertypes(types, &ty)?;
    }
    Ok(out)
}

/// Whether two methods' declared parameter types (encoded, `None` for a type parameter) agree: the same count, and
/// the same type by its last name wherever both are known.
fn params_match(a: &[Option<String>], b: &[Option<String>]) -> bool {
    let name = |enc: &str| TypeExpr::decode(enc).map(|t| t.map_names(&|n: &str| last_segment(n).to_string()).encode());
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| match (x, y) {
            (Some(x), Some(y)) => x == y || name(x) == name(y),
            _ => true,
        })
}

/// The reference kinds reaching a Terraform symbol (a resource, data source, module call, variable, local, output or
/// provider configuration).
pub fn terraform_ref_kinds(symbol: &SymbolRow) -> Option<&'static [&'static str]> {
    if crate::terraform::is_terraform_path(&symbol.file) { crate::terraform::ref_kinds(&symbol.kind) } else { None }
}

/// The reference kinds reaching an Ansible symbol (a role, a handler, a variable, a template, a task file or playbook).
pub fn ansible_ref_kinds(symbol: &SymbolRow) -> Option<&'static [&'static str]> {
    let ansible_file = crate::ansible::is_ansible_kind(&symbol.kind) || kc_extract::ansible::is_template_path(&symbol.file);
    if ansible_file { crate::ansible::ref_kinds(&symbol.kind) } else { None }
}

/// Every reference of one kind, resolved.
pub fn refs_of_kind(conn: &Connection, kind: &str) -> Result<(Vec<RefHit>, RefSummary)> {
    let sql = format!(
        "SELECT {REF_COLS} FROM refs r JOIN files f ON f.id = r.file_id LEFT JOIN symbols fs ON fs.id = r.from_id
         WHERE r.kind = ?1 ORDER BY f.path, r.line"
    );
    let mut hits: Vec<RefHit> = conn.prepare_cached(&sql)?.query_map([kind], ref_row)?.collect::<rusqlite::Result<_>>()?;
    resolve_all(conn, &mut hits)?;
    let summary = summarize(&hits);
    Ok((hits, summary))
}

fn confidence_rank(c: &str) -> u8 {
    match c {
        "exact" => 0,
        "unique_name" => 1,
        "ambiguous" => 2,
        "external_likely" => 3,
        _ => 4,
    }
}

/// The symbol's source as `limits.source` asks: the span (cut at the budget), a skeleton, or nothing. Returns the text,
/// whether it was cut, the mode used, a skeleton's kept lines, and whether the file's content differs from what the
/// index holds (its spans may then be wrong). A symbolic link is not read (the index never follows one).
fn read_source(
    conn: &Connection,
    root: &Path,
    s: &SymbolRow,
    limits: &ContextLimits,
    key_lines: &[u32],
) -> Result<(String, bool, &'static str, Option<String>, bool)> {
    use crate::skeleton::SourceMode;
    if limits.source == SourceMode::None {
        return Ok((String::new(), false, "none", None, false));
    }
    let path = root.join(&s.file);
    let raw = match std::fs::symlink_metadata(&path) {
        Ok(m) if !m.is_symlink() => std::fs::read(&path).ok(),
        _ => None,
    };
    let Some(raw) = raw else { return Ok((String::new(), false, "full", None, false)) };
    let indexed: Option<String> = conn.prepare_cached("SELECT hash FROM files WHERE path = ?1")?.query_row([&s.file], |r| r.get(0)).optional()?;
    let changed = indexed.is_some_and(|h| h != blake3::hash(&raw).to_hex().as_str());
    let decoded = kc_extract::decode(&raw);
    let bytes = decoded.as_bytes();
    let end = s.end_byte.min(bytes.len());
    let start = s.start_byte.min(end);
    let span = String::from_utf8_lossy(&bytes[start..end]);
    let skeleton = match limits.source {
        SourceMode::Skeleton => true,
        SourceMode::Auto => span.len() > limits.source_chars / 2 && s.end_line > s.line,
        _ => false,
    };
    if !skeleton {
        let (text, cut) = cap_text(&span, limits.source_chars);
        return Ok((text, cut, "full", None, changed));
    }
    let sk = crate::skeleton::skeleton(conn, s, &String::from_utf8_lossy(bytes), key_lines)?;
    let (text, cut) = cap_text(&sk.text, limits.source_chars);
    Ok((text, cut, "skeleton", Some(sk.lines), changed))
}

/// `text` within `cap` bytes, cut on a line boundary; and whether it was cut.
fn cap_text(text: &str, cap: usize) -> (String, bool) {
    if text.len() <= cap {
        return (text.to_string(), false);
    }
    let mut cut = cap;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    let cut = text[..cut].rfind('\n').unwrap_or(cut);
    (text[..cut].to_string(), true)
}

#[derive(Debug, Clone, Serialize)]
pub struct Stats {
    pub files: usize,
    pub lines: u64,
    pub files_by_lang: BTreeMap<String, usize>,
    pub files_with_parse_errors: usize,
    pub symbols_by_kind: BTreeMap<String, usize>,
    pub refs_by_kind: BTreeMap<String, usize>,
    pub relations: usize,
    pub imports: usize,
    pub tagged: BTreeMap<String, usize>,
    pub extractor_versions: BTreeMap<String, u32>,
}

pub fn stats(conn: &Connection) -> Result<Stats> {
    let count_by = |sql: &str| -> Result<BTreeMap<String, usize>> {
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as usize)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    };
    let one = |sql: &str| -> Result<i64> { Ok(conn.query_row(sql, [], |r| r.get(0))?) };
    let mut tagged = BTreeMap::new();
    let mut stmt = conn.prepare("SELECT tags FROM symbols WHERE tags != ''")?;
    for tags in stmt.query_map([], |r| r.get::<_, String>(0))? {
        for t in tags?.split(',') {
            *tagged.entry(t.to_string()).or_default() += 1;
        }
    }
    let mut versions = BTreeMap::new();
    let mut stmt = conn.prepare("SELECT lang, MIN(extractor_version) FROM files GROUP BY lang")?;
    for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, u32>(1)?)))? {
        let (l, v) = row?;
        versions.insert(l, v);
    }
    Ok(Stats {
        files: one("SELECT COUNT(*) FROM files")? as usize,
        lines: one("SELECT IFNULL(SUM(lines), 0) FROM files")? as u64,
        files_by_lang: count_by("SELECT lang, COUNT(*) FROM files GROUP BY lang")?,
        files_with_parse_errors: one("SELECT COUNT(*) FROM files WHERE parse_errors > 0")? as usize,
        symbols_by_kind: count_by("SELECT kind, COUNT(*) FROM symbols GROUP BY kind")?,
        refs_by_kind: count_by("SELECT kind, COUNT(*) FROM refs GROUP BY kind")?,
        relations: one("SELECT COUNT(*) FROM relations")? as usize,
        imports: one("SELECT COUNT(*) FROM imports")? as usize,
        tagged,
        extractor_versions: versions,
    })
}

/// Files with parse errors, worst first.
pub fn files_with_errors(conn: &Connection, limit: usize) -> Result<Vec<(String, u32)>> {
    let mut stmt =
        conn.prepare("SELECT path, parse_errors FROM files WHERE parse_errors > 0 ORDER BY parse_errors DESC, path LIMIT ?1")?;
    let rows = stmt.query_map([limit as i64], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Every distinct symbol name, for benchmarks and sampling.
pub fn symbol_names(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT DISTINCT name COLLATE BINARY AS n FROM symbols ORDER BY n")?;
    let rows = stmt.query_map([], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The most-referenced names, for worst-case benchmarks.
pub fn most_referenced(conn: &Connection, limit: usize) -> Result<Vec<(String, usize)>> {
    let mut stmt = conn.prepare("SELECT name, COUNT(*) AS n FROM refs GROUP BY name ORDER BY n DESC, name LIMIT ?1")?;
    let rows = stmt.query_map([limit as i64], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as usize)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Confidence counts over every reference of the given kinds in the repo.
pub fn resolution_summary(conn: &Connection, kinds: &[&str]) -> Result<RefSummary> {
    let mut stmt = conn.prepare("SELECT DISTINCT name FROM refs")?;
    let names: Vec<String> = stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    let mut total = RefSummary::default();
    for name in names {
        let (_, s) = refs(conn, &name, kinds, 0)?;
        total.total += s.total;
        total.via_di += s.via_di;
        total.precise += s.precise;
        total.corrected += s.corrected;
        for (k, v) in s.by_confidence {
            *total.by_confidence.entry(k).or_default() += v;
        }
    }
    Ok(total)
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct KeyType {
    pub qualified: String,
    pub kind: String,
    pub file: String,
    pub line: u32,
    /// Distinct other files that reference the type's name.
    pub used_by_files: u64,
}

/// Types used by the most other files (through resolved references): what a repo is built around.
pub fn key_types(conn: &Connection, edges: &[FileEdge], limit: usize) -> Result<Vec<KeyType>> {
    let mut used: HashMap<(&str, &str), u64> = HashMap::new();
    for e in edges {
        for n in &e.names {
            *used.entry((e.dst.as_str(), n.as_str())).or_default() += 1;
        }
    }
    let mut stmt = conn.prepare(
        "SELECT s.name, s.container, s.kind, f.path, s.start_line FROM symbols s JOIN files f ON f.id = s.file_id
         WHERE s.kind IN ('class', 'struct', 'enum', 'actor', 'protocol')",
    )?;
    let mut out: Vec<KeyType> = stmt
        .query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?, r.get::<_, u32>(4)?))
        })?
        .filter_map(|r| r.ok())
        .filter_map(|(name, container, kind, file, line)| {
            let n = used.get(&(file.as_str(), name.as_str())).copied().unwrap_or(0);
            (n > 0).then(|| KeyType { qualified: join(container.as_deref(), &name), kind, file, line, used_by_files: n })
        })
        .collect();
    out.sort_by(|a, b| b.used_by_files.cmp(&a.used_by_files).then_with(|| (&a.qualified, &a.file, a.line).cmp(&(&b.qualified, &b.file, b.line))));
    out.truncate(limit);
    Ok(out)
}

/// Symbols carrying a domain tag (`entrypoint`, `swiftui_view`, …).
pub fn tagged(conn: &Connection, tag: &str, limit: usize) -> Result<(Vec<SymbolRow>, usize)> {
    let sql = format!(
        "SELECT {SYMBOL_COLS} FROM symbols s JOIN files f ON f.id = s.file_id
         WHERE ',' || s.tags || ',' LIKE '%,' || ?1 || ',%' ORDER BY f.path, s.start_line"
    );
    let mut stmt = conn.prepare(&sql)?;
    let all: Vec<SymbolRow> = stmt.query_map([tag], symbol_row)?.collect::<rusqlite::Result<_>>()?;
    let n = all.len();
    Ok((all.into_iter().take(limit).collect(), n))
}

/// A resolved dependency between two files: `weight` distinct names `src` uses from `dst`.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct FileEdge {
    pub src: String,
    pub dst: String,
    pub weight: u32,
    /// The names `src` uses from `dst`.
    pub names: Vec<String>,
}

/// File-to-file dependencies from references that resolve `exact` or `unique_name`. Expensive (every reference is
/// resolved), so callers cache it per index generation.
pub fn file_edges(conn: &Connection) -> Result<Vec<FileEdge>> {
    let mut stmt = conn.prepare("SELECT DISTINCT name FROM refs")?;
    let names: Vec<String> = stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    let mut target_file: HashMap<i64, String> = HashMap::new();
    let mut file_stmt = conn.prepare_cached("SELECT f.path FROM symbols s JOIN files f ON f.id = s.file_id WHERE s.id = ?1")?;
    let mut edges: HashMap<(String, String), std::collections::HashSet<String>> = HashMap::new();
    for name in names {
        let (hits, _) = refs(conn, &name, &[], usize::MAX)?;
        for h in hits {
            if !matches!(h.confidence, "exact" | "unique_name") {
                continue;
            }
            for &id in &h.target_ids {
                target_file.entry(id).or_insert_with(|| {
                    let f: String = file_stmt.query_row([id], |r| r.get(0)).unwrap_or_default();
                    f
                });
                let dst = &target_file[&id];
                if !dst.is_empty() && *dst != h.file {
                    edges.entry((h.file.clone(), dst.clone())).or_default().insert(name.clone());
                }
            }
        }
    }
    let mut out: Vec<FileEdge> =
        edges
            .into_iter()
            .map(|((src, dst), names)| {
                let mut names: Vec<String> = names.into_iter().collect();
                names.sort();
                FileEdge { src, dst, weight: names.len() as u32, names }
            })
            .collect();
    out.sort_by(|a, b| (&a.src, &a.dst).cmp(&(&b.src, &b.dst)));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_sites_keep_the_imports_order() {
        let row = |f: &str, m: &str, l: u32| (f.to_string(), m.to_string(), l);
        let imports = [row("a.cs", "Lib.Data", 1), row("a.cs", "System", 2), row("b.cs", "Lib", 1), row("c.cs", "Lib.Data", 3), row("c.cs", "Library", 4)];
        let sites = ImportSites::new(&imports);
        let lines = |mods: &[&str]| -> Vec<(String, u32)> {
            let mods: Vec<String> = mods.iter().map(|m| m.to_string()).collect();
            sites.matching(&mods).map(|(f, _, l)| (f.clone(), *l)).collect()
        };
        assert_eq!(lines(&["Lib"]), [("a.cs".to_string(), 1), ("b.cs".to_string(), 1), ("c.cs".to_string(), 3)]);
        assert_eq!(lines(&["System", "Library"]), [("a.cs".to_string(), 2), ("c.cs".to_string(), 4)]);
        assert!(lines(&["Other"]).is_empty());
    }

    #[test]
    fn imports_match_a_module_or_anything_inside_it() {
        assert!(imports_module("Foundation", "Foundation"));
        assert!(imports_module("Acme.Common.Data.Models", "Acme.Common.Data"));
        assert!(imports_module("@acme/ui/button", "@acme/ui"));
        assert!(!imports_module("Acme.Common.DataAccess", "Acme.Common.Data"));
        assert!(!imports_module("@acme/uikit", "@acme/ui"));
        assert!(!imports_module("Acme.Common", "Acme.Common.Data"));
    }

    #[test]
    fn selector_parts_keep_element_and_attribute_conditions() {
        let part = |e: Option<&str>, a: &[&str], n: &[&str]| SelectorPart {
            element: e.map(str::to_string),
            attrs: a.iter().map(|s| s.to_string()).collect(),
            not: n.iter().map(|s| s.to_string()).collect(),
        };
        assert_eq!(
            selector_parts("nb-badge, [nbBadge], button[uiButton], ui-hint:not([validationFor]), .x, div, [data-x=\"y\"]"),
            [
                part(Some("nb-badge"), &[], &[]),
                part(None, &["nbBadge"], &[]),
                part(Some("button"), &["uiButton"], &[]),
                part(Some("ui-hint"), &[], &["validationFor"]),
            ]
        );
        let paged = &selector_parts("ui-paginator[withCursors]")[0];
        assert_eq!(paged.key(), Some(("renders", "ui-paginator")));
        let attrs = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(paged.matches("ui-paginator", &attrs(&["withCursors", "class"])));
        assert!(!paged.matches("ui-paginator", &attrs(&["class"])));
        let hint = &selector_parts("ui-hint:not([validationFor])")[0];
        assert!(hint.matches("ui-hint", &[]) && !hint.matches("ui-hint", &attrs(&["validationFor"])));
        assert_eq!(selector_parts("a[uiTab][routerLink]")[0].key(), Some(("attr", "uiTab")));
    }

    /// An action's prefix is the nearest `[Route]` up its class's bases (a constant resolved, `[controller]` its own
    /// class); with none, conventional routes place an action without a template, and a public method with no
    /// route attribute only then.
    #[test]
    fn controller_actions_take_inherited_and_conventional_prefixes() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE constants (file_id INTEGER, qualified TEXT, value TEXT, line INTEGER);
             INSERT INTO constants VALUES (1, 'Routes.Admin', 'admin/[controller]', 1);
             CREATE TABLE symbols (id INTEGER, name TEXT, kind TEXT);
             CREATE TABLE relations (file_id INTEGER, from_id INTEGER, target TEXT, target_name TEXT, kind TEXT, line INTEGER);
             INSERT INTO symbols VALUES (1, 'ItemsController', 'class'), (2, 'VersionedController', 'class'), (3, 'UsersController', 'class');
             INSERT INTO relations VALUES (1, 1, 'Api.VersionedController', 'VersionedController', 'inherits', 1),
               (1, 2, 'ApiControllerBase', 'ApiControllerBase', 'inherits', 1), (1, 3, 'AdminBase', 'AdminBase', 'inherits', 1);",
        )
        .unwrap();
        let row = |side: &str, path: &str, path_ref: Option<&str>, name: &str| {
            ((side.to_string(), String::new(), path.to_string(), path_ref.map(str::to_string)), (None, None), (Some(name.to_string()), None, None, String::new()), (None, None, 1))
        };
        let rows = [
            row("prefix", "api/v{version}/[controller]", None, "ApiControllerBase"),
            row("prefix", "", Some("Routes.Admin"), "AdminBase"),
            row("conventional", "{controller=Home}/{action=Index}/{id?}", None, "Configure"),
        ];
        let ctl = Controllers::new(&conn, &rows).unwrap();
        let mut c = Constants::new(&conn).unwrap();
        let mut paths = |class: &str, encoded: &str| ctl.paths(&mut c, class, encoded).unwrap();
        // Two bases up; `[controller]` is the subclass.
        assert_eq!(paths("ItemsController", "{^Latest}/Latest"), ["api/v{version}/Items/Latest"]);
        assert_eq!(paths("UsersController", "{^List}"), ["admin/Users"]);
        // Under an inherited prefix, a method with no route attribute isn't an action.
        assert!(paths("ItemsController", "{^*Helper}").is_empty());
        // No prefix up the bases: conventional routes, for actions without a template.
        assert_eq!(paths("HomeController", "{^*Privacy}"), ["Home/Privacy", "Home/Privacy/{id}"]);
        assert_eq!(paths("HomeController", "{^Save}"), ["Home/Save", "Home/Save/{id}"]);
        assert_eq!(paths("HomeController", "{^About}/about"), ["about"]);
        assert_eq!(conventional_paths("api/{controller}/{id}", "Orders", "Get"), ["api/Orders", "api/Orders/{id}"]);
    }

    #[test]
    fn client_paths_resolve_constants_and_drop_base_addresses() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE constants (file_id INTEGER, qualified TEXT, value TEXT, line INTEGER);
             INSERT INTO constants VALUES (1, 'environment.apiUrl', '/api', 1), (2, 'environment.apiUrl', 'http://localhost:5000/api', 1),
               (1, 'API', '{=environment.apiUrl}', 3), (1, 'Api.download', 'Domain/Download', 4);",
        )
        .unwrap();
        let mut c = Constants::new(&conn).unwrap();
        let mut path = |p: &str| client_path(&c.expand(p, 0).unwrap());
        // Both environments' base URLs agree once their hosts are dropped.
        assert_eq!(path("{=API}/orders/{}").as_deref(), Some("api/orders/{}"));
        // An unknown constant at the start is a base address; in the middle, a parameter.
        assert_eq!(path("{=config.apiUrl}/{=Api.download}").as_deref(), Some("Domain/Download"));
        assert_eq!(path("orders/{=x}/lines").as_deref(), Some("orders/{}/lines"));
        assert_eq!(path("https://h.example/api/x?y=1").as_deref(), Some("api/x"));
        // Only parameters left: no path.
        assert_eq!(path("{=nope}/{}"), None);
    }
}
