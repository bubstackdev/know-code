//! Rules: ast-grep's rule YAML, run over the files of a repo and reported, never applied.
//!
//! Rule files are read with ast-grep-config, the crate the ast-grep command line reads them with (vendored in
//! `crates/ast-grep-config` with its YAML read by yaml-rust2), over the same ast-grep-core: a rule's `stopBy`
//! defaults, kind selectors, utilities, constraints and suppression comments mean here what they mean there, and a
//! rule's matches are the command line's on the same files. Four things are
//! refused rather than ignored: `fix` (KnowCode only reports), languages KnowCode has no grammar for, JavaScript
//! (KnowCode parses it with the TSX grammar, whose node kinds differ from ast-grep's JavaScript grammar) and Kotlin
//! (KnowCode's Kotlin grammar isn't ast-grep's).

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};
use ast_grep_config::{CombinedScan, GlobalRules, RuleCollection, RuleConfig, Severity, from_yaml_string};
use ast_grep_core::matcher::{PatternBuilder, PatternError};
use ast_grep_core::tree_sitter::{LanguageExt, StrDoc, TSLanguage};
use ast_grep_core::{AstGrep, Language, Pattern};
use kc_extract::Lang;
use serde::{Deserialize, Deserializer, Serialize};

use crate::search::{self, ScanFile};
use crate::structural::Grammar;

/// A rule's `language`, one of the grammars KnowCode parses with, named as ast-grep names it.
#[derive(Clone)]
pub struct RuleLang {
    lang: Lang,
    grammar: Grammar,
}

impl PartialEq for RuleLang {
    fn eq(&self, other: &Self) -> bool {
        self.lang == other.lang
    }
}
impl Eq for RuleLang {}

/// ast-grep's names for each language (matched without case), and the extensions its command line reads as it.
const LANGS: &[(Lang, &[&str], &[&str])] = &[
    (Lang::CSharp, &["cs", "csharp"], &["cs"]),
    (Lang::Python, &["py", "python"], &["py", "py3", "pyi", "bzl", "bazel"]),
    (Lang::Rust, &["rs", "rust"], &["rs"]),
    (Lang::Swift, &["swift"], &["swift"]),
    (Lang::TypeScript, &["ts", "typescript"], &["ts", "cts", "mts"]),
    (Lang::Tsx, &["tsx"], &["tsx"]),
    (Lang::Java, &["java"], &["java"]),
    (Lang::Go, &["go", "golang"], &["go"]),
];

impl RuleLang {
    fn of(lang: Lang) -> Option<RuleLang> {
        Some(RuleLang { lang, grammar: Grammar::new(lang)? })
    }

    fn named(name: &str) -> Result<RuleLang> {
        if ["javascript", "js", "jsx"].iter().any(|a| name.eq_ignore_ascii_case(a)) {
            bail!(
                "JavaScript rules aren't supported: KnowCode parses JavaScript with the TSX grammar, whose node kinds differ from \
                 ast-grep's JavaScript grammar, so matches could differ from the ast-grep command line's"
            );
        }
        if ["kotlin", "kt"].iter().any(|a| name.eq_ignore_ascii_case(a)) {
            bail!(
                "Kotlin rules aren't supported: KnowCode parses Kotlin with tree-sitter-kotlin-ng, ast-grep with its own \
                 Kotlin grammar, whose node kinds differ, so matches could differ from the ast-grep command line's"
            );
        }
        let (lang, ..) = LANGS
            .iter()
            .find(|(_, names, _)| names.iter().any(|a| name.eq_ignore_ascii_case(a)))
            .ok_or_else(|| anyhow!("rules cover CSharp, TypeScript, Tsx, Python, Rust, Swift, Java and Go, not {name}"))?;
        RuleLang::of(*lang).ok_or_else(|| anyhow!("no grammar for {name}"))
    }
}

impl<'de> Deserialize<'de> for RuleLang {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let name = String::deserialize(d)?;
        RuleLang::named(&name).map_err(|e| serde::de::Error::custom(format!("{e:#}")))
    }
}

