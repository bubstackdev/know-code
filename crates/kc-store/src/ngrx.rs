//! NgRx action groups: `createActionGroup({ source, events: { 'Load Success': props<…>(), … } })` makes an action
//! creator per event, named as NgRx derives it (`Group.loadSuccess`). A dispatch calls `Group.loadSuccess(…)` and an
//! effect names it in `ofType(Group.loadSuccess)`; both are matched by the group's name and the creator's (see the
//! TypeScript extractor's `action` facts). Creators made one at a time (`createAction`) link by name elsewhere.

use std::collections::HashMap;

use anyhow::Result;
use rusqlite::Connection;

/// The repo's action groups: per group name, how many groups share it and their creators' names.
#[derive(Debug, Default)]
pub struct Groups {
    groups: HashMap<String, (usize, Vec<String>)>,
}

/// A group's name as a reference writes it: the last segment (`fromAccount.AccountActions` → `AccountActions`).
fn bare(receiver: &str) -> &str {
    receiver.rsplit('.').next().unwrap_or(receiver)
}

impl Groups {
    /// The repo's groups, an event key computed from a constant (`[Types.load]`) resolved through the constants.
    pub fn load(conn: &Connection) -> Result<Groups> {
        let rows: Vec<(i64, String, String)> = conn
            .prepare_cached(
                "SELECT s.id, s.name, n.value FROM ng_facts n JOIN symbols s ON s.id = n.symbol_id
                 JOIN files f ON f.id = n.file_id WHERE n.kind = 'action' ORDER BY f.path, n.line",
            )?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut by_symbol: HashMap<i64, (String, Vec<String>)> = HashMap::new();
        let mut constant = conn.prepare_cached("SELECT value FROM constants WHERE qualified = ?1 LIMIT 1")?;
        for (id, group, value) in rows {
            let creator = match value.strip_prefix("{=").and_then(|v| v.strip_suffix('}')) {
                Some(name) => match constant.query_map([name], |r| r.get::<_, String>(0))?.next().transpose()? {
                    Some(event) => kc_extract::action_name(&event),
                    None => continue,
                },
                None => value,
            };
            by_symbol.entry(id).or_insert_with(|| (group, Vec::new())).1.push(creator);
        }
        let mut groups: HashMap<String, (usize, Vec<String>)> = HashMap::new();
        for (_, (group, creators)) in by_symbol {
            let e = groups.entry(group).or_default();
            e.0 += 1;
            e.1.extend(creators);
        }
        Ok(Groups { groups })
    }

    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }

    /// Whether `receiver.name` is a group's action creator.
    pub fn creates(&self, receiver: &str, name: &str) -> bool {
        self.groups.get(bare(receiver)).is_some_and(|(_, c)| c.iter().any(|c| c == name))
    }

    /// For the action `Group.creator`: `unique_name` when one group has that name, else `ambiguous`.
    pub fn confidence(&self, action: &str) -> &'static str {
        let group = split(action).0;
        if self.groups.get(bare(group)).is_some_and(|(n, _)| *n == 1) { "unique_name" } else { "ambiguous" }
    }

    /// The group actions `id` dispatches, (`Group.creator`, first line): passed to `store.dispatch(…)` (see the
    /// TypeScript extractor's `dispatch` facts; a facade's method dispatching is one), or, in an effect (a symbol
    /// calling `createEffect`), created to be emitted. A creator called anywhere else (`const action = …` in a reducer
    /// spec) dispatches nothing.
    pub fn dispatched(&self, conn: &Connection, id: i64) -> Result<Vec<(String, u32)>> {
        let rows: Vec<(String, String, u32)> = conn
            .prepare_cached(
                "SELECT value, extra, MIN(line) FROM ng_facts WHERE symbol_id = ?1 AND kind = 'dispatch' AND extra != ''
                 GROUP BY value, extra
                 UNION ALL
                 SELECT name, receiver, MIN(line) FROM refs WHERE from_id = ?1 AND kind = 'call' AND receiver IS NOT NULL
                   AND EXISTS(SELECT 1 FROM refs e WHERE e.from_id = ?1 AND e.kind = 'call' AND e.name = 'createEffect')
                 GROUP BY name, receiver",
            )?
            .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut out: Vec<(String, u32)> = Vec::new();
        for (n, r, l) in rows.into_iter().filter(|(n, r, _)| self.creates(r, n)) {
            let action = format!("{}.{n}", bare(&r));
            match out.iter_mut().find(|(a, _)| *a == action) {
                Some(o) => o.1 = o.1.min(l),
                None => out.push((action, l)),
            }
        }
        out.sort_by_key(|(_, l)| *l);
        Ok(out)
    }

    /// The group actions an effect names other than by calling them (in `ofType`), as `Group.creator`.
    pub fn named(&self, conn: &Connection, id: i64) -> Result<Vec<String>> {
        let refs: Vec<(String, String)> = conn
            .prepare_cached("SELECT DISTINCT name, receiver FROM refs WHERE from_id = ?1 AND kind = 'member' AND receiver IS NOT NULL ORDER BY name")?
            .query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(refs.into_iter().filter(|(n, r)| self.creates(r, n)).map(|(n, r)| format!("{}.{n}", bare(&r))).collect())
    }
}

/// `Group.creator` → (`Group`, `creator`).
fn split(action: &str) -> (&str, &str) {
    action.rsplit_once('.').unwrap_or(("", action))
}

/// The effects handling the group action `Group.creator`: members calling `createEffect` that name it otherwise
/// than by calling it (in `ofType`).
pub fn effects(conn: &Connection, action: &str) -> Result<Vec<i64>> {
    let (group, creator) = split(action);
    let mut stmt = conn.prepare_cached(
        "SELECT DISTINCT r.from_id FROM refs r WHERE r.name = ?1 AND r.kind = 'member'
         AND (r.receiver = ?2 OR r.receiver LIKE '%.' || ?2)
         AND EXISTS(SELECT 1 FROM refs e WHERE e.from_id = r.from_id AND e.kind = 'call' AND e.name = 'createEffect')",
    )?;
    Ok(stmt.query_map([creator, group], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?)
}

/// The code dispatching the group action `Group.creator`, (symbol, file, first line): passing it to
/// `store.dispatch(…)`, or an effect emitting it (see [`Groups::dispatched`]).
pub fn dispatchers(conn: &Connection, action: &str) -> Result<Vec<(i64, String, u32)>> {
    let (group, creator) = split(action);
    let mut stmt = conn.prepare_cached(
        "SELECT id, path, MIN(line) FROM (
           SELECT n.symbol_id AS id, f.path AS path, n.line AS line FROM ng_facts n JOIN files f ON f.id = n.file_id
           WHERE n.kind = 'dispatch' AND n.value = ?1 AND (n.extra = ?2 OR n.extra LIKE '%.' || ?2) AND n.symbol_id IS NOT NULL
           UNION ALL
           SELECT r.from_id, f.path, r.line FROM refs r JOIN files f ON f.id = r.file_id
           WHERE r.name = ?1 AND r.kind = 'call' AND (r.receiver = ?2 OR r.receiver LIKE '%.' || ?2) AND r.from_id IS NOT NULL
             AND EXISTS(SELECT 1 FROM refs e WHERE e.from_id = r.from_id AND e.kind = 'call' AND e.name = 'createEffect'))
         GROUP BY id ORDER BY path, MIN(line)",
    )?;
    Ok(stmt.query_map([creator, group], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<_>>()?)
}
