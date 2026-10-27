//! Links Ansible content across repos: roles to the repos defining them, and what roles and playbooks deploy to the
//! application repos, message-bus hosts and SQL schemas the workspace knows.
//!
//! - A role reference a repo doesn't resolve itself goes through the `requirements.yml` (or meta dependency) giving
//!   the role's `src`: the indexed repo whose `origin` is that URL is `exact`; one whose directory is named like the
//!   URL's last segment is `unique_name`. Without a requirement, the one repo defining a role of that name (its
//!   directory, or its meta `role_name`) is `unique_name`; several are `ambiguous`. Galaxy roles (`ns.role`) stay
//!   unlinked.
//! - What a task deploys (see `kc_extract::ansible`) is read as written, with `{{ var }}` replaced by the variable's
//!   literal value when the role or file defines it once; a value still holding Jinja is left unlinked. A git `repo`
//!   URL links to the repo with that origin (`exact`). A service, image or app path name links to an application repo
//!   (one with manifests or HTTP routes) whose directory name, package name or registered service name is that name,
//!   compared without case, punctuation or a configured prefix (`[conventions.ansible] name_prefixes`; `unique_name`,
//!   `ambiguous` when several repos match). Generic infrastructure names (`consul`, `nginx`, `mysql`, …, and the
//!   configured `infra_names`) never link. A RabbitMQ vhost links to the repos
//!   whose message types declare that host; a database to the repos defining SQL objects in that schema.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use kc_store::ansible::{FactRow, RoleRow, RoleUse};
use serde::Serialize;

use crate::workspace::{RepoSummary, normalize_remote};

/// A role reference linked to the repo (and role) defining it.
#[derive(Debug, Clone, Serialize)]
pub struct RoleLink {
    /// The repo defining the role.
    pub repo: PathBuf,
    pub role: RoleRow,
    /// `exact` (the requirement's `src` is the repo's origin), `unique_name` or `ambiguous`.
    pub confidence: &'static str,
    /// The requirement's `src`, when one names the role.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub src: Option<String>,
}

