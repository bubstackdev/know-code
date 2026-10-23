//! Git vitals per file: weighted churn, recurring churn, bug-fix ratio, authors and co-change coupling from one
//! `git log` walk; complexity, nesting, function length and fan-in from the index; combined into a deterministic
//! concern score. The same walk answers [`coupled`]: which files historically change with a given one.
//!
//! Churn windows end at an [`Anchor`]: now, unless `KNOWCODE_NOW` (or `hotspots --as-of`) names another time, so a
//! dormant repo's last commits don't read as fresh churn. Co-change stays relative to the HEAD commit's date: which
//! files change together is a fact of the history, however old. A shallow clone's boundary commit (which "adds" the
//! whole tree) is left out. The log is cached per HEAD and anchor day in the index.
//!
//! Automated commits (CI jobs, dependency bots, nightly dumps; see [`Bots`]) are left out of churn, authors and
//! co-change: a job that rewrites 45 dump files every night would otherwise couple each to every other at 100%.
//! Renames are followed (`-M`): a moved file keeps its history under its current path, and a pure move counts as
//! neither churn nor a co-change.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use anyhow::Result;
use kc_store::Store;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::git;

const DAY: i64 = 86_400;

/// Commits touching more files than this (bulk renames, formatting, dependency bumps) say nothing about which files
/// belong together, so they don't count toward co-change.
const COCHANGE_MAX_FILES: usize = 50;
/// A partner counts toward `coupled_files` when it changed with the file in at least this many commits...
const COUPLED_MIN_SHARED: u32 = 3;
/// ...and in at least this share of the file's commits.
const COUPLED_MIN_RATIO: f64 = 0.5;
/// Half-life of a shared commit's weight when ranking partners: recent co-changes count for more.
const COCHANGE_HALF_LIFE_DAYS: f64 = 90.0;
/// Partners kept per file: every one sharing two or more commits, then the best single-commit ones up to this many.
const MAX_PARTNERS: usize = 50;
/// A commit's subject words count toward the files it changed (see [`subject_words`]) when it changed at most this
/// many: a broad commit's subject says little about each file.
const SUBJECT_MAX_FILES: usize = 20;
/// Subject words kept per file, the most frequent first.
const SUBJECT_WORDS_PER_FILE: usize = 48;
/// Bumped when the cached walk's shape changes, so a cache from an older build is redone even at the same HEAD.
const CHURN_FORMAT: u32 = 7;
/// A partner that isn't code (config, docs, scripts…) is a hub when it has at least this many commits of its own...
const HUB_MIN_COMMITS: u32 = 10;
/// ...and changed in at least this share of the repo's co-change commits: a changelog, or a settings file that
/// changes with everything.
const HUB_REPO_SHARE: f64 = 0.5;
/// A hub counts as a partner when at least this share of its commits came with the file...
const HUB_MIN_SHARE: f64 = 0.2;
/// ...or when it came with at least this share of the file's own commits: a README changed in every commit of a
/// script is that script's partner, however often it changes elsewhere.
const HUB_FILE_SHARE: f64 = 0.5;

/// Whether a partner that isn't code is left out as a hub: it has [`HUB_MIN_COMMITS`] commits or more (`partner`),
/// at least [`HUB_REPO_SHARE`] of the repo's co-change commits (`total`), and the `shared` commits are weak from both
/// sides: under [`HUB_MIN_SHARE`] of the partner's and under [`HUB_FILE_SHARE`] of the file's (`file`).
fn is_hub_partner(shared: u32, partner: u32, file: u32, total: u32) -> bool {
    partner >= HUB_MIN_COMMITS
        && partner as f64 >= HUB_REPO_SHARE * total as f64
        && (shared as f64) < HUB_MIN_SHARE * partner as f64
        && (shared as f64) < HUB_FILE_SHARE * file as f64
}
/// A repo with no commit in this many days before the anchor is reported as dormant.
const DORMANT_DAYS: i64 = 90;

/// Words that mark a defect fix on their own.
const FIX_ALWAYS: &[&str] = &["bug", "bugs", "bugfix", "bugfixes", "hotfix", "hotfixes", "revert", "reverts", "reverted", "defect", "defects"];
/// Words that mark a fix unless a chore word is next to them (`fix tests`, `lint fixes`, `broken build`).
const FIX_WORDS: &[&str] = &["fix", "fixes", "fixed", "fixing", "regression", "crash", "crashes", "crashing", "broken", "nre"];
/// What a "fix" can aim at that isn't a defect in the code: tests, tooling, text, merges. `gps`, `location` and
/// `position` keep the navigation sense ("GPS fix") out.
const CHORE_WORDS: &[&str] = &[
    "test", "tests", "spec", "specs", "lint", "linting", "build", "builds", "ci", "pipeline", "conflict", "conflicts",
    "merge", "typo", "typos", "format", "formatting", "whitespace", "warning", "warnings", "spelling", "docs", "doc",
    "readme", "changelog", "comment", "comments", "style", "gps", "location", "position",
];
/// Conventional-commit types that are never fixes, whatever the rest of the subject says.
const NONFIX_TYPES: &[&str] = &["test", "tests", "docs", "doc", "chore", "style", "ci", "build", "refactor", "perf", "feat"];

/// Whether a commit subject describes a bug fix. A deterministic local rule, designed against real repos' commit
/// subjects (Conventional Commits, Shortcut-style `sc-123:` prefixes, free text):
///
/// - a Conventional Commits type decides when present: `fix:`, `fix(scope):`, `bugfix:`, `hotfix:` are fixes;
///   `feat:`, `chore:`, `test:`, `docs:`, `refactor:` and the like are not;
/// - otherwise `bug`, `bugfix`, `hotfix`, `revert` or `defect` anywhere is a fix;
/// - `fix`, `fixed`, `regression`, `crash`, `broken` count unless a chore word is within two words (`fix tests`,
///   `lint fixes`, `merge conflict fix`, `GPS fix`);
/// - a subject starting with `correct` is a fix.
pub fn is_fix(subject: &str) -> bool {
    let low = subject.trim().to_ascii_lowercase();
    if let Some(t) = conventional_type(&low) {
        if matches!(t, "fix" | "fixes" | "bugfix" | "hotfix" | "bug") {
            return true;
        }
        if NONFIX_TYPES.contains(&t) {
            return false;
        }
    }
    let words: Vec<&str> = low.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).collect();
    if words.iter().any(|w| FIX_ALWAYS.contains(w)) {
        return true;
    }
    let near_chore = |i: usize| words[i.saturating_sub(2)..(i + 3).min(words.len())].iter().any(|w| CHORE_WORDS.contains(w));
    if words.iter().enumerate().any(|(i, w)| FIX_WORDS.contains(w) && !near_chore(i)) {
        return true;
    }
    words.first() == Some(&"correct")
}

/// Words in an author's name or email user that mark an automated account: `jenkins`, `CI Bot`, `ci@…`,
/// `build@…`, `Ann (via CI)`.
const BOT_WORDS: &[&str] =
    &["bot", "bots", "jenkins", "ci", "build", "builder", "buildbot", "automation", "automated", "renovate", "dependabot", "action", "actions"];
/// Email users of service mailboxes (`noreply@github.com`); a person's `123-ann@users.noreply…` is not one.
const SERVICE_USERS: &[&str] = &["noreply", "no-reply", "donotreply", "do-not-reply"];
/// Subject starts (after any Conventional Commits type) of commits a job writes, whoever it commits as.
const AUTOMATED_SUBJECTS: &[&str] = &[
    "automated task",
    "automated commit",
    "automated update",
    "automated build",
    "automated jenkins",
    "auto-update",
    "auto update",
    "autoupdate",
    "auto-generated",
    "autogenerated",
    "[bot]",
    "[automated]",
];

/// Which commits are automated: the built-in rule (see [`Bots::is_bot`]) plus the user's `bot_authors` file in
/// [`crate::config_dir`]: one name or email per line (any case, `#` comments) to count as a bot, or `!name` to
/// exempt an author the rule would flag.
#[derive(Debug, Clone, Default)]
pub struct Bots {
    listed: HashSet<String>,
    exempt: HashSet<String>,
}

impl Bots {
    /// The built-in rule and the user's `bot_authors` file, if any.
    pub fn load() -> Bots {
        let text = crate::config_dir().ok().and_then(|d| std::fs::read_to_string(d.join("bot_authors")).ok()).unwrap_or_default();
        Bots::parse(&text)
    }

    fn parse(text: &str) -> Bots {
        let mut b = Bots::default();
        for line in text.lines().map(|l| l.split('#').next().unwrap_or("").trim().to_lowercase()).filter(|l| !l.is_empty()) {
            match line.strip_prefix('!') {
                Some(e) => b.exempt.insert(e.trim().to_string()),
                None => b.listed.insert(line),
            };
        }
        b
    }

    /// Whether a commit is automated: an author named in `bot_authors`; a name or email with `[bot]`; a word of the
    /// name or of the email's user in [`BOT_WORDS`] (`jenkins`, `CI Bot`, `ci@…`, `Ann (via CI)`); a service mailbox
    /// (`noreply@…`); or a subject a job writes (`Automated task via jenkins`, `docs: auto-update README`, `[bot] …`).
    /// An author exempted with `!` in `bot_authors` is never a bot.
    pub fn is_bot(&self, name: &str, email: &str, subject: &str) -> bool {
        let (name, email) = (name.trim().to_lowercase(), email.trim().to_lowercase());
        if self.exempt.contains(&name) || self.exempt.contains(&email) {
            return false;
        }
        if self.listed.contains(&name) || self.listed.contains(&email) || name.contains("[bot]") || email.contains("[bot]") {
            return true;
        }
        let user = email.split('@').next().unwrap_or("");
        let words = |s: &str| s.split(|c: char| !c.is_ascii_alphanumeric()).any(|w| BOT_WORDS.contains(&w));
        if words(&name) || words(user) || SERVICE_USERS.contains(&user) {
            return true;
        }
        let low = subject.trim().to_lowercase();
        let rest = match conventional_type(&low) {
            Some(_) => low.split_once(':').map_or("", |(_, r)| r.trim_start()),
            None => low.as_str(),
        };
        AUTOMATED_SUBJECTS.iter().any(|s| rest.starts_with(s)) || low.contains("via jenkins")
    }
}

