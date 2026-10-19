//! Query-time resolution of Terraform references (see `kc_extract::terraform`), within one repo.
//!
//! Terraform resolves every address within its root configuration (or module), which is a directory: all `.tf`
//! files beside each other, never those of a subdirectory or a sibling. So does this. A reference resolves only to
//! the definition with that address in its own file's directory (`exact`); an address no file there defines is
//! `unresolved`, never another directory's same-named block, so per-environment and per-region copies of one
//! configuration stay apart.
//!
//! A module call links into its module's configuration through its `source`: a local path (`./modules/net`,
//! `../shared`) is that directory, where the call's arguments set variables (`tf_input`) and `module.m.out` reads an
//! output (`tf_output`), both `exact` when defined there and `unresolved` when not. A git or registry source is in
//! another repo: `external_likely` here, linked across repos by `kc_index::terraform`. A provider configuration a
//! child module uses without defining it (`provider = aws.east`, passed in by its caller) is `external_likely` too.

use std::collections::HashMap;

use anyhow::Result;
use rusqlite::Connection;

use crate::query::RefHit;

/// The reference kinds the Terraform extractor writes.
pub const REF_KINDS: &[&str] = &["tf_ref", "depends_on", "provider", "tf_input", "tf_output"];

/// Whether a reference of `kind` is resolved here.
pub(crate) fn handles(kind: &str) -> bool {
    REF_KINDS.contains(&kind)
}

/// Whether `file` is Terraform configuration.
pub fn is_terraform_path(file: &str) -> bool {
    kc_extract::terraform::is_terraform_path(file)
}

/// The reference kinds reaching a Terraform symbol of `kind`; `None` for other kinds.
pub fn ref_kinds(kind: &str) -> Option<&'static [&'static str]> {
    Some(match kind {
        "resource" | "data" | "module" | "local" => &["tf_ref", "depends_on"],
        "variable" => &["tf_ref", "tf_input"],
        "output" => &["tf_output"],
        "provider" => &["provider"],
        _ => return None,
    })
}

/// The edge a trace shows for a Terraform reference of `kind`.
pub fn edge_of(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "tf_ref" | "tf_output" => "uses",
        "depends_on" => "depends_on",
        "provider" => "provider",
        "tf_input" => "sets",
        _ => return None,
    })
}

fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

/// `rel` (a module source such as `./modules/x` or `../shared`) from directory `dir`; `None` when it climbs out of
/// the repo.
pub fn join(dir: &str, rel: &str) -> Option<String> {
    let mut parts: Vec<&str> = if dir.is_empty() { Vec::new() } else { dir.split('/').collect() };
    for p in rel.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            p => parts.push(p),
        }
    }
    Some(parts.join("/"))
}

/// A module source that is a local path: Terraform reads `./` and `../` prefixes as one.
pub fn is_local_source(source: &str) -> bool {
    source.starts_with("./") || source.starts_with("../") || source == "." || source == ".."
}

/// The repo's Terraform definitions by directory and address, and module sources, loaded at the first Terraform
/// reference.
#[derive(Default)]
pub(crate) struct Resolver {
    loaded: bool,
    defs: HashMap<(String, String), Vec<i64>>,
    /// Each module call's `source`, by its directory and address.
    sources: HashMap<(String, String), String>,
}

