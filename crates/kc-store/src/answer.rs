//! The answer contract every command shares: what makes an answer partial ([`Incomplete`]), which repos it was read
//! from ([`Provenance`]), and what each edge rests on ([`Evidence`]). The closed sets ([`Reason`], [`Mechanism`],
//! [`Assumption`]) are documented in docs/answers.md; a new member is a contract change.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// Why an answer is partial.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// A limit cut a list: the detail says which list and how many were left out.
    Capped,
    /// The answer was cut to fit a size budget (`max_chars`).
    Budget,
    /// A time or work budget ran out before the answer was finished.
    Deadline,
    /// Answered from data that doesn't match the working tree: derived data from an earlier index generation, or a
    /// repo read from its last sync.
    Stale,
    /// Files or repos could not be read; what comes from them is as last indexed.
    Failed,
    /// A repo the answer needed is not indexed or not cloned.
    MissingRepo,
    /// A construct the analysis doesn't model, when it is known to be there.
    Unsupported,
    /// The evidence policy asked for left edges out (see [`EvidencePolicy`]): what lies past them was not read.
    Policy,
    /// Content the answer may have drawn on is blocked by a repo's AI privacy file (see `kc_index::privacy`): the detail
    /// names the repo and level, never a path inside the blocked area.
    Withheld,
    /// A walk (`impact`, `trace`, `path`) stopped at its depth, the one asked for or the deepest it goes, where
    /// symbols lead further: the item's `frontier` names them, and the hint says how to go on.
    Depth,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::Capped => "capped",
            Reason::Budget => "budget",
            Reason::Deadline => "deadline",
            Reason::Stale => "stale",
            Reason::Failed => "failed",
            Reason::MissingRepo => "missing_repo",
            Reason::Unsupported => "unsupported",
            Reason::Policy => "policy",
            Reason::Withheld => "withheld",
            Reason::Depth => "depth",
        }
    }
}

/// One reason an answer is partial: what is missing and, when there is one, how to get it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Incomplete {
    pub reason: Reason,
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl Incomplete {
    pub fn new(reason: Reason, detail: impl Into<String>) -> Self {
        Incomplete { reason, detail: detail.into(), hint: None }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// A list cut by a limit: `list` showing `shown` of `total`.
    pub fn capped(list: &str, shown: usize, total: usize) -> Self {
        Incomplete::new(Reason::Capped, format!("{list}: {shown} of {total} shown"))
    }

    /// The text rendering: `incomplete (capped): callers: 10 of 40 shown; raise --limit`.
    pub fn line(&self) -> String {
        match &self.hint {
            Some(h) => format!("incomplete ({}): {}; {h}", self.reason.as_str(), self.detail),
            None => format!("incomplete ({}): {}", self.reason.as_str(), self.detail),
        }
    }
}

/// A repo an answer consulted: the queried repos, and the others it read (dependencies and dependents opened, repos
/// crossed by HTTP, the bus, SQL or DI, workspace summaries used).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    /// The repo's root.
    pub repo: String,
    /// HEAD when it was read (for a repo answered from its workspace summary: HEAD at its last sync); `None` outside
    /// a git checkout or when not recorded.
    pub commit: Option<String>,
    /// The index generation read ([`crate::Store::generation`]); `None` when not recorded.
    pub generation: Option<String>,
    /// Whether tracked files differ from `commit`; `None` when not checked (a repo consulted on the side, whose
    /// status isn't cached).
    pub dirty: Option<bool>,
    /// Whether the index was checked against the working tree for this answer and matched it. False when answered
    /// from an earlier sync (its workspace summary, `--no-sync`) or when files could not be read.
    pub fresh: bool,
}

impl Provenance {
    /// The text rendering: `orders-api @ 1a2b3c4 (dirty), from its last sync`.
    pub fn line(&self) -> String {
        let name = std::path::Path::new(&self.repo).file_name().map_or(self.repo.clone(), |n| n.to_string_lossy().into_owned());
        let commit = self.commit.as_deref().map(|c| format!(" @ {}", &c[..c.len().min(7)])).unwrap_or_default();
        let dirty = if self.dirty == Some(true) { " (dirty)" } else { "" };
        let fresh = if self.fresh { "" } else { ", from its last sync" };
        format!("{name}{commit}{dirty}{fresh}")
    }
}

