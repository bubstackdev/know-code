//! Test selection: which test files, and which test methods in them, to run for a change. Three signals, combined
//! into one confidence per test file:
//!
//! - **reverse edges**: test code that depends on the changed symbols, by hop and edge confidence (the `impact`
//!   walk under the caller's evidence policy, kept to test files);
//! - **naming**: `FooTests.cs`, `FooTest.cs`, `foo.spec.ts`, `foo.test.js`, `test_foo.py`, `foo_test.go` for `Foo`;
//! - **co-change**: test files that changed in the same commits as the changed files ([`crate::vitals::coupled`]).
//!
//! The confidence estimates whether the test exercises the change (would fail if the change broke something), not
//! whether it was edited with it: the weights were fitted on fault detection (tests that failed under seeded mutants)
//! and blind labels of the tests reaching a change, on real repos. Co-change is one
//! signal among three: it adds weight, and its absence never pushes a test out. The tight list (`selected`) holds the
//! tests at [`SELECT_AT`] or above, the most likely relevant; every other candidate is `left_out`, ranked, and may
//! still need running. Never selected: a test whose only signal is co-change in fewer than [`MIN_COCHANGE_ALONE`]
//! commits, and a file in a test folder with no tests in it (a base class or fixture: no test-marked symbol and no
//! test name).

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use kc_store::{Store, is_test_path, trace};
use serde::Serialize;

use crate::vitals;

/// Hops the reverse-edge walk follows: the deepest `impact` goes. Tests that failed under seeded mutants sat 4 and 5
/// hops away (a test calling an entry point that calls the changed helper), and the deeper walk costs little.
pub const DEPTH: u8 = 5;
/// Symbols the walk visits at most.
pub const MAX_WALK: usize = 3_000;
/// Co-change partners read per changed file.
const MAX_PARTNERS: usize = 50;
/// The confidence a test needs to be in the tight list. Absolute: a strong candidate (named and co-changed) never
/// raises the bar for the others.
pub const SELECT_AT: f64 = 0.2;
/// Shared commits a test needs when co-change is its only signal: one shared commit is often the commit that added both.
pub const MIN_COCHANGE_ALONE: u32 = 2;

/// Why a test file is a candidate: the raw signals behind its confidence.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Signals {
    /// Fewest hops from a changed symbol to code in the test file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hop: Option<u8>,
    /// The best edge confidence at that hop: `exact`, `unique_name` or `ambiguous`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edge: Option<&'static str>,
    /// Test symbols the walk reached in the file.
    pub reached: usize,
    /// The changed file the test is named for, and whether the name matches whole (`FooTests` for `Foo`) or only
    /// starts with it (`FooRetryTests` for `Foo`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub named_for: Option<String>,
    pub name_exact: bool,
    /// Commits that changed the test with a changed file, and that share of the changed file's commits.
    pub cochange_support: u32,
    pub cochange_confidence: f64,
}

/// A test file to run, with the test methods that reach the change.
#[derive(Debug, Clone, Serialize)]
pub struct TestPick {
    pub file: String,
    /// 0–1: how likely the change needs this test, from the fitted weights.
    pub confidence: f64,
    /// Plain-language reasons, strongest first.
    pub reasons: Vec<String>,
    /// Test methods (qualified names) that depend on the change, nearest first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub methods: Vec<String>,
    /// How many test methods depend on the change, when more than [`METHODS_SHOWN`] are listed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub methods_total: Option<usize>,
    pub signals: Signals,
}

/// At most this many dependent test methods are listed per test file.
pub const METHODS_SHOWN: usize = 20;

#[derive(Debug, Clone, Default, Serialize)]
pub struct Selection {
    /// Tests at or above the selection threshold, most confident first.
    pub selected: Vec<TestPick>,
    /// Weaker candidates, most confident first.
    pub left_out: Vec<TestPick>,
    /// Whether the reverse-edge walk stopped at its cap.
    pub truncated: bool,
}

/// Ranks the test files for a change to the symbols `ids` in the files `changed` (repo-relative; test files among
/// them are selected outright, as they changed themselves).
pub fn select(root: &Path, store: &Store, ids: &[i64], changed: &[String]) -> Result<Selection> {
    select_from(root, store, &trace::impact(store.conn(), ids, DEPTH, MAX_WALK)?, changed)
}

