//! Query-time resolution of Ansible references (see `kc_extract::ansible`), within one repo.
//!
//! Ansible finds things by place, so references resolve through paths and scopes:
//!
//! - a role (`roles:`, `include_role`, a meta dependency) in a `roles/<name>/` directory next to the referring file or
//!   up the tree is `exact`; one elsewhere in the repo is `unique_name` (or `ambiguous`). A role a `requirements.yml`
//!   or a meta dependency names with a `src`, and a Galaxy role (`namespace.role`), live in another repo:
//!   `external_likely`, linked across repos by `kc_index::ansible`;
//! - an included task file or playbook, and a template, relative to the referring file's directory (and for a role
//!   file, to the role's `tasks/` or `templates/` directory; `{{ playbook_dir }}` and `{{ role_path }}` read as those
//!   directories) is `exact`; failing that, the only file of that name is `unique_name`;
//! - a handler by name or `listen` topic in the same role or the same file is `exact`;
//! - a variable defined in scope is `exact`: in the same file (a play's `vars:`, `set_fact`, `register`), in the same
//!   role (its defaults and vars), in a vars file the play loads, or in `group_vars`/`host_vars` beside the playbook or
//!   above it. Several definitions in scope are all targets, highest precedence first (Ansible's order: `set_fact` and
//!   `register`, role vars, vars files, play vars, host_vars, group_vars, role defaults); which one wins at run time
//!   also depends on the inventory and extra vars, which this doesn't model. A variable defined only out of scope
//!   (a role parameter a playbook passes, say) is `unique_name` or `ambiguous`; one defined nowhere is `unresolved`
//!   (extra vars, inventory, facts).

use std::collections::HashMap;

use anyhow::Result;
use rusqlite::Connection;

use crate::query::RefHit;

/// The reference kinds an Ansible extractor writes.
pub const REF_KINDS: &[&str] = &["role", "include", "notify", "var", "template"];

/// Whether a reference of `kind` is resolved here.
pub(crate) fn handles(kind: &str) -> bool {
    REF_KINDS.contains(&kind)
}

/// The reference kinds reaching an Ansible symbol of `kind`; `None` for other symbols.
pub fn ref_kinds(kind: &str) -> Option<&'static [&'static str]> {
    Some(match kind {
        "role" => &["role"],
        "playbook" | "tasks" => &["include"],
        "handler" => &["notify"],
        "variable" => &["var"],
        "template" => &["template"],
        _ => return None,
    })
}

/// Whether `kind` is an Ansible symbol kind (`template` is shared with CFML; tell them apart by language).
pub fn is_ansible_kind(kind: &str) -> bool {
    matches!(kind, "role" | "playbook" | "play" | "task" | "handler" | "tasks" | "variable")
}

/// The edge a trace shows for an Ansible reference of `kind`.
pub fn edge_of(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "role" | "include" => "includes",
        "notify" => "notifies",
        "var" => "uses",
        "template" => "templates",
        _ => return None,
    })
}

/// Higher wins: where a variable's definition comes from, by Ansible's precedence (simplified).
fn precedence(tags: &str) -> u8 {
    match tags {
        "set_fact" | "register" => 9,
        "role_vars" => 8,
        "vars_file" => 7,
        "play_vars" => 6,
        "host_vars" => 5,
        "group_vars" => 4,
        "role_defaults" => 1,
        _ => 3,
    }
}

#[derive(Debug, Clone)]
struct Def {
    id: i64,
    kind: String,
    name: String,
    file: String,
    tags: String,
    attrs: String,
}

/// The repo's Ansible symbols and facts, loaded when the first Ansible reference is resolved.
#[derive(Default)]
pub(crate) struct Resolver {
    loaded: bool,
    by_name: HashMap<String, Vec<Def>>,
    /// Handlers by `listen` topic.
    by_topic: HashMap<String, Vec<Def>>,
    /// File-level symbols (playbook, task file, role main, template) by path.
    by_path: HashMap<String, Vec<Def>>,
    /// Vars files each file loads (`vars_files`, `include_vars`), as written.
    vars_files: HashMap<String, Vec<String>>,
    /// Role names a `requirements.yml` or a meta dependency gives a `src`.
    required: Vec<String>,
    /// Each symbol's parent and kind, for finding the play a definition or use is in.
    parents: HashMap<i64, (Option<i64>, String)>,
}

fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

fn join(dir: &str, rel: &str) -> String {
    let mut parts: Vec<&str> = if dir.is_empty() { Vec::new() } else { dir.split('/').collect() };
    for p in rel.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    parts.join("/")
}

