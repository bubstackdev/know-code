//! Mermaid diagrams of walk and area answers (`format: mermaid`), drawn from the answer as it is sent: the diagram
//! shows exactly the answer's nodes and edges, no more, so parsing its edges back gives the answer's edges.
//!
//! - `flow`: the start and every step, each step linked from the step it is reached from;
//! - `path`: every path found, each a chain from `from` to `to` (an edge two paths share is drawn once);
//! - `areas`: each area, linked to the areas it depends on as the answer lists them.
//!
//! A walk's node shows its symbol and `file:line`, grouped by repo when the answer crosses repos; its edge shows the
//! edge kind, what the edge carries (an HTTP verb and path, a bus message, an NgRx action) and its confidence in
//! brackets. Ambiguous edges are dashed. An area's edge shows how many file edges it stands for.

use std::collections::{BTreeMap, HashMap};

use serde_json::Value;

/// One node: its Mermaid id, its repo (empty for areas) and its label's lines.
struct Node {
    id: String,
    repo: String,
    lines: Vec<String>,
}

/// One edge between node ids, with its label and whether it is drawn dashed.
struct Edge {
    from: String,
    to: String,
    label: String,
    dashed: bool,
}

#[derive(Default)]
struct Diagram {
    /// What the answer says it left out or cut, as Mermaid comments: a diagram never looks whole when its answer
    /// isn't.
    notes: Vec<String>,
    nodes: Vec<Node>,
    by_key: HashMap<String, String>,
    edges: Vec<Edge>,
}

impl Diagram {
    /// The id of the node `key` names, added with `repo` and `lines` the first time.
    fn node(&mut self, key: String, repo: &str, lines: Vec<String>) -> String {
        if let Some(id) = self.by_key.get(&key) {
            return id.clone();
        }
        let id = format!("n{}", self.nodes.len());
        self.nodes.push(Node { id: id.clone(), repo: repo.to_string(), lines });
        self.by_key.insert(key, id.clone());
        id
    }

    fn edge(&mut self, from: String, to: String, label: String, dashed: bool) {
        if !self.edges.iter().any(|e| e.from == from && e.to == to && e.label == label) {
            self.edges.push(Edge { from, to, label, dashed });
        }
    }

    fn render(&self, direction: &str) -> String {
        let mut out = format!("flowchart {direction}\n");
        for n in &self.notes {
            out.push_str(&format!("  %% {n}\n"));
        }
        let mut repos: Vec<&str> = Vec::new();
        for n in &self.nodes {
            if !repos.contains(&n.repo.as_str()) {
                repos.push(&n.repo);
            }
        }
        let grouped = repos.len() > 1;
        for (k, repo) in repos.iter().enumerate() {
            let indent = if grouped { "    " } else { "  " };
            if grouped {
                out.push_str(&format!("  subgraph r{k}[\"{}\"]\n", escape(repo)));
            }
            for n in self.nodes.iter().filter(|n| n.repo == *repo) {
                let label: Vec<String> = n.lines.iter().map(|l| escape(l)).collect();
                out.push_str(&format!("{indent}{}[\"{}\"]\n", n.id, label.join("<br/>")));
            }
            if grouped {
                out.push_str("  end\n");
            }
        }
        for e in &self.edges {
            let arrow = if e.dashed { "-.->" } else { "-->" };
            out.push_str(&format!("  {} {arrow}|\"{}\"| {}\n", e.from, escape(&e.label), e.to));
        }
        out
    }
}

/// Text in a quoted Mermaid label: quotes and angle brackets as entities, so a generic type (`List<T>`) isn't read
/// as markup and a quote doesn't end the label.
fn escape(s: &str) -> String {
    s.replace('#', "#35;").replace('"', "#quot;").replace('<', "#lt;").replace('>', "#gt;")
}

#[cfg(test)]
fn unescape(s: &str) -> String {
    s.replace("#quot;", "\"").replace("#lt;", "<").replace("#gt;", ">").replace("#35;", "#")
}

