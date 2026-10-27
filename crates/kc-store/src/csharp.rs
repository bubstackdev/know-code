//! C# name visibility for resolution: which of the repo's same-named types a file can mean, from the namespaces it
//! sees (its own and their parents, and its `using`s), and which projects a file's project references.
//!
//! A type's namespace is recorded on its symbol (see [`kc_extract::TypeDecl`]). A file's namespaces are those of the
//! types it declares. Besides its own `using` directives, a file imports what its project imports in every file: the
//! `global using` directives of the project's files, the project file's `<Using>` items (`Remove` taking one away),
//! and the SDK's implicit usings when `ImplicitUsings` is enabled. A project whose imports can't be read (a property
//! setting `ImplicitUsings`, an SDK whose implicit list isn't known) leaves its files' visibility open: nothing is
//! ruled out by what they may not import. `Directory.Build.props` files are not read.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use rusqlite::Connection;

/// What a file sees.
#[derive(Debug, Clone, Default)]
struct FileScope {
    /// Namespaces of the types the file declares.
    namespaces: Vec<String>,
    /// Its `using` directives importing namespaces (and, for `using static`, a type).
    usings: Vec<String>,
    /// Its `using` aliases: (alias, target).
    aliases: Vec<(String, String)>,
    /// What it imports can't all be read (see the module docs): it may see any namespace.
    open: bool,
}

/// What a project imports into every file of it.
#[derive(Debug, Clone, Default)]
struct ProjectUsings {
    usings: Vec<String>,
    aliases: Vec<(String, String)>,
    removed: Vec<String>,
    open: bool,
}

/// Per-query caches of file scopes and project references.
#[derive(Default)]
pub(crate) struct Visibility {
    files: HashMap<String, FileScope>,
    projects: Option<Projects>,
    /// Per project directory (`None` for files in no project), what it imports into every file.
    usings: Option<HashMap<Option<String>, ProjectUsings>>,
}