/// What a task deploys or configures, and what it links to.
#[derive(Debug, Clone, Serialize)]
pub struct Deploy {
    /// What the task does to it: `deploys`, `undeploys`, `restarts` or `stops` (see
    /// [`kc_extract::ansible::TASK_ACTIONS`]).
    pub action: String,
    /// `service`, `image`, `repo`, `vhost`, `queue`, `exchange`, `database` or `url`.
    pub what: String,
    /// As written.
    pub value: String,
    /// With variables read, when that gave a literal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved: Option<String>,
    pub file: String,
    pub line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub links: Vec<DeployLink>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeployLink {
    pub repo: PathBuf,
    /// `repo` (an application repo), `bus` (its message types declare the vhost), `sql` (it defines the schema).
    pub how: &'static str,
    /// What matched: the repo's name, package or service; the message type; the SQL object.
    pub matched: String,
    pub confidence: &'static str,
}

/// Infrastructure names that a service or image never links to an application repo by, besides the configured
/// `infra_names`.
const GENERIC: &[&str] = &[
    "consul", "consultemplate", "telegraf", "nginx", "apache2", "httpd", "haproxy", "keepalived", "mysql", "mariadb",
    "redis", "redisserver", "redissentinel", "rabbitmq", "rabbitmqserver", "docker", "containerd", "sshd", "ssh",
    "ntp", "chronyd", "filebeat", "logstash", "kibana", "elasticsearch", "influxdb", "grafana", "grafanaserver",
    "jenkins", "gitlab", "gitlabrunner", "vault", "networkmanager", "networking", "sssd", "systemdresolved", "cron",
    "ufw", "iptables", "mongod", "mongodb", "cassandra", "node", "nodejs", "dotnet", "w3svc", "iis", "winrm", "apps",
    "app", "web", "api", "service", "server", "data", "logs", "admin",
];

/// Whether a fact kind records what a task does to something it acts on (`deploys`, `undeploys`, …).
pub fn is_task_action(kind: &str) -> bool {
    kc_extract::ansible::TASK_ACTIONS.contains(&kind)
}

/// A name as compared: lower case, letters and digits only, without the first configured prefix it starts with
/// (`name_prefixes`) when at least three characters are left.
fn key(name: &str, prefixes: &[String]) -> String {
    let k = crate::config::name_key(name);
    prefixes.iter().find_map(|p| k.strip_prefix(p.as_str()).filter(|r| r.len() >= 3)).map_or(k.clone(), str::to_string)
}

fn dir_name(root: &Path) -> String {
    root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

/// The workspace's Ansible facts, indexed for linking. Build once per answer. A repo whose checkout is gone links
/// nothing and is linked to by nothing; an answer it would have been linked in names it as missing.
pub struct Ansible<'a> {
    repos: Vec<&'a RepoSummary>,
    /// Application repos by name key: (repo, the name matched).
    apps: HashMap<String, Vec<(&'a Path, String)>>,
    by_origin: HashMap<String, &'a RepoSummary>,
    /// The repos left out because their checkout is gone (see [`crate::workspace::gone`]), indexed the same way: what
    /// a link would have found among them is noted as missing. `None` while none is gone.
    gone: Option<Box<Ansible<'a>>>,
    /// The configured name conventions.
    names: kc_extract::conventions::Ansible,
}

impl<'a> Ansible<'a> {
    pub fn new(summaries: &'a [RepoSummary]) -> Ansible<'a> {
        let gone = crate::workspace::gone(summaries);
        let mut live = Self::over(crate::http::distinct(summaries));
        if !gone.is_empty() {
            live.gone = Some(Box::new(Self::over(gone)));
        }
        live
    }

    fn over(repos: Vec<&'a RepoSummary>) -> Ansible<'a> {
        let naming = crate::config::conventions_now().ansible.clone();
        let mut apps: HashMap<String, Vec<(&Path, String)>> = HashMap::new();
        let mut by_origin = HashMap::new();
        for s in &repos {
            if let Some(o) = &s.origin {
                by_origin.insert(o.clone(), *s);
            }
            let app = !s.packages.is_empty() || s.routes.iter().any(|r| r.side == "server");
            if !app {
                continue;
            }
            let mut names = vec![dir_name(&s.root)];
            names.extend(s.packages.iter().map(|p| p.name.clone()));
            names.extend(s.routes.iter().filter(|r| r.side == "service").filter_map(|r| r.service.clone()));
            for n in names {
                let k = key(&n, &naming.name_prefixes);
                if k.len() < 4 || GENERIC.contains(&k.as_str()) || naming.infra_names.contains(&k) {
                    continue;
                }
                let e = apps.entry(k).or_default();
                if !e.iter().any(|(r, _)| *r == s.root.as_path()) {
                    e.push((s.root.as_path(), n));
                }
            }
        }
        Ansible { repos, apps, by_origin, gone: None, names: naming }
    }

    /// Notes each repo of `links` as missing: links found among gone repos.
    fn note<T>(links: &[T], repo: impl Fn(&T) -> &Path) {
        for l in links {
            crate::workspace::note_missing(repo(l));
        }
    }

    fn summary(&self, root: &Path) -> Option<&'a RepoSummary> {
        self.repos.iter().copied().find(|s| s.root == root)
    }

    /// The root role of a repo, or its only role.
    fn main_role(s: &RepoSummary) -> Option<&RoleRow> {
        s.ansible.roles.iter().find(|r| r.dir.is_empty()).or_else(|| (s.ansible.roles.len() == 1).then(|| &s.ansible.roles[0]))
    }

    /// The `src` a requirement in `s` gives role `name` for `file`: from the `requirements.yml` (or meta dependency)
    /// governing it, in its directory or one above (a `roles/` directory's counts for the one holding it), nearest
    /// first. A requirement in a sibling directory installs roles for other playbooks, not this one.
    fn requirement<'s>(s: &'s RepoSummary, name: &str, file: &str) -> Option<&'s FactRow> {
        let here = dir_of(file);
        s.ansible
            .facts
            .iter()
            .filter(|f| f.kind == "requirement" && f.name == name)
            .filter_map(|f| {
                let d = dir_of(&f.file);
                let d = d.strip_suffix("/roles").or_else(|| (d == "roles").then_some("")).unwrap_or(d);
                let governs = here.starts_with(d) && (d.is_empty() || here.len() == d.len() || here[d.len()..].starts_with('/'));
                governs.then_some((d.len(), f))
            })
            .max_by_key(|(len, _)| *len)
            .map(|(_, f)| f)
    }

    /// The repos and roles a role reference (`name`, in `file` of the repo at `root`) links to.
    pub fn role(&self, root: &Path, name: &str, file: &str) -> Vec<RoleLink> {
        let Some(me) = self.summary(root) else { return Vec::new() };
        let links = self.role_of(me, name, file);
        // Unless the requirement names a repo by its origin, a gone repo defining the role would have been a candidate.
        if let Some(gone) = &self.gone
            && !links.iter().any(|l| l.confidence == "exact")
        {
            Self::note(&gone.role_of(me, name, file), |l| &l.repo);
        }
        links
    }

    /// [`Ansible::role`] for a reference in `me`, among this index's repos.
    fn role_of(&self, me: &RepoSummary, name: &str, file: &str) -> Vec<RoleLink> {
        let root = me.root.as_path();
        if let Some(req) = Self::requirement(me, name, file) {
            if let Some(o) = normalize_remote(&req.value)
                && let Some(s) = self.by_origin.get(&o)
                && let Some(r) = Self::main_role(s)
            {
                return vec![RoleLink { repo: s.root.clone(), role: r.clone(), confidence: "exact", src: Some(req.value.clone()) }];
            }
            let last = req.value.trim_end_matches('/').rsplit(['/', ':']).next().unwrap_or("").trim_end_matches(".git").to_lowercase();
            let named: Vec<&RepoSummary> = self.repos.iter().copied().filter(|s| dir_name(&s.root).to_lowercase() == last).collect();
            if let [s] = named.as_slice()
                && let Some(r) = Self::main_role(s)
            {
                return vec![RoleLink { repo: s.root.clone(), role: r.clone(), confidence: "unique_name", src: Some(req.value.clone()) }];
            }
            return Vec::new();
        }
        if name.contains('.') && !self.repos.iter().any(|s| dir_name(&s.root) == name) {
            return Vec::new(); // a Galaxy role
        }
        let mut found = Vec::new();
        for s in &self.repos {
            if s.root == root {
                continue;
            }
            let alias = |r: &RoleRow| {
                s.ansible.facts.iter().any(|f| f.kind == "role_name" && f.name == name && dir_of(dir_of(&f.file)) == r.dir)
            };
            for r in &s.ansible.roles {
                if r.name == name || alias(r) {
                    found.push((s.root.clone(), r.clone()));
                }
            }
        }
        let confidence = if found.len() == 1 { "unique_name" } else { "ambiguous" };
        found.into_iter().map(|(repo, role)| RoleLink { repo, role, confidence, src: None }).collect()
    }

    /// The role references in other repos that link to a role of the repo at `root` (the role in `dir`, or any).
    pub fn role_users(&self, root: &Path, dir: Option<&str>) -> Vec<(PathBuf, RoleUse, &'static str)> {
        // A gone repo referencing the role would have been a user.
        if let Some(gone) = &self.gone {
            crate::workspace::note_missing_if(gone.repos.iter().copied(), |g| {
                g.ansible.uses.iter().any(|u| self.role_of(g, &u.name, &u.file).iter().any(|l| l.repo == root && dir.is_none_or(|d| l.role.dir == d)))
            });
        }
        let mut out = Vec::new();
        for s in &self.repos {
            if s.root == root {
                continue;
            }
            for u in &s.ansible.uses {
                let links = self.role_of(s, &u.name, &u.file);
                if !links.iter().any(|l| l.repo == root && dir.is_none_or(|d| l.role.dir == d)) {
                    continue;
                }
                // A use linking here: a gone repo defining the role too would have made it less sure.
                let links = self.role(&s.root, &u.name, &u.file);
                for l in links {
                    if l.repo == root && dir.is_none_or(|d| l.role.dir == d) {
                        out.push((s.root.clone(), u.clone(), l.confidence));
                    }
                }
            }
        }
        out
    }

    /// A deployed value with its variables read: the literal a variable of the same role (or file, or repo) holds.
    fn read(&self, s: &RepoSummary, f: &FactRow) -> Option<String> {
        let mut out = f.value.clone();
        for _ in 0..3 {
            let Some(start) = out.find("{{") else { break };
            let end = out[start..].find("}}")? + start + 2;
            let inner = out[start + 2..end - 2].trim();
            let name = inner.split('|').next().unwrap_or("").trim();
            if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                return None;
            }
            let lits: Vec<&(String, String, String)> = s.ansible.literals.iter().filter(|(n, ..)| n == name).collect();
            let near = |l: &&&(String, String, String)| match &f.role {
                Some(_) => kc_extract::ansible::place(&l.2).and_then(|p| p.role_dir) == kc_extract::ansible::place(&f.file).and_then(|p| p.role_dir),
                None => l.2 == f.file || dir_of(&l.2).starts_with(dir_of(&f.file)),
            };
            let scoped: Vec<&String> = lits.iter().filter(near).map(|l| &l.1).collect();
            let pick = if scoped.is_empty() { lits.iter().map(|l| &l.1).collect() } else { scoped };
            let mut values = pick.clone();
            values.sort();
            values.dedup();
            let [v] = values.as_slice() else { return None };
            out.replace_range(start..end, v);
        }
        (!out.contains("{{")).then_some(out)
    }

