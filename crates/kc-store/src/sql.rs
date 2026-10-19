//! SQL objects: resolving references in `.sql` files, and what `context` says about a table, view or routine.
//!
//! References from SQL resolve only to SQL definitions, and code in other languages never resolves to SQL
//! definitions by name (a C# method calls a stored procedure through SQL text, linked separately). Names match without
//! case. A reference resolves:
//!
//! - `exact`: to the definitions in the schema it names (`db.t`), or else in the schema of the routine or view making
//!   it, or else in its own file;
//! - `unique_name`: to the one schema defining the name, or the one nearest the reference in the tree;
//! - `ambiguous`: to several schemas' definitions.
//!
//! The same schema dumped in several places (one copy per server) gives one definition per copy; of those, the copies
//! nearest the reference in the tree are kept.

use std::collections::HashMap;

use anyhow::Result;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::query::{RefHit, SymbolRow};

/// SQL symbol kinds that other code links to: tables, views and routines.
pub const OBJECT_KINDS: &[&str] = &["table", "view", "procedure", "function"];

/// Whether `path` is a SQL file.
pub fn is_sql_path(path: &str) -> bool {
    path.rsplit_once('.').is_some_and(|(_, e)| e.eq_ignore_ascii_case("sql"))
}

/// Whether `sym` is a SQL table or view, reached by `read`, `write` and `type_ref` references.
pub fn is_relation(sym: &SymbolRow) -> bool {
    matches!(sym.kind.as_str(), "table" | "view") && is_sql_path(&sym.file)
}

/// Whether `sym` is a SQL table, view, procedure or function.
pub fn is_object(sym: &SymbolRow) -> bool {
    OBJECT_KINDS.contains(&sym.kind.as_str()) && is_sql_path(&sym.file)
}

/// The reference kinds reaching a symbol of `kind` defined in `file`, when it is a SQL table or view.
pub fn relation_ref_kinds(kind: &str, file: &str) -> Option<&'static [&'static str]> {
    (matches!(kind, "table" | "view") && is_sql_path(file)).then_some(&["read", "write", "type_ref"])
}

#[derive(Debug, Clone)]
struct Def {
    id: i64,
    kind: String,
    /// The schema, lower-cased.
    schema: Option<String>,
    file: String,
}

/// Resolves references made in SQL files, caching definitions by name.
#[derive(Default)]
pub(crate) struct Resolver {
    defs: HashMap<String, Vec<Def>>,
}

impl Resolver {
    fn defs(&mut self, conn: &Connection, name: &str) -> Result<&[Def]> {
        let key = name.to_lowercase();
        if !self.defs.contains_key(&key) {
            // `symbols.name` compares without case.
            let mut stmt = conn.prepare_cached(
                "SELECT s.id, s.kind, s.container, f.path FROM symbols s JOIN files f ON f.id = s.file_id
                 WHERE s.name = ?1 AND f.lang = 'sql' AND s.kind IN ('table', 'view', 'procedure', 'function')
                 ORDER BY f.path, s.start_byte",
            )?;
            let rows = stmt.query_map([&key], |r| {
                let schema: Option<String> = r.get(2)?;
                Ok(Def { id: r.get(0)?, kind: r.get(1)?, schema: schema.map(|s| s.to_lowercase()), file: r.get(3)? })
            })?;
            let defs = rows.collect::<rusqlite::Result<Vec<_>>>()?;
            self.defs.insert(key.clone(), defs);
        }
        Ok(&self.defs[&key])
    }

    /// The confidence and candidates for `hit`, a reference in a SQL file.
    pub(crate) fn resolve(&mut self, conn: &Connection, hit: &RefHit) -> Result<(&'static str, Vec<i64>)> {
        let wanted: &[&str] = match hit.kind.as_str() {
            "call" => &["procedure", "function"],
            "read" | "write" | "type_ref" => &["table", "view"],
            _ => return Ok(("unresolved", Vec::new())),
        };
        let cands: Vec<Def> = self.defs(conn, &hit.name)?.iter().filter(|d| wanted.contains(&d.kind.as_str())).cloned().collect();
        Ok(pick(hit, cands))
    }
}

