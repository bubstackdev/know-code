//! Structural search: ast-grep patterns matched against syntax trees, over the files of one language.
//!
//! A pattern is code with metavariables (`$X` one node, `$$$ARGS` any number, `$_` one node not captured), parsed
//! with the same tree-sitter grammar the extractor uses. Every identifier and literal the pattern spells out must
//! appear in a matching file, so those become a trigram formula ([`crate::grams`]) that picks candidate files before
//! any is parsed. Candidates are parsed and matched in parallel. Relational filters (`inside`, `has` and their
//! negations) narrow matches by their ancestors or descendants.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Result, anyhow, bail};
use ast_grep_core::matcher::{KindMatcher, PatternBuilder, PatternNode};
use ast_grep_core::meta_var::MetaVarEnv;
use ast_grep_core::tree_sitter::{LanguageExt, StrDoc, TSLanguage};
use ast_grep_core::{AstGrep, Doc, Language, Node, Pattern, PatternError};
use kc_extract::Lang;
use kc_store::Store;
use kc_store::content::{CFile, INDEXED, PENDING};
use serde::Serialize;

use crate::grams::{self, Plan};
use crate::search::{self, CandidateStats, ScanFile, content_lang, path_glob};

/// A tree-sitter grammar as an ast-grep language.
#[derive(Clone)]
pub(crate) struct Grammar {
    ts: TSLanguage,
    /// What `$` becomes before the pattern is parsed: grammars where `$` can't start an identifier get `µ`.
    expando: char,
}

impl Grammar {
    pub(crate) fn new(lang: Lang) -> Option<Grammar> {
        let ts = kc_extract::grammar(lang)?;
        let expando = if matches!(lang, Lang::TypeScript | Lang::Tsx | Lang::JavaScript) { '$' } else { 'µ' };
        Some(Grammar { ts, expando })
    }
}

impl Language for Grammar {
    fn pre_process_pattern<'q>(&self, query: &'q str) -> std::borrow::Cow<'q, str> {
        if self.expando == '$' {
            return query.into();
        }
        expand_sigils(query, self.expando).into()
    }

    fn expando_char(&self) -> char {
        self.expando
    }

    fn kind_to_id(&self, kind: &str) -> u16 {
        self.ts.id_for_node_kind(kind, true)
    }

    fn field_to_id(&self, field: &str) -> Option<u16> {
        self.ts.field_id_for_name(field).map(|f| f.get())
    }

    fn build_pattern(&self, builder: &PatternBuilder) -> Result<Pattern, PatternError> {
        builder.build(|src| StrDoc::try_new(src, self.clone()))
    }
}

impl LanguageExt for Grammar {
    fn get_ts_language(&self) -> TSLanguage {
        self.ts.clone()
    }
}

/// Rewrites metavariable sigils (`$X`, `$$$ARGS`, `$_`) to `expando`, leaving other `$`s (string interpolation)
/// alone.
fn expand_sigils(query: &str, expando: char) -> String {
    let mut out = String::with_capacity(query.len());
    let mut dollars = 0;
    for c in query.chars() {
        if c == '$' {
            dollars += 1;
            continue;
        }
        let sigil = if c.is_ascii_uppercase() || c == '_' || dollars == 3 { expando } else { '$' };
        out.extend(std::iter::repeat_n(sigil, dollars));
        dollars = 0;
        out.push(c);
    }
    out.extend(std::iter::repeat_n(if dollars == 3 { expando } else { '$' }, dollars));
    out
}

/// The content language (as `search -l` names it) for a language name or alias.
pub fn lang_named(name: &str) -> Result<&'static str> {
    Ok(match name.to_ascii_lowercase().as_str() {
        "csharp" | "cs" | "c#" => "csharp",
        "typescript" | "ts" | "tsx" => "typescript",
        "javascript" | "js" | "jsx" => "javascript",
        "python" | "py" => "python",
        "rust" | "rs" => "rust",
        "swift" => "swift",
        other => bail!("structural search covers csharp, typescript, javascript, python, rust and swift, not {other}"),
    })
}