impl Language for RuleLang {
    fn pre_process_pattern<'q>(&self, query: &'q str) -> std::borrow::Cow<'q, str> {
        self.grammar.pre_process_pattern(query)
    }
    fn expando_char(&self) -> char {
        self.grammar.expando_char()
    }
    fn from_path<P: AsRef<Path>>(path: P) -> Option<Self> {
        let ext = path.as_ref().extension()?.to_str()?;
        let (lang, ..) = LANGS.iter().find(|(_, _, exts)| exts.contains(&ext))?;
        RuleLang::of(*lang)
    }
    fn kind_to_id(&self, kind: &str) -> u16 {
        self.grammar.kind_to_id(kind)
    }
    fn field_to_id(&self, field: &str) -> Option<u16> {
        self.grammar.field_to_id(field)
    }
    fn build_pattern(&self, builder: &PatternBuilder) -> std::result::Result<Pattern, PatternError> {
        builder.build(|src| StrDoc::try_new(src, self.clone()))
    }
}

impl LanguageExt for RuleLang {
    fn get_ts_language(&self) -> TSLanguage {
        self.grammar.get_ts_language()
    }
}

/// The rules of one or more rule files.
pub struct RuleSet {
    rules: RuleCollection<RuleLang>,
    /// Each rule's id and the file it came from, in load order.
    pub ids: Vec<(String, String)>,
}

/// Reads rule files: each holds one or more YAML documents, each document one rule. `files` are (name shown in
/// errors, text). A rule file can come from the repo under review, so it is hostile input: every text first passes
/// the limits every YAML file KnowCode reads keeps to ([`kc_extract::check_yaml`]: 2 MiB, and aliases standing for
/// at most 100,000 nodes), so ast-grep-config's parser never sees a text that would expand past them.
pub fn load(files: &[(String, String)]) -> Result<RuleSet> {
    let globals = GlobalRules::default();
    let mut all: Vec<RuleConfig<RuleLang>> = Vec::new();
    let mut ids = Vec::new();
    for (name, text) in files {
        kc_extract::check_yaml(text).map_err(|e| anyhow!("{name}: not read: {e}"))?;
        let rules = from_yaml_string::<RuleLang>(text, &globals).map_err(|e| anyhow!("{name}: {}", chain(&e)))?;
        for r in rules {
            if r.id.is_empty() {
                bail!("{name}: a rule has no `id`");
            }
            if r.fix.is_some() {
                bail!("{name}: rule `{}` has a `fix`: KnowCode reports matches and never edits files, so `fix` is refused", r.id);
            }
            if let Some((_, first)) = ids.iter().find(|(id, _)| *id == r.id) {
                bail!("{name}: rule id `{}` is also defined in {first}", r.id);
            }
            ids.push((r.id.clone(), name.clone()));
            all.push(r);
        }
    }
    let rules = RuleCollection::try_new(all).map_err(|e| anyhow!("rule `files` or `ignores`: {e}"))?;
    Ok(RuleSet { rules, ids })
}

/// An error and its causes, on one line: ast-grep-config's top-level messages ("`rule` is not configured
/// correctly.") say where, their sources say what.
fn chain(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut src = e.source();
    while let Some(s) = src {
        out.push_str(": ");
        out.push_str(&s.to_string());
        src = s.source();
    }
    out
}

impl RuleSet {
    /// Whether any rule applies to a file at this repo-relative path.
    pub fn applies(&self, path: &str) -> bool {
        !self.rules.for_path(path).is_empty()
    }
}

/// One rule match.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RuleHit {
    #[serde(skip)]
    pub repo: usize,
    pub rule: String,
    pub severity: &'static str,
    pub file: String,
    pub line: u32,
    pub column: u32,
    pub end_line: u32,
    pub end_column: u32,
    /// Byte offsets of the match in the file.
    pub start: usize,
    pub end: usize,
    /// The rule's message, its metavariables filled in from the match.
    pub message: String,
    /// The match's first line, trimmed.
    pub text: String,
    /// The whole matched text: what a monitor compares with the base's matches.
    #[serde(skip)]
    pub matched: String,
}

fn severity(s: &Severity) -> &'static str {
    match s {
        Severity::Off => "off",
        Severity::Hint => "hint",
        Severity::Info => "info",
        Severity::Warning => "warning",
        Severity::Error => "error",
    }
}

