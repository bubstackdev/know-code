//! Terraform answers that cross directories and repos: where a module call's `source` leads, and the module calls
//! (in this repo or another) setting a module's variables and reading its outputs. Within one configuration,
//! references resolve in `kc_store::terraform`.

use std::path::{Path, PathBuf};

use anyhow::Result;
use kc_store::{query, trace};
use serde::Serialize;

use crate::ops::{HttpCrossing, Landing, Opened, open_synced, repo_name};

fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

/// The Terraform side of a context's symbol: for a module call, where its `source` leads; for any block of a
/// module's configuration, the module calls using that configuration, here and in other repos.
#[derive(Serialize, Default)]
pub struct TerraformOut {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<ModuleOut>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub module_called_by: Vec<ModuleCallerOut>,
}

/// Where a module call's `source` leads.
#[derive(Serialize)]
pub struct ModuleOut {
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// `local`, `git`, `registry` or `other`.
    pub kind: &'static str,
    /// The repo holding the module, when it is another indexed repo.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// The module's directory (in this repo for a local source, else in `repo`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
    /// `exact` (a local directory holding `.tf` files, or a git URL that is the repo's origin), `unique_name`,
    /// `ambiguous`, `unresolved` (a local directory with no configuration) or `external_likely` (not indexed).
    pub confidence: &'static str,
    /// What the link did not check: `revision_unverified` when the revision asked for isn't the indexed checkout's.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub assumptions: Vec<kc_store::answer::Assumption>,
}

/// A module call using a module's configuration.
#[derive(Serialize)]
pub struct ModuleCallerOut {
    pub repo: String,
    /// The call's address (`module.spa`).
    pub module: String,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub file: String,
    pub line: u32,
    pub confidence: &'static str,
    /// What the link did not check: `revision_unverified` when the revision asked for isn't the indexed checkout's.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub assumptions: Vec<kc_store::answer::Assumption>,
}

/// A module call in some repo: its address, source, version, file and line.
struct Call {
    name: String,
    source: String,
    version: Option<String>,
    file: String,
    line: u32,
}

/// The repo's module calls, local ones included.
fn calls(conn: &rusqlite::Connection) -> Result<Vec<Call>> {
    let mut stmt = conn.prepare(
        "SELECT a.kind, a.name, a.value, f.path, a.line FROM ansible_facts a JOIN files f ON f.id = a.file_id
         WHERE f.lang = 'terraform' AND a.kind IN ('tf_source', 'tf_version') ORDER BY f.path, a.line",
    )?;
    let rows: Vec<(String, String, String, String, u32)> =
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?.collect::<rusqlite::Result<_>>()?;
    let mut out: Vec<Call> = rows
        .iter()
        .filter(|r| r.0 == "tf_source")
        .map(|(_, name, source, file, line)| Call { name: name.clone(), source: source.clone(), version: None, file: file.clone(), line: *line })
        .collect();
    for (kind, name, value, file, _) in &rows {
        if kind == "tf_version"
            && let Some(c) = out.iter_mut().find(|c| c.name == *name && c.file == *file)
        {
            c.version = Some(value.clone());
        }
    }
    Ok(out)
}

/// The directories of the repo holding Terraform files.
fn config_dirs(conn: &rusqlite::Connection) -> Result<Vec<String>> {
    let paths: Vec<String> =
        conn.prepare("SELECT path FROM files WHERE lang = 'terraform'")?.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    let mut dirs: Vec<String> = paths.iter().map(|p| dir_of(p).to_string()).collect();
    dirs.sort();
    dirs.dedup();
    Ok(dirs)
}

