//! Historical contribution from git history (CodeScene's "knowledge map"): per file and per directory, who added
//! its lines (each author's share of lines added), the truck factor, how much was added by authors who have left, an
//! off-boarding simulation, and who should review a change. All of it counts history: lines added and later rewritten
//! or deleted still count, so it is not who wrote the code as it is now, nor who understands it today. For one file,
//! [`surviving`] counts the current lines by author (`git blame`).
//!
//! Everything comes from one `git log --numstat` walk over the whole local history, cached per HEAD and anchor day.
//! Names and emails are the ones already in that history; nothing leaves the machine.
//!
//! - **Ownership** is by lines added (`git log --numstat`, merges, automated commits ([`vitals::Bots`]) and a
//!   shallow clone's boundary commit left out).
//!   Renames are followed: a moved file keeps its earlier authors, and the move itself adds no lines. Authors are keyed by lower-cased email and shown by their latest name.
//! - **Truck factor** of a file or area: the fewest authors whose lines added exceed 50% of its lines added.
//! - **Former authors** are listed in `former_authors` in [`crate::config_dir`] (one name or email per line, `#`
//!   comments). An author with no commit in the repo for [`INACTIVE_MONTHS`] months before its newest commit is
//!   *possibly former*: an inference from this repo's history alone, flagged as such.
//! - **Contributors gone**: the share of lines added whose authors are former (listed, or possibly).

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::Result;
use kc_store::Store;
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::git;
use crate::vitals::{self, Anchor};

const DAY: i64 = 86_400;
/// Months without a commit to the repo after which an author is flagged "possibly former".
pub const INACTIVE_MONTHS: i64 = 6;
/// Half-life of lines added when ranking reviewers: recent authorship counts for more.
const REVIEW_HALF_LIFE_DAYS: f64 = 180.0;
/// Owners listed per file or area.
const OWNERS_SHOWN: usize = 5;
/// Bumped when the cached walk's shape changes.
const OWNERSHIP_FORMAT: u32 = 4;

/// One author's part of one file's history.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Part {
    /// Index into the walk's authors.
    author: u32,
    added: u64,
    commits: u32,
    /// The commits (numbered in the walk) counted in `commits`, so that a directory counts each commit once however
    /// many of its files it touched.
    #[serde(default)]
    ids: Vec<u32>,
    /// Lines added, each weighted `0.5^(age / 180 days)` at the anchor.
    recent: f64,
    last: i64,
}

/// An author across the repo.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct AuthorRec {
    email: String,
    name: String,
    commits: u32,
    last: i64,
}

/// The cached walk.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Walk {
    authors: Vec<AuthorRec>,
    files: HashMap<String, Vec<Part>>,
    /// The newest commit's time.
    last: i64,
    shallow: bool,
}

/// Why an author counts as gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Former {
    /// Listed in `former_authors`, or named by `--without`.
    Listed,
    /// No commit in the repo for [`INACTIVE_MONTHS`] months before its newest commit: an inference.
    PossiblyFormer,
}

/// An author's share of a file or area.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Owner {
    pub name: String,
    pub email: String,
    /// Share of the lines added, 0–1.
    pub share: f64,
    pub added: u64,
    pub commits: u32,
    /// Their newest commit to this file or area, `YYYY-MM-DD`.
    pub last: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub former: Option<Former>,
}

/// The historical contribution to a file or a directory: who added its lines over the history walked. History, not
/// the code as it is: lines added since rewritten or deleted still count, and nothing here measures who understands
/// the code now (see [`surviving`] for a file's current lines by author).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Knowledge {
    /// A file path, or a directory (`.` for the repo).
    pub path: String,
    /// Indexed files under it (1 for a file).
    pub files: u32,
    /// Current lines (from the index).
    pub lines: u64,
    /// Lines added over the history walked.
    pub added: u64,
    /// The author with the largest share of lines added.
    pub top_contributor: Option<Owner>,
    /// The fewest authors whose lines added exceed half the total; 0 with no history.
    pub truck_factor: u32,
    /// Share of the lines added (`added`) whose authors are listed as former.
    pub contributors_gone: f64,
    /// Share of the lines added whose authors are listed or possibly former (no commit for [`INACTIVE_MONTHS`]
    /// months: an inference).
    pub contributors_possibly_gone: f64,
    /// The largest contributors by lines added, largest first.
    pub contributors: Vec<Owner>,
    /// How many contributors there are, when more than [`OWNERS_SHOWN`] are listed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contributors_total: Option<usize>,
}