/// [`select`] from a reverse-edge walk the caller made: under its evidence policy, with what a diff removed.
pub fn select_from(root: &Path, store: &Store, impact: &trace::Impact, changed: &[String]) -> Result<Selection> {
    let mut cands: HashMap<String, Cand> = HashMap::new();
    let sources: Vec<&str> = changed.iter().map(String::as_str).filter(|f| !is_test_path(f)).collect();

    // Reverse edges into test code.
    for a in impact.affected.iter().filter(|a| is_test_path(&a.file)) {
        let c = cands.entry(a.file.clone()).or_default();
        c.signals.reached += 1;
        let better = match (c.signals.hop, c.signals.edge) {
            (None, _) => true,
            (Some(h), Some(e)) => a.hop < h || (a.hop == h && edge_rank(a.confidence) < edge_rank(e)),
            _ => false,
        };
        if better {
            c.signals.hop = Some(a.hop);
            c.signals.edge = Some(a.confidence);
            c.via = Some(format!("{} {} {} at hop {}", a.qualified, a.edge, a.via, a.hop));
        }
        if matches!(a.kind.as_str(), "method" | "function") {
            c.methods_seen += 1;
            if c.methods.len() < METHODS_SHOWN {
                c.methods.push(a.qualified.clone());
            }
        }
    }

    // Test files named for a changed file.
    if !sources.is_empty() {
        let subjects: Vec<(&str, String)> = sources.iter().map(|f| (*f, subject(f))).collect();
        let mut stmt = store.conn().prepare("SELECT path FROM files")?;
        let paths: Vec<String> = stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        for p in paths.into_iter().filter(|p| is_test_path(p)) {
            let t = tested_subject(&p);
            // A test folder named for the subject (`Steps/OrderService/…`) names its files for it too.
            let dir = p.rsplit('/').nth(1).map(str::to_ascii_lowercase);
            for (src, s) in &subjects {
                if s.len() < 3 {
                    continue;
                }
                let exact = t.as_deref() == Some(s.as_str()) || dir.as_deref() == Some(s.as_str());
                if exact || (t.as_deref().is_some_and(|t| t.starts_with(s.as_str())) && s.len() >= 4) {
                    let c = cands.entry(p.clone()).or_default();
                    if c.signals.named_for.is_none() || (exact && !c.signals.name_exact) {
                        c.signals.named_for = Some(src.to_string());
                        c.signals.name_exact = exact;
                    }
                }
            }
        }
    }

    // Test files that changed with the changed files.
    for (src, partners) in vitals::coupled(root, store, &sources, MAX_PARTNERS, true)? {
        for p in partners.into_iter().filter(|p| is_test_path(&p.file)) {
            let c = cands.entry(p.file.clone()).or_default();
            if p.confidence > c.signals.cochange_confidence
                || (p.confidence == c.signals.cochange_confidence && p.support > c.signals.cochange_support)
            {
                c.signals.cochange_support = p.support;
                c.signals.cochange_confidence = p.confidence;
                c.cochange = Some(format!("changed with {src} in {} of its {} commits", p.support, p.commits));
            }
        }
    }

    let mut picks: Vec<TestPick> = cands.into_iter().map(|(file, c)| c.pick(file)).collect();
    for f in changed.iter().filter(|f| is_test_path(f)) {
        picks.retain(|p| &p.file != f);
        picks.push(TestPick {
            file: f.clone(),
            confidence: 1.0,
            reasons: vec!["changed itself".into()],
            methods: Vec::new(),
            methods_total: None,
            signals: Signals::default(),
        });
    }
    // A project file, template or SQL script in a test folder (named `….Tests.csproj`, changed with the code) is no
    // test at all.
    picks.retain(|p| holds_code(&p.file));
    // Files with no tests in them run nothing: listed, never selected.
    let mut helpers: Vec<TestPick> = Vec::new();
    for mut p in std::mem::take(&mut picks) {
        if has_tests(store, &p.file)? {
            picks.push(p);
        } else {
            p.reasons.push("no tests in it (a helper or base class)".into());
            helpers.push(p);
        }
    }
    let (selected, mut left_out) = partition(picks);
    left_out.extend(helpers);
    left_out.sort_by(rank);
    Ok(Selection { selected, left_out, truncated: impact.truncated })
}