/// The grammars a content language's files are parsed with.
fn langs_of(content: &str) -> &'static [Lang] {
    match content {
        "csharp" => &[Lang::CSharp],
        "typescript" => &[Lang::TypeScript, Lang::Tsx],
        "javascript" => &[Lang::JavaScript],
        "python" => &[Lang::Python],
        "rust" => &[Lang::Rust],
        "swift" => &[Lang::Swift],
        _ => &[],
    }
}

#[derive(Debug, Clone, Default)]
pub struct AstOptions {
    /// The pattern: code with `$X`, `$$$ARGS` and `$_` metavariables, or `kind:<node kind>` for every node of a kind.
    pub pattern: String,
    /// The language (`csharp`, `typescript`, `python`, `rust`, `swift` or an alias).
    pub lang: String,
    /// Node kind to match within the pattern parsed as context (`method_declaration` within `class C { ... }`), for
    /// code that doesn't parse on its own.
    pub selector: Option<String>,
    /// Keep matches with an ancestor matching this (a pattern, or `kind:<node kind>`).
    pub inside: Option<String>,
    /// Keep matches with a descendant matching this.
    pub has: Option<String>,
    /// Drop matches with an ancestor matching this.
    pub not_inside: Option<String>,
    /// Drop matches with a descendant matching this.
    pub not_has: Option<String>,
    /// Keep matches whose text matches this regular expression.
    pub regex: Option<String>,
    /// Path globs; a leading `!` excludes (as in `search`).
    pub globs: Vec<String>,
    /// Stop collecting after this many matches.
    pub max_matches: usize,
}

/// What a pattern string compiles to: a pattern, or nodes of a kind (whose text, optionally, matches a regex).
enum Rel {
    Pattern(Pattern),
    Kind(KindMatcher, Option<regex::Regex>),
}

impl Rel {
    fn kind_matches<D: Doc>(k: &KindMatcher, re: &Option<regex::Regex>, n: &Node<D>) -> bool {
        n.matches(k) && re.as_ref().is_none_or(|re| re.is_match(&n.text()))
    }

    fn inside<D: Doc>(&self, n: &Node<D>) -> bool {
        match self {
            Rel::Pattern(p) => n.inside(p),
            Rel::Kind(k, re) => n.ancestors().any(|a| Rel::kind_matches(k, re, &a)),
        }
    }

    fn has<D: Doc>(&self, n: &Node<D>) -> bool {
        match self {
            Rel::Pattern(p) => n.has(p),
            Rel::Kind(k, re) => n.dfs().skip(1).any(|d| Rel::kind_matches(k, re, &d)),
        }
    }
}

/// One grammar's compiled rule.
struct Rule {
    lang: Lang,
    grammar: Grammar,
    pattern: Rel,
    inside: Option<Rel>,
    has: Option<Rel>,
    not_inside: Option<Rel>,
    not_has: Option<Rel>,
    regex: Option<regex::Regex>,
    /// Single-node metavariables to report, sorted.
    vars: Vec<String>,
}

impl Rule {
    fn keeps<D: Doc>(&self, n: &Node<D>) -> bool {
        self.inside.as_ref().is_none_or(|r| r.inside(n))
            && self.has.as_ref().is_none_or(|r| r.has(n))
            && !self.not_inside.as_ref().is_some_and(|r| r.inside(n))
            && !self.not_has.as_ref().is_some_and(|r| r.has(n))
            && self.regex.as_ref().is_none_or(|re| re.is_match(&n.text()))
    }
}

/// A compiled structural query.
pub struct AstMatcher {
    /// The content language searched.
    pub lang: &'static str,
    rules: Vec<Rule>,
    /// Trigrams every matching file has: the pattern's identifiers and literals (and the `inside` and `has`
    /// patterns', which must match too).
    pub plan: Plan,
    include: Vec<String>,
    exclude: Vec<String>,
}

fn has_error(n: &PatternNode) -> bool {
    match n {
        PatternNode::Internal { kind_id, children } => *kind_id == u16::MAX || children.iter().any(has_error),
        PatternNode::Terminal { kind_id, .. } => *kind_id == u16::MAX,
        PatternNode::MetaVar { .. } => false,
    }
}

/// Texts of the named leaves a pattern spells out (identifiers, literals). The matcher compares them verbatim, so
/// a matching file contains each. Keywords and punctuation are compared by node kind alone, so they don't count.
fn literals(n: &PatternNode, out: &mut Vec<String>) {
    match n {
        PatternNode::Terminal { text, is_named: true, .. } => out.push(text.clone()),
        PatternNode::Internal { children, .. } => children.iter().for_each(|c| literals(c, out)),
        _ => {}
    }
}