/// A suggested reviewer of a change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reviewer {
    pub name: String,
    pub email: String,
    /// Recency-weighted lines added across the changed files (half-life 180 days).
    pub score: f64,
    /// Changed files they have written lines of.
    pub files: u32,
    /// Their newest commit to the changed files, `YYYY-MM-DD`.
    pub last: String,
}

/// What the walk behind an ownership answer covers.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Coverage {
    pub authors: u32,
    /// The newest commit, `YYYY-MM-DD`.
    pub last_commit: Option<String>,
    /// A shallow clone: knowledge from before its boundary is unknown.
    pub shallow: bool,
    /// Former authors configured, and where.
    pub former_listed: u32,
    pub former_file: Option<String>,
}

/// The ownership model of one repo at an anchor.
pub struct Ownership {
    walk: Walk,
    /// Author index → why they count as gone.
    former: HashMap<u32, Former>,
    /// Current lines per indexed file.
    lines: HashMap<String, u64>,
    pub coverage: Coverage,
}

/// Names and emails in the user's `former_authors` file, lower-cased, and the file's path.
pub fn former_authors() -> (HashSet<String>, Option<String>) {
    let Ok(dir) = crate::config_dir() else { return (HashSet::new(), None) };
    let path = dir.join("former_authors");
    let Ok(text) = std::fs::read_to_string(&path) else { return (HashSet::new(), None) };
    let set = text
        .lines()
        .map(|l| l.split('#').next().unwrap_or("").trim().to_lowercase())
        .filter(|l| !l.is_empty())
        .collect();
    (set, Some(path.display().to_string()))
}

impl Ownership {
    /// Builds the model for `root`, treating the configured former authors and `without` (names or emails) as gone.
    pub fn load(root: &Path, store: &Store, anchor: Anchor, without: &[String]) -> Result<Ownership> {
        Self::load_upto(root, store, anchor, without, None)
    }

