//! Python module scoping for resolution: a name `from m import`ed comes from `m`, and a member of an `import`ed module
//! (`m.save()`) is that module's.
//!
//! A module is placed as the interpreter would find it from some source root: a relative module (`.b`, `..pkg.b`)
//! from the importing file's package; an absolute one (`pkg.b`) by the repo files whose path ends with `pkg/b.py` or
//! `pkg/b/__init__.py`. The module is certain only when one of them is under a plausible source root, the repo's or a
//! directory above the importer; otherwise (`src/pkg/b.py` imported from `tests/`, or a repo file shadowing a
//! standard module's name) every match is a candidate, never certain. A module no repo file provides (nor a
//! directory, a namespace package) is from outside the repo.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use rusqlite::Connection;

/// Where a Python reference's name must be defined, by the file's imports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Scope {
    /// Outside the repo: an installed package or the standard library.
    External,
    /// A top-level declaration named `name` in one of `files`: the module's file, and the files it takes the name
    /// from in turn (`from .x import name`, `from .x import *`). `sure` when the module is certain (see the module
    /// docs); otherwise `files` are every candidate.
    Module { files: Vec<String>, name: String, sure: bool },
}

#[derive(Debug, Clone)]
struct Binding {
    local: String,
    name: String,
    module: String,
}

/// What a module is.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Place {
    /// These repo files; certain when `true` (one file, under a plausible source root).
    Files(Vec<String>, bool),
    External,
    /// A package directory without `__init__.py`, or a relative module the index doesn't have: can't tell.
    Unknown,
}

/// Per-pass caches of import bindings and module places.
#[derive(Default)]
pub(crate) struct Modules {
    bindings: HashMap<String, Vec<Binding>>,
    places: HashMap<(String, String), Place>,
    paths: Option<HashSet<String>>,
}

impl Modules {
    fn file_bindings(&mut self, conn: &Connection, file: &str) -> Result<&[Binding]> {
        if !self.bindings.contains_key(file) {
            let mut stmt = conn.prepare_cached(
                "SELECT b.local, b.name, b.module FROM import_bindings b JOIN files f ON f.id = b.file_id WHERE f.path = ?1",
            )?;
            let rows = stmt
                .query_map([file], |r| Ok(Binding { local: r.get(0)?, name: r.get(1)?, module: r.get(2)? }))?
                .collect::<rusqlite::Result<_>>()?;
            self.bindings.insert(file.to_string(), rows);
        }
        Ok(&self.bindings[file])
    }

    fn paths(&mut self, conn: &Connection) -> Result<&HashSet<String>> {
        if self.paths.is_none() {
            let mut stmt = conn.prepare("SELECT path FROM files WHERE lang = 'python'")?;
            self.paths = Some(stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?);
        }
        Ok(self.paths.as_ref().expect("loaded"))
    }

    /// Where `module`, imported from `file`, is.
    fn place(&mut self, conn: &Connection, file: &str, module: &str) -> Result<Place> {
        let dir = dir_of(file);
        let key = (dir.to_string(), module.to_string());
        if let Some(p) = self.places.get(&key) {
            return Ok(p.clone());
        }
        let p = self.place_uncached(conn, dir, module)?;
        self.places.insert(key, p.clone());
        Ok(p)
    }

    fn place_uncached(&mut self, conn: &Connection, dir: &str, module: &str) -> Result<Place> {
        let paths = self.paths(conn)?;
        let rest = module.trim_start_matches('.');
        let dots = module.len() - rest.len();
        let path = rest.replace('.', "/");
        if dots > 0 {
            // `.b` is in the importer's package, `..b` in its parent.
            let mut base: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
            for _ in 1..dots {
                if base.pop().is_none() {
                    return Ok(Place::Unknown);
                }
            }
            let base = base.join("/");
            let at = [&base, &path].iter().filter(|p| !p.is_empty()).map(|p| p.as_str()).collect::<Vec<_>>().join("/");
            let cands = if path.is_empty() { vec![join(&at, "__init__.py")] } else { vec![format!("{at}.py"), join(&at, "__init__.py")] };
            return Ok(cands.into_iter().find(|c| paths.contains(c)).map_or(Place::Unknown, |f| Place::Files(vec![f], true)));
        }
        if path.is_empty() {
            return Ok(Place::Unknown);
        }
        let (file, init) = (format!("{path}.py"), format!("{path}/__init__.py"));
        let mut found: Vec<&String> = paths
            .iter()
            .filter(|p| [&file, &init].iter().any(|e| p.as_str() == e.as_str() || p.ends_with(&format!("/{e}"))))
            .collect();
        found.sort();
        // The source root each match implies: the repo's, or one above the importer, is plausible.
        let root_of = |p: &str| -> String {
            let tail = if p == init || p.ends_with(&format!("/{init}")) { init.len() } else { file.len() };
            p[..p.len() - tail].trim_end_matches('/').to_string()
        };
        let at_root: Vec<&String> = found.iter().copied().filter(|p| root_of(p).is_empty()).collect();
        let above: Vec<&String> = found
            .iter()
            .copied()
            .filter(|p| {
                let r = root_of(p);
                !r.is_empty() && (dir == r || dir.starts_with(&format!("{r}/")))
            })
            .collect();
        if at_root.len() == 1 {
            return Ok(Place::Files(vec![at_root[0].clone()], true));
        }
        if at_root.is_empty() && above.len() == 1 {
            return Ok(Place::Files(vec![above[0].clone()], true));
        }
        if !found.is_empty() {
            return Ok(Place::Files(found.into_iter().cloned().collect(), false));
        }
        // A namespace package (a directory without `__init__.py`) holding modules.
        let inside = format!("{path}/");
        if paths.iter().any(|p| p.starts_with(&inside) || p.contains(&format!("/{inside}"))) {
            return Ok(Place::Unknown);
        }
        Ok(Place::External)
    }