fn pick(hit: &RefHit, cands: Vec<Def>) -> (&'static str, Vec<i64>) {
    if cands.is_empty() {
        return ("unresolved", Vec::new());
    }
    let in_schema = |s: &str| -> Vec<&Def> { cands.iter().filter(|d| d.schema.as_deref() == Some(s)).collect() };
    if let Some(schema) = hit.receiver.as_deref() {
        let found = in_schema(schema);
        if !found.is_empty() {
            return ("exact", nearest(&found, &hit.file));
        }
        // A schema the index has no definitions in: only definitions whose schema isn't known may be meant.
        let unknown: Vec<&Def> = cands.iter().filter(|d| d.schema.is_none()).collect();
        return grouped(&unknown, &hit.file);
    }
    let own = hit.from.as_ref().and_then(|f| f.container.as_deref()).map(str::to_lowercase);
    if let Some(own) = own {
        let found = in_schema(&own);
        if !found.is_empty() {
            return ("exact", nearest(&found, &hit.file));
        }
    }
    let same_file: Vec<&Def> = cands.iter().filter(|d| d.file == hit.file).collect();
    if !same_file.is_empty() {
        return ("exact", same_file.iter().map(|d| d.id).collect());
    }
    grouped(&cands.iter().collect::<Vec<_>>(), &hit.file)
}

/// `unique_name` when the candidates are one schema's, or those nearest `file` are; `ambiguous` otherwise.
fn grouped(cands: &[&Def], file: &str) -> (&'static str, Vec<i64>) {
    let schemas = |ds: &[&Def]| {
        let mut s: Vec<Option<&str>> = ds.iter().map(|d| d.schema.as_deref()).collect();
        s.sort();
        s.dedup();
        s.len()
    };
    match schemas(cands) {
        0 => ("unresolved", Vec::new()),
        1 => ("unique_name", nearest(cands, file)),
        _ => {
            let best = cands.iter().map(|d| common_dirs(&d.file, file)).max().unwrap_or(0);
            let near: Vec<&Def> = cands.iter().copied().filter(|d| common_dirs(&d.file, file) == best).collect();
            if best > 0 && schemas(&near) == 1 {
                ("unique_name", near.iter().map(|d| d.id).collect())
            } else {
                ("ambiguous", cands.iter().map(|d| d.id).collect())
            }
        }
    }
}

/// Of definitions of one name, those nearest `file` in the tree: one copy of a schema dumped in several places.
fn nearest(cands: &[&Def], file: &str) -> Vec<i64> {
    let best = cands.iter().map(|d| common_dirs(&d.file, file)).max().unwrap_or(0);
    cands.iter().filter(|d| common_dirs(&d.file, file) == best).map(|d| d.id).collect()
}

/// The number of leading directories two paths share.
fn common_dirs(a: &str, b: &str) -> usize {
    let dirs = |p: &str| p.rsplit_once('/').map_or("", |(d, _)| d).to_string();
    let (a, b) = (dirs(a), dirs(b));
    if a.is_empty() || b.is_empty() {
        return 0;
    }
    a.split('/').zip(b.split('/')).take_while(|(x, y)| x == y).count()
}

/// A SQL name as written (`[dbo].[Orders]`, `` `shop`.`orders` ``, `"x"`) split into its schema and name, unquoted and
/// lower-cased. `dbo` (T-SQL's default schema) counts as no schema.
pub fn split_name(written: &str) -> (Option<String>, String) {
    let clean: String = written.chars().filter(|c| !matches!(c, '[' | ']' | '`' | '"')).collect();
    let clean = clean.trim().to_lowercase();
    let mut parts: Vec<&str> = clean.split('.').filter(|p| !p.is_empty()).collect();
    let name = parts.pop().unwrap_or("").to_string();
    let schema = parts.pop().filter(|s| *s != "dbo").map(str::to_string);
    (schema, name)
}

/// A SQL table, view, procedure or function a repo defines, for the workspace summary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SqlDef {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    pub name: String,
    pub file: String,
    pub line: u32,
    /// Further definitions of the same kind, schema and name in the repo (one schema dumped in several places).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub copies: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

impl SqlDef {
    /// `schema.name`, or the name.
    pub fn qualified(&self) -> String {
        match &self.schema {
            Some(s) => format!("{s}.{}", self.name),
            None => self.name.clone(),
        }
    }
}