    /// [`Ownership::load`] over the history reachable from commit `upto` instead of HEAD (not cached): who knew the
    /// code before a change.
    pub fn load_upto(root: &Path, store: &Store, anchor: Anchor, without: &[String], upto: Option<&str>) -> Result<Ownership> {
        let walk = match upto {
            Some(rev) => walk_log(root, anchor, rev),
            None => walk(root, store, anchor)?,
        };
        let (mut listed, file) = former_authors();
        let former_listed = listed.len() as u32;
        listed.extend(without.iter().map(|w| w.trim().to_lowercase()));
        let mut former = HashMap::new();
        for (i, a) in walk.authors.iter().enumerate() {
            if listed.contains(&a.email) || listed.contains(&a.name.to_lowercase()) {
                former.insert(i as u32, Former::Listed);
            } else if walk.last - a.last > INACTIVE_MONTHS * 30 * DAY {
                former.insert(i as u32, Former::PossiblyFormer);
            }
        }
        let mut lines = HashMap::new();
        let mut stmt = store.conn().prepare("SELECT path, lines FROM files WHERE lang != 'hostconfig'")?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
            let (p, n) = row?;
            lines.insert(p, n.max(0) as u64);
        }
        let coverage = Coverage {
            authors: walk.authors.len() as u32,
            last_commit: (walk.last > 0).then(|| vitals::format_date(walk.last)),
            shallow: walk.shallow,
            former_listed,
            former_file: file,
        };
        Ok(Ownership { walk, former, lines, coverage })
    }

    /// Indexed files, sorted.
    pub fn files(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.lines.keys().map(String::as_str).collect();
        v.sort_unstable();
        v
    }

    /// Whether `who` (a name or email, any case) is an author here.
    pub fn knows_author(&self, who: &str) -> bool {
        let w = who.trim().to_lowercase();
        self.walk.authors.iter().any(|a| a.email == w || a.name.to_lowercase() == w)
    }

    /// Who knows one file.
    pub fn file(&self, path: &str) -> Knowledge {
        self.area(path, &[path])
    }

    /// Who knows the indexed files under directory `dir` (`.` or empty for the whole repo).
    pub fn dir(&self, dir: &str) -> Knowledge {
        let d = dir.trim_end_matches('/');
        let files: Vec<&str> = self.files().into_iter().filter(|f| d.is_empty() || d == "." || f.starts_with(&format!("{d}/"))).collect();
        self.area(if d.is_empty() { "." } else { d }, &files)
    }

    /// Directories `depth` components deep (shallower files grouped under their own directory, root files under
    /// `.`), each with its knowledge, largest first.
    pub fn areas(&self, depth: usize) -> Vec<Knowledge> {
        let mut groups: HashMap<String, Vec<&str>> = HashMap::new();
        for f in self.files() {
            let comps: Vec<&str> = f.split('/').collect();
            let k = comps.len().saturating_sub(1).min(depth);
            let key = if k == 0 { ".".to_string() } else { comps[..k].join("/") };
            groups.entry(key).or_default().push(f);
        }
        let mut out: Vec<Knowledge> = groups.iter().map(|(k, files)| self.area(k, files)).collect();
        out.sort_by(|a, b| b.lines.cmp(&a.lines).then_with(|| a.path.cmp(&b.path)));
        out
    }

    /// The main author of a file, if it has history.
    fn main_of(&self, path: &str) -> Option<u32> {
        let parts = self.walk.files.get(path)?;
        parts.iter().max_by(|a, b| a.added.cmp(&b.added).then(b.author.cmp(&a.author))).filter(|p| p.added > 0).map(|p| p.author)
    }

    fn area(&self, path: &str, files: &[&str]) -> Knowledge {
        let mut by_author: HashMap<u32, (u64, u32, i64)> = HashMap::new();
        // Each author's commits across the files, each counted once.
        let mut commits: HashMap<u32, HashSet<u32>> = HashMap::new();
        let (mut lines, mut gone, mut possibly) = (0u64, 0u64, 0u64);
        for &f in files {
            lines += self.lines.get(f).copied().unwrap_or(0);
            for p in self.walk.files.get(f).map(Vec::as_slice).unwrap_or_default() {
                match self.former.get(&p.author) {
                    Some(Former::Listed) => {
                        gone += p.added;
                        possibly += p.added;
                    }
                    Some(Former::PossiblyFormer) => possibly += p.added,
                    None => {}
                }
                let e = by_author.entry(p.author).or_default();
                e.0 += p.added;
                e.2 = e.2.max(p.last);
                commits.entry(p.author).or_default().extend(&p.ids);
            }
        }
        for (a, e) in &mut by_author {
            e.1 = commits.get(a).map_or(0, |c| c.len() as u32);
        }
        let added: u64 = by_author.values().map(|v| v.0).sum();
        let mut ranked: Vec<(u32, (u64, u32, i64))> = by_author.into_iter().filter(|(_, v)| v.0 > 0).collect();
        ranked.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(a.0.cmp(&b.0)));
        let owners: Vec<Owner> = ranked
            .iter()
            .map(|(a, (n, c, last))| {
                let rec = &self.walk.authors[*a as usize];
                Owner {
                    name: rec.name.clone(),
                    email: rec.email.clone(),
                    share: round3(*n as f64 / added.max(1) as f64),
                    added: *n,
                    commits: *c,
                    last: vitals::format_date(*last),
                    former: self.former.get(a).copied(),
                }
            })
            .collect();
        let share = |n: u64| if added == 0 { 0.0 } else { round3(n as f64 / added as f64) };
        Knowledge {
            path: path.to_string(),
            files: files.len() as u32,
            lines,
            added,
            top_contributor: owners.first().cloned(),
            truck_factor: truck_factor(&ranked.iter().map(|r| r.1.0).collect::<Vec<_>>()),
            contributors_gone: share(gone),
            contributors_possibly_gone: share(possibly),
            contributors_total: (owners.len() > OWNERS_SHOWN).then_some(owners.len()),
            contributors: owners.into_iter().take(OWNERS_SHOWN).collect(),
        }
    }

    /// Who should review a change to `files`: authors ranked by recency-weighted lines added across them, leaving
    /// out former authors (listed or possibly) and `exclude` (names or emails: the change's own author).
    pub fn reviewers(&self, files: &[&str], exclude: &[String], limit: usize) -> Vec<Reviewer> {
        let skip: HashSet<String> = exclude.iter().map(|e| e.trim().to_lowercase()).collect();
        let mut by: HashMap<u32, (f64, u32, i64)> = HashMap::new();
        for &f in files {
            for p in self.walk.files.get(f).map(Vec::as_slice).unwrap_or_default() {
                let e = by.entry(p.author).or_default();
                e.0 += p.recent;
                e.1 += 1;
                e.2 = e.2.max(p.last);
            }
        }
        let mut out: Vec<Reviewer> = by
            .into_iter()
            .filter(|(a, v)| {
                let r = &self.walk.authors[*a as usize];
                v.0 > 0.0 && !self.former.contains_key(a) && !skip.contains(&r.email) && !skip.contains(&r.name.to_lowercase())
            })
            .map(|(a, (score, n, last))| {
                let r = &self.walk.authors[a as usize];
                Reviewer { name: r.name.clone(), email: r.email.clone(), score: round3(score), files: n, last: vitals::format_date(last) }
            })
            .collect();
        out.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap().then_with(|| a.email.cmp(&b.email)));
        out.truncate(limit);
        out
    }

    /// Files whose main author is `who` (a name or email), largest first.
    pub fn main_files_of(&self, who: &str) -> Vec<(String, u64)> {
        let w = who.trim().to_lowercase();
        let ids: HashSet<u32> = self
            .walk
            .authors
            .iter()
            .enumerate()
            .filter(|(_, a)| a.email == w || a.name.to_lowercase() == w)
            .map(|(i, _)| i as u32)
            .collect();
        let mut out: Vec<(String, u64)> = self
            .files()
            .into_iter()
            .filter(|f| self.main_of(f).is_some_and(|m| ids.contains(&m)))
            .map(|f| (f.to_string(), self.lines.get(f).copied().unwrap_or(0)))
            .collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        out
    }
}