    /// `files` and the files they take `name` from (`from .x import name`, `from .x import *`), a few levels deep.
    fn source_files(&mut self, conn: &Connection, files: Vec<String>, name: &str) -> Result<Vec<String>> {
        let mut out = files;
        let mut i = 0;
        while i < out.len() && out.len() < 16 {
            let cur = out[i].clone();
            let from: Vec<Binding> =
                self.file_bindings(conn, &cur)?.iter().filter(|b| (b.local == name && b.name == name) || b.local == "*").cloned().collect();
            for b in from {
                if let Place::Files(fs, _) = self.place(conn, &cur, &b.module)? {
                    for f in fs {
                        if !out.contains(&f) {
                            out.push(f);
                        }
                    }
                }
            }
            i += 1;
        }
        Ok(out)
    }

    /// The scope a module place gives `name`.
    fn in_module(&mut self, conn: &Connection, place: Place, name: &str) -> Result<Option<Scope>> {
        Ok(match place {
            Place::External => Some(Scope::External),
            Place::Files(files, sure) => Some(Scope::Module { files: self.source_files(conn, files, name)?, name: name.to_string(), sure }),
            Place::Unknown => None,
        })
    }

    /// Where the import rules put a reference from a Python file: `None` when they say nothing (the name isn't
    /// imported, names a module, or comes from one that can't be placed).
    pub(crate) fn scope(&mut self, conn: &Connection, file: &str, name: &str, receiver: Option<&str>) -> Result<Option<Scope>> {
        match receiver {
            None => {
                let Some(b) = self.file_bindings(conn, file)?.iter().find(|b| b.local == name && b.name != "*").cloned() else {
                    return Ok(None);
                };
                // `from pkg import mod`: a submodule, not a name declared in `pkg`.
                if matches!(self.place(conn, file, &submodule(&b.module, &b.name))?, Place::Files(..)) {
                    return Ok(None);
                }
                let place = self.place(conn, file, &b.module)?;
                self.in_module(conn, place, &b.name)
            }
            Some(r) => {
                let Some(b) = self.file_bindings(conn, file)?.iter().find(|b| b.local == r && b.local != "*").cloned() else {
                    return Ok(None);
                };
                // `import pkg.mod` / `import mod as m`, or `from pkg import mod`.
                let module = if b.name == "*" { b.module.clone() } else { submodule(&b.module, &b.name) };
                let place = self.place(conn, file, &module)?;
                if b.name != "*" && !matches!(place, Place::Files(..)) {
                    // A name imported from a module: the reference is a member of whatever it is.
                    return Ok(None);
                }
                self.in_module(conn, place, name)
            }
        }
    }
}

/// `pkg` + `mod` → `pkg.mod`; `.` + `mod` → `.mod`.
fn submodule(module: &str, name: &str) -> String {
    if module.ends_with('.') { format!("{module}{name}") } else { format!("{module}.{name}") }
}

fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

fn join(dir: &str, file: &str) -> String {
    if dir.is_empty() { file.to_string() } else { format!("{dir}/{file}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submodule_names() {
        assert_eq!(submodule("pkg", "mod"), "pkg.mod");
        assert_eq!(submodule(".", "mod"), ".mod");
        assert_eq!(submodule("..", "mod"), "..mod");
        assert_eq!(submodule(".pkg", "mod"), ".pkg.mod");
    }
}
