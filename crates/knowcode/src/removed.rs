//! A diff's pre-change side for `impact`: the definitions the change removes (deleted with their file, renamed, or
//! cut from it), and the code that still names them.
//!
//! The graph is the working tree's, and by the time `impact` runs the sync has dropped what the change removed (or an
//! earlier query did). So the removed definitions are read from the diff's base commit ([`kc_index::diff::old_definitions`])
//! and the code naming them is found by name in the current graph: the references stay, unresolved or resolving to
//! something else of the name. That is approximate, and the answer says so: no pre-change graph is kept, and a
//! reference the old graph had bound elsewhere by its receiver or imports is matched here by name alone.
//!
//! A C# method whose parameters the change alters is read the same way ([`resigned`]): the method is still there, but
//! a call written for its old parameters no longer binds to it, so the current graph has no edge from that call. Its
//! old signature counts as removed, and the calls still passing the old number of arguments are the code naming it.


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
    /// For a method the change kept with other parameters (see [`resigned`]): the declaration it had, whose calls are
    /// `named_by`. Absent for a definition the change removed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_signature: Option<String>,
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

/// How many arguments a call to a method passes: (fewest, most), most `None` with a `params` array.
type Arity = (u32, Option<u32>);
/// A method's arity, with whether it is an extension method.
type Form = (Arity, bool);

/// What the diff's base commit says is gone: the definitions it removed, and the C# methods it kept with other
/// parameters (whose old calls are gone).
pub struct PreChange<'a> {
    pub gone: &'a [OldDefinition],
    pub resigned: &'a [Resigned],
}

/// Whether a call passing `args` arguments fits `arity` (fewest, most; most `None` with a `params` array). An
/// extension method (`this` on its first parameter) may be called on a value, its receiver no argument, or statically,
/// its receiver the first.
fn fits(arity: Arity, extension: bool, args: u32) -> bool {
    let fit = |n: u32| n >= arity.0 && arity.1.is_none_or(|m| n <= m);
    fit(args) || (extension && args > 0 && fit(args - 1))
}

fn is_extension(signature: &str) -> bool {
    signature.split_once('(').is_some_and(|(_, params)| params.trim_start().starts_with("this "))
}

fn words(s: &str) -> Vec<&str> {
    s.split_whitespace().collect()
}

/// The declarations of the live definitions named `name` in `container` in `file`.
fn live_signatures(conn: &Connection, name: &str, container: Option<&str>, file: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare_cached(
        "SELECT s.container, s.signature FROM symbols s JOIN files f ON f.id = s.file_id WHERE s.name = ?1 AND f.path = ?2",
    )?;
    let rows = stmt.query_map([name, file], |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, String>(1)?)))?;
    let mut out = Vec::new();
    for row in rows {
        let (c, sig) = row?;
        if c.as_deref() == container {
            out.push(sig);
        }
    }
    Ok(out)
}