/// The Terraform side of a context's symbol, or `None` for a symbol that isn't Terraform or has none.
pub fn terraform_for(o: &Opened, s: &query::SymbolRow) -> Result<Option<TerraformOut>> {
    if !kc_store::terraform::is_terraform_path(&s.file) {
        return Ok(None);
    }
    let conn = o.store.conn();
    let summaries = crate::ops::full_summaries(o)?;
    let tf = kc_index::terraform::Terraform::new(&summaries);
    let dir = dir_of(&s.file);
    let mut out = TerraformOut::default();
    let mine = calls(conn)?;
    if s.kind == "module"
        && let Some(c) = mine.iter().find(|c| c.name == s.name && c.file == s.file)
    {
        out.module = Some(module_out(conn, &tf, dir, c)?);
    }
    // Calls in this repo whose local source is this directory, then calls in other repos linking to it.
    for c in &mine {
        if kc_store::terraform::is_local_source(&c.source) && kc_store::terraform::join(dir_of(&c.file), &c.source).as_deref() == Some(dir) {
            out.module_called_by.push(ModuleCallerOut {
                repo: repo_name(&o.root),
                module: c.name.clone(),
                source: c.source.clone(),
                version: c.version.clone(),
                file: c.file.clone(),
                line: c.line,
                confidence: "exact",
                assumptions: Vec::new(),
            });
        }
    }
    for (repo, c, l) in tf.callers(&o.root, dir) {
        out.module_called_by.push(ModuleCallerOut {
            repo: repo_name(&repo),
            module: c.name,
            source: c.source,
            version: c.version,
            file: c.file,
            line: c.line,
            confidence: l.confidence,
            assumptions: l.assumptions,
        });
    }
    Ok((out.module.is_some() || !out.module_called_by.is_empty()).then_some(out))
}

fn module_out(conn: &rusqlite::Connection, tf: &kc_index::terraform::Terraform, dir: &str, c: &Call) -> Result<ModuleOut> {
    use kc_index::terraform::Source;
    let mut out =
        ModuleOut { source: c.source.clone(), version: c.version.clone(), kind: "other", repo: None, dir: None, confidence: "external_likely", assumptions: Vec::new() };
    match kc_index::terraform::parse(&c.source) {
        Source::Local(rel) => {
            out.kind = "local";
            out.dir = kc_store::terraform::join(dir, &rel);
            out.confidence = match &out.dir {
                Some(d) if config_dirs(conn)?.contains(d) => "exact",
                _ => "unresolved",
            };
            return Ok(out);
        }
        Source::Git { .. } => out.kind = "git",
        Source::Registry { .. } => out.kind = "registry",
        Source::Other => {}
    }
    let links = tf.link_version(&c.source, c.version.as_deref());
    if let Some(l) = links.first() {
        out.repo = Some(if links.len() == 1 { repo_name(&l.repo) } else { links.iter().map(|l| repo_name(&l.repo)).collect::<Vec<_>>().join(", ") });
        out.dir = Some(l.dir.clone());
        out.confidence = l.confidence;
        out.assumptions.clone_from(&l.assumptions);
    }
    Ok(out)
}