/// The type of a Conventional Commits subject (`fix(api)!: …` → `fix`).
fn conventional_type(low: &str) -> Option<&str> {
    let end = low.find(|c: char| !c.is_ascii_lowercase())?;
    let (t, rest) = low.split_at(end);
    let rest = match rest.strip_prefix('(') {
        Some(r) => &r[r.find(')')? + 1..],
        None => rest,
    };
    (!t.is_empty() && rest.trim_start_matches('!').starts_with(':')).then_some(t)
}

/// The time hotspot windows end at.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Anchor {
    /// Unix seconds.
    pub time: i64,
    /// Named by `KNOWCODE_NOW` or `--as-of` rather than the clock: commits after it are left out, and the cache is
    /// keyed by it exactly (the clock's anchor is keyed by day).
    pub explicit: bool,
}

impl Anchor {
    /// `spec`, or `KNOWCODE_NOW` when `spec` is `None`, or the clock. A spec is unix seconds, a `YYYY-MM-DD` date
    /// (UTC midnight), or `head` for the HEAD commit's time (`head_time`, read only then).
    pub fn resolve(spec: Option<&str>, head_time: impl FnOnce() -> i64) -> Result<Anchor> {
        let env = std::env::var("KNOWCODE_NOW").ok().filter(|s| !s.trim().is_empty());
        let Some(spec) = spec.map(str::to_string).or(env) else {
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
            return Ok(Anchor { time: now, explicit: false });
        };
        let s = spec.trim();
        let time = if s.eq_ignore_ascii_case("head") {
            head_time()
        } else if let Ok(t) = s.parse::<i64>() {
            t
        } else {
            parse_date(s).ok_or_else(|| anyhow::anyhow!("bad time {s:?}: expected unix seconds, YYYY-MM-DD or head"))?
        };
        Ok(Anchor { time, explicit: true })
    }

    /// The part of a cache key that depends on the anchor.
    fn key(&self) -> String {
        if self.explicit { format!("at{}", self.time) } else { format!("day{}", self.time.div_euclid(DAY)) }
    }
}

/// Unix seconds at UTC midnight of a `YYYY-MM-DD` date.
fn parse_date(s: &str) -> Option<i64> {
    let mut it = s.splitn(3, '-').map(|p| p.parse::<i64>().ok());
    let (y, m, d) = (it.next()??, it.next()??, it.next()??);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // Days from civil (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146_097 + doe - 719_468) * DAY)
}

/// `YYYY-MM-DD` of unix seconds (UTC).
pub fn format_date(t: i64) -> String {
    let z = t.div_euclid(DAY) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{m:02}-{d:02}", yoe + era * 400 + (m <= 2) as i64)
}

/// The weight of a fix commit at time `t` (0 at the window's start, 1 at its end) in the time-weighted fix score
/// from Google's bug-prediction study (Lewis et al., ICSE 2013): `1 / (1 + e^(−12t + 12))`. A fix at the end of the
/// window counts 0.5, one halfway 0.0025, so the score is dominated by recent fixes.
pub fn fix_weight(t: f64) -> f64 {
    1.0 / (1.0 + (-12.0 * t + 12.0).exp())
}

/// What the history behind a repo's hotspots covers.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct History {
    /// The windows' end, `YYYY-MM-DD`.
    pub anchor: String,
    /// Whether the anchor was named (`KNOWCODE_NOW`, `--as-of`) rather than the clock.
    pub anchor_explicit: bool,
    /// The newest commit's date, `YYYY-MM-DD`, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_commit: Option<String>,
    /// Whole months (30 days) from the newest commit to the anchor, when there was no commit in the 90 days before
    /// it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dormant_months: Option<u32>,
    /// A shallow clone: history starts at its boundary commits, which are left out.
    pub shallow: bool,
    /// Commits in the 365-day window, automated ones left out.
    pub commits_365d: u32,
    /// Automated commits in the 365-day window (see [`Bots::is_bot`]): left out of churn, authors and co-change.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub bot_commits_365d: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Churn {
    c30: u64,
    c90: u64,
    c365: u64,
    commits: u32,
    fixes: u32,
    /// Time-weighted fix commits: the sum of [`fix_weight`] over them.
    #[serde(default)]
    fix_score: f64,
    in30: bool,
    in90: bool,
    in365_only: bool,
    /// Distinct author emails.
    authors: u32,
    /// Commits small enough to count for co-change.
    cc_commits: u32,
    /// Files changed in the same commits: (path, shared commits, recency-weighted shared commits, the partner's own
    /// co-change commits), best weight first. Pairs whose only shared commits created both files are left out
    /// (CodeScene's rule: files added in one commit say nothing yet about changing together).
    partners: Vec<(String, u32, f64, u32)>,
    /// Single-commit partners left out of `partners` past [`MAX_PARTNERS`]: a count of partners is short by these.
    #[serde(default)]
    partners_dropped: u32,
}

/// A file that historically changes in the same commits as another.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Coupled {
    pub file: String,
    /// What the file is: an indexed language (`csharp`, `typescript`, `javascript`, `cfml`, `sql`…), or `template`
    /// (HTML, Razor, Handlebars), `style` (CSS, SCSS, Sass, Less), `config`, `docs`, `script`, `infra` or `other`
    /// (see [`partner_kind`]).
    pub kind: String,
    /// Commits (of at most 50 files, over the 365 days before HEAD) that changed both.
    pub support: u32,
    /// `support` over the other file's commits: how often a change to it came with a change to this one.
    pub confidence: f64,
    /// The other file's commits counted for co-change (the denominator of `confidence`).
    pub commits: u32,
    /// What partners are ranked by: shared commits weighted by age, each counting half as much per 90 days before
    /// HEAD, so one recent shared commit can outrank three old ones.
    pub weight: f64,
    /// False for a partner of an `--as-of` answer that existed then but is gone now (deleted, or renamed since).
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub exists_now: bool,
}

fn yes() -> bool {
    true
}

fn is_true(b: &bool) -> bool {
    *b
}

/// The file that most often changes together with this one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoChange {
    pub file: String,
    pub shared_commits: u32,
    /// `shared_commits` over this file's commits (those with at most 50 files).
    pub ratio: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileVitals {
    pub file: String,
    /// 0–100: the file's concern percentile within the repo.
    pub risk: u32,
    /// 1–10, 10 healthiest.
    pub health: u32,
    pub concern: f64,
    pub churn_30d: u64,
    pub churn_90d: u64,
    pub churn_365d: u64,
    /// `3·churn₃₀ + 2·(churn₉₀ − churn₃₀) + (churn₃₆₅ − churn₉₀)`, in lines.
    pub weighted_churn: u64,
    /// Changed in each of the 30-, 90-day and 365-day windows (excluding the shorter ones).
    pub recurring: bool,
    pub commits_365d: u32,
    pub fix_commits_365d: u32,
    pub fix_ratio: f64,
    /// Fix commits over 365 days, each weighted by how recent it is: `Σ 1/(1+e^(−12t+12))`, with `t` the commit's
    /// time scaled to 0–1 over the window (see [`fix_weight`]).
    pub fix_score: f64,
    /// Distinct commit authors over 365 days; 1 means one person holds all the recent knowledge.
    pub authors_365d: u32,
    /// Sum of per-function branch counts.
    pub complexity: u64,
    /// Deepest control-flow nesting in any function.
    pub max_nesting: u32,
    /// Functions, methods and initializers defined in the file.
    pub functions: u32,
    /// Lines in the file's longest function.
    pub longest_function: u32,
    pub longest_function_name: Option<String>,
    /// Distinct other files whose references resolve into this file.
    pub fan_in: u64,
    /// Distinct other files this file's references resolve into.
    pub fan_out: u64,
    /// The source file most often changed in the same commits over 365 days (at least 2 shared commits).
    pub co_change: Option<CoChange>,
    /// Source files changed in at least 3 of this file's commits and at least half of them.
    pub coupled_files: u32,
    /// Code-health biomarkers in the file, counted by kind (see [`crate::health`]).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub biomarkers: BTreeMap<String, u32>,
}

/// Generated sources churn with their generator, not by hand; they'd crowd out real hotspots.
fn is_generated(path: &str) -> bool {
    path.split('/').any(|c| c == "Generated" || c == "generated")
        || path.ends_with(".generated.swift")
        || path.ends_with("+Generated.swift")
}

/// Per-file vitals for the indexed source files of `root` (generated files excluded), worst first, with windows
/// ending at [`anchor`]`(root, None)`. Cached until HEAD, the index or the anchor's day changes.
pub fn hotspots(root: &Path, store: &Store) -> Result<Vec<FileVitals>> {
    hotspots_at(root, store, anchor(root, None)?)
}

/// [`hotspots`] with windows ending at `anchor`.
pub fn hotspots_at(root: &Path, store: &Store, anchor: Anchor) -> Result<Vec<FileVitals>> {
    let head = git::GitDir::find(root).and_then(|g| g.head_commit()).unwrap_or_default();
    let generation = format!("{CHURN_FORMAT}|h{}|{head}|{}|{}|{}", crate::health::VERSION, anchor.key(), store.generation()?, history_inputs(root));
    store.derived("hotspots", &generation, || compute(root, store, anchor))
}

/// The anchor for `root`: `spec`, `KNOWCODE_NOW`, or the clock (see [`Anchor::resolve`]).
pub fn anchor(root: &Path, spec: Option<&str>) -> Result<Anchor> {
    Anchor::resolve(spec, || head_time(root))
}

/// The HEAD commit's time, or 0.
fn head_time(root: &Path) -> i64 {
    git::run(root, &["log", "-1", "--format=%ct"]).ok().and_then(|o| String::from_utf8_lossy(&o).trim().parse().ok()).unwrap_or(0)
}

/// What the history behind `root`'s hotspots covers, as of `anchor` (walking the log unless it is cached).
pub fn history(root: &Path, store: &Store, anchor: Anchor) -> Result<History> {
    walk_unless_cached(root, store, anchor)?;
    Ok(store.side_meta("churn.history")?.and_then(|h| serde_json::from_str(&h).ok()).unwrap_or_default())
}