/// An author's part of a file's current lines (`git blame` of the working tree).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Survivor {
    pub name: String,
    pub email: String,
    /// Current lines last changed by them.
    pub lines: u64,
    /// Share of the file's current lines, 0–1.
    pub share: f64,
}

/// Who last changed each current line of one file, by author (after `.mailmap`), largest first: the code as it is,
/// where [`Knowledge`] counts history. Uncommitted lines and lines from automated commits are counted apart
/// (`uncommitted`, `automated`). `None` when git can't blame the file (untracked, binary, outside a checkout).
pub fn surviving(root: &Path, path: &str) -> Option<(Vec<Survivor>, u64, u64)> {
    let out = git::run(root, &["blame", "--line-porcelain", "-w", "--", path]).ok()?;
    let bots = vitals::Bots::load();
    let text = String::from_utf8_lossy(&out);
    let (mut name, mut email) = (String::new(), String::new());
    let mut by: HashMap<String, (String, u64)> = HashMap::new();
    let (mut uncommitted, mut automated, mut total) = (0u64, 0u64, 0u64);
    for l in text.lines() {
        if let Some(n) = l.strip_prefix("author ") {
            name = n.to_string();
        } else if let Some(m) = l.strip_prefix("author-mail ") {
            email = m.trim_matches(['<', '>']).to_lowercase();
        } else if let Some(subject) = l.strip_prefix("summary ") {
            // The header's last field before the line itself: one line counted.
            total += 1;
            if email == "not.committed.yet" {
                uncommitted += 1;
            } else if bots.is_bot(&name, &email, subject) {
                automated += 1;
            } else {
                let e = by.entry(email.clone()).or_insert_with(|| (name.clone(), 0));
                e.1 += 1;
            }
        }
    }
    let mut v: Vec<Survivor> = by
        .into_iter()
        .map(|(email, (name, lines))| Survivor { name, email, lines, share: round3(lines as f64 / total.max(1) as f64) })
        .collect();
    v.sort_by(|a, b| b.lines.cmp(&a.lines).then_with(|| a.email.cmp(&b.email)));
    Some((v, uncommitted, automated))
}

/// The repo's configured `user.email`, lower-cased: the author of uncommitted changes.
pub fn user_email(root: &Path) -> Option<String> {
    git::run(root, &["config", "user.email"]).ok().map(|o| String::from_utf8_lossy(&o).trim().to_lowercase()).filter(|e| !e.is_empty())
}

