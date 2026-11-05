//! Completeness of traversals and limits: a walk lists everything within the bounds asked for, a better way to a
//! symbol never makes it weaker, a filter never loses a match to a cap applied before it, and a cap always says so.
//! Each case is a small synthetic repo; the invariants run over seeded random call graphs.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};

use crate::ops::tests::{repo, test_cache};
use crate::ops::{self, ImpactParams, Scope, WalkFilter};

fn scope(root: &Path) -> Scope {
    Scope { repos: vec![root.to_path_buf()], no_sync: false }
}

/// `(qualified, depth)` of each node of a trace.
fn traced(root: &Path, symbol: &str, direction: &str, depth: u8) -> (Vec<(String, u8)>, bool) {
    let out = ops::trace(symbol, direction, depth, 1000, &WalkFilter::default(), &scope(root)).unwrap();
    let t = &out.data.found[0].1;
    (t.nodes.iter().map(|n| (n.qualified.clone(), n.depth)).collect(), t.truncated)
}

/// Each affected symbol of an impact, by name: hop, confidence, and the hop its class goes by (the nearer of its
/// hop and its nearest ambiguous one).
fn impacted(root: &Path, symbol: &str, depth: u8) -> BTreeMap<String, (u8, &'static str, u8)> {
    let p = ImpactParams { symbol: Some(symbol.into()), depth, limit: 1000, ..Default::default() };
    let out = ops::impact(&p, &scope(root)).unwrap();
    let mut rows = BTreeMap::new();
    let r = &out.data[0];
    for a in &r.impact.affected {
        let severity = a.nearest_ambiguous_hop.unwrap_or(a.hop);
        assert_eq!(a.class, kc_store::trace::class_of(severity, &a.file), "{}", a.qualified);
        assert!(rows.insert(a.qualified.clone(), (a.hop, a.confidence, severity)).is_none(), "{} listed twice", a.qualified);
    }
    // Proximity is graph distance and nothing else: `nearest` is the class of the nearest non-test dependent, and the
    // deprecated `worst` the same hop in the old names.
    let nearest = r.impact.affected.iter().filter(|a| a.class != "test").map(|a| a.nearest_ambiguous_hop.unwrap_or(a.hop)).min();
    if r.cross_repo.is_empty() {
        assert_eq!(r.nearest(), nearest.map_or("none", |h| kc_store::trace::class_of(h, "x.py")));
        assert_eq!(r.legacy_worst(), nearest.map_or("none", kc_store::trace::legacy_class));
    }
    rows
}