const HINT: &str = "Write it as one complete piece of code, or put it in context and pass the node kind to match as \
                    --selector (`class C { async void $M() { $$$ } }` with --selector method_declaration).";

fn compile_pattern(src: &str, selector: Option<&str>, g: &Grammar, lang: Lang) -> Result<Pattern> {
    let p = match selector {
        Some(sel) => Pattern::contextual(src, sel, g.clone()),
        None => Pattern::try_new(src, g.clone()),
    }
    .map_err(|e| anyhow!("pattern `{src}` ({}): {e} {HINT}", lang.as_str()))?;
    if has_error(&p.node) {
        bail!("pattern `{src}` doesn't parse as {}: {HINT}", lang.as_str());
    }
    Ok(p)
}

/// A pattern; `kind:<node kind>` (every node of that kind); `kind:<node kind>~<regex>` (those whose text matches
/// the regex); or `kind:<node kind> <pattern>` (the pattern parsed as context, matching the node of that kind within
/// it, like `--selector`).
fn compile_rel(src: &str, selector: Option<&str>, g: &Grammar, lang: Lang) -> Result<Rel> {
    if let Some(rest) = src.trim().strip_prefix("kind:") {
        if let Some((kind, re)) = rest.split_once('~')
            && !kind.contains(char::is_whitespace)
        {
            let k = KindMatcher::try_new(kind, g.clone()).map_err(|e| anyhow!("{e} ({})", lang.as_str()))?;
            let re = regex::Regex::new(re).map_err(|e| anyhow!("invalid regex in `{src}`: {e}"))?;
            return Ok(Rel::Kind(k, Some(re)));
        }
        let (kind, context) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        if context.trim().is_empty() {
            let k = KindMatcher::try_new(kind, g.clone()).map_err(|e| anyhow!("{e} ({})", lang.as_str()))?;
            return Ok(Rel::Kind(k, None));
        }
        return Ok(Rel::Pattern(compile_pattern(context.trim(), Some(kind), g, lang)?));
    }
    Ok(Rel::Pattern(compile_pattern(src, selector, g, lang)?))
}

pub fn compile(o: &AstOptions) -> Result<AstMatcher> {
    if o.pattern.trim().is_empty() {
        bail!("empty pattern");
    }
    let lang = lang_named(&o.lang)?;
    let regex = o.regex.as_deref().map(regex::Regex::new).transpose().map_err(|e| anyhow!("invalid text regex: {e}"))?;
    let mut rules = Vec::new();
    let mut must = Vec::new();
    for &l in langs_of(lang) {
        let g = Grammar::new(l).ok_or_else(|| anyhow!("no grammar for {}", l.as_str()))?;
        let pattern = compile_rel(&o.pattern, o.selector.as_deref(), &g, l)?;
        let rel = |s: &Option<String>| s.as_deref().map(|s| compile_rel(s, None, &g, l)).transpose();
        let (inside, has, not_inside, not_has) = (rel(&o.inside)?, rel(&o.has)?, rel(&o.not_inside)?, rel(&o.not_has)?);
        // Literals are the same under every grammar of a language; one grammar's are enough.
        if rules.is_empty() {
            for r in [Some(&pattern), inside.as_ref(), has.as_ref()].into_iter().flatten() {
                if let Rel::Pattern(p) = r {
                    literals(&p.node, &mut must);
                }
            }
        }
        let mut vars: Vec<String> = match &pattern {
            Rel::Pattern(p) => p.defined_vars().into_iter().map(str::to_string).collect(),
            Rel::Kind(..) => Vec::new(),
        };
        vars.sort();
        rules.push(Rule { lang: l, grammar: g, pattern, inside, has, not_inside, not_has, regex: regex.clone(), vars });
    }
    must.sort();
    must.dedup();
    let parts: Vec<Plan> = must.iter().map(|s| grams::literal_plan(s.as_bytes())).filter(|p| *p != Plan::All).collect();
    let plan = match parts.len() {
        0 => Plan::All,
        1 => parts.into_iter().next().unwrap(),
        _ => Plan::And(parts),
    };
    let (exclude, include): (Vec<String>, Vec<String>) =
        o.globs.iter().map(|g| g.trim().to_string()).filter(|g| !g.is_empty()).partition(|g| g.starts_with('!'));
    Ok(AstMatcher { lang, rules, plan, include, exclude: exclude.into_iter().map(|g| g[1..].to_string()).collect() })
}