/// The kind of a co-change partner, if it may be one: an indexed source language (`csharp`, `typescript`, …); a
/// file beside the code that changes with it though nothing indexes its symbols: `template` (`.html`, `.htm`,
/// `.cshtml`, `.razor`, `.hbs`), `style` (`.css`, `.scss`, `.sass`, `.less`) or `javascript` (`.js`, `.jsx`, `.mjs`,
/// `.cjs`); or any other file git tracks: `config` (YAML, JSON, XML, TOML, INI, `.config`, `.env`, `.tfvars`),
/// `docs` (Markdown, text, reStructuredText, AsciiDoc), `script` (shell, PowerShell, batch), `infra` (Terraform,
/// Dockerfiles, compose files, Jenkinsfiles, Makefiles) or `other`. Lockfiles and minified or bundled output
/// (`.min.`, `.bundle.`, a `dist/` directory) stay out: tools write them.
pub fn partner_kind(path: &str, lang: Option<&str>) -> Option<&'static str> {
    if let Some(l) = lang
        && let Some(s) = kc_extract::Lang::SOURCE.iter().find(|s| **s == l)
    {
        return Some(s);
    }
    let file = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    if file.contains(".min.") || file.contains(".bundle.") || path.split('/').any(|c| c == "dist") {
        return None;
    }
    if file.ends_with(".lock")
        || file.ends_with(".lock.hcl")
        || matches!(file.as_str(), "package-lock.json" | "npm-shrinkwrap.json" | "pnpm-lock.yaml" | "package.resolved" | "go.sum")
    {
        return None;
    }
    if file.starts_with("dockerfile")
        || file.ends_with(".dockerfile")
        || file.starts_with("docker-compose")
        || file.starts_with("compose.")
        || file == "jenkinsfile"
        || file == "makefile"
        || file == "vagrantfile"
    {
        return Some("infra");
    }
    Some(match file.rsplit_once('.').map(|(_, e)| e) {
        Some("html" | "htm" | "cshtml" | "razor" | "hbs" | "handlebars") => "template",
        Some("css" | "scss" | "sass" | "less") => "style",
        Some("js" | "jsx" | "mjs" | "cjs") => "javascript",
        Some("tf" | "hcl" | "mk") => "infra",
        Some(
            "yml" | "yaml" | "json" | "xml" | "toml" | "ini" | "cfg" | "conf" | "config" | "properties" | "env" | "tfvars"
            | "j2" | "jn2" | "pubxml" | "props" | "targets" | "sln" | "csproj",
        ) => "config",
        Some("md" | "markdown" | "txt" | "rst" | "adoc") => "docs",
        Some("sh" | "bash" | "zsh" | "ps1" | "psm1" | "bat" | "cmd") => "script",
        _ if file.starts_with(".env") => "config",
        _ => "other",
    })
}

/// Whether a partner kind is code, or a template, stylesheet or script beside code: the kinds `coupled` listed
/// before it took every file. Partners of other kinds pass a hub check (see [`HUB_MIN_SHARE`]).
fn is_code_kind(kind: &str) -> bool {
    kc_extract::Lang::SOURCE.contains(&kind) || matches!(kind, "template" | "style" | "javascript")
}

/// Files that changed in the same commits as each of `files` (repo-relative) over the 365 days before HEAD, ranked
/// by recency-weighted shared commits: at most `limit` per file, other than the file itself, each a current file of
/// any kind ([`partner_kind`]): indexed source, or any other file discovery lists that the content index doesn't skip
/// as generated or binary. With `code_only`, partners are only source files and the templates, stylesheets and
/// scripts beside them. A file of `files` needs only history: it may be any tracked file, indexed or not. Generated
/// sources are left out, and so are hubs: a partner that isn't code, has [`HUB_MIN_COMMITS`] commits or more,
/// changed in half the repo's commits or more, and changed with the file in under a fifth of its own commits and
/// under half of the file's (a changelog; see [`is_hub_partner`]). Commits of more than 50 files don't count, nor
/// automated commits, nor pairs only ever created together. A file with no history maps to an empty list. Reuses the
/// hotspots walk and its cache. [`coupled_in`] takes another span and says which one each file's answer covers.
pub fn coupled(root: &Path, store: &Store, files: &[&str], limit: usize, code_only: bool) -> Result<HashMap<String, Vec<Coupled>>> {
    let (out, _) = coupled_in(root, store, files, limit, code_only, &Span::default())?;
    Ok(out)
}

/// The history a co-change answer reads: up to `as_of` (a commit, which counts, with its ancestors; or a date: the
/// commits made by its end), back to `since`. The default (neither) is the cached walk of the 365 days before HEAD.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Span {
    /// A git revision or a date (`YYYY-MM-DD`, or unix seconds).
    pub as_of: Option<String>,
    /// How far back: `<n>d` (or a bare number of days) before the end, a date, or `all`. Default 365 days.
    pub since: Option<String>,
}

/// Where a co-change walk ends.
enum End {
    Head,
    /// A commit, its time.
    Commit(String, i64),
    /// The end of a day (or a unix time).
    Date(i64),
}

/// Partners per file.
pub type CoupledMap = HashMap<String, Vec<Coupled>>;

/// Per file, the single-commit partners history keeps no record of.
pub type Unrecorded = HashMap<String, u32>;

/// Commits of a file read when the 365-day walk has none of it (see [`coupled_in`]).
const DORMANT_FILE_COMMITS: usize = 50;

/// [`coupled`] over `span`, and for each file the span its answer covers, in words (`365 days before HEAD`,
/// `365 days before abc1234 (2026-03-01)`, `all history before 2025-01-01`, `its last 50 commits`).
///
/// With the default span, a file the 365-day walk never saw changing (a dormant one in a busy repo) is read over its
/// own last 50 commits instead, however old: a walk of only the commits touching it, so partners keep their
/// support and confidence but hubs aren't told apart. Any other span walks the whole repo's history in it, uncached.
pub fn coupled_in(
    root: &Path,
    store: &Store,
    files: &[&str],
    limit: usize,
    code_only: bool,
    span: &Span,
) -> Result<(CoupledMap, HashMap<String, String>)> {
    coupled_in_counted(root, store, files, limit, code_only, span).map(|(out, spans, _)| (out, spans))
}

/// [`coupled_in`], also saying per file how many single-commit partners history keeps no record of (past the 50 it
/// keeps): a count of its partners is short by that many.
pub fn coupled_in_counted(
    root: &Path,
    store: &Store,
    files: &[&str],
    limit: usize,
    code_only: bool,
    span: &Span,
) -> Result<(CoupledMap, HashMap<String, String>, Unrecorded)> {
    let mut out = HashMap::new();
    let mut spans = HashMap::new();
    let mut dropped = HashMap::new();
    if *span == Span::default() {
        let churn = churn_of(root, store, files, anchor(root, None)?)?;
        let total: u32 = store.side_meta("churn.cc_total")?.and_then(|t| t.parse().ok()).unwrap_or(0);
        for &f in files {
            if let Some(c) = churn.get(f).filter(|c| c.cc_commits > 0) {
                dropped.insert(f.to_string(), c.partners_dropped);
                out.insert(f.to_string(), partners_of(store, f, c, total, limit, code_only, None)?);
                spans.insert(f.to_string(), "365 days before HEAD".to_string());
                continue;
            }
            let (walked, total) = walk_span(root, &End::Head, None, Some(f))?;
            let list = match walked.get(f) {
                Some(c) => {
                    dropped.insert(f.to_string(), c.partners_dropped);
                    partners_of(store, f, c, total, limit, code_only, None)?
                }
                None => Vec::new(),
            };
            spans.insert(f.to_string(), format!("its last {DORMANT_FILE_COMMITS} commits"));
            out.insert(f.to_string(), list);
        }
        return Ok((out, spans, dropped));
    }
    let end = match &span.as_of {
        None => End::Head,
        Some(spec) => resolve_end(root, spec)?,
    };
    let end_time = match &end {
        End::Head => head_time(root),
        End::Commit(_, t) | End::Date(t) => *t,
    };
    let (since, since_words) = match span.since.as_deref().map(str::trim) {
        None => (Some(end_time - 365 * DAY), "365 days".to_string()),
        Some(s) if s.eq_ignore_ascii_case("all") => (None, "all history".to_string()),
        Some(s) => match s.trim_end_matches(['d', 'D']).parse::<i64>() {
            Ok(days) if !s.contains('-') => (Some(end_time - days * DAY), format!("{days} days")),
            _ => {
                let t = parse_date(s).ok_or_else(|| anyhow::anyhow!("bad since {s:?}: expected <n>d, YYYY-MM-DD or all"))?;
                (Some(t), format!("since {s}"))
            }
        },
    };
    let to = match &end {
        End::Head => "HEAD".to_string(),
        End::Commit(sha, t) => format!("{} ({})", &sha[..sha.len().min(10)], format_date(*t)),
        End::Date(t) => format_date(*t),
    };
    let words = if since_words.starts_with("since") { format!("{since_words}, up to {to}") } else { format!("{since_words} before {to}") };
    let (walked, total) = walk_span(root, &end, since, None)?;
    // As of a past point, a partner is any file of the tree then, though it may be gone now.
    let then = match &end {
        End::Head => None,
        End::Commit(sha, _) => Some(tree_at(root, sha)),
        End::Date(t) => {
            let sha = git::run(root, &["rev-list", "-1", &format!("--before={t}"), "HEAD"]).map(|o| String::from_utf8_lossy(&o).trim().to_string()).unwrap_or_default();
            Some(if sha.is_empty() { HashSet::new() } else { tree_at(root, &sha) })
        }
    };
    for &f in files {
        let list = match walked.get(f) {
            Some(c) => {
                dropped.insert(f.to_string(), c.partners_dropped);
                partners_of(store, f, c, total, limit, code_only, then.as_ref())?
            }
            None => Vec::new(),
        };
        out.insert(f.to_string(), list);
        spans.insert(f.to_string(), words.clone());
    }
    Ok((out, spans, dropped))
}

