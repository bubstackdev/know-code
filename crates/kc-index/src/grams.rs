//! Turns a regular expression into a trigram query: a boolean formula over trigrams that every matching file must
//! satisfy. Files that fail it are never read.
//!
//! The analysis walks the parsed expression and tracks, per node, either the finite set of strings it can match
//! (`abc`, `(get|set)Value`, small classes like `[Ss]`) or, once that set grows too large, a formula. Exact sets turn
//! into an OR over strings of an AND over each string's trigrams. Anything that can match a string shorter than three
//! bytes imposes no constraint. The result is an over-approximation by construction: it may admit files that don't
//! match, never the reverse.

use regex_syntax::hir::{Class, Hir, HirKind};

use crate::content::fold;

/// A trigram formula.
#[derive(Debug, Clone, PartialEq)]
pub enum Plan {
    /// No constraint: every file is a candidate.
    All,
    /// Every one of these trigrams (ascending, distinct).
    Grams(Vec<u32>),
    And(Vec<Plan>),
    Or(Vec<Plan>),
}

/// Exact sets larger than this are turned into a formula.
const MAX_SET: usize = 64;
/// Classes with more characters than this match "any character" as far as the analysis cares.
const MAX_CLASS: u32 = 16;

struct Info {
    /// Every string the node can match, folded, when that set is small.
    exact: Option<Vec<Vec<u8>>>,
    /// Holds for every match when `exact` is `None`.
    plan: Plan,
}

impl Info {
    fn any() -> Info {
        Info { exact: None, plan: Plan::All }
    }

    fn exact(set: Vec<Vec<u8>>) -> Info {
        Info { exact: Some(set), plan: Plan::All }
    }

    fn to_plan(&self) -> Plan {
        match &self.exact {
            Some(set) => set_plan(set),
            None => self.plan.clone(),
        }
    }
}

/// The trigram formula for a literal string (already folded or not; it is folded here).
pub fn literal_plan(s: &[u8]) -> Plan {
    set_plan(&[s.iter().map(|&b| fold(b)).collect()])
}

fn string_grams(s: &[u8]) -> Vec<u32> {
    let mut g: Vec<u32> = s.windows(3).map(|w| (w[0] as u32) << 16 | (w[1] as u32) << 8 | w[2] as u32).collect();
    g.sort_unstable();
    g.dedup();
    g
}

fn set_plan(set: &[Vec<u8>]) -> Plan {
    if set.is_empty() || set.iter().any(|s| s.len() < 3) {
        return Plan::All;
    }
    let alts: Vec<Plan> = set.iter().map(|s| Plan::Grams(string_grams(s))).collect();
    or(alts)
}

fn and(parts: Vec<Plan>) -> Plan {
    let mut grams = Vec::new();
    let mut rest = Vec::new();
    for p in parts {
        match p {
            Plan::All => {}
            Plan::Grams(g) => grams.extend(g),
            Plan::And(inner) => {
                for q in inner {
                    match q {
                        Plan::Grams(g) => grams.extend(g),
                        other => rest.push(other),
                    }
                }
            }
            other => rest.push(other),
        }
    }
    grams.sort_unstable();
    grams.dedup();
    if !grams.is_empty() {
        rest.insert(0, Plan::Grams(grams));
    }
    match rest.len() {
        0 => Plan::All,
        1 => rest.pop().unwrap(),
        _ => Plan::And(rest),
    }
}

fn or(parts: Vec<Plan>) -> Plan {
    if parts.contains(&Plan::All) || parts.is_empty() {
        return Plan::All;
    }
    let mut parts = parts;
    parts.dedup();
    if parts.len() == 1 { parts.pop().unwrap() } else { Plan::Or(parts) }
}

/// The trigram formula for a parsed expression. Strings are folded the way the index folds file bytes.
pub fn plan(hir: &Hir) -> Plan {
    info(hir).to_plan()
}