/// The arities of the live definitions named `name` in `container` in `file`, with whether each is an extension method.
fn live_arities(conn: &Connection, name: &str, container: Option<&str>, file: &str) -> Result<Vec<Form>> {
    let mut stmt = conn.prepare_cached(
        "SELECT s.container, s.min_args, s.max_args, s.signature FROM symbols s JOIN files f ON f.id = s.file_id \
         WHERE s.name = ?1 AND f.path = ?2",
    )?;
    let rows = stmt.query_map([name, file], |r| {
        Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<u32>>(1)?, r.get::<_, Option<u32>>(2)?, r.get::<_, String>(3)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (c, min, max, sig) = row?;
        if c.as_deref() == container
            && let Some(min) = min
        {
            out.push(((min, max), is_extension(&sig)));
        }
    }
    Ok(out)
}

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

/// A C# method the change kept with other parameters.
#[derive(Debug, Clone)]
pub struct Resigned {
    /// The method as it was at the base commit.
    pub def: OldDefinition,
    /// The overloads of its name in its type the change left alone (each taking what one of them took before), with
    /// whether each is an extension method: a call fitting one of them may have meant that one.
    pub kept: Vec<Form>,
}

/// Of `old`, the C# methods the change kept with other parameters: still defined in the same file and container
/// (not in `gone`), but nothing of the name there takes the number of arguments it took. A call written for the old
/// signature no longer binds to it. A method whose parameters changed type only, or that gained a parameter with a
/// default, still takes the old calls, which the graph binds as before: it isn't listed.
pub fn resigned(conn: &Connection, old: &[OldDefinition], gone: &[OldDefinition]) -> Result<Vec<Resigned>> {
    let mut out: Vec<Resigned> = Vec::new();
    for d in old.iter().filter(|d| d.lang == "csharp" && d.kind == "method") {
        let (Some(arity), Some(now)) = (d.arity, d.now.as_deref()) else { continue };
        let same = |o: &OldDefinition| o.file == d.file && o.name == d.name && o.container == d.container && o.line == d.line;
        if gone.iter().any(|g| g.file == d.file && g.name == d.name && g.container == d.container) || out.iter().any(|r| same(&r.def)) {
            continue;
        }
        // Its declaration as it was, still there word for word: it is an overload the change left alone.
        if live_signatures(conn, &d.name, d.container.as_deref(), now)?.iter().any(|s| words(s) == words(&d.signature)) {
            continue;
        }
        let live = live_arities(conn, &d.name, d.container.as_deref(), now)?;
        // The other overloads it had: a live one taking what one of them took is one the change left alone.
        let before: Vec<Arity> = old
            .iter()
            .filter(|o| o.file == d.file && o.name == d.name && o.container == d.container && o.line != d.line)
            .filter_map(|o| o.arity)
            .collect();
        // Fewer of the name now take every call its old signature took than did before (itself and the others): it is
        // one that changed. A parameter added with a default still takes them all.
        let covers = |a: Arity| a.0 <= arity.0 && (a.1.is_none() || arity.1.is_some_and(|m| a.1.is_some_and(|n| n >= m)));
        let had = 1 + before.iter().filter(|a| covers(**a)).count();
        let has = live.iter().filter(|(a, _)| covers(*a)).count();
        if !live.is_empty() && has < had {
            let kept = live.into_iter().filter(|(a, _)| before.contains(a)).collect();
            out.push(Resigned { def: d.clone(), kept });
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
    pre: &PreChange,
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
    let none: &[Form] = &[];
    let (gone, resigned) = (pre.gone, pre.resigned);
    let all = gone.iter().map(|d| (d, false, none)).chain(resigned.iter().map(|r| (&r.def, true, r.kept.as_slice())));
    for (d, changed, kept) in all {
        let ty = matches!(d.kind, "class" | "struct" | "enum" | "actor" | "protocol" | "typealias");
        let kinds: &[&str] = if ty { &["instantiate", "type_ref"] } else { &["call", "member"] };
        // A method kept with other parameters is itself live: only the name's definitions in other types count.
        let others = live(conn, &d.name)?.iter().any(|(_, c, _, lang)| family(lang) == family(d.lang) && !(changed && *c == d.container));
        // For such a method, which calls were written for its old parameters: those passing a number of arguments the
        // old signature took and the method's new one doesn't (a call it still takes binds to it as before). One that
        // an overload the change left alone also takes may have meant that one: which, the count can't tell.
        let ext = is_extension(&d.signature);
        let now_arities = match (changed, d.now.as_deref()) {
            (true, Some(now)) => live_arities(conn, &d.name, d.container.as_deref(), now)?,
            _ => Vec::new(),
        };
        let old_call = |args: Option<u32>| -> Option<bool> {
            if !changed {
                return Some(false);
            }
            let Some(args) = args else { return Some(true) };
            let old = d.arity?;
            if !fits(old, ext, args) {
                return None;
            }
            let takes = |list: &[Form]| list.iter().any(|&(a, e)| fits(a, e, args));
            let others_take = takes(kept);
            let changed_takes = now_arities.iter().filter(|x| !kept.contains(x)).any(|&(a, e)| fits(a, e, args));
            if changed_takes && !others_take {
                return None;
            }
            Some(others_take)
        };
        let owner = d.container.as_deref().map(|c| c.rsplit('.').next().unwrap_or(c));
        let via = qualified(d);
        let mut named_by: Vec<NamedBy> = Vec::new();
        // What a reference has before resolution rules most out (a common name's other languages, constructor
        // chaining in other types), and those are never resolved; of the rest, only those the loop below may take
        // are kept as they are resolved.
        let named = |h: &query::RefHit| {
            let Some(from) = &h.from else { return false };
            let lang: Option<String> = lang_of.query_row([&h.file], |r| r.get(0)).ok();
            if h.name != d.name || lang.as_deref().map(family) != Some(family(d.lang)) || old_call(h.args).is_none() {
                return false;
            }
            // A C# constructor chaining (`: this(…)`) names `init` of its own type only: another type's never meant
            // this one.
            !(d.name == "init" && h.file.ends_with(".cs") && from.container.as_deref().map(|c| c.rsplit('.').next().unwrap_or(c)) != owner)
        };
        let edge_of = |h: &query::RefHit| match h.kind.as_str() {
            "call" => "calls",
            "instantiate" => "instantiates",
            _ => "uses",
        };
        let may_mean = |h: &query::RefHit| {
            // Through a receiver of another type.
            let other_type = matches!((h.receiver_type.as_deref(), owner), (Some(rt), Some(o)) if rt.rsplit('.').next() != Some(o));
            // Bound for certain to something else of the name: it never meant the removed one. A call an overload the
            // change left alone takes is bound to that one now, which doesn't say what it meant before.
            let bound_elsewhere = !h.target_ids.is_empty() && (h.confidence == "exact" || h.precise) && old_call(h.args) != Some(true);
            !other_type && !bound_elsewhere && filter.allows(edge_of(h))
        };
        for h in query::refs_kept(conn, &d.name, kinds, named, may_mean)? {
            let Some(from) = &h.from else { continue };
            let typed = h.receiver_type.is_some() && owner.is_some();
            let edge = edge_of(&h);
            let site = format!("{}:{}", h.file, h.line);
            // One entry per occurrence: a same-line use through another receiver is judged on its own.
            if named_by.iter().any(|n| n.site == site && (n.col == h.col || n.col == 0 || h.col == 0)) {
                continue;
            }
            let Some(sym) = trace::symbol(conn, from.id)? else { continue };
            if named_by.len() >= max {
                truncated = true;
                break;
            }
            let confidence = if old_call(h.args) == Some(true) || !(typed || !others) { "ambiguous" } else { "unique_name" };
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
                library: None,
                also: Vec::new(),
                copies: Vec::new(),
                ways: Vec::new(),
                via_removed: true,
                evidence,
                cite: None,
            });
        }
        let old_signature = changed.then(|| d.signature.clone());
        list.push(Removed { qualified: via, kind: d.kind, file: d.file.clone(), line: d.line, old_signature, named_by });
    }
    // Onward from the code naming a removed definition, as from any affected symbol; an ambiguous link isn't followed.
    if !truncated && depth > 1 && !onward.is_empty() {
        let (more, cut) = onward_walk(&onward, max - affected.len(), seen)?;
        affected.extend(more);
        truncated |= cut;
    }
    Ok((affected, list, truncated))
}
