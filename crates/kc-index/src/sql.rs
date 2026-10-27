//! Links code to the SQL defining what it uses: an Entity Framework table to the `CREATE TABLE` (or view) that defines
//! it, and a stored procedure call to the `CREATE PROCEDURE`, in the same repo or any other indexed repo (database
//! repos are often separate).
//!
//! Names match without case, quotes or brackets; when both sides name a schema, it must match (`dbo` counts as none).
//! A link is:
//!
//! - `exact`: the repo itself defines the name in one schema, or the schema both sides name defines it;
//! - `unique_name`: one schema in the workspace defines it;
//! - `dominant`: several schemas define it, and one stands out as the database the code uses. For an Entity Framework
//!   table, first by its DbContext: the schema the context is named for (`RegistrarContext`, `RegistrarDbContext` →
//!   `registrar`; underscores aside), else the one defining strictly more of the context's tables than any other.
//!   Then, as for a procedure call, the one defining strictly more of the repo's tables and procedures than any other:
//!   the database the repo uses, as its connection string would say;
//! - `ambiguous`: several schemas define it and none stands out.
//!
//! The same schema dumped in several places of one repo counts once. Checkouts of one repo count once, as for HTTP
//! routes, and a repo whose checkout is gone not at all: an answer its definitions or uses would have joined names
//! it as missing.
//!
//! A schema stands for the database it is in. The code's database is checked only when both sides name the schema,
//! the repo's own SQL defines the name, or the context's connection setting names the database; any other link
//! assumes `same_database` (see [`SqlLink::assumptions`]).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use kc_store::sql::{SqlDef, split_name};

use crate::workspace::RepoSummary;

/// A SQL definition that code links to.
#[derive(Debug, Clone)]
pub struct SqlLink {
    pub repo: PathBuf,
    pub def: SqlDef,
    pub confidence: &'static str,
    /// What picked the schema: `schema` (both sides name it), `repo` (the repo's own SQL defines the name),
    /// `only` (one schema defines it), `connection` (the context's connection setting names it), `context` (the
    /// context's name), `context_tables` (it defines most of the context's tables), `repo_tables` (most of the repo's
    /// names), or empty (`ambiguous`).
    pub by: &'static str,
}

impl SqlLink {
    /// What the link did not check: `same_database` unless the database the code uses was named (`by` `schema`,
    /// `repo` or `connection`).
    pub fn assumptions(&self) -> Vec<kc_store::answer::Assumption> {
        match self.by {
            "schema" | "repo" | "connection" => Vec::new(),
            _ => vec![kc_store::answer::Assumption::SameDatabase],
        }
    }
}

/// The workspace's SQL definitions, indexed for linking. Build once per answer.
pub struct Sql<'a> {
    repos: Vec<&'a RepoSummary>,
    /// Definitions by lower-cased name: the repo (index into `repos`) and the definition.
    by_name: HashMap<String, Vec<(usize, &'a SqlDef)>>,
    /// Names each schema (repo, lower-cased schema) defines, computed when needed.
    schema_names: RefCell<HashMap<Schema, HashSet<String>>>,
    /// Repos left out because their checkout is gone (see [`crate::workspace::gone`]).
    gone: Vec<&'a RepoSummary>,
}

/// A schema of one repo: its index in [`Sql`]'s repos, and its lower-cased name.
type Schema = (usize, Option<String>);

/// The DbContext an Entity Framework table is declared in: its class name and the tables it maps (as written).
#[derive(Debug, Clone, Default)]
pub struct DbContextTables {
    pub name: String,
    pub tables: Vec<String>,
    /// The connection setting the context is registered with (see `kc_store::db::Table::connection`).
    pub connection: Option<String>,
    /// For a table named in SQL text: no context, and code using several connections often, so the repo's names
    /// don't pick among schemas.
    pub raw: bool,
}

