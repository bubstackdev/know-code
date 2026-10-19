//! Query-time resolution of CFML references (see `kc_extract::cfml`).
//!
//! CFML names files, not types, so references resolve through paths, case-insensitively, as the engine finds them:
//!
//! - an include (`include:<path>`) relative to the including file, or from the web root (taken to be the repo root)
//!   when the path starts with `/`, through a `this.mappings` entry when one matches; a relative path found only from
//!   the web root (a fragment written for the page including it) is `unique_name`;
//! - a component (`component:a.b.C`) as `a/b/C.cfc` next to the caller, through a mapping, or from the root; failing
//!   those, the one component anywhere whose path ends that way (`unique_name`);
//! - a custom tag (`tag:x`) as `x.cfm` next to the caller, then in a `customtags` directory, then anywhere;
//! - a method on a component through the component and what it extends; an unqualified call in a component to its
//!   own methods first; a method on a component from outside the repo is `unresolved`;
//! - a user-defined function by name: in the caller's file, or a file it includes, is `exact`; otherwise the only
//!   definition is `unique_name`, several are `ambiguous`.

use std::collections::HashMap;

use anyhow::Result;
use rusqlite::Connection;

use kc_extract::cfml::MAPPING_PREFIX;

/// Whether a reference in `file` is resolved here.
pub(crate) fn handles(file: &str) -> bool {
    kc_extract::cfml::is_cfml_path(file)
}

/// A callable or property definition in a CFML file.
#[derive(Debug, Clone)]
struct Def {
    id: i64,
    container: Option<String>,
    file: String,
}

/// The repo's CFML files, loaded when the first CFML reference is resolved.
pub(crate) struct Files {
    /// Lower-case path → (path, file symbol id).
    by_path: HashMap<String, (String, i64)>,
    /// (logical prefix without `/`, lower case; physical directory, lower case), longest prefix first.
    mappings: Vec<(String, String)>,
    defs: HashMap<String, Vec<Def>>,
    /// Lower-case paths a file includes directly.
    includes: HashMap<String, Vec<String>>,
}

impl Files {
    pub(crate) fn load(conn: &Connection) -> Result<Files> {
        let mut stmt = conn.prepare(
            "SELECT f.path, s.id FROM files f JOIN symbols s ON s.file_id = f.id
             WHERE f.lang = 'cfml' AND s.parent_id IS NULL AND s.kind IN ('class', 'template') ORDER BY f.path, s.start_byte",
        )?;
        let mut by_path = HashMap::new();
        for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
            let (path, id) = row?;
            by_path.entry(path.to_ascii_lowercase()).or_insert((path, id));
        }
        let mut stmt = conn.prepare("SELECT qualified, value FROM constants WHERE qualified LIKE ?1 ORDER BY qualified, value")?;
        let mut mappings: Vec<(String, String)> = stmt
            .query_map([format!("{MAPPING_PREFIX}%")], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .filter_map(|r| r.ok())
            .map(|(q, v)| (q[MAPPING_PREFIX.len()..].trim_matches('/').to_ascii_lowercase(), v.to_ascii_lowercase()))
            .collect();
        mappings.sort_by_key(|(l, _)| std::cmp::Reverse(l.len()));
        Ok(Files { by_path, mappings, defs: HashMap::new(), includes: HashMap::new() })
    }

    fn file(&self, lower: &str) -> Option<i64> {
        self.by_path.get(lower).map(|(_, id)| *id)
    }

    /// The file a path from the web root names: through a mapping, else from the repo root.
    fn under_root(&self, rel: &str) -> Option<String> {
        let rel = rel.trim_start_matches('/');
        for (logical, physical) in &self.mappings {
            let rest = if logical.is_empty() {
                Some(rel)
            } else {
                rel.strip_prefix(logical.as_str()).and_then(|r| if r.is_empty() { Some(r) } else { r.strip_prefix('/') })
            };
            if let Some(rest) = rest {
                let p = normalize(&format!("{physical}/{rest}"));
                if self.by_path.contains_key(&p) {
                    return Some(p);
                }
            }
        }
        let p = normalize(rel);
        self.by_path.contains_key(&p).then_some(p)
    }