/// Most confident first, then the nearest reached, then by path.
fn rank(a: &TestPick, b: &TestPick) -> std::cmp::Ordering {
    let hop = |p: &TestPick| p.signals.hop.unwrap_or(u8::MAX);
    b.confidence.partial_cmp(&a.confidence).unwrap().then_with(|| hop(a).cmp(&hop(b))).then_with(|| a.file.cmp(&b.file))
}

/// The tight list (selectable, at [`SELECT_AT`] or above) and the rest, each ranked. Each test is judged on its own
/// signals: nothing about another candidate moves it in or out.
fn partition(mut picks: Vec<TestPick>) -> (Vec<TestPick>, Vec<TestPick>) {
    picks.sort_by(rank);
    picks.into_iter().partition(|p| selectable(p) && p.confidence >= SELECT_AT)
}

/// Whether `path` is in a language tests are written in (Swift, Python, Rust, C#, TypeScript, JavaScript, CFML).
fn holds_code(path: &str) -> bool {
    use kc_extract::Lang;
    matches!(
        Lang::from_path(path),
        Some(Lang::Swift | Lang::Python | Lang::Rust | Lang::CSharp | Lang::TypeScript | Lang::Tsx | Lang::JavaScript | Lang::Cfml)
    )
}

/// Whether a candidate may be selected: not on co-change alone in fewer than [`MIN_COCHANGE_ALONE`] commits.
fn selectable(p: &TestPick) -> bool {
    let s = &p.signals;
    let cochange_only = s.hop.is_none() && s.named_for.is_none() && s.cochange_support > 0;
    !(cochange_only && s.cochange_support < MIN_COCHANGE_ALONE)
}

/// Whether the test file at `path` holds tests: a test name ([`tested_subject`]: `FooTests.cs`, `foo.spec.ts`), or
/// a symbol the extractor marks as a test (`[Fact]`, `[Test]`, `[TestClass]`, `def test_x`, `#[test]`), or a BDD step
/// binding (`[Binding]`, `[Given]`, `[When]`, `[Then]`: steps run by feature files, often split over partial classes). Only C#,
/// Python and Rust mark tests; files in other languages are taken to hold tests.
fn has_tests(store: &Store, path: &str) -> Result<bool> {
    let marks_tests = matches!(path.rsplit_once('.').map(|(_, e)| e), Some("cs" | "py" | "rs"));
    if !marks_tests || tested_subject(path).is_some() {
        return Ok(true);
    }
    let mut stmt = store.conn().prepare_cached(
        "SELECT 1 FROM symbols s JOIN files f ON f.id = s.file_id WHERE f.path = ?1 AND ((',' || s.tags || ',') LIKE '%,test,%'
             OR (',' || s.attrs || ',') LIKE '%,Binding,%' OR (',' || s.attrs || ',') LIKE '%,Given,%'
             OR (',' || s.attrs || ',') LIKE '%,When,%' OR (',' || s.attrs || ',') LIKE '%,Then,%') LIMIT 1",
    )?;
    let tagged = stmt.exists([path])?;
    // A file the index doesn't know (deleted, or a language it doesn't read) can't be judged: keep it.
    Ok(tagged || !store.conn().prepare_cached("SELECT 1 FROM files WHERE path = ?1")?.exists([path])?)
}

#[derive(Default)]
struct Cand {
    signals: Signals,
    via: Option<String>,
    cochange: Option<String>,
    methods: Vec<String>,
    methods_seen: usize,
}

impl Cand {
    fn pick(self, file: String) -> TestPick {
        let s = &self.signals;
        let mut reasons = Vec::new();
        reasons.extend(self.via.clone());
        if let Some(src) = &s.named_for {
            reasons.push(format!("named for {src}{}", if s.name_exact { "" } else { " (name prefix)" }));
        }
        reasons.extend(self.cochange.clone());
        let methods_total = (self.methods_seen > self.methods.len()).then_some(self.methods_seen);
        TestPick { file, confidence: confidence(s), reasons, methods: self.methods, methods_total, signals: self.signals }
    }
}

fn edge_rank(confidence: &str) -> u8 {
    match confidence {
        "exact" => 0,
        "unique_name" => 1,
        _ => 2,
    }
}