impl DbContextTables {
    /// The context declaring `table` in `model`, with its tables; for a table named in SQL text, one marked `raw`;
    /// `None` for an entity mapped without a context.
    pub fn of(model: &kc_store::db::DbModel, table: &kc_store::db::Table) -> Option<DbContextTables> {
        if table.mapped_by == "sql" {
            return Some(DbContextTables { raw: true, ..Default::default() });
        }
        let name = table.context.clone()?;
        let tables = model.tables.iter().filter(|t| t.context.as_deref() == Some(name.as_str())).map(|t| t.table.clone()).collect();
        Some(DbContextTables { name, tables, connection: table.connection.clone(), raw: false })
    }

    /// The schema the context's name gives: `RegistrarContext`, `RegistrarDbContext`, `RegistrarDb` → `registrar`,
    /// lower-cased, without underscores.
    fn schema_stem(&self) -> String {
        let lower = self.name.rsplit('.').next().unwrap_or(&self.name).to_lowercase();
        let mut stem = lower.as_str();
        for suffix in ["dbcontext", "context"] {
            if let Some(s) = stem.strip_suffix(suffix) {
                stem = s;
                break;
            }
        }
        let stem = stem.strip_suffix("db").filter(|s| !s.is_empty()).unwrap_or(stem);
        stem.replace('_', "")
    }
}

/// The kinds an Entity Framework table maps to.
pub const TABLE_KINDS: &[&str] = &["table", "view"];
/// The kinds a stored procedure call runs.
pub const ROUTINE_KINDS: &[&str] = &["procedure", "function"];