    /// The file an include in `from` names, lower case, and whether it was found by path (else by suffix).
    fn include(&self, from: &str, template: &str) -> Option<(String, bool)> {
        let t = template.replace('\\', "/").to_ascii_lowercase();
        if t.contains('#') {
            return None;
        }
        if t.starts_with('/') {
            if let Some(p) = self.under_root(&t) {
                return Some((p, true));
            }
            // The web root may be a directory of the repo: the one file whose path ends this way.
            return self.unique_suffix(&t).map(|p| (p, false));
        }
        let p = normalize(&format!("{}/{t}", dir_of(&from.to_ascii_lowercase())));
        if self.by_path.contains_key(&p) {
            return Some((p, true));
        }
        // A fragment written for the page that includes it: the path from the web root.
        self.under_root(&t).map(|p| (p, false))
    }

    /// The component a dotted path names from `from`, and whether it was found by path (else by suffix). Several
    /// suffix matches are all returned, unranked.
    fn component(&self, from: &str, dotted: &str) -> Vec<(String, bool)> {
        let rel = format!("{}.cfc", dotted.trim().to_ascii_lowercase().replace('.', "/"));
        let here = normalize(&format!("{}/{rel}", dir_of(&from.to_ascii_lowercase())));
        if self.by_path.contains_key(&here) {
            return vec![(here, true)];
        }
        if let Some(p) = self.under_root(&rel) {
            return vec![(p, true)];
        }
        self.suffix(&rel).into_iter().map(|p| (p, false)).collect()
    }

    /// A custom tag's template from `from`: beside it, in a `customtags` directory, or anywhere.
    fn custom_tag(&self, from: &str, name: &str) -> Vec<(String, bool)> {
        let rel = format!("{}.cfm", name.to_ascii_lowercase().replace('.', "/"));
        let here = normalize(&format!("{}/{rel}", dir_of(&from.to_ascii_lowercase())));
        if self.by_path.contains_key(&here) {
            return vec![(here, true)];
        }
        let all = self.suffix(&rel);
        let tags: Vec<String> = all.iter().filter(|p| p.contains("customtags/")).cloned().collect();
        let pick = if tags.is_empty() { all } else { tags };
        pick.into_iter().map(|p| (p, false)).collect()
    }

    fn suffix(&self, rel: &str) -> Vec<String> {
        let rel = rel.trim_start_matches('/');
        let tail = format!("/{rel}");
        let mut out: Vec<String> = self.by_path.keys().filter(|p| p.as_str() == rel || p.ends_with(&tail)).cloned().collect();
        out.sort();
        out
    }

    fn unique_suffix(&self, rel: &str) -> Option<String> {
        match self.suffix(rel).as_slice() {
            [one] => Some(one.clone()),
            _ => None,
        }
    }

    fn defs(&mut self, conn: &Connection, name: &str) -> Result<&[Def]> {
        if !self.defs.contains_key(name) {
            // `name` compares without case: the column is NOCASE.
            let mut stmt = conn.prepare_cached(
                "SELECT s.id, s.container, f.path FROM symbols s JOIN files f ON f.id = s.file_id
                 WHERE s.name = ?1 AND f.lang = 'cfml' AND s.kind IN ('function', 'method') ORDER BY f.path, s.start_line",
            )?;
            let defs = stmt
                .query_map([name], |r| Ok(Def { id: r.get(0)?, container: r.get(1)?, file: r.get(2)? }))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            self.defs.insert(name.to_string(), defs);
        }
        Ok(&self.defs[name])
    }