/// A path as written, with `{{ playbook_dir }}`/`{{ role_path }}` read as `base`; `None` if other Jinja remains.
fn normalize(written: &str, base: &str) -> Option<(String, bool)> {
    let w = written.trim().replace('\\', "/");
    for prefix in ["{{ playbook_dir }}", "{{playbook_dir}}", "{{ role_path }}", "{{role_path}}"] {
        if let Some(rest) = w.strip_prefix(prefix) {
            return (!rest.contains("{{")).then(|| (join(base, rest.trim_start_matches('/')), true));
        }
    }
    if w.contains("{{") {
        return None;
    }
    Some((w, false))
}

/// The role directory a file is in (`roles/x`, or `""` for a repo-root role).
fn role_dir(path: &str) -> Option<String> {
    kc_extract::ansible::place(path).and_then(|p| p.role_dir)
}

impl Resolver {
    fn load(&mut self, conn: &Connection) -> Result<()> {
        if self.loaded {
            return Ok(());
        }
        self.loaded = true;
        let mut stmt = conn.prepare(
            "SELECT s.id, s.kind, s.name, f.path, s.tags, s.attrs, s.parent_id FROM symbols s JOIN files f ON f.id = s.file_id
             WHERE f.lang = 'ansible' ORDER BY f.path, s.start_line",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                Def { id: r.get(0)?, kind: r.get(1)?, name: r.get(2)?, file: r.get(3)?, tags: r.get(4)?, attrs: r.get(5)? },
                r.get::<_, Option<i64>>(6)?,
            ))
        })?;
        for row in rows {
            let (d, parent) = row?;
            let top = parent.is_none();
            self.parents.insert(d.id, (parent, d.kind.clone()));
            if top && matches!(d.kind.as_str(), "playbook" | "tasks" | "role" | "template") && !d.tags.contains("meta") {
                self.by_path.entry(d.file.to_lowercase()).or_default().push(d.clone());
            }
            if d.kind == "handler" && !d.attrs.is_empty() {
                for topic in d.attrs.split(',') {
                    self.by_topic.entry(topic.to_string()).or_default().push(d.clone());
                }
            }
            self.by_name.entry(d.name.clone()).or_default().push(d);
        }
        let mut stmt = conn.prepare(
            "SELECT f.path, a.kind, a.name FROM ansible_facts a JOIN files f ON f.id = a.file_id WHERE a.kind IN ('vars_file', 'requirement')
             ORDER BY f.path, a.rowid",
        )?;
        let facts: Vec<(String, String, String)> = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
        for (file, kind, name) in facts {
            if kind == "vars_file" {
                self.vars_files.entry(file).or_default().push(name);
            } else {
                self.required.push(name);
            }
        }
        Ok(())
    }

    fn named(&self, name: &str, kind: &str) -> Vec<&Def> {
        self.by_name.get(name).map_or_else(Vec::new, |v| v.iter().filter(|d| d.kind == kind).collect())
    }

    pub(crate) fn resolve(&mut self, conn: &Connection, hit: &RefHit) -> Result<(&'static str, Vec<i64>)> {
        self.load(conn)?;
        Ok(match hit.kind.as_str() {
            "role" => self.role(hit),
            "include" | "template" => self.file(hit),
            "notify" => self.notify(hit),
            "var" => self.var(hit),
            _ => ("unresolved", Vec::new()),
        })
    }

    /// A role's symbols: its main tasks' first, then its meta's.
    fn role_ids(&self, dir: &str, name: &str) -> Vec<i64> {
        let mut defs: Vec<&Def> = self.named(name, "role").into_iter().filter(|d| role_dir(&d.file).as_deref() == Some(dir)).collect();
        defs.sort_by_key(|d| d.tags.contains("meta"));
        defs.iter().map(|d| d.id).collect()
    }

    fn role(&self, hit: &RefHit) -> (&'static str, Vec<i64>) {
        let name = hit.name.trim_end_matches('/');
        if name.contains("{{") {
            return ("unresolved", Vec::new());
        }
        // `roles/<name>` beside the referring file or up the tree, as Ansible looks next to the playbook. With
        // `tasks_from`, the task file it names there.
        let tasks_from = hit.receiver.as_deref();
        let mut dir = dir_of(&hit.file).to_string();
        loop {
            let role = join(&dir, &format!("roles/{name}"));
            if let Some(from) = tasks_from {
                let file = from.trim_end_matches(".yml").trim_end_matches(".yaml");
                for ext in ["yml", "yaml"] {
                    let path = format!("{role}/tasks/{file}.{ext}").to_lowercase();
                    if let Some(defs) = self.by_path.get(&path) {
                        return ("exact", defs.iter().map(|d| d.id).collect());
                    }
                }
            }
            let ids = self.role_ids(&role, name);
            if !ids.is_empty() {
                return ("exact", ids);
            }
            if dir.is_empty() {
                break;
            }
            dir = dir_of(&dir).to_string();
        }
        // The repo is the role: its own playbooks name it by the repo's name.
        let own = self.role_ids("", name);
        if !own.is_empty() {
            return ("exact", own);
        }
        if self.required.iter().any(|r| r == name) || name.contains('.') {
            return ("external_likely", Vec::new());
        }
        let mut dirs: Vec<String> = self.named(name, "role").iter().filter_map(|d| role_dir(&d.file)).collect();
        dirs.sort();
        dirs.dedup();
        match dirs.len() {
            0 => ("unresolved", Vec::new()),
            1 => ("unique_name", self.role_ids(&dirs[0], name)),
            _ => ("ambiguous", dirs.iter().flat_map(|d| self.role_ids(d, name)).collect()),
        }
    }

    /// An included task file or playbook, or a template, by path.
    fn file(&self, hit: &RefHit) -> (&'static str, Vec<i64>) {
        let template = hit.kind == "template";
        let want = |d: &Def| if template { d.kind == "template" } else { matches!(d.kind.as_str(), "playbook" | "tasks" | "role") };
        let written = hit.receiver.as_deref().unwrap_or(&hit.name);
        let here = dir_of(&hit.file);
        let role = role_dir(&hit.file);
        let base = role.clone().unwrap_or_else(|| here.to_string());
        if let Some((path, anchored)) = normalize(written, &base) {
            let mut tries = vec![];
            if anchored {
                tries.push(path.clone());
            } else {
                if let Some(r) = &role {
                    let sub = if template { "templates" } else { "tasks" };
                    tries.push(join(r, &format!("{sub}/{path}")));
                    tries.push(join(r, &path));
                }
                if template {
                    tries.push(join(here, &format!("templates/{path}")));
                    tries.push(join(dir_of(here), &format!("templates/{path}")));
                }
                tries.push(join(here, &path));
                tries.push(join(dir_of(here), &path));
            }
            for t in tries {
                if let Some(defs) = self.by_path.get(&t.to_lowercase()) {
                    let ids: Vec<i64> = defs.iter().filter(|d| want(d)).map(|d| d.id).collect();
                    if !ids.is_empty() {
                        return ("exact", ids);
                    }
                }
            }
        }
        let file = hit.name.as_str();
        let mut cands: Vec<&Def> =
            self.by_path.values().flatten().filter(|d| want(d) && d.file.rsplit('/').next().is_some_and(|f| f.eq_ignore_ascii_case(file))).collect();
        cands.sort_by(|a, b| a.file.cmp(&b.file));
        cands.dedup_by_key(|d| d.id);
        match cands.len() {
            0 => ("unresolved", Vec::new()),
            1 => ("unique_name", vec![cands[0].id]),
            _ => ("ambiguous", cands.iter().map(|d| d.id).collect()),
        }
    }

    fn notify(&self, hit: &RefHit) -> (&'static str, Vec<i64>) {
        let mut cands: Vec<&Def> = self.named(&hit.name, "handler");
        cands.extend(self.by_topic.get(&hit.name).into_iter().flatten());
        cands.dedup_by_key(|d| d.id);
        let role = role_dir(&hit.file);
        let near: Vec<i64> = cands
            .iter()
            .filter(|d| d.file == hit.file || (role.is_some() && role_dir(&d.file) == role))
            .map(|d| d.id)
            .collect();
        if !near.is_empty() {
            return ("exact", near);
        }
        match cands.len() {
            0 => ("unresolved", Vec::new()),
            1 => ("unique_name", vec![cands[0].id]),
            _ => ("ambiguous", cands.iter().map(|d| d.id).collect()),
        }
    }

    /// The play a symbol is in (itself, for a play).
    fn play_of(&self, mut id: i64) -> Option<i64> {
        for _ in 0..64 {
            let (parent, kind) = self.parents.get(&id)?;
            if kind == "play" {
                return Some(id);
            }
            id = (*parent)?;
        }
        None
    }

    /// Whether a variable definition is in scope for a use in `file`.
    fn in_scope(&self, d: &Def, file: &str, role: Option<&str>, loaded: &[String]) -> bool {
        if d.file == file || loaded.iter().any(|l| l.eq_ignore_ascii_case(&d.file)) {
            return true;
        }
        if let Some(r) = role {
            return role_dir(&d.file).as_deref() == Some(r) && matches!(d.tags.as_str(), "role_defaults" | "role_vars" | "set_fact" | "register");
        }
        if matches!(d.tags.as_str(), "group_vars" | "host_vars") {
            // `group_vars/` beside the playbook or in a directory above it.
            let i = d.file.find("group_vars/").or_else(|| d.file.find("host_vars/"));
            if let Some(i) = i {
                let owner = d.file[..i].trim_end_matches('/');
                return owner.is_empty() || file.starts_with(&format!("{owner}/"));
            }
        }
        false
    }

    fn var(&self, hit: &RefHit) -> (&'static str, Vec<i64>) {
        let defs = self.named(&hit.name, "variable");
        if defs.is_empty() {
            return ("unresolved", Vec::new());
        }
        let role = role_dir(&hit.file);
        let here = dir_of(&hit.file);
        let loaded: Vec<String> = self
            .vars_files
            .get(&hit.file)
            .into_iter()
            .flatten()
            .filter_map(|w| normalize(w, here).map(|(p, anchored)| if anchored { p } else { join(here, &p) }))
            .collect();
        // A play's variables, facts and registered results belong to that play, not the others in its playbook.
        let play = hit.from.as_ref().and_then(|f| self.play_of(f.id));
        let other_play = |d: &Def| {
            d.file == hit.file
                && matches!(d.tags.as_str(), "play_vars" | "set_fact" | "register")
                && play.is_some()
                && self.play_of(d.id).is_some_and(|p| Some(p) != play)
        };
        let mut scoped: Vec<&Def> =
            defs.iter().copied().filter(|d| self.in_scope(d, &hit.file, role.as_deref(), &loaded) && !other_play(d)).collect();
        if !scoped.is_empty() {
            scoped.sort_by_key(|d| std::cmp::Reverse(precedence(&d.tags)));
            return ("exact", scoped.iter().map(|d| d.id).collect());
        }
        let mut all = defs;
        all.sort_by_key(|d| std::cmp::Reverse(precedence(&d.tags)));
        // One definition, or several in one file (a variable registered by several tasks): one place.
        let one_file = all.iter().all(|d| d.file == all[0].file);
        (if one_file { "unique_name" } else { "ambiguous" }, all.iter().map(|d| d.id).collect())
    }
}

