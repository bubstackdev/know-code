//! `knowcode history deepen`: fetch more history into a shallow clone, so the history-based answers have something
//! to read, and say how deep the history is before and after. CLI only, like `fetch-deps` (see docs/decisions.md).
//! It runs `git fetch --deepen=N` (or `--shallow-since=DATE`) and nothing else: no ref is written but what that
//! fetch writes. A clone that isn't shallow is left alone.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Result, bail};
use clap::Args;
use kc_index::{line_history, vitals};
use kc_store::answer::{Incomplete, Reason};
use serde::Serialize;
use serde_json::{Value, json};

use crate::ops::{self, repo_name};

/// Commits `history deepen` fetches when neither `--by` nor `--since` is given.
pub const DEFAULT_DEEPEN: u32 = 1000;

/// The window `hotspots` and `coupled` read, ending at HEAD.
const WINDOW_DAYS: i64 = 365;

#[derive(Args)]
pub struct DeepenOptions {
    /// The shallow clone to deepen (default: the repo containing the current directory).
    #[arg(long, short = 'r')]
    pub repo: Option<PathBuf>,
    /// Commits to fetch beyond the current boundary (`git fetch --deepen=N`); default 1000.
    #[arg(long, value_name = "N", conflicts_with = "since")]
    pub by: Option<u32>,
    /// Fetch every commit since DATE instead (`git fetch --shallow-since=DATE`): `2025-01-01`, `1 year ago`.
    #[arg(long, value_name = "DATE")]
    pub since: Option<String>,
    /// Emit the JSON envelope instead of text.
    #[arg(long)]
    pub json: bool,
}

/// How much history a checkout holds.
#[derive(Serialize, Clone, Copy)]
struct Depth {
    shallow: bool,
    /// Commits reachable from HEAD.
    commits: u64,
    /// Of those, the ones history-based answers count: a shallow clone's boundary commits are left out.
    counted: u64,
    /// The oldest counted commit's committer time, and HEAD's.
    #[serde(skip)]
    oldest: Option<i64>,
    #[serde(skip)]
    head: Option<i64>,
}

impl Depth {
    fn days(&self) -> i64 {
        match (self.oldest, self.head) {
            (Some(o), Some(h)) => ((h - o).max(0)) / 86_400,
            _ => 0,
        }
    }

    fn json(&self) -> Value {
        let mut v = serde_json::to_value(self).unwrap_or(Value::Null);
        if let Some(o) = self.oldest {
            v["since"] = json!(vitals::format_date(o));
            v["days"] = json!(self.days());
        }
        v
    }
}

fn git(root: &Path, args: &[&str], stdin: bool) -> Result<Vec<u8>, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .stdin(if stdin { Stdio::inherit() } else { Stdio::null() })
        .output()
        .map_err(|e| format!("running git: {e}"))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(crate::fetch::git_error(&String::from_utf8_lossy(&out.stderr)))
    }
}

fn measure(root: &Path) -> Depth {
    let boundary: HashSet<String> = vitals::shallow_commits(root);
    let mut d = Depth { shallow: !boundary.is_empty(), commits: 0, counted: 0, oldest: None, head: None };
    let Ok(log) = git(root, &["log", "--format=%H %ct", "HEAD"], false) else { return d };
    for line in String::from_utf8_lossy(&log).lines() {
        let Some((sha, t)) = line.split_once(' ') else { continue };
        let t: i64 = t.trim().parse().unwrap_or(0);
        d.commits += 1;
        d.head.get_or_insert(t);
        if !boundary.contains(sha) {
            d.counted += 1;
            d.oldest = Some(d.oldest.map_or(t, |o| o.min(t)));
        }
    }
    d
}

