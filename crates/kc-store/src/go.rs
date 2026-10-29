//! Go resolution: a name binds the way the Go compiler binds it, or it stays unknown.
//!
//! Go's scoping leaves little to guess, so a Go reference is never resolved by a name match alone:
//!
//! - **A package** is the files of one directory that share a package clause (`FileFacts::package`). A test file
//!   (`_test.go`) belongs to its directory's package, or with a `_test` clause to the external test package, and is
//!   seen only by tests.
//! - **A bare name** (`NewCart()`, `Cart{}`, `defaultCart`) is a package-level declaration of the caller's package, or
//!   else a builtin or a dot import's: never another package's.
//! - **A qualified name** (`shop.NewCart()`) names an import. The import path is placed through the repo's `go.mod`
//!   files: inside the caller's module, the directory under the module root; inside another module of the repo, only
//!   when the caller's `go.mod` replaces it with that directory; anything else (the standard library, a dependency, a
//!   released copy of the repo's own module) is outside the repo.
//! - **A member** (`c.Total()`, `s.cfg.Get()`) is looked up on its receiver's type: the methods and fields declared
//!   for that type in its package, then those promoted from embedded fields and interfaces, nearest first. A receiver
//!   whose type is outside the repo, or embeds an outside type that could supply the member, makes it outside.
//! - **A receiver of unknown type** lists the repo's methods and fields of that name as candidates (`ambiguous`),
//!   never one linked.
//!
//! What the syntax can't tell stays unknown: build-tag variants of one declaration are candidates of each other, and
//! a type's method set is never inferred from an interface it might satisfy (that is dispatch, read elsewhere).

use std::collections::HashMap;

use anyhow::Result;
use kc_extract::{PathRoot, Seg, TypeExpr};
use rusqlite::Connection;

use crate::query::RefHit;

/// How deep embedded types are followed, and type expressions evaluated.
const MAX_DEPTH: usize = 8;

#[derive(Debug, Clone)]
struct GoFile {
    dir: String,
    package: String,
    test: bool,
    /// The default build leaves the file out (its extraction says so, area `build`).
    other_build: bool,
}

