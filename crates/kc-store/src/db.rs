//! Table access: which code reads or writes a table, and which tables a method touches. Tables come from Entity
//! Framework (below) and from SQL text (CFML queries: `sql_read` and `sql_write` facts, see `sql_tables`).
//!
//! Tables come from the extractor's `db_facts`. Each `DbSet<T>` property is a table, named by a `[Table]` attribute on
//! `T` or a `ToTable` call on `T`'s builder, else by the property (EF Core's convention). Entities mapped without a
//! DbSet are tables too. A symbol accesses a table when one of its references resolves to the DbSet property (the
//! usual resolution, so `_context.Orders` finds `Orders` through `_context`'s declared type) or when it calls
//! `Set<T>()`. A reference the resolver leaves ambiguous counts only when every candidate is a DbSet or the receiver
//! is named like a context (`_context`, `db`), so `request.Orders` on a DTO doesn't; among several contexts' sets, the
//! one nearest the reference in the tree is picked. The access writes when the symbol calls `Add`, `Update`, `Remove`
//! or a variant on the set, or `SaveChanges` after touching it, or assigns the set, a navigation or a mapped property
//! and persists the change itself (see [`Persist`]); it reads when it calls anything else on the set (a LINQ method,
//! `Find`, `Include`) or queries it (`from o in ctx.Orders`). The mode is `read`, `write`, `read_write` (both) or
//! just `access`.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use anyhow::Result;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::query::{self, REF_COLS, RefHit, SYMBOL_COLS, SymbolRow, ref_row, resolve_all, symbol_row};

/// Calls on a set (or a query over it) that change rows.
const WRITE_OPS: &[&str] = &[
    "Add", "AddAsync", "AddRange", "AddRangeAsync", "AddOrUpdate", "Attach", "AttachRange", "Update", "UpdateRange",
    "Remove", "RemoveRange", "RemoveAll", "Clear", "ExecuteDelete", "ExecuteDeleteAsync", "ExecuteUpdate", "ExecuteUpdateAsync", "BulkInsert",
    "BulkInsertAsync", "BulkUpdate", "BulkUpdateAsync", "BulkDelete", "BulkDeleteAsync", "BulkInsertOrUpdate",
    "BulkInsertOrUpdateAsync", "BulkMerge", "BulkMergeAsync", "Delete", "DeleteAsync", "DeleteFromQuery",
    "DeleteFromQueryAsync", "UpdateFromQuery", "UpdateFromQueryAsync",
];

/// Calls that reach the database at once, unlike staging a change: a save, a commit, a set-based or bulk operation.
/// `Delete` and `DeleteAsync` do only on a set or a query over one (Z.EntityFramework's; `File.Delete` and an HTTP
/// client's `DeleteAsync` don't): see [`SET_FLUSH_OPS`].
const FLUSH_OPS: &[&str] = &[
    "SaveChanges", "SaveChangesAsync", "BulkSaveChanges", "BulkSaveChangesAsync", "Commit", "CommitAsync",
    "ExecuteUpdate", "ExecuteUpdateAsync", "ExecuteDelete", "ExecuteDeleteAsync", "BulkInsert", "BulkInsertAsync",
    "BulkUpdate", "BulkUpdateAsync", "BulkDelete", "BulkDeleteAsync", "BulkInsertOrUpdate", "BulkInsertOrUpdateAsync",
    "BulkMerge", "BulkMergeAsync", "DeleteFromQuery", "DeleteFromQueryAsync", "UpdateFromQuery", "UpdateFromQueryAsync",
];

/// Calls that reach the database at once on a set or a query over one (`ctx.Orders.Where(…).Delete()`), and are
/// common names elsewhere.
const SET_FLUSH_OPS: &[&str] = &["Delete", "DeleteAsync"];

/// Calls that write tracked changes back.
const SAVE_OPS: &[&str] = &["SaveChanges", "SaveChangesAsync", "BulkSaveChanges", "BulkSaveChangesAsync"];

/// Calls that persist an entity whatever their receiver (a context, a set, a query, a repository): no in-memory
/// collection has them. `Add` and `AddRange` count only on a context or a set (see [`Persist::direct`]).
const PERSIST_OPS: &[&str] = &[
    "Update", "UpdateRange", "UpdateAsync", "Attach", "AttachRange", "AddAsync", "AddRangeAsync", "AddOrUpdate",
    "ExecuteUpdate", "ExecuteUpdateAsync", "BulkUpdate", "BulkUpdateAsync", "BulkInsert", "BulkInsertAsync",
    "BulkInsertOrUpdate", "BulkInsertOrUpdateAsync", "BulkMerge", "BulkMergeAsync",
];

/// Bumped when [`model`]'s output changes, so cached models are recomputed.
pub const MODEL_VERSION: u32 = 12;

/// Hops [`touched`] follows from a method by default.
pub const TOUCH_DEPTH: u8 = 4;

/// Symbols [`touched`] visits at most.
const TOUCH_NODES: usize = 400;

/// A table: an entity set of a DbContext, or an entity mapped without one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Table {
    /// The table name, `schema.table` when a schema is given.
    pub table: String,
    pub entity: String,
    /// The DbContext declaring the set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    /// The `DbSet` property.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dbset: Option<String>,
    /// The connection the DbContext is registered with: the setting or key its provider call names
    /// (`UseMySql(Settings.ShopDbConnection)` → `ShopDbConnection`), or `Database=x` from a literal connection
    /// string. Which database the table is in follows from it (see `kc_index::sql`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection: Option<String>,
    /// For a table of an owned type (`OwnsMany(o => o.Payments, p => p.ToTable("x"))`): the owner's navigation,
    /// `Order.Payments`, whose uses are the table's accessors (its property's id is `dbset_id`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub navigation: Option<String>,
    /// Where the table is declared: the DbSet property, else the mapping.
    pub file: String,
    pub line: u32,
    /// `attribute` (`[Table]`), `to_table` (`ToTable` / `ToView`), or `convention` (the DbSet's name).
    pub mapped_by: String,
    /// The `[Table]` or `ToTable` site, `file:line`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mapping: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dbset_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity_id: Option<i64>,
    /// Accessors reading the table, `read_write` ones included.
    pub reads: usize,
    /// Accessors writing the table, `read_write` ones included.
    pub writes: usize,
    /// Accessors both reading and writing (counted in `reads` and in `writes`).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub read_writes: usize,
    /// Accessors writing the table only through a writer they call (`via`): not counted in `writes`, which counts
    /// each write once, where it is made.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub writes_via: usize,
    /// Callers listed only for a write through a callee (`only_via`) left out of `accessors` by [`compact`], per
    /// callee.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hidden_via: Vec<HiddenVia>,
    /// Accessors whose access is neither an obvious read nor an obvious write.
    pub other: usize,
    pub accessors: Vec<Access>,
    /// How many accessors there are, when an answer lists fewer ([`Table::cap_accessors`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accessors_total: Option<usize>,
}

impl Table {
    /// Keeps the first `max` accessors, saying how many there were when that cuts the list.
    pub fn cap_accessors(&mut self, max: usize) {
        if self.accessors.len() > max {
            self.accessors_total = Some(self.accessors.len());
            self.accessors.truncate(max);
        }
    }
}

/// A symbol (usually a method) that touches a table directly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Access {
    pub id: i64,
    pub symbol: String,
    pub kind: String,
    pub file: String,
    /// The first line where it touches the table.
    pub line: u32,
    /// `read`, `write`, `read_write` (both) or `access`.
    pub mode: String,
    /// The reference's confidence (see `query`); `nearest` for a `Set<T>()` whose entity several contexts declare.
    pub confidence: String,
    /// A write through a method this one calls, one call deep. When the callee is a helper that changes rows
    /// without writing by itself, this symbol saves the change and the write is part of `mode` (`saved: here`).
    /// When the callee is a writer, saving its write itself (`saved: callee`) or leaving it to this symbol (an
    /// entity's own method, `saved: here`), the write is counted there: not in `mode`, but in the table's
    /// `writes_via`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<Via>,
    /// It changes the table's rows and a caller saves them (that caller's write carries `via` naming this symbol);
    /// not a write by itself.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub saved_by_caller: bool,
    /// Listed only for its write `via` a callee: it doesn't touch the table itself.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub only_via: bool,
}

impl Access {
    /// How the text answers qualify the mode: `  (write via X, saved here)`, `  (writes via X, which saves)`,
    /// `  (changes, saved by caller)`, or nothing.
    pub fn note(&self) -> String {
        match (&self.via, self.saved_by_caller) {
            (Some(v), _) if v.saved == "callee" => format!("  (writes via {}, which saves)", v.symbol),
            (Some(v), _) => format!("  (write via {}, saved here)", v.symbol),
            (None, true) => "  (changes, saved by caller)".into(),
            (None, false) => String::new(),
        }
    }
}

/// Callers of one callee left out of a table's accessors (see [`compact`]).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HiddenVia {
    /// The callee they write through.
    pub via: String,
    /// Callers in test code.
    pub tests: usize,
    /// Other callers past the cap.
    pub more: usize,
}

/// `only_via` callers kept per callee by [`compact`].
pub const VIA_ROWS: usize = 5;

/// Shortens a table's accessors for an answer: callers listed only for a write through a callee (`only_via`) are left
/// out when they are test code, and kept to `per_via` per callee otherwise; `hidden_via` counts what was left out.
/// Counts (`writes_via`) still cover them all.
pub fn compact(t: &mut Table, per_via: usize) {
    let mut kept: HashMap<String, usize> = HashMap::new();
    let mut hidden: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    t.accessors.retain(|a| {
        if !a.only_via {
            return true;
        }
        let via = a.via.as_ref().map(|v| v.symbol.clone()).unwrap_or_default();
        if crate::is_test_path(&a.file) {
            hidden.entry(via).or_default().0 += 1;
            return false;
        }
        let n = kept.entry(via.clone()).or_default();
        if *n < per_via {
            *n += 1;
            true
        } else {
            hidden.entry(via).or_default().1 += 1;
            false
        }
    });
    t.hidden_via.extend(hidden.into_iter().map(|(via, (tests, more))| HiddenVia { via, tests, more }));
}

/// The callee an [`Access`] writes through.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Via {
    pub symbol: String,
    /// The call, `file:line`.
    pub site: String,
    /// `here` (the accessor saves what the callee changed) or `callee` (the callee saves).
    pub saved: String,
}

