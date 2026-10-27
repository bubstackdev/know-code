//! `trace` and `impact` walked on the symbol graph ([`crate::walk::Graph`]), as `path` and `flow` are: every edge is
//! read through the graph's edge caches and its gate ([`crate::graph::Gate`]), in a repo
//! ([`Graph::tree_edges`]) and across repos ([`crate::graph::Crossing`], each arrival at its confidence across the
//! link, [`crate::graph::across`]).
//!
//! A trace grows depth first from its root (out, then in), each symbol grown again where it is reached with more
//! depth left; then, shallowest first, each node it grew crosses into the code requesting the action it is, consuming
//! the message it is or publishes, using the SQL object it is, deploying the role or job it is, and that code is
//! traced on in its own repo. An impact walks dependents breadth first, each listed once by its best way
//! ([`trace::Reached`]); then crosses the same way from each symbol it grew, and walks on in the repo it lands in.
//! A node either walk lists without growing is a [`trace::Stop`], for the answer's frontier.

use std::collections::{BinaryHeap, HashMap, HashSet};
use std::cmp::Reverse;
use std::path::PathBuf;

use anyhow::Result;
use kc_store::answer::Verdict;
use kc_store::query::SymbolRow;
use kc_store::trace::{self, Affected, Impact, Reached, Stop, Trace};

use crate::ops::repo_name;
use crate::walk::Graph;

/// The trace of `root` (a symbol of the graph's home repo) `direction` (`in`, `out` or both), to `depth` hops (at most
/// [`trace::MAX_TRACE_DEPTH`]), at most `max` nodes.
pub fn trace(g: &mut Graph, root: &SymbolRow, direction: trace::Direction, depth: u8, max: usize) -> Result<Trace> {
    let mut t = Trace { nodes: vec![trace::node(root, 0, None, "", "", "", None)], truncated: false, omitted: 0, stopped: Vec::new() };
    let depth = depth.clamp(1, trace::MAX_TRACE_DEPTH);
    let (inward, outward) = (direction != trace::Direction::Out, direction != trace::Direction::In);
    // A library's users depend on it: crossing to them is for a walk inward only.
    g.crossing().library = inward;
    if outward {
        grow(g, &mut t, 0, 0, &None, depth, "out", &mut HashMap::from([(root.id, depth)]), max)?;
    }
    if inward {
        grow(g, &mut t, 0, 0, &None, depth, "in", &mut HashMap::from([(root.id, depth)]), max)?;
    }
    cross_trace(g, &mut t, depth, max, inward, outward)?;
    Ok(t)
}

/// Lists what node `at` (of repo `repo`, labelled `label` when it isn't the home repo) leads to (`dir`), `left` levels
/// down. `expanded` holds, per symbol, the most levels any of its nodes was grown with: a symbol reached again with
/// more levels left is grown again there, so depth first order never hides what lies within the depth asked for.
#[allow(clippy::too_many_arguments)]
fn grow(g: &mut Graph, t: &mut Trace, at: usize, repo: usize, label: &Option<String>, left: u8, dir: &'static str, expanded: &mut HashMap<i64, u8>, max: usize) -> Result<()> {
    if left == 0 {
        return Ok(());
    }
    let Some(sym) = trace::symbol(g.conn(repo), t.nodes[at].id)? else { return Ok(()) };
    let depth = t.nodes[at].depth + 1;
    let left = left - 1;
    for (h, precise) in g.tree_edges((repo, sym.id), &sym, dir, false)? {
        kc_store::answer::debug_check(|| format!("{} to {}", h.edge, h.sym.qualified()), h.confidence, h.evidence.as_ref());
        let verdict = g.gate().judge((label, sym.id, h.sym.id, h.edge, &h.site), h.confidence, h.evidence.as_ref());
        if verdict == Verdict::Exclude {
            continue;
        }
        let expand = verdict == Verdict::Follow && expanded.get(&h.sym.id).is_none_or(|&had| had < left);
        if expand {
            expanded.insert(h.sym.id, left);
        }
        // Past the limit, the rest are counted, not listed.
        if t.nodes.len() >= max {
            t.truncated = true;
            t.omitted += 1;
            continue;
        }
        let mut n = trace::node(&h.sym, depth, Some(at), dir, h.edge, h.confidence, h.site.clone());
        n.binding = h.binding;
        n.precise = precise;
        n.evidence = h.evidence;
        n.repo.clone_from(label);
        t.nodes.push(n);
        let idx = t.nodes.len() - 1;
        if expand && left > 0 {
            grow(g, t, idx, repo, label, left, dir, expanded, max)?;
        } else if expand {
            // At the depth asked for: a deeper trace would go on from here.
            t.stopped.push(Stop { id: h.sym.id, hop: depth, reason: "depth", dir, repo: label.clone() });
        } else if verdict == Verdict::Leaf {
            t.stopped.push(Stop { id: h.sym.id, hop: depth, reason: "ambiguous", dir, repo: label.clone() });
        }
    }
    Ok(())
}