/// What each history-based answer has to read at depth `d`: `complete` (all it reads is here), `partial` or `none`.
fn readiness(d: &Depth) -> Vec<Value> {
    [
        ("hotspots", true, "churn and fix commits over the last 365 days"),
        ("coupled", true, "files changed in the same commits over the last 365 days"),
        ("ownership", false, "every commit"),
        ("reviewers", false, "every commit, recent ones weighted most"),
        ("history", false, "every commit that changed a symbol's lines"),
    ]
    .into_iter()
    .map(|(answer, windowed, reads)| {
        let since = d.oldest.map(vitals::format_date).unwrap_or_default();
        let (status, detail) = if !d.shallow {
            ("complete", "the full history is here".to_string())
        } else if d.counted == 0 {
            ("none", "no countable history: only the shallow boundary commit".to_string())
        } else if windowed && d.days() >= WINDOW_DAYS {
            ("complete", format!("the window is covered: history since {since}"))
        } else if windowed {
            ("partial", format!("{} of the window's {WINDOW_DAYS} days: history since {since} ({} commits)", d.days(), d.counted))
        } else {
            ("partial", format!("history since {since} only ({} commits); before that is unknown", d.counted))
        };
        json!({ "answer": answer, "reads": reads, "status": status, "detail": detail })
    })
    .collect()
}

pub fn deepen(o: DeepenOptions) -> Result<()> {
    let start = match &o.repo {
        Some(r) => r.clone(),
        None => std::env::current_dir()?,
    };
    let root = kc_index::repo_root(&start)?;
    let fetch = match (&o.since, o.by) {
        (Some(date), _) => {
            if date.trim().is_empty() || date.chars().any(char::is_control) {
                bail!("--since takes a date git understands: `2025-01-01`, `1 year ago`");
            }
            format!("--shallow-since={}", date.trim())
        }
        (None, by) => {
            let n = by.unwrap_or(DEFAULT_DEEPEN);
            if n == 0 {
                bail!("--by takes a number of commits above 0");
            }
            format!("--deepen={n}")
        }
    };
    let before = measure(&root);
    let mut env = json!({ "repo": root, "shallow": before.shallow, "before": before.json() });
    if !before.shallow {
        env["note"] = json!("not a shallow clone: its full history is here; nothing to deepen");
        env["answers"] = json!(readiness(&before));
    } else {
        git(&root, &["fetch", "--quiet", &fetch], true).map_err(|e| anyhow::anyhow!("git fetch {fetch} failed in {}: {e}", root.display()))?;
        let after = measure(&root);
        env["fetched"] = json!(fetch);
        env["after"] = after.json();
        env["answers"] = json!(readiness(&after));
        if !after.shallow {
            env["note"] = json!("the clone holds its full history now");
        }
    }
    ops::finish(&mut env);
    if o.json {
        println!("{}", serde_json::to_string_pretty(&env)?);
        return Ok(());
    }
    let line = |d: &Value| {
        let since = d["since"].as_str().map(|s| format!(", since {s} ({} days)", d["days"])).unwrap_or_default();
        let shallow = if d["shallow"].as_bool() == Some(true) { "shallow" } else { "full history" };
        format!("{} commits, {} counted{since}; {shallow}", d["commits"], d["counted"])
    };
    println!("{}: {}", repo_name(&root), line(&env["before"]));
    if let Some(f) = env["fetched"].as_str() {
        println!("fetched {f}: {}", line(&env["after"]));
    }
    if let Some(n) = env["note"].as_str() {
        println!("{n}");
    }
    for a in env["answers"].as_array().into_iter().flatten() {
        println!("  {:<10} {:<9} {}", a["answer"].as_str().unwrap_or_default(), a["status"].as_str().unwrap_or_default(), a["detail"].as_str().unwrap_or_default());
    }
    Ok(())
}

/// Commits a definition's history reads at most (`git log -L … -n`); past it the oldest change is unknown, and the
/// answer says so.
pub const HISTORY_MAX_COMMITS: usize = 200;

/// Files listed as changed in the same commits, at least.
pub const CHANGED_WITH_SHOWN: usize = 5;