/// How an edge's target was found. A closed set: an evidence policy names these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mechanism {
    /// A SCIP index (a compiler's answer) covers the site.
    Scip,
    /// An import names the module the target is in (TypeScript, Python, a dependency's package, a module source).
    Import,
    /// The receiver's static type (or the caller's own type, for `self`/`this`/`super`, or a type named as the
    /// receiver) has the member.
    ReceiverType,
    /// The caller's own type or file declares the name.
    LexicalScope,
    /// The namespaces the file sees narrowed same-named candidates to one (C#).
    Namespace,
    /// The project references of the caller's compilation unit ruled the other candidates out (C#).
    ProjectScope,
    /// One definition (or one type's) has the name among those in scope.
    NameUnique,
    /// Several definitions have the name, and one was preferred by a heuristic (a dominant HTTP handler or SQL name).
    NameDominant,
    /// Several definitions have the name and nothing decides between them.
    NameAmbiguous,
    /// A dependency-injection registration binds the service to this implementation.
    DiBinding,
    /// An HTTP route the client requests matches the route the action serves.
    HttpRoute,
    /// A message type one side publishes and the other consumes.
    BusMessage,
    /// A SQL object matched by its name (an Entity Framework table, a procedure call, a statement's table).
    SqlName,
    /// A template names the component or element (Angular selectors).
    Template,
    /// A layout convention links the two (Ansible role directories, Terraform configuration directories, NgRx
    /// actions by name).
    Convention,
    /// A module placed by its file name, with no config mapping the import.
    FilenameGuess,
    /// The relation is written in a declaration: a base list, containment (a member of its type, a task of its
    /// play), an implementation of an interface member.
    Declaration,
}

/// What an edge's evidence did not check. A closed set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Assumption {
    /// Both ends run in one deployment (an HTTP route, a message queue).
    SameDeployment,
    /// Both ends use one database.
    SameDatabase,
    /// The revision a dependency is pinned at was not checked against the indexed checkout.
    RevisionUnverified,
    /// A directory layout stands in for configuration.
    DirectoryConvention,
    /// The caller or the target is in no project file, or past a project reference the index can't place (C#).
    ProjectUnverified,
    /// Both ends use one message destination (queue, exchange, virtual host): at least one side doesn't say which.
    SameDestination,
    /// The client reaches the serving repo through a host its URL, base address or settings name, which deploy or
    /// proxy configuration maps to that repo: which environment's configuration applies (and, for settings, which key
    /// the request reads) was not checked.
    ConfiguredHost,
    /// A generated API client's operation is the controller action of that name (under the controller named like
    /// the client's tag): the API description the client was generated from was not read.
    OperationName,
    /// Which server repo a client sends to is not known: no base address, configuration, client package or other
    /// request of the client's names it, so every repo with an action of the operation's name is a candidate.
    ServerUnverified,
}

/// What an edge rests on: how its target was found, and what that did not check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub mechanism: Mechanism,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assumptions: Vec<Assumption>,
}

impl Evidence {
    pub fn new(mechanism: Mechanism) -> Self {
        Evidence { mechanism, assumptions: Vec::new() }
    }

    pub fn assuming(mut self, a: Assumption) -> Self {
        if !self.assumptions.contains(&a) {
            self.assumptions.push(a);
            self.assumptions.sort();
        }
        self
    }

    /// [`Evidence::assuming`] each of `all`.
    pub fn assuming_all(self, all: &[Assumption]) -> Self {
        all.iter().fold(self, |e, a| e.assuming(*a))
    }

    /// The evidence for an edge of `confidence` whose exact form rests on `exact`: a name match otherwise. `None` for
    /// confidences that make no edge (`unresolved`, `external_likely`).
    pub fn by_confidence(confidence: &str, exact: Mechanism) -> Option<Evidence> {
        match confidence {
            "exact" => Some(Evidence::new(exact)),
            "unique_name" => Some(Evidence::new(Mechanism::NameUnique)),
            "ambiguous" | "nearest" => Some(Evidence::new(Mechanism::NameAmbiguous)),
            _ => None,
        }
    }