/// An Ansible fact as the workspace summary carries it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FactRow {
    pub kind: String,
    pub name: String,
    pub value: String,
    pub file: String,
    pub line: u32,
    /// The task, play or role the fact belongs to, qualified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    /// The role the file belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

/// A role the repo defines: its name and the file holding its main tasks (or its meta).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RoleRow {
    pub name: String,
    pub dir: String,
    pub file: String,
    pub line: u32,
}

/// A role reference that leaves the repo: the role's name as written, where, and from what.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RoleUse {
    pub name: String,
    pub file: String,
    pub line: u32,
    /// The play, task or role referring to it, qualified.
    pub from: Option<String>,
    pub from_kind: Option<String>,
    /// The role's task file an `include_role` runs (`tasks_from`), when not its main one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tasks_from: Option<String>,
}

/// What the workspace summary keeps of a repo's Ansible content.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Summary {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<RoleRow>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub facts: Vec<FactRow>,
    /// Role references not resolved in the repo (`external_likely` or `unresolved`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uses: Vec<RoleUse>,
    /// Variables with a literal value (`name`, `value`, role or file), for reading a deployed name written through one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub literals: Vec<(String, String, String)>,
    /// Variables whose value is Jinja (`name`, `value` as written, file), for reading a deployed value through the
    /// extra vars a CI pipeline passes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub templated: Vec<(String, String, String)>,
    /// Playbook files, by path.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub playbooks: Vec<String>,
    /// Role references resolved in the repo: the referring file and the role's directory.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub local_roles: Vec<(String, String)>,
}