#[derive(Debug, Clone)]
struct Module {
    dir: String,
    path: String,
    /// `replace` directives with a directory: the module path and the directory, relative to the repo root.
    replaced: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
struct Sym {
    id: i64,
    kind: String,
    container: Option<String>,
    file: String,
    type_name: Option<String>,
}

/// A type, as resolution places it.
#[derive(Debug, Clone, PartialEq)]
enum Ty {
    /// The repo's type declaration: its symbol, name and file.
    Repo { id: i64, name: String, file: String },
    /// A slice, array or map, by its element (a map's value) type.
    Seq(Box<Ty>, Option<Box<Ty>>),
    /// Declared outside the repo.
    Outside,
    /// Not known.
    Unknown,
}

/// Where a package is.
#[derive(Debug, Clone, PartialEq)]
enum Place {
    /// A directory of the repo, and the package clause its non-test files share.
    Dir(String, String),
    Outside,
}

/// Per-resolution caches of the repo's Go files, modules, imports and declarations.
#[derive(Default)]
pub(crate) struct Modules {
    files: Option<HashMap<String, GoFile>>,
    modules: Option<Vec<Module>>,
    imports: HashMap<String, Vec<(String, String)>>,
    by_name: HashMap<String, Vec<Sym>>,
    embeds: HashMap<i64, Vec<String>>,
}

impl Modules {
    fn files(&mut self, conn: &Connection) -> Result<&HashMap<String, GoFile>> {
        if self.files.is_none() {
            let mut stmt = conn.prepare(
                "SELECT f.path, f.package, EXISTS(SELECT 1 FROM unsupported u WHERE u.file_id = f.id AND u.area = 'build')
                 FROM files f WHERE f.lang = 'go'",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, bool>(2)?)))?;
            let mut out = HashMap::new();
            for row in rows {
                let (path, package, other_build) = row?;
                let dir = path.rsplit_once('/').map_or(String::new(), |(d, _)| d.to_string());
                let test = path.ends_with("_test.go");
                out.insert(path, GoFile { dir, package: package.unwrap_or_default(), test, other_build });
            }
            self.files = Some(out);
        }
        Ok(self.files.as_ref().expect("loaded"))
    }

    fn modules(&mut self, conn: &Connection) -> Result<&[Module]> {
        if self.modules.is_none() {
            let mut stmt = conn.prepare(
                "SELECT p.id, p.name, f.path FROM packages p JOIN files f ON f.id = p.file_id WHERE p.kind = 'gomod'",
            )?;
            let rows: Vec<(i64, String, String)> =
                stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
            let mut out = Vec::new();
            for (id, path, file) in rows {
                let dir = file.rsplit_once('/').map_or(String::new(), |(d, _)| d.to_string());
                let mut deps = conn.prepare_cached(
                    "SELECT name, location FROM package_deps WHERE package_id = ?1 AND source = 'path'",
                )?;
                let replaced = deps
                    .query_map([id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>()?
                    .into_iter()
                    .filter_map(|(m, loc)| normalize(&join(&dir, &loc)).map(|d| (m, d)))
                    .collect();
                out.push(Module { dir, path, replaced });
            }
            // Innermost first, so a file's module is the first whose directory holds it.
            out.sort_by_key(|m| std::cmp::Reverse(m.dir.len()));
            self.modules = Some(out);
        }
        Ok(self.modules.as_deref().expect("loaded"))
    }

    fn imports(&mut self, conn: &Connection, file: &str) -> Result<&[(String, String)]> {
        if !self.imports.contains_key(file) {
            let mut stmt = conn.prepare_cached(
                "SELECT b.local, b.module FROM import_bindings b JOIN files f ON f.id = b.file_id WHERE f.path = ?1",
            )?;
            let rows = stmt.query_map([file], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
            self.imports.insert(file.to_string(), rows);
        }
        Ok(&self.imports[file])
    }

    fn named(&mut self, conn: &Connection, name: &str) -> Result<&[Sym]> {
        if !self.by_name.contains_key(name) {
            let mut stmt = conn.prepare_cached(
                "SELECT s.id, s.kind, s.container, f.path, s.type_name FROM symbols s JOIN files f ON f.id = s.file_id
                 WHERE s.name = ?1 AND s.name = ?1 COLLATE BINARY AND f.lang = 'go'
                 ORDER BY f.path, s.start_byte",
            )?;
            let rows = stmt
                .query_map([name], |r| {
                    Ok(Sym { id: r.get(0)?, kind: r.get(1)?, container: r.get(2)?, file: r.get(3)?, type_name: r.get(4)? })
                })?
                .collect::<rusqlite::Result<_>>()?;
            self.by_name.insert(name.to_string(), rows);
        }
        Ok(&self.by_name[name])
    }

    fn embedded(&mut self, conn: &Connection, type_id: i64) -> Result<Vec<String>> {
        if let std::collections::hash_map::Entry::Vacant(e) = self.embeds.entry(type_id) {
            let mut stmt = conn.prepare_cached("SELECT target FROM relations WHERE from_id = ?1 AND kind = 'inherits' ORDER BY line")?;
            let rows = stmt.query_map([type_id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
            e.insert(rows);
        }
        Ok(self.embeds[&type_id].clone())
    }

    /// The package `file` declares into: its directory, its clause, and whether it is a test file.
    fn package_of(&mut self, conn: &Connection, file: &str) -> Result<Option<GoFile>> {
        Ok(self.files(conn)?.get(file).cloned())
    }

    /// Whether a declaration in `decl` is visible, unqualified, to code in `from`: the same package, where only a
    /// test sees a test file's declarations.
    fn same_package(&mut self, conn: &Connection, from: &str, decl: &str) -> Result<bool> {
        let files = self.files(conn)?;
        let (Some(a), Some(b)) = (files.get(from), files.get(decl)) else { return Ok(false) };
        Ok(a.dir == b.dir && a.package == b.package && (a.test || !b.test))
    }

    /// Whether `decl` declares into the package at `dir` with clause `package`, as an importer sees it (test files
    /// are never imported).
    fn in_package(&mut self, conn: &Connection, decl: &str, dir: &str, package: &str) -> Result<bool> {
        let files = self.files(conn)?;
        Ok(files.get(decl).is_some_and(|f| f.dir == dir && f.package == package && !f.test))
    }

    /// Where an import path of `file` is.
    fn place(&mut self, conn: &Connection, file: &str, import: &str) -> Result<Place> {
        let modules = self.modules(conn)?.to_vec();
        let Some(own) = modules.iter().find(|m| under(&m.dir, file)) else { return Ok(Place::Outside) };
        let dir = if let Some(rest) = strip_module(import, &own.path) {
            Some(join(&own.dir, rest))
        } else {
            own.replaced.iter().find_map(|(m, d)| strip_module(import, m).map(|rest| join(d, rest)))
        };
        let Some(dir) = dir else { return Ok(Place::Outside) };
        // The package clause of the directory's non-test files; a directory with none isn't a package of the repo.
        let files = self.files(conn)?;
        let mut clauses: Vec<&str> = files.values().filter(|f| f.dir == dir && !f.test).map(|f| f.package.as_str()).collect();
        clauses.sort_unstable();
        clauses.dedup();
        Ok(match clauses.as_slice() {
            [one] => Place::Dir(dir, one.to_string()),
            // No Go files there (generated, or not committed), or several clauses (one of them build-constrained):
            // the repo's, but not placeable.
            _ => Place::Dir(dir, String::new()),
        })
    }

    /// The package an import qualifier `q` of `file` names, when it is one.
    fn qualifier(&mut self, conn: &Connection, file: &str, q: &str) -> Result<Option<Place>> {
        let imports = self.imports(conn, file)?.to_vec();
        for (local, path) in &imports {
            if local == q {
                return self.place(conn, file, path).map(Some);
            }
        }
        for (_, path) in imports.iter().filter(|(l, _)| l.is_empty()) {
            match self.place(conn, file, path)? {
                Place::Dir(d, clause) if clause == q => return Ok(Some(Place::Dir(d, clause))),
                // The package's own name decides; a package without one placed is matched by its path.
                Place::Dir(d, clause) if clause.is_empty() && kc_extract::go_package_name_guess(path) == q => {
                    return Ok(Some(Place::Dir(d, clause)));
                }
                Place::Outside if kc_extract::go_package_name_guess(path) == q => return Ok(Some(Place::Outside)),
                _ => {}
            }
        }
        Ok(None)
    }

    /// Package-level declarations named `name` visible bare from `file`, of the kinds `kinds`.
    fn bare(&mut self, conn: &Connection, file: &str, name: &str, kinds: &[&str]) -> Result<Vec<Sym>> {
        let syms = self.named(conn, name)?.to_vec();
        let mut out = Vec::new();
        for s in syms.into_iter().filter(|s| s.container.is_none() && kinds.contains(&s.kind.as_str())) {
            if self.same_package(conn, file, &s.file)? {
                out.push(s);
            }
        }
        Ok(self.default_build(out))
    }

    /// The declarations of the default build: one in a file the default build leaves out (`x_windows.go`, `//go:build
    /// nomsgpack`) gives way to any other. Alone it stands, as the only declaration there is.
    fn default_build(&self, syms: Vec<Sym>) -> Vec<Sym> {
        if syms.len() < 2 {
            return syms;
        }
        let other = |f: &str| self.files.as_ref().and_then(|m| m.get(f)).is_some_and(|f| f.other_build);
        let kept: Vec<Sym> = syms.iter().filter(|s| !other(&s.file)).cloned().collect();
        if kept.is_empty() { syms } else { kept }
    }

    /// Package-level declarations named `name` of the package at `dir` with clause `package`.
    fn exported(&mut self, conn: &Connection, dir: &str, package: &str, name: &str, kinds: &[&str]) -> Result<Vec<Sym>> {
        let syms = self.named(conn, name)?.to_vec();
        let mut out = Vec::new();
        for s in syms.into_iter().filter(|s| s.container.is_none() && kinds.contains(&s.kind.as_str())) {
            let found = if package.is_empty() {
                self.files(conn)?.get(&s.file).is_some_and(|f| f.dir == dir && !f.test)
            } else {
                self.in_package(conn, &s.file, dir, package)?
            };
            if found {
                out.push(s);
            }
        }
        Ok(self.default_build(out))
    }

    /// The type a written type name (`T`, `pkg.T`) names in `file`.
    fn type_named(&mut self, conn: &Connection, file: &str, written: &str, depth: usize) -> Result<Ty> {
        if depth > MAX_DEPTH {
            return Ok(Ty::Unknown);
        }
        let found = match written.rsplit_once('.') {
            Some((q, name)) => match self.qualifier(conn, file, q)? {
                Some(Place::Dir(d, p)) => self.exported(conn, &d, &p, name, TYPES)?,
                Some(Place::Outside) => return Ok(Ty::Outside),
                None => return Ok(Ty::Unknown),
            },
            None => {
                let local = self.bare(conn, file, written, TYPES)?;
                if local.is_empty() {
                    // A predeclared type or one of a dot import's: either way not the repo's declaration.
                    return Ok(if self.has_dot_import(conn, file)? { Ty::Unknown } else { Ty::Outside });
                }
                local
            }
        };
        match found.as_slice() {
            [s] if s.kind == "typealias" => match s.type_name.as_deref().and_then(TypeExpr::decode) {
                Some(t) => self.eval(conn, &s.file.clone(), &t, depth + 1),
                None => Ok(Ty::Unknown),
            },
            [s] => Ok(Ty::Repo { id: s.id, name: written.rsplit('.').next().unwrap_or(written).to_string(), file: s.file.clone() }),
            [] => Ok(Ty::Outside),
            _ => Ok(Ty::Unknown),
        }
    }

    fn has_dot_import(&mut self, conn: &Connection, file: &str) -> Result<bool> {
        Ok(self.imports(conn, file)?.iter().any(|(l, _)| l == "."))
    }

    /// A type expression written in `file`, evaluated.
    fn eval(&mut self, conn: &Connection, file: &str, t: &TypeExpr, depth: usize) -> Result<Ty> {
        if depth > MAX_DEPTH {
            return Ok(Ty::Unknown);
        }
        Ok(match t {
            TypeExpr::Named(n) if n.is_empty() => Ty::Unknown,
            TypeExpr::Named(n) | TypeExpr::Generic { name: n, .. } => self.type_named(conn, file, n, depth + 1)?,
            TypeExpr::Array(e) => Ty::Seq(Box::new(self.eval(conn, file, e, depth + 1)?), None),
            TypeExpr::Dict(k, v) => Ty::Seq(Box::new(self.eval(conn, file, v, depth + 1)?), Some(Box::new(self.eval(conn, file, k, depth + 1)?))),
            TypeExpr::Elem(e) => match self.eval(conn, file, e, depth + 1)? {
                Ty::Seq(v, _) => *v,
                Ty::Outside => Ty::Outside,
                _ => Ty::Unknown,
            },
            TypeExpr::Call { owner: None, name } => self.result_of(conn, file, None, name, depth)?,
            TypeExpr::Call { owner: Some(q), name } => self.result_of(conn, file, Some(q), name, depth)?,
            TypeExpr::Path { root, segs } => {
                let mut ty = match root {
                    PathRoot::Prop(name) => self.value_type(conn, file, name, depth)?,
                    PathRoot::Expr(e) => self.eval(conn, file, e, depth + 1)?,
                    PathRoot::Static(n) => self.type_named(conn, file, n, depth + 1)?,
                    PathRoot::SelfType => Ty::Unknown,
                };
                for seg in segs {
                    ty = match (seg, ty) {
                        (Seg::Member(m), t) | (Seg::Call(m), t) => match self.member(conn, &t, m, 0)? {
                            Found::One(s) => match s.type_name.as_deref().and_then(TypeExpr::decode) {
                                Some(te) => self.eval(conn, &s.file, &te, depth + 1)?,
                                None => Ty::Unknown,
                            },
                            Found::Outside => Ty::Outside,
                            _ => Ty::Unknown,
                        },
                        (Seg::Index, Ty::Seq(v, _)) => *v,
                        (Seg::Index, Ty::Outside) => Ty::Outside,
                        (Seg::Index, _) => Ty::Unknown,
                    };
                }
                ty
            }
            _ => Ty::Unknown,
        })
    }

    /// The type of a package-level variable `name` (`v`, or `pkg.v`) as `file` sees it; a type's name stands for the
    /// type itself (a method expression `T.M`).
    fn value_type(&mut self, conn: &Connection, file: &str, name: &str, depth: usize) -> Result<Ty> {
        let found = match name.rsplit_once('.') {
            Some((q, n)) => match self.qualifier(conn, file, q)? {
                Some(Place::Dir(d, p)) => self.exported(conn, &d, &p, n, &["property", "struct", "protocol", "typealias"])?,
                Some(Place::Outside) => return Ok(Ty::Outside),
                None => return Ok(Ty::Unknown),
            },
            None => self.bare(conn, file, name, &["property", "struct", "protocol", "typealias"])?,
        };
        match found.as_slice() {
            [s] if TYPES.contains(&s.kind.as_str()) => self.type_named(conn, &s.file.clone(), name.rsplit('.').next().unwrap_or(name), depth + 1),
            [s] => match s.type_name.as_deref().and_then(TypeExpr::decode) {
                Some(t) => self.eval(conn, &s.file.clone(), &t, depth + 1),
                None => Ok(Ty::Unknown),
            },
            _ => Ok(Ty::Unknown),
        }
    }

    /// What calling `name` (or `q.name`) in `file` gives: a function's declared result, or a conversion's type.
    fn result_of(&mut self, conn: &Connection, file: &str, q: Option<&str>, name: &str, depth: usize) -> Result<Ty> {
        let found = match q {
            Some(q) => match self.qualifier(conn, file, q)? {
                Some(Place::Dir(d, p)) => self.exported(conn, &d, &p, name, CALLABLE)?,
                Some(Place::Outside) => return Ok(Ty::Outside),
                None => return Ok(Ty::Unknown),
            },
            None => self.bare(conn, file, name, CALLABLE)?,
        };
        match found.as_slice() {
            [s] if TYPES.contains(&s.kind.as_str()) => self.type_named(conn, &s.file.clone(), name, depth + 1),
            [s] => match s.type_name.as_deref().and_then(TypeExpr::decode) {
                Some(t) => self.eval(conn, &s.file.clone(), &t, depth + 1),
                None => Ok(Ty::Unknown),
            },
            [] if q.is_none() && !self.has_dot_import(conn, file)? => Ok(Ty::Outside),
            _ => Ok(Ty::Unknown),
        }
    }

    /// The member `name` of type `ty`: declared for it, else promoted from what it embeds, nearest first.
    fn member(&mut self, conn: &Connection, ty: &Ty, name: &str, depth: usize) -> Result<Found> {
        let (id, tname, file) = match ty {
            Ty::Repo { id, name, file } => (*id, name.clone(), file.clone()),
            Ty::Outside => return Ok(Found::Outside),
            _ => return Ok(Found::Unknown),
        };
        if depth > MAX_DEPTH {
            return Ok(Found::Unknown);
        }
        let (dir, package) = match self.package_of(conn, &file)? {
            Some(f) => (f.dir, f.package),
            None => return Ok(Found::Unknown),
        };
        let syms = self.named(conn, name)?.to_vec();
        let own: Vec<Sym> = syms
            .into_iter()
            .filter(|s| s.container.as_deref() == Some(tname.as_str()))
            .filter(|s| self.files.as_ref().and_then(|f| f.get(&s.file)).is_some_and(|f| f.dir == dir && f.package == package))
            .collect();
        let own = self.default_build(own);
        match own.len() {
            1 => return Ok(Found::One(own.into_iter().next().expect("one"))),
            0 => {}
            _ => return Ok(Found::Several(own)),
        }
        // A defined type (`type Celsius float64`, `type Handlers []Handler`) has its own methods only; a struct's
        // and an interface's embedded types promote theirs.
        let mut found: Vec<Sym> = Vec::new();
        let mut outside = false;
        let mut unknown = false;
        for target in self.embedded(conn, id)? {
            let t = self.type_named(conn, &file, &target, depth + 1)?;
            match self.member(conn, &t, name, depth + 1)? {
                Found::One(s) => found.push(s),
                Found::Several(s) => found.extend(s),
                Found::Outside => outside = true,
                Found::Unknown => unknown = true,
                Found::None => {}
            }
        }
        // Promotion picks the shallowest; at equal depth two are ambiguous (the compiler rejects the use), and an
        // embedded type the repo can't read may hold it too.
        Ok(match (found.len(), outside || unknown) {
            (1, false) => Found::One(found.into_iter().next().expect("one")),
            (0, false) => Found::None,
            (0, true) if outside && !unknown => Found::Outside,
            (0, true) => Found::Unknown,
            _ => Found::Several(found),
        })
    }

    /// The Go rules' answer for `hit`, a reference in a `.go` file: always one, so a Go name is never resolved by the
    /// name rules (see the module docs).
    pub(crate) fn resolve(&mut self, conn: &Connection, hit: &RefHit) -> Result<(&'static str, Vec<i64>)> {
        let kinds: &[&str] = match hit.kind.as_str() {
            "type_ref" | "instantiate" => TYPES,
            "call" => CALLABLE,
            _ => VALUES,
        };
        let typed = hit.receiver_type.as_deref().and_then(TypeExpr::decode);
        // A keyed field of a struct literal (`Cart{total: 1}`): a member of the literal's type.
        if hit.receiver.is_none() {
            if let Some(t) = typed {
                let ty = self.eval(conn, &hit.file, &t, 0)?;
                return self.answer(conn, hit, ty);
            }
            let found = self.bare(conn, &hit.file, &hit.name, kinds)?;
            return Ok(match found.len() {
                0 => ("unresolved", Vec::new()),
                1 => ("exact", ids(&found)),
                _ => ("ambiguous", ids(&found)),
            });
        }
        let receiver = hit.receiver.as_deref().unwrap_or_default();
        if typed.is_none() && is_ident(receiver) {
            match self.qualifier(conn, &hit.file, receiver)? {
                Some(Place::Outside) => return Ok(("unresolved", Vec::new())),
                Some(Place::Dir(d, p)) => {
                    let found = self.exported(conn, &d, &p, &hit.name, kinds)?;
                    return Ok(match found.len() {
                        0 => ("unresolved", Vec::new()),
                        1 => ("exact", ids(&found)),
                        _ => ("ambiguous", ids(&found)),
                    });
                }
                None => {}
            }
        }
        let ty = match typed {
            Some(t) => self.eval(conn, &hit.file, &t, 0)?,
            None => Ty::Unknown,
        };
        self.answer(conn, hit, ty)
    }

    fn answer(&mut self, conn: &Connection, hit: &RefHit, ty: Ty) -> Result<(&'static str, Vec<i64>)> {
        Ok(match self.member(conn, &ty, &hit.name, 0)? {
            Found::One(s) => ("exact", vec![s.id]),
            Found::Several(s) => ("ambiguous", ids(&s)),
            Found::Outside | Found::None => ("unresolved", Vec::new()),
            Found::Unknown => {
                // The receiver's type isn't known: the repo's members of that name are candidates.
                let found: Vec<Sym> = self.named(conn, &hit.name)?.iter().filter(|s| s.container.is_some()).cloned().collect();
                if found.is_empty() { ("unresolved", Vec::new()) } else { ("ambiguous", ids(&found)) }
            }
        })
    }
}

/// What looking up a member found.
enum Found {
    One(Sym),
    Several(Vec<Sym>),
    /// Not the repo's: the type, or an embedded type that may hold it, is outside.
    Outside,
    /// The type, or something it embeds, isn't known.
    Unknown,
    None,
}

const TYPES: &[&str] = &["struct", "protocol", "typealias"];
const CALLABLE: &[&str] = &["function", "method", "struct", "protocol", "typealias", "property"];
/// A bare name used as a value: a variable, constant or function, or a type in a conversion (`(*T)(nil)` reads as a
/// dereference of `T`).
const VALUES: &[&str] = &["property", "function", "method", "struct", "protocol", "typealias"];

fn ids(syms: &[Sym]) -> Vec<i64> {
    syms.iter().map(|s| s.id).collect()
}

fn is_ident(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// `import` minus the module path `module` when the import is in that module: the rest of the path, maybe empty.
fn strip_module<'a>(import: &'a str, module: &str) -> Option<&'a str> {
    if import == module {
        return Some("");
    }
    import.strip_prefix(module).and_then(|r| r.strip_prefix('/'))
}

fn join(dir: &str, rest: &str) -> String {
    match (dir.is_empty(), rest.is_empty()) {
        (true, _) => rest.to_string(),
        (_, true) => dir.to_string(),
        _ => format!("{dir}/{rest}"),
    }
}

/// `a/b/../c` as `a/c`; `None` when it leaves the repo.
fn normalize(path: &str) -> Option<String> {
    let mut out: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                out.pop()?;
            }
            p => out.push(p),
        }
    }
    Some(out.join("/"))
}

fn under(dir: &str, file: &str) -> bool {
    dir.is_empty() || file.strip_prefix(dir).is_some_and(|r| r.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_paths_and_directories() {
        assert_eq!(strip_module("github.com/example/shop/v2/cart", "github.com/example/shop/v2"), Some("cart"));
        assert_eq!(strip_module("github.com/example/shop/v2", "github.com/example/shop/v2"), Some(""));
        assert_eq!(strip_module("github.com/example/shopping", "github.com/example/shop"), None);
        assert_eq!(normalize("tools/../kit"), Some("kit".into()));
        assert_eq!(normalize("../kit"), None);
        assert!(under("", "cart.go") && under("a", "a/b.go") && !under("a", "ab/c.go"));
    }
}