pub use kc_index::line_history::{Change, ChangedWith};

pub struct HistoryParams {
    /// A name, `Type.member`, `file:line` or `path#name`.
    pub symbol: String,
    /// Commits listed at most (the rest are counted).
    pub limit: usize,
    /// Only commits since this date (`2025-01-01`, `90 days ago`): `git log --since`.
    pub since: Option<String>,
}

/// Who last changed some of a definition's current lines.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Writer {
    pub author: String,
    pub lines: u64,
    pub share: f64,
}

/// One repo's answer for a definition.
pub struct HistoryRepo {
    pub root: PathBuf,
    pub symbol: kc_store::query::SymbolRow,
    /// Other definitions the name matched, as `file:line`: `file:line` picks one.
    pub others: Vec<String>,
    /// The lines read at HEAD when uncommitted edits moved them: (start, end).
    pub head_lines: Option<(u32, u32)>,
    /// History has nothing to read for it: its declaration line isn't committed as it is.
    pub uncommitted: bool,
    /// Every commit read, newest first.
    pub commits: Vec<Change>,
    /// The walk stopped at [`HISTORY_MAX_COMMITS`].
    pub capped: bool,
    /// The walk stopped at the query's time budget.
    pub cut: bool,
    /// The repo is a shallow clone: history before its boundary isn't here.
    pub shallow: bool,
    pub writers: Vec<Writer>,
    pub writers_uncommitted: u64,
    pub writers_automated: u64,
    pub changed_with: Vec<ChangedWith>,
    pub changed_with_total: usize,
    /// Commits left out of `changed_with` for changing more than 50 files.
    pub sweeps: u32,
    /// `changed_with` wasn't read: the time budget was spent.
    pub changed_with_unread: bool,
    /// Its direct callers in other files changed in its commits, most shared first.
    pub callers: Vec<CallerHistory>,
    /// Its direct callers in the repo, every one: in other files and in its own.
    pub callers_total: usize,
}

impl HistoryRepo {
    /// The oldest change read is the first only when the walk read everything there is.
    pub fn first(&self) -> Option<&Change> {
        self.commits.last().filter(|_| !self.capped && !self.cut && !self.shallow)
    }
}

/// Callers walked at most for `callers`: a definition with more direct callers lists those of the first this many.
const CALLERS_WALKED: usize = 500;

/// Commits listed per caller.
const CALLER_COMMITS_SHOWN: usize = 3;

/// A direct caller of a definition, in another file, that changed in the definition's commits.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct CallerHistory {
    pub qualified: String,
    pub file: String,
    pub line: u32,
    /// The caller is test code.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub test: bool,
    /// The definition's commits that changed the caller's file too.
    pub shared: u32,
    /// The newest of them.
    pub commits: Vec<String>,
}

/// The definition's direct callers (hop 1 of `impact`, in this repo) whose files changed in its commits, with those
/// commits, most shared first; and how many direct callers it has. Callers in its own file aren't listed: every
/// commit of the definition changes that file.
fn callers_changed_with(o: &ops::Opened, def: &kc_store::query::SymbolRow, changed: &[ChangedWith], no_sync: bool) -> Result<(Vec<CallerHistory>, usize)> {
    let walk = crate::tree::impact(&mut crate::walk::Graph::new(o, &[], no_sync, usize::MAX), &[def.id], 1, CALLERS_WALKED)?;
    let direct: Vec<_> = walk.affected.iter().filter(|a| a.hop == 1 && a.repo.is_none()).collect();
    let by_file: std::collections::HashMap<&str, &ChangedWith> = changed.iter().map(|c| (c.file.as_str(), c)).collect();
    let mut seen = HashSet::new();
    let mut list: Vec<CallerHistory> = direct
        .iter()
        .filter(|a| seen.insert(a.id))
        .filter_map(|a| {
            let c = by_file.get(a.file.as_str())?;
            Some(CallerHistory {
                qualified: a.qualified.clone(),
                file: a.file.clone(),
                line: a.line,
                test: a.class == "test",
                shared: c.shared,
                commits: c.commits.iter().take(CALLER_COMMITS_SHOWN).cloned().collect(),
            })
        })
        .collect();
    list.sort_by(|a, b| b.shared.cmp(&a.shared).then_with(|| (&a.file, a.line).cmp(&(&b.file, b.line))));
    Ok((list, seen.len()))
}