impl Visibility {
    fn scope(&mut self, conn: &Connection, file: &str) -> Result<&FileScope> {
        if !self.files.contains_key(file) {
            let decls: Vec<String> = conn
                .prepare_cached(
                    "SELECT s.type_name FROM symbols s JOIN files f ON f.id = s.file_id
                     WHERE f.path = ?1 AND s.kind IN ('class', 'struct', 'protocol', 'enum') AND s.type_name IS NOT NULL",
                )?
                .query_map([file], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            let mut namespaces: Vec<String> = Vec::new();
            let mut aliases: Vec<(String, String)> = Vec::new();
            for d in decls {
                let d = kc_extract::TypeDecl::decode(Some(&d));
                if let Some(ns) = d.ns
                    && !namespaces.contains(&ns)
                {
                    namespaces.push(ns);
                }
                if aliases.is_empty() {
                    aliases = d.aliases;
                }
            }
            let mut usings: Vec<String> = conn
                .prepare_cached("SELECT i.module FROM imports i JOIN files f ON f.id = i.file_id WHERE f.path = ?1")?
                .query_map([file], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            // An alias's target is recorded as an import too; it imports nothing.
            for (_, target) in &aliases {
                if let Some(i) = usings.iter().position(|u| u == target) {
                    usings.remove(i);
                }
            }
            // And what its projects import into every file.
            let mut open = false;
            if file.ends_with(".cs") {
                let projects = self.projects_of(conn, file)?;
                let keys: Vec<Option<String>> = if projects.is_empty() { vec![None] } else { projects.into_iter().map(Some).collect() };
                let all = self.project_usings(conn)?;
                for k in keys {
                    let Some(p) = all.get(&k) else { continue };
                    open |= p.open;
                    usings.extend(p.usings.iter().filter(|u| !p.removed.contains(u)).cloned());
                    aliases.extend(p.aliases.iter().cloned());
                }
            }
            self.files.insert(file.to_string(), FileScope { namespaces, usings, aliases, open });
        }
        Ok(&self.files[file])
    }

    /// Whether code in `file` can name the type `name` declared in namespace `ns` without qualifying it. `true` when
    /// it can't be told: a type in the global namespace, or a file declaring no type (so its namespace is unknown).
    pub(crate) fn sees(&mut self, conn: &Connection, file: &str, ns: Option<&str>, name: &str) -> Result<bool> {
        let Some(ns) = ns else { return Ok(true) };
        let scope = self.scope(conn, file)?;
        if scope.namespaces.is_empty() || scope.open {
            return Ok(true);
        }
        let inside = |f: &String| f == ns || f.strip_prefix(ns).is_some_and(|rest| rest.starts_with('.'));
        let qualified = format!("{ns}.{name}");
        Ok(scope.namespaces.iter().any(inside) || scope.usings.iter().any(|u| u == ns) || scope.aliases.iter().any(|(a, t)| a == name && *t == qualified))
    }

    /// Of the namespaces declaring a type `name`, those whose type code in `file` names when it writes `name`, as the
    /// compiler looks: the file's own namespace and then each enclosing one, then an alias
    /// (`using Order = Shop.Data.Order;`), then the namespaces it imports.
    pub(crate) fn seen_among(&mut self, conn: &Connection, file: &str, name: &str, namespaces: &[String]) -> Result<Vec<String>> {
        let scope = self.scope(conn, file)?;
        for own in &scope.namespaces {
            let mut level = Some(own.as_str());
            while let Some(l) = level {
                if namespaces.iter().any(|ns| ns == l) {
                    return Ok(vec![l.to_string()]);
                }
                level = l.rsplit_once('.').map(|(p, _)| p);
            }
        }
        let aliased: Vec<String> =
            namespaces.iter().filter(|ns| scope.aliases.iter().any(|(a, t)| a == name && *t == format!("{ns}.{name}"))).cloned().collect();
        if !aliased.is_empty() {
            return Ok(aliased);
        }
        let mut out = Vec::new();
        for ns in namespaces {
            if self.sees(conn, file, Some(ns), name)? {
                out.push(ns.clone());
            }
        }
        Ok(out)
    }

    /// Whether a type declared in namespace `ns` (nested in `container`, if it is) is what `qualifier.Name` written
    /// in a file names: `Models.Domain` for `Shop.Models.Domain`, `Outer.Inner` for a nested type.
    pub(crate) fn qualifies(ns: Option<&str>, container: Option<&str>, qualifier: &str) -> bool {
        let ends = |full: &str| full == qualifier || full.strip_suffix(qualifier).is_some_and(|head| head.ends_with('.'));
        match container {
            Some(c) => ends(c) || ns.is_some_and(|n| ends(&format!("{n}.{c}"))),
            None => ns.is_some_and(ends),
        }
    }

    /// The namespaces `file` declares types in.
    pub(crate) fn namespaces(&mut self, conn: &Connection, file: &str) -> Result<Vec<String>> {
        Ok(self.scope(conn, file)?.namespaces.clone())
    }

    /// The projects code in `file` can use: its own projects and those they reference, directly or not. `None` when
    /// the file is in no project the index knows.
    pub(crate) fn reachable_projects(&mut self, conn: &Connection, file: &str) -> Result<Option<HashSet<String>>> {
        Ok(self.loaded(conn)?.reachable(file))
    }

    /// The project directory `file` belongs to: the deepest directory holding a `.csproj` above it.
    pub(crate) fn project_of(&mut self, conn: &Connection, file: &str) -> Result<Option<String>> {
        Ok(self.loaded(conn)?.project_of(file).map(str::to_string))
    }

    /// The projects compiling `file`: the one whose directory holds it, and those linking it in from outside their own
    /// directory (`<Compile Include="..\Shared\X.cs" />`, a shared project's `.projitems`). Empty when none does.
    pub(crate) fn projects_of(&mut self, conn: &Connection, file: &str) -> Result<Vec<String>> {
        Ok(self.loaded(conn)?.projects_of(file))
    }

    /// [`Visibility::reachable_projects`], and whether those projects may see more than that: one of them references
    /// a project the index can't place (`$(…)` in its path, a file not indexed).
    pub(crate) fn reach(&mut self, conn: &Connection, file: &str) -> Result<Option<(HashSet<String>, bool)>> {
        Ok(self.loaded(conn)?.reach(file))
    }

    /// What each project imports into every file of it: its files' `global using`s, its `<Using>` items and its SDK's
    /// implicit usings.
    fn project_usings(&mut self, conn: &Connection) -> Result<&HashMap<Option<String>, ProjectUsings>> {
        if self.usings.is_none() {
            let mut out: HashMap<Option<String>, ProjectUsings> = HashMap::new();
            let items: Vec<(String, String, String)> = conn
                .prepare(
                    "SELECT f.path, d.name, d.location FROM package_deps d JOIN packages p ON p.id = d.package_id JOIN files f ON f.id = p.file_id
                     WHERE p.kind = 'csproj' AND d.source = 'using' ORDER BY f.path, d.line",
                )?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<_>>()?;
            for (path, name, how) in items {
                let p = out.entry(Some(parent(&path).to_string())).or_default();
                match how.as_str() {
                    "implicit" => match kc_extract::implicit_usings(&name) {
                        Some(list) => p.usings.extend(list.iter().map(|s| s.to_string())),
                        None => p.open = true,
                    },
                    "remove" => p.removed.push(name),
                    "static" | "include" => p.usings.push(name),
                    "unknown" => p.open = true,
                    a => match a.strip_prefix("alias:") {
                        Some(alias) => p.aliases.push((alias.to_string(), name)),
                        None => p.open = true,
                    },
                }
            }
            let globals: Vec<(String, String, Option<String>)> = conn
                .prepare("SELECT f.path, g.namespace, g.alias FROM global_usings g JOIN files f ON f.id = g.file_id ORDER BY f.path, g.line")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<_>>()?;
            let projects = self.loaded(conn)?;
            let mut placed: Vec<(Option<String>, String, Option<String>)> = Vec::new();
            for (path, ns, alias) in globals {
                let keys = projects.projects_of(&path);
                if keys.is_empty() {
                    placed.push((None, ns, alias));
                } else {
                    placed.extend(keys.into_iter().map(|k| (Some(k), ns.clone(), alias.clone())));
                }
            }
            for (k, ns, alias) in placed {
                let p = out.entry(k).or_default();
                match alias {
                    Some(a) => p.aliases.push((a, ns)),
                    None => p.usings.push(ns),
                }
            }
            self.usings = Some(out);
        }
        Ok(self.usings.as_ref().expect("loaded"))
    }

    fn loaded(&mut self, conn: &Connection) -> Result<&Projects> {
        if self.projects.is_none() {
            self.projects = Some(Projects::load(conn)?);
        }
        Ok(self.projects.as_ref().expect("loaded"))
    }
}

/// The repo's MSBuild projects: each project's directory and the directories of the projects it references, and the
/// sources projects link in from outside their directories.
#[derive(Debug, Default)]
struct Projects {
    refs: HashMap<String, Vec<String>>,
    /// (project directory, repo-relative path pattern) of each linked source.
    links: Vec<(String, String)>,
    /// Projects with a reference to a project the index can't place.
    open: HashSet<String>,
}

impl Projects {
    fn load(conn: &Connection) -> Result<Projects> {
        let rows: Vec<(String, Option<String>, Option<String>)> = conn
            .prepare(
                "SELECT f.path, d.source, d.location FROM packages p JOIN files f ON f.id = p.file_id
                 LEFT JOIN package_deps d ON d.package_id = p.id AND d.source IN ('path', 'compile')
                 WHERE p.kind = 'csproj'",
            )?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut projects = Projects::default();
        for (path, source, location) in rows {
            let dir = parent(&path).to_string();
            let entry = projects.refs.entry(dir.clone()).or_default();
            match (source.as_deref(), location) {
                // Another repo's project: not one of this repo's.
                (Some("path"), Some(loc)) if escapes(&format!("{dir}/{loc}")) => {}
                (Some("path"), Some(loc)) if loc.contains("$(") => {
                    projects.open.insert(dir.clone());
                }
                (Some("path"), Some(loc)) => entry.push(parent(&normalize(&format!("{dir}/{loc}"))).to_string()),
                (Some("compile"), Some(loc)) => projects.links.push((dir.clone(), normalize(&format!("{dir}/{loc}")))),
                _ => {}
            }
        }
        // A reference landing on no project the index knows (a missing or unindexed file): what that project sees
        // can't be told.
        let unknown: Vec<String> =
            projects.refs.iter().filter(|(_, refs)| refs.iter().any(|r| !projects.refs.contains_key(r))).map(|(d, _)| d.clone()).collect();
        projects.open.extend(unknown);
        Ok(projects)
    }

    fn projects_of(&self, file: &str) -> Vec<String> {
        let mut out: Vec<String> = self.project_of(file).map(str::to_string).into_iter().collect();
        for (project, pattern) in &self.links {
            if !out.contains(project) && glob_matches(pattern, file) {
                out.push(project.clone());
            }
        }
        out
    }

    fn project_of(&self, file: &str) -> Option<&str> {
        self.refs
            .keys()
            .filter(|dir| dir.is_empty() || file.strip_prefix(dir.as_str()).is_some_and(|rest| rest.starts_with('/')))
            .max_by_key(|dir| dir.len())
            .map(String::as_str)
    }

    /// [`Projects::reachable`], and whether any of those projects references one the index can't place, so that
    /// code in `file` may see more.
    fn reach(&self, file: &str) -> Option<(HashSet<String>, bool)> {
        let projects = self.reachable(file)?;
        let open = projects.iter().any(|p| self.open.contains(p));
        Some((projects, open))
    }

    fn reachable(&self, file: &str) -> Option<HashSet<String>> {
        let start = self.projects_of(file);
        if start.is_empty() {
            return None;
        }
        let mut out: HashSet<String> = start.iter().cloned().collect();
        let mut stack = start;
        while let Some(p) = stack.pop() {
            for r in self.refs.get(&p).into_iter().flatten() {
                if out.insert(r.clone()) {
                    stack.push(r.clone());
                }
            }
        }
        Some(out)
    }
}

/// Whether `path` matches an MSBuild item pattern: `*` within a path segment, `**` any number of segments.
fn glob_matches(pattern: &str, path: &str) -> bool {
    fn segs(p: &[&str], f: &[&str]) -> bool {
        match (p.first(), f.first()) {
            (None, None) => true,
            (Some(&"**"), _) => segs(&p[1..], f) || (!f.is_empty() && segs(p, &f[1..])),
            (Some(s), Some(t)) => seg(s.as_bytes(), t.as_bytes()) && segs(&p[1..], &f[1..]),
            _ => false,
        }
    }
    fn seg(p: &[u8], t: &[u8]) -> bool {
        match (p.first(), t.first()) {
            (None, None) => true,
            (Some(b'*'), _) => seg(&p[1..], t) || (!t.is_empty() && seg(p, &t[1..])),
            (Some(a), Some(b)) => a.eq_ignore_ascii_case(b) && seg(&p[1..], &t[1..]),
            _ => false,
        }
    }
    let p: Vec<&str> = pattern.split('/').collect();
    let f: Vec<&str> = path.split('/').collect();
    segs(&p, &f)
}

fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

/// Whether a repo-relative path climbs out of the repo (`src/../../Other/X.csproj`).
fn escapes(path: &str) -> bool {
    let mut depth: i32 = 0;
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                depth -= 1;
                if depth < 0 {
                    return true;
                }
            }
            _ => depth += 1,
        }
    }
    false
}