fn info(hir: &Hir) -> Info {
    match hir.kind() {
        HirKind::Empty | HirKind::Look(_) => Info::exact(vec![Vec::new()]),
        HirKind::Literal(lit) => Info::exact(vec![lit.0.iter().map(|&b| fold(b)).collect()]),
        HirKind::Class(class) => class_info(class),
        HirKind::Capture(c) => info(&c.sub),
        HirKind::Repetition(r) => {
            let sub = info(&r.sub);
            match (r.min, r.max) {
                (0, Some(1)) => match sub.exact {
                    Some(mut set) if set.len() < MAX_SET => {
                        set.push(Vec::new());
                        set.sort();
                        set.dedup();
                        Info::exact(set)
                    }
                    _ => Info::any(),
                },
                (0, _) => Info::any(),
                // One or more: whatever one copy needs holds.
                _ => Info { exact: None, plan: sub.to_plan() },
            }
        }
        HirKind::Concat(subs) => {
            let mut done: Vec<Plan> = Vec::new();
            let mut cur: Option<Vec<Vec<u8>>> = Some(vec![Vec::new()]);
            let mut exact_all = true;
            for sub in subs {
                let i = info(sub);
                match (cur.take(), i.exact) {
                    (Some(a), Some(b)) if a.len() * b.len() <= MAX_SET => cur = Some(cross(&a, &b)),
                    (Some(a), Some(b)) => {
                        exact_all = false;
                        done.push(set_plan(&a));
                        cur = Some(b);
                    }
                    (Some(a), None) => {
                        exact_all = false;
                        done.push(set_plan(&a));
                        done.push(i.plan);
                        cur = Some(vec![Vec::new()]);
                    }
                    (None, _) => unreachable!(),
                }
            }
            let cur = cur.unwrap_or_default();
            if exact_all {
                Info::exact(cur)
            } else {
                done.push(set_plan(&cur));
                Info { exact: None, plan: and(done) }
            }
        }
        HirKind::Alternation(subs) => {
            let infos: Vec<Info> = subs.iter().map(info).collect();
            if infos.iter().all(|i| i.exact.is_some()) {
                let mut set: Vec<Vec<u8>> = infos.into_iter().flat_map(|i| i.exact.unwrap()).collect();
                set.sort();
                set.dedup();
                if set.len() <= MAX_SET {
                    return Info::exact(set);
                }
                return Info { exact: None, plan: set_plan(&set) };
            }
            Info { exact: None, plan: or(infos.iter().map(Info::to_plan).collect()) }
        }
    }
}

fn cross(a: &[Vec<u8>], b: &[Vec<u8>]) -> Vec<Vec<u8>> {
    let mut out = Vec::with_capacity(a.len() * b.len());
    for x in a {
        for y in b {
            let mut s = x.clone();
            s.extend_from_slice(y);
            out.push(s);
        }
    }
    out.sort();
    out.dedup();
    out
}

fn class_info(class: &Class) -> Info {
    let mut set: Vec<Vec<u8>> = Vec::new();
    match class {
        Class::Unicode(c) => {
            let size: u32 = c.ranges().iter().map(|r| r.end() as u32 - r.start() as u32 + 1).sum();
            if size > MAX_CLASS {
                return Info::any();
            }
            for r in c.ranges() {
                for ch in r.start()..=r.end() {
                    let mut buf = [0u8; 4];
                    set.push(ch.encode_utf8(&mut buf).bytes().map(fold).collect());
                }
            }
        }
        Class::Bytes(c) => {
            let size: u32 = c.ranges().iter().map(|r| r.end() as u32 - r.start() as u32 + 1).sum();
            if size > MAX_CLASS {
                return Info::any();
            }
            for r in c.ranges() {
                for b in r.start()..=r.end() {
                    set.push(vec![fold(b)]);
                }
            }
        }
    }
    set.sort();
    set.dedup();
    Info::exact(set)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::gram;

    fn p(re: &str, ci: bool) -> Plan {
        let hir = regex_syntax::ParserBuilder::new().case_insensitive(ci).multi_line(true).build().parse(re).unwrap();
        plan(&hir)
    }

    fn g(s: &str) -> Vec<u32> {
        string_grams(&s.bytes().map(fold).collect::<Vec<_>>())
    }

    #[test]
    fn literals_and_case() {
        assert_eq!(p("Settings", false), Plan::Grams(g("settings")));
        assert_eq!(p("ABC_def", true), p("abc_DEF", false));
        // Unicode case folding adds `ſ` for `s`: still sound, one more alternative.
        assert!(matches!(p("Settings", true), Plan::Or(ref alts) if alts.contains(&Plan::Grams(g("settings")))));
        assert_eq!(p("ab", false), Plan::All);
        assert_eq!(p(r"\bSettings\b", false), Plan::Grams(g("settings")));
    }

    #[test]
    fn alternation_and_classes() {
        assert_eq!(p("(get|set)Value", false), Plan::Or(vec![Plan::Grams(g("getvalue")), Plan::Grams(g("setvalue"))]));
        assert_eq!(p("colou?r", false), Plan::Or(vec![Plan::Grams(g("color")), Plan::Grams(g("colour"))]));
        // A big class breaks the string; both sides still constrain.
        assert_eq!(p(r"func \w+Store", false), Plan::Grams({
            let mut v = g("func ");
            v.extend(g("store"));
            v.sort();
            v.dedup();
            v
        }));
        assert_eq!(p(".*", false), Plan::All);
        assert_eq!(p("a|bcd", false), Plan::All);
        assert_eq!(gram(b'A', b'b', b'C'), g("abc")[0]);
    }

    #[test]
    fn repetition() {
        assert_eq!(p("(abc)+d", false), Plan::Grams(g("abc")));
        assert_eq!(p("x(abc)*yz", false), Plan::All);
    }
}
