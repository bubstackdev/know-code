//! A diff's pre-change side for `impact`: the definitions the change removes (deleted with their file, renamed, or
//! cut from it), and the code that still names them.
//!
//! The graph is the working tree's, and by the time `impact` runs the sync has dropped what the change removed (or an
//! earlier query did). So the removed definitions are read from the diff's base commit ([`kc_index::diff::old_definitions`])
//! and the code naming them is found by name in the current graph: the references stay, unresolved or resolving to
//! something else of the name. That is approximate, and the answer says so: no pre-change graph is kept, and a
//! reference the old graph had bound elsewhere by its receiver or imports is matched here by name alone.


use anyhow::Result;
use kc_index::diff::OldDefinition;
use kc_store::query;
use kc_store::trace::{self, Affected, EdgeFilter};
use rusqlite::Connection;
use serde::Serialize;

/// How the answer's removed definitions were found, for its `pre_change` field.
pub const PRE_CHANGE: &str = "approximate: removed definitions are parsed from the base commit; the code still naming them is matched by name in the current graph";

/// A definition the change removed.
#[derive(Debug, Clone, Serialize)]
pub struct Removed {
    pub qualified: String,
    pub kind: &'static str,
    /// Its file and first line at the base commit.
    pub file: String,
    pub line: u32,
    /// The code still naming it. Each is in `affected` too: at hop 1 with `via_removed`, or, when the walk reached it
    /// first from a changed symbol, there.
    pub named_by: Vec<NamedBy>,
}