    /// The evidence of a walk's edge of kind `edge` (see [`crate::trace::EDGE_KINDS`]) and `confidence`: `resolved`
    /// is the reference's own evidence when the edge is a resolved reference; `link` the confidence of the crossing
    /// (HTTP, SQL) that made it (`dominant` when one of several handlers was preferred); `precise` when a SCIP index
    /// answered it.
    pub fn of_edge(edge: &str, confidence: &str, resolved: Option<&Evidence>, link: Option<&str>, precise: bool) -> Option<Evidence> {
        if edge.is_empty() {
            return None;
        }
        if precise {
            return Some(Evidence::new(Mechanism::Scip));
        }
        let crossing = |m: Mechanism| Some(Evidence::new(if link == Some("dominant") { Mechanism::NameDominant } else { m }));
        match edge {
            "http" => crossing(Mechanism::HttpRoute),
            "sql" => crossing(Mechanism::SqlName),
            "bus" => Some(Evidence::new(Mechanism::BusMessage)),
            // A DbSet's accessors, an Angular child: written in the code that declares them.
            "conforms_to" | "conformer" | "witness" | "dispatch" | "member" | "runs" | "child" => Some(Evidence::new(Mechanism::Declaration)),
            "templates" | "dialog" => Some(Evidence::new(Mechanism::Template)),
            "ngrx" => Evidence::by_confidence(confidence, Mechanism::Convention),
            // A binding's own evidence when the walk recorded it (with what it assumed: a provider placed by its
            // directory), else the registration.
            "via_di" | "constructs" if resolved.is_none_or(|e| e.mechanism != Mechanism::Scip) => {
                Some(resolved.filter(|e| e.mechanism == Mechanism::DiBinding).cloned().unwrap_or_else(|| Evidence::new(Mechanism::DiBinding)))
            }
            // A resolved reference: its own evidence. An exact edge without it is a gap in the plumbing, never
            // papered over.
            _ => resolved.cloned().or_else(|| if confidence == "exact" { None } else { Evidence::by_confidence(confidence, Mechanism::NameUnique) }),
        }
    }
}

/// Whether an answer's rows quote the source lines they rest on (see [`Cite`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CiteMode {
    /// The first rows' `cite` quotes one line each (the site, or the line naming a declaration), shortened; the
    /// rest say where their lines are without quoting them.
    #[default]
    Brief,
    /// Every row's `cite` quotes its lines: up to 3 for a declaration, each up to 160 characters.
    Lines,
    /// Each row's `cite` says where its lines are and whether they match the index, without quoting them.
    Check,
    /// No `cite`.
    None,
}

impl std::str::FromStr for CiteMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "" | "brief" => Ok(CiteMode::Brief),
            "lines" => Ok(CiteMode::Lines),
            "check" => Ok(CiteMode::Check),
            "none" => Ok(CiteMode::None),
            other => Err(format!("cite must be brief, lines, check or none, not {other}")),
        }
    }
}

/// How a cited line compares with the index the answer was built from, checked when the answer is built. A closed
/// set, documented in docs/answers.md.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Check {
    /// The file is byte for byte what the index read: the lines are the ones the answer's facts come from.
    Verified,
    /// The file changed since it was indexed; the cited line still names what the row cites (or there is no name to
    /// look for), so the place holds, but what the index drew from the file may not.
    Changed,
    /// The file changed since it was indexed and what the row cites is on another line now: `at` quotes it there,
    /// `was` says where the index had it.
    Moved,
    /// The file is gone from the working tree, or no longer names what the row cites.
    Gone,
    /// The lines could not be checked: the file isn't in the index the answer read, can't be read, or is blocked.
    Unchecked,
}

impl Check {
    pub fn as_str(self) -> &'static str {
        match self {
            Check::Verified => "verified",
            Check::Changed => "changed",
            Check::Moved => "moved",
            Check::Gone => "gone",
            Check::Unchecked => "unchecked",
        }
    }

    /// Whether the cited lines differ from what the index read.
    pub fn drifted(self) -> bool {
        matches!(self, Check::Changed | Check::Moved | Check::Gone)
    }
}