    /// What the repo's Ansible deploys, each with its links, limited to facts `keep` accepts.
    pub fn deploys(&self, root: &Path, keep: impl Fn(&FactRow) -> bool) -> Vec<Deploy> {
        let Some(s) = self.summary(root) else { return Vec::new() };
        let mut out = Vec::new();
        for f in s.ansible.facts.iter().filter(|f| is_task_action(&f.kind) && keep(f)) {
            let literal = !f.value.contains("{{");
            let resolved = if literal { None } else { self.read(s, f) };
            let value = resolved.clone().unwrap_or_else(|| f.value.clone());
            let links = if value.contains("{{") { Vec::new() } else { self.links(root, &f.name, &value) };
            out.push(Deploy {
                action: f.kind.clone(),
                what: f.name.clone(),
                value: f.value.clone(),
                resolved,
                file: f.file.clone(),
                line: f.line,
                symbol: f.symbol.clone(),
                role: f.role.clone(),
                links,
            });
        }
        out
    }

    /// What a deployed value (`what`, a literal) links to, from the repo at `from`.
    pub(crate) fn links_of(&self, from: &Path, what: &str, value: &str) -> Vec<DeployLink> {
        self.links(from, what, value)
    }

    fn links(&self, from: &Path, what: &str, value: &str) -> Vec<DeployLink> {
        let links = self.links_among(from, what, value);
        // Unless a `repo` URL names a repo by its origin, a gone repo it would have linked to is missing.
        if let Some(gone) = &self.gone
            && !(what == "repo" && !links.is_empty())
        {
            Self::note(&gone.links_among(from, what, value), |l| &l.repo);
        }
        links
    }