#[test]
fn a_trace_lists_what_a_symbol_seen_deeper_first_leads_to_within_the_depth() {
    let cache = test_cache();
    let base = cache.dir("c-trace-depth");
    let root = repo(
        &base,
        "app",
        &[(
            "main.py",
            "def root():\n    first()\n    shared()\ndef first():\n    second()\ndef second():\n    shared()\ndef shared():\n    target()\ndef target():\n    pass\n",
        )],
    );
    let (nodes, truncated) = traced(&root, "root", "out", 3);
    // `shared` is met first at depth 3 (under `second`), then at depth 1, where `target` is within reach.
    assert!(nodes.contains(&("target".into(), 2)), "{nodes:?}");
    assert!(!truncated);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn impact_lists_a_dependent_by_its_exact_edge_though_an_ambiguous_one_came_first() {
    let cache = test_cache();
    let base = cache.dir("c-impact-confidence");
    let root = repo(
        &base,
        "app",
        &[
            ("changed.py", "class FirstStore:\n    def save(self): pass\nclass StrongStore:\n    def commit(self): pass\n"),
            ("other.py", "class OtherStore:\n    def save(self): pass\n"),
            (
                "caller.py",
                "from changed import StrongStore\ndef bridge(unknown, strong: StrongStore):\n    unknown.save()\n    strong.commit()\ndef top(unknown, strong):\n    bridge(unknown, strong)\n",
            ),
        ],
    );
    let p = ImpactParams { file: Some("changed.py".into()), depth: 3, limit: 100, ..Default::default() };
    let out = ops::impact(&p, &scope(&root)).unwrap();
    let rows: Vec<(String, u8, &str)> = out.data[0].impact.affected.iter().map(|a| (a.qualified.clone(), a.hop, a.confidence)).collect();
    // `bridge` is exact through `StrongStore.commit`, so it is grown: `top` depends on the change too.
    assert!(rows.contains(&("bridge".into(), 1, "exact")), "{rows:?}");
    assert!(rows.iter().any(|(q, h, _)| q == "top" && *h == 2), "{rows:?}");
    assert!(rows.iter().all(|(_, _, c)| *c != "ambiguous"), "{rows:?}");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_dependent_listed_by_a_farther_exact_way_keeps_the_class_of_a_nearer_ambiguous_one() {
    let cache = test_cache();
    let base = cache.dir("c-impact-severity");
    let root = repo(
        &base,
        "app",
        &[
            ("changed.py", "class FirstStore:\n    def save(self): pass\n"),
            ("other.py", "class OtherStore:\n    def save(self): pass\n"),
            ("mid.py", "from changed import FirstStore\nclass Mid:\n    def run(self, f: FirstStore):\n        f.save()\n"),
            ("caller.py", "from mid import Mid\ndef near(unknown, m: Mid):\n    unknown.save()\n    m.run()\n"),
        ],
    );
    let rows = impacted(&root, "FirstStore.save", 3);
    // `near` calls the change perhaps (hop 1, ambiguous) and surely through `Mid.run` (hop 2): exact, direct.
    assert_eq!(rows.get("near"), Some(&(2, "exact", 1)), "{rows:?}");
    let p = ImpactParams { symbol: Some("FirstStore.save".into()), depth: 3, limit: 100, ..Default::default() };
    let out = ops::impact(&p, &scope(&root)).unwrap();
    let text = crate::render::impact_text(&out.data, false);
    assert!(text.contains("(ambiguous at hop 1)"), "{text}");
    let _ = std::fs::remove_dir_all(&base);
}

/// A seeded generator (64-bit LCG): the invariants run on the same graphs every time.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Random call graph over `n` nodes: `(caller, callee, strong)`, no self calls, no duplicates.
fn graph(rng: &mut Rng, n: usize, edges: usize) -> Vec<(usize, usize, bool)> {
    let mut out: Vec<(usize, usize, bool)> = Vec::new();
    while out.len() < edges {
        let (a, b) = (rng.below(n as u64) as usize, rng.below(n as u64) as usize);
        if a != b && !out.iter().any(|e| (e.0, e.1) == (a, b)) {
            out.push((a, b, rng.below(2) == 0));
        }
    }
    out
}

/// Python functions `f0`… calling one another as `edges` say (every edge unique_name).
fn functions(n: usize, edges: &[(usize, usize, bool)]) -> String {
    let mut src = String::new();
    for u in 0..n {
        src.push_str(&format!("def f{u}():\n    pass\n"));
        for (_, v, _) in edges.iter().filter(|e| e.0 == u) {
            src.push_str(&format!("    f{v}()\n"));
        }
    }
    src
}

/// Nodes within `depth` steps of `from` following `edges` forward (`out`) or backward, with their distance.
fn within(n: usize, edges: &[(usize, usize, bool)], from: usize, depth: u8, out: bool) -> BTreeMap<usize, u8> {
    let mut dist = BTreeMap::from([(from, 0u8)]);
    let mut queue = VecDeque::from([from]);
    while let Some(u) = queue.pop_front() {
        let d = dist[&u];
        if d == depth {
            continue;
        }
        for &(a, b, _) in edges {
            let (x, y) = if out { (a, b) } else { (b, a) };
            if x == u && y < n && !dist.contains_key(&y) {
                dist.insert(y, d + 1);
                queue.push_back(y);
            }
        }
    }
    dist
}

#[test]
fn invariant_a_trace_holds_every_symbol_within_its_depth_and_an_added_edge_never_removes_one() {
    let cache = test_cache();
    let base = cache.dir("c-trace-random");
    let mut rng = Rng(7);
    for case in 0..6 {
        let n = 9;
        let mut edges = graph(&mut rng, n, 14);
        let root = repo(&base, &format!("g{case}"), &[("main.py", &functions(n, &edges))]);
        let mut before: HashMap<(&str, u8), BTreeSet<String>> = HashMap::new();
        for (dir, depth) in [("out", 2), ("out", 3), ("in", 3)] {
            let (nodes, truncated) = traced(&root, "f0", dir, depth);
            assert!(!truncated);
            let got: BTreeSet<String> = nodes.iter().map(|(q, _)| q.clone()).collect();
            let want: BTreeSet<String> = within(n, &edges, 0, depth, dir == "out").keys().map(|v| format!("f{v}")).collect();
            assert_eq!(got, want, "case {case} {dir} depth {depth}: {edges:?}");
            // And each symbol's shallowest node is at its distance.
            let dist = within(n, &edges, 0, depth, dir == "out");
            for (v, d) in dist {
                let shallowest = nodes.iter().filter(|(q, _)| *q == format!("f{v}")).map(|(_, d)| *d).min();
                assert_eq!(shallowest, Some(d), "case {case} {dir}: f{v}");
            }
            before.insert((dir, depth), got);
        }
        // One more call: everything listed before still is.
        loop {
            let (a, b) = (rng.below(n as u64) as usize, rng.below(n as u64) as usize);
            if a != b && !edges.iter().any(|e| (e.0, e.1) == (a, b)) {
                edges.push((a, b, true));
                break;
            }
        }
        std::fs::write(root.join("main.py"), functions(n, &edges)).unwrap();
        for ((dir, depth), was) in &before {
            let (nodes, _) = traced(&root, "f0", dir, *depth);
            let now: BTreeSet<String> = nodes.into_iter().map(|(q, _)| q).collect();
            assert!(now.is_superset(was), "case {case} {dir} {depth}: lost {:?}", was.difference(&now).collect::<Vec<_>>());
        }
    }
    let _ = std::fs::remove_dir_all(&base);
}

/// Python methods `T{v}.go{v}`, each with a twin `W{v}.go{v}`, calling one another: a strong edge through a typed
/// parameter (`exact`), a weak one through an untyped one (`ambiguous`: both `go{v}` match).
fn methods(n: usize, edges: &[(usize, usize, bool)]) -> String {
    let mut src = String::new();
    for u in 0..n {
        let out: Vec<_> = edges.iter().filter(|e| e.0 == u).collect();
        let params: String = out.iter().filter(|e| e.2).map(|e| format!(", t{}: T{}", e.1, e.1)).collect();
        src.push_str(&format!("class T{u}:\n    def go{u}(self, x{params}):\n        pass\n"));
        for (_, v, strong) in out {
            if *strong {
                src.push_str(&format!("        t{v}.go{v}()\n"));
            } else {
                src.push_str(&format!("        x.go{v}()\n"));
            }
        }
        src.push_str(&format!("class W{u}:\n    def go{u}(self):\n        pass\n"));
    }
    src
}

/// The impact the edges imply from `T{root}.go{root}`: what reaches it through confident edges, at its nearest hop,
/// confident; else what reaches a confidently reached symbol (or the root) through a weak edge, at its nearest hop,
/// ambiguous. The class goes by the nearer of the row's hop and the nearest weak way. `(hop, confident, severity
/// hop)` by node.
fn expected_impact(n: usize, edges: &[(usize, usize, bool)], root: usize, depth: u8) -> BTreeMap<usize, (u8, bool, u8)> {
    let strong: Vec<_> = edges.iter().copied().filter(|e| e.2).collect();
    let sure = within(n, &strong, root, depth, false);
    // The nearest weak way to each node, from the root or a confidently reached node.
    let mut weak: BTreeMap<usize, u8> = BTreeMap::new();
    for &(a, b, s) in edges {
        if let (false, Some(&d)) = (s, sure.get(&b))
            && d < depth
            && a != root
        {
            let e = weak.entry(a).or_insert(d + 1);
            *e = (*e).min(d + 1);
        }
    }
    let mut out = BTreeMap::new();
    for v in (0..n).filter(|&v| v != root) {
        match (sure.get(&v), weak.get(&v)) {
            (Some(&d), w) => out.insert(v, (d, true, w.map_or(d, |&w| w.min(d)))),
            (None, Some(&w)) => out.insert(v, (w, false, w)),
            (None, None) => None,
        };
    }
    out
}

#[test]
fn invariant_impact_lists_each_dependent_by_its_best_way_and_an_added_edge_never_weakens_or_lowers_one() {
    let cache = test_cache();
    let base = cache.dir("c-impact-random");
    let mut rng = Rng(11);
    // How good a row is: lower is better (confident first, then nearer).
    let rank = |(h, c, _): (u8, &str, u8)| (!matches!(c, "exact" | "unique_name"), h);
    for case in 0..10 {
        let n = 8;
        let mut edges = graph(&mut rng, n, 16);
        let root = repo(&base, &format!("m{case}"), &[("main.py", &methods(n, &edges))]);
        let got = impacted(&root, "T0.go0", 3);
        let want = expected_impact(n, &edges, 0, 3);
        let got_rows: BTreeMap<String, (u8, bool, u8)> =
            got.iter().map(|(q, (h, c, s))| (q.clone(), (*h, matches!(*c, "exact" | "unique_name"), *s))).collect();
        let want_rows: BTreeMap<String, (u8, bool, u8)> = want.iter().map(|(v, r)| (format!("T{v}.go{v}"), *r)).collect();
        assert_eq!(got_rows, want_rows, "case {case}: {edges:?}");
        // One more call, strong or weak: no dependent disappears, gets a worse row, or a lower severity.
        loop {
            let (a, b) = (rng.below(n as u64) as usize, rng.below(n as u64) as usize);
            if a != b && !edges.iter().any(|e| (e.0, e.1) == (a, b)) {
                edges.push((a, b, rng.below(2) == 0));
                break;
            }
        }
        std::fs::write(root.join("main.py"), methods(n, &edges)).unwrap();
        let after = impacted(&root, "T0.go0", 3);
        for (q, row) in &got {
            let now = after.get(q).unwrap_or_else(|| panic!("case {case}: {q} gone after adding {:?}", edges.last()));
            assert!(rank(*now) <= rank(*row), "case {case}: {q} went from {row:?} to {now:?}");
            assert!(now.2 <= row.2, "case {case}: {q}'s severity went from hop {} to {}", row.2, now.2);
        }
    }
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn invariant_stronger_evidence_for_an_edge_never_removes_or_weakens_a_dependent() {
    let cache = test_cache();
    let base = cache.dir("c-strengthen-random");
    let mut rng = Rng(23);
    let rank = |(h, c, _): (u8, &str, u8)| (!matches!(c, "exact" | "unique_name"), h);
    let mut strengthened = 0;
    for case in 0..10 {
        let n = 8;
        let mut edges = graph(&mut rng, n, 16);
        let root = repo(&base, &format!("s{case}"), &[("main.py", &methods(n, &edges))]);
        let before = impacted(&root, "T0.go0", 3);
        let (traced_before, _) = traced(&root, "T0.go0", "in", 3);
        // The same call, now through a typed parameter: exact where it was ambiguous.
        let weak: Vec<usize> = (0..edges.len()).filter(|&i| !edges[i].2).collect();
        let Some(&i) = weak.get(rng.below(weak.len().max(1) as u64) as usize) else { continue };
        edges[i].2 = true;
        strengthened += 1;
        std::fs::write(root.join("main.py"), methods(n, &edges)).unwrap();
        let after = impacted(&root, "T0.go0", 3);
        for (q, row) in &before {
            let now = after.get(q).unwrap_or_else(|| panic!("case {case}: {q} gone after strengthening {:?}", edges[i]));
            assert!(rank(*now) <= rank(*row), "case {case}: {q} went from {row:?} to {now:?}");
            assert!(now.2 <= row.2, "case {case}: {q}'s severity went from hop {} to {}", row.2, now.2);
        }
        let (traced_after, _) = traced(&root, "T0.go0", "in", 3);
        let (was, now): (BTreeSet<String>, BTreeSet<String>) =
            (traced_before.into_iter().map(|t| t.0).collect(), traced_after.into_iter().map(|t| t.0).collect());
        assert!(now.is_superset(&was), "case {case}: the trace lost {:?}", was.difference(&now).collect::<Vec<_>>());
    }
    assert!(strengthened >= 8, "only {strengthened} cases had an ambiguous edge to strengthen");
    let _ = std::fs::remove_dir_all(&base);
}

/// `a` reaches `z` in three hops (`a → m1 → m2 → z`) only through ambiguous calls (`m1` and `z` are defined twice),
/// while the cheapest way from `a` to `m1`, and from `m2` to `z`, takes three confident hops (each a call to a
/// definition in the caller's own file): a search keeping one label per symbol, its cheapest, finds no path within
/// three hops either way.
fn hop_limited(base: &Path) -> PathBuf {
    repo(
        base,
        "app",
        &[
            ("main.py", "def a():\n    m1()\n    x1()\ndef x1():\n    x2()\n"),
            ("b.py", "def x2():\n    m1()\ndef m1():\n    m2()\n"),
            ("c.py", "def m1():\n    pass\n"),
            ("d.py", "def m2():\n    z()\n    y1()\ndef y1():\n    y2()\n"),
            ("e.py", "def y2():\n    z()\ndef z():\n    pass\n"),
            ("f.py", "def z():\n    pass\n"),
        ],
    )
}

#[test]
fn a_path_within_the_hop_limit_is_found_though_a_cheaper_way_to_its_middle_is_longer() {
    let cache = test_cache();
    let base = cache.dir("c-path-hops");
    let root = hop_limited(&base);
    for one_way in [false, true] {
        let opts = crate::walk::PathOptions {
            max_hops: 3,
            max_nodes: 2000,
            max_repos: 4,
            paths: 3,
            one_way,
            allow_ambiguous: true,
            edges: Default::default(),
            from_skip: 0,
            to_skip: 0,
        };
        let a = crate::walk::path("a", "z", None, &opts, &scope(&root)).unwrap().data;
        let steps: Vec<Vec<String>> = a.paths.iter().map(|p| p.steps.iter().map(|s| s.qualified.clone()).collect()).collect();
        assert!(steps.contains(&vec!["a".into(), "m1".into(), "m2".into(), "z".into()]), "one_way {one_way}: {steps:?}");
        assert!(a.paths.iter().all(|p| p.hops <= 3));
    }
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn find_filters_by_container_before_its_candidate_cap_and_says_when_the_cap_cut() {
    let cache = test_cache();
    let base = cache.dir("c-find-cap");
    let n = kc_store::query::FIND_CANDIDATES + 5;
    let src: String = (0..n).map(|i| format!("public class Type{i:04} {{ public void Save() {{}} }}\n")).collect();
    let root = repo(&base, "app", &[("types.cs", &src)]);
    let last = format!("Type{:04}.Save", n - 1);
    let out = ops::find(&last, None, 10, &scope(&root)).unwrap();
    assert_eq!(out.data.items.iter().map(|(_, h)| h.symbol.qualified()).collect::<Vec<_>>(), [last]);
    assert!(!out.data.capped && out.env["truncated"] == false);
    // Every `Save`: more than the cap, so the total is a lower bound and says so.
    let all = ops::find("Save", Some("method"), 10, &scope(&root)).unwrap();
    assert!(all.data.capped, "{}", all.data.total);
    assert_eq!(all.env["truncated"], true);
    assert_eq!(all.env["capped"], true);
    assert!(all.env["hint"].as_str().unwrap().contains("Narrow it"));
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_flow_follows_through_a_helper_it_hides() {
    let cache = test_cache();
    let base = cache.dir("c-flow-helper");
    let root = repo(
        &base,
        "app",
        &[(
            "a.cs",
            "public class Sink { public void Write() {} }\npublic class OrdersHelper { public void Save() { new Sink().Write(); } }\npublic class Controller { public void Run() { new OrdersHelper().Save(); } }\n",
        )],
    );
    let opts = crate::flow::FlowOptions { depth: 10, max_steps: 80, max_repos: 4, edges: Default::default(), children: crate::flow::DEFAULT_CHILDREN };
    let f = crate::flow::flow("Controller.Run", &opts, &scope(&root)).unwrap().data.unwrap();
    let steps: Vec<_> = f.steps.iter().map(|s| (s.step.qualified.as_str(), s.parent, s.step.through.clone())).collect();
    assert_eq!(steps, [("Sink.Write", None, vec!["OrdersHelper.Save (utility)".to_string()])]);
    assert_eq!(f.hidden.get("utility"), Some(&1));
    assert!(f.left_out.is_empty(), "{:?}", f.left_out);
    let text = crate::render::flow_text("Controller.Run", Some(&f));
    assert!(text.contains("(through OrdersHelper.Save (utility))") && text.contains("hidden, followed through: 1 utility"), "{text}");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_flow_step_cut_at_the_depth_limit_is_grown_where_it_is_reached_nearer() {
    let cache = test_cache();
    let base = cache.dir("c-flow-depth");
    // `Run` calls `First.Go` (→ `Second.Go` → `Shared.Go`) before `Shared.Go` itself, which calls `Target.Go`.
    let root = repo(
        &base,
        "app",
        &[(
            "a.cs",
            "public class Target { public void Go() {} }\n\
             public class Shared { public void Go() { new Target().Go(); } }\n\
             public class Second { public void Go() { new Shared().Go(); } }\n\
             public class First { public void Go() { new Second().Go(); } }\n\
             public class Controller {\n    public void Run() {\n        new First().Go();\n        new Shared().Go();\n    }\n}\n",
        )],
    );
    let opts = crate::flow::FlowOptions { depth: 3, max_steps: 80, max_repos: 4, edges: Default::default(), children: crate::flow::DEFAULT_CHILDREN };
    let f = crate::flow::flow("Controller.Run", &opts, &scope(&root)).unwrap().data.unwrap();
    let steps: Vec<_> = f.steps.iter().map(|s| (s.step.qualified.as_str(), s.depth)).collect();
    assert!(steps.contains(&("Target.Go", 1)), "{steps:?}");
    assert!(steps.contains(&("Shared.Go", 2)) && steps.contains(&("Shared.Go", 0)), "{steps:?}");
    let _ = std::fs::remove_dir_all(&base);
}

/// A walk from several symbols, cut at its limit, keeps the same dependents and lists them alike whatever order the
/// symbols come in. They come in row-id order, which follows the order files were stored in and differs between two
/// indexes of one tree (#230: two cold indexes of swift-nio selected 127 and 136 tests).
#[test]
fn invariant_a_walk_cut_at_its_limit_doesnt_depend_on_the_order_of_its_start() {
    let cache = test_cache();
    let base = cache.dir("c-walk-start-order");
    let mut files: Vec<(String, String)> = vec![
        ("lib/alpha.py".into(), "def alpha():\n    pass\n".into()),
        ("lib/beta.py".into(), "def beta():\n    pass\n".into()),
    ];
    for i in 0..6 {
        files.push((format!("app/a{i}.py"), format!("from lib.alpha import alpha\ndef a{i}():\n    alpha()\n")));
        files.push((format!("app/b{i}.py"), format!("from lib.beta import beta\ndef b{i}():\n    beta()\n")));
    }
    let all: Vec<(&str, &str)> = files.iter().map(|(p, t)| (p.as_str(), t.as_str())).collect();
    let root = repo(&base, "app", &all);
    let o = ops::open_fresh(&root, false).unwrap();
    let mut g = crate::walk::Graph::new(&o, &[], false, 1);
    let alpha = g.resolve(0, "alpha").unwrap().unwrap().id;
    let beta = g.resolve(0, "beta").unwrap().unwrap().id;
    let mut walk = |start: &[i64]| -> Vec<(String, u8, String)> {
        let out = crate::tree::impact(&mut g, start, 2, 5).unwrap();
        assert!(out.truncated, "the limit cuts the walk");
        out.affected.iter().map(|a| (a.qualified.clone(), a.hop, a.via.clone())).collect()
    };
    assert_eq!(walk(&[alpha, beta]), walk(&[beta, alpha]));
    drop(g);
    let _ = std::fs::remove_dir_all(&base);
}

/// The same tree indexed with its files stored in path order and in reverse: a `tests` answer whose walk is cut at
/// its limit (3,000 dependents), started from two files, is the same (#230, where two cold indexes of swift-nio
/// selected 127 and 136 tests).
#[test]
fn invariant_capped_tests_dont_depend_on_the_order_files_were_stored_in() {
    let cache = test_cache();
    let base = cache.dir("c-capped-storage-order");
    let indexed = |name: &str, insert_reversed: bool| -> PathBuf {
        let dir = base.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(std::process::Command::new("git").arg("init").arg("-q").arg(&dir).status().unwrap().success());
        let mut files: Vec<(String, String)> =
            vec![("lib/alpha.py".into(), "def alpha():\n    pass\n".into()), ("lib/beta.py".into(), "def beta():\n    pass\n".into())];
        // 64 test files of 100 tests each, half reaching `alpha` and half `beta`: more dependents than the walk keeps.
        for f in 0..32 {
            for (lib, name) in [("alpha", "a"), ("beta", "b")] {
                let mut text = format!("from lib.{lib} import {lib}\n");
                for t in 0..100 {
                    text.push_str(&format!("def test_{name}{f}_{t}():\n    {lib}()\n"));
                }
                files.push((format!("tests/test_{name}{f:02}.py"), text));
            }
        }
        for (path, text) in &files {
            std::fs::create_dir_all(dir.join(path).parent().unwrap()).unwrap();
            std::fs::write(dir.join(path), text).unwrap();
        }
        let root = kc_index::repo_root(&dir).unwrap();
        let opts = kc_index::SyncOptions { threads: 1, insert_reversed, ..Default::default() };
        kc_index::sync(&root, &mut kc_index::open(&root).unwrap(), &opts).unwrap();
        root
    };
    // Each test file listed, in order, with its reasons and how many of its methods reach the change.
    let selected = |root: &Path| -> Vec<(String, Vec<String>, usize)> {
        let t = ops::TestsParams { targets: vec!["lib/alpha.py".into(), "lib/beta.py".into()], diff: None, limit: 3, evidence: String::new() };
        let out = ops::tests(&t, &scope(root)).unwrap();
        let s = &out.data[0].selection;
        assert!(s.truncated, "the limit cuts the walk");
        s.selected.iter().chain(&s.left_out).map(|p| (p.file.clone(), p.reasons.clone(), p.signals.reached)).collect()
    };
    assert_eq!(selected(&indexed("forward", false)), selected(&indexed("reversed", true)));
    let _ = std::fs::remove_dir_all(&base);
}