impl<'a> Sql<'a> {
    /// The SQL definitions of `summaries`, one checkout per repo: `home`'s for its own repo, else the most recently
    /// indexed.
    pub fn new(summaries: &'a [RepoSummary], home: Option<&Path>) -> Sql<'a> {
        let home: Vec<PathBuf> = home.map(Path::to_path_buf).into_iter().collect();
        let repos: Vec<&RepoSummary> = crate::workspace::distinct(summaries, &home);
        let mut by_name: HashMap<String, Vec<(usize, &SqlDef)>> = HashMap::new();
        for (i, s) in repos.iter().enumerate() {
            for d in &s.sql {
                by_name.entry(d.name.to_lowercase()).or_default().push((i, d));
            }
        }
        Sql { repos, by_name, schema_names: RefCell::new(HashMap::new()), gone: crate::workspace::gone(summaries) }
    }

    fn index_of(&self, root: &Path) -> Option<usize> {
        self.repos.iter().position(|s| s.root == root)
    }

    /// The definitions of `kinds` that the name `written` (an Entity Framework table, `schema.table`, or a procedure
    /// as the code names it) in the repo at `root` links to, most likely first.
    pub fn definitions(&self, root: &Path, written: &str, kinds: &[&str]) -> Vec<SqlLink> {
        self.definitions_in(root, written, kinds, None)
    }

    /// [`Self::definitions`] for an Entity Framework table declared in `context`, which picks among schemas first.
    pub fn definitions_in(&self, root: &Path, written: &str, kinds: &[&str], context: Option<&DbContextTables>) -> Vec<SqlLink> {
        let (schema, name) = split_name(written);
        let mut cands: Vec<(usize, &SqlDef)> =
            self.by_name.get(&name).into_iter().flatten().filter(|(_, d)| kinds.contains(&d.kind.as_str())).copied().collect();
        let lower = |d: &SqlDef| d.schema.as_ref().map(|s| s.to_lowercase());
        let mut named_schema = false;
        if let Some(s) = &schema {
            let same: Vec<(usize, &SqlDef)> = cands.iter().copied().filter(|(_, d)| lower(d).as_ref() == Some(s)).collect();
            named_schema = !same.is_empty();
            cands = if named_schema { same } else { cands.into_iter().filter(|(_, d)| d.schema.is_none()).collect() };
        }
        let me = self.index_of(root);
        // Unless the repo defines the name itself, a gone repo defining it would have been a candidate.
        if !me.is_some_and(|m| cands.iter().any(|(i, _)| *i == m)) {
            crate::workspace::note_missing_if(self.gone.iter().copied(), |g| {
                g.sql.iter().any(|d| {
                    d.name.to_lowercase() == name
                        && kinds.contains(&d.kind.as_str())
                        && (d.schema.is_none() || schema.is_none() || lower(d) == schema)
                })
            });
        }
        if cands.is_empty() {
            return Vec::new();
        }
        let local = me.is_some_and(|m| cands.iter().any(|(i, _)| *i == m));
        if local {
            cands.retain(|(i, _)| Some(*i) == me);
        }
        let mut groups: Vec<Schema> = cands.iter().map(|(i, d)| (*i, lower(d))).collect();
        groups.sort();
        groups.dedup();
        let (confidence, by) = if groups.len() == 1 {
            match (named_schema, local) {
                (true, _) => ("exact", "schema"),
                (false, true) => ("exact", "repo"),
                _ => ("unique_name", "only"),
            }
        } else if let Some((g, by)) = context.and_then(|c| self.by_context(c, &groups)).or_else(|| {
            // Not by the repo's names for SQL text, nor for a context whose connection names another database.
            let vetoed = context.is_some_and(|c| c.raw || self.connection_elsewhere(c, &groups));
            me.filter(|_| !vetoed).and_then(|m| self.dominant(m, &groups)).map(|g| (g, "repo_tables"))
        }) {
            // Every repo's copy of the schema picked.
            cands.retain(|(_, d)| lower(d) == g);
            // A literal connection string names the database outright.
            let literal = by == "connection" && context.and_then(|c| c.connection.as_deref()).is_some_and(|c| c.starts_with("Database="));
            (if literal { "exact" } else { "dominant" }, by)
        } else {
            ("ambiguous", "")
        };
        let mut out: Vec<SqlLink> =
            cands.into_iter().map(|(i, d)| SqlLink { repo: self.repos[i].root.clone(), def: d.clone(), confidence, by }).collect();
        out.sort_by(|a, b| (&a.repo, &a.def.file, a.def.line).cmp(&(&b.repo, &b.def.file, b.def.line)));
        out
    }

    /// Of several schemas, the one `context`'s connection setting names, else the one it is named for, else the one
    /// defining strictly more of its tables than any other. Schemas compare by name: one schema copied into several
    /// repos counts once.
    fn by_context(&self, context: &DbContextTables, groups: &[Schema]) -> Option<(Option<String>, &'static str)> {
        if context.raw || self.connection_elsewhere(context, groups) {
            return None;
        }
        if let Some(db) = context.connection.as_deref().and_then(kc_store::db::connection_database) {
            let mut named: Vec<&Option<String>> = groups.iter().map(|g| &g.1).filter(|s| s.as_ref().is_some_and(|s| s.replace('_', "") == db)).collect();
            if named.is_empty() {
                named = groups.iter().map(|g| &g.1).filter(|s| s.as_ref().is_some_and(|s| kc_store::db::same_database(&db, s))).collect();
            }
            named.sort();
            named.dedup();
            if let [one] = named.as_slice() {
                return Some(((*one).clone(), "connection"));
            }
        }
        let stem = context.schema_stem();
        let mut named: Vec<&Option<String>> = groups.iter().map(|g| &g.1).filter(|s| s.as_ref().is_some_and(|s| s.replace('_', "") == stem)).collect();
        named.sort();
        named.dedup();
        if let [one] = named.as_slice() {
            return Some(((*one).clone(), "context"));
        }
        let mut tables: Vec<String> = context.tables.iter().map(|t| split_name(t).1).collect();
        tables.sort();
        tables.dedup();
        self.most(&tables, groups).map(|g| (g, "context_tables"))
    }

    /// Whether `context`'s connection setting names a database (`SearchDbConnection`) that none of `groups` is:
    /// the table is in a database these definitions don't show, or one named otherwise, so none is picked.
    fn connection_elsewhere(&self, context: &DbContextTables, groups: &[Schema]) -> bool {
        let Some(db) = context.connection.as_deref().and_then(kc_store::db::connection_database) else { return false };
        !groups.iter().any(|g| g.1.as_ref().is_some_and(|s| kc_store::db::same_database(&db, s)))
    }

    /// The schemas defining a definition of `kinds` named `name` (without its schema) in any repo, lower-cased,
    /// sorted; `None` for a definition in no schema.
    pub fn schemas_named(&self, name: &str, kinds: &[&str]) -> Vec<Option<String>> {
        let (_, bare) = split_name(name);
        let mut out: Vec<Option<String>> = self
            .by_name
            .get(&bare)
            .into_iter()
            .flatten()
            .filter(|(_, d)| kinds.contains(&d.kind.as_str()))
            .map(|(_, d)| d.schema.as_ref().map(|s| s.to_lowercase()))
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Of several schemas, the one defining strictly more of the repo's linked names (`db_names`) than any other.
    fn dominant(&self, me: usize, groups: &[Schema]) -> Option<Option<String>> {
        self.most(&self.repos[me].db_names, groups)
    }

    /// Of several schemas (by name, over the repos in `groups`), the one defining strictly more of `mine` (bare
    /// lower-cased names) than any other.
    fn most(&self, mine: &[String], groups: &[Schema]) -> Option<Option<String>> {
        let mut by_schema: Vec<(Option<String>, HashSet<String>)> = Vec::new();
        for g in groups {
            let mut cache = self.schema_names.borrow_mut();
            let names = cache.entry(g.clone()).or_insert_with(|| {
                self.repos[g.0]
                    .sql
                    .iter()
                    .filter(|d| d.schema.as_ref().map(|s| s.to_lowercase()) == g.1)
                    .map(|d| d.name.to_lowercase())
                    .collect()
            });
            match by_schema.iter_mut().find(|(s, _)| *s == g.1) {
                Some((_, set)) => set.extend(names.iter().cloned()),
                None => by_schema.push((g.1.clone(), names.clone())),
            }
        }
        let mut counts: Vec<(usize, Option<String>)> =
            by_schema.into_iter().map(|(s, names)| (mine.iter().filter(|n| names.contains(*n)).count(), s)).collect();
        counts.sort_by_key(|c| std::cmp::Reverse(c.0));
        match counts.as_slice() {
            [(best, s), rest @ ..] if *best > 0 && rest.first().is_none_or(|(n, _)| n < best) => Some(s.clone()),
            _ => None,
        }
    }

    /// Repos whose code may link to a definition named `name`: an Entity Framework table or a procedure call of
    /// that name, as last indexed.
    pub fn users(&self, name: &str) -> Vec<PathBuf> {
        let name = name.to_lowercase();
        crate::workspace::note_missing_if(self.gone.iter().copied(), |g| g.db_names.binary_search(&name).is_ok());
        self.repos.iter().filter(|s| s.db_names.binary_search(&name).is_ok()).map(|s| s.root.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn repo(root: &str, sql: Vec<(&str, Option<&str>, &str)>, db_names: &[&str]) -> RepoSummary {
        let sql: Vec<serde_json::Value> = sql
            .into_iter()
            .map(|(kind, schema, name)| json!({ "kind": kind, "schema": schema, "name": name, "file": format!("{}.sql", name), "line": 1 }))
            .collect();
        let mut names: Vec<String> = db_names.iter().map(|s| s.to_string()).collect();
        names.sort();
        serde_json::from_value(json!({ "root": root, "packages": [], "modules": [], "sql": sql, "db_names": names, "stamp": "" })).unwrap()
    }

    fn workspace() -> Vec<RepoSummary> {
        vec![
            repo(
                "/w/schemas",
                vec![
                    ("table", Some("shop"), "orders"),
                    ("table", Some("shop"), "customers"),
                    ("table", Some("archive"), "orders"),
                    ("table", Some("billing"), "invoice"),
                    ("procedure", Some("shop"), "place_order"),
                ],
                &[],
            ),
            repo("/w/api", vec![], &["customers", "orders", "place_order"]),
            repo("/w/local", vec![("table", None, "Invoice")], &["invoice"]),
            repo("/w/other", vec![], &["orders"]),
        ]
    }

    fn links(sql: &Sql, root: &str, written: &str, kinds: &[&str]) -> Vec<(String, String, &'static str)> {
        sql.definitions(Path::new(root), written, kinds)
            .into_iter()
            .map(|l| (l.repo.to_string_lossy().into_owned(), l.def.qualified(), l.confidence))
            .collect()
    }

    #[test]
    fn names_link_to_the_schema_that_defines_them() {
        let ws = workspace();
        let sql = Sql::new(&ws, Some(Path::new("/w/api")));
        let one = |r: &str, q: &str, c: &'static str| vec![(r.to_string(), q.to_string(), c)];
        // One schema defines it.
        assert_eq!(links(&sql, "/w/api", "[dbo].[Customers]", TABLE_KINDS), one("/w/schemas", "shop.customers", "unique_name"));
        assert_eq!(links(&sql, "/w/api", "PLACE_ORDER", ROUTINE_KINDS), one("/w/schemas", "shop.place_order", "unique_name"));
        // Two schemas define `orders`: the one defining more of the repo's names is its database.
        assert_eq!(links(&sql, "/w/api", "orders", TABLE_KINDS), one("/w/schemas", "shop.orders", "dominant"));
        assert_eq!(links(&sql, "/w/other", "orders", TABLE_KINDS).len(), 2);
        assert!(links(&sql, "/w/other", "orders", TABLE_KINDS).iter().all(|l| l.2 == "ambiguous"));
        // A schema named on both sides.
        assert_eq!(links(&sql, "/w/other", "archive.orders", TABLE_KINDS), one("/w/schemas", "archive.orders", "exact"));
        assert!(links(&sql, "/w/other", "elsewhere.orders", TABLE_KINDS).is_empty());
        // The repo's own definition first.
        assert_eq!(links(&sql, "/w/local", "invoice", TABLE_KINDS), one("/w/local", "Invoice", "exact"));
        // Procedures don't link to tables.
        assert!(links(&sql, "/w/api", "orders", ROUTINE_KINDS).is_empty());
        assert_eq!(sql.users("Orders"), [PathBuf::from("/w/api"), PathBuf::from("/w/other")]);
    }

    /// A link assumes `same_database` unless the database the code uses is named: by both sides' schema, by the
    /// repo's own SQL, or by the context's connection setting.
    #[test]
    fn links_not_naming_the_database_assume_it() {
        let ws = workspace();
        let sql = Sql::new(&ws, Some(Path::new("/w/api")));
        let assumed = |root: &str, written: &str| -> Vec<(String, Vec<kc_store::answer::Assumption>)> {
            sql.definitions(Path::new(root), written, TABLE_KINDS).into_iter().map(|l| (l.def.qualified(), l.assumptions())).collect()
        };
        let same = vec![kc_store::answer::Assumption::SameDatabase];
        // Only one schema defines it, the code names none: the database isn't checked.
        assert_eq!(assumed("/w/api", "customers"), [("shop.customers".to_string(), same.clone())]);
        // Dominant: preferred by the repo's names.
        assert_eq!(assumed("/w/api", "orders"), [("shop.orders".to_string(), same.clone())]);
        // Ambiguous: every candidate.
        assert!(assumed("/w/other", "orders").iter().all(|(_, a)| *a == same));
        // Named: both sides' schema, or the repo's own SQL.
        assert_eq!(assumed("/w/other", "archive.orders"), [("archive.orders".to_string(), Vec::new())]);
        assert_eq!(assumed("/w/local", "invoice"), [("Invoice".to_string(), Vec::new())]);
    }

    #[test]
    fn a_db_context_picks_its_schema_before_the_repo_does() {
        let ws = vec![
            repo(
                "/w/schemas",
                vec![
                    ("table", Some("registrar"), "category_history"),
                    ("table", Some("history"), "category_history"),
                    ("table", Some("history"), "activity"),
                    ("table", Some("history"), "events"),
                    ("table", Some("whois_data"), "contact"),
                    ("table", Some("whois_copy"), "contact"),
                    ("table", Some("ledger"), "entries"),
                    ("table", Some("ledger"), "lines"),
                    ("table", Some("journal"), "entries"),
                ],
                &[],
            ),
            // The same schema copied into a second repo.
            repo("/w/registrar-db", vec![("table", Some("registrar"), "category_history")], &[]),
            repo("/w/api", vec![], &["activity", "category_history", "contact", "entries", "events", "lines"]),
        ];
        let sql = Sql::new(&ws, Some(Path::new("/w/api")));
        let ctx = |name: &str, tables: &[&str]| DbContextTables { name: name.into(), tables: tables.iter().map(|t| t.to_string()).collect(), connection: None, raw: false };
        let links = |written: &str, c: Option<&DbContextTables>| -> Vec<(String, String, &'static str)> {
            sql.definitions_in(Path::new("/w/api"), written, TABLE_KINDS, c)
                .into_iter()
                .map(|l| (l.repo.to_string_lossy().into_owned(), l.def.qualified(), l.confidence))
                .collect()
        };
        // The repo as a whole uses `history` most; the context named for `registrar` links to it, in both repos.
        assert_eq!(links("category_history", None), [("/w/schemas".into(), "history.category_history".into(), "dominant")]);
        let registrar = ctx("RegistrarContext", &["category_history"]);
        assert_eq!(
            links("category_history", Some(&registrar)),
            [
                ("/w/registrar-db".into(), "registrar.category_history".into(), "dominant"),
                ("/w/schemas".into(), "registrar.category_history".into(), "dominant"),
            ]
        );
        // Named with an underscore in the schema, and a `DbContext` suffix.
        assert_eq!(links("contact", Some(&ctx("WhoIsDataDbContext", &["contact"])))[0].1, "whois_data.contact");
        // Not named for any: the schema defining most of the context's tables.
        assert_eq!(links("entries", Some(&ctx("AccountingContext", &["entries", "lines"])))[0].1, "ledger.entries");
        assert!(links("entries", Some(&ctx("AccountingContext", &["entries"]))).iter().all(|l| l.2 == "dominant" || l.2 == "ambiguous"));
        assert_eq!(ctx("Shop.Data.RegistrarDb", &[]).schema_stem(), "registrar");
        // The connection setting names the database before the context's name does.
        let on = |c: &str| DbContextTables { connection: Some(c.into()), ..registrar.clone() };
        let picked = |c: &DbContextTables| sql.definitions_in(Path::new("/w/api"), "category_history", TABLE_KINDS, Some(c));
        let l = picked(&on("HistoryDbConnection"));
        assert!(l.iter().all(|l| l.def.qualified() == "history.category_history" && l.confidence == "dominant" && l.by == "connection"));
        assert!(picked(&on("Database=history")).iter().all(|l| l.confidence == "exact" && l.by == "connection"));
        // The connection setting names the database: nothing assumed. The context's name only suggests it.
        assert!(picked(&on("HistoryDbConnection")).iter().all(|l| l.assumptions().is_empty()));
        assert!(picked(&on("DbConnection")).iter().all(|l| l.assumptions() == [kc_store::answer::Assumption::SameDatabase]));
        // A setting naming no database here leaves it to the context's name.
        assert!(picked(&on("DbConnection")).iter().all(|l| l.def.qualified() == "registrar.category_history" && l.by == "context"));
        // One naming a database neither schema is: none picked, by the context's name or the repo's names.
        assert!(picked(&on("ArchiveDbConnection")).iter().all(|l| l.confidence == "ambiguous"));
        // Names in SQL text don't go by the repo's names either.
        let raw = DbContextTables { raw: true, ..Default::default() };
        assert!(picked(&raw).iter().all(|l| l.confidence == "ambiguous"));
        assert_eq!(sql.definitions(Path::new("/w/api"), "category_history", TABLE_KINDS)[0].by, "repo_tables");
        assert_eq!(sql.schemas_named("Category_History", TABLE_KINDS), [Some("history".to_string()), Some("registrar".to_string())]);
    }
}