/// Whether `path` is in a directory holding secondary copies of a schema: a data warehouse or replica (a directory
/// whose name contains `warehouse` or `replica`, or is `dw` or `dwh`), or an archive, backup or legacy copy (a name
/// starting `archive`, `backup` or `legacy`; `old`), compared without case. Such a copy is a poor representative of
/// a schema dumped in several places.
pub fn secondary_copy_path(path: &str) -> bool {
    let dirs = path.rsplit_once('/').map_or("", |(d, _)| d);
    dirs.split('/').map(str::to_ascii_lowercase).any(|d| {
        d.contains("warehouse")
            || d.contains("replica")
            || matches!(d.as_str(), "dw" | "dwh" | "old")
            || ["archive", "backup", "legacy"].iter().any(|p| d.starts_with(p))
    })
}

/// The repo's SQL tables, views and routines, one per kind, schema and name (compared without case). Of a schema
/// dumped in several places, the representative is a copy that stores data (not an `ENGINE=BLACKHOLE` replica table)
/// outside a secondary copy's directory ([`secondary_copy_path`]), then the first by path.
pub fn defs(conn: &Connection) -> Result<Vec<SqlDef>> {
    let mut stmt = conn.prepare(
        "SELECT s.kind, s.container, s.name, f.path, s.start_line, s.tags FROM symbols s JOIN files f ON f.id = s.file_id
         WHERE f.lang = 'sql' AND s.kind IN ('table', 'view', 'procedure', 'function') ORDER BY f.path, s.start_line",
    )?;
    let rows = stmt.query_map([], |r| {
        let tags: String = r.get(5)?;
        let def = SqlDef { kind: r.get(0)?, schema: r.get(1)?, name: r.get(2)?, file: r.get(3)?, line: r.get(4)?, copies: 0 };
        Ok((tags.split(',').any(|t| t == "blackhole"), def))
    })?;
    let mut rows: Vec<(bool, SqlDef)> = rows.collect::<rusqlite::Result<_>>()?;
    // Stable: by path within each rank.
    rows.sort_by_key(|(blackhole, d)| (*blackhole, secondary_copy_path(&d.file)));
    let mut out: Vec<SqlDef> = Vec::new();
    let mut at: HashMap<(String, Option<String>, String), usize> = HashMap::new();
    for (_, d) in rows {
        let key = (d.kind.clone(), d.schema.as_ref().map(|s| s.to_lowercase()), d.name.to_lowercase());
        match at.get(&key) {
            Some(&i) => out[i].copies += 1,
            None => {
                at.insert(key, out.len());
                out.push(d);
            }
        }
    }
    out.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
    Ok(out)
}