/// The commit an `--as-of` answer stands at: the commit `spec` names, or for a date the last commit made by its end
/// (`None` when there is none: the repo had no history then). Answers as of a point read nothing later than it.
pub fn pin_commit(root: &Path, spec: &str) -> Result<Option<String>> {
    Ok(match resolve_end(root, spec)? {
        End::Commit(sha, _) => Some(sha),
        End::Date(t) => {
            let sha = git::run(root, &["rev-list", "-1", &format!("--before={t}"), "HEAD"]).map(|o| String::from_utf8_lossy(&o).trim().to_string()).unwrap_or_default();
            Some(sha).filter(|s| !s.is_empty())
        }
        End::Head => None,
    })
}

/// A commit (any revision git names) or a date: the end of a co-change walk.
fn resolve_end(root: &Path, spec: &str) -> Result<End> {
    let s = spec.trim();
    if let Some(day) = parse_date(s) {
        return Ok(End::Date(day + DAY - 1));
    }
    if s.len() >= 9 && s.bytes().all(|b| b.is_ascii_digit()) {
        return Ok(End::Date(s.parse()?));
    }
    // Resolved first, so the user's string reaches `git log` only as an object id, never as an option.
    let none = || anyhow::anyhow!("no commit {s:?}: expected a git revision, YYYY-MM-DD or unix seconds");
    let sha = git::resolve_commit(root, s).map_err(|_| none())?;
    let out = git::run(root, &["log", "-1", "--format=%H %ct", &sha, "--"]).map_err(|_| none())?;
    let text = String::from_utf8_lossy(&out);
    let (sha, t) = text.trim().split_once(' ').ok_or_else(|| anyhow::anyhow!("no commit {s:?}"))?;
    Ok(End::Commit(sha.to_string(), t.parse().unwrap_or(0)))
}

/// An uncached co-change walk ending at `end`, back to `since` (unix time; `None`: all history). With `file`, only
/// the commits touching it (its last [`DORMANT_FILE_COMMITS`], every file of each). Returns per-path churn and the
/// walk's co-change commit count.
fn walk_span(root: &Path, end: &End, since: Option<i64>, file: Option<&str>) -> Result<(HashMap<String, Churn>, u32)> {
    let mut args: Vec<String> =
        ["log", "--no-merges", "--numstat", "--summary", "-M", "--format=@@%H\t%ct\t%aE\t%aN\t%s"].map(String::from).to_vec();
    let end_time = match end {
        End::Head => {
            args.push("HEAD".into());
            head_time(root)
        }
        End::Commit(sha, t) => {
            args.push(sha.clone());
            *t
        }
        End::Date(t) => {
            args.push(format!("--until={t}"));
            args.push("HEAD".into());
            *t
        }
    };
    if let Some(t) = since {
        args.push(format!("--since={t}"));
    }
    let privacy = crate::privacy::Privacy::load(root);
    match file {
        Some(f) => args.extend(["--full-diff".to_string(), format!("-n{DORMANT_FILE_COMMITS}"), "--".to_string(), f.to_string()]),
        None if privacy.blocks_anything() => args.push("--".to_string()),
        None => {}
    }
    args.extend(privacy.exclude_pathspecs());
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let log = git::run_walk(root, &args).unwrap_or_default();
    let w = Window {
        head_time: end_time,
        cc_since: since.unwrap_or(i64::MIN),
        anchor: Anchor { time: end_time, explicit: true },
        shallow: shallow_commits(root),
        bots: Bots::load(),
    };
    let (mut out, stats, _) = parse_log(&String::from_utf8_lossy(&log), &w);
    drop_blocked(&privacy, &mut out, None);
    Ok((out, stats.cc_commits))
}

/// Leaves the paths the repo's AI privacy file blocks out of a walk: as files and as partners. The log was asked
/// without them where git can leave them out; a walk following one file's commits whole (`--full-diff`) or a rename
/// out of a blocked folder can still name them.
fn drop_blocked(privacy: &crate::privacy::Privacy, out: &mut HashMap<String, Churn>, words: Option<&mut SubjectWords>) {
    if !privacy.blocks_anything() {
        return;
    }
    out.retain(|p, _| !privacy.blocks(p));
    for c in out.values_mut() {
        c.partners.retain(|(p, ..)| !privacy.blocks(p));
    }
    if let Some(w) = words {
        w.retain(|p, _| !privacy.blocks(p));
    }
}

/// The paths of the tree at commit `sha`, without those the repo's AI privacy file blocks.
fn tree_at(root: &Path, sha: &str) -> HashSet<String> {
    let privacy = crate::privacy::Privacy::load(root);
    git::run(root, &["ls-tree", "-r", "-z", "--name-only", sha])
        .map(|o| o.split(|&b| b == 0).filter(|p| !p.is_empty()).map(|p| String::from_utf8_lossy(p).into_owned()).filter(|p| !privacy.blocks(p)).collect())
        .unwrap_or_default()
}

/// A file's partners from its walk, filtered as [`coupled`] says. With `then` (the tree at an `--as-of` commit), a
/// partner is any file of that tree, current or not (`exists_now` says which); otherwise a current file.
fn partners_of(
    store: &Store,
    f: &str,
    c: &Churn,
    total: u32,
    limit: usize,
    code_only: bool,
    then: Option<&HashSet<String>>,
) -> Result<Vec<Coupled>> {
    let conn = store.conn();
    let mut lang_of = conn.prepare_cached("SELECT lang FROM files WHERE path = ?1")?;
    let mut listed = conn.prepare_cached("SELECT 1 FROM cfiles WHERE path = ?1 AND (skip IS NULL OR skip = 'large')")?;
    let mut list = Vec::new();
    for (p, n, w, pc) in &c.partners {
        if list.len() >= limit {
            break;
        }
        if p == f || is_generated(p) {
            continue;
        }
        let lang: Option<String> = lang_of.query_row([p], |r| r.get(0)).optional()?;
        let kind = match then {
            // Gone files have no language now: the extension says what they were.
            Some(_) if lang.is_none() => partner_kind(p, kc_extract::Lang::from_path(p).map(|l| l.as_str())),
            _ => partner_kind(p, lang.as_deref()),
        };
        let Some(kind) = kind.filter(|k| !code_only || is_code_kind(k)) else { continue };
        let exists_now = lang.as_deref().is_some_and(|l| kc_extract::Lang::SOURCE.contains(&l)) || listed.exists([p])?;
        let counts = match then {
            Some(tree) => tree.contains(p.as_str()),
            None => exists_now,
        };
        if !counts {
            continue;
        }
        if !is_code_kind(kind) && is_hub_partner(*n, *pc, c.cc_commits, total) {
            continue;
        }
        let confidence = if c.cc_commits == 0 { 0.0 } else { round2(*n as f64 / c.cc_commits as f64) };
        list.push(Coupled { file: p.clone(), kind: kind.to_string(), support: *n, confidence, commits: c.cc_commits, weight: round2(*w), exists_now });
    }
    Ok(list)
}

/// Per-function facts summed or maxed per file.
#[derive(Default)]
struct Structure {
    complexity: u64,
    max_nesting: u32,
    functions: u32,
    longest: u32,
    longest_name: Option<String>,
}

