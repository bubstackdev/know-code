//! What every walk (`trace`, `impact`, `path`, `flow`) decides about an edge, in one place.
//!
//! - [`Gate`]: the edge kinds a walk lists and follows, its evidence policy (an edge is listed and followed, listed as
//!   a leaf, or left out and counted), and the query it runs for: a cancelled query's walk stops at its next
//!   expansion ([`Gate::expand`]).
//! - [`Crossing`]: a hop across repos (an HTTP route, the bus, SQL, a deploy, a Terraform module) from a symbol to the
//!   code on the other side, each arrival with the confidence it has across the link and the evidence it rests on.
//!   `trace`, `impact` and the frontier check read their crossings as these (see `ops::HttpCrossing::crossings`).
//! - Confidence across a link, for every walk: [`across`]. An ambiguous link makes the edge ambiguous; a link matched
//!   by name (`unique_name`: an SQL object by its name) or preferred by a heuristic (`dominant`: one of several
//!   handlers or schemas) reaching exact code is `unique_name`, and its evidence (`sql_name`, `name_dominant`) lets the
//!   policy decide. Before 0.2 `trace` and `impact` kept the code's own `exact` there.
//!
//! `walk::Graph` reads every edge of every walk, in a repo and across repos, through its gate: `path` and `flow` through
//! `walk::Graph::out` and `walk::Graph::into`, `trace` and `impact` through `walk::Graph::tree_edges` and the crossings
//! (see `crate::tree`). The one difference left is what the trees list in a repo: a type's conformances and
//! conformers, an interface member's implementations, and a call through an interface member several types implement
//! at its own confidence (`kc_store::trace::tree_in_edges`), where `path` and `flow` count it as one of several.

use std::hash::Hash;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use kc_store::answer::{Evidence, Verdict};
use kc_store::limits::Operation;
use kc_store::query::SymbolRow;
use kc_store::trace::{self, EdgeFilter, Via};

/// A walk's edge kinds and evidence policy, and the query it runs for.
#[derive(Clone)]
pub struct Gate {
    filter: EdgeFilter,
    query: Arc<Operation>,
}

impl Default for Gate {
    fn default() -> Self {
        Gate::new(EdgeFilter::default())
    }
}

impl Gate {
    /// The gate of a walk following `filter`, for the query this thread runs (see [`kc_store::limits::current`]).
    pub fn new(filter: EdgeFilter) -> Gate {
        Gate { filter, query: kc_store::limits::current() }
    }

    /// The walk's edge filter, with its evidence policy.
    pub fn filter(&self) -> &EdgeFilter {
        &self.filter
    }

    /// Whether the walk lists and follows edges of this kind.
    pub fn allows(&self, edge: &str) -> bool {
        self.filter.allows(edge)
    }

    /// What the walk does with an edge of `confidence` resting on `evidence`; an excluded edge is counted for the
    /// answer's `policy`, once per `key` (what tells the edge apart: its ends and site).
    pub fn judge(&self, key: impl Hash, confidence: &str, evidence: Option<&Evidence>) -> Verdict {
        self.filter.judge(key, confidence, evidence)
    }

    /// Whether the policy leaves such an edge out, without counting it: for looking past where a walk stopped.
    pub fn leaves_out(&self, confidence: &str, evidence: Option<&Evidence>) -> bool {
        self.filter.policy().judge(confidence, evidence) == Verdict::Exclude
    }

    /// Whether the walk goes past an edge it listed.
    pub fn follows(&self, confidence: &str, evidence: Option<&Evidence>) -> bool {
        self.filter.follows(confidence, evidence)
    }

    /// Before reading a node's edges: an error once the query is cancelled, so its walk stops.
    pub fn expand(&self) -> Result<()> {
        self.query.check()
    }
}

/// Confidence of an edge across a link, for `path` and `flow`: an ambiguous link makes the edge ambiguous; a
/// `dominant` (or `unique_name`) link from exact code is `unique_name`, whose evidence the policy judges.
pub fn across(confidence: &'static str, link: &'static str) -> &'static str {
    match link {
        "ambiguous" => "ambiguous",
        "dominant" | "unique_name" if confidence == "exact" => "unique_name",
        _ => confidence,
    }
}

/// Code reached across a crossing: the symbol, where (the call, request or publish), and how sure the hop is.
pub struct Arrival {
    /// The symbol, the confidence it was found at in its own repo, and the site.
    pub creator: trace::Creator,
    /// The hop's confidence across the link (see [`across`]).
    pub confidence: &'static str,
    /// What the hop rests on: the link's evidence.
    pub evidence: Option<Evidence>,
}

impl Arrival {
    pub fn sym(&self) -> &SymbolRow {
        &self.creator.0
    }

    pub fn site(&self) -> &Option<String> {
        &self.creator.2
    }
}

/// A hop across repos from one symbol, into the repo at `root`, of an edge kind the walk allows.
pub struct Crossing {
    pub root: PathBuf,
    pub via: Via,
    /// For the bus from a publisher, the publish call (`file:line` in the publisher's repo): where the hop leaves.
    pub publish: Option<String>,
    pub arrivals: Vec<Arrival>,
}

impl Crossing {
    /// The crossing for code reached across `via` into `root`: each of `creators` at its confidence across the link.
    pub fn new(root: PathBuf, via: Via, publish: Option<String>, creators: Vec<trace::Creator>) -> Crossing {
        let arrivals = creators
            .into_iter()
            .map(|creator| {
                let confidence = across(creator.1, via.link());
                let evidence = via.evidence(confidence);
                Arrival { creator, confidence, evidence }
            })
            .collect();
        Crossing { root, via, publish, arrivals }
    }

    pub fn edge(&self) -> &'static str {
        self.via.edge()
    }

    /// What the walk does with arrival `a`, reached from symbol `id` of the repo at `from` (see [`Gate::judge`]).
    pub fn judge(&self, gate: &Gate, from: &Path, id: i64, a: &Arrival) -> Verdict {
        gate.judge((from, id, self.root.as_path(), a.sym().id, self.edge(), a.site()), a.confidence, a.evidence.as_ref())
    }
}