/// The fewest of `added` (lines per author, largest first) whose sum exceeds half the total; 0 for no lines.
pub fn truck_factor(added: &[u64]) -> u32 {
    let total: u64 = added.iter().sum();
    let mut sum = 0;
    for (i, n) in added.iter().enumerate() {
        sum += n;
        if 2 * sum > total {
            return i as u32 + 1;
        }
    }
    0
}

fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

/// The whole-history walk, cached in the `ownership` table per HEAD and anchor.
fn walk(root: &Path, store: &Store, anchor: Anchor) -> Result<Walk> {
    let head = git::GitDir::find(root).and_then(|g| g.head_commit()).unwrap_or_default();
    let key = if anchor.explicit { format!("at{}", anchor.time) } else { format!("day{}", anchor.time.div_euclid(DAY)) };
    let stamp = format!("{OWNERSHIP_FORMAT}|{head}|{key}|{}", vitals::history_inputs(root));
    let conn = store.side()?;
    conn.execute_batch("CREATE TABLE IF NOT EXISTS ownership (key TEXT PRIMARY KEY, data TEXT NOT NULL) WITHOUT ROWID;")?;
    if !head.is_empty() && store.side_meta("ownership.head")?.as_deref() == Some(stamp.as_str()) {
        let data: Option<String> = conn.query_row("SELECT data FROM ownership WHERE key = 'walk'", [], |r| r.get(0)).ok();
        if let Some(w) = data.and_then(|d| serde_json::from_str(&d).ok()) {
            return Ok(w);
        }
    }
    let cuts = kc_store::limits::cuts_so_far();
    let w = walk_log(root, anchor, "HEAD");
    // A walk cut at its deadline is partial: answered from (the answer says so), never cached.
    if kc_store::limits::cuts_so_far() != cuts {
        return Ok(w);
    }
    conn.execute("INSERT OR REPLACE INTO ownership(key, data) VALUES ('walk', ?1)", params![serde_json::to_string(&w)?])?;
    store.set_meta("ownership.head", &stamp)?;
    Ok(w)
}

/// The walk of the history reachable from `rev`. `%aE`/`%aN` apply the repo's `.mailmap`, so identities it merges
/// count as one author; without one, each email is its own author.
fn walk_log(root: &Path, anchor: Anchor, rev: &str) -> Walk {
    let shallow = vitals::shallow_commits(root);
    // Paths the repo's AI privacy file blocks are left out by git: a commit touching only them is no commit.
    let privacy = crate::privacy::Privacy::load(root);
    let excludes = privacy.exclude_pathspecs();
    let mut args = vec!["log", "--no-merges", "--numstat", "-M", "--format=@@%H\t%ct\t%aE\t%aN\t%s", rev, "--"];
    args.extend(excludes.iter().map(String::as_str));
    let log = git::run(root, &args).unwrap_or_default();
    let mut w = parse(&String::from_utf8_lossy(&log), anchor, &shallow, &vitals::Bots::load());
    w.files.retain(|p, _| !privacy.blocks(p));
    w.shallow = !shallow.is_empty();
    w
}

/// The commit a diff against `rev` starts from (`git merge-base rev HEAD`), and its time. `rev` comes from the user,
/// so it reaches `merge-base` only as the object id [`git::resolve_commit`] names.
pub fn diff_base(root: &Path, rev: &str) -> Option<(String, i64)> {
    let rev = git::resolve_commit(root, rev).ok()?;
    let out = git::run(root, &["merge-base", &rev, "HEAD"]).ok()?;
    let sha = String::from_utf8_lossy(&out).trim().to_string();
    let t = git::run(root, &["log", "-1", "--format=%ct", &sha]).ok()?;
    Some((sha.clone(), String::from_utf8_lossy(&t).trim().parse().ok()?))
}

/// Lower-cased author and committer emails (after `.mailmap`) of the commits from `base` to HEAD: the people who
/// made the change.
pub fn change_authors(root: &Path, base: &str) -> Vec<String> {
    let out = git::run(root, &["log", "--format=%aE%n%cE", &format!("{base}..HEAD"), "--"]).unwrap_or_default();
    let mut v: Vec<String> = String::from_utf8_lossy(&out).lines().map(|l| l.trim().to_lowercase()).filter(|l| !l.is_empty()).collect();
    v.sort();
    v.dedup();
    v
}