fn compute(root: &Path, store: &Store, anchor: Anchor) -> Result<Vec<FileVitals>> {
    let churn = churn(root, store, anchor)?;
    let conn = store.conn();

    let mut structure: HashMap<String, Structure> = HashMap::new();
    let langs = kc_extract::Lang::SOURCE.map(|l| format!("'{l}'")).join(", ");
    let mut stmt = conn.prepare(&format!(
        "SELECT f.path, s.kind, s.name, s.complexity, s.nesting, s.end_line - s.start_line + 1
         FROM files f LEFT JOIN symbols s ON s.file_id = f.id WHERE f.lang IN ({langs})"
    ))?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, Option<i64>>(3)?,
            r.get::<_, Option<i64>>(4)?,
            r.get::<_, Option<i64>>(5)?,
        ))
    })?;
    for row in rows {
        let (path, kind, name, cx, nest, lines) = row?;
        let s = structure.entry(path).or_default();
        s.complexity += cx.unwrap_or(0) as u64;
        s.max_nesting = s.max_nesting.max(nest.unwrap_or(0) as u32);
        if matches!(kind.as_deref(), Some("function" | "method" | "init")) {
            s.functions += 1;
            let lines = lines.unwrap_or(0) as u32;
            // Ties go to the alphabetically first name, so the answer doesn't depend on row order.
            if lines > s.longest || (lines == s.longest && name < s.longest_name) {
                s.longest = lines;
                s.longest_name = name;
            }
        }
    }

    // Coupling from resolved references (`exact` or `unique_name`), cached per index generation.
    let mut fan_in: HashMap<String, u64> = HashMap::new();
    let mut fan_out: HashMap<String, u64> = HashMap::new();
    for e in store.file_edges()? {
        *fan_in.entry(e.dst).or_default() += 1;
        *fan_out.entry(e.src).or_default() += 1;
    }

    let markers = crate::health::counts_by_file(&crate::health::findings(root, store, None)?);
    let sources: HashSet<&str> = structure.keys().map(String::as_str).filter(|p| !is_generated(p)).collect();
    let mut out: Vec<FileVitals> = structure
        .iter()
        .filter(|(path, _)| sources.contains(path.as_str()))
        .map(|(path, st)| {
            let c = churn.get(path).cloned().unwrap_or_default();
            let weighted = 3 * c.c30 + 2 * (c.c90 - c.c30) + (c.c365 - c.c90);
            // Partners are other source files: a changelog or lock file that changes with everything isn't a
            // design coupling.
            let ratio = |shared: u32| if c.cc_commits == 0 { 0.0 } else { round2(shared as f64 / c.cc_commits as f64) };
            let partners: Vec<(&String, u32)> =
                c.partners.iter().filter(|(p, n, _, _)| *n >= 2 && sources.contains(p.as_str())).map(|(p, n, _, _)| (p, *n)).collect();
            // The most shared partner; ties go to the first name.
            let top = partners.iter().min_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
            FileVitals {
                file: path.clone(),
                risk: 0,
                health: 10,
                concern: 0.0,
                churn_30d: c.c30,
                churn_90d: c.c90,
                churn_365d: c.c365,
                weighted_churn: weighted,
                recurring: c.in30 && c.in90 && c.in365_only,
                commits_365d: c.commits,
                fix_commits_365d: c.fixes,
                fix_ratio: if c.commits == 0 { 0.0 } else { c.fixes as f64 / c.commits as f64 },
                fix_score: (c.fix_score * 1000.0).round() / 1000.0,
                authors_365d: c.authors,
                complexity: st.complexity,
                max_nesting: st.max_nesting,
                functions: st.functions,
                longest_function: st.longest,
                longest_function_name: st.longest_name.clone(),
                fan_in: fan_in.get(path).copied().unwrap_or(0),
                fan_out: fan_out.get(path).copied().unwrap_or(0),
                co_change: top.map(|(p, n)| CoChange { file: (*p).clone(), shared_commits: *n, ratio: ratio(*n) }),
                coupled_files: partners
                    .iter()
                    .filter(|(_, n)| *n >= COUPLED_MIN_SHARED && ratio(*n) >= COUPLED_MIN_RATIO)
                    .count() as u32,
                biomarkers: markers.get(path).cloned().unwrap_or_default(),
            }
        })
        .collect();

    let refs: Vec<&FileVitals> = out.iter().collect();
    let concern = concern(&refs);
    for (f, c) in out.iter_mut().zip(concern) {
        f.concern = (c * 100.0).round() / 100.0;
    }
    out.sort_by(|a, b| b.concern.partial_cmp(&a.concern).unwrap().then(a.file.cmp(&b.file)));
    let n = out.len();
    for (rank, f) in out.iter_mut().enumerate() {
        // Percentile: the worst file is 100, the best 0.
        f.risk = if n <= 1 { 0 } else { (100.0 * (n - 1 - rank) as f64 / (n - 1) as f64).round() as u32 };
        f.health = 10 - (f.risk * 9 / 100);
    }
    Ok(out)
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// The concern score of each file, with z-scores taken across `files` (one repo, or a whole workspace):
///
/// ```text
/// concern = z(weighted_churn) + z(fix_score) + z(complexity)
///         + 0.5·z(max_nesting) + 0.5·z(longest_function) + 0.5·z(fan_in) + 0.5·z(coupled_files)
///         + 0.25·z(authors_365d)
/// ```
///
/// `fix_score` is the time-weighted count of fix commits (see [`fix_weight`]). It replaced the shrunk bug-fix rate
/// `fix_commits / (commits + 2)`. Replayed on real repos' history (files ranked at a past commit, scored against the
/// files fixes touched in the next 90 days), the swap left AUC unchanged and precision@10 about the same or slightly
/// better; alone, the fix score beats the fix rate at precision@10.
pub fn concern(files: &[&FileVitals]) -> Vec<f64> {
    let z = |vals: Vec<f64>| -> Vec<f64> {
        let n = vals.len().max(1) as f64;
        let mean = vals.iter().sum::<f64>() / n;
        let sd = (vals.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n).sqrt();
        vals.iter().map(|v| if sd > 0.0 { (v - mean) / sd } else { 0.0 }).collect()
    };
    let col = |f: &dyn Fn(&FileVitals) -> f64| z(files.iter().map(|v| f(v)).collect());
    let terms = [
        (1.0, col(&|f| f.weighted_churn as f64)),
        (1.0, col(&|f| f.fix_score)),
        (1.0, col(&|f| f.complexity as f64)),
        (0.5, col(&|f| f.max_nesting as f64)),
        (0.5, col(&|f| f.longest_function as f64)),
        (0.5, col(&|f| f.fan_in as f64)),
        (0.5, col(&|f| f.coupled_files as f64)),
        (0.25, col(&|f| f.authors_365d as f64)),
    ];
    (0..files.len()).map(|i| terms.iter().map(|(w, z)| w * z[i]).sum()).collect()
}

/// Lines changed, commits, fixes, authors and co-change partners per path, from one `git log` walk, cached per HEAD
/// commit and anchor: churn over the 365 days before `anchor`, co-change over the 365 days before HEAD.
fn churn(root: &Path, store: &Store, anchor: Anchor) -> Result<HashMap<String, Churn>> {
    if let Some(walked) = walk_unless_cached(root, store, anchor)? {
        return Ok(walked);
    }
    let mut stmt = store.side()?.prepare("SELECT path, data FROM churn")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut out = HashMap::new();
    for row in rows {
        let (p, d) = row?;
        if let Ok(c) = serde_json::from_str(&d) {
            out.insert(p, c);
        }
    }
    Ok(out)
}

/// A pair of files that changed in the same commits, from the co-change walk.
#[derive(Debug, Clone, PartialEq)]
pub struct CoChangePair {
    pub a: String,
    pub b: String,
    /// Commits (of at most 50 files, over the 365 days before HEAD) that changed both.
    pub shared: u32,
    /// Each file's commits counted for co-change.
    pub a_commits: u32,
    pub b_commits: u32,
}

/// Every co-change pair among `files` (repo-relative), each once with `a < b`, from the cached hotspots walk: pairs
/// sharing at least `min_shared` commits, under the same rules as [`coupled`] (no large or automated commits, no
/// pairs only ever created together; each file's partners are its 50 most recent-weighted).
pub fn co_change_pairs(root: &Path, store: &Store, files: &HashSet<&str>, min_shared: u32) -> Result<Vec<CoChangePair>> {
    let churn = churn(root, store, anchor(root, None)?)?;
    let mut out = Vec::new();
    let mut seen: HashSet<(&str, &str)> = HashSet::new();
    for (path, c) in &churn {
        if !files.contains(path.as_str()) {
            continue;
        }
        for (p, n, _, _) in &c.partners {
            if *n < min_shared || p == path || !files.contains(p.as_str()) {
                continue;
            }
            let (a, b) = if path < p { (path.as_str(), p.as_str()) } else { (p.as_str(), path.as_str()) };
            if !seen.insert((a, b)) {
                continue;
            }
            let commits = |f: &str| churn.get(f).map_or(0, |c| c.cc_commits);
            out.push(CoChangePair { a: a.to_string(), b: b.to_string(), shared: *n, a_commits: commits(a), b_commits: commits(b) });
        }
    }
    out.sort_by(|x, y| (&x.a, &x.b).cmp(&(&y.a, &y.b)));
    Ok(out)
}

/// [`churn`] for a few paths, reading only their rows when the cache is current.
fn churn_of(root: &Path, store: &Store, paths: &[&str], anchor: Anchor) -> Result<HashMap<String, Churn>> {
    if let Some(mut walked) = walk_unless_cached(root, store, anchor)? {
        walked.retain(|p, _| paths.contains(&p.as_str()));
        return Ok(walked);
    }
    let mut stmt = store.side()?.prepare_cached("SELECT data FROM churn WHERE path = ?1")?;
    let mut out = HashMap::new();
    for &p in paths {
        let data: Option<String> = stmt.query_row([p], |r| r.get(0)).optional()?;
        if let Some(c) = data.and_then(|d| serde_json::from_str(&d).ok()) {
            out.insert(p.to_string(), c);
        }
    }
    Ok(out)
}

/// The words of the commit subjects that changed each file over the 365 days before HEAD (stemmed, stop words and
/// ticket ids left out; see [`kc_store::locate::subject_terms`]), each with the number of commits whose subject has
/// it, at most 48 per file, the most frequent first. Only commits of at most 20 files count, and no automated ones.
/// What a team calls the code a file holds, in its own words: `locate` reads them as a field of the file. Reuses the
/// hotspots walk and its cache.
pub fn subject_words(root: &Path, store: &Store) -> Result<HashMap<String, Vec<(String, u32)>>> {
    walk_unless_cached(root, store, anchor(root, None)?)?;
    let conn = store.side()?;
    conn.execute_batch("CREATE TABLE IF NOT EXISTS churn_words (path TEXT PRIMARY KEY, words TEXT NOT NULL) WITHOUT ROWID;")?;
    let mut stmt = conn.prepare("SELECT path, words FROM churn_words")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut out = HashMap::new();
    for row in rows {
        let (p, text) = row?;
        let words = text.split(' ').filter_map(|w| w.rsplit_once(':')).filter_map(|(t, n)| Some((t.to_string(), n.parse().ok()?))).collect();
        out.insert(p, words);
    }
    Ok(out)
}

/// What a walk of the history depends on besides HEAD and the anchor, as a key for the caches built from one: the
/// user's `bot_authors` and `former_authors` files (see [`Bots`]), the repo's `.mailmap` and the git config files
/// that may name another (`mailmap.file`), and a shallow clone's boundary (`.git/shallow`). Each by its stamp (mtime
/// and size), so a change to any of them walks again.
pub fn history_inputs(root: &Path) -> String {
    let mut h = blake3::Hasher::new();
    let mut add = |p: &Path| {
        h.update(git::stamp(p).as_bytes());
        h.update(b"|");
    };
    if let Ok(dir) = crate::config_dir() {
        add(&dir.join("bot_authors"));
        add(&dir.join("former_authors"));
    }
    add(&root.join(".mailmap"));
    if let Some(g) = git::GitDir::find(root) {
        add(&g.common.join("shallow"));
        add(&g.common.join("config"));
    }
    for c in git::global_config_files() {
        add(&c);
    }
    // What the repo's AI privacy file blocks: those paths are left out of every walk.
    h.update(crate::privacy::Privacy::load(root).key().as_bytes());
    h.finalize().to_hex()[..16].to_string()
}

