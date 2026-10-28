//! The MCP server: the query operations as tools over stdio.
//!
//! Every tool returns the spec's JSON envelope (repo, commit, dirty, items, truncated) in its compact form (see
//! [`crate::compact`]), capped at about 2k tokens by default; traversals (`kc_trace`, `kc_impact`, `kc_path`, `kc_flow`) answer as an indented
//! tree of text by default. Output rules, for every tool:
//!
//! - a list cut short says so: `complete: false` with an `incomplete` entry (see docs/answers.md), and the
//!   older `truncated`, and a `hint` naming the parameter that gets the rest; a list that continues has a `cursor` (`<list>_cursor` when nested), which `kc_more` pages through, and
//!   the size cap renews the cursor of a list it cuts (a rename preview it cuts is refused);
//! - an empty answer says what was searched (`note`), so "nothing" is never mistaken for "not looked";
//! - `kc_context`, `kc_refs` and `kc_locate` take several inputs in one call (`symbols`, `names`, `queries`) and
//!   answer one result per input.
//!
//! What tools are advertised costs every agent context, so each is described in a sentence or two, with a line per
//! parameter (held to a budget by a test); the details are in `kc_help` topics (see [`crate::help`]). A profile
//! (`KNOWCODE_TOOLS`, see [`profile`]) picks the tools advertised: [`CORE`] by default, every tool with `all`.

use std::path::{Path, PathBuf};

use rmcp::handler::server::{router::tool::ToolRouter, wrapper::Parameters};
use rmcp::model::{Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use kc_store::answer::{Code, Refusal};

use crate::ops::{self, Scope};

/// Responses longer than this many characters lose list items until they fit (about 4 characters per token).
const DEFAULT_CHARS: usize = 8_000;

/// The least a `kc_context` bundle is capped at (its default budget makes 6,000).
const CONTEXT_CHARS: usize = 6_000;
const MIN_CHARS: usize = 1_000;
const MAX_CHARS: usize = 40_000;

/// A numeric parameter's bounds. Its schema line, its help text (see `help::fill`) and the clamp its answers apply
/// all read them here, so they can't say different things.
#[derive(Debug)]
pub struct Range {
    /// What the number counts, leading its schema line.
    pub what: &'static str,
    pub min: usize,
    pub default: usize,
    pub max: usize,
    /// Said after the bounds.
    pub note: &'static str,
}

impl Range {
    /// `v`, or the default, within the bounds.
    pub fn clamp(&self, v: Option<usize>) -> usize {
        v.unwrap_or(self.default).clamp(self.min, self.max)
    }

    /// `1 (default) to 16`, `1 to 16 (default 10)`.
    pub fn span(&self) -> String {
        if self.default == self.min {
            format!("{} (default) to {}", self.min, self.max)
        } else {
            format!("{} to {} (default {})", self.min, self.max, self.default)
        }
    }

    /// The parameter's line in its tool's schema and help: `Hops, 1 (default) to 16.`
    pub fn line(&self) -> String {
        format!("{}, {}{}.", self.what, self.span(), self.note)
    }
}

pub const TRACE_DEPTH: Range = Range { what: "Hops", min: 1, default: 1, max: kc_store::trace::MAX_TRACE_DEPTH as usize, note: "" };
pub const IMPACT_DEPTH: Range = Range { what: "Hops", min: 1, default: 1, max: kc_store::trace::MAX_IMPACT_DEPTH as usize, note: "" };
pub const FLOW_DEPTH: Range = Range { what: "Steps deep", min: 1, default: 10, max: crate::flow::MAX_DEPTH as usize, note: "" };
pub const PATH_HOPS: Range = Range { what: "Hops a path may have", min: 1, default: 8, max: crate::walk::MAX_PATH_HOPS as usize, note: "" };
pub const PATH_COUNT: Range = Range { what: "Paths to return", min: 1, default: 3, max: crate::walk::MAX_PATHS, note: "" };
pub const SEARCH_LIMIT: Range = Range { what: "Results per page", min: 1, default: 20, max: 500, note: "" };
pub const TASK_FILES: Range = Range { what: "Files to list", min: 1, default: 6, max: 30, note: "" };
pub const TASK_DEPENDENTS: Range = Range { what: "Dependents to list", min: 0, default: 6, max: 40, note: "" };
pub const TASK_TESTS: Range = Range { what: "Tests to list", min: 0, default: 4, max: 30, note: "" };
pub const SEARCH_CONTEXT: Range = Range { what: "Lines of context around each hit", min: 0, default: 0, max: 10, note: "" };
pub const ANSWER_CHARS: Range = Range {
    what: "Answer size cap in characters",
    min: MIN_CHARS,
    default: DEFAULT_CHARS,
    max: MAX_CHARS,
    note: ", the max doubled for several inputs; a list cut to fit says so (`incomplete`, `more`)",
};

/// Every bounded parameter, by tool (`*`: every tool).
pub const RANGES: &[(&str, &str, &Range)] = &[
    ("kc_trace", "depth", &TRACE_DEPTH),
    ("kc_impact", "depth", &IMPACT_DEPTH),
    ("kc_flow", "depth", &FLOW_DEPTH),
    ("kc_path", "max_hops", &PATH_HOPS),
    ("kc_path", "paths", &PATH_COUNT),
    ("kc_search", "limit", &SEARCH_LIMIT),
    ("kc_search", "context", &SEARCH_CONTEXT),
    ("kc_grep_ast", "limit", &SEARCH_LIMIT),
    ("kc_task", "files", &TASK_FILES),
    ("kc_task", "dependents", &TASK_DEPENDENTS),
    ("kc_task", "tests", &TASK_TESTS),
    ("*", "max_chars", &ANSWER_CHARS),
];

/// The bounds of `tool`'s parameter `param`.
pub fn range(tool: &str, param: &str) -> Option<&'static Range> {
    RANGES.iter().find(|(t, p, _)| (*t == tool || *t == "*") && *p == param).map(|(_, _, r)| *r)
}

/// An [`ADVERTISED`] line standing for the parameter's bounds (`1 (default) to 16.`).
const SPAN: &str = "{span}";

/// The string parameters taking one of a closed set of words, by tool (`*`: every tool taking it), the default first.
/// The full schema lists them (`enum`), and so do the capabilities; each op refuses any other word.
pub const CHOICES: &[(&str, &str, &[&str])] = &[
    ("kc_trace", "direction", &["both", "in", "out"]),
    ("kc_context", "source", &["auto", "full", "skeleton", "none"]),
    ("kc_impact", "order", &["rank", "hop"]),
    ("kc_unused", "tier", &["all", "unused", "possibly"]),
    ("kc_search", "case", &["sensitive", "insensitive", "smart"]),
    ("kc_grep_ast", "lang", &["csharp", "typescript", "javascript", "python", "rust", "swift"]),
    ("kc_note_add", "tag", &["decision", "gotcha", "todo"]),
    ("*", "format", &["tree", "json"]),
    ("*", "cite", &["brief", "lines", "check", "none"]),
];

/// The words `tool`'s parameter `param` takes, the default first.
pub fn choices(tool: &str, param: &str) -> Option<&'static [&'static str]> {
    CHOICES.iter().find(|(t, p, _)| (*t == tool || *t == "*") && *p == param).map(|(_, _, c)| *c)
}

/// `schema` with each bounded parameter's `minimum`, `maximum` and `default` from [`RANGES`], and each closed set's
/// `enum` from [`CHOICES`]: the full schema, which help and the capabilities read. The advertised one leaves them out.
fn bounded(tool: &str, mut schema: rmcp::model::JsonObject) -> rmcp::model::JsonObject {
    if let Some(props) = schema.get_mut("properties").and_then(Value::as_object_mut) {
        for (name, p) in props.iter_mut() {
            let Some(o) = p.as_object_mut() else { continue };
            if let Some(r) = range(tool, name) {
                o.insert("minimum".into(), Value::from(r.min));
                o.insert("maximum".into(), Value::from(r.max));
                o.insert("default".into(), Value::from(r.default));
            }
            if let Some(c) = choices(tool, name) {
                o.insert("enum".into(), Value::from(c.to_vec()));
            }
        }
    }
    schema
}

/// What [`bounded`] added, taken out of an advertised parameter.
const BOUNDS: &[&str] = &["minimum", "maximum", "default", "enum"];



/// A refused call as a client gets it: the English in the tool result's text, as it always was, and the machine code
/// in its `structuredContent` (`{"error": {"code", "message", "privacy"}}`, `privacy` for `withheld`), so a client
/// never reads the English to tell refusals apart. A [`Refusal`] raised where the call is refused keeps its code;
/// any other error is coded by what it says ([`code_of`]), and one nothing codes is `internal`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolError {
    pub code: Code,
    pub message: String,
    pub privacy: Option<kc_store::answer::PrivacyPolicy>,
}

impl ToolError {
    pub fn new(code: Code, message: impl Into<String>) -> Self {
        ToolError { code, message: message.into(), privacy: None }
    }

    /// The same refusal said in other words (`kc`'s op names).
    fn reworded(self, f: impl FnOnce(&str) -> String) -> Self {
        ToolError { message: f(&self.message), ..self }
    }

    /// `{"code", "message", "privacy"}`.
    pub fn json(&self) -> Value {
        let mut v = serde_json::json!({ "code": self.code, "message": self.message });
        if let Some(p) = &self.privacy {
            v["privacy"] = serde_json::json!(p);
        }
        v
    }
}

impl From<String> for ToolError {
    fn from(message: String) -> Self {
        ToolError::new(code_of(&message), message)
    }
}