/// A stored procedure call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Procedure {
    pub name: String,
    pub id: i64,
    pub symbol: String,
    pub file: String,
    pub line: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DbModel {
    pub tables: Vec<Table>,
    pub procedures: Vec<Procedure>,
    /// Names passed to methods that look like procedure-running wrappers (`ExecuteStoredProcedureAsync("p", …)`):
    /// calls of `p` only if a procedure `p` is defined somewhere, which the caller checks against SQL definitions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub wrapped: Vec<Procedure>,
}

impl DbModel {
    pub fn is_empty(&self) -> bool {
        self.tables.is_empty() && self.procedures.is_empty()
    }

    /// Tables whose name (with or without schema), entity or DbSet is `name`, case-insensitively.
    pub fn named(&self, name: &str) -> Vec<&Table> {
        let n = name.to_ascii_lowercase();
        self.tables
            .iter()
            .filter(|t| {
                let table = t.table.to_ascii_lowercase();
                table == n
                    || table.rsplit('.').next() == Some(n.as_str())
                    || t.entity.eq_ignore_ascii_case(name)
                    || t.dbset.as_deref().is_some_and(|d| d.eq_ignore_ascii_case(name))
            })
            .collect()
    }
}

struct Fact {
    kind: String,
    entity: String,
    name: String,
    line: u32,
    file: String,
    sym: Option<SymbolRow>,
}