fn str_of<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or_default()
}

/// A walk step's node: its symbol and where it is defined.
fn step_node(d: &mut Diagram, s: &Value, repo_default: &str) -> String {
    let repo = s["repo"].as_str().unwrap_or(repo_default);
    let place = format!("{}:{}", str_of(s, "file"), s["line"].as_u64().unwrap_or_default());
    let key = format!("{repo}\u{0}{}\u{0}{place}", str_of(s, "qualified"));
    d.node(key, repo, vec![format!("{} {}", str_of(s, "kind"), str_of(s, "qualified")), place])
}

/// A walk edge's label: its kind, what it carries, and its confidence.
fn edge_label(s: &Value) -> String {
    let mut label = str_of(s, "edge").to_string();
    if let Some(h) = s.get("http").filter(|h| h.is_object()) {
        label.push_str(&format!(" {} /{}", str_of(h, "verb"), str_of(h, "path")));
    }
    if let Some(b) = s.get("bus").filter(|b| b.is_object()) {
        label.push_str(&format!(" {}", str_of(b, "message")));
    }
    if let Some(a) = s["ngrx"].as_str() {
        label.push_str(&format!(" {a}"));
    }
    label.push_str(&format!(" [{}]", str_of(s, "confidence")));
    label
}

/// Comments for what an item says it left out (`left_out`, by reason) or cut (`truncated`).
fn notes(item: &Value) -> Vec<String> {
    let mut out = Vec::new();
    for (reason, n) in item["left_out"].as_object().into_iter().flatten() {
        out.push(format!("left out: {n} {reason} edges"));
    }
    if item["truncated"] == Value::Bool(true) {
        out.push("cut short: the answer was truncated".to_string());
    }
    if let Some(n) = item["paths_total"].as_u64() {
        out.push(format!("{n} paths found; those drawn are the answer's"));
    }
    out
}

fn flow(item: &Value) -> Diagram {
    let mut d = Diagram { notes: notes(item), ..Default::default() };
    let repo = str_of(&item["start"], "repo").to_string();
    let start = step_node(&mut d, &item["start"], &repo);
    let mut ids: BTreeMap<u64, String> = BTreeMap::new();
    let steps = item["steps"].as_array().cloned().unwrap_or_default();
    for s in &steps {
        let id = step_node(&mut d, s, &repo);
        ids.insert(s["index"].as_u64().unwrap_or_default(), id);
    }
    for s in &steps {
        let to = ids[&s["index"].as_u64().unwrap_or_default()].clone();
        let from = s["parent"].as_u64().and_then(|p| ids.get(&p).cloned()).unwrap_or_else(|| start.clone());
        d.edge(from, to, edge_label(s), str_of(s, "confidence") == "ambiguous");
    }
    d
}

fn path(item: &Value) -> Diagram {
    let mut d = Diagram { notes: notes(item), ..Default::default() };
    let repo = str_of(&item["from"], "repo").to_string();
    for p in item["paths"].as_array().into_iter().flatten() {
        let steps = p["steps"].as_array().cloned().unwrap_or_default();
        let mut prev: Option<String> = None;
        for s in &steps {
            let id = step_node(&mut d, s, &repo);
            if let Some(from) = prev {
                d.edge(from, id.clone(), edge_label(s), str_of(s, "confidence") == "ambiguous");
            }
            prev = Some(id);
        }
    }
    // With no path found, the two ends alone.
    if d.nodes.is_empty() {
        step_node(&mut d, &item["from"], &repo);
        step_node(&mut d, &item["to"], &repo);
    }
    d
}