/// The source lines a row of an answer rests on, read from the working tree when the answer is built: for an edge,
/// the line where it happens (its `site`: the call, the request, the publish); for a symbol reached without one, its
/// declaration's first lines. Bounded: what the cap leaves out is said in `cut`. Checked against the index: `check`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cite {
    /// `file:line` or `file:start-end`: the lines quoted.
    pub at: String,
    /// The lines against the index: `verified`, or the drift.
    pub check: Check,
    /// What drifted, when `check` isn't `verified`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drift: Option<String>,
    /// For `moved`: where the index had the lines (`file:line`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub was: Option<String>,
    /// The repo `at` is in, when it isn't the row's own (a bus hop's publish, in the publisher's repo).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// The lines, with their common indentation removed. Absent when the file could not be read (then `cut` says
    /// why).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// What the cap left out (the rest of a declaration, the end of a long line), or why nothing could be quoted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cut: Option<String>,
    /// For a site quoted with the rest of its statement: the site's own line; the other lines are the statement's.
    /// Not serialized: the row's own `site` or `line` says it.
    #[serde(skip)]
    pub line: Option<u32>,
}

/// What a walk does with an edge, under an [`EvidencePolicy`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Listed, and walked past.
    Follow,
    /// Listed, not walked past (an `ambiguous` edge by default).
    Leaf,
    /// Neither: counted in the answer's `policy`.
    Exclude,
}

/// What an evidence policy does with `ambiguous` edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ambiguous {
    Leaf,
    Exclude,
    Follow,
}

/// Which edges a walk (`trace`, `impact`, `path`, `flow`) may follow, by what they rest on: the mechanisms it trusts
/// and the assumptions it refuses. Written as a preset (`default`, `confirmed`, `all`), mechanisms, or both, with `-`
/// taking a mechanism away or refusing an assumption: `confirmed`, `confirmed,name_unique`, `all,-filename_guess`,
/// `scip,receiver_type`, `default,-same_deployment`.
///
/// - `default`: every edge as before: `exact` and `unique_name` followed, `ambiguous` listed but not followed.
/// - `confirmed`: only a compiler's or an explicit link ([`CONFIRMED`]); no name match, no convention, nothing
///   `ambiguous`.
/// - `all`: every edge, `ambiguous` followed too.
///
/// Mechanisms named without a preset are the only ones followed; `name_ambiguous` among them follows `ambiguous`
/// edges, which are otherwise left out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidencePolicy {
    name: String,
    /// The mechanisms followed; `None` for any.
    mechanisms: Option<BTreeSet<Mechanism>>,
    ambiguous: Ambiguous,
    refused: BTreeSet<Assumption>,
}

/// The mechanisms `confirmed` follows: a compiler's answer, an import, a type, a declaration, a registration, a
/// route or message both sides name, a template naming the component. Not a name match (`name_unique`,
/// `name_dominant`, `sql_name`), a convention or a file name.
pub const CONFIRMED: &[Mechanism] = &[
    Mechanism::Scip,
    Mechanism::Import,
    Mechanism::ReceiverType,
    Mechanism::LexicalScope,
    Mechanism::Namespace,
    Mechanism::ProjectScope,
    Mechanism::DiBinding,
    Mechanism::HttpRoute,
    Mechanism::BusMessage,
    Mechanism::Template,
    Mechanism::Declaration,
];

/// Every mechanism.
pub const MECHANISMS: &[Mechanism] = &[
    Mechanism::Scip,
    Mechanism::Import,
    Mechanism::ReceiverType,
    Mechanism::LexicalScope,
    Mechanism::Namespace,
    Mechanism::ProjectScope,
    Mechanism::NameUnique,
    Mechanism::NameDominant,
    Mechanism::NameAmbiguous,
    Mechanism::DiBinding,
    Mechanism::HttpRoute,
    Mechanism::BusMessage,
    Mechanism::SqlName,
    Mechanism::Template,
    Mechanism::Convention,
    Mechanism::FilenameGuess,
    Mechanism::Declaration,
];

/// Every assumption.
pub const ASSUMPTIONS: &[Assumption] = &[
    Assumption::SameDeployment,
    Assumption::SameDatabase,
    Assumption::SameDestination,
    Assumption::RevisionUnverified,
    Assumption::DirectoryConvention,
    Assumption::ProjectUnverified,
    Assumption::ConfiguredHost,
    Assumption::OperationName,
    Assumption::ServerUnverified,
];

impl Default for EvidencePolicy {
    fn default() -> Self {
        EvidencePolicy { name: "default".into(), mechanisms: None, ambiguous: Ambiguous::Leaf, refused: BTreeSet::new() }
    }
}