/// `a/b/../c/./d` → `a/c/d`.
fn normalize(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            p => out.push(p),
        }
    }
    out.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qualified_names_and_paths() {
        assert!(Visibility::qualifies(Some("Shop.Models"), None, "Models"));
        assert!(Visibility::qualifies(Some("Shop.Models"), None, "Shop.Models"));
        assert!(!Visibility::qualifies(Some("Shop.OtherModels"), None, "Models"));
        assert!(Visibility::qualifies(Some("Shop"), Some("Outer"), "Outer"));
        assert_eq!(normalize("src/Api/../Lib/./Lib.csproj"), "src/Lib/Lib.csproj");
        let p = Projects {
            links: vec![("src/Api".to_string(), "src/Shared/**/*.cs".to_string())],
            refs: HashMap::from([
                ("src/Api".to_string(), vec!["src/Lib".to_string()]),
                ("src/Lib".to_string(), vec!["src/Core".to_string()]),
                ("src/Core".to_string(), vec![]),
                ("src/Lib.Api".to_string(), vec![]),
            ]),
            open: HashSet::new(),
        };
        assert_eq!(p.project_of("src/Lib.Api/X.cs"), Some("src/Lib.Api"));
        let r = p.reachable("src/Api/Startup.cs").unwrap();
        assert!(r.contains("src/Core") && r.contains("src/Lib") && !r.contains("src/Lib.Api"));
        // A linked source is compiled by the project linking it, and sees what that project sees.
        assert_eq!(p.projects_of("src/Shared/Time/Clock.cs"), ["src/Api"]);
        assert!(p.reachable("src/Shared/Clock.cs").unwrap().contains("src/Core"));
        assert!(p.reachable("tools/Script.cs").is_none());
        assert!(escapes("src/A/../../../Other/X.csproj") && !escapes("src/A/../B/B.csproj"));
        assert!(glob_matches("a/**/*.cs", "a/B.cs") && glob_matches("a/*.cs", "a/b.CS") && !glob_matches("a/*.cs", "a/b/c.cs"));
    }
}