/// Changed files that say nothing about who knows the code: changelogs, lockfiles, generated and minified files.
pub fn is_incidental(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    name.starts_with("changelog")
        || name.ends_with(".lock")
        || matches!(name.as_str(), "package-lock.json" | "npm-shrinkwrap.json" | "pnpm-lock.yaml" | "packages.lock.json" | "go.sum")
        || name.contains(".min.")
        || name.contains(".generated.")
        || name.ends_with(".designer.cs")
        || path.split('/').any(|c| c == "Generated" || c == "generated")
}

/// Whether HEAD is a shallow clone's boundary commit: the clone has no history to count.
pub fn only_boundary(root: &Path) -> bool {
    let shallow = vitals::shallow_commits(root);
    !shallow.is_empty() && git::GitDir::find(root).and_then(|g| g.head_commit()).is_some_and(|h| shallow.contains(&h))
}

/// Folds `git log --numstat --format=@@%H\t%ct\t%aE\t%aN\t%s` (newest first) into per-file, per-author parts.
/// Automated commits ([`vitals::Bots`]) are left out: a job holds no knowledge and reviews nothing.
fn parse(log: &str, anchor: Anchor, shallow: &HashSet<String>, bots: &vitals::Bots) -> Walk {
    let mut w = Walk::default();
    let mut ids: HashMap<String, u32> = HashMap::new();
    // Per file: author → index into its parts.
    let mut slots: HashMap<String, HashMap<u32, usize>> = HashMap::new();
    let mut cur: Option<(u32, i64, u32)> = None;
    let mut counted: u32 = 0;
    // Renames seen so far (newer commits first): an older path -> the path it has now.
    let mut renamed: HashMap<String, String> = HashMap::new();
    for line in log.lines() {
        if let Some(rest) = line.strip_prefix("@@") {
            let mut p = rest.splitn(5, '\t');
            let (sha, time, email, name) = (p.next().unwrap_or(""), p.next().and_then(|t| t.parse::<i64>().ok()).unwrap_or(0), p.next().unwrap_or(""), p.next().unwrap_or(""));
            let email = email.trim().to_lowercase();
            let subject = p.next().unwrap_or("");
            if shallow.contains(sha) || (anchor.explicit && time > anchor.time) || email.is_empty() || bots.is_bot(name, &email, subject) {
                cur = None;
                continue;
            }
            let id = *ids.entry(email.clone()).or_insert_with(|| {
                // The log is newest first, so the first name seen is the latest.
                w.authors.push(AuthorRec { email, name: name.trim().to_string(), commits: 0, last: time });
                w.authors.len() as u32 - 1
            });
            let a = &mut w.authors[id as usize];
            a.commits += 1;
            a.last = a.last.max(time);
            w.last = w.last.max(time);
            cur = Some((id, time, counted));
            counted += 1;
            continue;
        }
        let mut parts = line.splitn(3, '\t');
        let (Some(add), Some(_), Some(path)) = (parts.next(), parts.next(), parts.next()) else { continue };
        // A move's lines belong to whoever wrote them before it: credit older commits to the path's current name.
        // Renames are followed in every commit, counted or not (a bot's move, a move after the anchor).
        let path = match split_rename(path) {
            Some((old, new)) => {
                let now = renamed.get(&new).cloned().unwrap_or(new);
                renamed.insert(old, now.clone());
                now
            }
            None => renamed.get(path).cloned().unwrap_or_else(|| path.to_string()),
        };
        let Some((author, time, commit)) = cur else { continue };
        let path = path.as_str();
        // Binary files show `-`.
        let added = add.parse::<u64>().unwrap_or(0);
        let recent = added as f64 * 0.5f64.powf((anchor.time - time).max(0) as f64 / (REVIEW_HALF_LIFE_DAYS * DAY as f64));
        let list = w.files.entry(path.to_string()).or_default();
        let slot = slots.entry(path.to_string()).or_default();
        let i = *slot.entry(author).or_insert_with(|| {
            list.push(Part { author, last: time, ..Default::default() });
            list.len() - 1
        });
        let part = &mut list[i];
        part.added += added;
        // A commit names a file once, but a rename followed onto a path it also changes names it twice.
        if part.ids.last() != Some(&commit) {
            part.ids.push(commit);
            part.commits += 1;
        }
        part.recent += recent;
        part.last = part.last.max(time);
    }
    w
}