/// The innermost Terraform block (or local) spanning `line` of `file`.
fn symbol_at(conn: &rusqlite::Connection, file: &str, line: u32) -> Result<Option<query::SymbolRow>> {
    let id: rusqlite::Result<i64> = conn
        .prepare_cached(
            "SELECT s.id FROM symbols s JOIN files f ON f.id = s.file_id WHERE f.path = ?1 AND s.start_line <= ?2 AND s.end_line >= ?2
             ORDER BY s.end_line - s.start_line LIMIT 1",
        )?
        .query_row(rusqlite::params![file, line], |r| r.get(0));
    match id {
        Ok(id) => trace::symbol(conn, id),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// A use of a module's variable or output by a module call in another repo.
struct Use {
    repo: PathBuf,
    /// `tf_input` (the call sets the variable) or `tf_output` (an expression reads the output).
    kind: &'static str,
    file: String,
    line: u32,
    call: String,
    source: String,
    link: &'static str,
    assumptions: Vec<kc_store::answer::Assumption>,
}

/// The uses, in other repos, of `sym`: a variable or output of a module configuration of the repo at `root`.
fn uses_elsewhere(summaries: &[kc_index::workspace::RepoSummary], root: &Path, sym: &query::SymbolRow) -> Vec<Use> {
    if !kc_store::terraform::is_terraform_path(&sym.file) || !matches!(sym.kind.as_str(), "variable" | "output") {
        return Vec::new();
    }
    let tf = kc_index::terraform::Terraform::new(summaries);
    let mut out = Vec::new();
    for (repo, c, l) in tf.callers(root, dir_of(&sym.file)) {
        let (link, assumptions) = (l.confidence, l.assumptions);
        if sym.kind == "variable" {
            for (_, line) in c.inputs.iter().filter(|(n, _)| *n == sym.name) {
                let source = c.source.clone();
                out.push(Use { repo: repo.clone(), kind: "tf_input", file: c.file.clone(), line: *line, call: c.name.clone(), source, link, assumptions: assumptions.clone() });
            }
        } else {
            for u in c.outputs.iter().filter(|u| u.name == sym.name) {
                let source = c.source.clone();
                out.push(Use { repo: repo.clone(), kind: "tf_output", file: u.file.clone(), line: u.line, call: c.name.clone(), source, link, assumptions: assumptions.clone() });
            }
        }
    }
    out
}

/// References in other repos to a variable or output of a module this repo holds (`name`), through module calls
/// whose `source` links here.
pub fn module_refs_elsewhere(o: &Opened, name: &str, no_sync: bool) -> Result<Vec<query::RefHit>> {
    let syms: Vec<query::SymbolRow> = query::definitions(o.store.conn(), name)?
        .into_iter()
        .filter(|s| kc_store::terraform::is_terraform_path(&s.file) && matches!(s.kind.as_str(), "variable" | "output"))
        .collect();
    if syms.is_empty() {
        return Ok(Vec::new());
    }
    let summaries = crate::ops::full_summaries(o)?;
    let mut out = Vec::new();
    for sym in &syms {
        for u in uses_elsewhere(&summaries, &o.root, sym) {
            if !u.repo.join(".git").exists() {
                continue;
            }
            let store = open_synced(o, &u.repo, no_sync)?;
            let from = symbol_at(store.conn(), &u.file, u.line)?
                .map(|s| query::FromSymbol { id: s.id, name: s.name, kind: s.kind, container: s.container });
            let target = query::Target { id: sym.id, qualified: sym.qualified(), kind: sym.kind.clone(), file: sym.file.clone(), line: sym.line, repo: None };
            let code = std::fs::read_to_string(u.repo.join(&u.file))
                .ok()
                .and_then(|t| t.lines().nth(u.line.saturating_sub(1) as usize).map(|l| l.trim().to_string()));
            out.push(query::RefHit {
                code,
                precise: false,
                corrected_from: None,
                name: sym.name.clone(),
                kind: u.kind.into(),
                receiver: Some(u.call),
                receiver_type: None,
                file: u.file,
                line: u.line,
                col: 0,
                from,
                confidence: u.link,
                targets: vec![target],
                repo: Some(repo_name(&u.repo)),
                receiver_chain: Vec::new(),
                target_ids: vec![sym.id],
                targets_total: None,
                via_di: None,
                via_di_any: Vec::new(),
                usage: None,
                ctors: None,
                args: None,
                arg_types: None,
                assigns: false,
                factory: false,
                through: None,
                basis: None,
                // A module call's source names the module: as an import does.
                evidence: kc_store::answer::Evidence::by_confidence(u.link, kc_store::answer::Mechanism::Import).map(|e| e.assuming_all(&u.assumptions)),
            });
        }
    }
    Ok(out)
}

impl HttpCrossing<'_> {
    /// The blocks in other repos setting `sym` (a module's variable) through a module call, or reading it (a module's
    /// output); one landing per repo and call.
    pub(crate) fn module_landings(
        &mut self,
        root: &Path,
        sym: &query::SymbolRow,
        summaries: &[kc_index::workspace::RepoSummary],
    ) -> Result<Vec<Landing>> {
        let mut out: Vec<Landing> = Vec::new();
        for u in uses_elsewhere(summaries, root, sym) {
            if !u.repo.join(".git").exists() || !self.open(&u.repo)? {
                continue;
            }
            let Some(from) = symbol_at(self.store(&u.repo).conn(), &u.file, u.line)? else { continue };
            let creator = (from, "exact", Some(format!("{}:{}", u.file, u.line)));
            match out.iter_mut().find(|l| l.root == u.repo && l.via.module().is_some_and(|m| m.module == u.call)) {
                Some(l) => {
                    if !l.creators.iter().any(|c| c.0.id == creator.0.id) {
                        l.creators.push(creator);
                    }
                }
                None => {
                    let via = trace::Via::Module(trace::ModuleVia { module: u.call.clone(), source: u.source.clone(), link: u.link, assumptions: u.assumptions.clone() });
                    out.push(Landing { root: u.repo.clone(), via, creators: vec![creator], publish: None });
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dirs() {
        assert_eq!(dir_of("envs/prod/main.tf"), "envs/prod");
        assert_eq!(dir_of("main.tf"), "");
    }
}
