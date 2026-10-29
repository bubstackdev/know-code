//! `rules`: a repo's own ast-grep rules (see [`kc_index::rules`]) run over its files, and reported. With a base
//! (`--new-since <ref>`) the rules are a monitor: only the matches the base didn't have, read from the files that
//! differ from it. Nothing is stored between runs, since the base ref is the monitor's whole state, and nothing is
//! written: rule files are read where the user keeps them, and `fix` is refused.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use kc_index::rules::{self, RuleHit, RuleSet};
use kc_index::search::{self, ScanFile};
use kc_store::answer::{Code, Incomplete, Reason, Refusal};
use kc_store::content::{INDEXED, PENDING, SKIPPED};
use serde_json::{Value, json};

use crate::contract::{finish, push_incomplete};
use crate::ops::{self, Out, Scope};

pub struct RulesParams {
    /// Rule files, or directories of them (their `.yml` and `.yaml` files, recursively). Relative paths are taken
    /// from each repo's root. Empty: the config's `[rules] paths`.
    pub paths: Vec<PathBuf>,
    /// Rules written inline: YAML documents, as in a rule file.
    pub inline: Option<String>,
    /// A line regex instead of rules: every line it matches is a match (rule id `regex`), as `search -e` matches.
    pub regex: Option<String>,
    /// The monitor's base: report only the matches it didn't have.
    pub new_since: Option<String>,
    /// Path globs; a leading `!` excludes.
    pub globs: Vec<String>,
    pub limit: usize,
    pub offset: usize,
    /// Stop collecting after this many matches.
    pub max_matches: usize,
}

pub struct RulesData {
    pub hits: Vec<RuleHit>,
    pub total: usize,
    pub multi: bool,
    pub roots: Vec<PathBuf>,
    pub next_offset: Option<usize>,
}

/// The largest rule file read.
const MAX_RULE_FILE: u64 = 2 << 20;

/// The rule files a path names: itself, or a directory's `.yml` and `.yaml` files, recursively and in name order.
fn rule_files(path: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let meta = std::fs::metadata(path).with_context(|| format!("rule file {}", path.display()))?;
    if !meta.is_dir() {
        out.push(path.to_path_buf());
        return Ok(());
    }
    let mut entries: Vec<PathBuf> = std::fs::read_dir(path)?.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            rule_files(&p, out)?;
        } else if p.extension().is_some_and(|e| e == "yml" || e == "yaml") {
            out.push(p);
        }
    }
    Ok(())
}

/// Reads the rules for the repo at `root`: the paths given (or the config's), and the inline text. A rule file in a
/// part of the repo its privacy file blocks is refused rather than read.
fn load_rules(root: &Path, p: &RulesParams) -> Result<RuleSet> {
    let configured;
    let paths = if p.paths.is_empty() && p.inline.is_none() {
        configured = kc_index::config::load()?.config.rules.clone();
        &configured
    } else {
        &p.paths
    };
    if paths.is_empty() && p.inline.is_none() {
        bail!("no rules: name rule files (--rules PATH) or set `[rules] paths` in the config (knowcode help rules)");
    }
    let privacy = kc_index::privacy::Privacy::load(root);
    let mut files = Vec::new();
    for path in paths {
        let full = if path.is_absolute() { path.clone() } else { root.join(path) };
        let mut found = Vec::new();
        rule_files(&full, &mut found)?;
        for f in found {
            if let Ok(rel) = f.strip_prefix(root)
                && privacy.blocks(&rel.to_string_lossy())
            {
                bail!("withheld: rule file {} is in a part of {} its privacy file blocks", rel.display(), ops::repo_name(root));
            }
            // As for every YAML file KnowCode reads: a rule file past 2 MiB isn't read.
            if std::fs::metadata(&f).map(|m| m.len()).unwrap_or(0) > MAX_RULE_FILE {
                bail!("rule file {} is over 2 MiB; it isn't read", f.display());
            }
            let text = std::fs::read_to_string(&f).with_context(|| format!("reading rule file {}", f.display()))?;
            let name = f.strip_prefix(root).unwrap_or(&f).display().to_string();
            files.push((name, text));
        }
    }
    if let Some(text) = &p.inline {
        files.push(("inline rules".to_string(), text.clone()));
    }
    rules::load(&files)
}