fn areas(item: &Value) -> Diagram {
    let mut d = Diagram::default();
    let list = item["areas"].as_array().cloned().unwrap_or_default();
    let label = |a: &Value| vec![format!("#{} {}", a["id"], str_of(a, "name")), format!("{} files", a["files"])];
    let mut ids = HashMap::new();
    for a in &list {
        let id = d.node(format!("area {}", a["id"]), "", label(a));
        ids.insert(a["id"].as_u64().unwrap_or_default(), id);
    }
    for a in &list {
        let from = ids[&a["id"].as_u64().unwrap_or_default()].clone();
        for dep in a["depends_on"].as_array().into_iter().flatten() {
            let n = dep["area"].as_u64().unwrap_or_default();
            // An area the answer names but doesn't list (one picked alone): a node of its own, from what it says.
            let to = match ids.get(&n) {
                Some(id) => id.clone(),
                None => {
                    let id = d.node(format!("area {n}"), "", vec![format!("#{n} {}", str_of(dep, "name"))]);
                    ids.insert(n, id.clone());
                    id
                }
            };
            d.edge(from.clone(), to, format!("{} file edges", dep["file_edges"]), false);
        }
    }
    d
}

/// The Mermaid diagram of an answer of `op` (`flow`, `path` or `areas`), one per item (an item is a repo's areas, a
/// path search, a flow); `None` for another op or an answer with no item.
pub fn mermaid(op: &str, env: &Value) -> Option<String> {
    let items = env["items"].as_array().filter(|i| !i.is_empty())?;
    let (draw, direction): (fn(&Value) -> Diagram, &str) = match op {
        "flow" => (flow, "TD"),
        "path" => (path, "LR"),
        "areas" => (areas, "LR"),
        _ => return None,
    };
    let diagrams: Vec<String> = items.iter().map(|i| draw(i).render(direction)).collect();
    Some(diagrams.join("\n"))
}

/// `diagram` (one or more diagrams [`mermaid`] drew; none: an empty flowchart) with `lines` added as comments after
/// each diagram's first line, where Mermaid takes them: what the answer says about itself, its repo and whether it
/// is complete, kept in a text that stays a valid diagram.
pub fn commented(diagram: &str, lines: &[String]) -> String {
    let diagram = if diagram.trim().is_empty() { "flowchart LR\n" } else { diagram };
    let notes: String = lines.iter().filter(|l| !l.trim().is_empty()).map(|l| format!("  %% {}\n", l.replace("%%", "%"))).collect();
    let mut out = String::new();
    for part in diagram.split_inclusive("\n") {
        out.push_str(part);
        if part.starts_with("flowchart ") {
            out.push_str(&notes);
        }
    }
    out
}

/// One edge as a diagram shows it: its ends' labels and its own label.
#[cfg(test)]
pub type ParsedEdge = (String, String, String);