/// The member of a closed set (`Mechanism`, `Assumption`) named `word`.
fn closed<T: serde::de::DeserializeOwned>(word: &str) -> Option<T> {
    serde_json::from_value(serde_json::Value::String(word.to_string())).ok()
}

/// The name a closed set's member is written as.
pub fn name_of<T: Serialize>(v: T) -> String {
    serde_json::to_value(v).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
}

impl EvidencePolicy {
    fn preset(word: &str) -> Option<EvidencePolicy> {
        let p = |mechanisms: Option<BTreeSet<Mechanism>>, ambiguous| EvidencePolicy { name: String::new(), mechanisms, ambiguous, refused: BTreeSet::new() };
        match word {
            "default" => Some(EvidencePolicy::default()),
            "confirmed" => Some(p(Some(CONFIRMED.iter().copied().collect()), Ambiguous::Exclude)),
            "all" => Some(p(None, Ambiguous::Follow)),
            _ => None,
        }
    }

    /// The policy `spec` names (see the type's docs); empty is `default`.
    pub fn parse(spec: &str) -> Result<EvidencePolicy, String> {
        let words: Vec<&str> = spec.split(',').map(str::trim).filter(|w| !w.is_empty()).collect();
        let mut p = EvidencePolicy::default();
        for (i, w) in words.iter().enumerate() {
            let (minus, word) = match w.strip_prefix('-') {
                Some(rest) => (true, rest),
                None => (false, w.strip_prefix('+').unwrap_or(w)),
            };
            if let Some(preset) = EvidencePolicy::preset(word) {
                if minus {
                    return Err(format!("-{word}: a preset can't be taken away"));
                }
                if i == 0 {
                    p = preset;
                } else {
                    // A later preset widens: what either follows.
                    p.mechanisms = match (p.mechanisms.take(), preset.mechanisms) {
                        (Some(mut a), Some(b)) => {
                            a.extend(b);
                            Some(a)
                        }
                        _ => None,
                    };
                    p.ambiguous = match (p.ambiguous, preset.ambiguous) {
                        (Ambiguous::Follow, _) | (_, Ambiguous::Follow) => Ambiguous::Follow,
                        (Ambiguous::Leaf, _) | (_, Ambiguous::Leaf) => Ambiguous::Leaf,
                        _ => Ambiguous::Exclude,
                    };
                }
                continue;
            }
            if let Some(m) = closed::<Mechanism>(word) {
                if i == 0 && !minus {
                    // Mechanisms without a preset: only those.
                    p = EvidencePolicy { name: String::new(), mechanisms: Some(BTreeSet::new()), ambiguous: Ambiguous::Exclude, refused: BTreeSet::new() };
                }
                let set = p.mechanisms.get_or_insert_with(|| MECHANISMS.iter().copied().collect());
                if minus {
                    set.remove(&m);
                } else {
                    set.insert(m);
                }
                if m == Mechanism::NameAmbiguous {
                    p.ambiguous = if minus { Ambiguous::Exclude } else { Ambiguous::Follow };
                }
                continue;
            }
            if let Some(a) = closed::<Assumption>(word) {
                if !minus {
                    return Err(format!("{word} is an assumption: write -{word} to leave out the edges making it"));
                }
                p.refused.insert(a);
                continue;
            }
            return Err(format!(
                "unknown evidence {word:?}: a preset (default, confirmed, all), a mechanism ({}), or -<assumption> ({})",
                MECHANISMS.iter().map(|m| name_of(*m)).collect::<Vec<_>>().join(", "),
                ASSUMPTIONS.iter().map(|a| name_of(*a)).collect::<Vec<_>>().join(", ")
            ));
        }
        p.name = if words.is_empty() { "default".into() } else { words.join(",") };
        Ok(p)
    }

    /// The policy as asked for: `default`, `confirmed`, `all,-filename_guess`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether this is `default`: every edge as walks always took them.
    pub fn is_default(&self) -> bool {
        self.mechanisms.is_none() && self.ambiguous == Ambiguous::Leaf && self.refused.is_empty()
    }

    /// Whether `ambiguous` edges are walked past.
    pub fn follows_ambiguous(&self) -> bool {
        self.ambiguous == Ambiguous::Follow
    }