/// One definition's git history, read over the lines the index says it spans (see [`kc_index::line_history`]):
/// every commit that changed them, the newest and oldest, who last changed its current lines (`git blame -w`), and
/// the files changed in the same commits. Bounded: at most [`HISTORY_MAX_COMMITS`] commits, and in the MCP server
/// the walk stops at the query's time budget, keeping what it read; either is said in the answer.
pub fn symbol_history(p: &HistoryParams, args: &ops::Scope) -> Result<ops::Out<Vec<HistoryRepo>>> {
    if let Some(s) = &p.since
        && (s.trim().is_empty() || s.chars().any(char::is_control) || s.trim_start().starts_with('-'))
    {
        bail!("since takes a date git understands: `2025-01-01`, `90 days ago`");
    }
    let roots = ops::target_repos(&args.repos)?;
    let mut opened = Vec::new();
    let mut found = Vec::new();
    for root in &roots {
        let Some(o) = ops::open_member(&roots, root, args.no_sync)? else { continue };
        let defs = kc_store::query::definitions(o.store.conn(), &p.symbol)?;
        let Some(def) = defs.first().cloned() else {
            opened.push(o);
            continue;
        };
        let others = defs.iter().skip(1).map(|d| format!("{}:{}", d.file, d.line)).collect();
        let (start, end) = (def.line, def.end_line.max(def.line));
        let head = line_history::head_span(&o.root, &def.file, start, end)?;
        let mut r = HistoryRepo {
            root: o.root.clone(),
            symbol: def.clone(),
            others,
            head_lines: head.filter(|h| *h != (start, end)),
            uncommitted: head.is_none(),
            commits: Vec::new(),
            capped: false,
            cut: false,
            shallow: !vitals::shallow_commits(&o.root).is_empty(),
            writers: Vec::new(),
            writers_uncommitted: 0,
            writers_automated: 0,
            changed_with: Vec::new(),
            changed_with_total: 0,
            sweeps: 0,
            changed_with_unread: false,
            callers: Vec::new(),
            callers_total: 0,
        };
        let until = kc_store::limits::current().freshness_deadline();
        if let Some(span) = head {
            let log = line_history::log(&o.root, &def.file, span, p.since.as_deref(), HISTORY_MAX_COMMITS, until)?;
            (r.commits, r.capped, r.cut) = (log.changes, log.capped, log.cut);
            if until.is_some_and(|u| std::time::Instant::now() >= u) {
                r.changed_with_unread = !r.commits.is_empty();
            } else {
                let shas: Vec<String> = r.commits.iter().map(|c| c.id.clone()).collect();
                let (list, sweeps) = line_history::changed_with(&o.root, &shas, &def.file)?;
                (r.changed_with_total, r.changed_with, r.sweeps) = (list.len(), list, sweeps);
                (r.callers, r.callers_total) = callers_changed_with(&o, &def, &r.changed_with, args.no_sync)?;
            }
        }
        if let Some((w, uncommitted, automated)) = kc_index::ownership::surviving_in(&o.root, &def.file, Some((start, end))) {
            r.writers = w.into_iter().map(|s| Writer { author: s.name, lines: s.lines, share: s.share }).collect();
            (r.writers_uncommitted, r.writers_automated) = (uncommitted, automated);
        }
        found.push(r);
        opened.push(o);
    }
    let items: Vec<Value> = found.iter().map(|r| history_item(r, p.limit)).collect();
    let n = items.len();
    let mut env = ops::envelope(&opened, json!(items), n, n);
    for r in &found {
        let name = &r.symbol.name;
        let at = format!("{}:{}", r.symbol.file, r.symbol.line);
        if !r.others.is_empty() {
            env["note"] = json!(format!("{} other definitions match {}; this is {at}. Ask with file:line for another", r.others.len(), p.symbol));
        }
        if r.uncommitted {
            env["note"] = json!(format!("{name} at {at} isn't committed yet (its declaration line, as it is): git has no history for it"));
        }
        if r.capped {
            ops::push_incomplete(
                &mut env,
                Incomplete::new(Reason::Capped, format!("commits: {name}'s history was read to its newest {HISTORY_MAX_COMMITS} commits; older ones, and its first change, aren't known"))
                    .with_hint(format!("git log -L{},{}:{} lists them all", r.symbol.line, r.symbol.end_line, r.symbol.file)),
            );
        }
        if r.changed_with_unread {
            ops::push_incomplete(
                &mut env,
                Incomplete::new(Reason::Capped, "changed_with: not read, the query's time budget was spent").with_hint("run it from the CLI, which has no time budget"),
            );
        }
        if r.shallow {
            env["note"] = json!(format!("{} is a shallow clone: changes before its boundary aren't here (`knowcode history deepen`)", ops::repo_name(&r.root)));
        }
    }
    if found.iter().any(|r| r.commits.len() > p.limit || r.changed_with_total > r.changed_with.len()) {
        env["truncated"] = json!(true);
    }
    ops::finish(&mut env);
    Ok(ops::Out { env, data: found })
}