/// Whether a path passes the globs (a leading `!` excludes), as in `search`.
fn globbed(globs: &[String], path: &str) -> bool {
    let (exclude, include): (Vec<&String>, Vec<&String>) = globs.iter().filter(|g| !g.trim().is_empty()).partition(|g| g.starts_with('!'));
    !exclude.iter().any(|g| search::path_glob(&g[1..], path)) && (include.is_empty() || include.iter().any(|g| search::path_glob(g, path)))
}

/// What a run matches: rules, or a line regex.
enum Matcher {
    Rules(RuleSet),
    Lines(regex::Regex),
}

impl Matcher {
    fn applies(&self, path: &str) -> bool {
        match self {
            Matcher::Rules(set) => set.applies(path),
            Matcher::Lines(_) => true,
        }
    }

    /// The matches in one file's source. A line regex's match is its whole line (without the line break), so a
    /// monitor compares lines as `git diff` does.
    fn matches(&self, path: &str, src: &str) -> Vec<RuleHit> {
        let re = match self {
            Matcher::Rules(set) => return rules::match_source(set, 0, path, src),
            Matcher::Lines(re) => re,
        };
        let mut out = Vec::new();
        let mut start = 0;
        for (i, line) in src.split('\n').enumerate() {
            let end = start + line.len();
            if let Some(m) = re.find(line) {
                out.push(RuleHit {
                    repo: 0,
                    rule: "regex".to_string(),
                    severity: "info",
                    file: path.to_string(),
                    line: i as u32 + 1,
                    column: m.start() as u32 + 1,
                    end_line: i as u32 + 1,
                    end_column: m.end() as u32 + 1,
                    start,
                    end,
                    message: format!("line matches {}", re.as_str()),
                    text: line.trim().chars().take(200).collect(),
                    matched: line.to_string(),
                });
            }
            start = end + 1;
        }
        out
    }
}

fn matcher(root: &Path, p: &RulesParams) -> Result<Matcher> {
    match &p.regex {
        Some(re) if p.paths.is_empty() && p.inline.is_none() => {
            Ok(Matcher::Lines(regex::Regex::new(re).map_err(|e| anyhow!("invalid regex: {e}"))?))
        }
        Some(_) => bail!("a line regex runs alone: give it without rule files"),
        None => Ok(Matcher::Rules(load_rules(root, p)?)),
    }
}

/// Parses and matches `files` in parallel, sorted by path and position; at `max` the scan stops at the same file
/// whatever the thread timing.
fn scan(root: &Path, files: &[ScanFile], m: &Matcher, threads: usize, max: usize) -> (Vec<RuleHit>, rules::RuleScanStats) {
    let work = |data: &mut Vec<u8>, f: &ScanFile| -> Option<(usize, Vec<RuleHit>)> {
        if !search::readable(search::read_into(&root.join(&f.path), data))? {
            return Some((0, Vec::new()));
        }
        Some((data.len(), m.matches(&f.path, &String::from_utf8_lossy(data))))
    };
    let p = search::scan_prefix(files, threads, max, Vec::new, work);
    let mut hits: Vec<RuleHit> = p.files.into_iter().flat_map(|(_, _, h)| h).collect();
    hits.sort_by(|a, b| (&a.file, a.start, a.end, &a.rule).cmp(&(&b.file, b.start, b.end, &b.rule)));
    let failed = p.failed.iter().map(|&k| (0, files[k].path.clone())).collect();
    (hits, rules::RuleScanStats { files_parsed: p.files_read, bytes_read: p.bytes_read, capped: p.capped, failed })
}

/// One repo's part of the answer: its matches (new ones only, for a monitor), the files read, and the base resolved.
struct RepoPart {
    opened: ops::Opened,
    hits: Vec<RuleHit>,
    files: usize,
    base: Option<String>,
    stats: rules::RuleScanStats,
}