impl Resolver {
    fn load(&mut self, conn: &Connection) -> Result<()> {
        if self.loaded {
            return Ok(());
        }
        self.loaded = true;
        let mut stmt = conn.prepare(
            "SELECT s.id, s.name, f.path FROM symbols s JOIN files f ON f.id = s.file_id WHERE f.lang = 'terraform'
             ORDER BY f.path, s.start_line",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?;
        for row in rows {
            let (id, name, file) = row?;
            self.defs.entry((dir_of(&file).to_string(), name)).or_default().push(id);
        }
        let mut stmt = conn.prepare(
            "SELECT f.path, a.name, a.value FROM ansible_facts a JOIN files f ON f.id = a.file_id
             WHERE f.lang = 'terraform' AND a.kind = 'tf_source' ORDER BY f.path, a.rowid",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?;
        for row in rows {
            let (file, name, source) = row?;
            self.sources.insert((dir_of(&file).to_string(), name), source);
        }
        Ok(())
    }

    fn here(&self, dir: &str, name: &str) -> (&'static str, Vec<i64>) {
        match self.defs.get(&(dir.to_string(), name.to_string())) {
            Some(ids) if ids.len() == 1 => ("exact", ids.clone()),
            // Defined twice in one configuration: Terraform rejects it; both are candidates.
            Some(ids) => ("ambiguous", ids.clone()),
            None => ("unresolved", Vec::new()),
        }
    }

    pub(crate) fn resolve(&mut self, conn: &Connection, hit: &RefHit) -> Result<(&'static str, Vec<i64>)> {
        self.load(conn)?;
        let dir = dir_of(&hit.file);
        Ok(match hit.kind.as_str() {
            "tf_ref" | "depends_on" => self.here(dir, &hit.name),
            "provider" => match self.here(dir, &hit.name) {
                // A provider configuration the calling configuration passes in.
                ("unresolved", _) => ("external_likely", Vec::new()),
                found => found,
            },
            "tf_input" | "tf_output" => {
                let Some(module) = hit.receiver.as_deref() else { return Ok(("unresolved", Vec::new())) };
                match self.sources.get(&(dir.to_string(), module.to_string())) {
                    Some(src) if is_local_source(src) => match join(dir, src) {
                        Some(target) => self.here(&target, &hit.name),
                        None => ("unresolved", Vec::new()),
                    },
                    Some(_) => ("external_likely", Vec::new()),
                    None => ("unresolved", Vec::new()),
                }
            }
            _ => ("unresolved", Vec::new()),
        })
    }
}

/// A module call whose `source` isn't a local path, as the workspace summary carries it: what other repos it may
/// link to, and which of the module's variables and outputs it uses.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ModuleCall {
    /// The call's address (`module.spa`).
    pub name: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub file: String,
    pub line: u32,
    /// The variables its arguments set (`var.bucket`), with the argument's line.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<(String, u32)>,
    /// The outputs read (`module.spa.arn` reads `output.arn`): output, file, line, and the block reading it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<OutputUse>,
}

/// An output of a module read in the calling configuration.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OutputUse {
    pub name: String,
    pub file: String,
    pub line: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

/// What the workspace summary keeps of a repo's Terraform: its module calls to other repos.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Summary {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modules: Vec<ModuleCall>,
}

/// The repo's module calls with a git or registry source, with the inputs they set and outputs they read.
pub fn summary(conn: &Connection) -> Result<Summary> {
    let mut stmt = conn.prepare(
        "SELECT a.kind, a.name, a.value, f.path, a.line FROM ansible_facts a JOIN files f ON f.id = a.file_id
         WHERE f.lang = 'terraform' AND a.kind IN ('tf_source', 'tf_version') ORDER BY f.path, a.line",
    )?;
    let rows: Vec<(String, String, String, String, u32)> =
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?.collect::<rusqlite::Result<_>>()?;
    let mut out = Summary::default();
    for (kind, name, value, file, line) in &rows {
        if kind == "tf_source" && !is_local_source(value) {
            out.modules.push(ModuleCall { name: name.clone(), source: value.clone(), version: None, file: file.clone(), line: *line, inputs: Vec::new(), outputs: Vec::new() });
        }
    }
    if out.modules.is_empty() {
        return Ok(out);
    }
    for (kind, name, value, file, _) in &rows {
        if kind == "tf_version"
            && let Some(m) = out.modules.iter_mut().find(|m| m.name == *name && m.file == *file)
        {
            m.version = Some(value.clone());
        }
    }
    let mut stmt = conn.prepare(
        "SELECT r.kind, r.name, r.receiver, f.path, r.line, s.name FROM refs r JOIN files f ON f.id = r.file_id
         LEFT JOIN symbols s ON s.id = r.from_id
         WHERE f.lang = 'terraform' AND r.kind IN ('tf_input', 'tf_output') ORDER BY f.path, r.line",
    )?;
    type Row = (String, String, Option<String>, String, u32, Option<String>);
    let refs: Vec<Row> =
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?.collect::<rusqlite::Result<_>>()?;
    for (kind, name, receiver, file, line, from) in refs {
        let Some(m) = out.modules.iter_mut().find(|m| Some(&m.name) == receiver.as_ref() && dir_of(&m.file) == dir_of(&file)) else {
            continue;
        };
        if kind == "tf_input" {
            m.inputs.push((name, line));
        } else {
            m.outputs.push(OutputUse { name, file, line, from });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_sources() {
        assert_eq!(join("envs/prod", "../../modules/net").as_deref(), Some("modules/net"));
        assert_eq!(join("", "./modules/net").as_deref(), Some("modules/net"));
        assert_eq!(join("", "../x"), None);
        assert!(is_local_source("./m") && is_local_source("../m"));
        assert!(!is_local_source("git::https://example.com/m.git") && !is_local_source("hashicorp/consul/aws"));
    }
}