/// The model's features, in [`WEIGHTS`] order: whether the walk reached the test by a confident edge (`exact`,
/// `unique_name`) or only an `ambiguous` one; how many test symbols it reached (log); whether it is named for a changed
/// file (exactly or by prefix); co-change: any, and its confidence. The hop doesn't count: on the fault-detection
/// labels a test three hops away failed as often as a direct caller, once reached confidently.
pub fn features(s: &Signals) -> [f64; 6] {
    let confident = s.edge.map(edge_rank).is_some_and(|r| r < 2);
    let reached = s.hop.is_some();
    [
        f64::from(u8::from(reached && confident)),
        f64::from(u8::from(reached && !confident)),
        (s.reached as f64).ln_1p(),
        f64::from(u8::from(s.named_for.is_some())),
        f64::from(u8::from(s.cochange_support > 0)),
        if s.cochange_support > 0 { s.cochange_confidence } else { 0.0 },
    ]
}

/// Intercept and weights of the logistic model over [`features`] (see [`confidence`]). Co-change weighs little: on
/// the new labels a test that changed with the code was hardly likelier to exercise it than one that didn't.
const BIAS: f64 = -2.35;
const WEIGHTS: [f64; 6] = [0.74, -0.06, 0.74, 2.15, 0.10, 0.30];

/// How likely a test file with these signals exercises the change: a logistic model over [`features`], fitted on
/// tests that failed under seeded mutants and blind labels of the tests reaching a change.
pub fn confidence(s: &Signals) -> f64 {
    let z = BIAS + features(s).iter().zip(WEIGHTS).map(|(x, w)| x * w).sum::<f64>();
    (1.0 / (1.0 + (-z).exp()) * 1000.0).round() / 1000.0
}

/// A source file's name as tests name it: lowercased, without its last extension (`OrderService.cs` →
/// `orderservice`, `order.service.ts` → `order.service`).
pub fn subject(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
    stem.to_ascii_lowercase()
}