fn run_repo(root: &Path, p: &RulesParams, args: &Scope, threads: usize) -> Result<RepoPart> {
    // A rule that can't be read or is refused is the caller's to fix: `invalid_argument`, or `withheld` for a rule
    // file the privacy file blocks.
    let set = matcher(root, p).map_err(|e| {
        let message = format!("{e:#}");
        let code = if message.starts_with("withheld:") { Code::Withheld } else { Code::InvalidArgument };
        anyhow::Error::new(Refusal::new(code, message))
    })?;
    let mut o = ops::open_fresh(root, args.no_sync)?;
    let files = if args.no_sync { o.store.cfiles()? } else { kc_index::content::sync_content_files(root, &mut o.store, threads)?.1 };
    // Every file the content index lists, but binary ones: unlike `search`, rules read generated and large files
    // too, as the ast-grep command line does.
    let listed: Vec<String> = files
        .into_iter()
        .filter(|f| matches!(f.state, INDEXED | PENDING | SKIPPED) && f.skip.as_deref() != Some("binary"))
        .map(|f| f.path)
        .filter(|path| set.applies(path) && globbed(&p.globs, path))
        .collect();
    let Some(base) = &p.new_since else {
        let files: Vec<ScanFile> = listed.into_iter().map(|path| ScanFile { repo: 0, path }).collect();
        let (hits, stats) = scan(root, &files, &set, threads, p.max_matches);
        let n = files.len();
        return Ok(RepoPart { opened: o, hits, files: n, base: None, stats });
    };
    // A monitor reads only the files that differ from the base, in both versions: every other file's matches are
    // the base's too.
    let (base, changed) = rules::changed_since(root, base)?;
    let listed: std::collections::HashSet<&str> = listed.iter().map(String::as_str).collect();
    let now_files: Vec<&str> = changed.iter().filter_map(|c| c.now.as_deref()).filter(|f| listed.contains(f)).collect();
    let was: Vec<&rules::BaseFile> = changed.iter().filter(|c| c.was.as_ref().is_some_and(|(old, _)| set.applies(old) && globbed(&p.globs, old))).collect();
    let mut stats = rules::RuleScanStats::default();
    let mut now = Vec::new();
    for f in &now_files {
        let mut data = Vec::new();
        match search::readable(search::read_into(&root.join(f), &mut data)) {
            Some(true) => {}
            Some(false) => continue,
            None => {
                stats.failed.push((0, f.to_string()));
                continue;
            }
        }
        stats.files_parsed += 1;
        stats.bytes_read += data.len() as u64;
        now.extend(set.matches(f, &String::from_utf8_lossy(&data)));
    }
    let mut before = Vec::new();
    for (old, src) in rules::base_sources(root, &was)? {
        before.extend(set.matches(&old, &src));
    }
    let mut hits = rules::new_matches(now, &before);
    if hits.len() > p.max_matches {
        hits.truncate(p.max_matches);
        stats.capped = true;
    }
    hits.sort_by(|a, b| (&a.file, a.start, a.end, &a.rule).cmp(&(&b.file, b.start, b.end, &b.rule)));
    Ok(RepoPart { opened: o, hits, files: now_files.len(), base: Some(base), stats })
}