    /// The files `file` includes directly, lower case.
    fn includes_of(&mut self, conn: &Connection, file: &str) -> Result<&[String]> {
        if !self.includes.contains_key(file) {
            let mut stmt = conn.prepare_cached(
                "SELECT r.receiver_type FROM refs r JOIN files f ON f.id = r.file_id
                 WHERE f.path = ?1 AND r.receiver_type LIKE 'include:%'",
            )?;
            let paths: Vec<String> = stmt.query_map([file], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<_>>()?;
            let mut out: Vec<String> = paths.iter().filter_map(|p| self.include(file, &p["include:".len()..]).map(|(f, _)| f)).collect();
            out.sort();
            out.dedup();
            self.includes.insert(file.to_string(), out);
        }
        Ok(&self.includes[file])
    }

    /// The methods named `name` in the component at `file` (lower case) or the components it extends.
    fn method_in(&mut self, conn: &Connection, file: &str, name: &str, depth: u8) -> Result<Vec<i64>> {
        let own: Vec<i64> =
            self.defs(conn, name)?.iter().filter(|d| d.container.is_some() && d.file.eq_ignore_ascii_case(file)).map(|d| d.id).collect();
        if !own.is_empty() || depth >= 6 {
            return Ok(own);
        }
        for sup in self.supers(conn, file)? {
            let found = self.method_in(conn, &sup, name, depth + 1)?;
            if !found.is_empty() {
                return Ok(found);
            }
        }
        Ok(Vec::new())
    }

    /// The components a component file extends or implements, lower case, where they resolve.
    fn supers(&self, conn: &Connection, file: &str) -> Result<Vec<String>> {
        let Some(&(ref path, id)) = self.by_path.get(file) else { return Ok(Vec::new()) };
        let mut stmt = conn.prepare_cached("SELECT target FROM relations WHERE from_id = ?1")?;
        let targets: Vec<String> = stmt.query_map([id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        Ok(targets.iter().filter_map(|t| self.component(path, t).into_iter().next().map(|(p, _)| p)).collect())
    }

    /// Resolves one reference from a CFML file: its confidence and candidate symbol ids.
    pub(crate) fn resolve(&mut self, conn: &Connection, file: &str, name: &str, kind: &str, receiver: Option<&str>, receiver_type: Option<&str>) -> Result<(&'static str, Vec<i64>)> {
        let found = |paths: Vec<(String, bool)>, files: &Files| -> (&'static str, Vec<i64>) {
            let ids: Vec<i64> = paths.iter().filter_map(|(p, _)| files.file(p)).collect();
            match (ids.len(), paths.first().is_some_and(|(_, exact)| *exact)) {
                (0, _) => ("unresolved", ids),
                (1, true) => ("exact", ids),
                (1, false) => ("unique_name", ids),
                _ => ("ambiguous", ids),
            }
        };
        let (scheme, value) = receiver_type.and_then(|t| t.split_once(':')).unwrap_or(("", ""));
        match scheme {
            "include" => return Ok(found(self.include(file, value).into_iter().collect(), self)),
            "tag" => return Ok(found(self.custom_tag(file, value), self)),
            "java" => return Ok(("unresolved", Vec::new())),
            "component" if kind == "instantiate" => return Ok(found(self.component(file, value), self)),
            "component" => {
                let comps = self.component(file, value);
                if let [(comp, exact)] = comps.as_slice() {
                    let ids = self.method_in(conn, comp, name, 0)?;
                    let confidence = if ids.is_empty() { "unresolved" } else if *exact { "exact" } else { "unique_name" };
                    return Ok((confidence, ids));
                }
                // Several components end with that path: their methods. None: a component from outside the repo.
                let mut ids = Vec::new();
                for (comp, _) in &comps {
                    ids.extend(self.method_in(conn, comp, name, 0)?);
                }
                return Ok((if ids.is_empty() { "unresolved" } else { "ambiguous" }, ids));
            }
            _ => {}
        }
        let lower = file.to_ascii_lowercase();
        let in_component = lower.ends_with(".cfc");
        match receiver.map(|r| r.to_ascii_lowercase()) {
            Some(r) if matches!(r.as_str(), "this" | "variables" | "super") && in_component => {
                let ids = if r == "super" {
                    let mut ids = Vec::new();
                    for sup in self.supers(conn, &lower)? {
                        ids = self.method_in(conn, &sup, name, 1)?;
                        if !ids.is_empty() {
                            break;
                        }
                    }
                    ids
                } else {
                    self.method_in(conn, &lower, name, 0)?
                };
                Ok((if ids.is_empty() { "unresolved" } else { "exact" }, ids))
            }
            None => {
                if in_component {
                    let own = self.method_in(conn, &lower, name, 0)?;
                    if !own.is_empty() {
                        return Ok(("exact", own));
                    }
                }
                // A user-defined function: free functions of templates.
                let free: Vec<Def> = self.defs(conn, name)?.iter().filter(|d| d.container.is_none()).cloned().collect();
                if free.is_empty() {
                    return Ok(("unresolved", Vec::new()));
                }
                let same: Vec<i64> = free.iter().filter(|d| d.file == file).map(|d| d.id).collect();
                if !same.is_empty() {
                    return Ok(("exact", same));
                }
                let included = self.includes_of(conn, file)?.to_vec();
                let via: Vec<i64> = free.iter().filter(|d| included.contains(&d.file.to_ascii_lowercase())).map(|d| d.id).collect();
                if via.len() == 1 {
                    return Ok(("exact", via));
                }
                Ok(match free.len() {
                    1 => ("unique_name", vec![free[0].id]),
                    _ => ("ambiguous", free.iter().map(|d| d.id).collect()),
                })
            }
            // A call on a variable whose component isn't known: a method by that name in a component.
            Some(_) => {
                let methods: Vec<Def> = self.defs(conn, name)?.iter().filter(|d| d.container.is_some()).cloned().collect();
                let mut containers: Vec<&str> = methods.iter().filter_map(|d| d.container.as_deref()).collect();
                containers.sort_unstable();
                containers.dedup();
                let ids = methods.iter().map(|d| d.id).collect();
                Ok(match containers.len() {
                    0 => ("unresolved", Vec::new()),
                    1 => ("unique_name", ids),
                    _ => ("ambiguous", ids),
                })
            }
        }
    }
}

fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

/// `a/./b/../c` → `a/c`, without leading or trailing `/`.
fn normalize(p: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(paths: &[&str], mappings: &[(&str, &str)]) -> Files {
        Files {
            by_path: paths.iter().enumerate().map(|(i, p)| (p.to_ascii_lowercase(), (p.to_string(), i as i64 + 1))).collect(),
            mappings: mappings.iter().map(|(l, p)| (l.to_string(), p.to_string())).collect(),
            defs: HashMap::new(),
            includes: HashMap::new(),
        }
    }

    #[test]
    fn includes_resolve_relative_to_the_file_or_the_root() {
        let f = files(&["site/_inc/Header.cfm", "site/page.cfm", "_inc/x.cfm", "admin/panels/a.cfm"], &[]);
        assert_eq!(f.include("site/page.cfm", "_inc/header.cfm"), Some(("site/_inc/header.cfm".into(), true)));
        assert_eq!(f.include("site/sub/b.cfm", "../_inc/HEADER.cfm"), Some(("site/_inc/header.cfm".into(), true)));
        assert_eq!(f.include("site/page.cfm", "/_inc/x.cfm"), Some(("_inc/x.cfm".into(), true)));
        // A web root below the repo root: the one file ending that way.
        assert_eq!(f.include("x.cfm", "/panels/a.cfm"), Some(("admin/panels/a.cfm".into(), false)));
        assert_eq!(f.include("site/page.cfm", "missing.cfm"), None);
        assert_eq!(f.include("site/page.cfm", "_inc/x.cfm"), Some(("_inc/x.cfm".into(), false)));
        assert_eq!(f.include("site/page.cfm", "#dyn#.cfm"), None);
    }

    #[test]
    fn components_resolve_beside_the_caller_through_mappings_then_by_suffix() {
        let f = files(&["app/model/User.cfc", "lib/model/User.cfc", "shared/Cache.cfc", "app/Svc.cfc", "x/y/Only.cfc"], &[("lib", "shared")]);
        assert_eq!(f.component("app/Svc.cfc", "model.User"), vec![("app/model/user.cfc".to_string(), true)]);
        assert_eq!(f.component("other/a.cfm", "lib.Cache"), vec![("shared/cache.cfc".to_string(), true)]);
        assert_eq!(f.component("other/a.cfm", "y.Only"), vec![("x/y/only.cfc".to_string(), false)]);
        assert_eq!(f.component("other/a.cfm", "model.User").len(), 2);
    }
}