/// What a test file is named for, in [`subject`]'s form: `OrderServiceTests.cs`, `OrderServiceTest.cs`,
/// `OrderServiceSpec.cs`, and BDD step files `OrderServiceSteps.Given.cs` → `orderservice`; `order.service.spec.ts`,
/// `order.service.test.ts` → `order.service`; `test_orders.py`, `orders_test.py`, `orders_tests.py` → `orders`.
/// `None` when the name has no test marker (a helper in a test folder).
pub fn tested_subject(path: &str) -> Option<String> {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    for marker in [".spec.", ".test.", ".tests."] {
        if let Some(i) = name.find(marker) {
            return Some(name[..i].to_string());
        }
    }
    // Up to the first dot: partial classes split one test over `FooSteps.Given.cs` and `FooSteps.Then.cs`.
    let stem = name.split('.').next().unwrap_or(&name);
    if let Some(s) = stem.strip_prefix("test_") {
        return Some(s.to_string());
    }
    for suffix in ["_tests", "_test", "tests", "test", "specs", "spec", "steps"] {
        if let Some(s) = stem.strip_suffix(suffix)
            && !s.is_empty()
        {
            return Some(s.trim_end_matches(['_', '.', '-']).to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_names_map_to_their_subject() {
        assert_eq!(tested_subject("tests/Orders/OrderServiceTests.cs").as_deref(), Some("orderservice"));
        assert_eq!(tested_subject("OrderServiceTest.cs").as_deref(), Some("orderservice"));
        assert_eq!(tested_subject("src/app/order.service.spec.ts").as_deref(), Some("order.service"));
        assert_eq!(tested_subject("src/order.test.js").as_deref(), Some("order"));
        assert_eq!(tested_subject("tests/test_orders.py").as_deref(), Some("orders"));
        assert_eq!(tested_subject("orders_test.py").as_deref(), Some("orders"));
        assert_eq!(tested_subject("tests/Steps/Orders/OrderServiceSteps.Given.cs").as_deref(), Some("orderservice"));
        assert_eq!(tested_subject("Tests/Fixtures/OrderBuilder.cs"), None);
        assert_eq!(subject("src/Services/OrderService.cs"), "orderservice");
        assert_eq!(subject("src/app/order.service.ts"), "order.service");
        assert!(holds_code("tests/Orders/OrderServiceTests.cs") && holds_code("src/app/order.service.spec.ts"));
        assert!(!holds_code("tests/Orders.Tests/Orders.Tests.csproj") && !holds_code("tests/Features/Orders.feature"));
    }

    #[test]
    fn confidence_rises_with_each_signal() {
        let none = Signals::default();
        let named = Signals { named_for: Some("a.cs".into()), name_exact: true, ..Default::default() };
        let named_and_called = Signals { hop: Some(1), edge: Some("exact"), reached: 1, ..named.clone() };
        assert!(confidence(&none) < confidence(&named));
        assert!(confidence(&named) < confidence(&named_and_called));
        let far = Signals { hop: Some(3), edge: Some("ambiguous"), reached: 1, ..Default::default() };
        assert!(confidence(&far) < SELECT_AT, "an ambiguous edge three hops away alone isn't enough");
    }

    #[test]
    fn co_change_alone_needs_two_commits() {
        let pick = |s: Signals| TestPick { file: "t.cs".into(), confidence: confidence(&s), reasons: Vec::new(), methods: Vec::new(), methods_total: None, signals: s };
        let once = Signals { cochange_support: 1, cochange_confidence: 1.0, ..Default::default() };
        let twice = Signals { cochange_support: 2, ..once.clone() };
        let once_and_named = Signals { named_for: Some("a.cs".into()), ..once.clone() };
        assert!(!selectable(&pick(once)));
        assert!(selectable(&pick(twice)));
        assert!(selectable(&pick(once_and_named)));
    }

    /// Every combination of signals the walk, naming and history can give.
    fn all_signals() -> Vec<Signals> {
        let mut out = Vec::new();
        let ways = [(None, None), (Some(1), Some("exact")), (Some(2), Some("unique_name")), (Some(5), Some("exact")), (Some(1), Some("ambiguous")), (Some(4), Some("ambiguous"))];
        for (hop, edge) in ways {
            for reached in [0, 1, 3, 40] {
                if hop.is_some() != (reached > 0) {
                    continue;
                }
                for named in [None, Some("src/A.cs".to_string())] {
                    for (support, conf) in [(0, 0.0), (1, 0.1), (2, 0.5), (9, 1.0)] {
                        out.push(Signals {
                            hop,
                            edge,
                            reached,
                            name_exact: named.is_some(),
                            named_for: named.clone(),
                            cochange_support: support,
                            cochange_confidence: conf,
                        });
                    }
                }
            }
        }
        out
    }

    fn pick_of(file: &str, s: Signals) -> TestPick {
        TestPick { file: file.into(), confidence: confidence(&s), reasons: Vec::new(), methods: Vec::new(), methods_total: None, signals: s }
    }

    /// Co-change is one signal, never the gate: history only ever adds weight, and a test reached by a confident edge
    /// or named for the change is in the tight list with no history at all.
    #[test]
    fn co_change_adds_weight_and_never_excludes() {
        for s in all_signals() {
            let without = Signals { cochange_support: 0, cochange_confidence: 0.0, ..s.clone() };
            assert!(confidence(&s) >= confidence(&without), "{s:?}");
            let confident = s.hop.is_some() && s.edge != Some("ambiguous");
            if confident || s.named_for.is_some() {
                let (tight, _) = partition(vec![pick_of("t.cs", without.clone())]);
                assert_eq!(tight.len(), 1, "{without:?} scores {}", confidence(&without));
            }
        }
    }

    /// Each test is judged on its own signals: adding a candidate, however strong, never moves another out of the
    /// tight list (the old rule raised the bar to half the best candidate's confidence).
    #[test]
    fn a_strong_candidate_never_pushes_another_out() {
        let strong =
            Signals { hop: Some(1), edge: Some("exact"), reached: 40, named_for: Some("src/A.cs".into()), name_exact: true, cochange_support: 9, cochange_confidence: 1.0 };
        for s in all_signals() {
            let (alone, _) = partition(vec![pick_of("b.cs", s.clone())]);
            let (with, _) = partition(vec![pick_of("a.cs", strong.clone()), pick_of("b.cs", s.clone())]);
            assert_eq!(alone.len(), with.iter().filter(|p| p.file == "b.cs").count(), "{s:?}");
        }
    }
}