    /// [`Ansible::links`] among this index's repos.
    fn links_among(&self, from: &Path, what: &str, value: &str) -> Vec<DeployLink> {
        let mut out = Vec::new();
        match what {
            "repo" => {
                if let Some(o) = normalize_remote(value)
                    && let Some(s) = self.by_origin.get(&o)
                {
                    out.push(DeployLink { repo: s.root.clone(), how: "repo", matched: dir_name(&s.root), confidence: "exact" });
                }
            }
            "service" | "image" => {
                // `registry/group/name:tag` → `name`; `name@host` → `name`.
                let name = value.rsplit('/').next().unwrap_or(value);
                let name = name.split([':', '@']).next().unwrap_or(name);
                let k = key(name, &self.names.name_prefixes);
                if let Some(hits) = self.apps.get(&k) {
                    let hits: Vec<_> = hits.iter().filter(|(r, _)| *r != from).collect();
                    let confidence = if hits.len() == 1 { "unique_name" } else { "ambiguous" };
                    for (r, n) in hits {
                        out.push(DeployLink { repo: r.to_path_buf(), how: "repo", matched: n.clone(), confidence });
                    }
                }
            }
            "vhost" => {
                let v = value.trim_start_matches('/');
                if v.is_empty() {
                    return out;
                }
                for s in &self.repos {
                    let types: Vec<&kc_store::bus::BusRow> = s
                        .bus
                        .iter()
                        .filter(|b| b.kind == "message" && b.queue.as_deref().is_some_and(|q| q.trim_start_matches('/').eq_ignore_ascii_case(v)))
                        .collect();
                    if let Some(b) = types.first() {
                        let matched = if types.len() == 1 { b.symbol.clone() } else { format!("{} and {} more", b.symbol, types.len() - 1) };
                        out.push(DeployLink { repo: s.root.clone(), how: "bus", matched, confidence: "exact" });
                    }
                }
            }
            "database" => {
                for s in &self.repos {
                    let defs: Vec<&kc_store::sql::SqlDef> =
                        s.sql.iter().filter(|d| d.schema.as_deref().is_some_and(|sc| sc.eq_ignore_ascii_case(value))).collect();
                    if let Some(d) = defs.first() {
                        out.push(DeployLink { repo: s.root.clone(), how: "sql", matched: format!("{} ({} objects)", d.schema.clone().unwrap_or_default(), defs.len()), confidence: "exact" });
                    }
                }
            }
            _ => {}
        }
        out
    }