impl Summary {
    pub fn is_empty(&self) -> bool {
        self.roles.is_empty() && self.facts.is_empty() && self.uses.is_empty()
    }
}

/// The repo's Ansible summary: roles it defines, facts, role references leaving it, literal variables.
pub fn summary(conn: &Connection) -> Result<Summary> {
    let has: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM files WHERE lang = 'ansible')", [], |r| r.get(0))?;
    if !has {
        return Ok(Summary::default());
    }
    let mut out = Summary::default();
    let mut stmt = conn.prepare(
        "SELECT s.name, f.path, s.start_line, s.tags FROM symbols s JOIN files f ON f.id = s.file_id
         WHERE f.lang = 'ansible' AND s.kind = 'role' ORDER BY s.tags != '', f.path",
    )?;
    let rows: Vec<(String, String, u32, String)> = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<rusqlite::Result<_>>()?;
    for (name, file, line, _) in rows {
        let dir = role_dir(&file).unwrap_or_default();
        if !out.roles.iter().any(|r: &RoleRow| r.dir == dir) {
            out.roles.push(RoleRow { name, dir, file, line });
        }
    }
    let mut stmt = conn.prepare(
        "SELECT a.kind, a.name, a.value, f.path, a.line, s.name, s.container FROM ansible_facts a JOIN files f ON f.id = a.file_id
         LEFT JOIN symbols s ON s.id = a.symbol_id WHERE f.lang = 'ansible' ORDER BY f.path, a.line",
    )?;
    let rows = stmt.query_map([], |r| {
        let file: String = r.get(3)?;
        let sym: Option<String> = r.get(5)?;
        let container: Option<String> = r.get(6)?;
        Ok(FactRow {
            kind: r.get(0)?,
            name: r.get(1)?,
            value: r.get(2)?,
            role: role_dir(&file).map(|_| container.clone().unwrap_or_default()),
            line: r.get(4)?,
            symbol: sym.map(|s| match container {
                Some(c) => format!("{c}.{s}"),
                None => s,
            }),
            file,
        })
    })?;
    for row in rows {
        let row = row?;
        if row.kind != "vars_file" && row.kind != "unsupported" {
            out.facts.push(row);
        }
    }
    let (hits, _) = crate::query::refs_of_kind(conn, "role")?;
    for h in hits {
        if matches!(h.confidence, "exact" | "unique_name") {
            for t in &h.targets {
                if let Some(dir) = role_dir(&t.file)
                    && !out.local_roles.iter().any(|(f, d)| *f == h.file && *d == dir)
                {
                    out.local_roles.push((h.file.clone(), dir));
                }
            }
        }
        if matches!(h.confidence, "external_likely" | "unresolved") {
            out.uses.push(RoleUse {
                name: h.name.clone(),
                file: h.file.clone(),
                line: h.line,
                from: h.from.as_ref().map(|f| match &f.container {
                    Some(c) => format!("{c}.{}", f.name),
                    None => f.name.clone(),
                }),
                from_kind: h.from.as_ref().map(|f| f.kind.clone()),
                tasks_from: h.receiver.clone(),
            });
        }
    }
    let mut stmt = conn.prepare(
        "SELECT s.name, s.signature, f.path FROM symbols s JOIN files f ON f.id = s.file_id
         WHERE f.lang = 'ansible' AND s.kind = 'variable' AND s.signature NOT LIKE '%{{%' AND s.signature NOT LIKE '%: {…}'
         AND s.signature NOT LIKE '%: […]' ORDER BY f.path, s.start_byte",
    )?;
    let rows: Vec<(String, String, String)> = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
    for (name, sig, file) in rows {
        let value = sig.split_once(": ").map_or("", |(_, v)| v).trim().trim_matches(['"', '\'']).to_string();
        if !value.is_empty() && value != "<vault>" {
            out.literals.push((name, value, file));
        }
    }
    let mut stmt = conn.prepare(
        "SELECT s.name, s.signature, f.path FROM symbols s JOIN files f ON f.id = s.file_id
         WHERE f.lang = 'ansible' AND s.kind = 'variable' AND s.signature LIKE '%{{%' AND s.signature NOT LIKE '%…'
         ORDER BY f.path, s.start_byte",
    )?;
    let rows: Vec<(String, String, String)> = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
    for (name, sig, file) in rows {
        let value = sig.split_once(": ").map_or("", |(_, v)| v).trim().trim_matches(['"', '\'']).to_string();
        out.templated.push((name, value, file));
    }
    out.playbooks = conn
        .prepare("SELECT DISTINCT f.path FROM symbols s JOIN files f ON f.id = s.file_id WHERE f.lang = 'ansible' AND s.kind = 'playbook' ORDER BY f.path")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert_eq!(join("a/b", "../c/./d.yml"), "a/c/d.yml");
        assert_eq!(join("", "x.yml"), "x.yml");
        assert_eq!(normalize("{{ playbook_dir }}/vars/x.yml", "plays/db"), Some(("plays/db/vars/x.yml".into(), true)));
        assert_eq!(normalize("{{ item }}.yml", "p"), None);
        assert_eq!(normalize("tasks/a.yml", "p"), Some(("tasks/a.yml".into(), false)));
    }
}