/// A symbol whose code names a removed definition.
#[derive(Debug, Clone, Serialize)]
pub struct NamedBy {
    pub qualified: String,
    pub file: String,
    pub line: u32,
    /// Where it names it, `file:line`.
    pub site: String,
    /// The column of the name on that line (0 when not known): two uses on one line are two entries.
    #[serde(skip_serializing_if = "is_zero")]
    pub col: u32,
    pub confidence: &'static str,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// Languages that resolve names into one another: TypeScript, TSX and JavaScript.
fn family(lang: &str) -> &str {
    match lang {
        "tsx" | "javascript" => "typescript",
        l => l,
    }
}

fn qualified(d: &OldDefinition) -> String {
    match &d.container {
        Some(c) => format!("{c}.{}", d.name),
        None => d.name.clone(),
    }
}

/// A live definition: id, container, file and language.
type Live = (i64, Option<String>, String, String);

/// Live definitions named `name` (exactly).
fn live(conn: &Connection, name: &str) -> Result<Vec<Live>> {
    let mut stmt = conn.prepare_cached(
        "SELECT s.id, s.name, s.container, f.path, f.lang FROM symbols s JOIN files f ON f.id = s.file_id WHERE s.name = ?1",
    )?;
    let rows = stmt.query_map([name], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?;
    let mut out = Vec::new();
    for row in rows {
        let (id, n, container, file, lang) = row?;
        if n == name {
            out.push((id, container, file, lang));
        }
    }
    Ok(out)
}

/// Of `old`, the definitions the change removed: nothing of the name in the same container is in its file now (a
/// deleted file has nothing). A definition moved to another file is not removed when its new copy is among the
/// changed symbols `scope`. One left in place under another kind, or as another overload, is not removed either.
pub fn removed(conn: &Connection, old: &[OldDefinition], scope: &[i64]) -> Result<Vec<OldDefinition>> {
    let mut out: Vec<OldDefinition> = Vec::new();
    for d in old {
        if out.iter().any(|o| o.file == d.file && o.name == d.name && o.container == d.container) {
            continue;
        }
        let same: Vec<_> = live(conn, &d.name)?
            .into_iter()
            .filter(|(_, c, _, lang)| *c == d.container && family(lang) == family(d.lang))
            .collect();
        let kept = same.iter().any(|(_, _, f, _)| d.now.as_deref() == Some(f.as_str()));
        let moved = same.iter().any(|(id, ..)| scope.contains(id));
        if !kept && !moved {
            out.push(d.clone());
        }
    }
    Ok(out)
}

/// The code still naming the `gone` definitions, and what depends on it, to hop `depth`: at most `max` symbols,
/// recorded in `seen`. Code naming a removed definition is a way to it at hop 1 with `via_removed`: it comes back
/// when that way is better than how the walk reached it before (see [`trace::Reached`]; the caller replaces the
/// row), and each removed definition lists all of it in `named_by`. Returns the new affected symbols, the removed definitions, and whether `max` cut the walk short.
///
/// A reference counts when it is in the definition's language and its receiver's type, if known, is the definition's
/// container. It is `unique_name` when nothing else of the name is left in that language (or the receiver's type
/// Walks on from code naming a removed definition (symbols reached at hop 1): `(from, room, seen)` to the rows found
/// and whether the room cut them (see `crate::tree::dependents`).
pub type OnwardWalk<'w> = dyn FnMut(&[i64], usize, &mut trace::Reached) -> Result<(Vec<Affected>, bool)> + 'w;

/// says it is this one), else `ambiguous`; one the current graph binds `exact`ly to another definition is left out.
pub fn dependents(
    conn: &Connection,
    gone: &[OldDefinition],
    seen: &mut trace::Reached,
    filter: &EdgeFilter,
    depth: u8,
    max: usize,
    onward_walk: &mut OnwardWalk,
) -> Result<(Vec<Affected>, Vec<Removed>, bool)> {
    let mut affected: Vec<Affected> = Vec::new();
    // The code naming a removed definition confidently, first reached so (or nearer) here: the walk goes on from it.
    let mut onward: Vec<i64> = Vec::new();
    let mut list = Vec::new();
    let mut truncated = false;
    let mut lang_of = conn.prepare_cached("SELECT lang FROM files WHERE path = ?1")?;
    for d in gone {
        let ty = matches!(d.kind, "class" | "struct" | "enum" | "actor" | "protocol" | "typealias");
        let kinds: &[&str] = if ty { &["instantiate", "type_ref"] } else { &["call", "member"] };
        let others = live(conn, &d.name)?.iter().any(|(.., lang)| family(lang) == family(d.lang));
        let owner = d.container.as_deref().map(|c| c.rsplit('.').next().unwrap_or(c));
        let via = qualified(d);
        let mut named_by: Vec<NamedBy> = Vec::new();
        for h in query::refs(conn, &d.name, kinds, usize::MAX)?.0 {
            let Some(from) = &h.from else { continue };
            let lang: Option<String> = lang_of.query_row([&h.file], |r| r.get(0)).ok();
            if h.name != d.name || lang.as_deref().map(family) != Some(family(d.lang)) {
                continue;
            }
            // A C# constructor chaining (`: this(…)`) names `init` of its own type only: another type's never meant
            // this one.
            if d.name == "init" && h.file.ends_with(".cs") && from.container.as_deref().map(|c| c.rsplit('.').next().unwrap_or(c)) != owner {
                continue;
            }
            let typed = match (h.receiver_type.as_deref(), owner) {
                (Some(rt), Some(o)) if rt.rsplit('.').next() != Some(o) => continue,
                (Some(_), Some(_)) => true,
                _ => false,
            };
            // Bound for certain to something else of the name: it never meant the removed one.
            if !h.target_ids.is_empty() && (h.confidence == "exact" || h.precise) {
                continue;
            }
            let edge = match h.kind.as_str() {
                "call" => "calls",
                "instantiate" => "instantiates",
                _ => "uses",
            };
            let site = format!("{}:{}", h.file, h.line);
            // One entry per occurrence: a same-line use through another receiver is judged on its own.
            if !filter.allows(edge) || named_by.iter().any(|n| n.site == site && (n.col == h.col || n.col == 0 || h.col == 0)) {
                continue;
            }
            let Some(sym) = trace::symbol(conn, from.id)? else { continue };
            if named_by.len() >= max {
                truncated = true;
                break;
            }
            let confidence = if typed || !others { "unique_name" } else { "ambiguous" };
            named_by.push(NamedBy {
                qualified: sym.qualified(),
                file: sym.file.clone(),
                line: sym.line,
                site: site.clone(),
                col: h.col,
                confidence,
            });
            // The removed definition is gone: only the name links the code to it.
            let evidence = kc_store::answer::Evidence::by_confidence(confidence, kc_store::answer::Mechanism::NameUnique);
            let verdict = filter.judge((sym.id, &via, &site, h.col), confidence, evidence.as_ref());
            if verdict == kc_store::answer::Verdict::Exclude {
                continue;
            }
            let Some(grow) = seen.offer_as(sym.id, 1, confidence, verdict == kc_store::answer::Verdict::Follow) else { continue };
            if grow {
                onward.push(sym.id);
            }
            if affected.len() >= max {
                truncated = true;
                continue;
            }
            affected.push(Affected {
                id: sym.id,
                qualified: sym.qualified(),
                kind: sym.kind.clone(),
                file: sym.file.clone(),
                line: sym.line,
                hop: 1,
                nearest_ambiguous_hop: None,
                class: trace::class_of(1, &sym.file),
                edge,
                confidence,
                via: via.clone(),
                site: Some(site),
                repo: None,
                http: None,
                bus: None,
                sql: None,
                role: None,
                deploy: None,
                module: None,
                copies: Vec::new(),
                via_removed: true,
                evidence,
                cite: None,
            });
        }
        list.push(Removed { qualified: via, kind: d.kind, file: d.file.clone(), line: d.line, named_by });
    }
    // Onward from the code naming a removed definition, as from any affected symbol; an ambiguous link isn't followed.
    if !truncated && depth > 1 && !onward.is_empty() {
        let (more, cut) = onward_walk(&onward, max - affected.len(), seen)?;
        affected.extend(more);
        truncated |= cut;
    }
    Ok((affected, list, truncated))
}