    /// Every deploy fact in the workspace linking to the repo at `root`, with the repo it is in.
    pub fn deployed_by(&self, root: &Path) -> Vec<(PathBuf, Deploy)> {
        let mut out = Vec::new();
        for s in &self.repos {
            if !s.ansible.facts.iter().any(|f| is_task_action(&f.kind)) {
                continue;
            }
            for d in self.deploys(&s.root, |_| true) {
                if d.links.iter().any(|l| l.repo == root) {
                    out.push((s.root.clone(), d));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(root: &str, origin: &str, ansible: serde_json::Value, packages: &[&str]) -> RepoSummary {
        let packages: Vec<serde_json::Value> =
            packages.iter().map(|p| serde_json::json!({"kind": "csproj", "name": p, "file": "x.csproj", "line": 1, "targets": [], "deps": []})).collect();
        serde_json::from_value(serde_json::json!({
            "root": root, "packages": packages, "modules": [], "origin": origin, "ansible": ansible, "stamp": "",
        }))
        .unwrap()
    }

    fn fact(kind: &str, name: &str, value: &str, file: &str) -> serde_json::Value {
        serde_json::json!({"kind": kind, "name": name, "value": value, "file": file, "line": 3})
    }

    #[test]
    fn links_roles_through_requirements_and_deploys_to_apps() {
        let role = repo(
            "/w/Configure.Orders",
            "git.example.com/roles/configure.orders",
            serde_json::json!({
                "roles": [{"name": "Configure.Orders", "dir": "", "file": "tasks/main.yml", "line": 1}],
                "facts": [
                    fact("deploys", "service", "{{ svc }}", "tasks/main.yml"),
                    fact("deploys", "service", "consul", "tasks/main.yml"),
                    fact("deploys", "image", "registry.example.com/apps/orders-api:1.2", "tasks/main.yml"),
                ],
                "literals": [["svc", "Acme.OrdersApi", "defaults/main.yml"]],
            }),
            &[],
        );
        let play = repo(
            "/w/plays",
            "git.example.com/ops/plays",
            serde_json::json!({
                "facts": [fact("requirement", "orders", "http://git.example.com/Roles/Configure.Orders.git", "db/requirements.yml")],
                "uses": [{"name": "orders", "file": "db/site.yml", "line": 4, "from": "site.yml.Orders", "from_kind": "play"}],
            }),
            &[],
        );
        let app = repo("/w/Acme.OrdersApi", "git.example.com/svc/acme.ordersapi", serde_json::json!({}), &["Acme.OrdersApi"]);
        let other = repo("/w/OrdersApi", "git.example.com/svc/ordersapi", serde_json::json!({}), &["Orders.Api.Client"]);
        let all = vec![role, play, app, other];
        // A code base whose repos are named with a prefix compares names without it.
        let prefixed = kc_extract::conventions::Conventions {
            ansible: kc_extract::conventions::Ansible { name_prefixes: vec!["acme".into()], ..Default::default() },
            ..Default::default()
        };
        let plain = kc_extract::conventions::scoped(Default::default(), || Ansible::new(&all).deploys(Path::new("/w/Configure.Orders"), |_| true));
        let only: Vec<&str> = plain[0].links.iter().map(|l| l.confidence).collect();
        assert_eq!(only, ["unique_name"], "without the prefix configured, the name is one repo's");
        kc_extract::conventions::scoped(prefixed, || {
            let a = Ansible::new(&all);
            let links = a.role(Path::new("/w/plays"), "orders", "db/site.yml");
            assert_eq!(links.len(), 1);
            assert_eq!((links[0].repo.to_str(), links[0].confidence), (Some("/w/Configure.Orders"), "exact"));
            let users = a.role_users(Path::new("/w/Configure.Orders"), Some(""));
            assert_eq!(users.len(), 1);

            let d = a.deploys(Path::new("/w/Configure.Orders"), |_| true);
            assert_eq!(d[0].resolved.as_deref(), Some("Acme.OrdersApi"));
            // `Acme.OrdersApi` and `OrdersApi` compare equal without the prefix: two repos, ambiguous.
            let repos: Vec<(&str, &str)> = d[0].links.iter().map(|l| (l.repo.to_str().unwrap(), l.confidence)).collect();
            assert_eq!(repos, [("/w/Acme.OrdersApi", "ambiguous"), ("/w/OrdersApi", "ambiguous")]);
            assert!(d[1].links.is_empty(), "infrastructure names never link");
            assert_eq!(d[2].links.len(), 2, "an image named orders-api is either app");
            assert_eq!(a.deployed_by(Path::new("/w/Acme.OrdersApi")).len(), 2);
        });
    }
}