/// Continues `t` (to `depth`, at most `max` nodes) across repos: below each action, the code requesting it; below
/// each message type or publisher, the consumers; below each SQL object, role or job, the code using or deploying it;
/// each traced inward in its own repo.
fn cross_trace(g: &mut Graph, t: &mut Trace, depth: u8, max: usize, inward: bool, outward: bool) -> Result<()> {
    let home = g.crossing().home_root();
    // Each labelled repo's number in the graph.
    let mut repos: HashMap<String, usize> = HashMap::new();
    let mut crossed: HashSet<(usize, i64)> = HashSet::new();
    // Shallowest first, so a symbol in the tree twice crosses where the most depth is left below it.
    let mut queue: BinaryHeap<Reverse<(u8, usize)>> = BinaryHeap::new();
    let mut queued = 0;
    loop {
        for j in queued..t.nodes.len() {
            queue.push(Reverse((t.nodes[j].depth, j)));
        }
        queued = t.nodes.len();
        let Some(Reverse((_, at))) = queue.pop() else { break };
        let n = &t.nodes[at];
        let confident = !n.confidence.is_empty() && g.gate().follows(n.confidence, n.evidence.as_ref());
        let expanded = at == 0 || (confident && ((n.direction == "in" && inward) || (n.direction == "out" && outward)));
        if !expanded || n.depth >= depth {
            continue;
        }
        let repo = match &n.repo {
            Some(name) => repos[name],
            None => 0,
        };
        if !crossed.insert((repo, n.id)) {
            continue;
        }
        let (id, qualified, file) = (n.id, n.qualified.clone(), n.file.clone());
        let root = g.root(repo).to_path_buf();
        g.gate().expand()?;
        // A publishing method's consumers run after it (out); a message type's consumers depend on it (in), as does
        // the code requesting an action.
        let publisher = matches!(n.kind.as_str(), "method" | "function" | "init");
        let downward = at == 0 || n.direction == "out";
        for landing in g.crossing().crossings(&root, id, &qualified, &file)? {
            let downstream = publisher && matches!(landing.via, trace::Via::Bus(_));
            if (downstream && !(outward && downward)) || (!downstream && !inward) {
                continue;
            }
            let there = g.index_of(&landing.root);
            let label = (landing.root != home).then(|| repo_name(&landing.root));
            if let Some(l) = &label {
                repos.insert(l.clone(), there);
            }
            for a in &landing.arrivals {
                // Code already in the tree (a test calling the action directly, say) isn't listed again.
                if t.nodes.iter().any(|n| n.id == a.sym().id && n.repo == label) {
                    continue;
                }
                if landing.judge(g.gate(), &root, id, a) == Verdict::Exclude {
                    continue;
                }
                let dir = if downstream { "out" } else { "in" };
                if t.nodes.len() >= max {
                    t.truncated = true;
                    t.omitted += 1;
                    return Ok(());
                }
                let parent = &t.nodes[at];
                let mut n = trace::node(a.sym(), parent.depth + 1, Some(at), dir, landing.via.edge(), a.confidence, a.site().clone());
                n.evidence.clone_from(&a.evidence);
                n.repo.clone_from(&label);
                n.http = landing.via.http();
                n.bus = landing.via.bus();
                n.sql = landing.via.sql();
                n.role = landing.via.role();
                n.deploy = landing.via.deploy();
                n.module = landing.via.module();
                n.library = landing.via.library();
                // A publisher's bus hop happens at the publish call, in the publisher's repo; the consumer is the node.
                if downstream && let Some(publish) = &landing.publish {
                    let from = parent.repo.clone();
                    n.site = Some(publish.clone());
                    if from != label {
                        n.site_repo = Some(from.unwrap_or_else(|| repo_name(&home)));
                    }
                }
                t.nodes.push(n);
                let idx = t.nodes.len() - 1;
                // A node the tree won't grow: at its depth, or reached by an ambiguous link.
                let (n, follows) = (&t.nodes[idx], g.gate().follows(a.confidence, a.evidence.as_ref()));
                let reason = if !follows { Some("ambiguous") } else if n.depth >= depth { Some("depth") } else { None };
                if let Some(reason) = reason {
                    t.stopped.push(Stop { id: n.id, hop: n.depth, reason, dir, repo: n.repo.clone() });
                }
                // The code reached is traced on in its own repo, inward.
                if !downstream && follows {
                    let left = depth.saturating_sub(t.nodes[idx].depth);
                    let id = t.nodes[idx].id;
                    grow(g, t, idx, there, &label, left, "in", &mut HashMap::from([(id, left)]), max)?;
                }
            }
        }
    }
    Ok(())
}