impl AstMatcher {
    /// Whether a path is in the language and passes the glob filters.
    pub fn keeps(&self, path: &str) -> bool {
        content_lang(path) == self.lang
            && self.rule_for(path).is_some()
            && !self.exclude.iter().any(|g| path_glob(g, path))
            && (self.include.is_empty() || self.include.iter().any(|g| path_glob(g, path)))
    }

    fn rule_for(&self, path: &str) -> Option<&Rule> {
        let l = Lang::from_path(path)?;
        self.rules.iter().find(|r| r.lang == l)
    }
}

/// Files of one repo that can match: the trigram formula's picks plus every pending file, in the language.
pub fn candidates(store: &Store, files: Vec<CFile>, m: &AstMatcher, repo: usize) -> Result<(Vec<ScanFile>, CandidateStats)> {
    let ids = search::eval(store, &m.plan)?;
    let mut stats = CandidateStats::default();
    let mut out = Vec::new();
    for f in files {
        if !matches!(f.state, INDEXED | PENDING) || !m.keeps(&f.path) {
            continue;
        }
        stats.files += 1;
        if f.state == PENDING || ids.as_ref().is_none_or(|ids| ids.binary_search(&(f.id as u32)).is_ok()) {
            out.push(ScanFile { repo, path: f.path });
        }
    }
    stats.candidates = out.len();
    // In path order, as for a text search (see [`search::candidates`]): a scan cut at its limit keeps a prefix.
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok((out, stats))
}

/// Matched text shown per hit: the first line, cut to this many bytes.
const SNIPPET: usize = 200;
/// Captured metavariable text, cut to this many bytes.
const VAR_TEXT: usize = 120;

/// One structural match.
#[derive(Debug, Clone, Serialize)]
pub struct AstHit {
    #[serde(skip)]
    pub repo: usize,
    pub file: String,
    pub line: u32,
    pub end_line: u32,
    pub column: u32,
    /// The match's first line, trimmed.
    pub text: String,
    /// Single-node metavariables and the text each captured.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub vars: BTreeMap<String, String>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct AstScanStats {
    pub files_parsed: usize,
    pub bytes_read: u64,
    /// Stopped early at the match cap.
    pub capped: bool,
    /// Files among those read that could not be read (repo index, path): their matches are missing.
    #[serde(skip)]
    pub failed: Vec<(usize, String)>,
}