pub fn rules(p: &RulesParams, args: &Scope) -> Result<Out<RulesData>> {
    let all = ops::target_repos(&args.repos)?;
    let threads = kc_index::SyncOptions::default().threads;
    let mut parts = Vec::new();
    for root in &all {
        match run_repo(root, p, args, threads) {
            Ok(part) => parts.push(part),
            Err(e) => ops::left_out(&all, root, e)?,
        }
    }
    if parts.is_empty() {
        return Err(anyhow!("no repo could be read"));
    }
    let names: Vec<String> = parts.iter().map(|r| ops::repo_name(&r.opened.root)).collect();
    let multi = parts.len() > 1;
    let mut hits: Vec<RuleHit> = Vec::new();
    let (mut files, mut parsed, mut bytes, mut capped, mut failed) = (0, 0, 0u64, false, Vec::new());
    let mut bases = Vec::new();
    for (k, part) in parts.iter_mut().enumerate() {
        files += part.files;
        parsed += part.stats.files_parsed;
        bytes += part.stats.bytes_read;
        capped |= part.stats.capped;
        failed.extend(part.stats.failed.iter().map(|(_, f)| (k, f.clone())));
        if let Some(b) = &part.base {
            bases.push(json!({ "repo": names[k], "commit": b }));
        }
        hits.extend(part.hits.drain(..).map(|mut h| {
            h.repo = k;
            h
        }));
    }
    let total = hits.len();
    let mut by_rule: BTreeMap<&str, usize> = BTreeMap::new();
    for h in &hits {
        *by_rule.entry(h.rule.as_str()).or_default() += 1;
    }
    let by_rule: Value = by_rule.into_iter().map(|(k, v)| (k.to_string(), json!(v))).collect::<serde_json::Map<_, _>>().into();
    let page: Vec<RuleHit> = hits.into_iter().skip(p.offset).take(p.limit).collect();
    let list: Vec<Value> = page
        .iter()
        .map(|h| {
            let mut v = serde_json::to_value(h).unwrap_or(Value::Null);
            if multi {
                v["repo"] = json!(names[h.repo]);
            }
            v
        })
        .collect();
    let shown = list.len();
    let next_offset = (p.offset + shown < total).then_some(p.offset + shown);
    let opened: Vec<ops::Opened> = parts.into_iter().map(|r| r.opened).collect();
    let mut env = ops::envelope(&opened, Value::Array(list), total, shown);
    env["offset"] = json!(p.offset);
    env["next_offset"] = json!(next_offset);
    env["by_rule"] = by_rule;
    if !bases.is_empty() {
        env["new_since"] = json!(bases);
    }
    env["stats"] = json!({ "files": files, "files_parsed": parsed, "bytes_read": bytes });
    if capped {
        push_incomplete(
            &mut env,
            Incomplete::new(Reason::Capped, format!("rules: the scan stopped at {} matches; later files were not read", p.max_matches))
                .with_hint(format!("narrow the paths, or raise the cap (--max-matches {})", p.max_matches.saturating_mul(4))),
        );
    }
    let roots: Vec<PathBuf> = opened.iter().map(|o| o.root.clone()).collect();
    for (k, f) in &failed {
        push_incomplete(
            &mut env,
            Incomplete::new(Reason::Failed, format!("{}: could not read {f}; its matches are missing", names[*k])).with_hint("fix the file's permissions, then run the rules again"),
        );
    }
    finish(&mut env);
    Ok(Out { env, data: RulesData { hits: page, total, multi, roots, next_offset } })
}