    /// What a walk does with an edge of `confidence` resting on `evidence`. An edge without a confidence (between a
    /// type and its members, a play and its tasks) is always followed; one that links nothing (`unresolved`) never is.
    /// Under `default`: `exact` and `unique_name` followed, `ambiguous` a leaf, whatever the evidence.
    pub fn judge(&self, confidence: &str, evidence: Option<&Evidence>) -> Verdict {
        if confidence.is_empty() {
            return Verdict::Follow;
        }
        if !matches!(confidence, "exact" | "unique_name" | "ambiguous") {
            return Verdict::Leaf;
        }
        if let Some(allowed) = &self.mechanisms
            && !evidence.is_some_and(|e| allowed.contains(&e.mechanism))
        {
            return Verdict::Exclude;
        }
        if evidence.is_some_and(|e| e.assumptions.iter().any(|a| self.refused.contains(a))) {
            return Verdict::Exclude;
        }
        if confidence == "ambiguous" {
            return match self.ambiguous {
                Ambiguous::Leaf => Verdict::Leaf,
                Ambiguous::Exclude => Verdict::Exclude,
                Ambiguous::Follow => Verdict::Follow,
            };
        }
        Verdict::Follow
    }

    /// What an excluded edge is counted under: the assumption refused, else its mechanism, else `ambiguous` (an
    /// ambiguous edge of a mechanism the policy follows); `unknown` without evidence.
    pub fn why(&self, confidence: &str, evidence: Option<&Evidence>) -> String {
        let Some(e) = evidence else { return "unknown".into() };
        if let Some(a) = e.assumptions.iter().find(|a| self.refused.contains(a)) {
            return name_of(*a);
        }
        if confidence == "ambiguous" && self.mechanisms.as_ref().is_none_or(|m| m.contains(&e.mechanism)) {
            return "ambiguous".into();
        }
        name_of(e.mechanism)
    }
}

/// What an evidence policy left out of a walk: the answer's `policy`, present whenever the policy isn't `default`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyReport {
    /// The policy, as asked for.
    pub evidence: String,
    /// Edges the walk met and neither listed nor followed: a lower bound on what the policy cut, since nothing past
    /// them was read.
    pub excluded: usize,
    /// `excluded` by each edge's mechanism (or the assumption refused, or `ambiguous`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub excluded_by: BTreeMap<String, usize>,
}

impl PolicyReport {
    /// The `incomplete` entry saying what was left out, when anything was.
    pub fn incomplete(&self) -> Option<Incomplete> {
        if self.excluded == 0 {
            return None;
        }
        let by: Vec<String> = self.excluded_by.iter().map(|(k, n)| format!("{k} {n}")).collect();
        let edges = if self.excluded == 1 { "edge" } else { "edges" };
        Some(
            Incomplete::new(
                Reason::Policy,
                format!("evidence {}: {} {edges} left out ({}); what lies past them was not read", self.evidence, self.excluded, by.join(", ")),
            )
            .with_hint("the default policy (--evidence default; MCP evidence: \"default\") lists them"),
        )
    }
}

/// In debug builds (every test), panics when an edge of `confidence` that links something (`exact`, `unique_name`,
/// `ambiguous`) has no evidence: each place deciding a confidence must say what it rests on.
/// `what` names the edge, made only on failure: release builds pay nothing.
#[track_caller]
pub fn debug_check(what: impl FnOnce() -> String, confidence: &str, evidence: Option<&Evidence>) {
    if cfg!(debug_assertions) && evidence.is_none() && matches!(confidence, "exact" | "unique_name" | "ambiguous") {
        panic!("{}: a {confidence} edge without evidence", what());
    }
}

/// How much older index generation `old` is than `now` ([`crate::Store::generation`]: the time of the sync that last
/// changed the index, then the resolver), in words: `3 minutes`, `2 days`; `None` when either has no time.
pub fn generation_age(old: &str, now: &str) -> Option<String> {
    let at = |g: &str| g.split('|').next()?.parse::<u128>().ok();
    let (a, b) = (at(old)?, at(now)?);
    let secs = (b.saturating_sub(a) / 1_000_000_000) as u64;
    Some(match secs {
        0..120 => format!("{secs} seconds"),
        120..7200 => format!("{} minutes", secs / 60),
        7200..172_800 => format!("{} hours", secs / 3600),
        _ => format!("{} days", secs / 86_400),
    })
}