fn cut(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

fn hit<D: Doc>(repo: usize, path: &str, n: &Node<D>, env: &MetaVarEnv<D>, vars: &[String]) -> AstHit {
    let text = n.text();
    let first = text.lines().next().unwrap_or("").trim();
    let more = text.trim_end().contains('\n');
    let start = n.start_pos();
    AstHit {
        repo,
        file: path.to_string(),
        line: start.line() as u32 + 1,
        end_line: n.end_pos().line() as u32 + 1,
        column: start.byte_point().1 as u32 + 1,
        text: if more { format!("{} …", cut(first, SNIPPET)) } else { cut(first, SNIPPET) },
        vars: vars
            .iter()
            .filter_map(|v| env.get_match(v).map(|m| (v.clone(), cut(m.text().trim(), VAR_TEXT))))
            .collect(),
    }
}

/// Matches of one file's source.
pub fn match_source(m: &AstMatcher, repo: usize, path: &str, src: &str) -> Vec<AstHit> {
    let Some(rule) = m.rule_for(path) else { return Vec::new() };
    let Ok(root) = AstGrep::<StrDoc<Grammar>>::try_new(src, rule.grammar.clone()) else { return Vec::new() };
    let top = root.root();
    let keep = |nm: ast_grep_core::NodeMatch<StrDoc<Grammar>>| {
        rule.keeps(nm.get_node()).then(|| hit(repo, path, nm.get_node(), nm.get_env(), &rule.vars))
    };
    match &rule.pattern {
        Rel::Pattern(p) => top.find_all(p).filter_map(keep).collect(),
        Rel::Kind(k, re) => top
            .find_all(k)
            .filter(|nm| re.as_ref().is_none_or(|re| re.is_match(&nm.get_node().text())))
            .filter_map(keep)
            .collect(),
    }
}

/// Parses and matches candidate files in parallel. Hits come back sorted by repo, path and position. At
/// `max_matches` the scan stops at the same file whatever the thread timing (see [`search::deterministic_cut`]);
/// files it could not read are in `failed`.
pub fn scan(roots: &[PathBuf], files: &[ScanFile], m: &AstMatcher, threads: usize, max_matches: usize) -> (Vec<AstHit>, AstScanStats) {
    let work = |data: &mut Vec<u8>, f: &ScanFile| -> Option<(usize, Vec<AstHit>)> {
        if !search::readable(search::read_into(&roots[f.repo].join(&f.path), data))? {
            return Some((0, Vec::new()));
        }
        let src = String::from_utf8_lossy(data);
        Some((data.len(), match_source(m, f.repo, &f.path, &src)))
    };
    let p = search::scan_prefix(files, threads, max_matches, Vec::new, work);
    let mut hits: Vec<AstHit> = p.files.into_iter().flat_map(|(_, _, h)| h).collect();
    hits.sort_by(|a, b| (a.repo, &a.file, a.line, a.column, a.end_line).cmp(&(b.repo, &b.file, b.line, b.column, b.end_line)));
    let failed = p.failed.iter().map(|&k| (files[k].repo, files[k].path.clone())).collect();
    let stats = AstScanStats { files_parsed: p.files_read, bytes_read: p.bytes_read, capped: p.capped, failed };
    (hits, stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matcher(lang: &str, pattern: &str) -> AstMatcher {
        compile(&AstOptions { pattern: pattern.into(), lang: lang.into(), ..Default::default() }).unwrap()
    }

    fn lines(m: &AstMatcher, path: &str, src: &str) -> Vec<u32> {
        match_source(m, 0, path, src).iter().map(|h| h.line).collect()
    }

    #[test]
    fn sigils_expand_only_before_metavariable_names() {
        assert_eq!(expand_sigils("$X.Result", 'µ'), "µX.Result");
        assert_eq!(expand_sigils("f($$$ARGS, $_)", 'µ'), "f(µµµARGS, µ_)");
        assert_eq!(expand_sigils("$\"{x}\" + $a", 'µ'), "$\"{x}\" + $a");
    }

    #[test]
    fn csharp_metavariables_match_any_receiver() {
        let m = matcher("csharp", "$T.Result");
        let src = "class A {\n  void F() {\n    var x = task.Result;\n    var y = GetAsync().Result;\n    var z = Result;\n  }\n}\n";
        assert_eq!(lines(&m, "A.cs", src), vec![3, 4]);
        let hits = match_source(&m, 0, "A.cs", src);
        assert_eq!(hits[0].vars.get("T").map(String::as_str), Some("task"));
        assert_eq!(hits[1].text, "GetAsync().Result");
    }

    #[test]
    fn multi_metavariables_match_any_arguments() {
        let m = matcher("cs", "new HttpClient($$$)");
        let src = "class A {\n  HttpClient a = new HttpClient();\n  HttpClient b = new HttpClient(handler, true);\n  object c = new Other();\n}\n";
        assert_eq!(lines(&m, "A.cs", src), vec![2, 3]);
    }

    #[test]
    fn a_selector_matches_code_that_needs_context() {
        let o = AstOptions {
            pattern: "try { $$$ } catch $$$ { }".into(),
            lang: "csharp".into(),
            selector: Some("catch_clause".into()),
            ..Default::default()
        };
        let m = compile(&o).unwrap();
        let src = "class A {\n  void F() {\n    try { G(); } catch (Exception e) { }\n    try { G(); } catch (Exception e) { Log(e); }\n    try { G(); } catch { /* ignore */ }\n  }\n}\n";
        // Comments don't count as statements.
        assert_eq!(lines(&m, "A.cs", src), vec![3, 5]);
    }

    #[test]
    fn relational_filters_narrow_matches() {
        let src = "class C {\n  ngOnInit() {\n    this.a.subscribe(x => x);\n  }\n  other() {\n    this.b.subscribe(y => y);\n  }\n}\n";
        let mut o = AstOptions { pattern: "$S.subscribe($$$)".into(), lang: "ts".into(), ..Default::default() };
        assert_eq!(lines(&compile(&o).unwrap(), "c.ts", src), vec![3, 6]);
        o.inside = Some("kind:method_definition".into());
        assert_eq!(lines(&compile(&o).unwrap(), "c.ts", src), vec![3, 6]);
        // A method on its own doesn't parse; in context, with the kind to select, it does.
        o.inside = Some("kind:method_definition class C { ngOnInit() { $$$ } }".into());
        assert_eq!(lines(&compile(&o).unwrap(), "c.ts", src), vec![3]);
        // A kind whose text matches a regex: also `ngOnInit(): void`, which the pattern above leaves out.
        o.inside = Some(r"kind:method_definition~^(async\s+)?ngOnInit\b".into());
        let typed = src.replace("ngOnInit() {", "ngOnInit(): void {");
        assert_eq!(lines(&compile(&o).unwrap(), "c.ts", &typed), vec![3]);
        o.inside = None;
        o.has = Some("y".into());
        assert_eq!(lines(&compile(&o).unwrap(), "c.ts", src), vec![6]);
        o.has = None;
        o.not_has = Some("y".into());
        assert_eq!(lines(&compile(&o).unwrap(), "c.tsx", src), vec![3]);
    }

    #[test]
    fn a_kind_with_a_regex_matches_what_modifiers_make_hard_to_spell() {
        let src = "class A {\n  public async void F() { }\n  async void G() { }\n  public async Task H() { }\n}\n";
        // A pattern's modifiers must line up with the code's: `async void $M` misses `public async void`.
        let o = AstOptions {
            pattern: "class C { async void $M($$$) { $$$ } }".into(),
            lang: "csharp".into(),
            selector: Some("method_declaration".into()),
            ..Default::default()
        };
        assert_eq!(lines(&compile(&o).unwrap(), "A.cs", src), vec![3]);
        let o = AstOptions {
            pattern: "kind:method_declaration".into(),
            lang: "csharp".into(),
            regex: Some(r"^[^(]*\basync\s+void\b".into()),
            ..Default::default()
        };
        assert_eq!(lines(&compile(&o).unwrap(), "A.cs", src), vec![2, 3]);
        let o = AstOptions { pattern: r"kind:method_declaration~\bTask\s+H\b".into(), lang: "csharp".into(), ..Default::default() };
        assert_eq!(lines(&compile(&o).unwrap(), "A.cs", src), vec![4]);
    }

    #[test]
    fn literals_become_the_trigram_prefilter() {
        let m = matcher("csharp", "new HttpClient($$$)");
        assert_eq!(m.plan, grams::literal_plan(b"HttpClient"));
        // Only metavariables and keywords: every file is a candidate.
        assert_eq!(matcher("csharp", "$A = $B;").plan, Plan::All);
    }

    #[test]
    fn files_of_other_languages_are_left_out() {
        let m = matcher("typescript", "console.log($$$)");
        assert!(m.keeps("src/app.ts") && m.keeps("src/view.tsx"));
        assert!(!m.keeps("src/app.js") && !m.keeps("Api/Program.cs"));
    }

    #[test]
    fn unparseable_patterns_say_how_to_fix_them() {
        let o = AstOptions { pattern: "catch ($E) { }".into(), lang: "csharp".into(), ..Default::default() };
        let err = compile(&o).err().map(|e| e.to_string()).unwrap_or_default();
        assert!(err.contains("--selector"), "{err}");
        assert!(lang_named("cobol").is_err());
    }

    #[test]
    fn python_rust_and_swift_patterns_parse() {
        assert_eq!(lines(&matcher("py", "print($$$)"), "a.py", "def f():\n    print(1, 2)\n"), vec![2]);
        assert_eq!(lines(&matcher("rs", "$X.unwrap()"), "a.rs", "fn f() {\n    let a = b.unwrap();\n}\n"), vec![2]);
        assert_eq!(lines(&matcher("swift", "try! $X"), "a.swift", "func f() {\n    let a = try! g()\n}\n"), vec![2]);
    }
}