/// Matches of every rule that applies to one file's source, in the order ast-grep reports them: by rule id, then
/// position. Suppression comments (`ast-grep-ignore`) are honoured as the command line honours them.
pub fn match_source(set: &RuleSet, repo: usize, path: &str, src: &str) -> Vec<RuleHit> {
    let rules = set.rules.for_path(path);
    let Some(first) = rules.first() else { return Vec::new() };
    let lang = first.language.clone();
    let Ok(root) = AstGrep::<StrDoc<RuleLang>>::try_new(src, lang) else { return Vec::new() };
    let scan = CombinedScan::new(rules);
    let mut hits = Vec::new();
    for (rule, nms) in scan.scan(&root, false).matches {
        for nm in nms {
            let (start, end) = (nm.start_pos(), nm.end_pos());
            let range = nm.range();
            hits.push(RuleHit {
                repo,
                rule: rule.id.clone(),
                severity: severity(&rule.severity),
                file: path.to_string(),
                line: start.line() as u32 + 1,
                column: start.byte_point().1 as u32 + 1,
                end_line: end.line() as u32 + 1,
                end_column: end.byte_point().1 as u32 + 1,
                start: range.start,
                end: range.end,
                message: rule.get_message(&nm),
                text: first_line(&nm.text()),
                matched: nm.text().into_owned(),
            });
        }
    }
    hits.sort_by(|a, b| (&a.rule, a.start, a.end).cmp(&(&b.rule, b.start, b.end)));
    hits
}

/// Matched text shown per hit: the first line, cut to this many bytes.
const SNIPPET: usize = 200;