/// The text form: the number of matches, then a line per match.
pub fn render(d: &RulesData) {
    println!("{} matches", d.total);
    for h in &d.hits {
        let file = if d.multi { format!("{}/{}", ops::repo_name(&d.roots[h.repo]), h.file) } else { h.file.clone() };
        println!("{file}:{}:{}: {} [{}] {}  | {}", h.line, h.column, h.severity, h.rule, h.message, h.text);
    }
    if let Some(next) = d.next_offset {
        println!("… more: --offset {next}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::tests::{commit_and_tag, repo, test_cache};

    const RULE: &str = "id: no-result\nlanguage: CSharp\nseverity: warning\nmessage: blocking on $T\nrule:\n  pattern: $T.Result\n";

    fn params(new_since: Option<&str>) -> RulesParams {
        RulesParams {
            paths: vec![PathBuf::from("lint")],
            inline: None,
            regex: None,
            new_since: new_since.map(Into::into),
            globs: Vec::new(),
            limit: 50,
            offset: 0,
            max_matches: 1000,
        }
    }

    fn rows(env: &Value) -> Vec<(String, u64, String)> {
        env["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| (i["file"].as_str().unwrap().to_string(), i["line"].as_u64().unwrap(), i["message"].as_str().unwrap().to_string()))
            .collect()
    }

    #[test]
    fn rules_report_matches_and_a_monitor_only_new_ones() {
        let cache = test_cache();
        let base = cache.dir("rules");
        let a = "class A\n{\n    int F() => G().Result;\n}\n";
        let root = repo(&base, "shop", &[("lint/no-result.yml", RULE), ("src/A.cs", a), ("src/B.cs", "class B { }\n")]);
        commit_and_tag(&root, "v1");
        let scope = Scope { repos: vec![root.clone()], no_sync: false };
        let out = rules(&params(None), &scope).unwrap();
        assert_eq!(rows(&out.env), [("src/A.cs".to_string(), 3, "blocking on G()".to_string())]);
        assert_eq!(out.env["by_rule"]["no-result"], 1);

        // Moved within its file and joined by a new one elsewhere, uncommitted: only the new one is reported.
        std::fs::write(root.join("src/A.cs"), "class A\n{\n    void H() { }\n    int F() => G().Result;\n}\n").unwrap();
        std::fs::write(root.join("src/B.cs"), "class B\n{\n    int F() => task.Result;\n}\n").unwrap();
        std::fs::write(root.join("src/C.cs"), "class C\n{\n    int F() => G().Result;\n}\n").unwrap();
        let out = rules(&params(Some("v1")), &scope).unwrap();
        assert_eq!(rows(&out.env), [("src/B.cs".to_string(), 3, "blocking on task".to_string()), ("src/C.cs".to_string(), 3, "blocking on G()".to_string())]);
        assert_eq!(out.env["new_since"][0]["commit"].as_str().map(str::len), Some(40));
        // Renamed: nothing new.
        commit_and_tag(&root, "v2");
        std::fs::rename(root.join("src/C.cs"), root.join("src/D.cs")).unwrap();
        let out = rules(&params(Some("v2")), &scope).unwrap();
        assert_eq!(rows(&out.env), []);
        assert_eq!(out.env["complete"], true);
    }

    #[test]
    fn a_line_regex_monitor_reports_lines_the_base_lacked() {
        let cache = test_cache();
        let base = cache.dir("rules-regex");
        let root = repo(&base, "shop", &[("src/a.ts", "// TODO: one\nf();\n"), ("src/b.ts", "g();\n")]);
        commit_and_tag(&root, "v1");
        let scope = Scope { repos: vec![root.clone()], no_sync: false };
        let p = |new_since: Option<&str>| RulesParams { paths: Vec::new(), regex: Some("TODO".into()), ..params(new_since) };
        assert_eq!(rows(&rules(&p(None), &scope).unwrap().env), [("src/a.ts".to_string(), 1, "line matches TODO".to_string())]);
        // The line moved to another file: not new. One more copy of it, and a new one: new.
        std::fs::write(root.join("src/a.ts"), "f();\n").unwrap();
        std::fs::write(root.join("src/b.ts"), "// TODO: one\ng();\n// TODO: one\n// TODO: two\n").unwrap();
        let got = rows(&rules(&p(Some("v1")), &scope).unwrap().env);
        assert_eq!(got.iter().map(|r| (r.0.as_str(), r.1)).collect::<Vec<_>>(), [("src/b.ts", 3), ("src/b.ts", 4)]);
    }

    #[test]
    fn a_rule_with_a_fix_or_no_rules_at_all_is_an_error() {
        let cache = test_cache();
        let base = cache.dir("rules-refused");
        let root = repo(&base, "shop", &[("lint/fix.yml", &format!("{RULE}fix: $T.GetAwaiter().GetResult()\n")), ("src/A.cs", "class A { }\n")]);
        let scope = Scope { repos: vec![root.clone()], no_sync: false };
        let err = rules(&params(None), &scope).err().map(|e| format!("{e:#}")).unwrap_or_default();
        assert!(err.contains("`fix` is refused"), "{err}");
        let none = RulesParams { paths: Vec::new(), ..params(None) };
        let err = rules(&none, &scope).err().map(|e| format!("{e:#}")).unwrap_or_default();
        assert!(err.contains("no rules"), "{err}");
    }
}