/// The boundary commits of a shallow clone (`.git/shallow`), empty for a full one.
pub fn shallow_commits(root: &Path) -> HashSet<String> {
    git::GitDir::find(root)
        .and_then(|g| std::fs::read_to_string(g.common.join("shallow")).ok())
        .map(|s| s.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
        .unwrap_or_default()
}

/// Walks the log into the `churn` table unless it holds this HEAD's and anchor's walk already; returns the walk when
/// it ran.
fn walk_unless_cached(root: &Path, store: &Store, anchor: Anchor) -> Result<Option<HashMap<String, Churn>>> {
    let head = git::GitDir::find(root).and_then(|g| g.head_commit()).unwrap_or_default();
    let stamp = format!("{CHURN_FORMAT}|{head}|{}|{}", anchor.key(), history_inputs(root));
    let conn = store.side()?;
    conn.execute_batch("CREATE TABLE IF NOT EXISTS churn (path TEXT PRIMARY KEY, data TEXT NOT NULL) WITHOUT ROWID;")?;
    if !head.is_empty() && store.side_meta("churn.head")?.as_deref() == Some(stamp.as_str()) {
        return Ok(None);
    }
    let head = head_time(root);
    let window = Window { head_time: head, cc_since: head - 365 * DAY, anchor, shallow: shallow_commits(root), bots: Bots::load() };
    let since = (window.head_time.min(anchor.time) - 365 * DAY).to_string();
    // `-M`: a moved file keeps its history under its current path. It costs git about as much as `--no-renames`
    // (about the same instructions retired on large histories).
    let cuts = kc_store::limits::cuts_so_far();
    // Paths the repo's AI privacy file blocks are left out by git itself: a commit touching only them is no commit.
    let privacy = crate::privacy::Privacy::load(root);
    let since = format!("--since={since}");
    let mut args = vec!["log", "--no-merges", "--numstat", "--summary", "-M", since.as_str(), "--format=@@%H\t%ct\t%aE\t%aN\t%s"];
    let excludes = privacy.exclude_pathspecs();
    if !excludes.is_empty() {
        args.push("--");
        args.extend(excludes.iter().map(String::as_str));
    }
    let log = git::run_walk(root, &args).unwrap_or_default();
    let (mut out, stats, mut words) = parse_log(&String::from_utf8_lossy(&log), &window);
    drop_blocked(&privacy, &mut out, Some(&mut words));
    // A log cut at its deadline is partial: answered from (the answer says so), never cached.
    if kc_store::limits::cuts_so_far() != cuts {
        return Ok(Some(out));
    }
    let last = stats.last_commit.or_else(|| (window.head_time > 0 && window.head_time <= anchor.time).then_some(window.head_time));
    let history = History {
        anchor: format_date(anchor.time),
        anchor_explicit: anchor.explicit,
        last_commit: last.map(format_date),
        dormant_months: last.filter(|t| anchor.time - t > DORMANT_DAYS * DAY).map(|t| ((anchor.time - t) / (30 * DAY)) as u32),
        shallow: !window.shallow.is_empty(),
        commits_365d: stats.commits,
        bot_commits_365d: stats.bot_commits,
    };
    conn.execute_batch("CREATE TABLE IF NOT EXISTS churn_words (path TEXT PRIMARY KEY, words TEXT NOT NULL) WITHOUT ROWID;")?;
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM churn", [])?;
    tx.execute("DELETE FROM churn_words", [])?;
    {
        let mut stmt = tx.prepare("INSERT INTO churn(path, data) VALUES (?1, ?2)")?;
        for (p, c) in &out {
            stmt.execute(params![p, serde_json::to_string(c)?])?;
        }
        let mut stmt = tx.prepare("INSERT INTO churn_words(path, words) VALUES (?1, ?2)")?;
        for (p, w) in &words {
            let text: Vec<String> = w.iter().map(|(t, n)| format!("{t}:{n}")).collect();
            stmt.execute(params![p, text.join(" ")])?;
        }
    }
    tx.commit()?;
    store.set_meta("churn.history", &serde_json::to_string(&history)?)?;
    store.set_meta("churn.cc_total", &stats.cc_commits.to_string())?;
    store.set_meta("churn.head", &stamp)?;
    Ok(Some(out))
}

/// The time frame of a walk.
struct Window {
    /// The walk's end: co-change weights halve per 90 days before it.
    head_time: i64,
    /// Co-change counts commits from this time on (365 days before `head_time` for the cached walk).
    cc_since: i64,
    /// Churn counts commits from the 365 days before this (and, when it is explicit, none after).
    anchor: Anchor,
    /// Shallow-clone boundary commits, left out: each "adds" every file it has.
    shallow: HashSet<String>,
    /// Automated commits, left out of churn and co-change.
    bots: Bots,
}

/// Repo-wide facts from a walk.
#[derive(Default)]
struct WalkStats {
    /// The newest commit at or before the anchor.
    last_commit: Option<i64>,
    /// Commits in the churn window, automated ones left out.
    commits: u32,
    /// Automated commits in the churn window.
    bot_commits: u32,
    /// Commits counted for co-change (of 1 to 50 files).
    cc_commits: u32,
}

/// Shared commits of a pair of files: count, recency-weighted count, and how many of them created both.
#[derive(Default)]
struct Pair {
    n: u32,
    weight: f64,
    created: u32,
}

/// Subject words per path (see [`subject_words`]).
type SubjectWords = HashMap<String, Vec<(String, u32)>>;

/// Folds `git log --numstat --summary --format=@@%H\t%ct\t%aE\t%s` output into per-path churn and subject words.
fn parse_log(log: &str, w: &Window) -> (HashMap<String, Churn>, WalkStats, SubjectWords) {
    let mut out: HashMap<String, Churn> = HashMap::new();
    let mut stats = WalkStats::default();
    let mut authors: HashMap<String, HashSet<String>> = HashMap::new();
    // Co-change: paths interned to ids, pairs keyed (low id, high id).
    let mut ids: HashMap<String, u32> = HashMap::new();
    let mut names: Vec<String> = Vec::new();
    let mut pairs: HashMap<(u32, u32), Pair> = HashMap::new();
    // Subject words per path id: word → commits.
    let mut said: HashMap<u32, HashMap<String, u32>> = HashMap::new();
    // The commit being read: its files, the paths it created, its co-change weight and its subject's words.
    let mut files: Vec<u32> = Vec::new();
    let mut created: HashSet<String> = HashSet::new();
    let mut weight = 1.0;
    let mut subject_terms: Vec<String> = Vec::new();
    let mut cc_total = 0u32;
    let mut flush = |files: &mut Vec<u32>, created: &mut HashSet<String>, weight: f64, terms: &[String], out: &mut HashMap<String, Churn>, names: &[String]| {
        files.sort_unstable();
        files.dedup();
        if !files.is_empty() && files.len() <= SUBJECT_MAX_FILES {
            for &f in files.iter() {
                let m = said.entry(f).or_default();
                for t in terms {
                    *m.entry(t.clone()).or_default() += 1;
                }
            }
        }
        if !files.is_empty() && files.len() <= COCHANGE_MAX_FILES {
            cc_total += 1;
            for (i, &a) in files.iter().enumerate() {
                out.get_mut(&names[a as usize]).unwrap().cc_commits += 1;
                let a_new = created.contains(&names[a as usize]);
                for &b in &files[i + 1..] {
                    let p = pairs.entry((a, b)).or_default();
                    p.n += 1;
                    p.weight += weight;
                    p.created += (a_new && created.contains(&names[b as usize])) as u32;
                }
            }
        }
        files.clear();
        created.clear();
    };
    let start = w.anchor.time - 365 * DAY;
    let (mut time, mut fix, mut author) = (0i64, false, String::new());
    // Whether the commit being read counts for churn, and for co-change.
    let (mut in_churn, mut in_cochange) = (false, false);
    // Renames seen so far (newer commits first): an older path → the path it has now.
    let mut renamed: HashMap<String, String> = HashMap::new();
    for line in log.lines() {
        if let Some(rest) = line.strip_prefix("@@") {
            flush(&mut files, &mut created, weight, &subject_terms, &mut out, &names);
            let mut parts = rest.splitn(5, '\t');
            let sha = parts.next().unwrap_or("");
            time = parts.next().and_then(|t| t.parse().ok()).unwrap_or(0);
            author = parts.next().unwrap_or("").to_ascii_lowercase();
            let name = parts.next().unwrap_or("");
            let subject = parts.next().unwrap_or("");
            fix = is_fix(subject);
            weight = 0.5f64.powf((w.head_time - time).max(0) as f64 / (COCHANGE_HALF_LIFE_DAYS * DAY as f64));
            let boundary = w.shallow.contains(sha);
            let after = w.anchor.explicit && time > w.anchor.time;
            let bot = w.bots.is_bot(name, &author, subject);
            in_churn = !boundary && !after && !bot && time >= start;
            in_cochange = !boundary && !bot && time >= w.cc_since && time <= w.head_time;
            subject_terms = if in_cochange { kc_store::locate::subject_terms(subject) } else { Vec::new() };
            if !boundary && !after {
                stats.last_commit = Some(stats.last_commit.map_or(time, |t| t.max(time)));
            }
            stats.commits += in_churn as u32;
            stats.bot_commits += (bot && !boundary && !after && time >= start) as u32;
            continue;
        }
        // `--summary`: ` create mode 100644 path` (the path as of that commit); other summary lines (` rename …`,
        // ` delete mode …`) add nothing to `--numstat`.
        if let Some(r) = line.strip_prefix(' ') {
            if let Some((_, path)) = r.strip_prefix("create mode ").and_then(|r| r.split_once(' '))
                && in_cochange
            {
                created.insert(renamed.get(path).cloned().unwrap_or_else(|| path.to_string()));
            }
            continue;
        }
        let mut parts = line.splitn(3, '\t');
        let (Some(add), Some(del), Some(raw)) = (parts.next(), parts.next(), parts.next()) else { continue };
        // A moved file's older commits count under the path it has now (newer renames were read first). Renames
        // are followed in every commit read, counted or not, so a move after the window still applies.
        let (path, moved) = match crate::ownership::split_rename(raw) {
            Some((old, new)) => {
                let now = renamed.get(&new).cloned().unwrap_or(new);
                renamed.insert(old, now.clone());
                (now, true)
            }
            None => (renamed.get(raw).cloned().unwrap_or_else(|| raw.to_string()), false),
        };
        if !in_churn && !in_cochange {
            continue;
        }
        let lines = add.parse::<u64>().unwrap_or(0) + del.parse::<u64>().unwrap_or(0);
        // A pure move edits nothing: it is neither churn nor a co-change.
        if moved && lines == 0 {
            continue;
        }
        let c = out.entry(path.clone()).or_default();
        if in_churn {
            let age = w.anchor.time - time;
            c.c365 += lines;
            c.commits += 1;
            if fix {
                c.fixes += 1;
                c.fix_score += fix_weight(((time - start) as f64 / (365 * DAY) as f64).clamp(0.0, 1.0));
            }
            if age <= 90 * DAY {
                c.c90 += lines;
            }
            if age <= 30 * DAY {
                c.c30 += lines;
                c.in30 = true;
            } else if age <= 90 * DAY {
                c.in90 = true;
            } else {
                c.in365_only = true;
            }
            if !author.is_empty() {
                authors.entry(path.to_string()).or_default().insert(author.clone());
            }
        }
        if in_cochange {
            let id = *ids.entry(path.to_string()).or_insert_with(|| {
                names.push(path.to_string());
                names.len() as u32 - 1
            });
            files.push(id);
        }
    }
    flush(&mut files, &mut created, weight, &subject_terms, &mut out, &names);
    stats.cc_commits = cc_total;

    let mut words: SubjectWords = HashMap::new();
    for (id, m) in said {
        let mut list: Vec<(String, u32)> = m.into_iter().collect();
        list.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        list.truncate(SUBJECT_WORDS_PER_FILE);
        if !list.is_empty() {
            words.insert(names[id as usize].clone(), list);
        }
    }
    for (p, a) in authors {
        if let Some(c) = out.get_mut(&p) {
            c.authors = a.len() as u32;
        }
    }
    let cc_commits: Vec<u32> = names.iter().map(|n| out.get(n).map_or(0, |c| c.cc_commits)).collect();
    let mut partners: HashMap<u32, Vec<(u32, u32, f64)>> = HashMap::new();
    for ((a, b), p) in pairs {
        // Created together and never changed together since: no evidence of coupling yet.
        if p.created < p.n {
            let w = (p.weight * 1000.0).round() / 1000.0;
            partners.entry(a).or_default().push((b, p.n, w));
            partners.entry(b).or_default().push((a, p.n, w));
        }
    }
    for (id, mut list) in partners {
        list.sort_by(|x, y| {
            y.2.partial_cmp(&x.2).unwrap().then(y.1.cmp(&x.1)).then_with(|| names[x.0 as usize].cmp(&names[y.0 as usize]))
        });
        // Every partner sharing two or more commits; single-commit ones only while there is room.
        let mut kept = 0;
        let before = list.len();
        list.retain(|p| {
            let keep = p.1 >= 2 || kept < MAX_PARTNERS;
            kept += keep as usize;
            keep
        });
        let dropped = (before - list.len()) as u32;
        if let Some(c) = out.get_mut(&names[id as usize]) {
            c.partners = list.into_iter().map(|(p, n, w)| (names[p as usize].clone(), n, w, cc_commits[p as usize])).collect();
            c.partners_dropped = dropped;
        }
    }
    (out, stats, words)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A walk with HEAD and the anchor both at `t`, no shallow boundary.
    fn win(t: i64) -> Window {
        Window { head_time: t, cc_since: t - 365 * DAY, anchor: Anchor { time: t, explicit: false }, shallow: HashSet::new(), bots: Bots::default() }
    }

    #[test]
    fn dates_round_trip() {
        assert_eq!(parse_date("1970-01-01"), Some(0));
        assert_eq!(parse_date("2026-10-13"), Some(20_739 * DAY));
        assert_eq!(format_date(20_739 * DAY + 3_600), "2026-10-13");
        assert_eq!(format_date(parse_date("2024-02-29").unwrap()), "2024-02-29");
        assert_eq!(parse_date("2026-13-01"), None);
        let a = Anchor::resolve(Some("head"), || 42).unwrap();
        assert_eq!(a, Anchor { time: 42, explicit: true });
        assert!(Anchor::resolve(Some("soon"), || 0).is_err());
    }

    #[test]
    fn fix_weight_favours_recent_fixes() {
        assert!((fix_weight(1.0) - 0.5).abs() < 1e-9);
        assert!(fix_weight(0.5) < 0.003);
        assert!(fix_weight(0.0) < 1e-5);
        let t = 1_000 * DAY;
        let log = format!(
            "@@h\t{}\tann@x\tdev\tfix: old\n\n1\t1\told.rs\n@@h\t{}\tann@x\tdev\tfix: new\n\n1\t1\tnew.rs\n",
            t - 300 * DAY,
            t - 5 * DAY
        );
        let c = parse_log(&log, &win(t)).0;
        assert_eq!((c["old.rs"].fixes, c["new.rs"].fixes), (1, 1));
        assert!(c["new.rs"].fix_score > 0.4 && c["old.rs"].fix_score < 0.01);
    }

    /// Windows end at the anchor, not at HEAD: a repo whose last commit is a year old has no recent churn, but its
    /// co-change (relative to HEAD) is unchanged.
    #[test]
    fn windows_end_at_the_anchor() {
        let head = 1_000 * DAY;
        let log = format!(
            "@@h\t{}\tann@x\tdev\tx\n\n1\t1\ta.rs\n1\t1\tb.rs\n@@h\t{}\tann@x\tdev\tx\n\n1\t1\ta.rs\n1\t1\tb.rs\n",
            head - DAY,
            head - 2 * DAY
        );
        let at_head = parse_log(&log, &win(head)).0;
        assert_eq!(at_head["a.rs"].c30, 4);
        let later = Window { head_time: head, cc_since: head - 365 * DAY, anchor: Anchor { time: head + 200 * DAY, explicit: false }, shallow: HashSet::new(), bots: Bots::default() };
        let (c, stats, _) = parse_log(&log, &later);
        assert_eq!((c["a.rs"].c30, c["a.rs"].c90, c["a.rs"].c365), (0, 0, 4));
        assert_eq!(stats.last_commit, Some(head - DAY));
        assert_eq!(c["a.rs"].partners, at_head["a.rs"].partners);
        // An explicit anchor before HEAD leaves out the commits after it.
        let before = Window { head_time: head, cc_since: head - 365 * DAY, anchor: Anchor { time: head - 2 * DAY, explicit: true }, shallow: HashSet::new(), bots: Bots::default() };
        let (c, stats, _) = parse_log(&log, &before);
        assert_eq!((c["a.rs"].commits, stats.commits), (1, 1));
    }

    #[test]
    fn shallow_boundary_commit_is_not_churn() {
        let t = 1_000 * DAY;
        let log = format!(
            "@@tip\t{}\tann@x\tdev\tfix: x\n\n1\t1\ta.rs\n@@root\t{}\tbob@x\tdev\tinitial\n\n900\t0\ta.rs\n900\t0\tb.rs\n",
            t - DAY,
            t - 2 * DAY
        );
        let w = Window { shallow: HashSet::from(["root".to_string()]), ..win(t) };
        let (c, stats, _) = parse_log(&log, &w);
        assert_eq!((c["a.rs"].c365, c["a.rs"].commits, c["a.rs"].authors), (2, 1, 1));
        assert!(!c.contains_key("b.rs"));
        assert_eq!(stats.commits, 1);
        assert_eq!(parse_log(&log, &win(t)).0["a.rs"].c365, 902, "without the graft it counts");
    }

    #[test]
    fn fix_words() {
        assert!(is_fix("Fix a bug in the parser"));
        assert!(is_fix("Revert \"Add x\""));
        assert!(is_fix("hotfix: crash"));
        assert!(!is_fix("Fix GPS fix handling"));
        assert!(!is_fix("Debug overlay"));
    }

    #[test]
    fn fix_conventions() {
        // Conventional Commits types decide.
        assert!(is_fix("fix: invalidate the cache after checkout"));
        assert!(is_fix("fix(grid)!: render rows after the upgrade"));
        assert!(!is_fix("feat: show a fixed footer"));
        assert!(!is_fix("test: add regression test for filters"));
        assert!(!is_fix("chore(deps): bump to fix advisory"));
        // Ticket prefixes and free text.
        assert!(is_fix("sc-4521: fix batch import crash on duplicate rows"));
        assert!(is_fix("[BUG] totals off by one"));
        assert!(is_fix("Fixed form field issues"));
        assert!(is_fix("Fix broken links"));
        assert!(is_fix("Correct inflated revenue for renewals"));
        // A fix aimed at tooling or text isn't a defect fix.
        assert!(!is_fix("fix tests"));
        assert!(!is_fix("lint fixes"));
        assert!(!is_fix("fix merge conflicts"));
        assert!(!is_fix("disable broken tests"));
        assert!(!is_fix("Fix typo in README"));
        // Bare resolve/correctly are too ambiguous (DI resolution, "works correctly").
        assert!(!is_fix("Resolve the decrypter immediately"));
        assert!(!is_fix("Save the update script correctly"));
        assert!(!is_fix("Add a prefix tree"));
    }

    #[test]
    fn walk_counts_authors_and_cochange() {
        let t = 1_000 * DAY;
        let mut log = String::new();
        // a.rs and b.rs change together three times, a.rs and d.rs once; one huge commit is ignored for co-change.
        for (i, author) in ["Ann@x", "bob@x", "ann@x"].iter().enumerate() {
            log += &format!("@@h\t{}\t{author}\tdev\tfix: thing {i}\n\n1\t1\ta.rs\n2\t0\tb.rs\n", t - i as i64 * DAY);
        }
        log += &format!("@@h\t{}\tcat@x\tdev\tadd c\n\n5\t0\ta.rs\n5\t0\td.rs\n", t - 40 * DAY);
        log += &format!("@@h\t{}\tcat@x\tdev\tformat everything\n\n", t - 50 * DAY);
        for i in 0..60 {
            log += &format!("1\t1\tf{i}.rs\n");
        }
        log += "1\t1\ta.rs\n1\t1\td.rs\n";
        let c = parse_log(&log, &win(t)).0;
        let a = &c["a.rs"];
        assert_eq!(a.commits, 5);
        assert_eq!(a.fixes, 3);
        assert_eq!(a.authors, 3, "emails are compared case-insensitively");
        assert_eq!(a.cc_commits, 4, "the 62-file commit is left out");
        let shared: Vec<(&str, u32)> = a.partners.iter().map(|(p, n, _, _)| (p.as_str(), *n)).collect();
        assert_eq!(shared, [("b.rs", 3), ("d.rs", 1)], "d.rs shares one small commit; the sweep doesn't count");
        assert_eq!(c["b.rs"].partners[0].0, "a.rs");
        assert_eq!(c["f0.rs"].cc_commits, 0);
        assert!(c["f0.rs"].partners.is_empty());
    }

    #[test]
    fn pairs_only_created_together_are_not_coupled() {
        let t = 1_000 * DAY;
        // new.rs and spec.rs are added in one commit and never change together again; a.rs and b.rs are added
        // together, then change together once more.
        let log = format!(
            "@@h\t{t}\tann@x\tdev\tadd\n\n1\t0\tnew.rs\n1\t0\tspec.rs\n1\t0\ta.rs\n1\t0\tb.rs\n \
             create mode 100644 new.rs\n create mode 100644 spec.rs\n create mode 100644 a.rs\n create mode 100644 b.rs\n\
             @@h\t{}\tann@x\tdev\ttweak\n\n1\t1\ta.rs\n1\t1\tb.rs\n1\t1\tnew.rs\n",
            t - DAY
        );
        let c = parse_log(&log, &win(t)).0;
        let names = |f: &str| -> Vec<(String, u32)> { c[f].partners.iter().map(|(p, n, _, _)| (p.clone(), *n)).collect() };
        assert_eq!(names("spec.rs"), Vec::<(String, u32)>::new(), "only ever created with the others");
        assert_eq!(names("a.rs"), [("b.rs".to_string(), 2), ("new.rs".to_string(), 2)]);
        assert_eq!(c["new.rs"].partners.len(), 2);
    }

    #[test]
    fn recent_shared_commits_rank_first() {
        let t = 1_000 * DAY;
        // a.rs changed with old.rs twice a year ago, and with new.rs once yesterday.
        let mut log = String::new();
        for d in [360, 350] {
            log += &format!("@@h\t{}\tann@x\tdev\tx\n\n1\t1\ta.rs\n1\t1\told.rs\n", t - d * DAY);
        }
        log += &format!("@@h\t{}\tann@x\tdev\tx\n\n1\t1\ta.rs\n1\t1\tnew.rs\n", t - DAY);
        let c = parse_log(&log, &win(t)).0;
        let order: Vec<&str> = c["a.rs"].partners.iter().map(|(p, _, _, _)| p.as_str()).collect();
        assert_eq!(order, ["new.rs", "old.rs"]);
    }

    #[test]
    fn bots_by_author_and_subject() {
        let b = Bots::default();
        assert!(b.is_bot("jenkins", "jenkins@corp.example", "Automated task via jenkins"));
        assert!(b.is_bot("renovate[bot]", "bot@renovateapp.com", "chore(deps): update x"));
        assert!(b.is_bot("CI Bot", "ci-bot@corp.example", "docs: auto-update README from package.json"));
        assert!(b.is_bot("Acme CI", "ci@corp.example", "release"));
        assert!(b.is_bot("Ann Lee (via CI)", "ann@corp.example", "ci: add a config"));
        assert!(b.is_bot("GitHub", "noreply@github.com", "Update README.md"));
        assert!(b.is_bot("Ann Lee", "ann@corp.example", "Automated update of schema dumps"));
        assert!(b.is_bot("Ann Lee", "ann@corp.example", "chore: auto-generated client"));
        // People, including ones with a noreply address, and words that only contain a bot word.
        assert!(!b.is_bot("Ann Lee", "ann@corp.example", "fix: build the cart total"));
        assert!(!b.is_bot("Jack", "30406998-jack@users.noreply.gitlab.com", "add x"));
        assert!(!b.is_bot("Abbot Cinder", "abbot@corp.example", "Automated tests for the cart"));
        // The user's file adds and exempts.
        let b = Bots::parse("# ours\nrelease-train@corp.example\n!Ann Lee (via CI)\n");
        assert!(b.is_bot("Release Train", "release-train@corp.example", "bump"));
        assert!(!b.is_bot("Ann Lee (via CI)", "ann@corp.example", "ci: add a config"));
    }

    /// A nightly job's commits of many files couple nothing and add no churn or authors; people's commits still do.
    #[test]
    fn automated_commits_are_left_out() {
        let t = 1_000 * DAY;
        let mut log = String::new();
        for d in 1..=5 {
            log += &format!("@@h\t{}\tjenkins@x\tjenkins\tAutomated task via jenkins\n\n1\t1\ta.sql\n1\t1\tb.sql\n1\t1\td.sql\n", t - d * DAY);
        }
        log += &format!("@@h\t{}\tann@x\tAnn\tadd an index\n\n3\t0\ta.sql\n1\t0\tb.sql\n", t - 9 * DAY);
        let (c, stats, _) = parse_log(&log, &win(t));
        assert_eq!((stats.commits, stats.bot_commits), (1, 5));
        assert_eq!((c["a.sql"].commits, c["a.sql"].c365, c["a.sql"].authors, c["a.sql"].cc_commits), (1, 3, 1, 1));
        assert!(!c.contains_key("d.sql"), "only the job touched it");
        assert_eq!(c["a.sql"].partners.iter().map(|(p, n, _, _)| (p.as_str(), *n)).collect::<Vec<_>>(), [("b.sql", 1)]);
    }

    /// A file moved from src/ to tests/ keeps its history and partners under its new path; the move itself is
    /// neither churn nor a co-change.
    #[test]
    fn renames_are_followed() {
        let t = 1_000 * DAY;
        let mut log = format!(
            "@@h\t{}\tann@x\tAnn\tmove the store\n\n0\t0\t{{src => tests}}/Store.cs\n0\t0\t{{src => tests}}/StoreTests.cs\n \
             rename {{src => tests}}/Store.cs (100%)\n",
            t - DAY
        );
        log += &format!("@@h\t{}\tann@x\tAnn\tedit and move\n\n2\t1\tOld.cs => src/Graph.cs\n", t - 2 * DAY);
        for d in 3..6 {
            log += &format!("@@h\t{}\tann@x\tAnn\tx\n\n1\t1\tsrc/Store.cs\n1\t1\tOld.cs\n", t - d * DAY);
        }
        let c = parse_log(&log, &win(t)).0;
        let s = &c["tests/Store.cs"];
        assert_eq!((s.commits, s.c365, s.cc_commits), (3, 6, 3), "the move adds nothing");
        assert_eq!(s.partners.iter().map(|(p, n, _, _)| (p.as_str(), *n)).collect::<Vec<_>>(), [("src/Graph.cs", 3)]);
        assert_eq!((c["src/Graph.cs"].commits, c["src/Graph.cs"].c365), (4, 9), "an edit with the move counts");
        assert!(!c.contains_key("src/Store.cs") && !c.contains_key("Old.cs"));
        assert!(!c.contains_key("tests/StoreTests.cs"), "a pure move alone isn't a change");
    }

    #[test]
    fn hubs_are_weak_from_both_sides() {
        // A changelog in 72 of a repo's 100 commits, with 3 of its file's 20: a hub, left out.
        assert!(is_hub_partner(3, 72, 20, 100));
        // The same changelog in all 7 of a script's commits: a partner, though under a fifth of its own.
        assert!(!is_hub_partner(7, 72, 7, 100));
        // Half the file's commits is enough; just under isn't.
        assert!(!is_hub_partner(5, 72, 10, 100));
        assert!(is_hub_partner(4, 72, 9, 100));
        // Not a hub at all: few commits, or a small share of the repo's, or a fifth of its own with the file.
        assert!(!is_hub_partner(1, 9, 20, 10));
        assert!(!is_hub_partner(1, 40, 20, 100));
        assert!(!is_hub_partner(15, 72, 40, 100));
    }

    #[test]
    fn partner_kinds() {
        assert_eq!(partner_kind("src/Orders.cs", Some("csharp")), Some("csharp"));
        assert_eq!(partner_kind("src/app/a.component.html", Some("html")), Some("template"));
        assert_eq!(partner_kind("src/app/a.component.scss", None), Some("style"));
        assert_eq!(partner_kind("site/js/cart.js", None), Some("javascript"));
        assert_eq!(partner_kind("Views/Home/Index.cshtml", None), Some("template"));
        // Any other file is a partner too, by its kind; built output isn't.
        for (p, k) in [
            ("app.csproj", "config"),
            ("README.md", "docs"),
            ("deploy/Dockerfile", "infra"),
            ("infra/main.tf", "infra"),
            ("scripts/release.sh", "script"),
            (".env.production", "config"),
            ("logo.png", "other"),
        ] {
            assert_eq!(partner_kind(p, None), Some(k), "{p}");
        }
        assert_eq!(partner_kind("app.csproj", Some("csproj")), Some("config"));
        for p in ["js/app.min.js", "dist/main.js", "static/app.bundle.js", "package-lock.json", "yarn.lock", "web/pnpm-lock.yaml", "infra/.terraform.lock.hcl"] {
            assert_eq!(partner_kind(p, None), None, "{p}");
        }
    }

    fn vitals(file: &str, churn: u64, nesting: u32, longest: u32) -> FileVitals {
        FileVitals {
            file: file.into(),
            risk: 0,
            health: 10,
            concern: 0.0,
            churn_30d: 0,
            churn_90d: 0,
            churn_365d: churn,
            weighted_churn: churn,
            recurring: false,
            commits_365d: 1,
            fix_commits_365d: 0,
            fix_ratio: 0.0,
            fix_score: 0.0,
            authors_365d: 1,
            complexity: 1,
            max_nesting: nesting,
            functions: 1,
            longest_function: longest,
            longest_function_name: None,
            fan_in: 0,
            fan_out: 0,
            co_change: None,
            coupled_files: 0,
            biomarkers: BTreeMap::new(),
        }
    }

    #[test]
    fn nesting_and_length_raise_concern() {
        let flat = vitals("flat.rs", 100, 1, 10);
        let deep = vitals("deep.rs", 100, 5, 10);
        let long = vitals("long.rs", 100, 1, 300);
        let c = concern(&[&flat, &deep, &long]);
        assert!(c[1] > c[0] && c[2] > c[0]);
    }

    #[test]
    fn recent_fixes_raise_concern() {
        let mut old = vitals("old.rs", 0, 0, 0);
        old.fix_commits_365d = 3;
        old.fix_score = 3.0 * fix_weight(0.2);
        let mut recent = vitals("recent.rs", 0, 0, 0);
        recent.fix_commits_365d = 1;
        recent.fix_score = fix_weight(0.98);
        let c = concern(&[&old, &recent, &vitals("none.rs", 0, 0, 0)]);
        assert!(c[1] > c[0], "one fix last week outranks three most of a year ago");
    }
}