/// The name a walk takes for symbol `qualified` declared at `file` in a repo: the qualified name, or `file#Name` when
/// other definitions share it (so the call goes on from this one).
pub fn symbol_arg(conn: &rusqlite::Connection, qualified: &str, file: &str, line: u32) -> Result<String> {
    let defs = kc_store::query::definitions(conn, qualified)?;
    if defs.len() <= 1 {
        return Ok(qualified.to_string());
    }
    // Overloads in one file (`Log.ForContext<T>()` beside `Log.ForContext(Type)`): the line tells them apart.
    if defs.iter().filter(|d| d.file == file).count() > 1 {
        return Ok(format!("{file}:{line}"));
    }
    let name = qualified.rsplit('.').next().unwrap_or(qualified);
    Ok(format!("{file}#{name}"))
}

/// Gives each of `frontier` (stops of a walk on `g`) the exact call going on from it: `tool` with `args` (the walk's
/// depth and filters) and the stop's symbol and repo, plus what `extra` adds per stop (a trace's direction).
pub fn continuations(
    g: &mut Graph,
    frontier: &mut [trace::Frontier],
    tool: &str,
    args: &serde_json::Map<String, serde_json::Value>,
    extra: impl Fn(&trace::Frontier, &mut serde_json::Map<String, serde_json::Value>),
) -> Result<()> {
    let home = g.crossing().home_root();
    for f in frontier {
        let root = match &f.repo {
            None => home.clone(),
            Some(label) => match g.stores().into_iter().find(|(r, _)| repo_name(r) == *label) {
                Some((r, _)) => r,
                None => continue,
            },
        };
        let symbol = symbol_arg(g.crossing().store(&root).conn(), &f.qualified, &f.file, f.line)?;
        let mut call = serde_json::Map::new();
        call.insert("symbol".into(), symbol.into());
        call.insert("repo".into(), repo_name(&root).into());
        call.extend(args.clone());
        extra(f, &mut call);
        f.next = format!("{tool} {}", serde_json::Value::Object(call));
    }
    Ok(())
}