fn first_line(text: &str) -> String {
    let first = text.lines().next().unwrap_or("").trim();
    let mut end = first.len().min(SNIPPET);
    while !first.is_char_boundary(end) {
        end -= 1;
    }
    let cut = end < first.len();
    let more = text.trim_end().contains('\n');
    format!("{}{}", &first[..end], if cut || more { " …" } else { "" })
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct RuleScanStats {
    pub files_parsed: usize,
    pub bytes_read: u64,
    /// Stopped early at the match cap.
    pub capped: bool,
    /// Files that could not be read (repo index, path): their matches are missing.
    #[serde(skip)]
    pub failed: Vec<(usize, String)>,
}

/// Parses and matches files in parallel. Hits come back sorted by repo, path, position and rule. At `max_matches`
/// the scan stops at the same file whatever the thread timing.
pub fn scan(roots: &[PathBuf], files: &[ScanFile], set: &RuleSet, threads: usize, max_matches: usize) -> (Vec<RuleHit>, RuleScanStats) {
    let work = |data: &mut Vec<u8>, f: &ScanFile| -> Option<(usize, Vec<RuleHit>)> {
        if !search::readable(search::read_into(&roots[f.repo].join(&f.path), data))? {
            return Some((0, Vec::new()));
        }
        let src = String::from_utf8_lossy(data);
        Some((data.len(), match_source(set, f.repo, &f.path, &src)))
    };
    let p = search::scan_prefix(files, threads, max_matches, Vec::new, work);
    let mut hits: Vec<RuleHit> = p.files.into_iter().flat_map(|(_, _, h)| h).collect();
    hits.sort_by(|a, b| (a.repo, &a.file, a.start, a.end, &a.rule).cmp(&(b.repo, &b.file, b.start, b.end, &b.rule)));
    let failed = p.failed.iter().map(|&k| (files[k].repo, files[k].path.clone())).collect();
    (hits, RuleScanStats { files_parsed: p.files_read, bytes_read: p.bytes_read, capped: p.capped, failed })
}

/// A file that differs between a monitor's base and the working tree.
#[derive(Debug, Clone)]
pub struct BaseFile {
    /// Its path now: `None` for a file deleted since the base.
    pub now: Option<String>,
    /// Its path at the base (another for a renamed file) and its blob there: `None` for a file the base didn't have.
    pub was: Option<(String, String)>,
}

/// The files that differ from `base` (a commit) in the working tree of `root`, each with its version at the base:
/// tracked files git's diff lists, then untracked ones. A file not listed is the same at the base, so its matches
/// cancel out. Paths the repo's privacy file blocks are left out, and their old versions never read.
pub fn changed_since(root: &Path, base: &str) -> Result<(String, Vec<BaseFile>)> {
    let d = crate::diff::diff(root, Some(base))?;
    let mut out: Vec<BaseFile> = d
        .changes
        .into_iter()
        .map(|c| {
            let was = c.old_blob.map(|b| (c.renamed_from.unwrap_or_else(|| c.file.clone()), b));
            BaseFile { now: (!c.deleted).then_some(c.file), was }
        })
        .collect();
    let privacy = crate::privacy::Privacy::load(root);
    let untracked = crate::git::run(root, &["-c", "core.quotePath=false", "ls-files", "-z", "--others", "--exclude-standard"])?;
    for p in untracked.split(|&b| b == 0).filter(|p| !p.is_empty()) {
        let file = String::from_utf8_lossy(p).into_owned();
        if !privacy.blocks(&file) {
            out.push(BaseFile { now: Some(file), was: None });
        }
    }
    Ok((d.base, out))
}

/// The base's source of each file that had one, read from git's object store with one `git cat-file --batch`, by
/// path at the base.
pub fn base_sources(root: &Path, files: &[&BaseFile]) -> Result<Vec<(String, String)>> {
    let wanted: Vec<&(String, String)> = files.iter().filter_map(|f| f.was.as_ref()).collect();
    if wanted.is_empty() {
        return Ok(Vec::new());
    }
    let input: String = wanted.iter().map(|(_, b)| format!("{b}\n")).collect();
    let out = crate::git::run_with_input(root, &["cat-file", "--batch"], input.as_bytes())?;
    let mut sources = Vec::new();
    let mut at = 0;
    for (path, blob) in wanted {
        // `<sha> <type> <size>\n<content>\n`, or `<sha> missing\n`.
        let Some(nl) = out[at..].iter().position(|&b| b == b'\n') else { break };
        let head = String::from_utf8_lossy(&out[at..at + nl]).into_owned();
        at += nl + 1;
        let mut parts = head.split(' ');
        if parts.next() != Some(blob.as_str()) {
            bail!("git cat-file answered out of order for {path}");
        }
        let (Some(_), Some(Ok(size))) = (parts.next(), parts.next().map(str::parse::<usize>)) else { continue };
        let Some(bytes) = out.get(at..at + size) else { break };
        at += size + 1;
        sources.push((path.clone(), String::from_utf8_lossy(bytes).into_owned()));
    }
    Ok(sources)
}

/// Of the matches now in the files that differ from a monitor's base, those the base didn't have. A match is known
/// by its rule and its whole text, wherever it is: one that moved, within its file or to another, or whose file was
/// renamed, is not new, since the base's versions of the same files had it. When there are more matches of one rule
/// and text than the base had, the last ones (by file, then position) are the new ones.
pub fn new_matches(mut now: Vec<RuleHit>, base: &[RuleHit]) -> Vec<RuleHit> {
    use std::collections::HashMap;
    let mut left: HashMap<(&str, &str), isize> = HashMap::new();
    for h in &now {
        *left.entry((h.rule.as_str(), h.matched.as_str())).or_default() += 1;
    }
    for h in base {
        if let Some(n) = left.get_mut(&(h.rule.as_str(), h.matched.as_str())) {
            *n -= 1;
        }
    }
    let mut surplus: HashMap<(String, String), isize> = left.into_iter().filter(|(_, n)| *n > 0).map(|((r, t), n)| ((r.to_string(), t.to_string()), n)).collect();
    now.sort_by(|a, b| (&a.file, a.start, a.end, &a.rule).cmp(&(&b.file, b.start, b.end, &b.rule)));
    let mut out = Vec::new();
    for h in now.into_iter().rev() {
        if let Some(n) = surplus.get_mut(&(h.rule.clone(), h.matched.clone()))
            && *n > 0
        {
            *n -= 1;
            out.push(h);
        }
    }
    out.reverse();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(yaml: &str) -> Result<RuleSet> {
        load(&[("rules.yml".into(), yaml.into())])
    }

    #[test]
    fn a_rule_matches_and_fills_its_message() {
        let s = set("id: no-result\nlanguage: CSharp\nseverity: warning\nmessage: blocking on $T\nrule:\n  pattern: $T.Result\n").unwrap();
        let src = "class A {\n  void F() {\n    var x = task.Result;\n  }\n}\n";
        let hits = match_source(&s, 0, "A.cs", src);
        assert_eq!(hits.len(), 1);
        assert_eq!((hits[0].line, hits[0].severity, hits[0].message.as_str()), (3, "warning", "blocking on task"));
        assert!(match_source(&s, 0, "a.ts", "task.Result;").is_empty());
    }

    #[test]
    fn inside_checks_only_the_parent_unless_told_to_stop_at_the_end() {
        // ast-grep's relational rules stop at the neighbour by default: the parent for `inside`.
        let near = "id: r\nlanguage: ts\nrule:\n  pattern: console.log($$$)\n  inside:\n    kind: method_definition\n";
        let far = format!("{near}    stopBy: end\n");
        let src = "class C {\n  m() {\n    console.log(1);\n  }\n}\n";
        assert!(match_source(&set(near).unwrap(), 0, "c.ts", src).is_empty());
        assert_eq!(match_source(&set(&far).unwrap(), 0, "c.ts", src).len(), 1);
    }

    #[test]
    fn suppression_comments_hide_a_match() {
        let s = set("id: no-log\nlanguage: ts\nrule:\n  pattern: console.log($$$)\n").unwrap();
        let src = "console.log(1);\n// ast-grep-ignore: no-log\nconsole.log(2);\n";
        let lines: Vec<u32> = match_source(&s, 0, "a.ts", src).iter().map(|h| h.line).collect();
        assert_eq!(lines, vec![1]);
    }

    #[test]
    fn fix_javascript_and_unknown_languages_are_refused() {
        let err = |y: &str| set(y).err().map(|e| format!("{e:#}")).unwrap_or_default();
        assert!(err("id: r\nlanguage: ts\nrule: {pattern: a}\nfix: b\n").contains("`fix` is refused"));
        assert!(err("id: r\nlanguage: js\nrule: {pattern: a}\n").contains("JavaScript rules aren't supported"));
        assert!(err("id: r\nlanguage: kotlin\nrule: {pattern: a}\n").contains("Kotlin rules aren't supported"));
        assert!(err("id: r\nlanguage: ruby\nrule: {pattern: a}\n").contains("not ruby"));
        assert!(err("language: ts\nrule: {pattern: a}\n").contains("no `id`"));
    }
    #[test]
    fn a_rule_file_of_nested_aliases_is_refused_not_expanded() {
        // Nine levels of ten aliases each would stand for 10^9 nodes.
        let mut y = String::from("a0: &a0 [x, x, x, x, x, x, x, x, x, x]\n");
        for i in 1..10 {
            let p = i - 1;
            y.push_str(&format!("a{i}: &a{i} [*a{p}, *a{p}, *a{p}, *a{p}, *a{p}, *a{p}, *a{p}, *a{p}, *a{p}, *a{p}]\n"));
        }
        y.push_str("id: r\nlanguage: ts\nrule: {pattern: a}\n");
        // Refused by the YAML limits before ast-grep-config parses it: nothing is expanded.
        let err = set(&y).err().map(|e| format!("{e:#}")).unwrap_or_default();
        assert!(err.contains("aliases stand for more than 100000 nodes"), "{err}");
        // Over 2 MiB, refused unread.
        let big = format!("id: r\nlanguage: ts\nrule: {{pattern: a}}\nnote: \"{}\"\n", "x".repeat(3 << 20));
        assert!(set(&big).err().map(|e| e.to_string()).unwrap_or_default().contains("larger than 2 MiB"));
    }

    #[test]
    fn a_monitor_reports_only_matches_the_base_lacks() {
        let s = set("id: no-log\nlanguage: ts\nrule:\n  pattern: console.log($$$)\n").unwrap();
        let base = "console.log(1);\nf();\nconsole.log(2);\n";
        // Moved, kept, one more of an existing text, and a new text.
        let head = "f();\nconsole.log(2);\nconsole.log(1);\nconsole.log(1);\nconsole.log(3);\n";
        let new = new_matches(match_source(&s, 0, "a.ts", head), &match_source(&s, 0, "a.ts", base));
        let lines: Vec<u32> = new.iter().map(|h| h.line).collect();
        assert_eq!(lines, vec![4, 5]);
    }
}