/// Every table with its accessors, and every stored procedure call. Cached by `Store::db_model`.
pub fn model(conn: &Connection) -> Result<DbModel> {
    let sql = format!(
        "SELECT d.kind, d.entity, d.name, d.line, df.path, {SYMBOL_COLS}
         FROM db_facts d JOIN files df ON df.id = d.file_id
         LEFT JOIN symbols s ON s.id = d.symbol_id LEFT JOIN files f ON f.id = s.file_id
         ORDER BY df.path, d.line"
    );
    let mut stmt = conn.prepare(&sql)?;
    let facts: Vec<Fact> = stmt
        .query_map([], |r| {
            let id: Option<i64> = r.get(5)?;
            let sym = match id {
                Some(_) => Some(symbol_row_at(r, 5)?),
                None => None,
            };
            Ok(Fact { kind: r.get(0)?, entity: r.get(1)?, name: r.get(2)?, line: r.get(3)?, file: r.get(4)?, sym })
        })?
        .collect::<rusqlite::Result<_>>()?;
    if facts.is_empty() {
        return Ok(DbModel::default());
    }
    let sets: Vec<&Fact> = facts.iter().filter(|f| f.kind == "dbset" && f.sym.is_some()).collect();
    let contexts: HashSet<&str> = sets.iter().filter_map(|f| f.sym.as_ref()?.container.as_deref()).collect();
    let mappings: Vec<&Fact> = facts.iter().filter(|f| f.kind == "table").collect();
    let mut used = HashSet::new();
    let mut tables = Vec::new();
    for f in &sets {
        let prop = f.sym.as_ref().expect("filtered");
        let context = prop.container.clone();
        // A ToTable in another context's model configures that context's entity, not this one.
        let best = mappings
            .iter()
            .enumerate()
            .filter(|(_, m)| m.entity == f.entity)
            .filter(|(_, m)| {
                let owner = m.sym.as_ref().and_then(|s| s.container.as_deref());
                !owner.is_some_and(|o| contexts.contains(o) && Some(o) != context.as_deref())
            })
            .max_by_key(|(_, m)| {
                let own = m.sym.as_ref().and_then(|s| s.container.as_deref()) == context.as_deref();
                (own, common_prefix(&m.file, &f.file))
            });
        let mut t = Table {
            table: f.name.clone(),
            entity: f.entity.clone(),
            context,
            dbset: Some(f.name.clone()),
            connection: None,
            navigation: None,
            file: f.file.clone(),
            line: prop.line,
            mapped_by: "convention".into(),
            mapping: None,
            dbset_id: Some(prop.id),
            entity_id: None,
            reads: 0,
            writes: 0,
            read_writes: 0,
            writes_via: 0,
            hidden_via: Vec::new(),
            other: 0,
            accessors: Vec::new(),
            accessors_total: None,
        };
        if let Some((i, m)) = best {
            used.insert(i);
            apply_mapping(&mut t, m);
        }
        tables.push(t);
    }
    // Entities mapped without a DbSet in any context.
    for (i, m) in mappings.iter().enumerate() {
        if used.contains(&i) || tables.iter().any(|t| t.entity == m.entity && t.table == m.name) {
            continue;
        }
        let owner = m.sym.as_ref().and_then(|s| s.container.clone()).filter(|c| contexts.contains(c.as_str()));
        let mut t = Table {
            table: m.name.clone(),
            entity: m.entity.clone(),
            context: owner,
            dbset: None,
            connection: None,
            navigation: None,
            file: m.file.clone(),
            line: m.line,
            mapped_by: String::new(),
            mapping: None,
            dbset_id: None,
            entity_id: None,
            reads: 0,
            writes: 0,
            read_writes: 0,
            writes_via: 0,
            hidden_via: Vec::new(),
            other: 0,
            accessors: Vec::new(),
            accessors_total: None,
        };
        apply_mapping(&mut t, m);
        tables.push(t);
    }
    // Tables of owned types, through their owner's navigation.
    for m in facts.iter().filter(|f| f.kind == "owned_table") {
        let Some((owned, nav)) = owned_type(conn, &m.entity, &m.file)? else { continue };
        let owner = m.entity.split('#').next().unwrap_or_default();
        let context = tables
            .iter()
            .find(|t| t.entity == owner && t.context.is_some())
            .and_then(|t| t.context.clone())
            .or_else(|| m.sym.as_ref().and_then(|s| s.container.clone()).filter(|c| contexts.contains(c.as_str())));
        let mut t = Table {
            table: m.name.clone(),
            entity: owned,
            context,
            dbset: None,
            connection: None,
            navigation: Some(nav.qualified()),
            file: nav.file.clone(),
            line: nav.line,
            mapped_by: String::new(),
            mapping: None,
            dbset_id: Some(nav.id),
            entity_id: None,
            reads: 0,
            writes: 0,
            read_writes: 0,
            writes_via: 0,
            hidden_via: Vec::new(),
            other: 0,
            accessors: Vec::new(),
            accessors_total: None,
        };
        apply_mapping(&mut t, m);
        tables.push(t);
    }
    for t in &mut tables {
        let classes = query::definitions(conn, &t.entity)?;
        t.entity_id = classes
            .iter()
            .filter(|d| matches!(d.kind.as_str(), "class" | "struct"))
            .max_by_key(|d| common_prefix(&d.file, &t.file))
            .map(|d| d.id);
    }
    // Each context's connection, from its registration (`AddDbContext<C>(o => o.UseMySql(…))`) or `OnConfiguring`;
    // of several (one per environment, say), the first in file order.
    for t in &mut tables {
        let Some(context) = t.context.as_deref() else { continue };
        let short = context.rsplit('.').next().unwrap_or(context);
        t.connection = facts.iter().find(|f| f.kind == "connection" && f.entity == short).map(|f| f.name.clone());
    }
    let navs = navigations(conn, &tables)?;

    // Accessors, per table: (symbol id) -> (first line, confidence, segment as the receiver text names it).
    let mut found: Vec<BTreeMap<i64, (SymbolLite, u32, String, String)>> = vec![BTreeMap::new(); tables.len()];
    let dbset_ids: HashSet<i64> = tables.iter().filter_map(|t| t.dbset_id).collect();
    // The model configuration naming an owned type's navigation (`OwnsMany(p => p.Payments, …)`) doesn't access it.
    let configuring: HashSet<i64> = facts.iter().filter(|f| f.kind == "owned_table").filter_map(|f| Some(f.sym.as_ref()?.id)).collect();
    let mut refs_by_name: HashMap<String, Vec<RefHit>> = HashMap::new();
    let mut defined: HashMap<String, bool> = HashMap::new();
    for (i, t) in tables.iter().enumerate() {
        let source = t.dbset.clone().or_else(|| t.navigation.as_deref().and_then(|n| n.rsplit('.').next()).map(str::to_string));
        let (Some(name), Some(id)) = (&source, t.dbset_id) else { continue };
        if !refs_by_name.contains_key(name) {
            refs_by_name.insert(name.clone(), query::refs(conn, name, &["member", "call"], usize::MAX)?.0);
        }
        for h in &refs_by_name[name] {
            let Some(from) = &h.from else { continue };
            if matches!(h.confidence, "external_likely" | "unresolved") || !h.may_target(id) || dbset_ids.contains(&from.id) {
                continue;
            }
            if t.navigation.is_some() && configuring.contains(&from.id) {
                continue;
            }
            let mut confidence = h.confidence;
            // `Entities.Orders.Order`: a namespace, not a context, when the receiver's root names nothing in the repo.
            if confidence != "exact" {
                let root = h.receiver.as_deref().and_then(|r| r.split('.').next()).unwrap_or("");
                if root.starts_with(|c: char| c.is_ascii_uppercase()) {
                    if !defined.contains_key(root) {
                        defined.insert(root.to_string(), !query::definitions(conn, root)?.is_empty());
                    }
                    if !defined[root] {
                        continue;
                    }
                }
            }
            if confidence == "ambiguous" {
                let sets: Vec<&Table> = tables.iter().filter(|t| t.dbset_id.is_some_and(|d| h.target_ids.contains(&d))).collect();
                let all_sets = sets.len() == h.target_ids.len();
                if !all_sets && !h.receiver.as_deref().is_some_and(context_like) {
                    continue;
                }
                // Several contexts' sets: the one nearest the reference, or all of them (still ambiguous) on a tie.
                let closeness: Vec<usize> = sets.iter().map(|s| common_prefix(&s.file, &h.file)).collect();
                let nearest = closeness.iter().copied().max().unwrap_or(0);
                if common_prefix(&t.file, &h.file) < nearest {
                    continue;
                }
                if sets.len() > 1 && closeness.iter().filter(|&&c| c == nearest).count() == 1 {
                    confidence = "nearest";
                }
            }
            let lite = SymbolLite { qualified: join(from.container.as_deref(), &from.name), kind: from.kind.clone(), file: h.file.clone() };
            found[i].entry(from.id).or_insert((lite, h.line, confidence.to_string(), name.clone()));
        }
    }
    for f in facts.iter().filter(|f| f.kind == "set") {
        let Some(s) = &f.sym else { continue };
        if dbset_ids.contains(&s.id) {
            continue; // `DbSet<T> Items => Set<T>();` declares the set
        }
        let candidates: Vec<usize> = (0..tables.len()).filter(|&i| tables[i].entity == f.entity).collect();
        let Some(&best) = candidates.iter().max_by_key(|&&i| common_prefix(&tables[i].file, &f.file)) else { continue };
        let confidence = if candidates.len() == 1 { "exact" } else { "nearest" };
        let lite = SymbolLite { qualified: s.qualified(), kind: s.kind.clone(), file: f.file.clone() };
        found[best].entry(s.id).or_insert((lite, f.line, confidence.to_string(), format!("Set<{}>()", f.entity)));
    }
    let mut calls_of: HashMap<i64, Vec<(String, String, u32)>> = HashMap::new();
    let mut assigns_of: HashMap<i64, Vec<(String, String, u32)>> = HashMap::new();
    // Tables reached through a navigation, per (accessor, table): loaded by an `Include` (or lazily), or changed
    // through the parent (`order.Lines.Add(…)`, `order.Lines = …`).
    let mut loaded: HashSet<(i64, usize)> = HashSet::new();
    let mut changed: HashSet<(i64, usize)> = HashSet::new();
    // Accessors found only through a navigation (a navigation may share its set's name, `OrderLines`).
    let mut by_navigation: HashSet<(i64, usize)> = HashSet::new();
    // `Include(o => o.Lines)`: a read of the navigation's table. Of same-named navigations on several entities, the
    // ones on a table the method already queries (or includes, earlier in the chain).
    let mut includes: BTreeMap<i64, Vec<&Fact>> = BTreeMap::new();
    for f in facts.iter().filter(|f| f.kind == "include") {
        if let Some(s) = &f.sym {
            includes.entry(s.id).or_default().push(f);
        }
    }
    for (id, mut list) in includes {
        list.sort_by_key(|f| f.line);
        let mut reached: HashSet<usize> = (0..tables.len()).filter(|&i| found[i].contains_key(&id)).collect();
        for f in list {
            let cands: Vec<&Nav> = navs.iter().filter(|n| n.name == f.name).collect();
            let mut children: Vec<usize> = cands.iter().map(|n| n.child).collect();
            children.sort_unstable();
            children.dedup();
            if children.len() > 1 {
                let mut on_reached: Vec<usize> = cands.iter().filter(|n| reached.contains(&n.owner)).map(|n| n.child).collect();
                on_reached.sort_unstable();
                on_reached.dedup();
                if !on_reached.is_empty() {
                    children = on_reached;
                }
            }
            let confidence = if children.len() == 1 { "unique_name" } else { "ambiguous" };
            let s = f.sym.as_ref().expect("grouped by symbol");
            for c in children {
                let lite = SymbolLite { qualified: s.qualified(), kind: s.kind.clone(), file: f.file.clone() };
                if let std::collections::btree_map::Entry::Vacant(e) = found[c].entry(id) {
                    e.insert((lite, f.line, confidence.to_string(), f.name.clone()));
                    by_navigation.insert((id, c));
                }
                loaded.insert((id, c));
                reached.insert(c);
            }
        }
    }
    // Uses of each navigation: a change through it, a join in a query on the owner (or with lazy loading, any read
    // of it).
    let lazy = facts.iter().any(|f| f.kind == "lazy_loading");
    let lambda_members: HashSet<(i64, &str)> =
        facts.iter().filter(|f| f.kind == "lambda_member").filter_map(|f| Some((f.sym.as_ref()?.id, f.name.as_str()))).collect();
    for n in &navs {
        if !refs_by_name.contains_key(&n.name) {
            refs_by_name.insert(n.name.clone(), query::refs(conn, &n.name, &["member", "call"], usize::MAX)?.0);
        }
        for h in &refs_by_name[&n.name] {
            let Some(from) = &h.from else { continue };
            // Resolved to this navigation, or to navigations of this table only.
            let to_child = |t: &i64| navs.iter().any(|m| m.prop == *t && m.child == n.child);
            let resolved = matches!(h.confidence, "exact" | "unique_name")
                || (h.confidence == "ambiguous" && !h.target_ids.is_empty() && h.target_ids.iter().all(to_child));
            if !resolved || !h.may_target(n.prop) || configuring.contains(&from.id) {
                continue;
            }
            if let std::collections::hash_map::Entry::Vacant(e) = calls_of.entry(from.id) {
                e.insert(calls_from(conn, from.id)?);
            }
            if let std::collections::hash_map::Entry::Vacant(e) = assigns_of.entry(from.id) {
                e.insert(assignments_from(conn, from.id)?);
            }
            let mutates = calls_of[&from.id].iter().any(|(name, recv, _)| COLLECTION_CHANGES.contains(&name.as_str()) && mentions(recv, &n.name))
                // Assigning a reference navigation sets the owner's foreign key, not the child's row.
                || (h.assigns && n.collection)
                || assigns_of[&from.id].iter().any(|(_, recv, _)| mentions(recv, &n.name));
            let lazily = lazy && n.lazy && !h.assigns;
            // `ctx.Orders.Where(o => o.Customer.Vip)`: a query on the owner's table naming the navigation in a query
            // lambda joins the child's.
            let joined = !h.assigns
                && lambda_members.contains(&(from.id, n.name.as_str()))
                && found[n.owner].contains_key(&from.id)
                && !by_navigation.contains(&(from.id, n.owner));
            if !mutates && !lazily && !joined {
                continue;
            }
            let lite = SymbolLite { qualified: join(from.container.as_deref(), &from.name), kind: from.kind.clone(), file: h.file.clone() };
            if let std::collections::btree_map::Entry::Vacant(e) = found[n.child].entry(from.id) {
                e.insert((lite, h.line, h.confidence.to_string(), n.name.clone()));
                by_navigation.insert((from.id, n.child));
            }
            if mutates {
                changed.insert((from.id, n.child));
            }
            if lazily || joined {
                loaded.insert((from.id, n.child));
            }
        }
    }
    let queried: HashSet<(i64, &str)> =
        facts.iter().filter(|f| f.kind == "query").filter_map(|f| Some((f.sym.as_ref()?.id, f.name.as_str()))).collect();
    // Per accessor, the entities whose mapped properties it assigns (`order.Total = …`).
    let accessor_ids: Vec<i64> = found.iter().flat_map(|a| a.keys().copied()).collect();
    let entity_writes = entity_property_writes(conn, &tables, &accessor_ids)?;
    let sets: Vec<String> = tables.iter().filter_map(|t| t.dbset.clone()).collect();
    let mut persist = Persist::new(conn, &sets);
    let mut call_refs: HashMap<String, Vec<RefHit>> = HashMap::new();
    let mut impls: HashMap<i64, Vec<i64>> = HashMap::new();
    // Per table, the sets whose changes are a change of it too: an owned type's rows are saved with its owner's set.
    let saved_with: Vec<Vec<String>> = tables
        .iter()
        .map(|t| {
            let owner = t.navigation.as_deref().and_then(|n| n.rsplit_once('.')).map(|(o, _)| o.rsplit('.').next().unwrap_or(o));
            tables.iter().filter(|o| owner.is_some_and(|e| o.entity == e)).filter_map(|o| o.dbset.clone()).collect()
        })
        .collect();
    for (i, ((t, accessors), saved_with)) in tables.iter_mut().zip(found).zip(saved_with).enumerate() {
        // The classes whose own methods change the table's rows by assignment alone: the entity, and for an owned
        // type's table, the owner declaring the navigation (`Payout.Cancel` setting `CancellationDetails`).
        let mut own: Vec<i64> = t.entity_id.into_iter().collect();
        if t.navigation.is_some()
            && let Some(nav) = t.dbset_id
        {
            own.extend(parent_of(conn, nav)?);
        }
        // Per accessor: (read, write, saves after touching, assigns without persisting, through a navigation).
        let mut how: Vec<(bool, bool, bool, bool, bool)> = Vec::with_capacity(accessors.len());
        for (&id, (_, line, _, segment)) in &accessors {
            if let std::collections::hash_map::Entry::Vacant(e) = calls_of.entry(id) {
                e.insert(calls_from(conn, id)?);
            }
            if let std::collections::hash_map::Entry::Vacant(e) = assigns_of.entry(id) {
                e.insert(assignments_from(conn, id)?);
            }
            // Through a parent's navigation (not an owned type's): a read only where loaded (`Include`, lazily), a
            // change only where made through it, and calls on it are on the loaded collection, not a query.
            let by_nav = by_navigation.contains(&(id, i));
            let (mut read, mut write, saved) = uses(&calls_of[&id], segment, *line);
            if by_nav {
                (read, write) = (false, false);
            }
            read |= queried.contains(&(id, segment.as_str())) || loaded.contains(&(id, i));
            // Assigning the set or navigation (`plan.Details = …`, `Details = …` in the owner), or a member through it
            // (`order.Details.Status = …`), or a mapped property of the entity, writes the table when this symbol
            // persists the change (saves it, or hands it to code that does), or when the entity's (or owner's) own
            // method makes it. Elsewhere it is hydration, fix-up, or a change left to the caller to save.
            let through = |(name, recv, _): &(String, String, u32)| name == segment || mentions(recv, segment);
            let assigned = if by_nav {
                changed.contains(&(id, i))
            } else {
                assigns_of[&id].iter().any(through) || t.entity_id.is_some_and(|e| entity_writes.contains(&(id, e)))
            };
            let mut unsaved = false;
            if !write && assigned {
                // A change through a navigation is persisted by a save, a persisting call or a context's `Add`, not by
                // the change itself (`order.Lines.Add(…)` when a set is also named `Lines`).
                let persisted = if by_nav {
                    nav_persists(&calls_of[&id], &assigns_of[&id], segment) || persist.saves(id)?
                } else {
                    parent_of(conn, id)?.is_some_and(|p| own.contains(&p)) || persist.persists(id)?
                };
                if persisted {
                    write = true;
                } else {
                    unsaved = true;
                }
            }
            how.push((read, write, saved, unsaved, by_nav));
        }
        // Reading the table, then `SaveChanges`, writes it (the entities read may have changed), unless the changes
        // the symbol saves are visibly to other sets (`ctx.Logs.Add(…)` beside a read of `Orders`). Saving also writes
        // it after changing an entity through the context (`ctx.Remove(x)`, `ctx.Entry(x).State = …`).
        let mut helpers: Vec<(i64, String)> = Vec::new();
        for ((id, (s, line, confidence, segment)), (read, mut write, saved, unsaved, by_nav)) in accessors.into_iter().zip(how) {
            if !write
                && !by_nav
                && saved
                && ((read && !changes_other_sets(&calls_of[&id], &segment, &saved_with, &sets))
                    || changes_through_context(&calls_of[&id], &assigns_of[&id], &sets))
            {
                write = true;
            }
            if unsaved && returns_nothing(conn, id)? {
                helpers.push((id, s.qualified.clone()));
            }
            t.accessors.push(Access {
                id,
                symbol: s.qualified,
                kind: s.kind,
                file: s.file,
                line,
                mode: mode_of(read, write).into(),
                confidence,
                via: None,
                saved_by_caller: false,
                only_via: false,
            });
        }
        // Writes through a call, one call deep, from callers resolved (exact or unique_name) to the callee alone, or
        // to an interface member it alone implements:
        // - a helper changing the table's rows without saving them (`void MarkExpired(Order o) { o.Status = … }`;
        //   one returning data is a query hydrating its result): each caller that saves writes the table, `via` the
        //   helper (`saved: here`), and the helper is marked `saved_by_caller`;
        // - a writer: each caller not writing the table itself gets `via` the writer, its mode left as is, counted in
        //   `writes_via`, so the write is counted once, where it is made. A writer that saves (`SaveChanges`, a
        //   commit, a bulk operation, itself or one call down) is credited to every caller (`saved: callee`); one
        //   that only stages its write (`Add`, `Update`, `Attach`, `Entry(x).State`, or an entity's own method) only
        //   to callers that save (`saved: here`), and is marked `saved_by_caller`.
        // Callers not touching the table themselves are listed too.
        let mut credit: BTreeMap<i64, (u8, Via, SymbolLite, String)> = BTreeMap::new();
        let writers: Vec<(i64, String)> =
            t.accessors.iter().filter(|a| writes(&a.mode)).map(|a| (a.id, a.symbol.clone())).collect();
        let mut deferred: HashSet<i64> = HashSet::new();
        for (rank, (callee, symbol)) in helpers.iter().map(|h| (0u8, h)).chain(writers.iter().map(|w| (1u8, w))) {
            let saves_itself = rank == 1 && persist.saves(*callee)?;
            for (c, site, lite, confidence) in callers(conn, &mut call_refs, &mut impls, *callee, symbol)? {
                if !saves_itself && !persist.saves(c)? {
                    continue;
                }
                if !saves_itself {
                    deferred.insert(*callee);
                }
                let saved = if saves_itself { "callee" } else { "here" };
                let via = Via { symbol: symbol.clone(), site, saved: saved.into() };
                // Of several, a helper first, then by name and call site, whatever the index's row order.
                let better = |old: &(u8, Via, SymbolLite, String)| (rank, &via.symbol, &via.site) < (old.0, &old.1.symbol, &old.1.site);
                match credit.entry(c) {
                    std::collections::btree_map::Entry::Occupied(mut e) => {
                        if better(e.get()) {
                            e.insert((rank, via, lite, confidence));
                        }
                    }
                    std::collections::btree_map::Entry::Vacant(e) => {
                        e.insert((rank, via, lite, confidence));
                    }
                }
            }
        }
        for a in &mut t.accessors {
            a.saved_by_caller = deferred.contains(&a.id);
            let Some((rank, via, _, _)) = credit.remove(&a.id) else { continue };
            if writes(&a.mode) {
                continue;
            }
            if rank == 0 {
                a.mode = combine(&a.mode, "write").into();
            }
            a.via = Some(via);
        }
        for (id, (rank, via, s, confidence)) in credit {
            let line = via.site.rsplit(':').next().and_then(|l| l.parse().ok()).unwrap_or(0);
            let mode = if rank == 0 { "write" } else { "access" };
            t.accessors.push(Access { id, symbol: s.qualified, kind: s.kind, file: s.file, line, mode: mode.into(), confidence, via: Some(via), saved_by_caller: false, only_via: true });
        }
        recount(t);
    }
    auto_includes(conn, &facts, &navs, &mut tables, &mut calls_of)?;
    sql_tables(conn, &facts, &mut tables)?;
    tables.sort_by(|a, b| (&a.context, a.mapped_by == "sql", &a.table).cmp(&(&b.context, b.mapped_by == "sql", &b.table)));

    let calls = |kind: &str| -> Vec<Procedure> {
        facts
            .iter()
            .filter(|f| f.kind == kind)
            .filter_map(|f| {
                let s = f.sym.as_ref()?;
                Some(Procedure { name: f.name.clone(), id: s.id, symbol: s.qualified(), file: f.file.clone(), line: f.line })
            })
            .collect()
    };
    Ok(DbModel { tables, procedures: calls("procedure"), wrapped: calls("wrapped_procedure") })
}