/// What depends on `ids` (symbols of the graph's home repo), to `depth` hops (at most [`trace::MAX_IMPACT_DEPTH`]),
/// at most `max` of them, in the home repo: each listed once, by its best way (see [`Reached`]).
pub fn impact(g: &mut Graph, ids: &[i64], depth: u8, max: usize) -> Result<Impact> {
    let mut scope = Vec::new();
    let mut seen = Reached::default();
    for &id in ids {
        if seen.root(id)
            && let Some(s) = trace::symbol(g.conn(0), id)?
        {
            scope.push(trace::node(&s, 0, None, "", "", "", None));
        }
    }
    let ids: Vec<i64> = scope.iter().map(|s| s.id).collect();
    let (mut affected, truncated) = dependents(g, 0, &ids, 0, depth.clamp(1, trace::MAX_IMPACT_DEPTH), max, &mut seen)?;
    for a in &mut affected {
        seen.settle(a);
    }
    let stopped = seen.stops(None);
    let mut out = Impact { scope, affected, counts: Default::default(), nearest: "none", low_confidence: 0, truncated, stopped };
    out.recount();
    Ok(out)
}

/// Where a dependent's row is during [`dependents`]: among this hop's, or listed at an earlier one.
enum Slot {
    Found(usize),
    Listed(usize),
}

/// What depends on `from`, symbols of repo `repo` reached at hop `hop`: breadth first from hop `hop + 1` to `depth`, at
/// most `max` of them, listing those `seen` has no better way to (and recording them there). A symbol `seen` held
/// already comes back when this walk found a better way to it: the caller replaces its row. Also says whether `max`
/// cut the walk short. Rows carry no repo: the caller labels them.
pub fn dependents(g: &mut Graph, repo: usize, from: &[i64], hop: u8, depth: u8, max: usize, seen: &mut Reached) -> Result<(Vec<Affected>, bool)> {
    let mut frontier = Vec::new();
    for &id in from {
        if let Some(s) = trace::symbol(g.conn(repo), id)? {
            frontier.push(s);
        }
    }
    let mut affected: Vec<Affected> = Vec::new();
    let mut slots: HashMap<i64, Slot> = HashMap::new();
    for hop in hop.saturating_add(1)..=depth {
        // This hop's dependents, in the order found, each with what the walk continues from.
        let mut found: Vec<(Affected, Vec<SymbolRow>)> = Vec::new();
        // What the walk continues from for symbols listed at an earlier hop and first reached confidently at this one.
        let mut late: Vec<SymbolRow> = Vec::new();
        for sym in &frontier {
            for (h, _) in g.tree_edges((repo, sym.id), sym, "in", true)? {
                let (dep, edge, confidence, site, evidence) = (h.sym, h.edge, h.confidence, h.site, h.evidence);
                kc_store::answer::debug_check(|| format!("{edge} from {}", dep.qualified()), confidence, evidence.as_ref());
                let verdict = g.gate().judge((&None::<String>, sym.id, dep.id, edge, &site), confidence, evidence.as_ref());
                if verdict == Verdict::Exclude {
                    continue;
                }
                // Its other sites reaching `sym`: another reference on the site's own line says nothing new.
                let mut also: Vec<String> = g.also(repo, dep.id, sym.id).into_iter().filter(|a| Some(a) != site.as_ref()).collect();
                also.sort_by_key(|a| a.rsplit_once(':').map(|(f, l)| (f.to_string(), l.parse::<u32>().unwrap_or(0))));
                let Some(grow) = seen.offer_as(dep.id, hop, confidence, verdict == Verdict::Follow) else {
                    // Listed by a better way: this one is another of its ways.
                    let listed = match slots.get(&dep.id) {
                        Some(&Slot::Found(i)) => Some(&mut found[i].0),
                        Some(&Slot::Listed(i)) => Some(&mut affected[i]),
                        None => None,
                    };
                    if let Some(r) = listed {
                        r.add_way(&sym.qualified(), hop, confidence, site.as_ref(), &also);
                    }
                    continue;
                };
                let row = Affected {
                    id: dep.id,
                    qualified: dep.qualified(),
                    kind: dep.kind.clone(),
                    file: dep.file.clone(),
                    line: dep.line,
                    hop,
                    nearest_ambiguous_hop: None,
                    class: trace::class_of(hop, &dep.file),
                    edge,
                    confidence,
                    via: sym.qualified(),
                    site,
                    repo: None,
                    http: None,
                    bus: None,
                    sql: None,
                    role: None,
                    deploy: None,
                    module: None,
                    library: None,
                    also,
                    copies: Vec::new(),
                    ways: Vec::new(),
                    via_removed: false,
                    evidence,
                    cite: None,
                };
                let mut onward = Vec::new();
                if grow {
                    // An Entity Framework context holding a DbSet of the entity changes only for the code using that
                    // DbSet: the walk continues from it, not from everything injecting or constructing the context.
                    let sets = trace::entity_sets(g.conn(repo), sym, &dep)?;
                    if sets.is_empty() {
                        onward.push(dep);
                    } else {
                        seen.grew(dep.id);
                        onward.extend(sets.into_iter().filter(|s| seen.offer(s.id, hop, "exact") == Some(true)));
                    }
                } else if verdict == Verdict::Leaf {
                    // Listed, not walked past: what lies behind it is reported if anything does.
                    seen.stop(dep.id, hop, "ambiguous");
                }
                for o in &onward {
                    // At the depth asked for, the walk would go on from here: a stop of its own.
                    if hop < depth { seen.grew(o.id) } else { seen.stop(o.id, hop, "depth") }
                }
                // A better way to a symbol already listed replaces its row.
                match slots.get(&row.id) {
                    Some(&Slot::Found(i)) => {
                        found[i].0 = std::mem::replace(&mut found[i].0, row.clone()).replaced_by(row);
                        found[i].1.extend(onward);
                    }
                    Some(&Slot::Listed(i)) => {
                        affected[i] = std::mem::replace(&mut affected[i], row.clone()).replaced_by(row);
                        late.extend(onward);
                    }
                    None => {
                        slots.insert(row.id, Slot::Found(found.len()));
                        found.push((row, onward));
                    }
                }
            }
        }
        // At the limit, what a change reaches directly (a use, a call, a subtype) outranks what it reaches only
        // through the container's binding or a factory it injects.
        let room = max.saturating_sub(affected.len());
        if found.len() > room {
            let weak = |a: &Affected| matches!(a.edge, "via_di" | "constructs");
            let mut order: Vec<usize> = (0..found.len()).collect();
            order.sort_by_key(|&i| weak(&found[i].0));
            let mut keep = vec![false; found.len()];
            for &i in order.iter().take(room) {
                keep[i] = true;
            }
            affected.extend(found.into_iter().zip(keep).filter(|(_, k)| *k).map(|((a, _), _)| a));
            return Ok((affected, true));
        }
        let mut next = Vec::new();
        for (row, onward) in found {
            slots.insert(row.id, Slot::Listed(affected.len()));
            affected.push(row);
            next.extend(onward);
        }
        next.extend(late);
        frontier = next;
        if frontier.is_empty() {
            break;
        }
    }
    Ok((affected, false))
}