/// `old => new` or `dir/{a => b}/file` from `--numstat` with rename detection, as (old path, new path).
pub(crate) fn split_rename(path: &str) -> Option<(String, String)> {
    let (l, r) = path.split_once(" => ")?;
    match (l.rfind('{'), r.find('}')) {
        (Some(o), Some(c)) => {
            let (pre, a) = (&l[..o], &l[o + 1..]);
            let (b, post) = (&r[..c], &r[c + 1..]);
            let join = |mid: &str| format!("{pre}{mid}{post}").replace("//", "/").trim_start_matches('/').to_string();
            Some((join(a), join(b)))
        }
        _ => Some((l.to_string(), r.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (sha, time, author, [(path, lines added)]).
    type Commit<'a> = (&'a str, i64, &'a str, &'a [(&'a str, u64)]);

    fn log(commits: &[Commit]) -> String {
        let mut s = String::new();
        for (sha, t, who, files) in commits {
            s += &format!("@@{sha}\t{t}\t{who}@x\t{who}\n\n");
            for (f, n) in *files {
                s += &format!("{n}\t0\t{f}\n");
            }
        }
        s
    }

    #[test]
    fn moves_keep_their_authors() {
        assert_eq!(split_rename("src/{a => b}/x.cs"), Some(("src/a/x.cs".into(), "src/b/x.cs".into())));
        assert_eq!(split_rename("{ => lib}/x.cs"), Some(("x.cs".into(), "lib/x.cs".into())));
        assert_eq!(split_rename("a.cs => b.cs"), Some(("a.cs".into(), "b.cs".into())));
        assert_eq!(split_rename("a.cs"), None);
        let t = 1_000 * DAY;
        // Newest first: mover renames old/x.cs to new/x.cs (no lines added), after ann wrote it.
        let text = log(&[("c2", t - DAY, "mover", &[("{old => new}/x.cs", 0)]), ("c1", t - 9 * DAY, "ann", &[("old/x.cs", 40)])]);
        let m = model(&text, &[("new/x.cs", 40)], &[], t);
        assert_eq!(m.file("new/x.cs").top_contributor.unwrap().name, "ann");
    }

    /// A job's commits own nothing, but a move it makes still carries the file's authors to the new path.
    #[test]
    fn automated_commits_own_nothing() {
        let t = 1_000 * DAY;
        let text = log(&[
            ("c3", t - DAY, "jenkins", &[("new/x.cs", 500)]),
            ("c2", t - 2 * DAY, "jenkins", &[("{old => new}/x.cs", 0)]),
            ("c1", t - 9 * DAY, "ann", &[("old/x.cs", 40)]),
        ]);
        let w = parse(&text, Anchor { time: t, explicit: false }, &HashSet::new(), &vitals::Bots::default());
        assert_eq!(w.authors.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), ["ann"]);
        assert_eq!(w.files["new/x.cs"].len(), 1);
        assert_eq!(w.files["new/x.cs"][0].added, 40);
    }

    /// A directory counts each of an author's commits once, however many of its files the commit touched.
    #[test]
    fn a_directory_counts_each_commit_once() {
        let t = 1_000 * DAY;
        let text = log(&[
            ("c3", t - DAY, "ann", &[("src/a.cs", 1), ("src/b.cs", 1), ("src/c.cs", 1)]),
            ("c2", t - 2 * DAY, "ann", &[("src/a.cs", 5), ("src/{x => b}.cs", 2)]),
            ("c1", t - 3 * DAY, "bob", &[("src/x.cs", 9)]),
        ]);
        let m = model(&text, &[("src/a.cs", 6), ("src/b.cs", 12), ("src/c.cs", 1)], &[], t);
        let commits = |k: &Knowledge| {
            let mut v: Vec<(String, u32)> = k.contributors.iter().map(|o| (o.name.clone(), o.commits)).collect();
            v.sort();
            v
        };
        assert_eq!(commits(&m.dir("src")), [("ann".to_string(), 2), ("bob".to_string(), 1)]);
        assert_eq!(commits(&m.file("src/b.cs")), [("ann".to_string(), 2), ("bob".to_string(), 1)]);
    }

    #[test]
    fn truck_factor_is_the_fewest_authors_past_half() {
        assert_eq!(truck_factor(&[]), 0);
        assert_eq!(truck_factor(&[10]), 1);
        assert_eq!(truck_factor(&[50, 50]), 2, "exactly half isn't more than half");
        assert_eq!(truck_factor(&[40, 30, 30]), 2);
        assert_eq!(truck_factor(&[20, 20, 20, 20, 20]), 3);
    }

    fn model(text: &str, lines: &[(&str, u64)], former: &[(u32, Former)], anchor: i64) -> Ownership {
        let w = parse(text, Anchor { time: anchor, explicit: false }, &HashSet::new(), &vitals::Bots::default());
        Ownership {
            walk: w,
            former: former.iter().copied().collect(),
            lines: lines.iter().map(|(p, n)| (p.to_string(), *n)).collect(),
            coverage: Coverage::default(),
        }
    }

    #[test]
    fn contribution_shares_and_contributors_gone() {
        let t = 1_000 * DAY;
        // Newest first: bob adds 30 lines to a.rs; ann wrote 70 of it and all of b.rs earlier.
        let text = log(&[
            ("c3", t - DAY, "bob", &[("src/a.rs", 30)]),
            ("c2", t - 10 * DAY, "ann", &[("src/a.rs", 70), ("src/b.rs", 100)]),
            ("c1", t - 400 * DAY, "cat", &[("lib/c.rs", 10)]),
        ]);
        let ann = 1; // author ids follow first appearance (newest first): bob 0, ann 1, cat 2
        let m = model(&text, &[("src/a.rs", 100), ("src/b.rs", 100), ("lib/c.rs", 10)], &[(ann, Former::Listed)], t);
        let a = m.file("src/a.rs");
        assert_eq!(a.top_contributor.as_ref().map(|o| o.name.as_str()), Some("ann"));
        assert_eq!(a.top_contributor.as_ref().unwrap().share, 0.7);
        assert_eq!(a.truck_factor, 1);
        // Lines added by gone authors, not the lines of files whose top contributor left: ann added 70 of 100.
        assert_eq!((a.contributors_gone, a.contributors_possibly_gone), (0.7, 0.7));
        let src = m.dir("src");
        assert_eq!((src.files, src.lines, src.added, src.truck_factor), (2, 200, 200, 1));
        let repo = m.dir(".");
        assert_eq!(repo.contributors_gone, round3(170.0 / 210.0));
        let areas: Vec<String> = m.areas(1).into_iter().map(|k| k.path).collect();
        assert_eq!(areas, ["src", "lib"]);
        // Reviewers skip former authors and the change's author.
        let r = m.reviewers(&["src/a.rs"], &[], 5);
        assert_eq!(r.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), ["bob"]);
        assert!(m.reviewers(&["src/a.rs"], &["BOB@x".into()], 5).is_empty());
        assert_eq!(m.main_files_of("ann"), [("src/a.rs".to_string(), 100), ("src/b.rs".to_string(), 100)]);
    }

    #[test]
    fn recent_authorship_ranks_reviewers_first() {
        let t = 1_000 * DAY;
        let text = log(&[("c2", t - DAY, "new", &[("a.rs", 50)]), ("c1", t - 720 * DAY, "old", &[("a.rs", 100)])]);
        let m = model(&text, &[("a.rs", 150)], &[], t);
        assert_eq!(m.file("a.rs").top_contributor.unwrap().name, "old");
        assert_eq!(m.reviewers(&["a.rs"], &[], 5)[0].name, "new");
    }

    #[test]
    fn shallow_boundary_and_later_commits_are_left_out() {
        let t = 1_000 * DAY;
        let text = log(&[("late", t + DAY, "zed", &[("a.rs", 5)]), ("tip", t - DAY, "ann", &[("a.rs", 3)]), ("root", t - 2 * DAY, "bob", &[("a.rs", 900)])]);
        let w = parse(&text, Anchor { time: t, explicit: true }, &HashSet::from(["root".to_string()]), &vitals::Bots::default());
        assert_eq!(w.authors.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), ["ann"]);
        assert_eq!(w.files["a.rs"][0].added, 3);
    }
}