/// The edges of a diagram [`mermaid`] drew, read back from its text: each end as its label's lines joined by
/// ` | `, prefixed by its repo group when there is one, and the edge's label.
#[cfg(test)]
pub fn parse(text: &str) -> Vec<ParsedEdge> {
    let mut labels: HashMap<String, String> = HashMap::new();
    let mut edges = Vec::new();
    let mut group = String::new();
    for line in text.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("subgraph ") {
            let name = rest.split_once("[\"").map(|(_, n)| n.trim_end_matches("\"]")).unwrap_or_default();
            group = format!("{}: ", unescape(name));
        } else if line == "end" {
            group.clear();
        } else if let Some((id, label)) = line.split_once("[\"") {
            let text: Vec<String> = label.trim_end_matches("\"]").split("<br/>").map(unescape).collect();
            labels.insert(id.to_string(), format!("{group}{}", text.join(" | ")));
        } else if let Some((from, rest)) = line.split_once(" -->|\"").or_else(|| line.split_once(" -.->|\"")) {
            let (label, to) = rest.split_once("\"| ").unwrap_or_default();
            edges.push((labels[from].clone(), labels[to].clone(), unescape(label)));
        }
    }
    edges
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::Scope;
    use crate::ops::tests::{repo, test_cache};
    use crate::walk::PathOptions;
    use std::path::{Path, PathBuf};

    /// A walk answer's edges, read from its JSON: (from symbol and place, to symbol and place, edge kind, confidence),
    /// each place prefixed by its repo when the answer crosses repos.
    fn answer_edges(op: &str, env: &Value) -> Vec<(String, String, String, String)> {
        let mut out = Vec::new();
        for item in env["items"].as_array().into_iter().flatten() {
            let repo0 = |k: &str| item[k]["repo"].as_str().unwrap_or_default().to_string();
            let node = |s: &Value, r: &str| {
                let repo = s["repo"].as_str().unwrap_or(r);
                (repo.to_string(), format!("{} {} | {}:{}", str_of(s, "kind"), str_of(s, "qualified"), str_of(s, "file"), s["line"]))
            };
            let mut pairs: Vec<(&Value, &Value)> = Vec::new();
            let r;
            match op {
                "flow" => {
                    r = repo0("start");
                    let steps = item["steps"].as_array().unwrap();
                    for s in steps {
                        let parent = s["parent"].as_u64().map(|p| steps.iter().find(|x| x["index"].as_u64() == Some(p)).unwrap());
                        pairs.push((parent.unwrap_or(&item["start"]), s));
                    }
                }
                _ => {
                    r = repo0("from");
                    for p in item["paths"].as_array().into_iter().flatten() {
                        let steps = p["steps"].as_array().unwrap();
                        pairs.extend(steps.windows(2).map(|w| (&w[0], &w[1])));
                    }
                }
            }
            let multi = pairs.iter().flat_map(|(a, b)| [node(a, &r).0, node(b, &r).0]).collect::<std::collections::HashSet<_>>().len() > 1;
            let show = |(repo, label): (String, String)| if multi { format!("{repo}: {label}") } else { label };
            for (a, b) in pairs {
                let e = (show(node(a, &r)), show(node(b, &r)), str_of(b, "edge").to_string(), str_of(b, "confidence").to_string());
                if !out.contains(&e) {
                    out.push(e);
                }
            }
        }
        out
    }

    /// A drawn edge as (from, to, edge kind, confidence): the kind is the label's first word, the confidence its
    /// bracketed end.
    fn drawn_edges(text: &str) -> Vec<(String, String, String, String)> {
        parse(text)
            .into_iter()
            .map(|(a, b, label)| {
                let kind = label.split(' ').next().unwrap_or_default().to_string();
                let conf = label.rsplit_once('[').map(|(_, c)| c.trim_end_matches(']').to_string()).unwrap_or_default();
                (a, b, kind, conf)
            })
            .collect()
    }

    /// Every `file:line` a diagram's node shows exists: the file in its repo (the group it is drawn in, else the repo
    /// answered for), with at least that many lines.
    fn places_exist(text: &str, env: &Value) {
        let roots: Vec<PathBuf> = env["provenance"].as_array().into_iter().flatten().filter_map(|p| p["repo"].as_str()).map(PathBuf::from).collect();
        let answered = PathBuf::from(str_of(env, "repo"));
        for (a, b, _) in parse(text) {
            for end in [a, b] {
                let (repo, label) = end.split_once(": ").map(|(r, l)| (Some(r), l)).unwrap_or((None, end.as_str()));
                let Some((_, place)) = label.split_once(" | ") else { continue };
                let (file, line) = place.rsplit_once(':').unwrap();
                let root = match repo {
                    Some(n) => roots.iter().find(|r| r.file_name().is_some_and(|f| f == n)).unwrap(),
                    None => &answered,
                };
                let text = std::fs::read_to_string(root.join(file)).unwrap_or_else(|_| panic!("{file} in {}", root.display()));
                assert!(text.lines().count() >= line.parse::<usize>().unwrap(), "{place}");
            }
        }
    }

    #[test]
    fn diagrams_match_their_golden_file_and_their_answers_edges() {
        let cache = test_cache();
        let base = cache.dir("diagrams");
        let (web, api, schema) = crate::walk::tests::shop(&base, "dg");
        // Two functions of one name, either of which a call may mean: an ambiguous edge, drawn dashed.
        let amb = repo(&base, "amb", &[("main.py", "def go():\n    run()\n"), ("mail.py", "def run():\n    pass\n"), ("print.py", "def run():\n    pass\n")]);
        let scope = |r: &PathBuf| Scope { repos: vec![r.clone()], no_sync: false };
        let mut golden = String::new();
        let mut check = |title: String, op: &str, env: &Value| {
            let text = mermaid(op, env).unwrap();
            if op == "areas" {
                let want: Vec<(String, String, String)> = env["items"][0]["areas"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .flat_map(|a| {
                        a["depends_on"].as_array().unwrap().iter().map(move |d| {
                            (format!("#{} {} | {} files", a["id"], str_of(a, "name"), a["files"]), format!("#{} {}", d["area"], str_of(d, "name")), format!("{} file edges", d["file_edges"]))
                        })
                    })
                    .collect();
                let got: Vec<(String, String, String)> = parse(&text).into_iter().map(|(a, b, l)| (a, b.split(" | ").next().unwrap().to_string(), l)).collect();
                assert_eq!(got, want, "{title}");
            } else {
                assert_eq!(drawn_edges(&text), answer_edges(op, env), "{title}");
                places_exist(&text, env);
            }
            golden.push_str(&format!("=== {title}\n{text}\n"));
        };
        for policy in ["", "all"] {
            let edges = crate::ops::edge_filter(&[], policy).unwrap();
            for (root, entry) in [(&web, "OrderPage"), (&web, "OrderService.place"), (&amb, "go")] {
                let fo = crate::flow::FlowOptions { depth: 10, max_steps: 500, max_repos: 16, edges: edges.clone(), children: crate::flow::DEFAULT_CHILDREN };
                let fl = crate::flow::flow(entry, &fo, &scope(root)).unwrap();
                check(format!("flow {entry} [{policy}]"), "flow", &fl.env);
            }
            for (from, to, at, root, allow_ambiguous) in [
                ("OrderPage.submit", "orders_dg", &schema, &web, false),
                ("OrderPage", "OrderPlacedConsumer.Consume", &api, &web, false),
                ("go", "run", &amb, &amb, true),
            ] {
                let po = PathOptions { max_hops: 8, max_nodes: 5000, max_repos: 16, paths: 3, one_way: false, allow_ambiguous, edges: edges.clone(), from_skip: 0, to_skip: 0 };
                let p = crate::walk::path(from, to, Some(at), &po, &scope(root)).unwrap();
                check(format!("path {from} -> {to} [{policy}]"), "path", &p.env);
            }
        }
        for (name, root) in [("web", &web), ("api", &api)] {
            let a = crate::ops::areas(None, &scope(root)).unwrap();
            check(format!("areas {name}"), "areas", &a.env);
        }
        let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/diagrams.txt");
        if std::env::var_os("KNOWCODE_TEST_BLESS").is_some() {
            std::fs::write(&file, &golden).unwrap();
        }
        let want = std::fs::read_to_string(&file).unwrap_or_default();
        assert!(golden == want, "diagrams differ from tests/snapshots/diagrams.txt:\n{golden}");
        assert!(golden.contains("-.->"), "an ambiguous edge is drawn dashed:\n{golden}");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn comments_follow_each_diagrams_first_line() {
        let two = "flowchart TD\n  n0[\"a\"]\n\nflowchart TD\n  n0[\"b\"]\n";
        let out = commented(two, &["repo shop; complete".into(), "100%% sure".into()]);
        assert_eq!(out.matches("  %% repo shop; complete\n").count(), 2, "{out}");
        assert!(out.contains("%% 100% sure") && !out.contains("100%%"), "{out}");
        assert!(commented("", &["nothing found".into()]).starts_with("flowchart LR\n  %% nothing found\n"));
    }

    #[test]
    fn labels_escape_what_mermaid_would_read() {
        assert_eq!(escape("List<T> \"x\" #1"), "List#lt;T#gt; #quot;x#quot; #35;1");
        assert_eq!(unescape(&escape("List<T> \"x\" #quot;")), "List<T> \"x\" #quot;");
    }
}