impl From<anyhow::Error> for ToolError {
    fn from(e: anyhow::Error) -> Self {
        let message = format!("{e:#}");
        match e.chain().find_map(|c| c.downcast_ref::<Refusal>()) {
            Some(r) => ToolError { code: r.code, message, privacy: r.privacy.clone() },
            None if kc_store::limits::current().is_cancelled() => ToolError::new(Code::Cancelled, message),
            None => ToolError::from(message),
        }
    }
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::ops::Deref for ToolError {
    type Target = str;
    fn deref(&self) -> &str {
        &self.message
    }
}

impl PartialEq<&str> for ToolError {
    fn eq(&self, other: &&str) -> bool {
        self.message == *other
    }
}

impl rmcp::handler::server::tool::IntoCallToolResult for ToolError {
    fn into_call_tool_result(self) -> Result<rmcp::model::CallToolResponse, rmcp::ErrorData> {
        Ok(coded(rmcp::model::CallToolResult::error(vec![rmcp::model::ContentBlock::text(self.message.clone())]), &self).into())
    }
}

/// `result` with `e`'s code in its `structuredContent`.
fn coded(mut result: rmcp::model::CallToolResult, e: &ToolError) -> rmcp::model::CallToolResult {
    result.structured_content = Some(serde_json::json!({ "error": e.json() }));
    result
}

/// The code of an error raised without one, by what it says: the argument checks of each op, the messages of the
/// libraries parsing a call's arguments (serde), and the errors of reading files, the index and git. Every family an
/// op raises is here or raised as a [`Refusal`]; the tests call each op into each and check its code.
pub fn code_of(message: &str) -> Code {
    let m = message;
    let has = |words: &[&str]| words.iter().any(|w| m.contains(w));
    if m.starts_with("withheld:") {
        Code::Withheld
    } else if has(&["stale cursor", "is no longer held by this server"]) {
        Code::StaleCursor
    } else if has(&["paths matched 0 files"]) {
        Code::NoFiles
    } else if has(&[
        "failed to deserialize",
        "invalid type:",
        "missing field",
        "invalid value:",
        "unknown variant",
        " must be ",
        "is not a cursor",
        "no op ",
        "takes no `q`",
        "takes one `q`",
        "per call; split them",
        "is not a valid identifier",
        "is an assumption: write",
        "a preset can't be taken away",
        "unknown evidence",
        "unknown edge kind",
        "unknown symbol kind",
        "unknown kind `",
        "unknown tag ",
        "edges for references are",
        "doesn't parse as",
        "invalid regex",
        "invalid pattern",
        "invalid text regex",
        "empty pattern",
        "structural search covers",
        "takes a date git understands",
        "a note needs some text",
        "no help topic",
        "not a path search's cursor",
        "are the same symbol",
    ]) || m.starts_with("give ")
    {
        Code::InvalidArgument
    } else if has(&["names several definitions", "definitions; pass", "beyond the repo limit", "rename previews one repo at a time"])
        || (m.contains(" is in ") && m.contains(" repos ("))
    {
        Code::Ambiguous
    } else if has(&["no repo at ", "nothing is indexed", "is indexed; run", "not inside a git repo", "no indexed repo named", "no repo to query"]) {
        Code::NotIndexed
    } else if has(&["no definition", "no symbol ", "no indexed repo defines", "no commit ", "not a git revision", "shares no history", "no such file"]) {
        Code::NotFound
    } else if has(&["cancelled"]) {
        Code::Cancelled
    } else if has(&["failed in ", "opening ", "reading ", "could not read", "is in use", "can't open", "no longer exists", "changed while it was being opened"]) {
        Code::Failed
    } else {
        Code::Internal
    }
}

// Parameter docs are the schema's descriptions, advertised with every tool: one short line each. The details are in
// each tool's help topic (`help.rs`), which lists these too.

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SymbolAtParams {
    /// The file: repo-relative, or absolute (which picks its repo).
    pub file: String,
    /// 1-based line.
    pub line: u32,
    #[serde(flatten)]
    pub common: Common,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FindParams {
    /// A name, prefix, camelCase words or `Type.member`.
    pub query: String,
    /// Kind filter: type, func, class, method, property, ...
    #[serde(default)]
    pub kind: Option<String>,
    /// Results (default 10).
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(flatten)]
    pub common: Common,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LocateParams {
    /// What the code does, in plain words.
    #[serde(default)]
    pub query: Option<String>,
    /// Several queries instead of `query` (at most 20).
    #[serde(default)]
    pub queries: Vec<String>,
    /// Files (default 10).
    #[serde(default)]
    pub limit: Option<usize>,
    /// Rank files across every indexed repo.
    #[serde(default)]
    pub workspace: bool,
    #[serde(flatten)]
    pub common: Common,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TaskParams {
    /// The goal, in plain words: a bug report, a ticket title, what the change should do.
    pub goal: String,
    /// Files to list (bounds in `RANGES`).
    #[serde(default)]
    pub files: Option<usize>,
    /// Dependents to list (bounds in `RANGES`).
    #[serde(default)]
    pub dependents: Option<usize>,
    /// Tests to list (bounds in `RANGES`).
    #[serde(default)]
    pub tests: Option<usize>,
    #[serde(flatten)]
    pub common: Common,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RefsParams {
    /// A name, `Type.member` for that member's references, or `file:line` (after its repo's name too) for those of the
    /// definition there.
    #[serde(default)]
    pub name: Option<String>,
    /// Several names instead of `name` (at most 20).
    #[serde(default)]
    pub names: Vec<String>,
    /// Reference kinds: call, instantiate, type_ref, member, read, write.
    #[serde(default)]
    pub kinds: Vec<String>,
    /// `kinds` by edge name: calls, instantiates, uses, member, reads, writes.
    #[serde(default)]
    pub edges: Vec<String>,
    /// References (default 20).
    #[serde(default)]
    pub limit: Option<usize>,
    /// `brief` (default): first 12 rows quote a line, all checked vs the index; `lines`: every row; `check`; `none`.
    #[serde(default)]
    pub cite: Option<String>,
    #[serde(flatten)]
    pub common: Common,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ContextParams {
    /// A name, `Type.member`, or `file:line` (after its repo's name too: `shop/src/a.cs:12`).
    #[serde(default)]
    pub symbol: Option<String>,
    /// Several symbols instead of `symbol` (at most 20), sharing the budget.
    #[serde(default)]
    pub symbols: Vec<String>,
    /// Token budget for the bundle (default 1200).
    #[serde(default)]
    pub budget: Option<usize>,
    /// `auto` (default), `full`, `skeleton` or `none`.
    #[serde(default)]
    pub source: Option<String>,
    /// A repo's deploy answer: rows per list (default all that fit).
    #[serde(default)]
    pub limit: Option<usize>,
    /// `brief` (default): first 12 rows quote a line, all checked vs the index; `lines`: every row; `check`; `none`.
    #[serde(default)]
    pub cite: Option<String>,
    #[serde(flatten)]
    pub common: Common,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TraceParams {
    /// A name, or `Type.member`.
    pub symbol: String,
    /// `in`, `out` or `both` (default).
    #[serde(default)]
    pub direction: Option<String>,
    /// Hops (bounds in `RANGES`).
    #[serde(default)]
    pub depth: Option<u8>,
    /// Node cap (default 25).
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(flatten)]
    pub walk: WalkParams,
    #[serde(flatten)]
    pub common: Common,
}

/// Filters for a walk, and its answer format.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct WalkParams {
    /// Edge kinds to follow (default all).
    #[serde(default)]
    pub edges: Vec<String>,
    /// Symbol kinds to list; the walk passes through others.
    #[serde(default)]
    pub kinds: Vec<String>,
    /// Evidence policy (kc_help evidence).
    #[serde(default)]
    pub evidence: Option<String>,
    /// `brief` (default): first 12 rows quote a line, all checked vs the index; `lines`: every row; `check`; `none`.
    #[serde(default)]
    pub cite: Option<String>,
    /// `tree` (default) or `json`.
    #[serde(default)]
    pub format: Option<String>,
}

impl WalkParams {
    fn filter(&self) -> ops::WalkFilter {
        ops::WalkFilter { edges: self.edges.clone(), kinds: self.kinds.clone(), evidence: self.evidence.clone().unwrap_or_default() }
    }

    fn tree(&self) -> Result<bool, String> {
        match self.format.as_deref() {
            None | Some("tree") => Ok(true),
            Some("json") => Ok(false),
            Some(other) => Err(format!("format must be tree or json, not {other}")),
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PathParams {
    /// Where the path starts: a name or `Type.member`, in `repo`.
    pub from: String,
    /// Where it ends: a name or `Type.member`, in `to_repo`.
    pub to: String,
    /// The repo `to` is in (default: `repo`, else the repos defining it).
    #[serde(default)]
    pub to_repo: Option<String>,
    /// Hops a path may have (bounds in `RANGES`).
    #[serde(default)]
    pub max_hops: Option<u8>,
    /// Paths to return (bounds in `RANGES`).
    #[serde(default)]
    pub paths: Option<usize>,
    /// Follow ambiguous edges too.
    #[serde(default)]
    pub allow_ambiguous: bool,
    /// Symbols to explore at most (default 2000).
    #[serde(default)]
    pub max_nodes: Option<usize>,
    /// Repos to open at most (default 16).
    #[serde(default)]
    pub max_repos: Option<usize>,
    /// Edge kinds a path may use (default all).
    #[serde(default)]
    pub edges: Vec<String>,
    /// Evidence policy (kc_help evidence).
    #[serde(default)]
    pub evidence: Option<String>,
    /// `brief` (default): first 12 rows quote a line, all checked vs the index; `lines`: every row; `check`; `none`.
    #[serde(default)]
    pub cite: Option<String>,
    /// `tree` (default) or `json`.
    #[serde(default)]
    pub format: Option<String>,
    #[serde(flatten)]
    pub common: Common,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FlowParams {
    /// An Angular route, a component, a controller action, `Type.member` or `file:line`.
    pub entry: String,
    /// Steps deep (bounds in `RANGES`).
    #[serde(default)]
    pub depth: Option<u8>,
    /// Steps to list (default 40).
    #[serde(default)]
    pub limit: Option<usize>,
    /// Repos to open at most (default 16).
    #[serde(default)]
    pub max_repos: Option<usize>,
    /// Edge kinds to follow (default all).
    #[serde(default)]
    pub edges: Vec<String>,
    /// Evidence policy (kc_help evidence).
    #[serde(default)]
    pub evidence: Option<String>,
    /// Levels of template children to follow (default 2).
    #[serde(default)]
    pub children: Option<u8>,
    /// `brief` (default): first 12 rows quote a line, all checked vs the index; `lines`: every row; `check`; `none`.
    #[serde(default)]
    pub cite: Option<String>,
    /// `tree` (default) or `json`.
    #[serde(default)]
    pub format: Option<String>,
    #[serde(flatten)]
    pub common: Common,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ImpactParams {
    /// A symbol (name, `Type.member`, or `file:line`, after its repo's name too).
    #[serde(default)]
    pub symbol: Option<String>,
    /// A file instead of a symbol.
    #[serde(default)]
    pub file: Option<String>,
    /// A diff instead: `working`, or a git revision.
    #[serde(default)]
    pub diff: Option<String>,
    /// Hops (bounds in `RANGES`).
    #[serde(default)]
    pub depth: Option<u8>,
    /// Affected symbols to list (default 30).
    #[serde(default)]
    pub limit: Option<usize>,
    /// Order within a hop: `rank` (default) or `hop`.
    #[serde(default)]
    pub order: Option<String>,
    /// Coupled files: history up to this commit or date.
    #[serde(default)]
    pub as_of: Option<String>,
    /// Coupled files: history since `<n>d`, a date or `all` (default 365d).
    #[serde(default)]
    pub since: Option<String>,
    #[serde(flatten)]
    pub walk: WalkParams,
    #[serde(flatten)]
    pub common: Common,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UnusedParams {
    /// Kinds to check (default all).
    #[serde(default)]
    pub kinds: Vec<String>,
    /// `unused` (confident only), `possibly`, or `all` (default).
    #[serde(default)]
    pub tier: Option<String>,
    /// Findings (default 25).
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(flatten)]
    pub common: Common,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RenamePreviewParams {
    /// `Type.member`, a name, or `file:line` (for an overload).
    pub symbol: String,
    /// The new name.
    pub new_name: String,
    /// Preview even when a site is ambiguous or the name collides.
    #[serde(default)]
    pub allow_ambiguous: bool,
    #[serde(flatten)]
    pub common: Common,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CoupledParams {
    /// The file (repo-relative or absolute).
    pub file: String,
    /// Partners (default 10).
    #[serde(default)]
    pub limit: Option<usize>,
    /// History up to this commit or date.
    #[serde(default)]
    pub as_of: Option<String>,
    /// History since `<n>d`, a date or `all` (default 365d).
    #[serde(default)]
    pub since: Option<String>,
    #[serde(flatten)]
    pub common: Common,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TestsParams {
    /// Changed files or symbols.
    #[serde(default)]
    pub targets: Vec<String>,
    /// A diff instead: `working`, or a git revision.
    #[serde(default)]
    pub diff: Option<String>,
    /// Tests in the tight list (default 10); left_out shows at least 20.
    #[serde(default)]
    pub limit: Option<usize>,
    /// Evidence policy (kc_help evidence); here `all` by default, so ambiguous edges count too, ranked low.
    #[serde(default)]
    pub evidence: Option<String>,
    #[serde(flatten)]
    pub common: Common,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct HotspotsParams {
    /// One file's vitals instead of the ranking.
    #[serde(default)]
    pub path: Option<String>,
    /// Files (default 10).
    #[serde(default)]
    pub top: Option<usize>,
    /// Rank every indexed repo instead.
    #[serde(default)]
    pub workspace: bool,
    /// End the windows here: unix seconds, YYYY-MM-DD or `head`.
    #[serde(default)]
    pub as_of: Option<String>,
    #[serde(flatten)]
    pub common: Common,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct OwnershipParams {
    /// A file or directory (default: the repo).
    #[serde(default)]
    pub path: Option<String>,
    /// Authors (names or emails) to treat as gone.
    #[serde(default)]
    pub without: Vec<String>,
    /// Directory depth of the areas listed (default 2).
    #[serde(default)]
    pub depth: Option<usize>,
    /// Areas or files to list (default 10).
    #[serde(default)]
    pub top: Option<usize>,
    #[serde(flatten)]
    pub common: Common,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct HistoryParams {
    /// A name, `Type.member` or [repo/]file:line.
    pub symbol: String,
    /// Commits to list (default 10).
    #[serde(default)]
    pub limit: Option<usize>,
    /// Only commits since this date (`2025-01-01`, `90 days ago`).
    #[serde(default)]
    pub since: Option<String>,
    #[serde(flatten)]
    pub common: Common,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReviewersParams {
    /// Changed files.
    #[serde(default)]
    pub files: Vec<String>,
    /// A diff instead: `working`, or a git revision.
    #[serde(default)]
    pub diff: Option<String>,
    /// Reviewers (default 5).
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(flatten)]
    pub common: Common,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct OverviewParams {
    /// Every indexed repo instead of one.
    #[serde(default)]
    pub workspace: bool,
    /// Bias the map toward a name or path fragment.
    #[serde(default)]
    pub focus: Option<String>,
    /// Token budget for the map (default 400; 0: none).
    #[serde(default)]
    pub map_budget: Option<usize>,
    #[serde(flatten)]
    pub common: Common,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchParams {
    /// Text to find (literal unless `regex`).
    pub pattern: String,
    /// `pattern` is a regular expression.
    #[serde(default)]
    pub regex: bool,
    /// `sensitive` (default), `insensitive` or `smart`.
    #[serde(default)]
    pub case: Option<String>,
    /// Whole words only.
    #[serde(default)]
    pub word: bool,
    /// Languages to keep.
    #[serde(default)]
    pub lang: Vec<String>,
    /// Path globs; a leading `!` excludes; after a repo's name (`shop/src/**`, `**/shop/src/**`), that repo only.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Lines of context around each hit (bounds in `RANGES`).
    #[serde(default)]
    pub context: Option<usize>,
    /// Results per page (default 20).
    #[serde(default)]
    pub limit: Option<usize>,
    /// Results to skip: the previous `next_offset`.
    #[serde(default)]
    pub offset: Option<usize>,
    /// One item per matching file.
    #[serde(default)]
    pub files_only: bool,
    /// Stop after this many matching lines (default 50000).
    #[serde(default)]
    pub max_lines: Option<usize>,
    /// Search every indexed repo.
    #[serde(default)]
    pub workspace: bool,
    /// Search these repos instead of one.
    #[serde(default)]
    pub repos: Vec<String>,
    #[serde(flatten)]
    pub common: Common,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GrepAstParams {
    /// An ast-grep pattern, or `kind:<node kind>[~regex]` (kc_help kc_grep_ast).
    pub pattern: String,
    /// csharp, typescript, javascript, python, rust or swift.
    pub lang: String,
    /// Node kind to match within the pattern parsed as context.
    #[serde(default)]
    pub selector: Option<String>,
    /// Keep matches inside a node matching this.
    #[serde(default)]
    pub inside: Option<String>,
    /// Keep matches containing a node matching this.
    #[serde(default)]
    pub has: Option<String>,
    /// Drop matches inside a node matching this.
    #[serde(default)]
    pub not_inside: Option<String>,
    /// Drop matches containing a node matching this.
    #[serde(default)]
    pub not_has: Option<String>,
    /// Keep matches whose text matches this regex.
    #[serde(default)]
    pub text: Option<String>,
    /// Path globs; a leading `!` excludes; after a repo's name (`shop/src/**`, `**/shop/src/**`), that repo only.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Results per page (default 20).
    #[serde(default)]
    pub limit: Option<usize>,
    /// Results to skip: the previous `next_offset`.
    #[serde(default)]
    pub offset: Option<usize>,
    /// Stop after this many matches (default 50000).
    #[serde(default)]
    pub max_matches: Option<usize>,
    /// Search every indexed repo.
    #[serde(default)]
    pub workspace: bool,
    /// Search these repos instead of one.
    #[serde(default)]
    pub repos: Vec<String>,
    #[serde(flatten)]
    pub common: Common,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RepoOnlyParams {
    /// Every indexed repo instead of one.
    #[serde(default)]
    pub workspace: bool,
    #[serde(flatten)]
    pub common: Common,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AreasParams {
    /// One area (id, name or directory): it and its files.
    #[serde(default)]
    pub area: Option<String>,
    #[serde(flatten)]
    pub common: Common,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct NotesParams {
    /// A name or `Type.member` (default: every note in the repo).
    #[serde(default)]
    pub symbol: Option<String>,
    #[serde(flatten)]
    pub common: Common,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct NoteAddParams {
    /// A name or `Type.member`.
    pub symbol: String,
    /// The note: a short decision, gotcha or todo.
    pub text: String,
    /// decision, gotcha or todo.
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(flatten)]
    pub common: Common,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct MoreParams {
    /// A `cursor` or `<list>_cursor`, as it is.
    pub cursor: String,
    #[serde(default)]
    pub max_chars: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TestTargetsParams {
    /// Only the targets of this name (a SwiftPM target, a .NET project, a script); none: every one.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(flatten)]
    pub common: Common,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct CapabilitiesParams {
    /// One op (`impact`) or tool (`kc_impact`) to describe; none: every op.
    #[serde(default)]
    pub of: Option<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct HelpParams {
    /// A tool (`kc_context`), a section (`kc_context.http`) or a concept (`contract`); none: the index.
    #[serde(default)]
    pub topic: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct Common {
    // Every tool has these two, so their descriptions are said once, in each tool's help topic (see `help::COMMON`),
    // not in every tool's schema; the server's instructions name `repo`.
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub max_chars: Option<usize>,
}

/// The `kc` tool's parameters (the `one` profile, the default): the tool to run (`op`, its name without `kc_`) and
/// its main input (`q`, or several in `qs`). Any other parameter of that tool is passed by its own name.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct OneParams {
    /// The tool to run, by its name without `kc_`: impact, find, refs, context, search, locate, more, help, or any other
    /// (trace, path, flow, tests, …).
    pub op: String,
    /// The op's main input: a name, `Type.member`, [repo/]file:line, text, a concept, a cursor or a help topic (see
    /// `ONE_OPS` for the parameter it fills).
    #[serde(default)]
    pub q: Option<String>,
    /// Several `q` at once (refs, context, locate; tests' targets, reviewers' files).
    #[serde(default)]
    pub qs: Vec<String>,
    /// How many hops (impact: 2 for callers' callers; trace).
    #[serde(default)]
    pub depth: Option<u32>,
    /// Any other parameter of the op's tool, by its name (`repo`, `limit`, `regex`, `diff`, …).
    #[serde(flatten)]
    pub rest: serde_json::Map<String, Value>,
}

/// The ops of the `kc` tool, one per tool: (op, the parameter `q` fills, the one `qs` fills). The core tools come
/// first, in the order the tool's description names them. A parameter that takes a list gets `q` as a list of one.
pub const ONE_OPS: &[(&str, &str, &str)] = &[
    ("impact", "symbol", ""),
    ("find", "query", ""),
    ("refs", "name", "names"),
    ("context", "symbol", "symbols"),
    ("search", "pattern", ""),
    ("locate", "query", "queries"),
    ("more", "cursor", ""),
    ("help", "topic", ""),
    ("trace", "symbol", ""),
    ("path", "from", ""),
    ("flow", "entry", ""),
    ("symbol_at", "file", ""),
    ("unused", "", ""),
    ("rename_preview", "symbol", ""),
    ("coupled", "file", ""),
    ("tests", "targets", "targets"),
    ("hotspots", "", ""),
    ("ownership", "path", ""),
    ("reviewers", "files", "files"),
    ("history", "symbol", ""),
    ("overview", "focus", ""),
    ("areas", "area", ""),
    ("notes", "symbol", ""),
    ("note_add", "symbol", ""),
    ("grep_ast", "pattern", ""),
    ("status", "", ""),
    ("capabilities", "of", ""),
    ("task", "goal", ""),
    ("test_targets", "name", ""),
];

/// What `q` and `qs` fill, a line per parameter `q` fills (ops in [`ONE_OPS`] order), the ops taking no `q` last: the
/// `kc` help topic's list.
pub fn one_q_lines() -> String {
    let mut groups: Vec<(&str, Vec<(&str, &str)>)> = Vec::new();
    for (op, main, many) in ONE_OPS {
        match groups.iter_mut().find(|(m, _)| m == main) {
            Some((_, ops)) => ops.push((op, many)),
            None => groups.push((main, vec![(op, many)])),
        }
    }
    groups.sort_by_key(|(main, _)| main.is_empty());
    let mut lines = Vec::new();
    for (main, ops) in groups {
        let names: Vec<&str> = ops.iter().map(|(o, _)| *o).collect();
        if main.is_empty() {
            lines.push(format!("- {}: no `q` (an empty one is ignored)", names.join(", ")));
            continue;
        }
        let mut line = format!("- {}: `{main}`", names.join(", "));
        for (op, many) in ops.iter().filter(|(_, m)| !m.is_empty()) {
            if many == &main {
                line.push_str(&format!(" ({op}: `q` as a list of one, or `qs`)"));
            } else {
                line.push_str(&format!("; {op}: `{many}` from `qs`"));
            }
        }
        lines.push(line);
    }
    lines.join("\n")
}

/// The parameters that take a list, among those `q` fills.
const ONE_LISTS: &[&str] = &["targets", "files"];

/// A `kc` call as the tool it runs and that tool's arguments.
pub fn one_call(p: OneParams) -> Result<(String, serde_json::Map<String, Value>), String> {
    let op = p.op.trim().trim_start_matches("kc_").to_string();
    let Some((_, main, many)) = ONE_OPS.iter().find(|(o, _, _)| *o == op) else {
        let ops: Vec<&str> = ONE_OPS.iter().map(|(o, _, _)| *o).collect();
        return Err(format!("no op {op:?}: one of {}", ops.join(", ")));
    };
    let mut args = p.rest;
    // An op taking no query takes an empty one as none.
    let q = p.q.filter(|q| !main.is_empty() || !q.trim().is_empty());
    if let Some(q) = q {
        if main.is_empty() {
            return Err(format!("op {op} takes no `q`; pass its parameters by name (kc {{op: help, q: {op}}})"));
        }
        let value = if ONE_LISTS.contains(main) { Value::from(vec![q]) } else { Value::from(q) };
        args.insert(main.to_string(), value);
    }
    if !p.qs.is_empty() {
        if many.is_empty() {
            return Err(format!("op {op} takes one `q`, not `qs`"));
        }
        args.insert(many.to_string(), Value::from(p.qs));
    }
    if let Some(d) = p.depth {
        args.insert("depth".into(), Value::from(d));
    }
    Ok((format!("kc_{op}"), args))
}

/// Tool calls an answer or a text names (`kc_more {cursor: …}`, `kc_refs Save`, `kc_context(symbol: "X")`,
/// `kc_trace`) as the `kc` tool takes them: `kc {op: more, cursor: …}`, `kc {op: refs, q: Save}`,
/// `kc {op: context, symbol: "X"}`, `kc {op: trace}`. In `prose` (help topics, notes), a bare name followed by a
/// word is never read as a call with an argument, and `kc_help topic` names the topic as `q`; a topic's section
/// (`kc_impact.crossings`) loses its prefix.
pub fn one_text(text: &str, prose: bool) -> String {
    // Words that follow a tool's name in an answer without being its argument (`kc_search finds them`).
    const NOT_ARGS: &[&str] = &["finds", "lists", "answers", "and", "or", "for", "on", "with", "to", "is", "takes"];
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut out = String::with_capacity(text.len() + 32);
    let mut rest = text;
    while let Some(at) = rest.find("kc_") {
        let before = &rest[..at];
        let prev_ok = !before.chars().next_back().is_some_and(ident);
        let after = &rest[at + 3..];
        let name_len = after.find(|c: char| !ident(c)).unwrap_or(after.len());
        let name = &after[..name_len];
        out.push_str(before);
        if !prev_ok || !ONE_OPS.iter().any(|(o, _, _)| *o == name) {
            out.push_str("kc_");
            rest = after;
            continue;
        }
        let mut tail = &after[name_len..];
        if let Some(section) = tail.strip_prefix('.').filter(|s| s.starts_with(|c: char| c.is_ascii_lowercase())) {
            // A help topic's section.
            out.push_str(name);
            out.push('.');
            rest = section;
            continue;
        }
        if let Some(inner) = tail.strip_prefix(" {").or_else(|| tail.strip_prefix('{')) {
            // `kc_more {cursor: …}`, `kc_impact {"depth":2,…}`, `kc_help {topic}`.
            if let Some(close) = inner.strip_prefix('}') {
                out.push_str(&format!("kc {{op: {name}}}"));
                tail = close;
            } else {
                out.push_str(&format!("kc {{op: {name}, "));
                tail = inner;
            }
        } else if let Some(inner) = tail.strip_prefix('(') {
            // `kc_context(symbol: "X")`: up to the matching parenthesis, outside quotes.
            let (mut depth, mut quoted, mut end) = (1, false, None);
            for (i, c) in inner.char_indices() {
                match c {
                    '"' => quoted = !quoted,
                    '(' if !quoted => depth += 1,
                    ')' if !quoted => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(i);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            match end {
                Some(0) => out.push_str(&format!("kc {{op: {name}}}")),
                Some(i) => out.push_str(&format!("kc {{op: {name}, {}}}", &inner[..i])),
                None => {
                    out.push_str(&format!("kc {{op: {name}}}("));
                    rest = inner;
                    continue;
                }
            }
            tail = &inner[end.unwrap() + 1..];
        } else if let Some(word) = tail.strip_prefix(' ').map(|t| t.split(char::is_whitespace).next().unwrap_or(""))
            && (!prose || name == "help")
            && !word.is_empty()
            && !NOT_ARGS.contains(&word)
            && !word.starts_with(['(', '`', '"', '<'])
        {
            // `kc_refs Save lists them all`, `(kc_refs isValid)`, `kc_help kc_impact`.
            let raw = word.trim_end_matches([')', ',', ';', ':', '.']);
            let arg = if name == "help" { raw.trim_start_matches("kc_") } else { raw };
            if arg.is_empty() {
                out.push_str(&format!("kc {{op: {name}}}"));
            } else {
                out.push_str(&format!("kc {{op: {name}, q: {arg}}}"));
                tail = &tail[1 + raw.len()..];
            }
        } else {
            out.push_str(&format!("kc {{op: {name}}}"));
        }
        rest = tail;
    }
    out.push_str(rest);
    out
}

#[derive(Clone)]
pub struct KnowcodeServer {
    default: Scope,
    tool_router: ToolRouter<Self>,
    /// Every tool's input schema as advertised, advertised or not (for `kc_help`).
    schemas: std::sync::Arc<std::collections::BTreeMap<String, rmcp::model::JsonObject>>,
    /// Every tool's description, advertised or not (for the capabilities).
    descriptions: std::sync::Arc<std::collections::BTreeMap<String, String>>,
    /// Tools the profile leaves out (see [`profile`]).
    hidden: std::sync::Arc<Vec<String>>,
    /// The `one` profile: every tool is reached through `kc`, and answers name calls as its ops.
    one: bool,
    /// Restricted mode's scope (see [`crate::restricted`]).
    restriction: Option<std::sync::Arc<crate::restricted::Restriction>>,
}

/// The tools of the `core` profile, the default: the ones that answer the first questions about code (where X is
/// defined, every use of X, what X is, where a concept lives, text, what calls X or breaks if X changes), paging, and
/// the help that names the rest. A client that loads every tool's schema on every turn carries these only; `all`
/// advertises the rest too. kc_trace is left out: kc_impact answers callers by hop and kc_context a symbol's callers
/// and callees, and its schema is one of the largest.
pub const CORE: &[&str] = &["kc_find", "kc_locate", "kc_context", "kc_refs", "kc_impact", "kc_search", "kc_more", "kc_help"];

/// The tools Claude Code loads up front instead of deferring to tool search (`_meta` `anthropic/alwaysLoad`): the core
/// tools that answer a first question. Their schemas are paid on every turn, so the core tools sessions don't start
/// with stay deferred, one tool search away: in 0.3's study kc_locate and kc_help were never called and kc_more 3
/// times in 320 sessions. A cut answer names the kc_more call that continues it (`hint`), and the instructions name
/// all three. A client that doesn't defer tools still loads every core tool.
pub const ALWAYS_LOADED: &[&str] = &["kc_find", "kc_context", "kc_refs", "kc_impact", "kc_search"];

/// The parameters each tool's schema advertises, with the line each carries there: the ones most calls need. Every
/// other parameter (`repo` and `max_chars`, which every tool takes, and the rarer ones such as kc_impact's `file`,
/// `as_of`, `since`, `order`, `format`, `kinds`, `evidence`, `edges`, `cite`) is accepted all the same and documented
/// in the tool's help topic, with its full line. A client without tool search loads every schema on every turn, so a
/// parameter belongs here only when most questions need it, and its line only when its name doesn't say enough (a
/// line costs about as many tokens as the parameter: `symbols` says "several").
pub const ADVERTISED: &[(&str, &[(&str, &str)])] = &[
    ("kc_help", &[("topic", "")]),
    ("kc_symbol_at", &[("file", ""), ("line", "")]),
    ("kc_find", &[("query", "")]),
    ("kc_locate", &[("query", ""), ("queries", "")]),
    ("kc_refs", &[("name", ""), ("names", "")]),
    ("kc_context", &[("symbol", ""), ("symbols", "")]),
    ("kc_trace", &[("symbol", "A name, `Type.member` or [repo/]file:line."),("direction", "`in`, `out` or `both` (default)."), ("depth", SPAN)]),
    ("kc_path", &[("from", ""), ("to", "")]),
    ("kc_flow", &[("entry", "")]),
    ("kc_impact", &[("symbol", "Name, `Type.member` or file:line."), ("diff", "`working` or a git revision."), ("depth", SPAN)]),
    ("kc_unused", &[]),
    ("kc_rename_preview", &[("symbol", ""), ("new_name", "")]),
    ("kc_coupled", &[("file", "")]),
    ("kc_tests", &[("targets", ""), ("diff", "`working` or a git revision.")]),
    ("kc_hotspots", &[]),
    ("kc_ownership", &[("path", "")]),
    ("kc_reviewers", &[("files", ""), ("diff", "`working` or a git revision.")]),
    ("kc_history", &[("symbol", "")]),
    ("kc_overview", &[]),
    ("kc_areas", &[("area", "")]),
    ("kc_notes", &[("symbol", "")]),
    ("kc_note_add", &[("symbol", ""), ("text", "")]),
    ("kc_search", &[("pattern", "Literal unless `regex`."), ("regex", ""), ("paths", "Globs; `!` excludes.")]),
    ("kc_grep_ast", &[("pattern", ""), ("lang", "csharp, typescript, javascript, python, rust or swift.")]),
    ("kc_more", &[("cursor", "")]),
    ("kc_status", &[]),
    ("kc_capabilities", &[("of", "")]),
    ("kc_task", &[("goal", "")]),
    ("kc_test_targets", &[("name", "")]),
    ("kc", &[("op", ""), ("q", "Name, `Type.member`, file:line, text, cursor or topic."), ("qs", ""), ("depth", "impact: 2 for callers' callers.")]),
];

/// `schema` (a tool's full input schema) cut to the parameters [`ADVERTISED`] lists for `tool`, each with its line
/// there (none when the line is empty).
fn advertised(tool: &str, schema: &rmcp::model::JsonObject) -> rmcp::model::JsonObject {
    let Some((_, keep)) = ADVERTISED.iter().find(|(t, _)| *t == tool) else { return schema.clone() };
    let mut out = schema.clone();
    let full = schema.get("properties").and_then(Value::as_object).cloned().unwrap_or_default();
    let mut props = serde_json::Map::new();
    for (name, line) in keep.iter() {
        let Some(mut p) = full.get(*name).cloned() else { continue };
        if let Some(o) = p.as_object_mut() {
            if range(tool, name).is_some() || choices(tool, name).is_some() {
                BOUNDS.iter().for_each(|k| drop(o.remove(*k)));
            }
            if line.is_empty() {
                o.remove("description");
            } else if *line == SPAN
                && let Some(r) = range(tool, name)
            {
                o.insert("description".into(), Value::from(format!("{}.", r.span())));
            } else {
                o.insert("description".into(), Value::from(*line));
            }
        }
        props.insert(name.to_string(), p);
    }
    out.insert("properties".into(), Value::Object(props));
    out
}

/// The tools `all` leaves out, advertised only when named (`all,kc_capabilities`): what they answer is read once by a
/// client building its tool surface, not on every turn, and is also served as a resource and as the `kc` tool's op.
pub const ON_REQUEST: &[&str] = &["kc_capabilities", "kc_test_targets"];

/// The profile `serve` uses unless `--tools` or `KNOWCODE_TOOLS` names another: the single `kc` tool.
pub const DEFAULT_PROFILE: &str = "one";

/// The profile from `KNOWCODE_TOOLS`, or the default.
pub fn profile_spec() -> String {
    std::env::var("KNOWCODE_TOOLS").ok().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| DEFAULT_PROFILE.into())
}

/// Whether a profile spec advertises the single `kc` tool, so that calls are named as its ops.
pub fn is_one(spec: &str) -> bool {
    spec.split(',').any(|p| p.trim() == "one")
}

/// The tools a profile spec names, out of `all`: `one` (the `kc` tool), `all`, `core`, and tool names (with or
/// without `kc_`), comma separated. `kc_help` is always in, unless `kc` is, whose `help` op stands in for it.
pub fn profile(spec: &str, all: &[String]) -> Result<Vec<String>, String> {
    // `one`: the single `kc` tool, whose ops reach every tool, kc_help included.
    let one = is_one(spec);
    let mut out: Vec<String> = if one { Vec::new() } else { vec!["kc_help".into()] };
    for part in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let names: Vec<String> = match part {
            "all" => all.iter().filter(|t| !ON_REQUEST.contains(&t.as_str())).cloned().collect(),
            "core" => CORE.iter().map(|s| s.to_string()).collect(),
            "one" => vec!["kc".into()],
            name => {
                let n = if name.starts_with("kc_") { name.to_string() } else { format!("kc_{name}") };
                if !all.contains(&n) {
                    return Err(format!("KNOWCODE_TOOLS: no tool or profile {name:?}; profiles are one, core and all, tools {}", all.join(", ")));
                }
                vec![n]
            }
        };
        for n in names {
            if !out.contains(&n) {
                out.push(n);
            }
        }
    }
    Ok(out)
}

/// `schema` with each bounded parameter's line from [`RANGES`].
fn ranged(tool: &str, mut schema: rmcp::model::JsonObject) -> rmcp::model::JsonObject {
    if let Some(props) = schema.get_mut("properties").and_then(Value::as_object_mut) {
        for (name, p) in props.iter_mut() {
            if let Some(r) = range(tool, name).filter(|_| p.get("description").is_some())
                && let Some(o) = p.as_object_mut()
            {
                o.insert("description".into(), Value::from(r.line()));
            }
        }
    }
    schema
}

/// A tool's input schema as advertised: without what tells an agent nothing (`$schema`, `default: null`, integer
/// formats and bounds) and with optional values typed plainly (`"integer"`, not `["integer", "null"]`: a parameter
/// left out is the null).
fn slim(schema: &rmcp::model::JsonObject) -> rmcp::model::JsonObject {
    fn go(v: &mut Value) {
        match v {
            Value::Object(m) => {
                m.remove("$schema");
                if m.get("default").is_some_and(|d| d.is_null() || d.as_array().is_some_and(Vec::is_empty) || *d == Value::Bool(false)) {
                    m.remove("default");
                }
                if m.contains_key("format") && matches!(m.get("type"), Some(t) if t.to_string().contains("integer")) {
                    m.remove("format");
                    m.remove("minimum");
                    m.remove("maximum");
                }
                if let Some(Value::Array(types)) = m.get("type")
                    && types.len() == 2
                    && types.contains(&Value::from("null"))
                    && let Some(t) = types.iter().find(|t| **t != "null").cloned()
                {
                    m.insert("type".into(), t);
                }
                m.values_mut().for_each(go);
            }
            Value::Array(a) => a.iter_mut().for_each(go),
            _ => {}
        }
    }
    let mut v = Value::Object(schema.clone());
    go(&mut v);
    match v {
        Value::Object(m) => m,
        _ => schema.clone(),
    }
}

impl KnowcodeServer {
    /// Refuses a call whose arguments name a path an AI privacy file blocks in the repos it asks about (see
    /// [`ops::guard_inputs`]), before any work starts.
    fn guard_privacy(&self, args: Option<&serde_json::Map<String, Value>>) -> anyhow::Result<()> {
        let Some(args) = args else { return Ok(()) };
        fn strings(v: &Value, out: &mut Vec<String>) {
            match v {
                Value::String(s) => out.push(s.clone()),
                Value::Array(a) => a.iter().for_each(|x| strings(x, out)),
                Value::Object(m) => m.values().for_each(|x| strings(x, out)),
                _ => {}
            }
        }
        let mut inputs = Vec::new();
        for (k, v) in args {
            if k != "repo" {
                strings(v, &mut inputs);
            }
        }
        let repos = match args.get("repo").and_then(Value::as_str) {
            Some(r) => self.scope(&Common { repo: Some(r.to_string()), max_chars: None }, false).map(|s| s.repos).unwrap_or_default(),
            None => self.default.repos.clone(),
        };
        ops::guard_inputs(&repos, &inputs)
    }

    /// What this server offers, for its capabilities.
    pub fn surface(&self) -> crate::capabilities::Surface<'_> {
        crate::capabilities::Surface {
            schemas: &self.schemas,
            descriptions: &self.descriptions,
            hidden: &self.hidden,
            one: self.one,
            restriction: self.restriction.as_deref(),
        }
    }

    /// A help topic as text (with a tool's parameters from its schema), or the index without one.
    fn help(&self, topic: Option<&str>) -> Result<String, String> {
        // Under `one` every tool is reachable, as an op of `kc`.
        let hidden: &[String] = if self.one { &[] } else { &self.hidden };
        let Some(name) = topic.map(str::trim).filter(|t| !t.is_empty()) else { return Ok(crate::help::index(hidden)) };
        let t = crate::help::topic(name).ok_or_else(|| format!("no help topic {name:?}; kc_help without a topic lists them"))?;
        let mut text = crate::help::render(t, self.schemas.get(t.name));
        if let Some((_, keep)) = ADVERTISED.iter().find(|(n, _)| *n == t.name) {
            let names: Vec<String> = keep.iter().map(|(p, _)| format!("`{p}`")).collect();
            let shown = if names.is_empty() { "none of them".to_string() } else { names.join(", ") };
            text.push_str(&format!("\nThe tool's schema lists {shown}; every parameter above is accepted.\n"));
        }
        if hidden.iter().any(|h| h == t.name) {
            text.push_str("\nThis server doesn't advertise this tool (see `profiles`).\n");
        }
        Ok(text)
    }

    /// Several repos by name or path.
    fn scope_many(&self, repos: &[String]) -> Result<Scope, String> {
        let mut out = Vec::new();
        for r in repos {
            let one = Common { repo: Some(r.clone()), max_chars: None };
            out.extend(self.scope(&one, false)?.repos);
        }
        Ok(Scope { repos: out, no_sync: false })
    }

    fn scope(&self, common: &Common, workspace: bool) -> Result<Scope, String> {
        if workspace {
            let roots = ops::workspace_repos().map_err(|e| format!("{e:#}"))?;
            return Ok(Scope { repos: roots, no_sync: false });
        }
        let Some(repo) = &common.repo else { return Ok(self.default.clone()) };
        let path = PathBuf::from(repo);
        if path.exists() {
            return Ok(Scope { repos: vec![path], no_sync: false });
        }
        // By name, any indexed repo: one outside the workspace roots still answers for itself.
        let roots = kc_index::all_indexed_roots().map_err(|e| format!("{e:#}"))?;
        roots
            .into_iter()
            .find(|r| ops::repo_name(r) == *repo)
            .map(|r| Scope { repos: vec![r], no_sync: false })
            .ok_or_else(|| format!("no repo at {repo} and no indexed repo named {repo}"))
    }
}

/// The work lanes. In-process operations take the main lane, one at a time: concurrent calls each open every repo's
/// index and hold their own allocations, which is what made memory grow under pipelined requests. But one slow call
/// (a cold workspace query, a long history walk) must not hold up every cheap one behind it: once the main lane's
/// call has run [`SLOW`], a second call may take the overflow lane, so at most two run in the process. Searches run in
/// a child process (see [`search_in_child`]), whose memory is the child's: they take their own lane and never wait
/// for in-process work, nor it for them.
static WORK: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
static OVERFLOW: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
static CHILDREN: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

/// When the call holding the main lane started.
static MAIN_SINCE: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

/// How long the main lane's call runs before a waiting call may take the overflow lane.
const SLOW: std::time::Duration = std::time::Duration::from_secs(2);

/// A lane held for the length of one call's blocking work (see [`WORK`]).
struct Lane {
    main: bool,
    _permit: tokio::sync::SemaphorePermit<'static>,
}

impl Drop for Lane {
    fn drop(&mut self) {
        if self.main {
            *MAIN_SINCE.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
    }
}

/// Waits for a lane: the children's for a child-process call, else the main one, or the overflow one while the
/// main one's call has run [`SLOW`].
async fn lane(child: bool) -> Result<Lane, String> {
    if child {
        return Ok(Lane { main: false, _permit: CHILDREN.acquire().await.map_err(|e| e.to_string())? });
    }
    let main = |p| {
        *MAIN_SINCE.lock().unwrap_or_else(|e| e.into_inner()) = Some(std::time::Instant::now());
        Lane { main: true, _permit: p }
    };
    loop {
        if let Ok(p) = WORK.try_acquire() {
            return Ok(main(p));
        }
        let since = *MAIN_SINCE.lock().unwrap_or_else(|e| e.into_inner());
        if since.is_some_and(|t| t.elapsed() >= SLOW)
            && let Ok(p) = OVERFLOW.try_acquire()
        {
            return Ok(Lane { main: false, _permit: p });
        }
        tokio::select! {
            p = WORK.acquire() => return Ok(main(p.map_err(|e| e.to_string())?)),
            _ = tokio::time::sleep(SLOW / 8) => {}
        }
    }
}

/// The envelopes a call's answer was made from, kept when the call asked for its provenance record.
type Recording = Option<std::sync::Arc<std::sync::Mutex<Vec<Value>>>>;

tokio::task_local! {
    /// The operation a tool call runs as: cancelled when the client cancels the request (see `call_tool`).
    static OPERATION: std::sync::Arc<kc_store::limits::Operation>;
    /// Where a call that asked for its provenance record keeps its envelopes (see `call_tool`).
    static RECORDING: Recording;
}

/// The call's [`RECORDING`], when it asked for one.
fn recording() -> Recording {
    RECORDING.try_with(Clone::clone).ok().flatten()
}

/// Keeps `env` for the call's provenance record, when it asked for one.
fn note(recording: &Recording, env: &Value) {
    if let Some(r) = recording {
        r.lock().unwrap_or_else(|e| e.into_inner()).push(env.clone());
    }
}

/// Runs a blocking operation and returns its envelope as compact JSON within the size cap.
async fn run(max_chars: Option<usize>, f: impl FnOnce() -> anyhow::Result<Value> + Send + 'static) -> Result<String, ToolError> {
    let env = blocking(false, with_cuts(f)).await?;
    note(&recording(), &env);
    Ok(cap(env, max_chars.unwrap_or(DEFAULT_CHARS).clamp(MIN_CHARS, MAX_CHARS)))
}

/// [`run`] for an operation that runs a child process (a search): in the children's lane.
async fn run_child(max_chars: Option<usize>, f: impl FnOnce() -> anyhow::Result<Value> + Send + 'static) -> Result<String, ToolError> {
    let env = blocking(true, with_cuts(f)).await?;
    note(&recording(), &env);
    Ok(cap(env, max_chars.unwrap_or(DEFAULT_CHARS).clamp(MIN_CHARS, MAX_CHARS)))
}

/// `f`, with the child processes cut while it ran said in its envelope (see [`ops::add_cuts`]). Runs on the blocking
/// thread, where the cuts are recorded.
fn with_cuts(f: impl FnOnce() -> anyhow::Result<Value> + Send + 'static) -> impl FnOnce() -> anyhow::Result<Value> + Send + 'static {
    move || {
        let mut env = f()?;
        ops::add_cuts(&mut env);
        Ok(env)
    }
}

/// Runs a blocking operation in a lane (see [`WORK`]) as the call's operation: a client's cancellation stops its
/// child processes and the work at its next check. The lane is held until the work ends, even when the call's
/// future is dropped first, so a cancelled call never lets more work run than the lanes allow.
async fn blocking<T: Send + 'static>(child: bool, f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> Result<T, ToolError> {
    let lane = lane(child).await?;
    let call = OPERATION.try_with(|o| o.clone()).unwrap_or_else(|_| kc_store::limits::Operation::new());
    // Dropping the awaiting future (the client went away) cancels the work too.
    let mut cancel_on_drop = CancelOnDrop(Some(call.clone()));
    // The work is one query: its own context, starting with nothing collected, cancelled with the call.
    let query = call.sharing_cancellation();
    let result = tokio::task::spawn_blocking(move || {
        let _lane = lane;
        let _entered = kc_store::limits::enter(query);
        let _freshness = crate::refresh::Budget::enter(std::time::Duration::from_secs(2));
        f()
    })
    .await
    .map_err(|e| ToolError::new(Code::Internal, e.to_string()));
    cancel_on_drop.0 = None;
    release_memory();
    result?.map_err(ToolError::from)
}

/// Cancels an operation when dropped.
struct CancelOnDrop(Option<std::sync::Arc<kc_store::limits::Operation>>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(op) = &self.0 {
            op.cancel();
        }
    }
}

/// Runs a traversal and answers as text: a header naming the repo and commit, then the tree, cut at the size cap
/// on a line boundary with a hint saying how much was cut.
async fn run_text(max_chars: Option<usize>, f: impl FnOnce() -> anyhow::Result<(Value, String)> + Send + 'static) -> Result<String, ToolError> {
    let (env, text) = blocking(false, move || {
        let (mut env, text) = f()?;
        ops::add_cuts(&mut env);
        crate::contract::finish(&mut env);
        // The text shows no cursor fields: its hints name the cursors themselves.
        crate::compact::text_hints(&mut env);
        crate::compact::text_stale(&mut env);
        Ok((env, text))
    })
    .await?;
    note(&recording(), &env);
    let max = max_chars.unwrap_or(DEFAULT_CHARS).clamp(MIN_CHARS, MAX_CHARS);
    let body = notes_first(&lean_sites(&mcp_names(&text)));
    // Complete when the envelope says so and the tree says nothing was cut (its cut notes come first).
    let complete = env["complete"].as_bool() != Some(false) && crate::contract::incomplete_of(&env).is_empty() && !body.starts_with('…');
    let whole = format!("{}{body}", header(&env, complete));
    if whole.len() <= max {
        return Ok(whole);
    }
    // A row's other ways go first, from the last row up, said by a count when the rest fits; when it doesn't, the
    // answer is cut as it would be without them.
    let noted = |text: &str, left: usize| {
        let note = format!("… {left} more ways to listed symbols left out at max_chars={max}; pass max_chars (up to {MAX_CHARS}) for them\n");
        Some(format!("{}{note}{text}", header(&env, false))).filter(|w| left > 0 && w.len() <= max)
    };
    let (lean, left) = unway_text(&body, whole.len() - max);
    if let Some(w) = noted(&lean, left) {
        return Ok(w);
    }
    let (body, left) = unway_text(&body, usize::MAX);
    if let Some(w) = noted(&body, left) {
        return Ok(w);
    }
    let whole = format!("{}{body}", header(&env, complete && left == 0));
    if whole.len() <= max {
        return Ok(whole);
    }
    // Rows before the rest of their sites' statements: those lines go first, from the last row up.
    let body = unstate_text(&body, whole.len() - max);
    let whole = format!("{}{body}", header(&env, complete));
    if whole.len() <= max {
        return Ok(whole);
    }
    Ok(cap_text(&format!("{}{body}", header(&env, false)), max))
}

/// A tree's text less its rows' other ways (`render::WAY`, last on a row), from the last row up, until at least
/// `excess` bytes and the note saying so are gone; with how many ways it left out. All of them when that isn't enough.
fn unway_text(text: &str, excess: usize) -> (String, usize) {
    // The note's bytes, at most.
    let excess = excess.saturating_add(160);
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let (mut dropped, mut left) = (0, 0);
    for l in lines.iter_mut().rev() {
        if dropped >= excess {
            break;
        }
        if let Some(at) = l.find(crate::render::WAY) {
            left += l[at..].matches(crate::render::WAY).count();
            dropped += l.len() - at;
            l.truncate(at);
        }
    }
    let mut out = lines.join("\n");
    out.push('\n');
    (out, left)
}

/// A tree's text less up to `excess` bytes of the lines quoting the rest of a site's statement (`  41 - text`, and the
/// cite's `… the statement …` line), from the bottom up: what the size cap drops before any row.
fn unstate_text(text: &str, excess: usize) -> String {
    let statement = |l: &str| {
        let t = l.trim_start();
        let n = t.bytes().take_while(u8::is_ascii_digit).count();
        (n > 0 && t[n..].starts_with(" - ")) || t.starts_with("… the statement ")
    };
    let mut lines: Vec<&str> = text.lines().collect();
    let mut dropped = 0;
    for i in (0..lines.len()).rev() {
        if dropped >= excess {
            break;
        }
        if statement(lines[i]) {
            dropped += lines[i].len() + 1;
            lines.remove(i);
        }
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// A tree line's use site said once: `at <file>:<line>` after a symbol in that same file becomes `at line <line>`,
/// or nothing when it is the symbol's own line (`used by method A.b  a.cs:113  [exact] direct at a.cs:113`).
fn lean_sites(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        out.push_str(&lean_site(line));
        out.push('\n');
    }
    out
}

fn lean_site(line: &str) -> String {
    let Some(at) = line.rfind(" at ") else { return line.to_string() };
    let site = &line[at + 4..];
    let (token, rest) = site.split_once(char::is_whitespace).map_or((site, ""), |(t, r)| (t, r));
    let Some((file, n)) = token.rsplit_once(':') else { return line.to_string() };
    if file.is_empty() || n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
        return line.to_string();
    }
    let head = &line[..at];
    let Some(def) = head.find(&format!("  {file}:")) else { return line.to_string() };
    let own: String = head[def + 3 + file.len()..].chars().take_while(char::is_ascii_digit).collect();
    let rest = if rest.is_empty() { String::new() } else { format!(" {rest}") };
    if own == n { format!("{head}{rest}") } else { format!("{head} at line {n}{rest}") }
}

/// The text with its notes (`… truncated`, depth and hidden-kind notes) moved to the top, so a size cap never cuts them.
fn notes_first(text: &str) -> String {
    let note = |l: &str| {
        let t = l.trim_start();
        t.starts_with("… truncated") || t.starts_with("(depth ") || t.starts_with("(one hop") || t.ends_with("hidden by kinds)")
    };
    let (notes, rest): (Vec<&str>, Vec<&str>) = text.lines().partition(|l| note(l));
    let mut out = String::new();
    for l in notes.iter().map(|l| l.trim_start()).chain(rest.iter().copied()) {
        out.push_str(l);
        out.push('\n');
    }
    out
}

/// CLI flags in a text answer, named as this server's parameters.
fn mcp_names(text: &str) -> String {
    [
        ("--max-nodes", "max_nodes"),
        ("--max-repos", "max_repos"),
        ("--allow-ambiguous", "allow_ambiguous"),
        ("--edges/--kinds", "edges/kinds"),
        ("--limit", "limit"),
        ("--depth", "depth"),
        ("raise -n for all", "raise limit for all"),
    ]
    .iter()
    .fold(text.to_string(), |t, (cli, mcp)| t.replace(cli, mcp))
}

/// A note when a walk stopped at `depth` while symbols at that depth lead further: how to see more.
fn depth_note(depth: u8, max: u8, deeper: bool) -> Option<String> {
    (deeper && depth < max).then(|| format!("(depth {depth} shown; pass depth {} (up to {max}) for more)", depth + 1))
}

/// `repo orders-api @ 1a2b3c4 (dirty)`: what the text answers for. A clean, current answer over several repos names
/// them only, and not the repos it consulted besides (as the compact JSON leaves out `repos` and `provenance`; see
/// [`crate::compact::clean`]). A `complete` answer says so at the end of that line (`repo shop @ 1a2b3c4; complete`); a partial one
/// says why in the lines that follow.
fn header(env: &Value, complete: bool) -> String {
    let clean = crate::compact::clean(env);
    let repo = |v: &Value, commit: bool| {
        let path = v["repo"].as_str().unwrap_or("");
        let name = Path::new(path).file_name().map_or(path.into(), |n| n.to_string_lossy().into_owned());
        let commit = v["commit"].as_str().filter(|_| commit).map(|c| format!(" @ {}", &c[..c.len().min(7)])).unwrap_or_default();
        let dirty = if v["dirty"].as_bool() == Some(true) { " (dirty)" } else { "" };
        format!("{name}{commit}{dirty}")
    };
    let done = if complete { "; complete" } else { "" };
    let head = match env["repos"].as_array() {
        Some(list) => format!("repos {}{done}\n", list.iter().map(|r| repo(r, !clean)).collect::<Vec<_>>().join(", ")),
        None => format!("repo {}{done}\n", repo(env, true)),
    };
    // Why the answer is incomplete (files that could not be read, lists cut), and the other repos it consulted.
    let notes = crate::ops::freshness_notes(env).into_iter().filter(|n| !(clean && n.starts_with("also consulted: ")));
    notes.fold(head, |h, n| format!("{h}{n}\n"))
}

/// `text` within `max` characters, cut on a line boundary, ending with what was cut and how to get it.
fn cap_text(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut cut = max.saturating_sub(160);
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut cut = text[..cut].rfind('\n').map_or(cut, |i| i + 1);
    // Never inside a path: a path cut short would read as one that ends there. Cut before its `path N of M` line.
    if text[cut..].starts_with("  ")
        && let Some(start) = text[..cut].rfind("\npath ").map(|i| i + 1)
        && text[start..cut].lines().skip(1).all(|l| l.starts_with("  "))
    {
        cut = start;
    }
    let left = text[cut..].lines().count();
    format!(
        "{}… {left} more lines cut at max_chars={max}; pass max_chars (up to {MAX_CHARS}), a lower depth, or edges/kinds filters\n",
        &text[..cut]
    )
}

/// Inputs one batch call takes at most.
pub const MAX_BATCH: usize = 20;

/// The least room an input's result is given: below it, an answer is too cut to use.
const MIN_SHARE: usize = 1_000;

/// Runs one operation per input and answers `{"results": [...]}`, each result the operation's envelope with the
/// input as `input`, the whole answer within the size cap: each result gets an even share of the room left (at least
/// [`MIN_SHARE`]), and inputs that don't fit are not run, listed in `left_out` with a `hint` saying how to get them.
/// More than [`MAX_BATCH`] inputs is an error.
async fn run_batch(
    max_chars: Option<usize>,
    inputs: Vec<String>,
    f: impl Fn(&str) -> anyhow::Result<Value> + Send + 'static,
) -> Result<String, ToolError> {
    if inputs.len() > MAX_BATCH {
        return Err(ToolError::new(Code::InvalidArgument, format!("{} inputs: at most {MAX_BATCH} per call; split them across calls", inputs.len())));
    }
    let max = max_chars.unwrap_or(DEFAULT_CHARS).clamp(MIN_CHARS, MAX_CHARS * 2);
    let recording = recording();
    blocking(false, move || {
        let n = inputs.len();
        let mut results: Vec<String> = Vec::new();
        let mut shares: Vec<usize> = Vec::new();
        // `{"results":[` and `]}`, and the commas between results.
        let mut used = 14;
        for (k, i) in inputs.iter().enumerate() {
            let room = max.saturating_sub(used + usize::from(k > 0));
            let share = (room / (n - k)).max(MIN_SHARE).min(room);
            if share < MIN_SHARE / 2 {
                break;
            }
            let text = match f(i) {
                Ok(mut env) => {
                    ops::add_cuts(&mut env);
                    env["input"] = Value::from(i.as_str());
                    note(&recording, &env);
                    shares.push(share);
                    cap_plain(env, share)
                }
                Err(e) => {
                    // As a refused call: the English, its code and, for `withheld`, the policy.
                    let e = ToolError::from(e);
                    let mut v = serde_json::json!({ "input": i, "error": e.message, "code": e.code });
                    if let Some(p) = &e.privacy {
                        v["privacy"] = serde_json::json!(p);
                    }
                    shares.push(0);
                    v.to_string()
                }
            };
            if text.len() > room {
                break;
            }
            used += text.len() + usize::from(k > 0);
            results.push(text);
        }
        // What is left out, said within the cap too: fewer results until the answer fits.
        let mut answer = loop {
            let answer = batch_answer(&results, &inputs[results.len()..], max);
            if answer.len() <= max || results.is_empty() {
                break answer;
            }
            results.pop();
        };
        // Each result's summary (see `summary`), where the batch still fits with it.
        let left = &inputs[results.len()..];
        for k in 0..results.len() {
            let Some(with) = crate::summary::add(&results[k], shares[k]) else { continue };
            let plain = std::mem::replace(&mut results[k], with);
            let trial = batch_answer(&results, left, max);
            if trial.len() <= max {
                answer = trial;
            } else {
                results[k] = plain;
            }
        }
        Ok(answer)
    })
    .await
}

/// `{"results": [...]}` from results already serialized, and with inputs `left` not answered, `left_out` (as many
/// as fit `max`, the rest counted) and a `hint`.
fn batch_answer(results: &[String], left: &[String], max: usize) -> String {
    let mut out = format!("{{\"results\":[{}]", results.join(","));
    // Complete when every input was answered and every answer is complete; each partial answer is named here, its
    // own `incomplete` says why.
    let mut incomplete: Vec<kc_store::answer::Incomplete> = Vec::new();
    for (k, r) in results.iter().enumerate() {
        let v: Value = serde_json::from_str(r).unwrap_or(Value::Null);
        if v["complete"].as_bool() == Some(false) || v.get("error").is_some() {
            // Its first entry's reason and code, and its policy where it is `withheld`; a refused input's code.
            let first = crate::contract::incomplete_of(&v).into_iter().next();
            let reason = first.as_ref().map_or(kc_store::answer::Reason::Failed, |i| i.reason);
            let input = v["input"].as_str().unwrap_or("");
            let what = if v.get("error").is_some() { "failed" } else { "incomplete: see its `incomplete`" };
            let mut entry = kc_store::answer::Incomplete::new(reason, format!("results[{k}] ({input}): {what}"));
            entry.code = first.as_ref().and_then(|i| i.code).or_else(|| serde_json::from_value(v["code"].clone()).ok());
            entry.privacy = first.and_then(|i| i.privacy).or_else(|| serde_json::from_value(v["privacy"].clone()).ok());
            incomplete.push(entry);
        }
    }
    if !left.is_empty() {
        incomplete.push(
            kc_store::answer::Incomplete::new(kc_store::answer::Reason::Budget, format!("inputs: {} not run to fit max_chars={max}", left.len()))
                .with_hint("call again with the inputs in left_out, or pass a larger max_chars"),
        );
    }
    // As every compact answer, the batch says whether it is complete.
    if incomplete.is_empty() {
        out.push_str(",\"complete\":true");
    } else {
        out.push_str(&format!(",\"complete\":false,\"incomplete\":{}", serde_json::to_string(&incomplete).unwrap_or_default()));
    }
    if !left.is_empty() {
        let hint = format!(
            "{} of {} inputs left out to fit max_chars={max}: call again with them (left_out), or pass max_chars (up to {})",
            left.len(),
            results.len() + left.len(),
            MAX_CHARS * 2
        );
        let mut listed = left.len();
        loop {
            let named = serde_json::to_string(&left[..listed]).unwrap_or_default();
            let more = if listed < left.len() { format!(",\"left_out_more\":{}", left.len() - listed) } else { String::new() };
            let tail = format!(",\"truncated\":true,\"left_out\":{named}{more},\"hint\":{}}}", Value::from(hint.as_str()));
            if out.len() + tail.len() <= max || listed == 0 {
                out.push_str(&tail);
                return out;
            }
            listed -= 1;
        }
    }
    out.push('}');
    out
}

/// The inputs of a tool taking one (`one`) or several (`many`).
fn inputs(one: Option<String>, many: Vec<String>, what: &str) -> Result<Vec<String>, String> {
    let all: Vec<String> = one.into_iter().chain(many).filter(|s| !s.trim().is_empty()).collect();
    if all.is_empty() {
        return Err(format!("give {what}"));
    }
    Ok(all)
}

/// Says what an empty answer searched, and how to get the rest of a list cut short by `limit`: `note` and `hint`.
fn explain(mut env: Value, searched: &str, limit_param: &str) -> Value {
    let repos: Vec<String> = match env["repos"].as_array() {
        Some(list) => list.iter().filter_map(|r| r["repo"].as_str()).map(repo_label).collect(),
        None => env["repo"].as_str().map(repo_label).into_iter().collect(),
    };
    let items = env["items"].as_array().map_or(0, Vec::len);
    let empty = items == 0 || env["items"].as_array().is_some_and(|list| list.iter().all(|i| i["nodes"].as_array().is_some_and(Vec::is_empty)));
    if empty && env.get("note").is_none() {
        env["note"] = Value::from(format!("{searched} in {}", if repos.is_empty() { "the repos searched".into() } else { repos.join(", ") }));
    }
    if let (Some(total), true) = (env["total"].as_u64(), env["truncated"].as_bool() == Some(true))
        && total > items as u64
        && !limit_param.is_empty()
    {
        let shown = format!("showing {items} of {total}; raise {limit_param} for more");
        // A capped count says so itself: keep it.
        env["hint"] = match env["hint"].as_str() {
            Some(h) if env["capped"].as_bool() == Some(true) => Value::from(format!("{shown}; {h}")),
            _ => Value::from(shown),
        };
    }
    // Lists inside the items that carry their own totals (`partners_total`, `reviewers_total`, `areas_total`).
    let mut hints = Vec::new();
    let mut inner_empty = items > 0;
    for item in env["items"].as_array().cloned().unwrap_or_default() {
        for (list, total) in [("partners", "partners_total"), ("reviewers", "reviewers_total"), ("areas", "areas_total")] {
            let shown = item[list].as_array().map_or(0, Vec::len) as u64;
            if let Some(t) = item[total].as_u64() {
                if t > shown {
                    hints.push(format!("{list}: showing {shown} of {t}; raise {limit_param} for more"));
                }
                inner_empty &= t == 0;
            } else {
                inner_empty &= !item[list].is_array() || shown == 0;
            }
        }
    }
    if !hints.is_empty() {
        env["truncated"] = Value::Bool(true);
        env["hint"] = Value::from(hints.join("; "));
    }
    if inner_empty && env.get("note").is_none() && ["partners", "reviewers"].iter().any(|l| env["items"][0][*l].is_array()) {
        env["note"] = Value::from(format!("{searched} in {}", if repos.is_empty() { "the repos searched".into() } else { repos.join(", ") }));
    }
    env
}

/// Refuses an empty answer to a place (`file:line`, `path#name`) that named no definition, saying what didn't resolve
/// (see [`ops::unresolved_place`]): `complete` with no items would read as "nothing there".
fn placed(symbol: Option<&str>, empty: bool, scope: &Scope) -> anyhow::Result<()> {
    if empty
        && let Some(s) = symbol
        && let Some(why) = ops::unresolved_place(s, scope)?
    {
        anyhow::bail!(Refusal::new(Code::NotFound, why));
    }
    Ok(())
}

/// Why a search with a paths or lang filter (`filtered`) searched nothing: its filter matched no file. Refused with
/// the search's note, rather than answered `complete` with no items.
fn searched_nothing(env: &Value, filtered: bool) -> Option<String> {
    (filtered && env["stats"]["searchable_files"].as_u64() == Some(0))
        .then(|| env["note"].as_str().unwrap_or("paths and lang matched 0 files: nothing was searched").to_string())
}

fn repo_label(path: &str) -> String {
    Path::new(path).file_name().map_or(path.into(), |n| n.to_string_lossy().into_owned())
}

/// Runs `knowcode search --json` in a child process and returns its envelope. A search over every repo churns
/// through file buffers and SQLite pages, and the allocator keeps the pages it touched resident in whichever
/// process did the work; a child returns them all when it exits, so the server stays small however big the search.
///
/// The child has the search deadline (see [`kc_store::limits`]) and is killed when the call is cancelled. Killed at
/// its deadline, it answers no items, `complete: false` and an `incomplete` entry with reason `deadline`.
/// Starts the line a child (see [`search_in_child`]) gives its refusal's code on, after its message.
pub const REFUSAL_LINE: &str = "knowcode-refusal: ";

fn search_in_child(args: &[String]) -> anyhow::Result<Value> {
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.args(args);
    // Its envelope keeps what cursors are made from, for the size cap here to renew them.
    cmd.env(crate::KEEP_RESUME, "1");
    let out = match kc_store::limits::output(cmd, kc_store::limits::Kind::Search, &format!("knowcode {}", args[0]), None) {
        Ok(out) => out,
        Err(e) => match e.downcast_ref::<kc_store::limits::Cut>() {
            // Said by `with_cuts`, from the recorded cut.
            Some(cut) if cut.reason == "deadline" => {
                return Ok(serde_json::json!({ "items": [], "total": 0, "truncated": false, "complete": false }));
            }
            _ => return Err(e),
        },
    };
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let (msg, coded) = match stderr.trim_end().rsplit_once('\n') {
            Some((msg, last)) if last.starts_with(REFUSAL_LINE) => (msg, serde_json::from_str::<Value>(&last[REFUSAL_LINE.len()..]).ok()),
            _ => (stderr.as_ref(), None),
        };
        let msg = msg.trim().trim_start_matches("knowcode: ").to_string();
        if let Some(c) = coded
            && let Ok(code) = serde_json::from_value::<Code>(c["code"].clone())
        {
            let privacy = serde_json::from_value(c["privacy"].clone()).ok();
            anyhow::bail!(Refusal { code, message: msg, privacy });
        }
        anyhow::bail!("{msg}");
    }
    Ok(serde_json::from_slice(&out.stdout)?)
}

/// A workspace-wide answer (`workspace`), with the workspace's repos not indexed yet said in it (see
/// [`ops::workspace_repos`]): whatever form the answer takes, even one repo's.
fn noted(workspace: bool, mut env: Value) -> Value {
    if workspace {
        ops::add_unindexed(&mut env);
    }
    env
}

/// Hands freed pages back to the OS after each call, so resident memory tracks what is in use rather than the
/// high-water mark of the biggest call.
fn release_memory() {
    #[cfg(target_os = "macos")]
    {
        unsafe extern "C" {
            fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
        }
        // SAFETY: a null zone means all zones; goal 0 means release as much as possible. No pointers are kept.
        unsafe {
            malloc_zone_pressure_relief(std::ptr::null_mut(), 0);
        }
    }
}

/// The answer as the server outputs it: [`cap_full`], said in its compact form (see [`crate::compact`]).
pub(crate) fn cap(env: Value, max: usize) -> String {
    let names = crate::compact::Names::load();
    let out = crate::compact::output(&cap_full_named(env, max, &names), &names);
    // A partial answer says how, and what it cost (see `summary`), in the room the cap left: never in place of a row.
    crate::summary::add(&out, max).unwrap_or(out)
}

/// [`cap`] without the summary: a batch's results, which get theirs only in the room the whole batch leaves.
fn cap_plain(env: Value, max: usize) -> String {
    let names = crate::compact::Names::load();
    crate::compact::output(&cap_full_named(env, max, &names), &names)
}

/// Drops trailing list items (from `items`, or from the largest list inside each item) until the JSON fits, as
/// measured in its compact form. Each list cut says how many items it lost, by its path (`omitted`), and `hint` says
/// how to get them. The full envelope: cursors as they are.
#[cfg(test)]
pub(crate) fn cap_full(env: Value, max: usize) -> Value {
    cap_full_named(env, max, &crate::compact::Names::load())
}

fn cap_full_named(mut env: Value, max: usize, names: &crate::compact::Names) -> Value {
    // What cursors are made from is never output: measured without it, kept aside to renew the cursors of lists cut.
    let mut resume = crate::cursor::take(&mut env);
    // Said once: the compact form has no copy of the first item (see `compact::once`), so the cap cuts none.
    crate::compact::once(&mut env);
    crate::contract::finish(&mut env);
    // The compact form is measured exactly when the full form, less what compacting saved at the last exact
    // measure, says it may fit: compacting a clone at every item cut would cost more than the cut.
    let mut len = crate::compact::measure(&env, names).len();
    let mut saved = env.to_string().len().saturating_sub(len);
    let mut guard = 0;
    let mut lost: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    while len > max && guard < 10_000 {
        guard += 1;
        // Rows' detail dropped (a statement's lines, whose cite then compacts further than the full form says): measured
        // exactly before any row is cut.
        let mut detail = false;
        match shrink(&mut env) {
            // Rows' detail dropped: no row lost.
            Some(path) if path.is_empty() => detail = true,
            Some(path) => {
                if let Some(r) = resume.as_mut() {
                    crate::cursor::cut(&mut env, r, &path);
                }
                refuse_partial_rename(&mut env, &path);
                *lost.entry(path).or_default() += 1;
            }
            None => break,
        }
        // A workspace `kc_status` answers with an array of per-repo objects.
        if let Some(obj) = env.as_object_mut()
            && !lost.is_empty()
        {
            obj.insert("truncated".into(), Value::Bool(true));
        }
        // Paged answers: the next page starts after the last item that survived.
        if let (Some(offset), Some(items)) = (env["offset"].as_u64(), env["items"].as_array()) {
            env["next_offset"] = Value::from(offset + items.len() as u64);
        }
        if let Some(obj) = env.as_object_mut()
            && !lost.is_empty()
        {
            let total: u64 = lost.values().sum();
            obj.insert("omitted".into(), serde_json::json!(lost));
            let paged = if obj.contains_key("next_offset") { ", or page with offset=next_offset" } else { "" };
            obj.insert(
                "hint".into(),
                Value::from(format!("{total} list items cut to fit max_chars={max}; pass max_chars (up to {MAX_CHARS}) or a smaller limit{paged}")),
            );
        }
        crate::contract::finish(&mut env);
        let full = env.to_string().len();
        len = full.saturating_sub(saved);
        if len <= max || detail {
            len = crate::compact::measure(&env, names).len();
            saved = full.saturating_sub(len);
        }
    }
    // A context bundle's own lists, counted as they are after the cap.
    if env["items"].as_array().is_some_and(|items| items.iter().any(|i| i.get("callers_summary").is_some())) {
        env = context_hints(env);
        crate::contract::finish(&mut env);
    }
    env
}

/// A rename preview whose sites the size cap cut can't be applied as it is: refused, saying why. Mentions (not
/// edited) and other lists don't refuse it.
fn refuse_partial_rename(env: &mut Value, path: &str) {
    if env.get("new_name").is_none() || !matches!(path, "/items" | "/cross_repo") {
        return;
    }
    env["refused"] = Value::Bool(true);
    let why = "partial preview: sites were cut to fit max_chars, so it can't be applied as it is; get the rest with the cursor, or pass max_chars";
    if let Some(reasons) = env["reasons"].as_array_mut()
        && !reasons.iter().any(|r| r == why)
    {
        reasons.push(Value::from(why));
        if let Some(codes) = env["codes"].as_array_mut() {
            codes.push(Value::from(Code::Cut.as_str()));
        }
    }
}

/// What the size cap never cuts: a rename's `group` and a path's `steps` (cut short, they would mislead), cursors
/// (a cut cursor continues nothing), and what the answer says about itself (`incomplete`, `more`, `rest`, `provenance`,
/// and a deploy answer's `ci_unread` and `ci_cut`, which its `incomplete` entries are made from).
fn untouchable(key: &str) -> bool {
    // `none_found` says what is missing; `overloads` is what rows' `overload` numbers point at.
    matches!(key, "group" | "steps" | "cursor" | "incomplete" | "more" | "rest" | "provenance" | "omitted" | "ci_unread" | "ci_cut" | "none_found" | "overloads")
        || key.ends_with("_cursor")
}

/// Lists whose rows the size cap cuts whole, never inside: a deploy chain or a CI include cut short would mislead,
/// and only the list itself continues by cursor.
fn atomic(key: &str) -> bool {
    matches!(key, "deploy_chains" | "ci_includes" | "deployed_by")
}

/// Lists that hold what a question about a type or an endpoint asks for: its implementations and derived types, its DI
/// registrations, the links reaching it (HTTP, bus, SQL), a search's other files. The size cap cuts them last, after
/// every other list and the source (see [`shrink`]): a conformer cut to fit was the one item an agent's grep found
/// that an answer had and didn't show.
fn answer_list(key: &str) -> bool {
    matches!(
        key,
        "conformers"
            | "conformers_indirect"
            | "conformers_elsewhere"
            | "di_implementers"
            | "implementers"
            | "derived"
            | "through"
            | "subclasses"
            | "di"
            | "registrations"
            | "registered_as"
            | "bound"
            | "http"
            | "called_over_http"
            | "by"
            | "bus"
            | "sql"
            | "links"
            | "other_files"
    )
}

/// The rank of the list at `path` in what the cap cuts first: a deploy answer's long tails
/// (`items[0].ansible.ci_includes`, …) before its deploy steps (see `ops::DEPLOY_LISTS`), then any other list, then
/// the lists an answer is about (see [`answer_list`]).
fn tier(path: &str) -> u8 {
    let Some((parent, key)) = path.rsplit_once('/') else { return 1 };
    let tail = matches!(key, "ci_includes" | "ci_skipped" | "ci_templates" | "own");
    if tail && parent.ends_with("/ansible") {
        2
    } else if answer_list(key) {
        0
    } else {
        1
    }
}

/// Detail an answer list's rows can do without, dropped before a row of one is cut: the row keeps its name, kind and
/// `file:line`.
const ROW_DETAIL: &[&str] = &["signature", "doc", "attrs", "start_line", "end_line", "complexity", "cite"];

/// The last row in `v` whose cite quotes the rest of its site's statement (`at` a range around the row's `site`, else
/// its `line`), quoting the site's line alone again; `false` when no row does.
fn unstate(v: &mut Value) -> bool {
    match v {
        Value::Array(a) => a.iter_mut().rev().any(unstate),
        Value::Object(m) => {
            if m.iter_mut().rev().filter(|(k, _)| !untouchable(k)).any(|(_, x)| unstate(x)) {
                return true;
            }
            let line = match m.get("site").and_then(Value::as_str) {
                Some(site) => site.rsplit_once(':').and_then(|(_, l)| l.parse::<u64>().ok()),
                None => m.get("line").and_then(Value::as_u64),
            };
            let Some(Value::Object(c)) = m.get_mut("cite") else { return false };
            let (Some(line), Some(at), Some(text)) = (line, c.get("at").and_then(Value::as_str), c.get("text").and_then(Value::as_str)) else {
                return false;
            };
            let Some((file, (first, last))) = at.rsplit_once(':').and_then(|(f, r)| Some((f, r.split_once('-')?))) else { return false };
            let (Ok(first), Ok(last)) = (first.parse::<u64>(), last.parse::<u64>()) else { return false };
            if !(first..=last).contains(&line) {
                return false;
            }
            let Some(own) = text.lines().nth((line - first) as usize).map(|l| l.trim().to_string()) else { return false };
            let at = format!("{file}:{line}");
            // What the cap said of the site's line stays; what it said of the statement goes.
            let cut: Vec<String> = c.get("cut").and_then(Value::as_str).unwrap_or("").split("; ").filter(|p| p.starts_with(&format!("line {line} "))).map(str::to_string).collect();
            c.insert("at".into(), Value::from(at));
            c.insert("text".into(), Value::from(own));
            if cut.is_empty() {
                c.remove("cut");
            } else {
                c.insert("cut".into(), Value::from(cut.join("; ")));
            }
            true
        }
        _ => false,
    }
}

/// Trims one item from the longest list, returning its path (`/items`, `/items/0/callers`); or shortens a long
/// string, returning `/…`; or drops the detail of answer lists' rows, returning an empty path; `None` when nothing is
/// left to trim. A rename's `group` (the members renamed together) and a path's `steps` are never cut: a path or a
/// group cut short would mislead; the list holding them is cut instead. A deploy answer's tails go before the rest,
/// and its chains and includes are cut whole (see [`atomic`]), in the answer and in a page of one of those lists. The
/// lists an answer is about (see [`answer_list`]) go last: every other list down to one row, the source shortened and
/// their rows' detail dropped first.
fn shrink(v: &mut Value) -> Option<String> {
    type Found<'a> = (&'a mut Vec<Value>, String, (u8, usize));
    // Prefer trimming the longest array anywhere in the response; a list inside an answer list's row ranks with it.
    fn longest(v: &mut Value, path: String, whole: bool, answer: bool) -> Option<Found<'_>> {
        let mut best: Option<Found<'_>> = None;
        match v {
            Value::Array(a) => {
                if a.len() > 1 {
                    let rank = (if answer { 0 } else { tier(&path) }, a.len());
                    return Some((a, path, rank));
                }
                if whole {
                    return None;
                }
                for (i, x) in a.iter_mut().enumerate() {
                    if let Some(inner) = longest(x, format!("{path}/{i}"), false, answer)
                        && inner.2 > best.as_ref().map_or((0, 1), |b| b.2)
                    {
                        best = Some(inner);
                    }
                }
            }
            Value::Object(m) => {
                // A page of a list cut whole (`kc_more`): its items are that list's rows.
                let page_whole = path.is_empty() && m.get("list").and_then(Value::as_str).is_some_and(|l| atomic(l.rsplit('.').next().unwrap_or("")));
                for (k, x) in m.iter_mut().filter(|(k, _)| !untouchable(k)) {
                    let whole = atomic(k) || (page_whole && k == "items");
                    if let Some(inner) = longest(x, format!("{path}/{k}"), whole, answer || answer_list(k))
                        && inner.2 > best.as_ref().map_or((0, 1), |b| b.2)
                    {
                        best = Some(inner);
                    }
                }
            }
            _ => {}
        }
        best
    }
    // Shortens a long string (a source span); `answers` too in the lists an answer is about.
    fn trim_strings(v: &mut Value, answers: bool) -> bool {
        match v {
            Value::String(s) if s.len() > 400 => {
                let mut cut = s.len() / 2;
                while !s.is_char_boundary(cut) {
                    cut -= 1;
                }
                s.truncate(cut);
                s.push('…');
                true
            }
            Value::Array(a) => a.iter_mut().any(|x| trim_strings(x, answers)),
            Value::Object(m) => m.iter_mut().filter(|(k, _)| !untouchable(k) && (answers || !answer_list(k))).any(|(_, x)| trim_strings(x, answers)),
            _ => false,
        }
    }
    // Drops the detail of the rows of the lists an answer is about (see [`ROW_DETAIL`]).
    fn slim(v: &mut Value, list: bool) -> bool {
        match v {
            Value::Array(a) => a.iter_mut().fold(false, |any, x| {
                let dropped = list && x.as_object_mut().is_some_and(|row| ROW_DETAIL.iter().fold(false, |d, k| row.remove(*k).is_some() | d));
                slim(x, false) | dropped | any
            }),
            Value::Object(m) => m.iter_mut().filter(|(k, _)| !untouchable(k)).fold(false, |any, (k, x)| slim(x, answer_list(k)) | any),
            _ => false,
        }
    }
    // The last row of a list an item holds (`/items/0/callers`): not of the answer's own `items`, and nothing inside a
    // row of a list.
    fn last_row(v: &mut Value, path: String) -> Option<String> {
        match v {
            Value::Array(a) if path.is_empty() || path == "/items" => a.iter_mut().enumerate().find_map(|(i, x)| last_row(x, format!("{path}/{i}"))),
            Value::Array(a) if a.len() == 1 => {
                a.pop();
                Some(path)
            }
            Value::Array(_) => None,
            Value::Object(m) => {
                m.iter_mut().filter(|(k, _)| !untouchable(k) && !answer_list(k) && !atomic(k)).find_map(|(k, x)| last_row(x, format!("{path}/{k}")))
            }
            _ => None,
        }
    }
    // The rest of a site's statement goes before any row.
    if unstate(v) {
        return Some(String::new());
    }
    if let Some((a, path, (tier, _))) = longest(v, String::new(), false, false)
        && tier > 0
    {
        a.pop();
        return Some(path);
    }
    // Only the lists the answer is about are left to cut: the source, their rows' detail and the other lists' last
    // rows go first.
    if trim_strings(v, false) {
        return Some("/…".to_string());
    }
    if slim(v, false) {
        return Some(String::new());
    }
    // A page of one list (`kc_more`) is that list's rows only.
    if v.get("list").is_none()
        && let Some(path) = last_row(v, String::new())
    {
        return Some(path);
    }
    if let Some((a, path, _)) = longest(v, String::new(), false, false) {
        a.pop();
        return Some(path);
    }
    // Last resort: shorten long strings anywhere.
    trim_strings(v, true).then(|| "/…".to_string())
}


/// The cite mode a `cite` parameter asks for (see [`kc_store::answer::CiteMode`]).
fn cite_mode(cite: Option<&str>) -> Result<kc_store::answer::CiteMode, String> {
    cite.unwrap_or_default().parse()
}

/// Sets the cite mode for the query the thread runs.
fn cite(mode: kc_store::answer::CiteMode) {
    kc_store::limits::current().set_cite(mode);
}

/// Reference kinds from `kinds` and `edges` (edge names map to reference kinds: calls → call, uses → type_ref).
fn ref_kinds(kinds: &[String], edges: &[String]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = kinds.iter().flat_map(|k| k.split(',')).map(|k| k.trim().to_string()).filter(|k| !k.is_empty()).collect();
    for e in edges.iter().flat_map(|e| e.split(',')).map(str::trim).filter(|e| !e.is_empty()) {
        let k = match e {
            "calls" | "call" => "call",
            "instantiates" | "instantiate" => "instantiate",
            "uses" | "type_ref" => "type_ref",
            "member" => "member",
            "reads" | "read" => "read",
            "writes" | "write" => "write",
            other => return Err(format!("edges for references are calls, instantiates, uses, member, reads, writes; not {other}")),
        };
        out.push(k.to_string());
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// A context bundle's own cut lists, said out loud: callers, callees and members shown of how many, and a source cut
/// at the budget.
fn context_hints(mut env: Value) -> Value {
    if let Some(items) = env["items"].as_array_mut() {
        for item in items {
            let mut hints = Vec::new();
            let name = item["symbol"]["name"].as_str().unwrap_or("").to_string();
            let shown = |k: &str| item[k].as_array().map_or(0, Vec::len) as u64;
            if let Some(total) = item["callers_summary"]["total"].as_u64()
                && total > shown("callers")
            {
                // An action's callers over HTTP are `kc_impact`'s, not `kc_refs`'s (references by name).
                let serves = item["http"].as_array().is_some_and(|h| h.iter().any(|e| e["role"] == "serves"));
                hints.push(if serves {
                    let qualified = item["symbol"]["container"].as_str().map_or(name.clone(), |c| format!("{c}.{name}"));
                    format!("callers: {} of {total} in code shown; kc_impact {qualified} lists every caller by hop, over HTTP too", shown("callers"))
                } else {
                    format!("callers: {} of {total} shown; kc_refs {name} lists them all", shown("callers"))
                });
            }
            if let Some(total) = item["callees_total"].as_u64()
                && total > shown("callees")
            {
                hints.push(format!("callees: {} of {total} shown; raise budget (and max_chars) for more", shown("callees")));
            }
            if let Some(total) = item["members_total"].as_u64()
                && total > shown("members")
            {
                hints.push(format!("members: {} of {total} shown; raise budget (and max_chars) for more", shown("members")));
            }
            if item["source_truncated"].as_bool() == Some(true) {
                let alt = if item["source_mode"] == "skeleton" { "" } else { " or pass source=skeleton" };
                hints.push(format!("source cut at the budget; raise budget{alt}"));
            }
            if !hints.is_empty() {
                item["hints"] = serde_json::json!(hints);
            }
        }
    }
    env
}

#[tool_router]
impl KnowcodeServer {
    /// The server for `default`, advertising the tools the profile `tools` names (see [`profile`]).
    pub fn new(default: Scope, tools: &str) -> Result<Self, String> {
        let mut router = ToolRouter::new();
        let mut schemas = std::collections::BTreeMap::new();
        let mut descriptions = std::collections::BTreeMap::new();
        for mut route in Self::tool_router() {
            // The full schema documents every parameter in help; the advertised one only the common ones.
            let full = bounded(&route.attr.name, ranged(&route.attr.name, slim(&route.attr.input_schema)));
            route.attr.input_schema = std::sync::Arc::new(advertised(&route.attr.name, &full));
            if ALWAYS_LOADED.contains(&route.attr.name.as_ref()) || route.attr.name == "kc" {
                let mut meta = rmcp::model::JsonObject::new();
                meta.insert("anthropic/alwaysLoad".into(), Value::Bool(true));
                route.attr.meta = Some(rmcp::model::MetaObject(meta));
            }
            schemas.insert(route.attr.name.to_string(), full);
            descriptions.insert(route.attr.name.to_string(), route.attr.description.as_deref().unwrap_or_default().to_string());
            router.add_route(route);
        }
        // `kc` is the `one` profile's alone: `all` is every other tool.
        let all: Vec<String> = schemas.keys().filter(|k| *k != "kc").cloned().collect();
        let on = profile(tools, &all)?;
        let hidden: Vec<String> = schemas.keys().filter(|t| !on.contains(t)).cloned().collect();
        for t in &hidden {
            router.disable_route(t.clone());
        }
        let one = on.iter().any(|t| t == "kc");
        Ok(Self {
            default,
            tool_router: router,
            schemas: std::sync::Arc::new(schemas),
            descriptions: std::sync::Arc::new(descriptions),
            hidden: std::sync::Arc::new(hidden),
            one,
            restriction: None,
        })
    }

    #[tool(description = "Docs on any tool, parameter or concept; no topic: the index.")]
    async fn kc_help(&self, Parameters(p): Parameters<HelpParams>) -> Result<String, ToolError> {
        self.help(p.topic.as_deref()).map_err(|e| ToolError::new(Code::InvalidArgument, e))
    }

    #[tool(description = "The symbols enclosing a file:line, innermost first: a stack frame's code.")]
    async fn kc_symbol_at(&self, Parameters(p): Parameters<SymbolAtParams>) -> Result<String, ToolError> {
        let scope = if Path::new(&p.file).is_absolute() && p.common.repo.is_none() {
            Scope { repos: Vec::new(), ..self.default.clone() }
        } else {
            self.scope(&p.common, false)?
        };
        run(p.common.max_chars, move || {
            let what = format!("no symbol encloses {}:{}", p.file, p.line);
            Ok(explain(ops::at(&p.file, p.line, &scope)?.env, &what, ""))
        })
        .await
    }

    #[tool(description = "Where a name is defined (name, prefix or `Type.member`): file:line and signature.")]
    async fn kc_find(&self, Parameters(p): Parameters<FindParams>) -> Result<String, ToolError> {
        let scope = self.scope(&p.common, false)?;
        run(p.common.max_chars, move || {
            let env = ops::find(&p.query, p.kind.as_deref(), p.limit.unwrap_or(10), &scope)?.env;
            let what = format!("no definition matches {:?}{}", p.query, p.kind.as_deref().map(|k| format!(" of kind {k}")).unwrap_or_default());
            Ok(explain(env, &what, "limit"))
        })
        .await
    }

    #[tool(description = "Files for a concept, bug report or ticket, ranked. First, when you don't know the names.")]
    async fn kc_locate(&self, Parameters(p): Parameters<LocateParams>) -> Result<String, ToolError> {
        let scope = self.scope(&p.common, p.workspace)?;
        let limit = p.limit.unwrap_or(10);
        let locate = move |q: &str| -> anyhow::Result<Value> {
            let what = format!("no file's path, symbols, docs, referenced names, text or commit subjects contain a word of {q:?} (filler words dropped)");
            Ok(noted(p.workspace, explain(ops::locate(q, limit, &scope)?.env, &what, "limit")))
        };
        match (p.query, p.queries) {
            (Some(q), many) if many.is_empty() => run(p.common.max_chars, move || locate(&q)).await,
            (one, many) => run_batch(p.common.max_chars, inputs(one, many, "query or queries")?, locate).await,
        }
    }

    #[tool(description = "One bounded answer for a goal in words: the files involved, what depends on them, the tests to run, what the index can't settle.")]
    async fn kc_task(&self, Parameters(p): Parameters<TaskParams>) -> Result<String, ToolError> {
        let scope = self.scope(&p.common, false)?;
        let params = crate::task::TaskParams {
            goal: p.goal,
            files: TASK_FILES.clamp(p.files),
            dependents: TASK_DEPENDENTS.clamp(p.dependents),
            tests: TASK_TESTS.clamp(p.tests),
        };
        run(p.common.max_chars, move || {
            let what = format!("no file matches a word of {:?} (filler words dropped)", params.goal);
            Ok(explain(crate::task::task(&params, &scope)?.env, &what, ""))
        })
        .await
        .map(|text| crate::task::reading_order(&text))
    }

    #[tool(description = "Every reference to a name, resolved, across repos. What calls X, by hop or across HTTP: kc_impact on X's file:line.")]
    async fn kc_refs(&self, Parameters(p): Parameters<RefsParams>) -> Result<String, ToolError> {
        let scope = self.scope(&p.common, false)?;
        let limit = p.limit.unwrap_or(20);
        let kinds = ref_kinds(&p.kinds, &p.edges)?;
        let mode = cite_mode(p.cite.as_deref())?;
        let refs = move |name: &str| -> anyhow::Result<Value> {
            cite(mode);
            let what = format!("no reference to {name}{}", if kinds.is_empty() { String::new() } else { format!(" of kind {}", kinds.join(", ")) });
            Ok(explain(ops::refs(name, &kinds, limit, &scope)?.env, &what, "limit"))
        };
        match (p.name, p.names) {
            (Some(n), many) if many.is_empty() => run(p.common.max_chars, move || refs(&n)).await,
            (one, many) => run_batch(p.common.max_chars, inputs(one, many, "name or names")?, refs).await,
        }
    }

    #[tool(description = "One symbol: source, members, callers, callees and cross-repo links (HTTP, DI, bus, SQL). A repo's name: how it deploys.")]
    async fn kc_context(&self, Parameters(p): Parameters<ContextParams>) -> Result<String, ToolError> {
        let scope = self.scope(&p.common, false)?;
        let source: kc_store::skeleton::SourceMode = p.source.as_deref().unwrap_or("auto").parse().map_err(|e| format!("{e:#}"))?;
        let symbols = inputs(p.symbol, p.symbols, "symbol or symbols")?;
        let n = symbols.len();
        // Several symbols share the budget; each still gets enough for a useful bundle.
        let budget = (p.budget.unwrap_or(1200) / n).max(600);
        let limit = p.limit;
        let mode = cite_mode(p.cite.as_deref())?;
        let context = move |sym: &str| -> anyhow::Result<Value> {
            cite(mode);
            let what = format!("no definition or table named {sym}");
            let env = ops::context_limited(sym, budget, source, limit, &scope)?.env;
            placed(Some(sym), env["items"].as_array().is_some_and(Vec::is_empty), &scope)?;
            Ok(explain(env, &what, ""))
        };
        // The bundle's own size: five characters a budget token (source, members, callers and callees with their
        // bookkeeping), at least [`CONTEXT_CHARS`].
        let max = p.common.max_chars.or(Some((budget * n * 5).max(CONTEXT_CHARS)));
        if n == 1 {
            let sym = symbols.into_iter().next().unwrap_or_default();
            run(max, move || context(&sym)).await
        } else {
            run_batch(max, symbols, context).await
        }
    }

    #[tool(description = "Call tree of a symbol: callers (`in`), callees (`out`) or both, across DI, HTTP, bus and SQL.")]
    async fn kc_trace(&self, Parameters(p): Parameters<TraceParams>) -> Result<String, ToolError> {
        let scope = self.scope(&p.common, false)?;
        let dir = p.direction.clone().unwrap_or_else(|| "both".into());
        let tree = p.walk.tree()?;
        let filter = p.walk.filter();
        let what = format!("no definition named {}", p.symbol);
        // Asked deeper than a trace goes, the answer says so (`ops::trace`); the notes go by the depth walked.
        let asked = p.depth.unwrap_or(TRACE_DEPTH.default as u8).max(1);
        let depth = asked.min(kc_store::trace::MAX_TRACE_DEPTH);
        let limit = p.limit.unwrap_or(25);
        let mode = cite_mode(p.walk.cite.as_deref())?;
        let trace = move || {
            cite(mode);
            let out = ops::trace(&p.symbol, &dir, asked, limit, &filter, &scope)?;
            placed(Some(&p.symbol), out.data.found.is_empty(), &scope)?;
            anyhow::Ok(out)
        };
        // Whether a node at the last depth leads further (a confident edge the walk would expand).
        let deeper = move |t: &kc_store::trace::Trace| t.nodes.iter().any(|n| n.depth == depth && matches!(n.confidence, "exact" | "unique_name"));
        if tree {
            run_text(p.common.max_chars, move || {
                let out = trace()?;
                let mut text = String::new();
                for (root, t) in &out.data.found {
                    text.push_str(&crate::render::trace_text(root, t, out.data.multi));
                }
                if let Some(n) = depth_note(depth, kc_store::trace::MAX_TRACE_DEPTH, out.data.found.iter().any(|(_, t)| deeper(t))) {
                    text.push_str(&format!("{n}\n"));
                }
                for (_, n) in out.data.found.iter().zip(&out.data.hidden).filter(|(_, n)| **n > 0) {
                    text.push_str(&format!("({n} symbols of other kinds hidden by kinds)\n"));
                }
                if out.data.found.is_empty() {
                    text = format!("{what}\n");
                }
                Ok((out.env, text))
            })
            .await
        } else {
            run(p.common.max_chars, move || {
                let out = trace()?;
                let mut env = explain(out.env, &what, "");
                let mut hints = Vec::new();
                let omitted: usize = out.data.found.iter().map(|(_, t)| t.omitted).sum();
                if out.data.found.iter().any(|(_, t)| t.truncated) {
                    hints.push(format!("node limit {limit} reached: at least {omitted} more neighbours left out; raise limit, lower depth, or filter with edges/kinds"));
                }
                if let Some(n) = depth_note(depth, kc_store::trace::MAX_TRACE_DEPTH, out.data.found.iter().any(|(_, t)| deeper(t))) {
                    hints.push(n.trim_matches(['(', ')']).to_string());
                }
                if !hints.is_empty() {
                    env["hint"] = Value::from(hints.join("; "));
                }
                Ok(env)
            })
            .await
        }
    }

    #[tool(description = "How symbol `from` reaches symbol `to`, across repos: the best paths, step by step.")]
    async fn kc_path(&self, Parameters(p): Parameters<PathParams>) -> Result<String, ToolError> {
        let scope = self.scope(&p.common, false)?;
        let to_repo = match &p.to_repo {
            Some(r) => self.scope(&Common { repo: Some(r.clone()), max_chars: None }, false)?.repos.into_iter().next(),
            None => None,
        };
        let tree = WalkParams { format: p.format.clone(), ..Default::default() }.tree()?;
        let mode = cite_mode(p.cite.as_deref())?;
        let path = move || {
            cite(mode);
            let opts = crate::walk::PathOptions {
                max_hops: p.max_hops.unwrap_or(PATH_HOPS.default as u8),
                max_nodes: p.max_nodes.unwrap_or(2000).max(1),
                max_repos: p.max_repos.unwrap_or(16).max(1),
                paths: p.paths.unwrap_or(PATH_COUNT.default),
                one_way: false,
                allow_ambiguous: p.allow_ambiguous,
                edges: ops::edge_filter(&p.edges, p.evidence.as_deref().unwrap_or_default())?,
                from_skip: 0,
                to_skip: 0,
            };
            crate::walk::path(&p.from, &p.to, to_repo.as_deref(), &opts, &scope)
        };
        if tree {
            run_text(p.common.max_chars, move || {
                let out = path()?;
                Ok((out.env, crate::render::path_text(&out.data)))
            })
            .await
        } else {
            run(p.common.max_chars, move || {
                let out = path()?;
                let mut env = out.env;
                let a = &out.data;
                if a.paths.is_empty() {
                    let kind = if a.allow_ambiguous { "no path" } else { "no confident path (allow_ambiguous follows ambiguous edges too)" };
                    env["note"] = Value::from(format!(
                        "{kind} from {} to {} within {} hops; explored {} nodes in {} repos",
                        a.from.qualified, a.to.qualified, a.max_hops, a.explored, a.repos_opened
                    ));
                }
                if a.truncated {
                    env["hint"] = Value::from("the node or repo budget stopped the search: raise max_nodes or max_repos");
                }
                Ok(env)
            })
            .await
        }
    }

    #[tool(description = "What runs end to end from an entry point (route, component, action, method), step by step across repos.")]
    async fn kc_flow(&self, Parameters(p): Parameters<FlowParams>) -> Result<String, ToolError> {
        let scope = self.scope(&p.common, false)?;
        let tree = WalkParams { format: p.format.clone(), ..Default::default() }.tree()?;
        let entry = p.entry.clone();
        let mode = cite_mode(p.cite.as_deref())?;
        let flow = move || {
            cite(mode);
            let opts = crate::flow::FlowOptions {
                depth: FLOW_DEPTH.clamp(p.depth.map(usize::from)) as u8,
                max_steps: p.limit.unwrap_or(40),
                max_repos: p.max_repos.unwrap_or(16).max(1),
                edges: ops::edge_filter(&p.edges, p.evidence.as_deref().unwrap_or_default())?,
                children: p.children.unwrap_or(crate::flow::DEFAULT_CHILDREN),
            };
            crate::flow::flow(&p.entry, &opts, &scope)
        };
        if tree {
            run_text(p.common.max_chars, move || {
                let out = flow()?;
                Ok((out.env, crate::render::flow_text(&entry, out.data.as_ref())))
            })
            .await
        } else {
            let what = format!("no Angular route, component or definition named {entry}");
            run(p.common.max_chars, move || {
                let out = flow()?;
                let mut env = explain(out.env, &what, "");
                if let Some(f) = &out.data {
                    if f.steps.is_empty() {
                        env["note"] = Value::from(format!("nothing followed from {entry}: no edge leaves it, or every one was left out ({:?})", f.left_out));
                    }
                    if f.truncated {
                        env["hint"] = Value::from("stopped at the step or repo limit: raise limit or max_repos, or lower depth");
                    }
                }
                Ok(env)
            })
            .await
        }
    }

    #[tool(description = "Who calls a method or an HTTP endpoint, from any repo, by hop (`depth` 2: callers' callers): what breaks if it changes. Bus and SQL too.")]
    async fn kc_impact(&self, Parameters(p): Parameters<ImpactParams>) -> Result<String, ToolError> {
        let scope = self.scope(&p.common, false)?;
        let tree = p.walk.tree()?;
        let order: ops::ImpactOrder = p.order.as_deref().unwrap_or("rank").parse().map_err(|e| format!("{e:#}"))?;
        let mode = cite_mode(p.walk.cite.as_deref())?;
        let what = match (&p.symbol, &p.file, &p.diff) {
            (Some(s), _, _) => format!("no definition named {s}"),
            (_, Some(f), _) => format!("no indexed file {f}"),
            (_, _, Some(d)) => format!("no changed lines in diff {d}"),
            _ => String::new(),
        };
        let params = ops::ImpactParams {
            symbol: p.symbol,
            file: p.file,
            diff: p.diff,
            depth: p.depth.unwrap_or(IMPACT_DEPTH.default as u8),
            limit: p.limit.unwrap_or(30),
            filter: p.walk.filter(),
            order,
            cochange: kc_index::vitals::Span { as_of: p.as_of, since: p.since },
            coupled_max: None,
        };
        if tree {
            run_text(p.common.max_chars, move || {
                cite(mode);
                let out = ops::impact(&params, &scope)?;
                placed(params.symbol.as_deref(), out.data.is_empty(), &scope)?;
                let mut text = crate::render::impact_text(&out.data, out.data.len() > 1);
                if out.data.is_empty() {
                    text = format!("{what}\n");
                } else if params.depth < 2 && out.data.iter().any(|r| !r.impact.affected.is_empty()) {
                    text.push_str(&format!("(one hop: direct dependents only; pass depth 2-{} for more)\n", IMPACT_DEPTH.max));
                }
                Ok((out.env, text))
            })
            .await
        } else {
            run(p.common.max_chars, move || {
                cite(mode);
                let out = ops::impact(&params, &scope)?;
                placed(params.symbol.as_deref(), out.data.is_empty(), &scope)?;
                let mut env = explain(out.env, &what, "");
                let mut hints = Vec::new();
                if out.data.iter().any(|r| r.impact.truncated) {
                    hints.push(format!("symbol limit {} reached; raise limit, lower depth, or filter with edges/kinds", params.limit));
                }
                if params.depth < 2 && out.data.iter().any(|r| !r.impact.affected.is_empty()) {
                    hints.push(format!("one hop: direct dependents only; pass depth 2-{} for more", IMPACT_DEPTH.max));
                }
                if !hints.is_empty() {
                    env["hint"] = Value::from(hints.join("; "));
                }
                Ok(env)
            })
            .await
        }
    }

    #[tool(description = "Dead-code candidates: symbols nothing reaches, with reasons.")]
    async fn kc_unused(&self, Parameters(p): Parameters<UnusedParams>) -> Result<String, ToolError> {
        let scope = self.scope(&p.common, false)?;
        run(p.common.max_chars, move || {
            let params = crate::refactor::UnusedParams {
                kinds: p.kinds,
                tier: p.tier.unwrap_or_else(|| "all".into()),
                limit: p.limit.unwrap_or(25),
            };
            Ok(crate::refactor::unused(&params, &scope)?.env)
        })
        .await
    }

    #[tool(description = "Every site renaming a C# or TypeScript symbol would edit, and collisions. Edits nothing.")]
    async fn kc_rename_preview(&self, Parameters(p): Parameters<RenamePreviewParams>) -> Result<String, ToolError> {
        let scope = self.scope(&p.common, false)?;
        run(p.common.max_chars, move || {
            let params = crate::refactor::RenameParams { symbol: p.symbol, new_name: p.new_name, allow_ambiguous: p.allow_ambiguous };
            Ok(crate::refactor::rename(&params, &scope)?.env)
        })
        .await
    }

    #[tool(description = "Files that change in the same commits as a file (git history).")]
    async fn kc_coupled(&self, Parameters(p): Parameters<CoupledParams>) -> Result<String, ToolError> {
        let scope = self.scope(&p.common, false)?;
        run(p.common.max_chars, move || {
            let what = format!("no co-change partner of {} in the last 365 days of history", p.file);
            let span = kc_index::vitals::Span { as_of: p.as_of, since: p.since };
            Ok(explain(ops::coupled(&p.file, p.limit.unwrap_or(10), &span, &scope)?.env, &what, "limit"))
        })
        .await
    }

    #[tool(description = "Tests to run for changed files, symbols or a diff, ranked: a likely sample.")]
    async fn kc_tests(&self, Parameters(p): Parameters<TestsParams>) -> Result<String, ToolError> {
        let scope = self.scope(&p.common, false)?;
        run(p.common.max_chars, move || {
            let what = format!("no test reaches, is named for, or changes with {}", p.diff.as_deref().map_or_else(|| p.targets.join(", "), |d| format!("diff {d}")));
            let params = ops::TestsParams { targets: p.targets, diff: p.diff, limit: p.limit.unwrap_or(10), evidence: p.evidence.unwrap_or_default() };
            Ok(explain(ops::tests(&params, &scope)?.env, &what, "limit"))
        })
        .await
    }

    #[tool(description = "Files that keep hurting: churn, fixes, complexity and coupling.")]
    async fn kc_hotspots(&self, Parameters(p): Parameters<HotspotsParams>) -> Result<String, ToolError> {
        let scope = self.scope(&p.common, p.workspace)?;
        run(p.common.max_chars, move || {
            let what = match &p.path {
                Some(path) => format!("no indexed file {path} with history"),
                None => "no file with history".to_string(),
            };
            Ok(noted(p.workspace, explain(ops::hotspots(p.path.as_deref(), p.top.unwrap_or(10), p.as_of.as_deref(), &scope)?.env, &what, "top")))
        })
        .await
    }

    #[tool(description = "Who wrote a file, directory or repo (git history): top authors, truck factor.")]
    async fn kc_ownership(&self, Parameters(p): Parameters<OwnershipParams>) -> Result<String, ToolError> {
        let scope = self.scope(&p.common, false)?;
        run(p.common.max_chars, move || {
            let what = format!("no file in history under {}", p.path.as_deref().unwrap_or("the repo"));
            let params = ops::OwnershipParams { path: p.path, without: p.without, depth: p.depth.unwrap_or(2), top: p.top.unwrap_or(10), as_of: None };
            Ok(explain(ops::ownership(&params, &scope)?.env, &what, "top"))
        })
        .await
    }

    #[tool(description = "A symbol's git history: commits that changed it, first and last, who wrote it, files changed with it.")]
    async fn kc_history(&self, Parameters(p): Parameters<HistoryParams>) -> Result<String, ToolError> {
        let scope = self.scope(&p.common, false)?;
        run(p.common.max_chars, move || {
            let what = format!("no definition {}", p.symbol);
            let params = crate::history::HistoryParams { symbol: p.symbol, limit: p.limit.unwrap_or(10), since: p.since };
            Ok(explain(crate::history::symbol_history(&params, &scope)?.env, &what, "limit"))
        })
        .await
    }

    #[tool(description = "Who could review a change (files or a diff), by recent lines written.")]
    async fn kc_reviewers(&self, Parameters(p): Parameters<ReviewersParams>) -> Result<String, ToolError> {
        let scope = self.scope(&p.common, false)?;
        run(p.common.max_chars, move || {
            let what = format!(
                "no author other than the change's own wrote {}",
                p.diff.as_deref().map_or_else(|| p.files.join(", "), |d| format!("the files changed in diff {d}"))
            );
            let params = ops::ReviewersParams { files: p.files, diff: p.diff, limit: p.limit.unwrap_or(5), as_of: None };
            Ok(explain(ops::reviewers(&params, &scope)?.env, &what, "limit"))
        })
        .await
    }

    #[tool(description = "What a repo is: languages, dependencies, entry points, key types and a map.")]
    async fn kc_overview(&self, Parameters(p): Parameters<OverviewParams>) -> Result<String, ToolError> {
        let scope = self.scope(&p.common, p.workspace)?;
        let budget = p.map_budget.unwrap_or(400);
        let max = p.common.max_chars.or(Some((budget * 4 + 4_000).max(DEFAULT_CHARS)));
        run(max, move || {
            let mut env = noted(p.workspace, ops::overview(&scope, p.focus.as_deref(), budget)?.env);
            if let Some(f) = &p.focus
                && env["items"].as_array().is_some_and(|items| items.iter().any(|i| i["map"]["focus_matched"] == Value::Bool(false)))
            {
                env["note"] = Value::from(format!("focus {f:?} matched no symbol name or path; the map is not biased"));
            }
            Ok(env)
        })
        .await
    }

    #[tool(description = "A repo's functional areas: files clustered by references, co-change and directory.")]
    async fn kc_areas(&self, Parameters(p): Parameters<AreasParams>) -> Result<String, ToolError> {
        let scope = self.scope(&p.common, false)?;
        run(p.common.max_chars, move || Ok(ops::areas(p.area.as_deref(), &scope)?.env)).await
    }

    #[tool(description = "Notes on symbols, each current, stale or orphaned.")]
    async fn kc_notes(&self, Parameters(p): Parameters<NotesParams>) -> Result<String, ToolError> {
        let scope = self.scope(&p.common, false)?;
        run(p.common.max_chars, move || Ok(ops::note_list(p.symbol.as_deref(), &scope)?.env)).await
    }

    #[tool(description = "WRITES LOCAL STATE: adds a note to a symbol, kept in KnowCode's data directory, never the repo.")]
    async fn kc_note_add(&self, Parameters(p): Parameters<NoteAddParams>) -> Result<String, ToolError> {
        let scope = self.scope(&p.common, false)?;
        run(p.common.max_chars, move || Ok(ops::note_add(&p.symbol, &p.text, p.tag.as_deref(), &scope)?.env)).await
    }

    #[tool(description = "Ranked grep over every file, definitions first: for strings and keys that aren't symbol names.")]
    async fn kc_search(&self, Parameters(p): Parameters<SearchParams>) -> Result<String, ToolError> {
        let scope = if !p.repos.is_empty() { self.scope_many(&p.repos)? } else { self.scope(&p.common, p.workspace)? };
        let workspace = p.workspace && p.repos.is_empty();
        let mut args: Vec<String> = vec!["search".into(), "--json".into()];
        match p.case.as_deref() {
            None | Some("sensitive") => {}
            Some("insensitive") => args.push("-i".into()),
            Some("smart") => args.push("-S".into()),
            Some(other) => return Err(ToolError::new(Code::InvalidArgument, format!("case must be sensitive, insensitive or smart, not {other}"))),
        }
        for (on, flag) in [(p.regex, "-e"), (p.word, "-w"), (p.files_only, "--files")] {
            if on {
                args.push(flag.into());
            }
        }
        let filtered = !p.lang.is_empty() || !p.paths.is_empty();
        for l in p.lang {
            args.extend(["-l".into(), l]);
        }
        for g in p.paths {
            args.extend(["-g".into(), g]);
        }
        args.extend(["-C".into(), SEARCH_CONTEXT.clamp(p.context).to_string()]);
        // A first page at the default limit shows every hit in code when they fit the answer (at most 60 hits).
        if p.limit.is_none() && p.offset.unwrap_or(0) == 0 && !p.files_only {
            args.extend(["--fit".into(), p.common.max_chars.unwrap_or(DEFAULT_CHARS).clamp(MIN_CHARS, MAX_CHARS).to_string()]);
        }
        args.extend(["-n".into(), SEARCH_LIMIT.clamp(p.limit).to_string()]);
        args.extend(["--offset".into(), p.offset.unwrap_or(0).to_string()]);
        if let Some(n) = p.max_lines {
            args.extend(["--max-lines".into(), n.max(1).to_string()]);
        }
        for r in &scope.repos {
            args.extend(["--repo".into(), r.to_string_lossy().into_owned()]);
        }
        let what = format!("no line matches {:?}", p.pattern);
        args.extend(["--".into(), p.pattern]);
        run_child(p.common.max_chars, move || {
            let env = noted(workspace, search_in_child(&args)?);
            if let Some(why) = searched_nothing(&env, filtered) {
                anyhow::bail!(Refusal::new(Code::NoFiles, why));
            }
            Ok(explain(env, &what, "limit (or offset=next_offset)"))
        })
        .await
    }

    #[tool(description = "Structural search: an ast-grep pattern (`$X`, `$$$`) over one language. kc_help kc_grep_ast.")]
    async fn kc_grep_ast(&self, Parameters(p): Parameters<GrepAstParams>) -> Result<String, ToolError> {
        let scope = if !p.repos.is_empty() { self.scope_many(&p.repos)? } else { self.scope(&p.common, p.workspace)? };
        let workspace = p.workspace && p.repos.is_empty();
        let mut args: Vec<String> = vec!["grep-ast".into(), "--json".into(), "-l".into(), p.lang];
        for (flag, v) in [
            ("--selector", p.selector),
            ("--inside", p.inside),
            ("--has", p.has),
            ("--not-inside", p.not_inside),
            ("--not-has", p.not_has),
            ("--text", p.text),
        ] {
            if let Some(v) = v {
                args.extend([flag.into(), v]);
            }
        }
        for g in p.paths {
            args.extend(["-g".into(), g]);
        }
        args.extend(["-n".into(), SEARCH_LIMIT.clamp(p.limit).to_string()]);
        args.extend(["--offset".into(), p.offset.unwrap_or(0).to_string()]);
        if let Some(n) = p.max_matches {
            args.extend(["--max-matches".into(), n.max(1).to_string()]);
        }
        for r in &scope.repos {
            args.extend(["--repo".into(), r.to_string_lossy().into_owned()]);
        }
        let what = format!("no {} code matches the pattern {:?}", args[3], p.pattern);
        args.extend(["--".into(), p.pattern]);
        run_child(p.common.max_chars, move || Ok(noted(workspace, explain(search_in_child(&args)?, &what, "limit (or offset=next_offset)")))).await
    }

    #[tool(description = "The next page of a cut list.")]
    async fn kc_more(&self, Parameters(p): Parameters<MoreParams>) -> Result<String, ToolError> {
        // In a child, as a search: a page can be a search's. A handle (see `compact`) stands for the full cursor.
        let args: Vec<String> = vec!["more".into(), "--json".into(), crate::compact::resolve(&p.cursor)?];
        run_child(p.max_chars, move || search_in_child(&args)).await
    }

    #[tool(description = "Code graph of the workspace's repos, instead of grep and reads. `op`: impact (who calls X or an HTTP endpoint, any repo, by hop; what breaks), find, refs, context, search, locate, history, more, help.")]
    async fn kc(&self, Parameters(p): Parameters<OneParams>) -> Result<String, ToolError> {
        let (tool, args) = one_call(p)?;
        fn a<T: serde::de::DeserializeOwned>(tool: &str, args: serde_json::Map<String, Value>) -> Result<Parameters<T>, String> {
            serde_json::from_value(Value::Object(args)).map(Parameters).map_err(|e| format!("op {}: {e}", &tool[3..]))
        }
        let t = tool.as_str();
        let out = match t {
            "kc_impact" => self.kc_impact(a(t, args)?).await,
            "kc_find" => self.kc_find(a(t, args)?).await,
            "kc_refs" => self.kc_refs(a(t, args)?).await,
            "kc_context" => self.kc_context(a(t, args)?).await,
            "kc_search" => self.kc_search(a(t, args)?).await,
            "kc_locate" => self.kc_locate(a(t, args)?).await,
            "kc_more" => self.kc_more(a(t, args)?).await,
            // Data, as it is: its names are the tools' own.
            "kc_capabilities" => return self.kc_capabilities(a(t, args)?).await,
            "kc_help" => return self.kc_help(a(t, args)?).await.map(|h| one_text(&h, true)).map_err(|e| e.reworded(|m| one_text(m, true))),
            "kc_trace" => self.kc_trace(a(t, args)?).await,
            "kc_path" => self.kc_path(a(t, args)?).await,
            "kc_flow" => self.kc_flow(a(t, args)?).await,
            "kc_symbol_at" => self.kc_symbol_at(a(t, args)?).await,
            "kc_unused" => self.kc_unused(a(t, args)?).await,
            "kc_rename_preview" => self.kc_rename_preview(a(t, args)?).await,
            "kc_coupled" => self.kc_coupled(a(t, args)?).await,
            "kc_tests" => self.kc_tests(a(t, args)?).await,
            "kc_hotspots" => self.kc_hotspots(a(t, args)?).await,
            "kc_ownership" => self.kc_ownership(a(t, args)?).await,
            "kc_reviewers" => self.kc_reviewers(a(t, args)?).await,
            "kc_history" => self.kc_history(a(t, args)?).await,
            "kc_overview" => self.kc_overview(a(t, args)?).await,
            "kc_areas" => self.kc_areas(a(t, args)?).await,
            "kc_notes" => self.kc_notes(a(t, args)?).await,
            "kc_note_add" => self.kc_note_add(a(t, args)?).await,
            "kc_grep_ast" => self.kc_grep_ast(a(t, args)?).await,
            "kc_status" => self.kc_status(a(t, args)?).await,
            "kc_task" => self.kc_task(a(t, args)?).await,
            "kc_test_targets" => self.kc_test_targets(a(t, args)?).await,
            other => Err(ToolError::new(Code::InvalidArgument, format!("no op {other}"))),
        };
        out.map(|o| crate::summary::restate(one_text(&o, false))).map_err(|e| e.reworded(|m| one_text(m, false)))
    }

    #[tool(description = "Every op as data: its parameters with ranges and choices, whether it writes, its answer's shape.")]
    async fn kc_capabilities(&self, Parameters(p): Parameters<CapabilitiesParams>) -> Result<String, ToolError> {
        let doc = crate::capabilities::document(&self.surface(), p.of.as_deref())?;
        Ok(serde_json::to_string(&doc).unwrap_or_default())
    }

    #[tool(description = "Declared test targets (SwiftPM, .NET test projects, package.json test scripts): their tests and how to run them.")]
    async fn kc_test_targets(&self, Parameters(p): Parameters<TestTargetsParams>) -> Result<String, ToolError> {
        let scope = self.scope(&p.common, false)?;
        run(p.common.max_chars, move || {
            let what = "no declared test target: no SwiftPM test target, .NET test project or package.json test script".to_string();
            Ok(explain(ops::test_targets(p.name.as_deref(), &scope)?.env, &what, ""))
        })
        .await
    }

    #[tool(description = "Index status: coverage, parse errors, freshness, config, SCIP.")]
    async fn kc_status(&self, Parameters(p): Parameters<RepoOnlyParams>) -> Result<String, ToolError> {
        // Every repo of the workspace, those not indexed yet included.
        let scope = if p.workspace {
            Scope { repos: kc_index::indexed_roots().map_err(|e| format!("{e:#}"))?, no_sync: false }
        } else {
            self.scope(&p.common, false)?
        };
        run(p.common.max_chars, move || {
            let env = ops::status(&scope)?.env;
            // Several repos answer as a list: wrap it so a cut says how many were left out.
            Ok(match env {
                Value::Array(list) => {
                    let n = list.len();
                    let mut env = serde_json::json!({ "items": list, "total": n, "truncated": false });
                    // The workspace's repos not indexed yet, counted once.
                    if let Some(note) = ops::unindexed_note().filter(|_| p.workspace) {
                        ops::push_incomplete(&mut env, note);
                    }
                    env
                }
                one => one,
            })
        })
        .await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for KnowcodeServer {
    /// Runs the tool as an operation the client can cancel: `notifications/cancelled` cancels the request's token,
    /// which here cancels the operation (its child processes are killed, its work stops at the next check) rather
    /// than leaving the blocking work to run on unseen.
    async fn call_tool(
        &self,
        mut request: rmcp::model::CallToolRequestParams,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::CallToolResponse, rmcp::ErrorData> {
        if let Some(r) = &self.restriction
            && let Err(e) = r.check(&request.name, request.arguments.as_ref())
        {
            return rmcp::handler::server::tool::IntoCallToolResult::into_call_tool_result(ToolError::from(anyhow::Error::new(e)));
        }
        if let Err(e) = self.guard_privacy(request.arguments.as_ref()) {
            return rmcp::handler::server::tool::IntoCallToolResult::into_call_tool_result(ToolError::from(e));
        }
        // `provenance: true` on any call asks for its provenance record (see `crate::record`), as a second content
        // block; the tool itself never sees the argument.
        let provenance = request.arguments.as_mut().and_then(|a| a.remove("provenance")).is_some_and(|v| v == Value::Bool(true));
        let recording: Recording = provenance.then(Default::default);
        let call_desc = serde_json::json!({ "tool": request.name, "arguments": request.arguments.clone().unwrap_or_default() });
        let ct = context.ct.clone();
        let op = kc_store::limits::Operation::new();
        let tcc = rmcp::handler::server::tool::ToolCallContext::new(self, request, context);
        let call = OPERATION.scope(op.clone(), RECORDING.scope(recording.clone(), self.tool_router.call(tcc)));
        tokio::pin!(call);
        let response = tokio::select! {
            r = &mut call => r,
            _ = ct.cancelled() => {
                op.cancel();
                // The work stops soon after; its answer (an error) goes nowhere the client waits for.
                call.await
            }
        };
        // An error raised before the tool ran (its arguments didn't fit its parameters) is coded by what it says.
        let response = match response {
            Ok(rmcp::model::CallToolResponse::Complete(result)) if result.is_error == Some(true) && result.structured_content.is_none() => {
                let text: String = result.content.iter().filter_map(|c| c.as_text().map(|t| t.text.clone())).collect();
                Ok(coded(result, &ToolError::from(text)).into())
            }
            r => r,
        };
        match (response, recording) {
            (Ok(rmcp::model::CallToolResponse::Complete(mut result)), Some(envs)) => {
                let answer: String = result.content.iter().filter_map(|c| c.as_text().map(|t| t.text.as_str())).collect();
                let envs = envs.lock().unwrap_or_else(|e| e.into_inner()).clone();
                let record = tokio::task::spawn_blocking(move || crate::record::record(&call_desc, answer.as_bytes(), &envs))
                    .await
                    .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?;
                result.content.push(rmcp::model::ContentBlock::text(serde_json::json!({ "record": record }).to_string()));
                Ok(result.into())
            }
            (response, _) => response,
        }
    }

    fn get_info(&self) -> ServerConfig {
        let mut capabilities = ServerCapabilities::builder().enable_tools().enable_resources().build();
        // A restricted server states its scope in its first reply.
        if let Some(r) = &self.restriction
            && let Value::Object(scope) = r.scope()
        {
            capabilities.experimental = Some([("knowcode/scope".to_string(), scope)].into_iter().collect());
        }
        ServerConfig::new(capabilities)
            .with_server_info(Implementation::new("knowcode", concat!(env!("CARGO_PKG_VERSION"), "+", env!("KNOWCODE_BUILD"))))
            .with_instructions(if self.one { ONE_INSTRUCTIONS } else { INSTRUCTIONS })
    }

    /// The help topics, as resources (`knowcode://help/<topic>`), for clients that read resources.
    async fn list_resources(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::ListResourcesResult, rmcp::ErrorData> {
        let mut resources: Vec<rmcp::model::Resource> = crate::help::TOPICS
            .iter()
            .map(|t| {
                rmcp::model::Resource::new(format!("{HELP_URI}{}", t.name), t.name).with_description(t.summary).with_mime_type("text/markdown")
            })
            .collect();
        resources.push(
            rmcp::model::Resource::new(CAPABILITIES_URI, "capabilities")
                .with_description("every op as data (kc_help capabilities)")
                .with_mime_type("application/json"),
        );
        Ok(rmcp::model::ListResourcesResult::with_all_items(resources))
    }

    async fn read_resource(
        &self,
        request: rmcp::model::ReadResourceRequestParams,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::ReadResourceResponse, rmcp::ErrorData> {
        if request.uri == CAPABILITIES_URI {
            let doc = crate::capabilities::document(&self.surface(), None).map_err(|e| rmcp::ErrorData::internal_error(e, None))?;
            let text = serde_json::to_string(&doc).unwrap_or_default();
            return Ok(rmcp::model::ReadResourceResult::new(vec![rmcp::model::ResourceContents::text(text, request.uri)]).into());
        }
        let topic = request.uri.strip_prefix(HELP_URI).ok_or_else(|| rmcp::ErrorData::resource_not_found(format!("no resource {}", request.uri), None))?;
        let text = self.help(Some(topic)).map_err(|e| rmcp::ErrorData::resource_not_found(e, None))?;
        Ok(rmcp::model::ReadResourceResult::new(vec![rmcp::model::ResourceContents::text(text, request.uri)]).into())
    }
}

/// Where the help topics are served as resources.
const HELP_URI: &str = "knowcode://help/";

/// Where the capabilities are served as a resource.
const CAPABILITIES_URI: &str = "knowcode://capabilities";

/// The server's instructions: what KnowCode is for and which core tool answers which question (a short map; each
/// tool's description says more), callers and HTTP endpoints first, the questions agents most often took to grep; the
/// rest is in `kc_help`. A client may leave them out (Claude Code's `--bare` does), so the tools' descriptions carry the
/// same map, and every token here is paid on every turn by the clients that keep them.
const INSTRUCTIONS: &str = "KnowCode: a code graph of your workspace's repos, read from the working tree. Use it instead \
of grep and reading files. Who calls a method or an HTTP endpoint, from any repo, by hop, and what breaks if X changes: \
kc_impact, X as file:line or `Type.member`, `depth` 2 for callers' callers. Where X is defined kc_find; every use \
kc_refs; what X is kc_context; files for a concept or bug kc_locate; text kc_search; the next page kc_more; every tool \
and parameter kc_help. Tools take `repo`. Each edge has a confidence; an incomplete answer says why. `withheld`: a \
privacy file blocks it; don't read it another way.";

/// The `one` profile's instructions, the default: the same map, as ops of the one tool.
const ONE_INSTRUCTIONS: &str = "KnowCode: a code graph of your workspace's repos. Use its `kc` tool instead of grep \
and reading files. Who calls a method or an HTTP endpoint, from any repo, by hop, and what breaks if X changes: \
kc {op: impact, q: X}, X as file:line or `Type.member`, `depth` 2 for callers' callers. Where X is defined op find; \
every use refs; what X is context; files for a concept or bug locate; text search; X's commits and authors history; \
files changing with a file coupled; next page more; other ops and parameters help. `repo` picks a repo. Each edge \
has a confidence; an incomplete answer says why. `withheld`: a privacy file blocks it; don't read it another way.";

/// Serves MCP over stdin/stdout until the client disconnects.
pub fn serve(default: Scope, tools: &str) -> anyhow::Result<()> {
    let server = KnowcodeServer::new(default, tools).map_err(anyhow::Error::msg)?;
    run_server(server)
}

/// [`serve`] in restricted mode: about its bound repos only, its allowed ops only (see [`crate::restricted`]).
pub fn serve_restricted(r: crate::restricted::Restriction, tools: &str) -> anyhow::Result<()> {
    let default = Scope { repos: r.repos.clone(), no_sync: false };
    let mut server = KnowcodeServer::new(default, tools).map_err(anyhow::Error::msg)?;
    // Under a profile of separate tools, the tools it may not run aren't listed either.
    let refused: Vec<String> = server.schemas.keys().filter(|t| *t != "kc" && !r.allows(t) && !server.hidden.contains(t)).cloned().collect();
    for t in &refused {
        server.tool_router.disable_route(t.clone());
    }
    let mut hidden = (*server.hidden).clone();
    hidden.extend(refused);
    server.hidden = std::sync::Arc::new(hidden);
    server.restriction = Some(std::sync::Arc::new(r));
    run_server(server)
}

fn run_server(server: KnowcodeServer) -> anyhow::Result<()> {
    kc_index::light::discover_roots()?;
    crate::refresh::start();
    // No process-wide SQLite memory limit: SQLite is built without the counting it needs (see .cargo/config.toml).
    // Each connection's page cache is bounded by its `cache_size`, and `release_memory` hands freed pages back.
    // Stdin and stdout each hold a blocking thread; the others run tool work, one per lane (see `WORK`).
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(1).max_blocking_threads(5).enable_all().build()?;
    rt.block_on(async move {
        let service = server.serve(rmcp::transport::stdio()).await?;
        service.waiting().await?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn server(tools: &str) -> KnowcodeServer {
        KnowcodeServer::new(Scope { repos: Vec::new(), no_sync: false }, tools).unwrap()
    }

    /// The size cap drops the rest of sites' statements, from the last row up, before it cuts a row: a reference's
    /// cite goes back to its line, keeping what was said of that line; a tree's `-` lines go first.
    #[test]
    fn the_size_cap_drops_statement_lines_before_rows() {
        let row = |line: u64| {
            json!({ "file": "src/A.cs", "line": line, "name": "Log", "confidence": "exact", "evidence": "receiver_type",
                    "cite": { "at": format!("src/A.cs:{}-{}", line - 1, line + 1), "check": "verified",
                              "text": format!("Audit.Log(\n    \"{}\",\n    2);", "x".repeat(300)),
                              "cut": format!("the statement starts at line 1; line {line} cut at 120 of 300 characters") } })
        };
        let env = json!({ "complete": true, "items": (2..12).map(row).collect::<Vec<_>>(), "total": 10 });
        let full = env.to_string().len();
        let capped = cap_full(env, full - 400);
        let items = capped["items"].as_array().unwrap();
        assert_eq!(items.len(), 10, "{capped}");
        // The last rows lost their statements; the first still have theirs.
        let last = &items[9]["cite"];
        assert_eq!((last["at"].as_str(), last["text"].as_str()), (Some("src/A.cs:11"), Some(format!("\"{}\",", "x".repeat(300)).as_str())), "{last}");
        assert_eq!(last["cut"], "line 11 cut at 120 of 300 characters");
        assert_eq!(items[0]["cite"]["at"], "src/A.cs:1-3");
        assert!(capped.get("omitted").is_none(), "{capped}");
        // A tree: the `-` lines and the statement's cut line, from the bottom up.
        let tree = "a\n   1 - Audit.Log(\n   2 |     \"x\",\n   3 -     2);\n     … the statement starts at line 0\nb\n   7 - Audit.Log(\n   8 |     \"y\",\n";
        assert_eq!(unstate_text(tree, 10), "a\n   1 - Audit.Log(\n   2 |     \"x\",\n   3 -     2);\n     … the statement starts at line 0\nb\n   8 |     \"y\",\n");
        assert_eq!(unstate_text(tree, 1000), "a\n   2 |     \"x\",\nb\n   8 |     \"y\",\n");
    }

    /// Bytes the default (`core`) tool list and the instructions may take together: 3,707 at b0a3564 with kc_trace in
    /// core and the shorter instructions; 3,470 at 0.2.0; 3,073 with kc_impact naming HTTP endpoints and hops and the
    /// parameter lines whose names say enough dropped.
    const CORE_BUDGET: usize = 3_150;

    /// What tool discovery costs an agent's context, held to a budget so it can't creep back: the advertised schema
    /// (every tool, as `tools/list` sends it), each description, each parameter's line, and the instructions.
    #[test]
    fn bounds_are_said_as_answers_apply_them() {
        let s = server("all");
        let advertised: std::collections::HashMap<String, rmcp::model::Tool> =
            s.tool_router.list_all().into_iter().map(|t| (t.name.to_string(), t)).collect();
        for (tool, param, r) in RANGES.iter().filter(|(t, _, _)| *t != "*") {
            // The schema's line, the help topic's and the advertised one.
            let full = &s.schemas[*tool]["properties"][*param];
            assert_eq!(full["description"].as_str(), Some(r.line().as_str()), "{tool}.{param}");
            let help = s.help(Some(tool)).unwrap();
            assert!(help.contains(&format!("`{param}` (integer): {}\n", r.line())), "{tool}.{param}: {help}");
            if let Some(shown) = advertised[*tool].input_schema["properties"].get(*param).and_then(|p| p["description"].as_str()) {
                assert_eq!(shown, format!("{}.", r.span()), "{tool}.{param}");
            }
            // The clamp.
            assert_eq!((r.clamp(Some(r.max + 1)), r.clamp(Some(r.min)), r.clamp(None)), (r.max, r.min, r.default), "{tool}.{param}");
        }
        assert!(s.help(Some("kc_find")).unwrap().contains(&format!("`max_chars` (integer): {}\n", ANSWER_CHARS.line())));
        // The ranges are the runtime's.
        assert_eq!(TRACE_DEPTH.max, kc_store::trace::MAX_TRACE_DEPTH as usize);
        assert_eq!(IMPACT_DEPTH.max, kc_store::trace::MAX_IMPACT_DEPTH as usize);
        assert_eq!(FLOW_DEPTH.max, crate::flow::MAX_DEPTH as usize);
        assert_eq!((PATH_HOPS.max, PATH_COUNT.max), (crate::walk::MAX_PATH_HOPS as usize, crate::walk::MAX_PATHS));
        // Help prose names bounds by placeholder only: every one fills, and no topic says a stale one.
        for t in crate::help::TOPICS {
            let text = crate::help::fill(t.text);
            for p in ["{min:", "{max:", "{default:", "{span:"] {
                assert!(!text.contains(p), "{}: unfilled {p}", t.name);
            }
        }
        let impact = s.help(Some("impact")).unwrap();
        assert!(impact.contains(&format!("up to {} hops", IMPACT_DEPTH.max)) && impact.contains(&format!("pass depth 2-{} for more", IMPACT_DEPTH.max)), "{impact}");
        assert!(!impact.contains("to 5"), "{impact}");
    }

    /// A refusal raised with its code keeps it, through `anyhow` and its context; one raised as English is coded by
    /// what it says, each family the ops raise; and a refused call carries its code beside the English.
    #[test]
    fn every_refusal_is_coded() {
        let policy = kc_store::answer::PrivacyPolicy { repo: "app".into(), file: ".knowcode-privacy.yml".into(), block_at: 1, blocks: "paths".into() };
        let e = anyhow::Error::new(Refusal::withheld("withheld: AI privacy level 1 in app", policy.clone())).context("asking");
        let t = ToolError::from(e);
        assert_eq!((t.code, t.privacy.as_ref()), (Code::Withheld, Some(&policy)));
        assert_eq!(t.json()["privacy"]["blocks"], "paths");
        for (message, code) in [
            ("failed to deserialize parameters: invalid type: string \"x\", expected u8", Code::InvalidArgument),
            ("op trace: missing field `symbol`", Code::InvalidArgument),
            ("direction must be in, out or both, not sideways", Code::InvalidArgument),
            ("no op \"nope\": one of impact, find", Code::InvalidArgument),
            ("give a symbol or symbols", Code::InvalidArgument),
            ("21 inputs: at most 20 per call; split them across calls", Code::InvalidArgument),
            ("unknown evidence \"sometimes\": a preset (default, confirmed, all)", Code::InvalidArgument),
            ("pattern `class {` doesn't parse as csharp: …", Code::InvalidArgument),
            ("cursor kc~ff is no longer held by this server (it keeps the last 4096, until it restarts)", Code::StaleCursor),
            ("stale cursor: the list changed since this cursor was made", Code::StaleCursor),
            ("paths matched 0 files (nowhere/**): in shop; nothing was searched", Code::NoFiles),
            ("no repo at nowhere and no indexed repo named nowhere", Code::NotIndexed),
            ("no definition named Nope in shop", Code::NotFound),
            ("Save names 3 definitions; pass Type.member or file:line:", Code::Ambiguous),
            ("git log failed in /x: fatal", Code::Failed),
            ("something nobody expected", Code::Internal),
        ] {
            assert_eq!(code_of(message), code, "{message}");
        }
        let r = rmcp::handler::server::tool::IntoCallToolResult::into_call_tool_result(ToolError::new(Code::NotFound, "no definition named X"));
        let Ok(rmcp::model::CallToolResponse::Complete(r)) = r else { panic!("{r:?}") };
        assert_eq!(r.is_error, Some(true));
        assert_eq!(r.structured_content, Some(json!({ "error": { "code": "not_found", "message": "no definition named X" } })));
    }

    #[test]
    fn invariant_the_advertised_schema_keeps_within_its_budget() {
        let tools = server("all").tool_router.list_all();
        let bytes = serde_json::to_string(&tools).unwrap().len();
        // 9,766 bytes and 875 of instructions before the second diet (0.2.0's study); 6,550 and 636 after it.
        assert!(bytes <= 6_800, "advertised schema {bytes} bytes, budget 6800: advertise fewer parameters (ADVERTISED) or move detail into a help topic");
        assert!(bytes + INSTRUCTIONS.len() <= 7_450, "schema and instructions {} bytes: at least 30% under 0.2.0's 10,641", bytes + INSTRUCTIONS.len());
        for t in &tools {
            let d = t.description.as_deref().unwrap_or("");
            assert!(!d.is_empty() && d.len() <= 160, "{}: description {} bytes, budget 160", t.name, d.len());
            assert!(!serde_json::to_string(&t.input_schema).unwrap().contains("\"$schema\""), "{}", t.name);
            for (name, p) in t.input_schema.get("properties").and_then(Value::as_object).into_iter().flatten() {
                let pd = p["description"].as_str().unwrap_or("");
                assert!(pd.len() <= 120, "{}.{name}: {} bytes, budget 120", t.name, pd.len());
                assert!(!p["type"].is_array(), "{}.{name}: optional parameters are typed plainly: {p}", t.name);
            }
        }
        // The callers-and-HTTP map made the instructions shorter, not longer.
        assert!(INSTRUCTIONS.len() <= 640, "instructions {} bytes", INSTRUCTIONS.len());
        // The instructions map questions to the core tools, every one of them and no other: the default server
        // advertises only those.
        assert!(INSTRUCTIONS.contains("Use it instead of grep and reading files"));
        for t in &tools {
            let core = CORE.contains(&t.name.as_ref());
            assert_eq!(INSTRUCTIONS.contains(t.name.as_ref()), core, "{}: the instructions name the core tools only, all of them", t.name);
        }
        // The core profile, the default, is a fraction of it: what a client loading every schema on every turn carries.
        let core = serde_json::to_string(&server("core").tool_router.list_all()).unwrap().len();
        assert!(core * 2 < bytes, "core {core} of all {bytes}");
        assert!(core + INSTRUCTIONS.len() <= CORE_BUDGET, "core schema and instructions {} bytes, budget {CORE_BUDGET}", core + INSTRUCTIONS.len());
        // The default, `one`: its tool and instructions together, 1,290 bytes and 501 tokens (count_tokens) when
        // set; at most 510 tokens is the bar.
        let one = serde_json::to_string(&server("one").tool_router.list_all()).unwrap().len();
        assert!(one + ONE_INSTRUCTIONS.len() <= 1_300, "one schema and instructions {} bytes, budget 1300", one + ONE_INSTRUCTIONS.len());
    }

    /// "What calls X", "who calls this HTTP endpoint" and "what breaks if X changes" go to kc_impact with a `file:line`
    /// selector, said where an agent asking them looks: the instructions, the tool it would otherwise use, kc_refs, and
    /// kc_impact's own description, which a client dropping the instructions (Claude Code's `--bare`) still shows: it
    /// names methods, HTTP endpoints and hops. The selector those name is one kc_impact and kc_trace take.
    #[test]
    fn caller_and_impact_questions_are_steered_to_kc_impact_by_file_line() {
        let tools = server("all").tool_router.list_all();
        let described = |name: &str| tools.iter().find(|t| t.name == name).and_then(|t| t.description.as_deref()).unwrap_or("").to_string();
        for (place, text) in [("instructions", INSTRUCTIONS.to_string()), ("kc_refs", described("kc_refs"))] {
            assert!(text.contains("kc_impact") && text.contains("file:line") && text.contains("calls"), "{place}: {text}");
            assert!(text.contains("HTTP") && text.contains("hop"), "{place}: {text}");
        }
        let impact = described("kc_impact");
        for word in ["calls a method", "HTTP endpoint", "any repo", "hop", "`depth` 2"] {
            assert!(impact.contains(word), "kc_impact: {word:?} in {impact}");
        }
        for tool in ["kc_impact", "kc_trace"] {
            let t = tools.iter().find(|t| t.name == tool).unwrap();
            assert!(t.input_schema["properties"]["symbol"]["description"].as_str().unwrap().contains("file:line"), "{tool}");
        }
    }

    /// Nothing an agent needs is lost by the short schemas: every tool has a help topic, and every parameter of every
    /// tool is described in its schema or in that topic; every topic a description or topic points to exists.
    #[test]
    fn invariant_every_parameter_is_documented_where_an_agent_can_reach_it() {
        let s = server("all,kc_capabilities");
        let one = server("one");
        let mut tools = s.tool_router.list_all();
        tools.extend(one.tool_router.list_all());
        for t in &tools {
            let topic = crate::help::topic(&t.name).unwrap_or_else(|| panic!("{} has no help topic", t.name));
            let text = s.help(Some(&t.name)).unwrap();
            // Every parameter the tool takes, advertised or not.
            let full = s.schemas.get(t.name.as_ref()).unwrap();
            for (name, p) in full.get("properties").and_then(Value::as_object).into_iter().flatten() {
                let in_schema = p["description"].as_str().is_some_and(|d| !d.is_empty());
                let line = text.lines().find(|l| l.starts_with(&format!("- `{name}` ")));
                let in_help = line.is_some_and(|l| l.split_once("): ").is_some_and(|(_, d)| !d.trim().is_empty()));
                assert!(in_schema || in_help, "{}.{name} is documented nowhere", t.name);
                assert!(in_help, "{}.{name} is missing from its help topic {}", t.name, topic.name);
            }
        }
        // Topics named in descriptions, the instructions and topics themselves exist.
        let mut texts: Vec<String> = tools.iter().filter_map(|t| t.description.as_deref().map(str::to_string)).collect();
        texts.push(INSTRUCTIONS.to_string());
        texts.push(ONE_INSTRUCTIONS.to_string());
        texts.extend(crate::help::TOPICS.iter().map(|t| t.text.to_string()));
        for text in &texts {
            for (i, _) in text.match_indices("kc_help ") {
                let name: String = text[i + 8..].chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.').collect();
                let name = name.trim_end_matches('.');
                if !name.is_empty() && !name.starts_with('{') {
                    assert!(crate::help::topic(name).is_some(), "kc_help {name} named but no such topic");
                }
            }
        }
        // The concepts the brief names, and every tool's sections, are there.
        for t in ["contract", "evidence", "cursors", "privacy", "profiles", "kc_context.http", "kc_impact.crossings"] {
            assert!(crate::help::topic(t).is_some(), "{t}");
        }
        assert!(s.help(None).unwrap().contains("`kc_context.http`"));
        assert!(s.help(Some("nope")).is_err());
    }

    /// The tool's parameters struct read from `args`, as the server reads a call's arguments: its `Debug` form, or why
    /// the arguments don't fit it.
    fn parse(tool: &str, args: Value) -> Result<String, String> {
        fn as_<T: serde::de::DeserializeOwned + std::fmt::Debug>(args: Value) -> Result<String, String> {
            serde_json::from_value::<T>(args).map(|p| format!("{p:?}")).map_err(|e| e.to_string())
        }
        match tool {
            "kc_help" => as_::<HelpParams>(args),
            "kc_symbol_at" => as_::<SymbolAtParams>(args),
            "kc_find" => as_::<FindParams>(args),
            "kc_locate" => as_::<LocateParams>(args),
            "kc_refs" => as_::<RefsParams>(args),
            "kc_context" => as_::<ContextParams>(args),
            "kc_trace" => as_::<TraceParams>(args),
            "kc_path" => as_::<PathParams>(args),
            "kc_flow" => as_::<FlowParams>(args),
            "kc_impact" => as_::<ImpactParams>(args),
            "kc_unused" => as_::<UnusedParams>(args),
            "kc_rename_preview" => as_::<RenamePreviewParams>(args),
            "kc_coupled" => as_::<CoupledParams>(args),
            "kc_tests" => as_::<TestsParams>(args),
            "kc_hotspots" => as_::<HotspotsParams>(args),
            "kc_ownership" => as_::<OwnershipParams>(args),
            "kc_reviewers" => as_::<ReviewersParams>(args),
            "kc_history" => as_::<HistoryParams>(args),
            "kc_overview" => as_::<OverviewParams>(args),
            "kc_areas" => as_::<AreasParams>(args),
            "kc_notes" => as_::<NotesParams>(args),
            "kc_note_add" => as_::<NoteAddParams>(args),
            "kc_search" => as_::<SearchParams>(args),
            "kc_grep_ast" => as_::<GrepAstParams>(args),
            "kc_more" => as_::<MoreParams>(args),
            "kc_status" => as_::<RepoOnlyParams>(args),
            "kc_capabilities" => as_::<CapabilitiesParams>(args),
            "kc_task" => as_::<TaskParams>(args),
            "kc_test_targets" => as_::<TestTargetsParams>(args),
            "kc" => as_::<OneParams>(args),
            other => Err(format!("no parameters type for {other}")),
        }
    }

    /// The schema advertises each tool's common parameters only; every other parameter is still taken. For every tool:
    /// what it advertises is a parameter it has, every required one is advertised, and every parameter it doesn't
    /// advertise is documented in its help topic and read from a call's arguments (its value reaches the parameters
    /// the tool runs with, not dropped as unknown).
    #[test]
    fn every_unadvertised_parameter_is_documented_and_accepted() {
        let s = server("all,kc_capabilities,kc_test_targets");
        let mut tools = s.tool_router.list_all();
        tools.extend(server("one").tool_router.list_all());
        assert_eq!(ADVERTISED.len(), tools.len(), "every tool says what it advertises");
        let mut unadvertised = 0;
        for t in &tools {
            let full = s.schemas.get(t.name.as_ref()).unwrap();
            let all = full.get("properties").and_then(Value::as_object).cloned().unwrap_or_default();
            let shown = t.input_schema.get("properties").and_then(Value::as_object).cloned().unwrap_or_default();
            let (_, keep) = ADVERTISED.iter().find(|(n, _)| *n == t.name).unwrap_or_else(|| panic!("{} not in ADVERTISED", t.name));
            assert_eq!(shown.len(), keep.len(), "{}: an advertised name it doesn't take", t.name);
            for r in full.get("required").and_then(Value::as_array).into_iter().flatten() {
                assert!(shown.contains_key(r.as_str().unwrap()), "{}: required {r} not advertised", t.name);
            }
            let help = s.help(Some(&t.name)).unwrap();
            let mut required = serde_json::Map::new();
            for r in full.get("required").and_then(Value::as_array).into_iter().flatten() {
                let r = r.as_str().unwrap();
                required.insert(r.into(), sample(&all[r], r));
            }
            for (name, p) in all.iter().filter(|(n, _)| !shown.contains_key(*n)) {
                unadvertised += 1;
                assert!(help.lines().any(|l| l.starts_with(&format!("- `{name}` ("))), "{}.{name} isn't in its help topic", t.name);
                let mut args = required.clone();
                let value = sample(p, name);
                args.insert(name.clone(), value.clone());
                let read = parse(&t.name, Value::Object(args)).unwrap_or_else(|e| panic!("{}.{name}: {e}", t.name));
                let want = match &value {
                    Value::String(v) => v.clone(),
                    Value::Array(a) => a[0].as_str().unwrap().to_string(),
                    other => other.to_string(),
                };
                assert!(read.contains(&want), "{}.{name}: {value} was dropped: {read}", t.name);
            }
        }
        // `repo`, `max_chars` and the rare ones: the bulk of what a tool takes.
        assert!(unadvertised > 60, "{unadvertised}");
    }

    /// Under `one`, every tool is reachable as an op of `kc`, with every parameter it takes: `q` and `qs` fill the
    /// op's main parameters, and any other is passed by its own name and reaches the tool.
    #[test]
    fn every_op_reaches_its_tool_with_every_parameter() {
        let s = server("all,kc_capabilities,kc_test_targets");
        let tools = s.tool_router.list_all();
        assert_eq!(ONE_OPS.len(), tools.len(), "an op per tool");
        let kc = |v: Value| one_call(serde_json::from_value::<OneParams>(v).unwrap());
        for (op, main, many) in ONE_OPS {
            let tool = format!("kc_{op}");
            let full = s.schemas.get(&tool).unwrap_or_else(|| panic!("op {op}: no tool {tool}"));
            let all = full.get("properties").and_then(Value::as_object).cloned().unwrap_or_default();
            let mut required = serde_json::Map::new();
            for r in full.get("required").and_then(Value::as_array).into_iter().flatten() {
                let r = r.as_str().unwrap();
                required.insert(r.into(), sample(&all[r], r));
            }
            for (name, p) in &all {
                let mut args = required.clone();
                let value = sample(p, name);
                args.insert(name.clone(), value.clone());
                args.insert("op".into(), json!(op));
                let (t, args) = kc(Value::Object(args)).unwrap_or_else(|e| panic!("{op}.{name}: {e}"));
                assert_eq!(t, tool);
                let read = parse(&t, Value::Object(args)).unwrap_or_else(|e| panic!("{op}.{name}: {e}"));
                let want = match &value {
                    Value::String(v) => v.clone(),
                    Value::Array(a) => a[0].as_str().unwrap().to_string(),
                    other => other.to_string(),
                };
                assert!(read.contains(&want), "{op}.{name}: {value} was dropped: {read}");
            }
            if !main.is_empty() {
                assert!(all.contains_key(*main), "{op}: q fills {main}, which {tool} doesn't take");
                let mut args = required.clone();
                args.remove(*main);
                args.insert("op".into(), json!(op));
                args.insert("q".into(), json!("zq-main"));
                let (t, args) = kc(Value::Object(args)).unwrap();
                assert!(parse(&t, Value::Object(args)).unwrap().contains("zq-main"), "{op}: q");
            } else {
                assert!(kc(json!({ "op": op, "q": "x" })).is_err(), "{op} takes no q");
                // An empty or absent `q` is none.
                for args in [json!({ "op": op }), json!({ "op": op, "q": "" }), json!({ "op": op, "q": " " })] {
                    let (t, args) = kc(args.clone()).unwrap_or_else(|e| panic!("{args}: {e}"));
                    assert!(parse(&t, Value::Object(args)).is_ok(), "{op}");
                }
            }
            if !many.is_empty() {
                assert!(all.contains_key(*many), "{op}: qs fills {many}, which {tool} doesn't take");
                let mut args = required.clone();
                args.remove(*many);
                args.insert("op".into(), json!(op));
                args.insert("qs".into(), json!(["zq-many"]));
                let (t, args) = kc(Value::Object(args)).unwrap();
                assert!(parse(&t, Value::Object(args)).unwrap().contains("zq-many"), "{op}: qs");
            }
        }
        // The `kc` topic names the ops taking no `q`.
        let no_q: Vec<&str> = ONE_OPS.iter().filter(|(_, main, _)| main.is_empty()).map(|(o, _, _)| *o).collect();
        let kc_topic = crate::help::fill(crate::help::topic("kc").unwrap().text);
        assert!(kc_topic.contains(&format!("- {}: no `q`", no_q.join(", "))), "{no_q:?}");
        // With or without the prefix; an unknown op is refused, naming the ops.
        assert_eq!(kc(json!({ "op": "kc_find", "q": "A" })).unwrap().0, "kc_find");
        assert!(kc(json!({ "op": "nope" })).unwrap_err().contains("impact, find, refs"));
        // A help topic named by its op is its tool's.
        for (q, topic) in [("impact", "kc_impact"), ("impact.crossings", "kc_impact.crossings"), ("contract", "contract")] {
            let (_, args) = kc(json!({ "op": "help", "q": q })).unwrap();
            assert_eq!(crate::help::topic(args["topic"].as_str().unwrap()).unwrap().name, topic);
        }
    }

    /// Calls a text names, as `kc` ops: in answers (a word after the name is its argument unless it is prose) and in
    /// prose (help topics, skills).
    #[test]
    fn calls_are_named_as_ops_of_kc() {
        let cases = [
            ("continue: kc_more {cursor: \"kc~1f\"}", "continue: kc {op: more, cursor: \"kc~1f\"}"),
            ("kc_more {cursor: <`items[0].callers_cursor`>}", "kc {op: more, cursor: <`items[0].callers_cursor`>}"),
            ("continue: kc_impact {\"depth\":2,\"symbol\":\"a.cs#B\"}", "continue: kc {op: impact, \"depth\":2,\"symbol\":\"a.cs#B\"}"),
            ("callers: 1 of 4 shown; kc_refs Save lists them all", "callers: 1 of 4 shown; kc {op: refs, q: Save} lists them all"),
            ("a use: 2 references (kc_refs isValid)", "a use: 2 references (kc {op: refs, q: isValid})"),
            ("kc_search finds them as text", "kc {op: search} finds them as text"),
            ("ask again later (kc_status lists what is left)", "ask again later (kc {op: status} lists what is left)"),
            ("use kc_context(symbol: \"A (b)\"); every reference: kc_refs(name: \"A\").", "use kc {op: context, symbol: \"A (b)\"}; every reference: kc {op: refs, name: \"A\"}."),
            ("kc_overview()", "kc {op: overview}"),
            ("kc_index and xkc_find and kc_nope stay", "kc_index and xkc_find and kc_nope stay"),
        ];
        for (text, want) in cases {
            assert_eq!(one_text(text, false), want, "{text}");
        }
        let prose = [
            ("get the rest with `kc_refs` or the cursor", "get the rest with `kc {op: refs}` or the cursor"),
            ("`kc_overview` with no arguments", "`kc {op: overview}` with no arguments"),
            ("see kc_help kc_context.http, and kc_impact.crossings", "see kc {op: help, q: context.http}, and impact.crossings"),
            ("KnowCode help topics (kc_help {topic}):", "KnowCode help topics (kc {op: help, topic}):"),
        ];
        for (text, want) in prose {
            assert_eq!(one_text(text, true), want, "{text}");
        }
    }

    /// The default profile is `one`: a single always-loaded tool, its instructions naming calls as ops, and every
    /// answer, help topic and error naming calls as ops too; `core` and `all` keep the separate tools and their names.
    #[test]
    fn the_one_profile_names_every_call_as_an_op() {
        assert_eq!(DEFAULT_PROFILE, "one");
        let s = server("one");
        let tools = s.tool_router.list_all();
        assert_eq!(tools.iter().map(|t| t.name.to_string()).collect::<Vec<_>>(), ["kc"]);
        assert_eq!(serde_json::to_value(&tools[0]).unwrap()["_meta"], json!({ "anthropic/alwaysLoad": true }));
        assert_eq!(s.get_info().instructions.as_deref(), Some(ONE_INSTRUCTIONS));
        assert_eq!(server("core").get_info().instructions.as_deref(), Some(INSTRUCTIONS));
        assert!(!ONE_INSTRUCTIONS.contains("kc_") && ONE_INSTRUCTIONS.contains("kc {op: impact, q: X}"), "{ONE_INSTRUCTIONS}");
        let d = tools[0].description.as_deref().unwrap();
        for word in ["impact", "who calls", "HTTP endpoint", "any repo", "hop", "find", "refs", "context", "search", "locate", "more", "help"] {
            assert!(d.contains(word), "{word}: {d}");
        }
        // Help reaches every tool's topic, by op name too, naming calls as ops, and says nothing is unadvertised.
        let rt = runtime();
        let help = |q: &str| rt.block_on(s.kc(Parameters(serde_json::from_value(json!({ "op": "help", "q": q })).unwrap()))).unwrap();
        let topic = help("more");
        assert!(topic.starts_with("kc {op: more}") && !topic.contains("doesn't advertise"), "{topic}");
        let index = rt.block_on(s.kc(Parameters(serde_json::from_value(json!({ "op": "help" })).unwrap()))).unwrap();
        assert!(!index.contains("not advertised"), "{index}");
        assert!(help("kc").contains("- `op` (string, required): "), "the kc topic lists its parameters");
        // Profiles: one is kc alone, mixed with tools by name too; kc is never in `all` or `core`.
        assert!(server("all").tool_router.list_all().iter().all(|t| t.name != "kc"));
        assert!(server("core").tool_router.list_all().iter().all(|t| t.name != "kc"));
        let mixed: Vec<String> = server("one,kc_trace").tool_router.list_all().into_iter().map(|t| t.name.to_string()).collect();
        assert_eq!(mixed, ["kc", "kc_trace"]);
        assert!(is_one("core,one") && !is_one("core") && !is_one("all,kc_tests"));
    }

    /// A value of `p`'s type, distinct enough to find in the parameters it was read into.
    fn sample(p: &Value, name: &str) -> Value {
        match p["type"].as_str() {
            Some("integer") => json!(213),
            Some("boolean") => json!(true),
            Some("array") => json!([format!("zq-{name}")]),
            _ => json!(format!("zq-{name}")),
        }
    }

    #[test]
    fn profiles_pick_the_advertised_tools() {
        let names = |s: &KnowcodeServer| s.tool_router.list_all().into_iter().map(|t| t.name.to_string()).collect::<Vec<_>>();
        let core = server("core");
        let mut got = names(&core);
        got.sort();
        let mut want: Vec<String> = CORE.iter().map(|s| s.to_string()).collect();
        want.sort();
        assert_eq!(got, want);
        // A tool left out says so in help, and the index names it as not advertised.
        assert!(core.help(Some("kc_unused")).unwrap().contains("doesn't advertise"));
        assert!(core.help(None).unwrap().contains("`kc_unused`: dead-code candidates (not advertised"));
        // Tools add to a profile, with or without their prefix; kc_help is always in; an unknown name is refused.
        let more = server("core,tests,kc_flow");
        assert!(names(&more).contains(&"kc_tests".to_string()) && names(&more).contains(&"kc_flow".to_string()));
        assert_eq!(names(&server("kc_find")), ["kc_find", "kc_help"]);
        assert!(KnowcodeServer::new(Scope { repos: Vec::new(), no_sync: false }, "core,kc_nope").is_err());
        assert_eq!(names(&server("all")).len(), names(&server("core,all")).len());
    }

    /// Exactly the always-loaded tools, core tools all of them, carry Claude Code's `anthropic/alwaysLoad` mark; the
    /// rest stay deferred, kc_locate, kc_more and kc_help among them, and a profile that leaves one out doesn't list
    /// it. Their schemas are the upfront cost under `all`.
    #[test]
    fn only_the_always_loaded_tools_are_marked() {
        let tools = server("all").tool_router.list_all();
        let mut marked: Vec<String> = Vec::new();
        let mut bytes = 0;
        for t in &tools {
            let v = serde_json::to_value(t).unwrap();
            match &v["_meta"] {
                Value::Null => {}
                meta => {
                    assert_eq!(meta, &json!({ "anthropic/alwaysLoad": true }), "{}", t.name);
                    marked.push(t.name.to_string());
                    bytes += v.to_string().len();
                }
            }
        }
        marked.sort();
        let mut want: Vec<String> = ALWAYS_LOADED.iter().map(|s| s.to_string()).collect();
        want.sort();
        assert_eq!(marked, want);
        assert!(marked.iter().all(|t| CORE.contains(&t.as_str())), "always loaded are core tools: {marked:?}");
        for deferred in ["kc_locate", "kc_more", "kc_help"] {
            assert!(!marked.iter().any(|t| t == deferred), "{deferred} is deferred");
        }
        // 2,462 bytes with all eight core tools loaded (0.3.0); 1,770 with five.
        assert!(bytes <= 1_800, "always-loaded schemas {bytes} bytes");
        let few = server("kc_trace,kc_context").tool_router.list_all();
        let marked: Vec<&str> = few.iter().filter(|t| t.meta.is_some()).map(|t| t.name.as_ref()).collect();
        assert_eq!(marked, ["kc_context"]);
    }

    #[test]
    fn caps_long_responses() {
        let items: Vec<Value> = (0..500).map(|i| json!({ "file": format!("Sources/F{i}.swift"), "line": i })).collect();
        let env = json!({ "repo": "r", "items": items, "truncated": false });
        let out = cap(env, 2_000);
        assert!(out.len() <= 2_000);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["truncated"], json!(true));
        assert!(!v["items"].as_array().unwrap().is_empty());
    }

    fn batch(max: usize, n: usize) -> Result<String, ToolError> {
        let inputs: Vec<String> = (0..n).map(|i| format!("name{i}")).collect();
        let answer = |i: &str| -> anyhow::Result<Value> {
            let items: Vec<Value> = (0..40).map(|k| json!({ "file": format!("src/{i}/F{k}.cs"), "line": k })).collect();
            Ok(json!({ "repo": "r", "items": items, "total": 40, "truncated": false }))
        };
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(run_batch(Some(max), inputs, answer))
    }

    #[test]
    fn a_batch_answer_keeps_within_max_chars_and_names_the_inputs_left_out() {
        for (max, n) in [(1_000, 20), (3_000, 20), (8_000, 7), (8_000, 1), (2_000, 3)] {
            let out = batch(max, n).unwrap();
            assert!(out.len() <= max, "max {max}, {n} inputs: {} chars", out.len());
            let v: Value = serde_json::from_str(&out).unwrap();
            let results = v["results"].as_array().unwrap();
            let left: Vec<&str> = v["left_out"].as_array().map(|l| l.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
            let more = v["left_out_more"].as_u64().unwrap_or(0) as usize;
            // Every input is answered or accounted for, in order.
            assert_eq!(results.len() + left.len() + more, n, "{out}");
            for (k, r) in results.iter().enumerate() {
                assert_eq!(r["input"], json!(format!("name{k}")));
            }
            if results.len() < n {
                assert_eq!(v["truncated"], json!(true));
                assert_eq!(left.first().copied(), Some(format!("name{}", results.len()).as_str()), "{out}");
                assert!(v["hint"].as_str().unwrap().contains("call again with them"), "{out}");
            }
        }
        // A batch above the bound is refused, saying so.
        let err = batch(8_000, MAX_BATCH + 1).unwrap_err();
        assert!(err.contains(&format!("at most {MAX_BATCH} per call")), "{err}");
    }

    #[test]
    fn a_cut_to_fit_max_chars_is_said_and_makes_the_answer_incomplete_when_it_is_what_was_asked() {
        // A symbol's callers, cut: more to read, the answer complete.
        let callers: Vec<Value> = (0..300).map(|i| json!({ "file": format!("Sources/F{i}.swift"), "line": i })).collect();
        let env = json!({ "repo": "r", "items": [{ "callers": callers }], "total": 1, "truncated": false, "cursor": null });
        let v: Value = serde_json::from_str(&cap(env, 2_000)).unwrap();
        assert_eq!((v["complete"].clone(), v.get("incomplete")), (json!(true), None), "{v}");
        let i = &v["more"][0];
        assert_eq!((i["reason"].as_str(), i["detail"].as_str().is_some_and(|d| d.starts_with("/items/0/callers: "))), (Some("budget"), true), "{v}");
        assert!(v.get("cursor").is_none(), "no dummy cursor: {v}");
        // Its implementations, cut: incomplete.
        let conformers: Vec<Value> = (0..300).map(|i| json!({ "file": format!("Sources/F{i}.swift"), "line": i })).collect();
        let env = json!({ "repo": "r", "items": [{ "conformers": conformers }], "total": 1, "truncated": false });
        let v: Value = serde_json::from_str(&cap(env, 2_000)).unwrap();
        assert_eq!(v["complete"], json!(false));
        assert!(v["incomplete"][0]["detail"].as_str().is_some_and(|d| d.starts_with("/items/0/conformers: ")), "{v}");
        // Within the cap: complete, and it says so.
        let v: Value = serde_json::from_str(&cap(json!({ "repo": "r", "items": [1], "total": 1, "truncated": false }), 2_000)).unwrap();
        assert_eq!((v.get("complete"), v.get("incomplete")), (Some(&json!(true)), None), "{v}");
        // A batch leaving inputs out is incomplete; one answering every input is not.
        let v: Value = serde_json::from_str(&batch(1_000, 20).unwrap()).unwrap();
        assert_eq!(v["complete"], json!(false));
        assert!(v["incomplete"].as_array().unwrap().iter().any(|i| i["reason"] == "budget" && i["detail"].as_str().unwrap().starts_with("inputs: ")), "{v}");
        let v: Value = serde_json::from_str(&batch(80_000, 1).unwrap()).unwrap();
        assert_eq!((v.get("complete"), v.get("incomplete"), v["results"][0].get("complete")), (Some(&json!(true)), None, Some(&json!(true))), "{v}");
    }

    #[test]
    fn caps_array_responses() {
        let repos: Vec<Value> = (0..50).map(|i| json!({ "repo": format!("r{i}"), "files": vec![i; 40] })).collect();
        let out = cap(Value::Array(repos), 2_000);
        assert!(out.len() <= 2_000, "{}", out.len());
    }

    #[test]
    fn a_capped_list_says_how_many_items_it_lost_and_how_to_get_them() {
        let callers: Vec<Value> = (0..300).map(|i| json!({ "file": format!("Sources/F{i}.swift"), "line": i })).collect();
        let env = json!({ "repo": "r", "items": [{ "callers": callers }], "truncated": false });
        let v = cap_full(env.clone(), 2_000);
        let kept = v["items"][0]["callers"].as_array().unwrap().len() as u64;
        assert_eq!(v["omitted"]["/items/0/callers"], json!(300 - kept));
        assert!(v["hint"].as_str().unwrap().contains("max_chars"));
        // Output in the compact form: the count is in the list's `more` entry, with how to get the rest.
        let v: Value = serde_json::from_str(&cap(env, 2_000)).unwrap();
        let i = &v["more"][0];
        assert_eq!(i["detail"], json!(format!("/items/0/callers: {} items cut to fit max_chars", 300 - kept)), "{v}");
        assert!(i["hint"].as_str().unwrap().contains("max_chars"), "{v}");
    }

    #[test]
    fn text_is_cut_on_a_line_with_what_was_left_out() {
        let text: String = (0..500).map(|i| format!("line {i}\n")).collect();
        let out = cap_text(&text, 1_000);
        assert!(out.len() <= 1_000, "{}", out.len());
        let last = out.lines().last().unwrap();
        assert!(last.starts_with("… ") && last.contains("more lines cut at max_chars=1000"), "{last}");
        assert_eq!(cap_text("short\n", 1_000), "short\n");
        // A path is never cut inside: the cut moves before its first line.
        let paths: String = (1..=20).map(|i| format!("path {i} of 20: 3 hops, weight 6\n{}", "  A.b (x.cs:1)\n    ↳ calls\n    C.d (y.cs:2)\n".repeat(3))).collect();
        let out = cap_text(&paths, 1_000);
        let body: Vec<&str> = out.lines().filter(|l| !l.starts_with('…')).collect();
        assert!(body.last().is_some_and(|l| l.ends_with("C.d (y.cs:2)")), "{out}");
        assert_eq!(body.iter().filter(|l| l.starts_with("path ")).count() * 10, body.len(), "{out}");
    }

    #[test]
    fn the_size_cap_never_cuts_a_rename_group_or_a_path_and_renews_cut_cursors() {
        let steps: Vec<Value> = (0..40).map(|i| serde_json::json!({ "qualified": format!("S{i}.m"), "file": "a.cs", "line": i })).collect();
        let env = serde_json::json!({
            "items": [{ "paths": [{ "steps": steps }, { "steps": [{ "qualified": "X.y" }] }] }],
            "group": (0..60).map(|i| format!("G{i}.m")).collect::<Vec<_>>(),
            "total": 1, "truncated": false,
        });
        let out: Value = serde_json::from_str(&cap(env, 1_500)).unwrap();
        assert_eq!(out["group"].as_array().map(Vec::len), Some(60), "{out}");
        for p in out["items"][0]["paths"].as_array().unwrap() {
            assert!(matches!(p["steps"].as_array().map(Vec::len), Some(40 | 1)), "{out}");
        }
    }

    /// The lists an answer is about (a type's implementations, its DI registrations, an endpoint's HTTP links) go last:
    /// every other list down to none, the source shortened and their rows' detail dropped first. A conformer cut to
    /// fit while callers were shown was what an agent's grep found that the answer had.
    #[test]
    fn the_size_cap_cuts_the_lists_an_answer_is_about_last() {
        let conformer = |i: usize| {
            json!({ "file": format!("src/Impl{i}.cs"), "line": 3, "end_line": 90, "kind": "class", "name": format!("Impl{i}"),
                "signature": format!("public sealed class Impl{i} : IThing, IDisposable"), "doc": "Does the thing for one tenant." })
        };
        let caller = |i: usize| json!({ "file": "src/Use.cs", "line": i, "from": format!("Use.M{i}"), "kind": "type_ref", "confidence": "exact" });
        let link = |i: usize| {
            json!({ "repo": "web", "file": format!("src/pages/p{i}.ts"), "line": 7, "start_line": 5, "end_line": 20, "kind": "function",
                "symbol": format!("load{i}"), "confidence": "exact", "assumptions": ["same_deployment"] })
        };
        let env = |n: usize| {
            json!({ "items": [{
                "symbol": { "name": "IThing", "kind": "protocol" }, "source": "x".repeat(1_500),
                "conformers": (0..n).map(conformer).collect::<Vec<_>>(),
                "callers": (0..60).map(caller).collect::<Vec<_>>(),
                "di": { "registrations": [{ "file": "src/Startup.cs", "line": 9, "service": "IThing", "implementation": "Impl0" }], "injection_sites": 4 },
                "http": [{ "verb": "GET", "path": "/things", "links": (0..n).map(link).collect::<Vec<_>>() }],
            }], "total": 1, "truncated": false })
        };
        let v: Value = serde_json::from_str(&cap(env(25), 6_000)).unwrap();
        let item = &v["items"][0];
        // Only lists on the side were cut: more to read, the answer complete.
        assert_eq!((v["complete"].clone(), v.get("incomplete")), (json!(true), None), "{v}");
        let cut: Vec<&str> = v["more"].as_array().unwrap().iter().filter_map(|i| i["detail"].as_str()).collect();
        assert!(cut.iter().any(|d| d.starts_with("/items/0/callers")) && !cut.iter().any(|d| d.contains("conformers") || d.contains("links")), "{v}");
        assert!(item["callers"].as_array().is_some_and(|c| c.len() <= 1) && item["source"].as_str().is_some_and(|s| s.len() < 400), "{v}");
        // Every conformer, by name and place, without its signature and doc.
        let rows = item["conformers"].as_array().unwrap();
        assert_eq!(rows.len(), 25, "{v}");
        assert!(rows.iter().all(|r| r["name"].is_string() && r["line"] == 3 && r.get("signature").is_none() && r.get("doc").is_none()), "{v}");
        assert_eq!(item["di"]["registrations"][0]["service"], "IThing", "{v}");
        // Links said as text rows, what they share said once.
        let http = &item["http"][0];
        assert_eq!(http["links"].as_array().map(Vec::len), Some(25), "{v}");
        assert_eq!((http["links"][0].as_str(), http["links_repo"].as_str(), http["links_assumptions"].clone()), (Some("src/pages/p0.ts:7 function load0"), Some("web"), json!(["same_deployment"])), "{v}");
        // Too many to fit even so: the answer lists are cut then, saying so.
        let v: Value = serde_json::from_str(&cap(env(200), 6_000)).unwrap();
        let cut: Vec<&str> = v["incomplete"].as_array().unwrap().iter().filter_map(|i| i["detail"].as_str()).collect();
        assert!(cut.iter().any(|d| d.starts_with("/items/0/conformers") || d.starts_with("/items/0/http/0/links")), "{v}");
        assert!(v.to_string().len() <= 6_000, "{v}");
    }

    #[test]
    fn empty_answers_say_what_was_searched_and_short_lists_how_to_get_more() {
        let env = json!({ "repo": "/x/Orders", "items": [], "total": 0, "truncated": false });
        let v = explain(env, "no definition named Foo", "limit");
        assert_eq!(v["note"], "no definition named Foo in Orders");
        let env = json!({ "repo": "/x/Orders", "items": [1, 2], "total": 9, "truncated": true });
        let v = explain(env, "no definition named Foo", "limit");
        assert!(v.get("note").is_none());
        assert_eq!(v["hint"], "showing 2 of 9; raise limit for more");
    }

    #[test]
    fn reference_edges_name_reference_kinds() {
        assert_eq!(ref_kinds(&[], &["calls,uses".into()]).unwrap(), ["call", "type_ref"]);
        assert_eq!(ref_kinds(&["member".into()], &["calls".into()]).unwrap(), ["call", "member"]);
        assert!(ref_kinds(&[], &["http".into()]).is_err());
    }

    #[test]
    fn context_bundles_say_which_lists_are_cut() {
        let env = json!({ "items": [{ "symbol": { "name": "Save" }, "callers": [1], "callers_summary": { "total": 4 },
            "callees": [], "callees_total": 0, "members_total": 0, "source_truncated": true, "source_mode": "full" }] });
        let hints = context_hints(env)["items"][0]["hints"].clone();
        assert_eq!(hints, json!(["callers: 1 of 4 shown; kc_refs Save lists them all", "source cut at the budget; raise budget or pass source=skeleton"]));
        // A controller action's: kc_impact, which follows its routes to the code requesting them.
        let env = json!({ "items": [{ "symbol": { "name": "Save", "container": "OrdersController" }, "callers": [1],
            "callers_summary": { "total": 4 }, "http": [{ "role": "serves" }] }] });
        let hints = context_hints(env)["items"][0]["hints"].clone();
        assert_eq!(hints, json!(["callers: 1 of 4 in code shown; kc_impact OrdersController.Save lists every caller by hop, over HTTP too"]));
    }

    #[test]
    fn text_answers_put_their_notes_first_and_name_mcp_parameters() {
        let text = "method A  a.cs:1\n  calls method B  b.cs:2\n… truncated at the node limit: at least 3 more (raise --limit, lower --depth)\n(depth 1 shown; pass depth 2 (up to 3) for more)\n";
        let out = notes_first(&mcp_names(text));
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[0].starts_with("… truncated") && lines[0].contains("raise limit, lower depth"), "{out}");
        assert!(lines[1].starts_with("(depth 1 shown"));
        assert_eq!(lines[2..], ["method A  a.cs:1", "  calls method B  b.cs:2"]);
        assert_eq!(depth_note(1, 3, true).as_deref(), Some("(depth 1 shown; pass depth 2 (up to 3) for more)"));
        assert!(depth_note(3, 3, true).is_none() && depth_note(1, 3, false).is_none());
    }

    #[test]
    fn inner_lists_with_totals_say_how_many_are_left_out() {
        let env = json!({ "repo": "/x/r", "items": [{ "partners": [1, 2], "partners_total": 19 }], "total": 1, "truncated": false });
        let v = explain(env, "no partner", "limit");
        assert_eq!(v["truncated"], true);
        assert_eq!(v["hint"], "partners: showing 2 of 19; raise limit for more");
        let env = json!({ "repo": "/x/r", "items": [{ "reviewers": [], "reviewers_total": 0 }], "total": 1, "truncated": false });
        assert_eq!(explain(env, "no author wrote nope.cs", "limit")["note"], "no author wrote nope.cs in r");
    }

    #[test]
    fn capping_a_page_moves_next_offset() {
        let items: Vec<Value> = (0..200).map(|i| json!({ "file": format!("Sources/F{i}.swift"), "line": i })).collect();
        let env = json!({ "repo": "r", "items": items, "truncated": false, "offset": 40, "next_offset": 240 });
        let v: Value = serde_json::from_str(&cap(env, 2_000)).unwrap();
        let shown = v["items"].as_array().unwrap().len() as u64;
        assert_eq!(v["next_offset"], json!(40 + shown));
    }

    /// What the compact form drops, anywhere in an answer: timings, the build stamp, index generations, what cursors
    /// are made from, and symbols' row ids. The places they were found.
    fn noise(v: &Value, at: &str, out: &mut Vec<String>) {
        match v {
            Value::Array(a) => a.iter().enumerate().for_each(|(i, x)| noise(x, &format!("{at}[{i}]"), out)),
            Value::Object(m) => {
                let symbol = m.contains_key("qualified") || (m.contains_key("kind") && m.contains_key("name"));
                for (k, x) in m {
                    let timing = k == "ms" || k.ends_with("_ms") || matches!(k.as_str(), "elapsed" | "duration" | "took");
                    let stamp = matches!(k.as_str(), "knowcode" | "build" | "generation" | "_resume");
                    let row_id = k == "id" && symbol && !m.contains_key("text") && x.is_i64();
                    if timing || stamp || row_id {
                        out.push(format!("{at}.{k}"));
                    }
                    noise(x, &format!("{at}.{k}"), out);
                }
            }
            _ => {}
        }
    }

    /// The compact form's promise for every tool (see `compact`): no timings, build stamps, generations or row ids.
    /// Each tool is called as an agent calls it (the search tools, which answer from a child process, through the
    /// operation the child runs, cut and compacted as the server does).
    #[test]
    fn invariant_no_tool_answers_with_timings_build_stamps_or_row_ids() {
        let cache = crate::ops::tests::test_cache();
        let base = cache.dir("mcp-noise");
        let root = crate::ops::tests::repo(
            &base,
            "shop",
            &[
                ("src/Shop.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n"),
                ("src/IOrders.cs", "namespace Shop;\npublic interface IOrders { void Place(); }\n"),
                ("src/Orders.cs", "namespace Shop;\npublic class Orders : IOrders\n{\n    public void Place() { Audit.Log(); }\n}\n"),
                ("src/Audit.cs", "namespace Shop;\npublic static class Audit\n{\n    public static void Log() { }\n}\n"),
                ("src/Checkout.cs", "namespace Shop;\npublic class Checkout\n{\n    private readonly IOrders _orders;\n    public void Run() { _orders.Place(); Audit.Log(); }\n}\n"),
                ("tests/CheckoutTests.cs", "namespace Shop.Tests;\npublic class CheckoutTests\n{\n    public void Runs() { new Shop.Checkout().Run(); }\n}\n"),
            ],
        );
        crate::ops::tests::commit_and_tag(&root, "v1");
        // Indexed before the answers are taken (no budget: the sync finishes): a cold first call on a loaded machine
        // can outlast the server's freshness budget and say it is still indexing, which this test isn't about.
        drop(crate::refresh::open(&root, false).unwrap());
        let s = server("all");
        let s = KnowcodeServer { default: Scope { repos: vec![root.clone()], no_sync: false }, ..s };
        fn p<T: serde::de::DeserializeOwned>(v: Value) -> Parameters<T> {
            Parameters(serde_json::from_value(v).unwrap())
        }
        let rt = runtime();
        let mut answers: Vec<(&str, Result<String, ToolError>)> = rt.block_on(async {
            vec![
                ("kc_symbol_at", s.kc_symbol_at(p(json!({ "file": "src/Orders.cs", "line": 4 }))).await),
                ("kc_find", s.kc_find(p(json!({ "query": "Orders" }))).await),
                ("kc_locate", s.kc_locate(p(json!({ "query": "place orders" }))).await),
                ("kc_refs", s.kc_refs(p(json!({ "name": "Log" }))).await),
                ("kc_context", s.kc_context(p(json!({ "symbol": "Orders.Place" }))).await),
                ("kc_context repo", s.kc_context(p(json!({ "symbol": "shop" }))).await),
                ("kc_trace", s.kc_trace(p(json!({ "symbol": "Audit.Log", "format": "json" }))).await),
                ("kc_path", s.kc_path(p(json!({ "from": "Checkout.Run", "to": "Audit.Log", "format": "json" }))).await),
                ("kc_flow", s.kc_flow(p(json!({ "entry": "Checkout.Run", "format": "json" }))).await),
                ("kc_impact", s.kc_impact(p(json!({ "symbol": "Audit.Log", "format": "json" }))).await),
                ("kc_unused", s.kc_unused(p(json!({}))).await),
                ("kc_rename_preview", s.kc_rename_preview(p(json!({ "symbol": "Audit.Log", "new_name": "Write" }))).await),
                ("kc_coupled", s.kc_coupled(p(json!({ "file": "src/Orders.cs" }))).await),
                ("kc_tests", s.kc_tests(p(json!({ "targets": ["src/Checkout.cs"] }))).await),
                ("kc_hotspots", s.kc_hotspots(p(json!({}))).await),
                ("kc_ownership", s.kc_ownership(p(json!({}))).await),
                ("kc_reviewers", s.kc_reviewers(p(json!({ "files": ["src/Orders.cs"] }))).await),
                ("kc_history", s.kc_history(p(json!({ "symbol": "Orders.Place" }))).await),
                ("kc_overview", s.kc_overview(p(json!({}))).await),
                ("kc_areas", s.kc_areas(p(json!({}))).await),
                ("kc_notes", s.kc_notes(p(json!({}))).await),
                ("kc_status", s.kc_status(p(json!({}))).await),
            ]
        });
        // The tools answering from a child: the operation it runs, then the cap and the compact form.
        let scope = Scope { repos: vec![root.clone()], no_sync: false };
        let opts = kc_index::search::SearchOptions {
            pattern: "Log".into(),
            regex: false,
            case: kc_index::search::Case::Sensitive,
            word: false,
            langs: Vec::new(),
            globs: Vec::new(),
            max_lines: 50_000,
        };
        let search = ops::search(&ops::SearchParams { opts, limit: 1, offset: 0, context: 0, files_only: false, fit: None }, &scope).unwrap().env;
        let page = crate::cursor::more(search["cursor"].as_str().expect("a cut search continues")).unwrap();
        answers.push(("kc_search", Ok(cap(search, DEFAULT_CHARS))));
        answers.push(("kc_more", Ok(cap(page, DEFAULT_CHARS))));
        let ast = ops::AstParams {
            opts: kc_index::structural::AstOptions { pattern: "Audit.Log()".into(), lang: "csharp".into(), max_matches: 1000, ..Default::default() },
            limit: 20,
            offset: 0,
        };
        answers.push(("kc_grep_ast", Ok(cap(ops::grep_ast(&ast, &scope).unwrap().env, DEFAULT_CHARS))));
        for (tool, answer) in answers {
            let text = answer.unwrap_or_else(|e| panic!("{tool}: {e}"));
            let v: Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{tool}: {e}: {text}"));
            let mut found = Vec::new();
            noise(&v, "", &mut found);
            assert!(found.is_empty(), "{tool} leaks {found:?}: {text}");
            // A search's hits, and a page of them, are text rows: `file:line kind symbol | text`.
            if matches!(tool, "kc_search" | "kc_more" | "kc_grep_ast") {
                let rows = v["items"].as_array().unwrap();
                assert!(!rows.is_empty() && rows.iter().all(|r| r.as_str().is_some_and(|r| r.contains(".cs:") && r.contains(" | "))), "{tool}: {text}");
            }
            // A clean, current answer from one repo says nothing about repos or defaults.
            for key in ["provenance", "repos"] {
                assert!(v.get(key).is_none(), "{tool}: {key}: {text}");
            }
            assert!(v.get("dirty") != Some(&Value::Bool(false)), "{tool}: {text}");
            // Every answer says whether it is complete, true included.
            assert!(v["complete"].is_boolean(), "{tool}: {text}");
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The cut/continuation contract at the default caps: an answer cut by its default limit says so (`complete:
    /// false`, an `incomplete` entry with how to get the rest), and its cursor continues to exactly the rest; the
    /// trees name the parameter to raise; the context bundle keeps within its cap and its cut lists continue too.
    #[test]
    fn default_caps_cut_with_what_was_left_out_and_continue() {
        let cache = crate::ops::tests::test_cache();
        let base = cache.dir("mcp-caps");
        let mut files: Vec<(String, String)> = vec![
            ("src/Shop.csproj".into(), "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n".into()),
            ("src/Audit.cs".into(), "namespace Shop;\npublic static class Audit\n{\n    public static void Log() { }\n}\n".into()),
        ];
        for i in 0..40 {
            files.push((format!("src/C{i}.cs"), format!("namespace Shop;\npublic class C{i}\n{{\n    public void Run() {{ Audit.Log(); }}\n}}\n")));
        }
        let files: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        let root = crate::ops::tests::repo(&base, "shop", &files);
        let s = KnowcodeServer { default: Scope { repos: vec![root.clone()], no_sync: false }, ..server("all") };
        fn p<T: serde::de::DeserializeOwned>(v: Value) -> Parameters<T> {
            Parameters(serde_json::from_value(v).unwrap())
        }
        let rt = runtime();

        // kc_refs: 20 of 40, the rest by cursor; the target said once, beside the rows.
        let v: Value = serde_json::from_str(&rt.block_on(s.kc_refs(p(json!({ "name": "Audit.Log" })))).unwrap()).unwrap();
        let shown = v["items"].as_array().unwrap();
        assert_eq!((shown.len(), v["total"].as_u64(), v["complete"].as_bool()), (20, Some(40), Some(false)), "{v}");
        let capped = v["incomplete"].as_array().unwrap().iter().find(|i| i["reason"] == "capped" && i["detail"].as_str().unwrap().starts_with("items")).expect("a capped entry");
        assert!(capped["detail"].as_str().unwrap().contains("20 of 40") && capped["hint"].as_str().unwrap().contains("kc_more"), "{v}");
        assert_eq!(v["targets"][0]["qualified"], "Audit.Log", "{v}");
        assert!(shown.iter().all(|r| r["from"].as_str().is_some_and(|f| f.ends_with(".Run")) && r.get("col").is_none()), "{v}");
        let rest = crate::cursor::more(&crate::compact::resolve(v["cursor"].as_str().unwrap()).unwrap()).unwrap();
        let rest: Value = serde_json::from_str(&cap(rest, DEFAULT_CHARS)).unwrap();
        let mut seen: Vec<String> = shown.iter().chain(rest["items"].as_array().unwrap()).map(|r| r["from"].as_str().unwrap().to_string()).collect();
        seen.sort();
        seen.dedup();
        assert_eq!((seen.len(), rest.get("complete"), rest.get("incomplete")), (40, Some(&json!(true)), None), "{rest}");

        // kc_impact and kc_trace: cut at their default limits (30, 25), saying how to get the rest.
        let impact = rt.block_on(s.kc_impact(p(json!({ "symbol": "Audit.Log" })))).unwrap();
        assert_eq!(impact.lines().filter(|l| l.trim_start().starts_with("called by method")).count(), 30, "{impact}");
        assert!(impact.contains("incomplete (capped)") && impact.contains("kc_more {cursor: \"kc~") && impact.contains("raise limit"), "{impact}");
        assert!(!impact.lines().next().unwrap().ends_with("; complete"), "{impact}");
        // A use site in the dependent's own file is said by its line, or not at all on the dependent's own line.
        assert!(!impact.lines().any(|l| l.contains("called by method") && l.contains(" at ")), "{impact}");
        let trace = rt.block_on(s.kc_trace(p(json!({ "symbol": "Audit.Log", "direction": "in" })))).unwrap();
        assert!(trace.contains("truncated at the node limit") && trace.contains("raise limit"), "{trace}");
        // Asked deeper than they go, they walk as deep as their schema and help say (`RANGES`).
        for (range, v) in [
            (&IMPACT_DEPTH, rt.block_on(s.kc_impact(p(json!({ "symbol": "Audit.Log", "depth": 99, "format": "json" }))))),
            (&TRACE_DEPTH, rt.block_on(s.kc_trace(p(json!({ "symbol": "Audit.Log", "depth": 99, "format": "json" }))))),
        ] {
            let v: Value = serde_json::from_str(&v.unwrap()).unwrap();
            assert_eq!(v["depth"], json!({ "asked": 99, "walked": range.max }), "{v}");
        }
        assert!(!trace.lines().next().unwrap().ends_with("; complete"), "{trace}");

        // kc_context: within its cap; its callers list cut, with the cursor that continues it: more to read, the
        // answer about the method complete.
        let out = rt.block_on(s.kc_context(p(json!({ "symbol": "Audit.Log" })))).unwrap();
        assert!(out.len() <= CONTEXT_CHARS, "{} chars", out.len());
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["complete"], true, "{v}");
        let callers = v["more"].as_array().unwrap().iter().find(|i| i["detail"].as_str().unwrap().contains("callers")).expect("callers cut");
        assert!(callers["hint"].as_str().unwrap().contains("items[0].callers_cursor"), "{v}");
        assert!(crate::compact::resolve(v["items"][0]["callers_cursor"].as_str().unwrap()).is_ok(), "{v}");

        // Every answer that continues by cursor names the kc_more call in a hint: kc_more and kc_help are deferred in
        // Claude Code, so the answer itself says how to page.
        let scope = Scope { repos: vec![root.clone()], no_sync: false };
        let opts = kc_index::search::SearchOptions {
            pattern: "Audit.Log".into(),
            regex: false,
            case: kc_index::search::Case::Sensitive,
            word: false,
            langs: Vec::new(),
            globs: Vec::new(),
            max_lines: 50_000,
        };
        let search = ops::search(&ops::SearchParams { opts, limit: 5, offset: 0, context: 0, files_only: false, fit: None }, &scope).unwrap().env;
        let answers = [
            ("kc_find", rt.block_on(s.kc_find(p(json!({ "query": "C" })))).unwrap()),
            ("kc_refs", rt.block_on(s.kc_refs(p(json!({ "name": "Audit.Log" })))).unwrap()),
            ("kc_context", rt.block_on(s.kc_context(p(json!({ "symbol": "Audit.Log" })))).unwrap()),
            ("kc_impact", rt.block_on(s.kc_impact(p(json!({ "symbol": "Audit.Log" })))).unwrap()),
            ("kc_impact json", rt.block_on(s.kc_impact(p(json!({ "symbol": "Audit.Log", "format": "json" })))).unwrap()),
            ("kc_search", cap(search, DEFAULT_CHARS)),
        ];
        let mut paged = 0;
        for (tool, text) in answers {
            if text.contains("kc~") {
                paged += 1;
                assert!(text.contains("kc_more {cursor: "), "{tool} continues by cursor without naming kc_more: {text}");
            }
        }
        assert!(paged >= 5, "{paged} of the answers were cut");

        // Under `one` (the default), the same answers name the calls that continue them as ops of `kc`.
        let one = KnowcodeServer { default: Scope { repos: vec![root.clone()], no_sync: false }, ..server("one") };
        let kc = |args: Value| rt.block_on(one.kc(p(args.clone()))).unwrap_or_else(|e| panic!("{args}: {e}"));
        for (what, text) in [
            ("refs", kc(json!({ "op": "refs", "q": "Audit.Log" }))),
            ("context", kc(json!({ "op": "context", "q": "Audit.Log" }))),
            ("impact", kc(json!({ "op": "impact", "q": "Audit.Log" }))),
            ("trace", kc(json!({ "op": "trace", "q": "Audit.Log", "direction": "in" }))),
        ] {
            assert!(!text.contains("kc_"), "{what} names a tool `one` doesn't advertise: {text}");
        }
        let v: Value = serde_json::from_str(&kc(json!({ "op": "refs", "q": "Audit.Log" }))).unwrap();
        let hint = v["incomplete"].as_array().unwrap().iter().find(|i| i["reason"] == "capped").unwrap()["hint"].as_str().unwrap().to_string();
        assert_eq!(hint, "kc {op: more, cursor: <`cursor`>}");
        // The call the hint names is one `kc` takes: it pages on.
        let cursor = v["cursor"].as_str().unwrap();
        // (kc_more runs in a child process, so the call is checked as kc reads it, and the page as kc_more makes it.)
        let (tool, args) = one_call(serde_json::from_value(json!({ "op": "more", "cursor": cursor })).unwrap()).unwrap();
        assert_eq!((tool.as_str(), args["cursor"].as_str()), ("kc_more", Some(cursor)));
        let page = crate::cursor::more(&crate::compact::resolve(cursor).unwrap()).unwrap();
        assert_eq!(page["items"].as_array().unwrap().len(), 20, "{page}");
        let impact = kc(json!({ "op": "impact", "q": "Audit.Log" }));
        assert!(impact.contains("kc {op: more, cursor: \"kc~"), "{impact}");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A place or a paths filter written after a repo's name, the way questions name them, resolves in kc_context,
    /// kc_impact, kc_refs and kc_search; one resolving to nothing is refused, saying what didn't resolve, never
    /// answered `complete` with no items.
    #[test]
    fn places_and_globs_after_a_repo_name_resolve_or_say_what_did_not() {
        let cache = crate::ops::tests::test_cache();
        let base = cache.dir("mcp-places");
        let audit = "namespace Shop;\npublic static class Audit\n{\n    public static void Log() { }\n}\n\npublic class Run\n{\n    public void Go() { Audit.Log(); }\n}\n";
        let root = crate::ops::tests::repo(&base, "shop", &[("src/Audit.cs", audit)]);
        let s = KnowcodeServer { default: Scope { repos: vec![root], no_sync: false }, ..server("all") };
        fn p<T: serde::de::DeserializeOwned>(v: Value) -> Parameters<T> {
            Parameters(serde_json::from_value(v).unwrap())
        }
        let rt = runtime();
        let v: Value = serde_json::from_str(&rt.block_on(s.kc_context(p(json!({ "symbol": "shop/src/Audit.cs:4" })))).unwrap()).unwrap();
        assert_eq!(v["items"][0]["symbol"]["name"], "Log", "{v}");
        let impact = rt.block_on(s.kc_impact(p(json!({ "symbol": "shop/src/Audit.cs:4" })))).unwrap();
        assert!(impact.contains("Run.Go"), "{impact}");
        let v: Value = serde_json::from_str(&rt.block_on(s.kc_refs(p(json!({ "name": "shop/src/Audit.cs:4" })))).unwrap()).unwrap();
        assert_eq!((v["name"].as_str(), v["total"].as_u64()), (Some("Audit.Log"), Some(1)), "{v}");
        // Resolving to nothing: refused, saying so.
        for (tool, err) in [
            ("kc_context", rt.block_on(s.kc_context(p(json!({ "symbol": "shop/src/Gone.cs:4" }))))),
            ("kc_impact", rt.block_on(s.kc_impact(p(json!({ "symbol": "shop/src/Audit.cs:6" }))))),
            ("kc_refs", rt.block_on(s.kc_refs(p(json!({ "name": "shop/src/Audit.cs:6" }))))),
            ("kc_trace", rt.block_on(s.kc_trace(p(json!({ "symbol": "shop/src/Audit.cs#Gone" }))))),
        ] {
            let err = err.expect_err(tool);
            assert!(err.contains("didn't resolve"), "{tool}: {err}");
        }
        // A search whose filter matched no file (it runs in a child, so its envelope stands in here).
        let note = "paths matched 0 files (**/web/src/**): in shop; nothing was searched";
        let env = json!({ "items": [], "total": 0, "note": note, "stats": { "searchable_files": 0 } });
        assert_eq!(searched_nothing(&env, true).as_deref(), Some(note));
        assert_eq!(searched_nothing(&env, false), None);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A text answer's first line says it is complete when it is; a partial one says why on the lines after it.
    #[test]
    fn a_complete_text_answer_says_so_in_its_header() {
        let env = json!({ "repo": "/w/shop", "commit": "0123456789ab", "dirty": false, "complete": true });
        assert_eq!(header(&env, true), "repo shop @ 0123456; complete\n");
        let env = json!({ "repo": "/w/shop", "commit": "0123456789ab", "complete": false,
            "incomplete": [{ "reason": "capped", "detail": "items: 3 of 9 shown" }] });
        let h = header(&env, false);
        assert!(h.starts_with("repo shop @ 0123456\n") && h.contains("incomplete (capped)"), "{h}");
    }

    #[test]
    fn a_capped_tree_leaves_out_its_rows_other_ways_first() {
        let deep = format!("src/{}/g.cs", "deep/".repeat(40));
        let rows = format!(
            "a\n  called by method A.b  src/a.cs:12  [exact] direct (also via C.d at src/a.cs:14) (also via E.f at src/a.cs:20)\n  called by method G.h  {deep}:3  [exact] direct (also via C.d at {deep}:9)\n"
        );
        // From the last row up, until enough is gone (room for the note included); each way counted.
        let (text, left) = unway_text(&rows, 1);
        assert_eq!(
            (text, left),
            (format!("a\n  called by method A.b  src/a.cs:12  [exact] direct (also via C.d at src/a.cs:14) (also via E.f at src/a.cs:20)\n  called by method G.h  {deep}:3  [exact] direct\n"), 1)
        );
        let (text, left) = unway_text(&rows, usize::MAX);
        assert_eq!((text, left), (format!("a\n  called by method A.b  src/a.cs:12  [exact] direct\n  called by method G.h  {deep}:3  [exact] direct\n"), 3));
    }

    #[test]
    fn use_sites_in_the_same_file_are_said_by_their_line() {
        assert_eq!(lean_site("  used by method A.b  src/a.cs:10  [exact] direct at src/a.cs:12"), "  used by method A.b  src/a.cs:10  [exact] direct at line 12");
        assert_eq!(lean_site("  used by method A.b  src/a.cs:10  [exact] direct at src/a.cs:10"), "  used by method A.b  src/a.cs:10  [exact] direct");
        // Another file, or no site: as it was.
        let other = "  calls method C.d  src/c.cs:4  [exact] at src/a.cs:12";
        assert_eq!(lean_site(other), other);
        let note = "incomplete (depth): shop: 3 symbols the walk stopped at not checked";
        assert_eq!(lean_site(note), note);
        // A prefix of another file's name isn't the same file.
        let prefix = "  used by method A.b  src/a.cs:10  [exact] direct at a.cs:12";
        assert_eq!(lean_site(prefix), prefix);
        // A row leading with its own site already says each place once.
        let led = "  called by method A.b  src/a.cs:12  [exact] direct (also src/a.cs:14) (declared at line 10)";
        assert_eq!(lean_site(led), led);
    }

    /// Every tool listing locations quotes the lines under them (`cite`), unless the call says `cite: none`; the mode
    /// holds for the call's query alone.
    #[test]
    fn answers_quote_their_lines_unless_the_call_says_cite_none() {
        let cache = crate::ops::tests::test_cache();
        let base = cache.dir("mcp-cite");
        let root = crate::ops::tests::repo(
            &base,
            "shop",
            &[
                ("src/Audit.cs", "public static class Audit\n{\n    public static void Log() { }\n}\n"),
                ("src/Checkout.cs", "public class Checkout\n{\n    public void Run() { Audit.Log(); }\n}\n"),
            ],
        );
        let s = KnowcodeServer { default: Scope { repos: vec![root.clone()], no_sync: false }, ..server("all") };
        fn p<T: serde::de::DeserializeOwned>(v: Value) -> Parameters<T> {
            Parameters(serde_json::from_value(v).unwrap())
        }
        let rt = runtime();
        let quoted = "3 | public void Run() { Audit.Log(); }";
        let (tree, bare, refs, refs_bare, bad) = rt.block_on(async {
            (
                s.kc_trace(p(json!({ "symbol": "Audit.Log" }))).await.unwrap(),
                s.kc_trace(p(json!({ "symbol": "Audit.Log", "cite": "none" }))).await.unwrap(),
                s.kc_refs(p(json!({ "name": "Log" }))).await.unwrap(),
                s.kc_refs(p(json!({ "name": "Log", "cite": "none" }))).await.unwrap(),
                s.kc_flow(p(json!({ "entry": "Checkout.Run", "cite": "all" }))).await,
            )
        });
        assert!(tree.contains(quoted), "{tree}");
        assert!(!bare.contains(" | "), "{bare}");
        let refs: Value = serde_json::from_str(&refs).unwrap();
        assert_eq!(refs["items"][0]["cite"]["text"], "public void Run() { Audit.Log(); }", "{refs}");
        // The row says where already: the compact cite doesn't repeat it.
        assert!(refs["items"][0]["cite"].get("at").is_none(), "{refs}");
        assert!(!refs_bare.contains("\"cite\""), "{refs_bare}");
        assert_eq!(bad.unwrap_err(), "cite must be brief, lines, check or none, not all");
        let _ = std::fs::remove_dir_all(&base);
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread().worker_threads(2).max_blocking_threads(6).enable_all().build().unwrap()
    }

    fn sleep_child(secs: &str) -> anyhow::Result<Value> {
        let mut c = std::process::Command::new("sleep");
        c.arg(secs);
        kc_store::limits::output(c, kc_store::limits::Kind::Search, "sleep", None)?;
        Ok(json!({ "items": [], "total": 0, "truncated": false }))
    }

    /// Execution limits, not just output limits: a cheap call isn't held behind a slow one, a child past its deadline
    /// is killed and the answer says `incomplete: deadline`, and cancelling a call kills its child. One test: the lanes
    /// and the deadline variable are process-wide.
    #[test]
    fn invariant_slow_calls_neither_block_cheap_ones_nor_outlive_their_deadline_or_cancellation() {
        let rt = runtime();
        rt.block_on(async {
            // A slow in-process call holds the main lane; a cheap one waits at most SLOW and a little, not 6 s.
            let slow = tokio::spawn(run(None, || {
                std::thread::sleep(std::time::Duration::from_secs(6));
                Ok(json!({ "items": [] }))
            }));
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let t = std::time::Instant::now();
            run(None, || Ok(json!({ "items": [] }))).await.unwrap();
            assert!(t.elapsed() < std::time::Duration::from_secs(5), "the cheap call waited {:?}", t.elapsed());
            assert!(!slow.is_finished());

            // A child-process call runs in its own lane, beside both. Past its deadline, the child is killed and the
            // answer says so.
            // SAFETY: no other test in this binary runs a search child.
            unsafe { std::env::set_var("KNOWCODE_SEARCH_DEADLINE", "1") };
            let t = std::time::Instant::now();
            let out: Value = serde_json::from_str(&run_child(None, || match sleep_child("30") {
                Err(e) if e.downcast_ref::<kc_store::limits::Cut>().is_some() => Ok(json!({ "items": [], "complete": false })),
                other => other,
            })
            .await
            .unwrap())
            .unwrap();
            assert!(t.elapsed() < std::time::Duration::from_secs(5), "{:?}", t.elapsed());
            assert_eq!(out["complete"], false);
            assert_eq!(out["incomplete"][0]["reason"], "deadline", "{out}");
            assert!(out["incomplete"][0]["hint"].as_str().unwrap().contains("KNOWCODE_SEARCH_DEADLINE"));
            unsafe { std::env::set_var("KNOWCODE_SEARCH_DEADLINE", "0") };

            // Cancelling the call's operation (as `call_tool` does on `notifications/cancelled`) kills its child.
            let op = kc_store::limits::Operation::new();
            let call = tokio::spawn(OPERATION.scope(op.clone(), run_child(None, || sleep_child("30"))));
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            let t = std::time::Instant::now();
            op.cancel();
            let err = call.await.unwrap().unwrap_err();
            assert!(t.elapsed() < std::time::Duration::from_secs(5), "{:?}", t.elapsed());
            assert!(err.contains("cancelled"), "{err}");
            unsafe { std::env::remove_var("KNOWCODE_SEARCH_DEADLINE") };
            slow.await.unwrap().unwrap();
        });
    }
}