/// Files listed as changed in the same commits for `limit` commits listed.
pub fn changed_with_shown(limit: usize) -> usize {
    CHANGED_WITH_SHOWN.max(limit / 2)
}

/// One repo's answer as JSON: `commits` cut at `limit`, the newest change (`last`) and, when the whole history was
/// read, the oldest (`first`).
fn history_item(r: &HistoryRepo, limit: usize) -> Value {
    let s = &r.symbol;
    let mut v = json!({
        "repo": ops::repo_name(&r.root),
        "symbol": { "name": s.name, "kind": s.kind, "file": s.file, "line": s.line, "end_line": s.end_line },
        "commits_total": r.commits.len(),
        "commits": r.commits.iter().take(limit).collect::<Vec<_>>(),
        "writers": r.writers,
    });
    if let Some(c) = &s.container {
        v["symbol"]["container"] = json!(c);
    }
    if let Some((a, b)) = r.head_lines {
        v["lines_at_head"] = json!(format!("{a}-{b}"));
    }
    if let Some(last) = r.commits.first() {
        v["last"] = json!(last);
    }
    if let Some(first) = r.first() {
        v["first"] = json!(first);
    }
    if r.writers_uncommitted > 0 {
        v["writers_uncommitted"] = json!(r.writers_uncommitted);
    }
    if r.writers_automated > 0 {
        v["writers_automated"] = json!(r.writers_automated);
    }
    if !r.commits.is_empty() && !r.changed_with_unread {
        v["changed_with"] = json!(r.changed_with.iter().take(changed_with_shown(limit)).collect::<Vec<_>>());
        v["changed_with_total"] = json!(r.changed_with_total);
        if r.sweeps > 0 {
            v["sweeps"] = json!(r.sweeps);
        }
    }
    if !r.commits.is_empty() && !r.changed_with_unread {
        v["callers_changed_with"] = json!(r.callers);
        v["callers_total"] = json!(r.callers_total);
    }
    if !r.others.is_empty() {
        v["others"] = json!(r.others);
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::tests::{repo, test_cache};

    const CART: &str = "namespace Shop;\n\npublic class Cart\n{\n    public int Total(int a)\n    {\n        return a;\n    }\n\n    public int Twice(int a)\n    {\n        return a * 2;\n    }\n}\n";

    fn commit_as(dir: &Path, who: &str, msg: &str) {
        let (name, email) = (format!("user.name={who}"), format!("user.email={who}@x"));
        for args in [vec!["add", "-A"], vec!["commit", "-q", "-m", msg]] {
            let ok = std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(["-c", name.as_str(), "-c", email.as_str(), "-c", "commit.gpgsign=false"])
                .args(&args)
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        }
    }

    fn write(dir: &Path, file: &str, text: &str) {
        std::fs::write(dir.join(file), text).unwrap();
    }

    fn ask(app: &Path, symbol: &str, since: Option<&str>) -> ops::Out<Vec<HistoryRepo>> {
        let p = HistoryParams { symbol: symbol.into(), limit: 10, since: since.map(str::to_string) };
        symbol_history(&p, &ops::Scope { repos: vec![app.to_path_buf()], no_sync: false }).unwrap()
    }

    fn authors(r: &HistoryRepo) -> Vec<(&str, &str)> {
        r.commits.iter().map(|c| (c.author.as_str(), c.subject.as_str())).collect()
    }

    /// A method's history is its lines' commits, not its file's: a commit changing only the other method isn't one,
    /// and what changed with it counts only its own commits.
    #[test]
    fn a_methods_history_is_the_commits_changing_its_lines() {
        let cache = test_cache();
        let base = cache.dir("symbol-history");
        let _ = std::fs::remove_dir_all(&base);
        let app = repo(&base, "app", &[("src/Cart.cs", CART), ("src/Price.cs", "class Price {}\n")]);
        commit_as(&app, "ann", "start");
        write(&app, "src/Cart.cs", &CART.replace("return a;", "return a + 1;"));
        write(&app, "src/Price.cs", "class Price { int p; }\n");
        commit_as(&app, "bob", "price total");
        write(&app, "src/Cart.cs", &CART.replace("return a;", "return a + 1;").replace("a * 2", "a + a"));
        write(&app, "src/Tax.cs", "class Tax {}\n");
        commit_as(&app, "cy", "tax twice");
        write(&app, "src/Cart.cs", &CART.replace("return a;", "return a + 2;").replace("a * 2", "a + a"));
        write(&app, "src/Price.cs", "class Price { int q; }\n");
        commit_as(&app, "bob", "price total again");

        let out = ask(&app, "Cart.Total", None);
        let r = &out.data[0];
        assert_eq!(authors(r), [("bob", "price total again"), ("bob", "price total"), ("ann", "start")]);
        assert_eq!(r.first().map(|c| c.subject.as_str()), Some("start"));
        assert_eq!(r.changed_with.iter().map(|c| (c.file.as_str(), c.shared)).collect::<Vec<_>>(), [("src/Price.cs", 3)]);
        assert_eq!(r.writers.iter().map(|w| (w.author.as_str(), w.lines)).collect::<Vec<_>>(), [("ann", 3), ("bob", 1)]);
        assert_eq!(out.env["items"][0]["last"]["subject"], "price total again");
        assert_eq!(out.env["items"][0]["commits_total"], 3);
        assert_eq!(out.env["complete"], true, "{}", out.env);

        // Twice: only the commits changing its lines, by its file:line too.
        let twice = ask(&app, "src/Cart.cs:10", None);
        assert_eq!(authors(&twice.data[0]), [("cy", "tax twice"), ("ann", "start")]);

        // Uncommitted lines above it move it: its history is read at HEAD's lines, and said.
        write(&app, "src/Cart.cs", &CART.replace("return a;", "return a + 2;").replace("a * 2", "a + a").replace("public class Cart\n{\n", "public class Cart\n{\n    int x;\n    int y;\n"));
        let moved = ask(&app, "Cart.Total", None);
        assert_eq!(moved.data[0].head_lines, Some((5, 8)));
        assert_eq!(authors(&moved.data[0]).len(), 3);

        // A method not committed yet has no history, and says so.
        write(&app, "src/Cart.cs", &CART.replace("    public int Twice", "    public int Fresh()\n    {\n        return 0;\n    }\n\n    public int Twice"));
        let fresh = ask(&app, "Cart.Fresh", None);
        assert!(fresh.data[0].uncommitted && fresh.data[0].commits.is_empty());
        assert!(fresh.env["note"].as_str().unwrap().contains("isn't committed yet"), "{}", fresh.env);

        // A bad `since` is refused before git sees it.
        let p = HistoryParams { symbol: "Cart.Total".into(), limit: 10, since: Some("--output=x".into()) };
        assert!(symbol_history(&p, &ops::Scope { repos: vec![app.clone()], no_sync: false }).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Its direct callers whose files changed in its commits are listed with those commits; a caller whose file never
    /// changed with it isn't, but counts among its callers.
    #[test]
    fn callers_changed_in_its_commits() {
        let cache = test_cache();
        let base = cache.dir("symbol-history-callers");
        let _ = std::fs::remove_dir_all(&base);
        let checkout = |n: i32| format!("namespace Shop;\n\npublic class Checkout\n{{\n    public int Pay(Cart c)\n    {{\n        return c.Total({n});\n    }}\n}}\n");
        let report = "namespace Shop;\n\npublic class Report\n{\n    public int Sum(Cart c)\n    {\n        return c.Total(0);\n    }\n}\n";
        let app = repo(&base, "app", &[("src/Cart.cs", CART), ("src/Checkout.cs", &checkout(1))]);
        commit_as(&app, "ann", "start");
        write(&app, "src/Cart.cs", &CART.replace("return a;", "return a + 1;"));
        write(&app, "src/Checkout.cs", &checkout(2));
        commit_as(&app, "bob", "total and checkout");
        write(&app, "src/Report.cs", report);
        commit_as(&app, "cy", "report");
        write(&app, "src/Report.cs", &report.replace("Total(0)", "Total(1)"));
        commit_as(&app, "cy", "report alone");

        let out = ask(&app, "Cart.Total", None);
        let r = &out.data[0];
        assert_eq!(r.callers_total, 2, "{}", out.env);
        let listed: Vec<(&str, u32)> = r.callers.iter().map(|c| (c.qualified.as_str(), c.shared)).collect();
        // Checkout changed in both of Total's commits (the first created both); Report in neither.
        assert_eq!(listed, [("Checkout.Pay", 2)]);
        assert_eq!(r.callers[0].commits, r.commits.iter().map(|c| c.sha.clone()).collect::<Vec<_>>());
        assert_eq!(out.env["items"][0]["callers_changed_with"][0]["file"], "src/Checkout.cs");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Past the query's time budget the walk stops, keeps what it read and says so; it never runs on.
    #[test]
    fn the_walk_stops_at_the_querys_time_budget() {
        let cache = test_cache();
        let base = cache.dir("symbol-history-budget");
        let _ = std::fs::remove_dir_all(&base);
        let app = repo(&base, "app", &[("src/Cart.cs", CART)]);
        commit_as(&app, "ann", "start");
        let query = kc_store::limits::Operation::new();
        let _entered = kc_store::limits::enter(query.clone());
        // Indexing is done; the budget is spent before git starts.
        let _ = ask(&app, "Cart.Total", None);
        query.set_freshness_deadline(Some(std::time::Instant::now()));
        let mut out = ask(&app, "Cart.Total", None);
        ops::add_cuts(&mut out.env);
        ops::finish(&mut out.env);
        assert!(out.data[0].cut, "{}", out.env);
        assert!(out.data[0].first().is_none());
        assert_eq!(out.env["complete"], false, "{}", out.env);
        assert!(out.env.to_string().contains("time budget"), "{}", out.env);
        let _ = std::fs::remove_dir_all(&base);
    }
}