/// The names (bare, lower-cased) the repo's code may link to SQL definitions by: Entity Framework tables (mapped
/// names and DbSet names), called stored procedures, and names passed to procedure-running wrappers. For the workspace summary, so repos that can't link to a SQL
/// object are never opened to look.
pub fn linked_names(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT DISTINCT name FROM db_facts WHERE kind IN ('dbset', 'table', 'procedure', 'wrapped_procedure')")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    let mut out: Vec<String> = Vec::new();
    for row in rows {
        let name = split_name(&row?).1;
        if !name.is_empty() {
            out.push(name);
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// What `context` adds for a SQL table, view or routine: its columns, and the tables and routines it uses by kind.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SqlSection {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<kc_extract::sql::Column>,
    /// Tables and views its body reads, as written (`schema.name` when qualified).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub reads: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub writes: Vec<String>,
    /// Routines it calls.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<String>,
}

impl SqlSection {
    pub fn is_empty(&self) -> bool {
        self.columns.is_empty() && self.reads.is_empty() && self.writes.is_empty() && self.calls.is_empty()
    }
}

/// [`SqlSection`] for `sym` (a SQL object or trigger), whose `source` is its statement's text.
pub fn section(conn: &Connection, sym: &SymbolRow, source: &str) -> Result<SqlSection> {
    let mut out = SqlSection::default();
    if matches!(sym.kind.as_str(), "table" | "view") {
        out.columns = kc_extract::sql::columns(source);
    }
    let mut stmt = conn.prepare_cached("SELECT DISTINCT kind, receiver, name FROM refs WHERE from_id = ?1 ORDER BY line")?;
    let rows = stmt.query_map([sym.id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, String>(2)?)))?;
    for row in rows {
        let (kind, receiver, name) = row?;
        let written = match receiver {
            Some(r) => format!("{r}.{name}"),
            None => name,
        };
        let list = match kind.as_str() {
            "read" => &mut out.reads,
            "write" => &mut out.writes,
            "call" => &mut out.calls,
            _ => continue,
        };
        if !list.contains(&written) {
            list.push(written);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::FromSymbol;

    fn def(id: i64, schema: Option<&str>, file: &str) -> Def {
        Def { id, kind: "table".into(), schema: schema.map(str::to_string), file: file.into() }
    }

    fn hit(receiver: Option<&str>, from_schema: Option<&str>, file: &str) -> RefHit {
        RefHit {
            name: "orders".into(),
            kind: "read".into(),
            receiver: receiver.map(str::to_string),
            receiver_type: None,
            file: file.into(),
            line: 1,
            col: 0,
            from: Some(FromSymbol { id: 99, name: "p".into(), kind: "procedure".into(), container: from_schema.map(str::to_string) }),
            confidence: "unresolved",
            targets: Vec::new(),
            repo: None,
            receiver_chain: Vec::new(),
            target_ids: Vec::new(),
            targets_total: None,
            via_di: None,
            via_di_any: Vec::new(),
            usage: None,
            ctors: None,
            args: None,
            arg_types: None,
            assigns: false,
            precise: false,
            corrected_from: None,
            code: None,
            factory: false,
            through: None,
            basis: None,
            evidence: None,
        }
    }

    #[test]
    fn schemas_then_the_nearest_copy() {
        let defs = || {
            vec![
                def(1, Some("shop"), "dumps/a/shop.sql"),
                def(2, Some("shop"), "dumps/b/shop.sql"),
                def(3, Some("archive"), "dumps/a/archive.sql"),
                def(4, None, "migrations/001.sql"),
            ]
        };
        // The routine's own schema, in its own dump.
        assert_eq!(pick(&hit(None, Some("Shop"), "dumps/b/shop.Procs.sql"), defs()), ("exact", vec![2]));
        // A schema named: that schema's copy nearest the reference.
        assert_eq!(pick(&hit(Some("archive"), Some("shop"), "dumps/b/shop.Procs.sql"), defs()), ("exact", vec![3]));
        // A schema with no definitions: only those of unknown schema can be meant.
        assert_eq!(pick(&hit(Some("other"), None, "x.sql"), defs()), ("unique_name", vec![4]));
        // No schema to go by: several schemas define it; the nearest one if there is one.
        assert_eq!(pick(&hit(None, None, "dumps/a/views.sql"), defs()).0, "ambiguous");
        assert_eq!(pick(&hit(None, None, "migrations/002.sql"), defs()), ("unique_name", vec![4]));
        assert_eq!(pick(&hit(None, None, "x.sql"), vec![def(5, Some("shop"), "s.sql")]), ("unique_name", vec![5]));
        assert_eq!(pick(&hit(None, None, "x.sql"), Vec::new()).0, "unresolved");
    }

    #[test]
    fn secondary_copies_by_directory() {
        assert!(secondary_copy_path("Warehouse/shop.sql"));
        assert!(secondary_copy_path("db/dwh/shop.sql"));
        assert!(secondary_copy_path("Archive-A/shop.sql"));
        assert!(secondary_copy_path("x/legacy/shop.Procs.sql"));
        assert!(!secondary_copy_path("api/shop.sql"));
        assert!(!secondary_copy_path("shop.sql"));
        assert!(!secondary_copy_path("oldies/shop.sql"));
    }

    #[test]
    fn names_split_without_quotes_or_case() {
        assert_eq!(split_name("[dbo].[Orders]"), (None, "orders".to_string()));
        assert_eq!(split_name("`Shop`.`Orders`"), (Some("shop".to_string()), "orders".to_string()));
        assert_eq!(split_name("Orders"), (None, "orders".to_string()));
        assert_eq!(split_name("db.dbo.usp_x"), (None, "usp_x".to_string()));
    }
}
