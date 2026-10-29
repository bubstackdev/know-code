//! Which definitions a reference can bind to by name, before any rule picks among them.
//!
//! - **Language.** A reference binds only to definitions in a language its own can see: the same language, and
//!   TypeScript, TSX and JavaScript among each other (one module system). Edges between languages come only from the
//!   integration rules that know the mechanism (HTTP routes, the message bus, SQL, DI, Angular templates), never from
//!   a shared name.
//! - **C# project.** A C# file is compiled by the project whose directory holds it (the deepest `.csproj` above it)
//!   and by projects linking it in (`<Compile Include="..\…" />`, a shared project's `.projitems`). It sees those
//!   projects and the projects they reference, transitively; a definition compiled only by other projects is out of
//!   its reach. When no project covers the file, or the definition, or a project on the way references one the index
//!   can't place, the binding is an assumption: never `exact` from the name alone.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use rusqlite::Connection;

use crate::csharp::Visibility;

/// Whether code in one file can see a definition in another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Sight {
    /// It can.
    Yes,
    /// It may: what decides it (a project file) is missing.
    Assumed,
    /// It can't.
    No,
}

/// The family of languages that see each other's definitions by name.
pub(crate) fn family(lang: &str) -> &str {
    match lang {
        "typescript" | "tsx" | "javascript" => "js",
        // Java and Kotlin call each other within a module, as one JVM family.
        "java" | "kotlin" => "jvm",
        l => l,
    }
}

/// Per-resolution caches of files' languages and C# project reach.
#[derive(Default)]
pub(crate) struct Scope {
    langs: HashMap<String, Option<String>>,
    /// Per C# file: the projects it can use, and whether they may see more (see [`Visibility::reach`]).
    reach: HashMap<String, Option<(HashSet<String>, bool)>>,
    projects: HashMap<String, Vec<String>>,
    keys: HashMap<String, String>,
    vis: Visibility,
}

impl Scope {
    /// A file's language as indexed, `None` for a file the index doesn't have.
    pub(crate) fn lang(&mut self, conn: &Connection, file: &str) -> Result<Option<&str>> {
        if !self.langs.contains_key(file) {
            let mut stmt = conn.prepare_cached("SELECT lang FROM files WHERE path = ?1")?;
            let lang: Option<String> = stmt.query_row([file], |r| r.get(0)).ok();
            self.langs.insert(file.to_string(), lang);
        }
        Ok(self.langs[file].as_deref())
    }

    fn reach(&mut self, conn: &Connection, file: &str) -> Result<Option<&(HashSet<String>, bool)>> {
        if !self.reach.contains_key(file) {
            let r = self.vis.reach(conn, file)?;
            self.reach.insert(file.to_string(), r);
        }
        Ok(self.reach[file].as_ref())
    }

    fn projects(&mut self, conn: &Connection, file: &str) -> Result<&[String]> {
        if !self.projects.contains_key(file) {
            let p = self.vis.projects_of(conn, file)?;
            self.projects.insert(file.to_string(), p);
        }
        Ok(&self.projects[file])
    }

    /// A key equal for callers that see the same definitions: their language family and, in C#, the projects they
    /// reach.
    pub(crate) fn key(&mut self, conn: &Connection, caller: &str) -> Result<String> {
        if let Some(k) = self.keys.get(caller) {
            return Ok(k.clone());
        }
        let k = self.key_uncached(conn, caller)?;
        self.keys.insert(caller.to_string(), k.clone());
        Ok(k)
    }

    fn key_uncached(&mut self, conn: &Connection, caller: &str) -> Result<String> {
        let Some(lang) = self.lang(conn, caller)? else { return Ok(String::new()) };
        let fam = family(lang).to_string();
        if fam != "csharp" {
            return Ok(fam);
        }
        Ok(match self.reach(conn, caller)? {
            Some((projects, open)) => {
                let mut p: Vec<&str> = projects.iter().map(String::as_str).collect();
                p.sort_unstable();
                format!("csharp|{open}|{}", p.join("|"))
            }
            None => "csharp|*".to_string(),
        })
    }

    /// Whether code in `caller` can see a definition in `file`, of language `lang`.
    pub(crate) fn sees(&mut self, conn: &Connection, caller: &str, file: &str, lang: &str) -> Result<Sight> {
        if caller == file {
            return Ok(Sight::Yes);
        }
        let Some(own) = self.lang(conn, caller)? else { return Ok(Sight::Yes) };
        let own = family(own).to_string();
        if own != family(lang) {
            return Ok(Sight::No);
        }
        if own != "csharp" {
            return Ok(Sight::Yes);
        }
        let theirs = self.projects(conn, file)?.to_vec();
        let Some((reach, open)) = self.reach(conn, caller)? else { return Ok(Sight::Assumed) };
        Ok(if theirs.is_empty() {
            Sight::Assumed
        } else if theirs.iter().any(|p| reach.contains(p)) {
            Sight::Yes
        } else if *open {
            Sight::Assumed
        } else {
            Sight::No
        })
    }
}