/// Tables named in SQL text (`sql_read` and `sql_write` facts, from CFML queries and C# strings): each symbol running
/// a query is an accessor, `write` when any of its queries writes the table. Text a field or property holds as its
/// value (`const string ByIdSql = "SELECT …"`, `entity` `held`) is run by the methods using it, resolved (`exact` or
/// `unique_name`), which are the accessors; the holder itself only when nothing uses it (and never when the
/// extractor credited users in its file, `held_used`). Text a getter runs is the property's. A name joins a table of
/// the same name, else (see [`sql_table_for`]) one differing only by a schema.
fn sql_tables(conn: &Connection, facts: &[Fact], tables: &mut Vec<Table>) -> Result<()> {
    let mut touched = HashSet::new();
    let mut users_of: HashMap<i64, Vec<SqlUser>> = HashMap::new();
    for f in facts.iter().filter(|f| matches!(f.kind.as_str(), "sql_read" | "sql_write")) {
        let Some(s) = &f.sym else { continue };
        let mode = if f.kind == "sql_write" { "write" } else { "read" };
        let held = s.kind == "property" && matches!(f.entity.as_str(), "held" | "held_used");
        if held && !users_of.contains_key(&s.id) {
            let mut users: Vec<SqlUser> = Vec::new();
            for h in query::refs(conn, &s.name, &["member", "call"], usize::MAX)?.0 {
                let Some(from) = &h.from else { continue };
                if from.id == s.id || !matches!(h.confidence, "exact" | "unique_name") || !h.may_target(s.id) {
                    continue;
                }
                if !users.iter().any(|u| u.1 == from.id) {
                    let lite = SymbolLite { qualified: join(from.container.as_deref(), &from.name), kind: from.kind.clone(), file: h.file.clone() };
                    users.push((lite, from.id, h.line, h.confidence));
                }
            }
            users_of.insert(s.id, users);
        }
        let holder = (SymbolLite { qualified: s.qualified(), kind: s.kind.clone(), file: f.file.clone() }, s.id, f.line, "exact");
        let accessors = match users_of.get(&s.id) {
            Some(u) if held && !u.is_empty() => u.clone(),
            _ if f.entity == "held_used" => Vec::new(),
            _ => vec![holder],
        };
        let i = match sql_table_for(tables, &f.name) {
            Some(i) => i,
            None => {
                tables.push(Table {
                    table: f.name.clone(),
                    entity: String::new(),
                    context: None,
                    dbset: None,
                    connection: None,
                    navigation: None,
                    file: f.file.clone(),
                    line: f.line,
                    mapped_by: "sql".into(),
                    mapping: None,
                    dbset_id: None,
                    entity_id: None,
                    reads: 0,
                    writes: 0,
                    read_writes: 0,
                    writes_via: 0,
                    hidden_via: Vec::new(),
                    other: 0,
                    accessors: Vec::new(),
                    accessors_total: None,
                });
                tables.len() - 1
            }
        };
        touched.insert(i);
        let t = &mut tables[i];
        for (lite, id, line, confidence) in accessors {
            match t.accessors.iter_mut().find(|a| a.id == id) {
                Some(a) => a.mode = combine(&a.mode, mode).into(),
                None => t.accessors.push(Access {
                    id,
                    symbol: lite.qualified,
                    kind: lite.kind,
                    file: lite.file,
                    line,
                    mode: mode.into(),
                    confidence: confidence.into(),
                    via: None,
                    saved_by_caller: false,
                    only_via: false,
                }),
            }
        }
    }
    for i in touched {
        recount(&mut tables[i]);
    }
    Ok(())
}