/// Continues `impact` (of the home repo, to `depth` hops, at most `max` affected) across repos: from each action among
/// the changed or affected symbols to the code requesting it, from each message type or publisher to the consumers,
/// from SQL objects, roles and jobs to the code using or deploying them, and on to their dependents. Each symbol is
/// listed once, by its best way in any order the crossings are found (see [`Reached`]): a better way replaces its row
/// and grows it again.
pub fn cross_impact(g: &mut Graph, impact: &mut Impact, depth: u8, max: usize) -> Result<()> {
    let depth = depth.clamp(1, trace::MAX_IMPACT_DEPTH);
    let home = g.crossing().home_root();
    let mut seen: HashMap<PathBuf, Reached> = HashMap::new();
    let mine = seen.entry(home.clone()).or_default();
    for s in &impact.scope {
        mine.root(s.id);
    }
    for a in &impact.affected {
        if let Some(h) = a.nearest_ambiguous_hop {
            mine.offer(a.id, h, "ambiguous");
        }
        mine.offer(a.id, a.hop, a.confidence);
    }
    // Each row's place, by repo and symbol, for replacing it.
    let mut rows: HashMap<(Option<String>, i64), usize> = impact.affected.iter().enumerate().map(|(i, a)| ((a.repo.clone(), a.id), i)).collect();
    let mut changed = false;
    let place = |impact: &mut Impact, rows: &mut HashMap<(Option<String>, i64), usize>, a: Affected| match rows.get(&(a.repo.clone(), a.id)) {
        Some(&i) => impact.affected[i] = std::mem::replace(&mut impact.affected[i], a.clone()).replaced_by(a),
        None => {
            rows.insert((a.repo.clone(), a.id), impact.affected.len());
            impact.affected.push(a);
        }
    };
    // Symbols a crossing may continue from: in a repo, at a hop.
    let mut work: Vec<(PathBuf, i64, String, String, u8)> = impact.scope.iter().map(|s| (home.clone(), s.id, s.qualified.clone(), s.file.clone(), 0)).collect();
    let gate = g.gate().clone();
    let expands = |a: &Affected| gate.follows(a.confidence, a.evidence.as_ref());
    work.extend(impact.affected.iter().filter(|a| expands(a)).map(|a| (home.clone(), a.id, a.qualified.clone(), a.file.clone(), a.hop)));
    let mut i = 0;
    while i < work.len() {
        let (root, id, qualified, file, hop) = work[i].clone();
        i += 1;
        // Reached nearer since it was queued: that way's item continues from it.
        if hop >= depth || seen.get(&root).and_then(|r| r.hop(id)).is_some_and(|h| h < hop) {
            continue;
        }
        gate.expand()?;
        for landing in g.crossing().crossings(&root, id, &qualified, &file)? {
            let there = g.index_of(&landing.root);
            let label = (landing.root != home).then(|| repo_name(&landing.root));
            let seen_there = seen.entry(landing.root.clone()).or_default();
            let mut from = Vec::new();
            for arrival in &landing.arrivals {
                let verdict = landing.judge(&gate, &root, id, arrival);
                if verdict == Verdict::Exclude {
                    continue;
                }
                let (s, confidence) = (arrival.sym(), arrival.confidence);
                // A symbol reaching it again at another line, by the same way and hop: one of its lines (`also`).
                if let Some(&i) = rows.get(&(label.clone(), s.id))
                    && let Some(site) = arrival.site()
                {
                    let row = &mut impact.affected[i];
                    if row.hop == hop + 1 && row.via == qualified && row.edge == landing.via.edge() && row.site.as_ref() != Some(site) {
                        if !row.also.contains(site) {
                            row.also.push(site.clone());
                        }
                        continue;
                    }
                }
                let new = !rows.contains_key(&(label.clone(), s.id));
                if new && impact.affected.len() >= max {
                    impact.truncated = true;
                    break;
                }
                let Some(grow) = seen_there.offer_as(s.id, hop + 1, confidence, verdict == Verdict::Follow) else {
                    if let Some(&i) = rows.get(&(label.clone(), s.id)) {
                        impact.affected[i].add_way(&qualified, hop + 1, confidence, arrival.site().as_ref(), &[]);
                    }
                    continue;
                };
                let a = Affected {
                    id: s.id,
                    qualified: s.qualified(),
                    kind: s.kind.clone(),
                    file: s.file.clone(),
                    line: s.line,
                    hop: hop + 1,
                    nearest_ambiguous_hop: None,
                    class: trace::class_of(hop + 1, &s.file),
                    edge: landing.via.edge(),
                    confidence,
                    via: qualified.clone(),
                    site: arrival.site().clone(),
                    repo: label.clone(),
                    http: landing.via.http(),
                    bus: landing.via.bus(),
                    sql: landing.via.sql(),
                    role: landing.via.role(),
                    deploy: landing.via.deploy(),
                    module: landing.via.module(),
                    library: landing.via.library(),
                    also: Vec::new(),
                    copies: Vec::new(),
                    ways: Vec::new(),
                    via_removed: false,
                    evidence: arrival.evidence.clone(),
                    cite: None,
                };
                if grow {
                    // At the depth asked for, it is where the walk stops.
                    if hop + 1 < depth { seen_there.grew(s.id) } else { seen_there.stop(s.id, hop + 1, "depth") }
                    from.push(s.id);
                    work.push((landing.root.clone(), a.id, a.qualified.clone(), a.file.clone(), a.hop));
                } else if verdict == Verdict::Leaf {
                    seen_there.stop(s.id, hop + 1, "ambiguous");
                }
                place(impact, &mut rows, a);
                changed = true;
            }
            let room = max.saturating_sub(impact.affected.len());
            let (more, truncated) = dependents(g, there, &from, hop + 1, depth, room, seen_there)?;
            impact.truncated |= truncated;
            for mut a in more {
                a.repo.clone_from(&label);
                if expands(&a) {
                    work.push((landing.root.clone(), a.id, a.qualified.clone(), a.file.clone(), a.hop));
                }
                place(impact, &mut rows, a);
                changed = true;
            }
        }
    }
    // Where the walk stopped, in every repo it reached: what a later way walked past is no stop.
    let by_label: HashMap<Option<String>, &Reached> = seen.iter().map(|(root, r)| ((*root != home).then(|| repo_name(root)), r)).collect();
    let mut stops = std::mem::take(&mut impact.stopped);
    for (label, r) in &by_label {
        stops.extend(r.stops(label.as_deref()));
    }
    let mut kept = HashSet::new();
    stops.retain(|st| !by_label.get(&st.repo).is_some_and(|r| r.was_grown(st.id)) && kept.insert((st.repo.clone(), st.id)));
    impact.stopped = stops;
    if changed {
        // Each row's class by the nearest way, ambiguous or not, to it in its repo.
        for a in &mut impact.affected {
            if let Some(r) = by_label.get(&a.repo) {
                r.settle(a);
            }
        }
        impact.recount();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::ops::{self, Scope};
    use serde_json::Value;

    /// The arguments of a continuation call `kc_<tool> {…}`.
    fn args(call: &str, tool: &str) -> Value {
        let json = call.strip_prefix(&format!("{tool} ")).unwrap_or_else(|| panic!("not a {tool} call: {call}"));
        serde_json::from_str(json).unwrap()
    }

    /// Every place a walk stops short names the exact call going on from it, and that call goes on: a trace or impact
    /// stop at its depth, a flow step at its depth, a path search out of budget, and one finding no path in its hops.
    #[test]
    fn every_stop_says_why_and_gives_the_call_that_goes_on() {
        let cache = ops::tests::test_cache();
        let base = cache.dir("tree-stops");
        let (web, api, _schema) = crate::walk::tests::shop(&base, "ts");
        let scope = |r: &std::path::Path| Scope { repos: vec![r.to_path_buf()], no_sync: false };

        // impact, one hop: stops at OrderService.place, whose dependents it didn't list.
        let ip = ops::ImpactParams { symbol: Some("LoggerService.info".into()), depth: 1, limit: 50, ..Default::default() };
        let i = ops::impact(&ip, &scope(&web)).unwrap();
        let f = &i.data[0].frontier;
        assert_eq!(f.len(), 1);
        assert_eq!((f[0].qualified.as_str(), f[0].reason), ("OrderService.place", "depth"));
        let a = args(&f[0].next, "kc_impact");
        assert_eq!(a, serde_json::json!({ "symbol": "OrderService.place", "repo": "web", "depth": 1 }));
        let depth = crate::contract::incomplete_of(&i.env).into_iter().find(|e| e.reason == kc_store::answer::Reason::Depth).unwrap();
        assert!(depth.hint.unwrap().ends_with(&format!("continue: {}", f[0].next)));
        // The call goes on: what the first walk didn't list.
        let ip = ops::ImpactParams { symbol: Some(a["symbol"].as_str().unwrap().into()), depth: 1, limit: 50, ..Default::default() };
        let on = ops::impact(&ip, &scope(&web)).unwrap();
        assert!(on.data[0].impact.affected.iter().any(|r| r.qualified == "OrderEffects.place$"));

        // trace, in: the stop's direction is in its call.
        let t = ops::trace("LoggerService.info", "in", 1, 50, &ops::WalkFilter::default(), &scope(&web)).unwrap();
        let (_, frontier) = (&t.data.found[0], t.env["items"][0]["frontier"].as_array().unwrap());
        let a = args(frontier[0]["next"].as_str().unwrap(), "kc_trace");
        assert_eq!((a["symbol"].as_str(), a["direction"].as_str(), a["depth"].as_u64()), (Some("OrderService.place"), Some("in"), Some(1)), "{a}");

        // flow, two deep: the step at the depth leading on carries its stop, and the answer says so.
        let fo = crate::flow::FlowOptions { depth: 2, max_steps: 50, max_repos: 16, edges: Default::default(), children: crate::flow::DEFAULT_CHILDREN };
        let fl = crate::flow::flow("/orders/new/", &fo, &scope(&web)).unwrap();
        let flow = fl.data.as_ref().unwrap();
        let stopped: Vec<_> = flow.steps.iter().filter_map(|s| s.stop.as_ref().map(|st| (s.step.qualified.as_str(), st))).collect();
        assert_eq!(stopped.len(), 1, "{}", fl.env);
        let (name, stop) = stopped[0];
        assert_eq!((name, stop.reason), ("OrderService.place", "depth"));
        let a = args(&stop.next, "kc_flow");
        assert_eq!(a, serde_json::json!({ "entry": "src/order.service.ts:3", "repo": "web", "depth": 2 }));
        let entry = crate::contract::incomplete_of(&fl.env).into_iter().find(|e| e.reason == kc_store::answer::Reason::Depth).expect("depth");
        assert!(entry.detail.starts_with("flow: depth 2 reached: 1 step there leads to"), "{}", entry.detail);
        let on = crate::flow::flow(a["entry"].as_str().unwrap(), &fo, &scope(&web)).unwrap();
        assert!(!on.data.unwrap().steps.is_empty());
        // Cut by its step limit: the call raising it.
        let small = crate::flow::FlowOptions { max_steps: 1, ..fo.clone() };
        let cut = crate::flow::flow("/orders/new/", &small, &scope(&web)).unwrap();
        let capped = crate::contract::incomplete_of(&cut.env).into_iter().find(|e| e.detail.starts_with("flow: the step limit")).expect("capped");
        let a = args(capped.hint.unwrap().strip_prefix("continue: ").unwrap(), "kc_flow");
        assert_eq!(a["limit"], 2);
        assert!(crate::contract::incomplete_of(&cut.env).iter().all(|e| !e.detail.starts_with("items[]")), "{}", cut.env);

        // path: out of budget, the call raising it; no path in its hops, the call going further.
        let po = |max_nodes, max_hops| crate::walk::PathOptions {
            max_hops,
            max_nodes,
            max_repos: 16,
            paths: 3,
            one_way: false,
            allow_ambiguous: false,
            edges: Default::default(),
            from_skip: 0,
            to_skip: 0,
        };
        let p = crate::walk::path("OrderPage.submit", "OrderPlacedConsumer.Consume", Some(&api), &po(2, 8), &scope(&web)).unwrap();
        let capped = crate::contract::incomplete_of(&p.env).into_iter().find(|e| e.detail.starts_with("path search:")).expect("capped");
        let a = args(capped.hint.unwrap().strip_prefix("continue: ").unwrap(), "kc_path");
        assert_eq!((a["max_nodes"].as_u64(), a["to_repo"].as_str()), (Some(4), Some("api")), "{a}");
        let p = crate::walk::path("OrderPage.submit", "OrderPlacedConsumer.Consume", Some(&api), &po(5000, 2), &scope(&web)).unwrap();
        assert!(p.data.paths.is_empty() && !p.data.truncated);
        let a = args(p.data.next.as_deref().expect("a call going further"), "kc_path");
        assert_eq!(a["max_hops"], 12);
        let p = crate::walk::path("OrderPage.submit", "OrderPlacedConsumer.Consume", Some(&api), &po(5000, 12), &scope(&web)).unwrap();
        assert!(!p.data.paths.is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }
}