/// The `stale` entry for derived data (`what`: areas, a ranking) computed from index generation `old`, now `now`. A
/// ranking still being built in the background ([`crate::rank::PENDING`]) is a `deadline` entry instead: none was used.
pub fn stale_derived(what: &str, old: &str, now: &str) -> Incomplete {
    if old == crate::rank::PENDING {
        return Incomplete::new(Reason::Deadline, format!("{what}: being computed in the background, not ready within this call's budget; in index order here"))
            .with_hint("ask again shortly for the ranked order");
    }
    let resolver = |g: &str| g.rsplit('|').next().map(str::to_string);
    let age = match generation_age(old, now) {
        Some(a) if resolver(old) == resolver(now) => format!("the index as it was {a} earlier"),
        _ => "an earlier index (another resolver)".to_string(),
    };
    Incomplete::new(Reason::Stale, format!("{what}: computed from {age}, not recomputed for this answer"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_closed_sets_serialize_as_their_documented_names() {
        let i = Incomplete::new(Reason::MissingRepo, "x").with_hint("clone it");
        assert_eq!(serde_json::to_value(&i).unwrap(), serde_json::json!({ "reason": "missing_repo", "detail": "x", "hint": "clone it" }));
        assert_eq!(i.line(), "incomplete (missing_repo): x; clone it");
        let e = Evidence::new(Mechanism::ReceiverType).assuming(Assumption::ProjectUnverified);
        assert_eq!(serde_json::to_value(&e).unwrap(), serde_json::json!({ "mechanism": "receiver_type", "assumptions": ["project_unverified"] }));
        for r in [Reason::Capped, Reason::Budget, Reason::Deadline, Reason::Stale, Reason::Failed, Reason::MissingRepo, Reason::Unsupported, Reason::Policy] {
            assert_eq!(serde_json::to_value(r).unwrap(), serde_json::json!(r.as_str()));
        }
        for c in [Check::Verified, Check::Changed, Check::Moved, Check::Gone, Check::Unchecked] {
            assert_eq!(serde_json::to_value(c).unwrap(), serde_json::json!(c.as_str()));
            assert_eq!(c.drifted(), matches!(c, Check::Changed | Check::Moved | Check::Gone));
        }
        assert_eq!("check".parse::<CiteMode>(), Ok(CiteMode::Check));
        assert_eq!("".parse::<CiteMode>(), Ok(CiteMode::Brief));
    }

    #[test]
    fn evidence_policies_name_what_they_follow() {
        let e = |m: Mechanism| Some(Evidence::new(m));
        let route = Evidence::new(Mechanism::HttpRoute).assuming(Assumption::SameDeployment);
        let default = EvidencePolicy::parse("").unwrap();
        assert!(default.is_default() && default.name() == "default");
        assert_eq!(EvidencePolicy::parse("default").unwrap(), default);
        // Default: as walks always went, whatever the evidence.
        assert_eq!(default.judge("unique_name", e(Mechanism::NameDominant).as_ref()), Verdict::Follow);
        assert_eq!(default.judge("ambiguous", e(Mechanism::NameAmbiguous).as_ref()), Verdict::Leaf);
        assert_eq!(default.judge("", None), Verdict::Follow);
        assert_eq!(default.judge("unresolved", None), Verdict::Leaf);
        // Confirmed: a compiler's or an explicit link only.
        let confirmed = EvidencePolicy::parse("confirmed").unwrap();
        assert_eq!(confirmed.judge("exact", e(Mechanism::Scip).as_ref()), Verdict::Follow);
        assert_eq!(confirmed.judge("exact", Some(&route)), Verdict::Follow);
        for m in [Mechanism::NameUnique, Mechanism::NameDominant, Mechanism::FilenameGuess, Mechanism::Convention, Mechanism::SqlName] {
            assert_eq!(confirmed.judge("unique_name", e(m).as_ref()), Verdict::Exclude, "{m:?}");
        }
        assert_eq!(confirmed.judge("ambiguous", e(Mechanism::DiBinding).as_ref()), Verdict::Exclude);
        assert_eq!(confirmed.why("ambiguous", e(Mechanism::DiBinding).as_ref()), "ambiguous");
        assert_eq!(confirmed.why("unique_name", e(Mechanism::NameDominant).as_ref()), "name_dominant");
        // All: ambiguous edges followed too.
        let all = EvidencePolicy::parse("all").unwrap();
        assert!(all.follows_ambiguous());
        assert_eq!(all.judge("ambiguous", e(Mechanism::NameAmbiguous).as_ref()), Verdict::Follow);
        // Changed presets, mechanisms alone, refused assumptions.
        let p = EvidencePolicy::parse("confirmed,name_unique").unwrap();
        assert_eq!(p.judge("unique_name", e(Mechanism::NameUnique).as_ref()), Verdict::Follow);
        let p = EvidencePolicy::parse("all,-filename_guess").unwrap();
        assert_eq!(p.judge("unique_name", e(Mechanism::FilenameGuess).as_ref()), Verdict::Exclude);
        assert_eq!(p.judge("ambiguous", e(Mechanism::NameAmbiguous).as_ref()), Verdict::Follow);
        let p = EvidencePolicy::parse("scip,receiver_type").unwrap();
        assert_eq!(p.judge("exact", e(Mechanism::ReceiverType).as_ref()), Verdict::Follow);
        assert_eq!(p.judge("exact", e(Mechanism::Import).as_ref()), Verdict::Exclude);
        assert_eq!(p.judge("exact", None), Verdict::Exclude);
        let p = EvidencePolicy::parse("default,-same_deployment").unwrap();
        assert_eq!(p.judge("exact", Some(&route)), Verdict::Exclude);
        assert_eq!(p.why("exact", Some(&route)), "same_deployment");
        assert_eq!(p.judge("ambiguous", e(Mechanism::NameAmbiguous).as_ref()), Verdict::Leaf);
        assert!(EvidencePolicy::parse("same_deployment").is_err());
        assert!(EvidencePolicy::parse("-all").is_err());
        assert!(EvidencePolicy::parse("strict").unwrap_err().contains("name_dominant"));
    }

    /// Whatever the edge, a stricter policy never does more with it: `confirmed` ≤ `default` ≤ `all`, where an
    /// excluded edge is less than a leaf, which is less than one followed.
    #[test]
    fn stricter_policies_never_follow_more() {
        let rank = |v: Verdict| match v {
            Verdict::Exclude => 0,
            Verdict::Leaf => 1,
            Verdict::Follow => 2,
        };
        let chain = ["confirmed", "default", "all"].map(|p| EvidencePolicy::parse(p).unwrap());
        let narrowed = [EvidencePolicy::parse("all,-name_dominant").unwrap(), EvidencePolicy::parse("default,-same_database").unwrap()];
        for confidence in ["exact", "unique_name", "ambiguous", "unresolved", ""] {
            for m in MECHANISMS {
                for a in [None, Some(Assumption::SameDatabase), Some(Assumption::DirectoryConvention)] {
                    let mut ev = Evidence::new(*m);
                    if let Some(a) = a {
                        ev = ev.assuming(a);
                    }
                    let v: Vec<u8> = chain.iter().map(|p| rank(p.judge(confidence, Some(&ev)))).collect();
                    assert!(v[0] <= v[1] && v[1] <= v[2], "{confidence} {m:?} {a:?}: {v:?}");
                    assert!(rank(narrowed[0].judge(confidence, Some(&ev))) <= v[2]);
                    assert!(rank(narrowed[1].judge(confidence, Some(&ev))) <= v[1]);
                }
            }
        }
        let report = PolicyReport { evidence: "confirmed".into(), excluded: 3, excluded_by: [("name_unique".to_string(), 2), ("ambiguous".to_string(), 1)].into() };
        let i = report.incomplete().unwrap();
        assert_eq!(i.reason, Reason::Policy);
        assert!(i.detail.starts_with("evidence confirmed: 3 edges left out (ambiguous 1, name_unique 2)"), "{}", i.detail);
        assert!(PolicyReport { excluded: 0, ..report }.incomplete().is_none());
    }

    #[test]
    fn a_dominant_crossing_is_a_name_preference_not_a_route() {
        assert_eq!(Evidence::of_edge("http", "unique_name", None, Some("dominant"), false).unwrap().mechanism, Mechanism::NameDominant);
        assert_eq!(Evidence::of_edge("http", "exact", None, Some("exact"), false).unwrap().mechanism, Mechanism::HttpRoute);
        assert_eq!(Evidence::of_edge("calls", "exact", None, None, true).unwrap().mechanism, Mechanism::Scip);
        assert!(Evidence::of_edge("calls", "unresolved", None, None, false).is_none());
    }
}