/// A symbol running SQL text a field or property holds: (symbol, id, line of the use, confidence).
type SqlUser = (SymbolLite, i64, u32, &'static str);

/// The table SQL text naming `name` accesses: one of that name (`[dbo].[Users]` is `dbo.Users`; `dbo.Users` and
/// `Users` are one, T-SQL's default schema naming no database). Else an Entity Framework table differing only by a
/// database: for `db.t`, the table `t` unless its context's connection names another database; for `t`, the one
/// table `db.t`. Two names in SQL text with different databases (`t`, `archive.t`) stay two tables.
fn sql_table_for(tables: &[Table], name: &str) -> Option<usize> {
    let plain = |s: &str| {
        let s = s.replace(['[', ']', '`', '"'], "");
        let lower = s.to_ascii_lowercase();
        lower.strip_prefix("dbo.").map(str::to_string).unwrap_or(lower)
    };
    let me = plain(name);
    if let Some(i) = tables.iter().position(|t| plain(&t.table) == me) {
        return Some(i);
    }
    let ef = |t: &Table| t.mapped_by != "sql";
    match me.rsplit_once('.') {
        Some((db, bare)) => tables.iter().position(|t| {
            ef(t) && plain(&t.table) == bare && t.connection.as_deref().and_then(connection_database).is_none_or(|c| same_database(&c, db))
        }),
        None => {
            let qualified: Vec<usize> =
                (0..tables.len()).filter(|&i| ef(&tables[i]) && plain(&tables[i].table).rsplit('.').next() == Some(me.as_str())).collect();
            match qualified.as_slice() {
                [one] => Some(*one),
                _ => None,
            }
        }
    }
}

/// The database a connection setting names, lower-cased without underscores: `Database=shop` gives `shop`;
/// `ShopDbConnection`, `ShopConnectionString`, `Shop_ReadOnly_Connection`, `ShopMySqlConnection` give `shop`; a name
/// saying nothing more (`DbConnection`, `ConnectionString`, `connStr`) gives none.
pub fn connection_database(setting: &str) -> Option<String> {
    if let Some(db) = setting.strip_prefix("Database=") {
        return Some(db.to_ascii_lowercase().replace('_', ""));
    }
    let mut s = setting.rsplit(['.', ':']).next().unwrap_or(setting).to_ascii_lowercase().replace(['_', '-'], "");
    loop {
        let before = s.len();
        for suffix in ["string", "str", "connection", "conn", "readonly", "readwrite", "read", "write", "mysql", "sql", "database", "db"] {
            if let Some(rest) = s.strip_suffix(suffix) {
                s = rest.to_string();
                break;
            }
        }
        if s.len() == before {
            break;
        }
    }
    (s.len() >= 3).then_some(s)
}

/// Whether a connection's database (see [`connection_database`]) is the schema `db`: the same name, underscores
/// aside, or one the other plus a word naming the app around it or a copy (`api`, `service`, `svc`, `app`,
/// `application`, `web`, `jobs`, `replica`, `db`: `orderexportapi` and `order_export`, `shopreplica` and `shop`), at
/// least 4 letters shared. Not any longer name (`ticket` and `tickets_archive`, `shop` and `shoparchive`).
pub fn same_database(conn_db: &str, db: &str) -> bool {
    const APP_WORDS: &[&str] = &["api", "service", "svc", "app", "application", "web", "jobs", "replica", "db"];
    let conn_db = conn_db.to_ascii_lowercase().replace('_', "");
    let db = db.to_ascii_lowercase().replace('_', "");
    let (short, long) = if conn_db.len() <= db.len() { (&conn_db, &db) } else { (&db, &conn_db) };
    conn_db == db || (short.len() >= 4 && long.strip_prefix(short.as_str()).is_some_and(|rest| APP_WORDS.contains(&rest)))
}

/// A table's counts from its accessors, which it sorts: writes first, then by place.
fn recount(t: &mut Table) {
    (t.reads, t.writes, t.read_writes, t.other, t.writes_via) = (0, 0, 0, 0, 0);
    for a in &t.accessors {
        let (read, write) = (reads(&a.mode), writes(&a.mode));
        t.reads += usize::from(read);
        t.writes += usize::from(write);
        t.read_writes += usize::from(read && write);
        // Listed for a write through a callee: counted in `writes_via`, not `other`.
        t.other += usize::from(!read && !write && a.via.is_none());
        t.writes_via += usize::from(a.via.is_some() && !write);
    }
    t.accessors.sort_by(|a, b| (mode_rank(&a.mode), &a.file, a.line, &a.symbol).cmp(&(mode_rank(&b.mode), &b.file, b.line, &b.symbol)));
}

/// The type a `ToTable` on an owned type's builder maps, written `Order#Payments` (or `Order#Address#Lines` for one
/// owned in turn): each navigation's type, a collection's element, from the owner. Returns that type's name and the
/// last navigation's property. Of same-named owner types (a model and an entity both `Product`), the one in a
/// namespace the mapping's file (`file`) sees.
fn owned_type(conn: &Connection, spec: &str, file: &str) -> Result<Option<(String, SymbolRow)>> {
    let mut parts = spec.split('#');
    let Some(owner) = parts.next() else { return Ok(None) };
    let mut cur = owner.rsplit('.').next().unwrap_or(owner).to_string();
    let mut nav = None;
    let sql = format!(
        "SELECT {SYMBOL_COLS}, CASE WHEN json_valid(p.type_name) THEN json_extract(p.type_name, '$.ns') END
         FROM symbols s JOIN files f ON f.id = s.file_id LEFT JOIN symbols p ON p.id = s.parent_id
         WHERE s.name = ?1 AND s.name = ?1 COLLATE BINARY AND s.kind = 'property' AND f.lang = 'csharp'
         AND (s.container = ?2 OR s.container LIKE '%.' || ?2) ORDER BY f.path, s.start_byte"
    );
    // Which namespaces the mapping's file sees, as the compiler scopes names: its own and their parents, and its
    // `using`s exactly (`using Shop.Models.Enums` doesn't bring in `Shop.Models`).
    let mut vis = crate::csharp::Visibility::default();
    let mut stmt = conn.prepare_cached(&sql)?;
    for name in parts {
        let cols = SYMBOL_COLS.split(',').count();
        let candidates: Vec<(SymbolRow, Option<String>)> = stmt
            .query_map(params![name, cur], |r| Ok((symbol_row(r)?, r.get(cols)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut pick = 0;
        for (i, (prop, ns)) in candidates.iter().enumerate() {
            let owner = prop.container.as_deref().map(|c| c.rsplit('.').next().unwrap_or(c)).unwrap_or_default();
            if ns.is_some() && vis.sees(conn, file, ns.as_deref(), owner)? {
                pick = i;
                break;
            }
        }
        let Some((prop, _)) = candidates.into_iter().nth(pick) else { return Ok(None) };
        let ty: Option<String> =
            conn.prepare_cached("SELECT type_name FROM symbols WHERE id = ?1")?.query_row([prop.id], |r| r.get(0))?;
        let Some(t) = ty.as_deref().and_then(kc_extract::TypeExpr::decode) else { return Ok(None) };
        let element = match t {
            kc_extract::TypeExpr::Array(e) => *e,
            kc_extract::TypeExpr::Generic { name, args } if args.len() == 1 && COLLECTIONS.contains(&name.rsplit('.').next().unwrap_or(&name)) => {
                match args.into_iter().next().flatten() {
                    Some(a) => a,
                    None => return Ok(None),
                }
            }
            other => other,
        };
        let (kc_extract::TypeExpr::Named(n) | kc_extract::TypeExpr::Generic { name: n, .. }) = element else { return Ok(None) };
        cur = n.rsplit('.').next().unwrap_or(&n).to_string();
        nav = Some(prop);
    }
    Ok(nav.map(|n| (cur, n)))
}

/// Collection types whose one argument is the element an owned-type navigation holds.
const COLLECTIONS: &[&str] = &[
    "List", "IList", "ICollection", "IEnumerable", "HashSet", "ISet", "IReadOnlyCollection", "IReadOnlyList", "Collection",
];

/// Calls changing a collection's items: through a parent's navigation, a change of the child table's rows.
const COLLECTION_CHANGES: &[&str] = &["Add", "AddRange", "Insert", "InsertRange", "Remove", "RemoveAt", "RemoveAll", "RemoveRange", "RemoveWhere", "Clear"];

/// A navigation property of a table's entity to another table's entity (`Order.Lines`, a `List<OrderLine>`; or
/// `Order.Customer`), not an owned type's.
struct Nav {
    prop: i64,
    name: String,
    /// The tables (indexes) of the declaring entity and of the type it holds.
    owner: usize,
    child: usize,
    /// `virtual`: loaded on first use when the context uses lazy-loading proxies.
    lazy: bool,
    /// A collection (`Lines`), not a reference (`Customer`, whose assignment sets the owner's foreign key).
    collection: bool,
}

/// A property row: id, name, type, signature, container.
type PropRow = (i64, String, Option<String>, Option<String>, Option<String>);

/// The navigations between `tables`' entities. Of several tables of the held type (one per context), the one in the
/// owner's context, else the nearest in the tree.
fn navigations(conn: &Connection, tables: &[Table]) -> Result<Vec<Nav>> {
    let mut out = Vec::new();
    let owned: HashSet<&str> = tables.iter().filter_map(|t| t.navigation.as_deref()).collect();
    let mut stmt = conn.prepare_cached(
        "SELECT s.id, s.name, s.type_name, s.signature, s.container FROM symbols s WHERE s.parent_id = ?1 AND s.kind = 'property' ORDER BY s.start_byte",
    )?;
    let mut seen = HashSet::new();
    for (owner, t) in tables.iter().enumerate() {
        let Some(e) = t.entity_id else { continue };
        if t.navigation.is_some() || !seen.insert((e, t.context.clone())) {
            continue;
        }
        let rows: Vec<PropRow> =
            stmt.query_map([e], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?.collect::<rusqlite::Result<_>>()?;
        for (prop, name, ty, sig, container) in rows {
            if owned.contains(join(container.as_deref(), &name).as_str()) {
                continue;
            }
            let Some((held, collection)) = ty.as_deref().and_then(kc_extract::TypeExpr::decode).and_then(element_name) else { continue };
            let candidates: Vec<usize> = (0..tables.len()).filter(|&i| tables[i].entity == held && tables[i].navigation.is_none()).collect();
            let Some(&child) = candidates
                .iter()
                .max_by_key(|&&i| (tables[i].context == t.context, common_prefix(&tables[i].file, &t.file), std::cmp::Reverse(i)))
            else {
                continue;
            };
            let lazy = sig.as_deref().is_some_and(|s| s.split_whitespace().any(|w| w == "virtual"));
            out.push(Nav { prop, name, owner, child, lazy, collection });
        }
    }
    Ok(out)
}

/// The entity type a navigation holds, by its last name: the type, or a collection's element; and whether it is a
/// collection.
fn element_name(t: kc_extract::TypeExpr) -> Option<(String, bool)> {
    let (element, collection) = match t {
        kc_extract::TypeExpr::Array(e) => (*e, true),
        kc_extract::TypeExpr::Generic { name, args } if args.len() == 1 && COLLECTIONS.contains(&name.rsplit('.').next().unwrap_or(&name)) => {
            (args.into_iter().next().flatten()?, true)
        }
        other => (other, false),
    };
    let (kc_extract::TypeExpr::Named(n) | kc_extract::TypeExpr::Generic { name: n, .. }) = element else { return None };
    Some((n.rsplit('.').next().unwrap_or(&n).trim_end_matches('?').to_string(), collection))
}

/// `Navigation(o => o.Lines).AutoInclude()` on `Order`: every query reading `Order`'s table reads `Lines`' table too,
/// with the reader's confidence, unless it calls `IgnoreAutoIncludes()`. Followed through auto-included tables in
/// turn.
fn auto_includes(
    conn: &Connection,
    facts: &[Fact],
    navs: &[Nav],
    tables: &mut [Table],
    calls_of: &mut HashMap<i64, Vec<(String, String, u32)>>,
) -> Result<()> {
    let mut edges: Vec<(usize, usize)> = Vec::new();
    for f in facts.iter().filter(|f| f.kind == "auto_include") {
        let context = f.sym.as_ref().and_then(|s| s.container.as_deref());
        for n in navs.iter().filter(|n| n.name == f.name && tables[n.owner].entity == f.entity) {
            // The configuring context's own table, when it declares one.
            let ctx = tables[n.owner].context.as_deref();
            if ctx.is_some() && context.is_some() && ctx != context && navs.iter().any(|m| m.name == f.name && tables[m.owner].entity == f.entity && tables[m.owner].context.as_deref() == context) {
                continue;
            }
            if !edges.contains(&(n.owner, n.child)) {
                edges.push((n.owner, n.child));
            }
        }
    }
    let mut touched = HashSet::new();
    for _ in 0..4 {
        let mut grew = false;
        for &(owner, child) in &edges {
            let readers: Vec<Access> = tables[owner].accessors.iter().filter(|a| reads(&a.mode) && !a.only_via).cloned().collect();
            for a in readers {
                if let std::collections::hash_map::Entry::Vacant(e) = calls_of.entry(a.id) {
                    e.insert(calls_from(conn, a.id)?);
                }
                if calls_of[&a.id].iter().any(|(n, _, _)| n == "IgnoreAutoIncludes") {
                    continue;
                }
                let c = &mut tables[child];
                match c.accessors.iter_mut().find(|x| x.id == a.id) {
                    Some(x) if reads(&x.mode) => {}
                    Some(x) => {
                        x.mode = combine(&x.mode, "read").into();
                        grew = true;
                    }
                    None => {
                        c.accessors.push(Access { mode: "read".into(), via: None, saved_by_caller: false, only_via: false, ..a });
                        grew = true;
                    }
                }
                touched.insert(child);
            }
        }
        if !grew {
            break;
        }
    }
    for i in touched {
        recount(&mut tables[i]);
    }
    Ok(())
}

fn apply_mapping(t: &mut Table, m: &Fact) {
    t.table = m.name.clone();
    let on_class = m.sym.as_ref().is_some_and(|s| s.name == m.entity && matches!(s.kind.as_str(), "class" | "struct"));
    t.mapped_by = if on_class { "attribute" } else { "to_table" }.into();
    t.mapping = Some(format!("{}:{}", m.file, m.line));
}

#[derive(Clone)]
struct SymbolLite {
    qualified: String,
    kind: String,
    file: String,
}

fn join(container: Option<&str>, name: &str) -> String {
    container.map_or_else(|| name.to_string(), |c| format!("{c}.{name}"))
}

/// [`symbol_row`] for columns starting at `at`.
fn symbol_row_at(r: &rusqlite::Row, at: usize) -> rusqlite::Result<SymbolRow> {
    Ok(SymbolRow {
        id: r.get(at)?,
        name: r.get(at + 1)?,
        kind: r.get(at + 2)?,
        container: r.get(at + 3)?,
        signature: r.get(at + 4)?,
        doc: r.get(at + 5)?,
        attrs: r.get(at + 6)?,
        tags: r.get(at + 7)?,
        file: r.get(at + 8)?,
        line: r.get(at + 9)?,
        end_line: r.get(at + 10)?,
        start_byte: r.get::<_, i64>(at + 11)? as usize,
        end_byte: r.get::<_, i64>(at + 12)? as usize,
        complexity: r.get(at + 13)?,
        namespace: None,
    })
}

/// Length of the shared leading directories of two paths: how close two files are in the tree.
fn common_prefix(a: &str, b: &str) -> usize {
    a.split('/').zip(b.split('/')).take_while(|(x, y)| x == y).count()
}

/// The calls a symbol makes: (name, receiver text without whitespace, line).
fn calls_from(conn: &Connection, id: i64) -> Result<Vec<(String, String, u32)>> {
    let mut stmt = conn.prepare_cached("SELECT name, receiver, line FROM refs WHERE from_id = ?1 AND kind = 'call'")?;
    let rows = stmt.query_map([id], |r| {
        let recv: Option<String> = r.get(1)?;
        Ok((r.get(0)?, recv.unwrap_or_default().chars().filter(|c| !c.is_whitespace()).collect(), r.get(2)?))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The member references in `id` that assign the member: (name, receiver, line).
fn assignments_from(conn: &Connection, id: i64) -> Result<Vec<(String, String, u32)>> {
    let mut stmt = conn.prepare_cached("SELECT name, receiver, line FROM refs WHERE from_id = ?1 AND kind = 'member' AND assigns = 1")?;
    let rows = stmt.query_map([id], |r| {
        let recv: Option<String> = r.get(1)?;
        Ok((r.get(0)?, recv.unwrap_or_default().chars().filter(|c| !c.is_whitespace()).collect(), r.get(2)?))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// (accessor, entity) for each of `accessors` assigning a property the entity class of a table declares, resolved: a
/// change to a tracked entity is a write of its table.
fn entity_property_writes(conn: &Connection, tables: &[Table], accessors: &[i64]) -> Result<HashSet<(i64, i64)>> {
    let mut out = HashSet::new();
    let entities: HashSet<i64> = tables.iter().filter_map(|t| t.entity_id).collect();
    if entities.is_empty() || accessors.is_empty() {
        return Ok(out);
    }
    // The entities' properties, and the names the accessors assign.
    let mut owner_of: HashMap<i64, i64> = HashMap::new();
    let mut stmt = conn.prepare_cached("SELECT id FROM symbols WHERE parent_id = ?1 AND kind = 'property'")?;
    for &e in &entities {
        for p in stmt.query_map([e], |r| r.get::<_, i64>(0))? {
            owner_of.insert(p?, e);
        }
    }
    let mine: HashSet<i64> = accessors.iter().copied().collect();
    let mut names: Vec<String> = Vec::new();
    let mut stmt = conn.prepare_cached("SELECT DISTINCT name FROM refs WHERE from_id = ?1 AND kind = 'member' AND assigns = 1 ORDER BY name")?;
    for &a in accessors {
        for n in stmt.query_map([a], |r| r.get::<_, String>(0))? {
            let n = n?;
            if !names.contains(&n) {
                names.push(n);
            }
        }
    }
    if names.is_empty() {
        return Ok(out);
    }
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    for h in query::refs_named(conn, &names)? {
        let Some(from) = &h.from else { continue };
        if !h.assigns || !mine.contains(&from.id) || !matches!(h.confidence, "exact" | "unique_name") {
            continue;
        }
        for id in &h.target_ids {
            if let Some(&e) = owner_of.get(id) {
                out.insert((from.id, e));
            }
        }
    }
    Ok(out)
}

/// Whether a symbol making `calls` reads the set named `segment` (`Orders`, `Set<Order>()`), first touched at
/// `first_line`; whether it writes it (`Add`, `Remove`… on the set); and whether it saves changes after touching it.
/// A call chained into a write (`Where` in `ctx.Orders.Where(…).ExecuteDelete()`) isn't a read.
fn uses(calls: &[(String, String, u32)], segment: &str, first_line: u32) -> (bool, bool, bool) {
    let on_set: Vec<&(String, String, u32)> = calls.iter().filter(|(_, recv, _)| mentions(recv, segment)).collect();
    let write_recvs: Vec<&str> =
        on_set.iter().filter(|(name, _, _)| WRITE_OPS.contains(&name.as_str())).map(|(_, recv, _)| recv.as_str()).collect();
    let read = on_set
        .iter()
        .any(|(name, _, _)| !WRITE_OPS.contains(&name.as_str()) && !write_recvs.iter().any(|r| r.contains(&format!(".{name}("))));
    let saved = calls.iter().any(|(n, _, l)| SAVE_OPS.contains(&n.as_str()) && *l >= first_line);
    (read, !write_recvs.is_empty(), saved)
}

/// Whether a symbol adds, updates or removes rows of a set other than `segment` and those in `same`
/// (`ctx.Logs.Add(…)`, `Set<Log>().Add(…)`).
fn changes_other_sets(calls: &[(String, String, u32)], segment: &str, same: &[String], sets: &[String]) -> bool {
    calls.iter().any(|(name, recv, _)| {
        (WRITE_OPS.contains(&name.as_str()) || PERSIST_OPS.contains(&name.as_str()))
            && !mentions(recv, segment)
            && !same.iter().any(|s| mentions(recv, s))
            && (recv.contains("Set<") || sets.iter().any(|s| mentions(recv, s)))
    })
}

/// Whether a symbol changes an entity through its context rather than a named set: `ctx.Remove(x)`, `ctx.Update(x)`,
/// `ctx.Add(x)`, `ctx.Entry(x).State = …`.
fn changes_through_context(calls: &[(String, String, u32)], assigns: &[(String, String, u32)], sets: &[String]) -> bool {
    calls.iter().any(|(name, recv, _)| {
        (WRITE_OPS.contains(&name.as_str()) || PERSIST_OPS.contains(&name.as_str()))
            && context_like(recv)
            && !recv.contains("Set<")
            && !sets.iter().any(|s| mentions(recv, s))
    }) || assigns.iter().any(|(name, recv, _)| name == "State" && recv.contains("Entry("))
}

/// Whether a symbol saves the changes it makes to tracked entities: it calls `SaveChanges`, `Update`, `Attach` or a
/// variant (any receiver), `Add` or `AddRange` on a context or a set, or sets `Entry(x).State`; or it calls (resolved,
/// an interface member through its implementations) a method doing so, such as a repository's `SaveAsync` or a unit
/// of work's `Commit`. One hop down and none up: a helper leaving the save to its caller doesn't save.
struct Persist<'a> {
    conn: &'a Connection,
    sets: &'a [String],
    direct: HashMap<i64, bool>,
    persists: HashMap<i64, bool>,
    saves: HashMap<i64, bool>,
    callees: HashMap<i64, Vec<i64>>,
}

impl<'a> Persist<'a> {
    fn new(conn: &'a Connection, sets: &'a [String]) -> Self {
        Persist { conn, sets, direct: HashMap::new(), persists: HashMap::new(), saves: HashMap::new(), callees: HashMap::new() }
    }

    /// Whether `id` saves by itself.
    fn direct(&mut self, id: i64) -> Result<bool> {
        if let Some(&d) = self.direct.get(&id) {
            return Ok(d);
        }
        let saves = calls_from(self.conn, id)?.iter().any(|(name, recv, _)| {
            let name = name.as_str();
            SAVE_OPS.contains(&name)
                || PERSIST_OPS.contains(&name)
                || (matches!(name, "Add" | "AddRange")
                    && (context_like(recv) || recv.contains("Set<") || self.sets.iter().any(|s| mentions(recv, s))))
        }) || assignments_from(self.conn, id)?.iter().any(|(name, recv, _)| name == "State" && recv.contains("Entry("));
        self.direct.insert(id, saves);
        Ok(saves)
    }

    /// Whether `id` writes to the database at once (`FLUSH_OPS`, or `SET_FLUSH_OPS` on a set), itself or through a
    /// method it calls: unlike staging a change (`Add`, `Update`, `Entry(x).State`), which waits for a save.
    fn saves(&mut self, id: i64) -> Result<bool> {
        if let Some(&p) = self.saves.get(&id) {
            return Ok(p);
        }
        let sets = self.sets;
        let flushes = |conn, id| -> Result<bool> {
            Ok(calls_from(conn, id)?.iter().any(|(n, recv, _)| {
                FLUSH_OPS.contains(&n.as_str())
                    || (SET_FLUSH_OPS.contains(&n.as_str()) && (recv.contains("Set<") || sets.iter().any(|s| mentions(recv, s))))
            }))
        };
        let mut saves = flushes(self.conn, id)?;
        if !saves {
            for callee in self.callees(id)? {
                if callee != id && flushes(self.conn, callee)? {
                    saves = true;
                    break;
                }
            }
        }
        self.saves.insert(id, saves);
        Ok(saves)
    }

    /// Whether `id` persists the changes it makes, staging or saving them, itself or through a method it calls.
    fn persists(&mut self, id: i64) -> Result<bool> {
        if let Some(&p) = self.persists.get(&id) {
            return Ok(p);
        }
        let mut saves = self.direct(id)?;
        if !saves {
            for callee in self.callees(id)? {
                if callee != id && self.direct(callee)? {
                    saves = true;
                    break;
                }
            }
        }
        self.persists.insert(id, saves);
        Ok(saves)
    }

    /// The methods `id` calls, resolved `exact` or `unique_name`, an interface member with its implementations.
    fn callees(&mut self, id: i64) -> Result<Vec<i64>> {
        if !self.callees.contains_key(&id) {
            self.resolve(id)?;
        }
        Ok(self.callees[&id].clone())
    }

    fn resolve(&mut self, id: i64) -> Result<()> {
        let sql = format!(
            "SELECT {REF_COLS} FROM refs r JOIN files f ON f.id = r.file_id LEFT JOIN symbols fs ON fs.id = r.from_id
             WHERE r.from_id = ?1 AND r.kind = 'call' ORDER BY r.line"
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let mut hits: Vec<RefHit> = stmt.query_map([id], ref_row)?.collect::<rusqlite::Result<_>>()?;
        resolve_all(self.conn, &mut hits)?;
        let mut all = Vec::new();
        for h in hits.iter().filter(|h| matches!(h.confidence, "exact" | "unique_name")) {
            for &tid in &h.target_ids {
                for c in std::iter::once(tid).chain(implementations(self.conn, tid)?) {
                    if !all.contains(&c) {
                        all.push(c);
                    }
                }
            }
        }
        self.callees.insert(id, all);
        Ok(())
    }
}

/// Whether a symbol changing a table through the navigation `nav` persists the change itself: it saves, calls a
/// persisting method (`Update`, `Attach`…) or a context's `Add`, or sets `Entry(x).State`, on anything but `nav`.
fn nav_persists(calls: &[(String, String, u32)], assigns: &[(String, String, u32)], nav: &str) -> bool {
    calls.iter().any(|(name, recv, _)| {
        let name = name.as_str();
        !mentions(recv, nav)
            && (SAVE_OPS.contains(&name) || PERSIST_OPS.contains(&name) || (matches!(name, "Add" | "AddRange") && (context_like(recv) || recv.contains("Set<"))))
    }) || assigns.iter().any(|(name, recv, _)| name == "State" && recv.contains("Entry("))
}

/// `read`, `write`, `read_write` or `access`.
fn mode_of(read: bool, write: bool) -> &'static str {
    match (read, write) {
        (true, true) => "read_write",
        (false, true) => "write",
        (true, false) => "read",
        (false, false) => "access",
    }
}

/// The callers of method `id` (qualified `symbol`): calls resolved `exact` or `unique_name` to it alone, or to an
/// interface member only it implements. Each with the call site, the caller and the call's confidence, in file order.
fn callers(
    conn: &Connection,
    call_refs: &mut HashMap<String, Vec<RefHit>>,
    impls: &mut HashMap<i64, Vec<i64>>,
    id: i64,
    symbol: &str,
) -> Result<Vec<(i64, String, SymbolLite, String)>> {
    let name = symbol.rsplit('.').next().unwrap_or(symbol);
    if !call_refs.contains_key(name) {
        let mut hits = query::refs(conn, name, &["call"], usize::MAX)?.0;
        hits.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
        call_refs.insert(name.to_string(), hits);
    }
    let mut out: Vec<(i64, String, SymbolLite, String)> = Vec::new();
    for h in &call_refs[name] {
        let Some(from) = &h.from else { continue };
        let [tid] = h.target_ids[..] else { continue };
        if from.id == id || !matches!(h.confidence, "exact" | "unique_name") {
            continue;
        }
        if tid != id {
            if let std::collections::hash_map::Entry::Vacant(e) = impls.entry(tid) {
                e.insert(running_implementations(conn, tid)?);
            }
            if impls[&tid] != [id] {
                continue;
            }
        }
        if out.iter().any(|(c, ..)| *c == from.id) {
            continue;
        }
        let lite = SymbolLite { qualified: join(from.container.as_deref(), &from.name), kind: from.kind.clone(), file: h.file.clone() };
        out.push((from.id, format!("{}:{}", h.file, h.line), lite, h.confidence.to_string()));
    }
    Ok(out)
}

/// Whether method `id` returns nothing (`void`, `Task`, `ValueTask`): a helper changing what it is given, not a query
/// returning what it read.
fn returns_nothing(conn: &Connection, id: i64) -> Result<bool> {
    let mut stmt = conn.prepare_cached("SELECT signature FROM symbols WHERE id = ?1")?;
    let sig: Option<String> = stmt.query_row([id], |r| r.get(0)).ok();
    let head = sig.as_deref().unwrap_or("").split('(').next().unwrap_or("");
    let words: Vec<&str> = head.split_whitespace().collect();
    Ok(words.len() >= 2 && matches!(words[words.len() - 2], "void" | "Task" | "ValueTask"))
}

/// The symbol enclosing `id` (a method's type).
fn parent_of(conn: &Connection, id: i64) -> Result<Option<i64>> {
    let mut stmt = conn.prepare_cached("SELECT parent_id FROM symbols WHERE id = ?1")?;
    let mut rows = stmt.query([id])?;
    Ok(match rows.next()? {
        Some(r) => r.get(0)?,
        None => None,
    })
}

/// The mode of a symbol using a table as `a` in one place and `b` in another.
fn combine(a: &str, b: &str) -> &'static str {
    match (reads(a) || reads(b), writes(a) || writes(b)) {
        (true, true) => "read_write",
        (false, true) => "write",
        (true, false) => "read",
        (false, false) => "access",
    }
}

/// Whether an access mode reads: `read` or `read_write`.
pub fn reads(mode: &str) -> bool {
    matches!(mode, "read" | "read_write")
}

/// Whether an access mode writes: `write` or `read_write`.
pub fn writes(mode: &str) -> bool {
    matches!(mode, "write" | "read_write")
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// Whether a receiver chain goes through `segment` as a whole member: `ctx.Orders`, `ctx.Orders.Where(…)`, not
/// `ctx.OrdersById`.
fn mentions(recv: &str, segment: &str) -> bool {
    recv.match_indices(segment).any(|(i, _)| {
        let before = recv[..i].chars().last();
        let after = recv[i + segment.len()..].chars().next();
        matches!(before, None | Some('.')) && matches!(after, None | Some('.'))
    })
}

/// A receiver named like a DbContext: `_context`, `dbContext`, `_db`, `ctx`, `this._context`.
fn context_like(recv: &str) -> bool {
    let last = recv.rsplit('.').next().unwrap_or(recv).trim_start_matches('_').to_ascii_lowercase();
    last.contains("context") || last == "db" || last.ends_with("db") || last.starts_with("db") || last == "ctx"
}

fn mode_rank(mode: &str) -> u8 {
    match mode {
        "write" => 0,
        "read_write" => 1,
        "read" => 2,
        _ => 3,
    }
}

/// A table or stored procedure a symbol reaches, directly (hop 0) or through the calls it makes.
#[derive(Debug, Clone, Serialize)]
pub struct Touch {
    /// `table` or `procedure`.
    pub kind: &'static str,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    /// `read`, `write`, `read_write` or `access`, over every path: `read_write` when one path reads and another writes.
    pub mode: String,
    /// Calls between the symbol and the code touching the table (0: the symbol itself).
    pub hop: u8,
    /// The symbol touching it, on the shortest path (the first by file and line among as short ones).
    pub via: String,
    /// Where, `file:line`.
    pub site: String,
}

/// A `file:line` site's order: by file, then line as a number.
fn site_order(site: &str) -> (&str, u32) {
    match site.rsplit_once(':') {
        Some((file, line)) => (file, line.parse().unwrap_or(0)),
        None => (site, 0),
    }
}

/// Tables and procedures `root` touches, itself or through its calls, to `depth` hops. Calls are followed when they
/// resolve `exact` or `unique_name`, and a call to an interface member also reaches its implementations. Also says
/// whether the walk stopped at [`TOUCH_NODES`] with symbols left to visit.
pub fn touched(conn: &Connection, model: &DbModel, root: i64, depth: u8) -> Result<(Vec<Touch>, bool)> {
    let mut cut = false;
    let mut direct: HashMap<i64, Vec<(usize, &Access)>> = HashMap::new();
    for (i, t) in model.tables.iter().enumerate() {
        // A caller listed only for a callee's own write reaches the table through the call; one saving a helper's
        // change writes it here.
        for a in t.accessors.iter().filter(|a| !a.only_via || writes(&a.mode)) {
            direct.entry(a.id).or_default().push((i, a));
        }
    }
    let mut procs: HashMap<i64, Vec<&Procedure>> = HashMap::new();
    for p in &model.procedures {
        procs.entry(p.id).or_default().push(p);
    }
    let mut out: Vec<Touch> = Vec::new();
    // Of the ways to one table or procedure, the nearest names it; among as near ones, the first by file and line, so
    // the answer doesn't follow the order the index happened to store symbols in.
    let mut merge = |touch: Touch| match out.iter_mut().find(|t| t.kind == touch.kind && t.name == touch.name && t.context == touch.context) {
        Some(t) => {
            t.mode = combine(&t.mode, &touch.mode).into();
            if (touch.hop, site_order(&touch.site)) < (t.hop, site_order(&t.site)) {
                (t.hop, t.via, t.site) = (touch.hop, touch.via, touch.site);
            }
        }
        None => out.push(touch),
    };
    let mut seen = HashSet::from([root]);
    let mut queue = VecDeque::from([(root, 0u8)]);
    while let Some((id, hop)) = queue.pop_front() {
        for (i, a) in direct.get(&id).into_iter().flatten() {
            let t = &model.tables[*i];
            merge(Touch {
                kind: "table",
                name: t.table.clone(),
                entity: (!t.entity.is_empty()).then(|| t.entity.clone()),
                context: t.context.clone(),
                mode: a.mode.clone(),
                hop,
                via: a.symbol.clone(),
                site: format!("{}:{}", a.file, a.line),
            });
        }
        for p in procs.get(&id).into_iter().flatten() {
            merge(Touch {
                kind: "procedure",
                name: p.name.clone(),
                entity: None,
                context: None,
                mode: "access".into(),
                hop,
                via: p.symbol.clone(),
                site: format!("{}:{}", p.file, p.line),
            });
        }
        if hop >= depth {
            continue;
        }
        if seen.len() >= TOUCH_NODES {
            cut = true;
            continue;
        }
        let sql = format!(
            "SELECT {REF_COLS} FROM refs r JOIN files f ON f.id = r.file_id LEFT JOIN symbols fs ON fs.id = r.from_id
             WHERE r.from_id = ?1 AND r.kind IN ('call', 'member') ORDER BY r.line"
        );
        let mut stmt = conn.prepare_cached(&sql)?;
        let mut hits: Vec<RefHit> = stmt.query_map([id], ref_row)?.collect::<rusqlite::Result<_>>()?;
        resolve_all(conn, &mut hits)?;
        for h in hits.iter().filter(|h| matches!(h.confidence, "exact" | "unique_name")) {
            // A call's targets and their implementations in file and line order, not the order the index stored them
            // in: which symbols the walk visits before its limit is the same however the repo was indexed.
            let mut nexts: Vec<(String, u32, String, i64)> = Vec::new();
            for &tid in &h.target_ids {
                for next in std::iter::once(tid).chain(implementations(conn, tid)?) {
                    if !seen.contains(&next)
                        && !nexts.iter().any(|n| n.3 == next)
                        && let Some(s) = crate::trace::symbol(conn, next)?
                    {
                        nexts.push((s.file.clone(), s.line, s.qualified(), next));
                    }
                }
            }
            nexts.sort();
            for (.., next) in nexts {
                if seen.len() < TOUCH_NODES {
                    seen.insert(next);
                    queue.push_back((next, hop + 1));
                } else {
                    cut = true;
                }
            }
        }
    }
    out.sort_by(|a, b| (a.hop, a.kind, &a.name).cmp(&(b.hop, b.kind, &b.name)));
    Ok((out, cut))
}

/// For an interface member, its implementations in the types that declare the interface, and those a SCIP index
/// names.
pub(crate) fn implementations(conn: &Connection, id: i64) -> Result<Vec<i64>> {
    let mut ids = declared_implementations(conn, id)?;
    for i in crate::scip::implementations(conn, id)? {
        if !ids.contains(&i) {
            ids.push(i);
        }
    }
    Ok(ids)
}

/// [`implementations`] that may run outside tests: those in test code left out when exactly one other remains, so a
/// fake or spy beside the one real implementation doesn't make calls through the interface ambiguous. Otherwise all
/// of them. Test code is in a test path, or named as a double of the interface or of another implementation: `Mock`,
/// `Fake`, `Stub` or `Dummy` and the interface's name without its `I` (`FakeClock` for `IClock`), or another
/// implementation's name (`MockAddressParser` beside `AddressParser`); `StubHillClient` beside `BoxOfficeClient`, both
/// `ITicketClient`s, is a real one.
pub(crate) fn running_implementations(conn: &Connection, id: i64) -> Result<Vec<i64>> {
    let all = implementations(conn, id)?;
    if all.len() < 2 {
        return Ok(all);
    }
    let mut stmt = conn.prepare_cached("SELECT f.path, s.container FROM symbols s JOIN files f ON f.id = s.file_id WHERE s.id = ?1")?;
    let short = |c: Option<String>| c.map(|c| c.rsplit('.').next().unwrap_or(&c).to_string()).unwrap_or_default();
    let (_, interface): (String, Option<String>) = stmt.query_row([id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let interface = short(interface);
    let interface = interface.strip_prefix('I').filter(|r| r.starts_with(|c: char| c.is_ascii_uppercase())).unwrap_or(&interface).to_string();
    let mut owners = Vec::with_capacity(all.len());
    for &i in &all {
        let (file, container): (String, Option<String>) = stmt.query_row([i], |r| Ok((r.get(0)?, r.get(1)?)))?;
        owners.push((i, file, short(container)));
    }
    let real: Vec<i64> = owners
        .iter()
        .filter(|(_, file, owner)| {
            let doubled = crate::di::double_of(owner).is_some_and(|d| d == interface || owners.iter().any(|(_, _, o)| o != owner && *o == d));
            !crate::is_test_path(file) && !doubled
        })
        .map(|(i, _, _)| *i)
        .collect();
    Ok(if real.len() == 1 { real } else { all })
}

/// [`implementations`] from the types declaring the protocol, by name.
pub(crate) fn declared_implementations(conn: &Connection, id: i64) -> Result<Vec<i64>> {
    let sql = format!("SELECT {SYMBOL_COLS} FROM symbols s JOIN files f ON f.id = s.file_id WHERE s.id = ?1");
    let Some(sym) = conn.prepare_cached(&sql)?.query_map([id], symbol_row)?.next().transpose()? else { return Ok(Vec::new()) };
    let Some(container) = sym.container.as_deref() else { return Ok(Vec::new()) };
    if !query::definitions(conn, container)?.iter().any(|d| d.kind == "protocol") {
        return Ok(Vec::new());
    }
    let short = container.rsplit('.').next().unwrap_or(container);
    let mut stmt = conn.prepare_cached(
        "SELECT s.id FROM symbols s
         WHERE s.name = ?1 AND s.name = ?1 COLLATE BINARY AND s.kind = ?2 AND s.container IN (
           SELECT CASE WHEN c.container IS NULL THEN c.name ELSE c.container || '.' || c.name END
           FROM relations r JOIN symbols c ON c.id = r.from_id WHERE r.target_name = ?3 COLLATE BINARY)",
    )?;
    let ids = stmt.query_map(params![sym.name, sym.kind, short], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    Ok(ids)
}

/// What the model says about one symbol: for an entity class or a DbSet property, its tables with their accessors;
/// for a DbContext, its tables (accessor counts only); for anything else, the tables and procedures it touches.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SymbolTables {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tables: Vec<Table>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub touches: Vec<Touch>,
    /// The walk for `touches` stopped at its node limit: tables reached only past it are missing.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub touches_truncated: bool,
}

impl SymbolTables {
    pub fn is_empty(&self) -> bool {
        self.tables.is_empty() && self.touches.is_empty()
    }
}

/// [`SymbolTables`] for `sym`, keeping at most `max_accessors` accessors per table.
pub fn for_symbol(conn: &Connection, model: &DbModel, sym: &SymbolRow, max_accessors: usize) -> Result<SymbolTables> {
    let mut out = SymbolTables::default();
    if model.is_empty() {
        return Ok(out);
    }
    let own: Vec<&Table> =
        model.tables.iter().filter(|t| t.entity_id == Some(sym.id) || t.dbset_id == Some(sym.id)).collect();
    if !own.is_empty() {
        out.tables = own
            .into_iter()
            .map(|t| {
                let mut t = t.clone();
                compact(&mut t, VIA_ROWS);
                t.cap_accessors(max_accessors);
                t
            })
            .collect();
        return Ok(out);
    }
    if query::is_type_or_extension(&sym.kind) {
        let name = sym.qualified();
        out.tables = model
            .tables
            .iter()
            .filter(|t| t.context.as_deref() == Some(name.as_str()))
            .map(|t| Table { accessors: Vec::new(), ..t.clone() })
            .collect();
        return Ok(out);
    }
    (out.touches, out.touches_truncated) = touched(conn, model, sym.id, TOUCH_DEPTH)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_follow_the_calls_on_the_set() {
        let call = |n: &str, r: &str, l: u32| (n.to_string(), r.to_string(), l);
        // (read, write by a call on the set, saved after touching it)
        assert_eq!(uses(&[call("Add", "context.Orders", 3)], "Orders", 3), (false, true, false));
        assert_eq!(uses(&[call("Where", "_db.Orders", 3), call("ToListAsync", "_db.Orders.Where(o=>o.Open)", 3)], "Orders", 3), (true, false, false));
        // Read, then saved: tracked entities may have changed.
        assert_eq!(uses(&[call("FindAsync", "_db.Orders", 3), call("SaveChangesAsync", "_db", 5)], "Orders", 3), (true, false, true));
        // A save before the access doesn't count.
        assert_eq!(uses(&[call("SaveChanges", "_db", 1), call("First", "_db.Orders", 3)], "Orders", 3), (true, false, false));
        assert_eq!(uses(&[call("Process", "", 3)], "Orders", 3), (false, false, false));
        assert_eq!(uses(&[call("Remove", "ctx.Set<Order>()", 3)], "Set<Order>()", 3), (false, true, false));
        // A filter chained into a bulk delete isn't a read.
        let delete = [call("Where", "ctx.Orders", 3), call("ExecuteDelete", "ctx.Orders.Where(o=>o.Old)", 3)];
        assert_eq!(uses(&delete, "Orders", 3), (false, true, false));
        assert!(context_like("_context") && context_like("this._dbContext") && context_like("db") && context_like("ctx"));
        assert!(!context_like("request") && !context_like("domain"));
        // `OrdersById` is another member.
        assert_eq!(uses(&[call("Add", "ctx.OrdersById", 3)], "Orders", 3), (false, false, false));
    }

    #[test]
    fn modes_combine_into_read_write() {
        assert_eq!(combine("read", "write"), "read_write");
        assert_eq!(combine("access", "write"), "write");
        assert_eq!(combine("read_write", "access"), "read_write");
        assert_eq!(combine("access", "read"), "read");
        assert!(reads("read_write") && writes("read_write") && !writes("read") && !reads("write"));
    }
}
