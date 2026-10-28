//! TypeScript module scoping for resolution: a name a file imports comes from the module it names, so it resolves in
//! that module's file, or, when the module is a package from outside the repo (`rxjs`, `@angular/core`), not in the
//! repo at all.
//!
//! A module is resolved the way the compiler would, as far as the index can tell: a relative path from the importing
//! file (`./cart.actions` is `cart.actions.ts` or `cart.actions/index.ts`); else, by the nearest `tsconfig.json` (or
//! `jsconfig.json`) above the file and the configs it `extends`, a `paths` pattern (`*` substituted, each
//! substitution tried in order) or a path from `baseUrl`. A module those don't place is a package when the repo's
//! `package.json` declares it, and left to the name-based rules when the repo itself provides it (a workspace
//! library). One a config maps to no indexed file is unplaced: it is never guessed.
//!
//! Only when no config maps a module (there is none, or a bundler's alias the compiler never sees) is it placed by
//! guesswork: a path from a directory above the file (`src/app/…`), or an alias's tail (`@app/core/x`) naming one
//! repo file. A name resolved through a guess is at most `unique_name`, never `exact`.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use rusqlite::Connection;

/// Built-in objects of browsers and Node, used through their own name (`console.log`, `window.open`): their members
/// are never the repo's.
const GLOBAL_OBJECTS: &[&str] = &[
    "console", "document", "globalThis", "history", "localStorage", "location", "navigator", "performance", "process",
    "screen", "sessionStorage", "window",
];

/// What the module a name comes from is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Origin {
    /// A file in the repo, as the compiler would resolve it.
    File(String),
    /// A file in the repo by guesswork: no config maps the module, but a directory above the importer or the module's
    /// tail names this file.
    Guess(String),
    /// A package from outside the repo.
    External,
    /// A config maps the module, but to no file the index has (generated, or outside the repo).
    Missing,
    /// Can't tell (a workspace library, or a relative path to a file the index doesn't have).
    Unknown,
}

/// Where a TypeScript reference's name must be defined, by the file's imports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Scope {
    /// Outside the repo: an imported package's export, a member of one, or a member of a built-in object.
    External,
    /// A top-level declaration named `name` in one of `files` (the module's file and the files it re-exports from),
    /// or, when none of them declares it (a re-export the index can't follow), any top-level declaration named so.
    /// `guessed` are those of `files` placed by guesswork (see [`Origin::Guess`]).
    Module { files: Vec<String>, guessed: Vec<String>, name: String },
    /// Imported from a module a config maps to no indexed file: the name can't be placed.
    Unplaced,
}

/// A config's `paths` patterns, each with its substitutions, and a directory they are relative to.
type Paths = (Vec<(String, Vec<String>)>, String);

/// `baseUrl` and `paths` (with the directory of the config setting them), as a config and those it extends set them.
type Raw = (Option<String>, Option<Paths>);

/// The module resolution settings in force for a file, from its nearest config and the configs it extends; paths are
/// repo-relative.
#[derive(Debug, Clone, Default)]
struct Settings {
    /// `baseUrl`'s directory.
    base_url: Option<String>,
    /// `paths`, and the directory its substitutions are relative to (`baseUrl`'s, else the defining config's).
    paths: Option<Paths>,
}

/// How a config places a module.
enum Mapped {
    /// To this file.
    File(String),
    /// A `paths` pattern matches it, but no substitution (nor `baseUrl`) names an indexed file.
    Missing,
    /// It says nothing about the module.
    No,
}

#[derive(Debug, Clone)]
struct Binding {
    local: String,
    name: String,
    module: String,
    export: bool,
}

/// Per-pass caches of import bindings and module resolutions.
#[derive(Default)]
pub(crate) struct Modules {
    bindings: HashMap<String, Vec<Binding>>,
    origins: HashMap<(String, String), Origin>,
    exports: HashMap<(String, String), Vec<(String, bool)>>,
    paths: Option<HashSet<String>>,
    /// The repo's configs, by path.
    configs: Option<HashMap<String, kc_extract::TsConfig>>,
    /// Per directory, the settings in force there (several for a solution-style config's references).
    in_force: HashMap<String, Vec<Settings>>,
    /// npm packages the repo depends on from a registry, and those it provides itself.
    packages: Option<(HashSet<String>, HashSet<String>)>,
    /// Per file, whether it is a script (no import or export): its top-level declarations are global.
    scripts: HashMap<String, bool>,
}

/// A TypeScript or JavaScript module's path, by its extension: its imports resolve through [`Modules`].
pub fn is_ts_path(path: &str) -> bool {
    matches!(path.rsplit_once('.').map(|(_, e)| e), Some("ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs"))
}

impl Modules {
    fn file_bindings(&mut self, conn: &Connection, file: &str) -> Result<&[Binding]> {
        if !self.bindings.contains_key(file) {
            let mut stmt = conn.prepare_cached(
                "SELECT b.local, b.name, b.module, b.export FROM import_bindings b JOIN files f ON f.id = b.file_id WHERE f.path = ?1",
            )?;
            let rows = stmt
                .query_map([file], |r| Ok(Binding { local: r.get(0)?, name: r.get(1)?, module: r.get(2)?, export: r.get(3)? }))?
                .collect::<rusqlite::Result<_>>()?;
            self.bindings.insert(file.to_string(), rows);
        }
        Ok(&self.bindings[file])
    }

    /// The import binding `local` in `file`, if it imports one by that name.
    fn imported(&mut self, conn: &Connection, file: &str, local: &str) -> Result<Option<Binding>> {
        Ok(self.file_bindings(conn, file)?.iter().find(|b| !b.export && b.local == local).cloned())
    }

    fn paths(&mut self, conn: &Connection) -> Result<&HashSet<String>> {
        if self.paths.is_none() {
            let mut stmt = conn.prepare("SELECT path FROM files")?;
            self.paths = Some(stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?);
        }
        Ok(self.paths.as_ref().expect("loaded"))
    }

    fn has_path(&mut self, conn: &Connection, path: &str) -> Result<bool> {
        Ok(self.paths(conn)?.contains(path))
    }

    /// The repo file a module path names (`base` joined with it, with an extension or as a directory's index).
    fn file_at(&mut self, conn: &Connection, base: &str) -> Result<Option<String>> {
        let stem = base.strip_suffix(".js").unwrap_or(base);
        // TypeScript first (`./x.js` in TypeScript names `x.ts`), then plain JavaScript.
        for cand in [
            format!("{stem}.ts"),
            format!("{stem}.tsx"),
            format!("{stem}.d.ts"),
            format!("{stem}/index.ts"),
            format!("{stem}/index.tsx"),
            stem.to_string(),
            format!("{stem}.js"),
            format!("{stem}.jsx"),
            format!("{stem}/index.js"),
            base.to_string(),
        ] {
            if is_ts_path(&cand) && self.has_path(conn, &cand)? {
                return Ok(Some(cand));
            }
        }
        Ok(None)
    }

    fn packages(&mut self, conn: &Connection) -> Result<&(HashSet<String>, HashSet<String>)> {
        if self.packages.is_none() {
            let deps: HashSet<String> = conn
                .prepare(
                    "SELECT d.name FROM package_deps d JOIN packages p ON p.id = d.package_id
                     WHERE p.kind = 'npm' AND d.source IN ('registry', 'url')",
                )?
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            let own: HashSet<String> = conn
                .prepare("SELECT name FROM packages WHERE kind = 'npm'")?
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            self.packages = Some((deps, own));
        }
        Ok(self.packages.as_ref().expect("loaded"))
    }

    /// What `module`, imported from `file`, is.
    pub(crate) fn origin(&mut self, conn: &Connection, file: &str, module: &str) -> Result<Origin> {
        let dir = file.rsplit_once('/').map_or("", |(d, _)| d);
        let key = (dir.to_string(), module.to_string());
        if let Some(o) = self.origins.get(&key) {
            return Ok(o.clone());
        }
        let o = self.origin_uncached(conn, dir, module)?;
        self.origins.insert(key, o.clone());
        Ok(o)
    }

    fn origin_uncached(&mut self, conn: &Connection, dir: &str, module: &str) -> Result<Origin> {
        if module.starts_with('.') {
            return Ok(match normalize(dir, module) {
                Some(base) => self.file_at(conn, &base)?.map_or(Origin::Unknown, Origin::File),
                None => Origin::Unknown,
            });
        }
        // The compiler's rules first: `paths`, then `baseUrl`, by the configs in force.
        let mut found: Vec<String> = Vec::new();
        let mut missing = false;
        for settings in self.settings_at(conn, dir)? {
            match self.mapped(conn, &settings, module)? {
                Mapped::File(f) => {
                    if !found.contains(&f) {
                        found.push(f);
                    }
                }
                Mapped::Missing => missing = true,
                Mapped::No => {}
            }
        }
        match found.as_slice() {
            [one] => return Ok(Origin::File(one.clone())),
            // A solution's projects map it differently.
            [_, _, ..] => return Ok(Origin::Unknown),
            [] => {}
        }
        let package = package_name(module);
        let (deps, own) = self.packages(conn)?;
        if own.contains(package) {
            return Ok(Origin::Unknown);
        }
        if deps.contains(package) {
            return Ok(Origin::External);
        }
        if missing {
            return Ok(Origin::Missing);
        }
        // No config places it: guesswork. A path from a base directory: the repo root, or a directory above the
        // importing file (`src/app/x` from `web/src/app/y`).
        let mut base = Some(dir);
        while let Some(b) = base {
            let path = if b.is_empty() { module.to_string() } else { format!("{b}/{module}") };
            if let Some(f) = self.file_at(conn, &path)? {
                return Ok(Origin::Guess(f));
            }
            base = if b.is_empty() { None } else { Some(dir_of(b)) };
        }
        // A path alias (`@app/core/x`, `@shared/models`): its tail names a file, or several (it can't tell which).
        if let Some((_, tail)) = module.split_once('/') {
            return self.by_tail(conn, tail);
        }
        Ok(Origin::External)
    }

    fn configs(&mut self, conn: &Connection) -> Result<&HashMap<String, kc_extract::TsConfig>> {
        if self.configs.is_none() {
            let mut stmt = conn.prepare("SELECT f.path, c.config FROM ts_configs c JOIN files f ON f.id = c.file_id")?;
            let rows: Vec<(String, String)> = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
            self.configs = Some(rows.into_iter().filter_map(|(p, c)| Some((p, serde_json::from_str(&c).ok()?))).collect());
        }
        Ok(self.configs.as_ref().expect("loaded"))
    }

    /// The config a path written in config `from` (`extends`, `references`) names: the file, with `.json` added, or a
    /// directory's `tsconfig.json`. A package (`@tsconfig/node20`) is never in the index.
    fn config_named(&mut self, conn: &Connection, from: &str, path: &str) -> Result<Option<String>> {
        if !path.starts_with('.') {
            return Ok(None);
        }
        let Some(p) = normalize(dir_of(from), path) else { return Ok(None) };
        let configs = self.configs(conn)?;
        Ok([p.clone(), format!("{p}.json"), format!("{p}/tsconfig.json")].into_iter().find(|c| configs.contains_key(c)))
    }

    /// Config `path`'s `baseUrl` and `paths`, those of the configs it extends applied under its own, each later one
    /// over the earlier.
    fn raw(&mut self, conn: &Connection, path: &str, depth: usize) -> Result<Raw> {
        let Some(config) = self.configs(conn)?.get(path).cloned() else { return Ok((None, None)) };
        let dir = dir_of(path).to_string();
        let (mut base_url, mut paths) = (None, None);
        if depth < 8 {
            for e in &config.extends {
                if let Some(parent) = self.config_named(conn, path, e)? {
                    let (b, p) = self.raw(conn, &parent, depth + 1)?;
                    base_url = b.or(base_url);
                    paths = p.or(paths);
                }
            }
        }
        if let Some(b) = &config.base_url {
            base_url = normalize(&dir, b);
        }
        if let Some(p) = &config.paths {
            paths = Some((p.clone(), dir));
        }
        Ok((base_url, paths))
    }

    /// Config `path`'s settings: `paths` substitutions are relative to the `baseUrl` in force, which a config may set
    /// over the one defining `paths`, else to the config defining them.
    fn settings(&mut self, conn: &Connection, path: &str) -> Result<Settings> {
        let (base_url, paths) = self.raw(conn, path, 0)?;
        Ok(Settings { paths: paths.map(|(p, d)| (p, base_url.clone().unwrap_or(d))), base_url })
    }

    /// The settings in force for files in `dir`: the nearest `tsconfig.json` (or `jsconfig.json`) above it, or, when
    /// that one sets neither `paths` nor `baseUrl` and lists `references` (a solution-style config), those projects'.
    fn settings_at(&mut self, conn: &Connection, dir: &str) -> Result<Vec<Settings>> {
        if let Some(s) = self.in_force.get(dir) {
            return Ok(s.clone());
        }
        let mut nearest = None;
        let mut at = Some(dir);
        while let Some(d) = at {
            let configs = self.configs(conn)?;
            let join = |f: &str| if d.is_empty() { f.to_string() } else { format!("{d}/{f}") };
            if let Some(c) = [join("tsconfig.json"), join("jsconfig.json")].into_iter().find(|c| configs.contains_key(c)) {
                nearest = Some(c);
                break;
            }
            at = if d.is_empty() { None } else { Some(dir_of(d)) };
        }
        let mut out = Vec::new();
        if let Some(c) = nearest {
            let own = self.settings(conn, &c)?;
            if own.paths.is_none() && own.base_url.is_none() {
                let refs = self.configs(conn)?.get(&c).map(|x| x.references.clone()).unwrap_or_default();
                for r in refs {
                    if let Some(p) = self.config_named(conn, &c, &r)? {
                        let s = self.settings(conn, &p)?;
                        if s.paths.is_some() || s.base_url.is_some() {
                            out.push(s);
                        }
                    }
                }
            } else {
                out.push(own);
            }
        }
        self.in_force.insert(dir.to_string(), out.clone());
        Ok(out)
    }

    /// Where `settings` put `module`, as the compiler does: the `paths` pattern matching it (an exact one, else the
    /// one with the longest prefix), each substitution in order, then `baseUrl`.
    fn mapped(&mut self, conn: &Connection, settings: &Settings, module: &str) -> Result<Mapped> {
        let mut matched = false;
        if let Some((paths, base)) = &settings.paths
            && let Some((subs, star)) = match_pattern(paths, module)
        {
            matched = true;
            for sub in subs {
                let target = sub.replacen('*', star, 1);
                if let Some(p) = normalize(base, &target)
                    && let Some(f) = self.file_at(conn, &p)?
                {
                    return Ok(Mapped::File(f));
                }
            }
        }
        if let Some(base) = &settings.base_url
            && let Some(p) = normalize(base, module)
            && let Some(f) = self.file_at(conn, &p)?
        {
            return Ok(Mapped::File(f));
        }
        Ok(if matched { Mapped::Missing } else { Mapped::No })
    }

    /// The repo file whose path ends with `tail` (as a module path): `External` when none does.
    fn by_tail(&mut self, conn: &Connection, tail: &str) -> Result<Origin> {
        let ends: Vec<String> = ["ts", "tsx"].iter().flat_map(|e| [format!("/{tail}.{e}"), format!("/{tail}/index.{e}")]).collect();
        let found: Vec<&String> = self.paths(conn)?.iter().filter(|p| ends.iter().any(|e| p.ends_with(e.as_str()))).collect();
        Ok(match found.as_slice() {
            [] => Origin::External,
            [one] => Origin::Guess(one.to_string()),
            _ => Origin::Unknown,
        })
    }

    /// `file` and the files it re-exports `name` from (`export { name } from`, `export * from`), a few levels deep,
    /// each with whether guesswork placed it (see [`Origin::Guess`]): itself, or a re-export on the way to it.
    fn export_files(&mut self, conn: &Connection, file: &str, name: &str) -> Result<Vec<(String, bool)>> {
        let key = (file.to_string(), name.to_string());
        if let Some(v) = self.exports.get(&key) {
            return Ok(v.clone());
        }
        let mut out = vec![(file.to_string(), false)];
        let mut i = 0;
        while i < out.len() && out.len() < 16 {
            let (cur, cur_guessed) = out[i].clone();
            let reexports: Vec<Binding> =
                self.file_bindings(conn, &cur)?.iter().filter(|b| b.export && (b.local == name || b.local == "*")).cloned().collect();
            for b in reexports {
                let (f, g) = match self.origin(conn, &cur, &b.module)? {
                    Origin::File(f) => (f, cur_guessed),
                    Origin::Guess(f) => (f, true),
                    _ => continue,
                };
                if !out.iter().any(|(o, _)| *o == f) {
                    out.push((f, g));
                }
            }
            i += 1;
        }
        self.exports.insert(key, out.clone());
        Ok(out)
    }

    /// [`Scope::Module`] for `name` in the module at `file`, which guesswork placed when `guessed`.
    fn module_scope(&mut self, conn: &Connection, file: &str, name: &str, guessed: bool) -> Result<Scope> {
        let all = self.export_files(conn, file, name)?;
        let guessed = all.iter().filter(|(_, g)| guessed || *g).map(|(f, _)| f.clone()).collect();
        Ok(Scope::Module { files: all.into_iter().map(|(f, _)| f).collect(), guessed, name: name.to_string() })
    }

    /// Where the import rules put a reference from a TypeScript file: `None` when they say nothing (the name isn't
    /// imported, or comes from a module that can't be placed).
    pub(crate) fn scope(
        &mut self,
        conn: &Connection,
        file: &str,
        name: &str,
        receiver: Option<&str>,
        receiver_typed: bool,
    ) -> Result<Option<Scope>> {
        match receiver {
            // `map(…)`, `new Foo()`, `Foo` imported by name.
            None => {
                let Some(b) = self.imported(conn, file, name)? else { return Ok(None) };
                if b.name == "*" {
                    return Ok(None);
                }
                let exported = if b.name == "default" { name } else { b.name.as_str() };
                Ok(match self.origin(conn, file, &b.module)? {
                    Origin::External => Some(Scope::External),
                    Origin::File(f) => Some(self.module_scope(conn, &f, exported, false)?),
                    Origin::Guess(f) => Some(self.module_scope(conn, &f, exported, true)?),
                    Origin::Missing => Some(Scope::Unplaced),
                    Origin::Unknown => None,
                })
            }
            Some(r) => {
                // A member of a namespace import (`CartActions.SetCart(…)`), or of something imported from a package
                // (`Validators.required`, `TestBed.inject`), or of a built-in object (`console.log`).
                let root = r.split('.').next().unwrap_or(r);
                if !root.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '$') || r.contains(['(', '[', '<', '!', '?']) {
                    return Ok(None);
                }
                let Some(b) = self.imported(conn, file, root)? else {
                    return Ok((!receiver_typed && GLOBAL_OBJECTS.contains(&root)).then_some(Scope::External));
                };
                let origin = match self.origin(conn, file, &b.module)? {
                    o @ (Origin::File(_) | Origin::Guess(_)) if b.name == "*" => o,
                    // A namespace a barrel re-exports (`export * as fromCart from './cart.actions'`).
                    Origin::File(f) => match self.reexported_namespace(conn, &f, &b.name)? {
                        Some(o) => o,
                        None => return Ok(None),
                    },
                    Origin::Guess(f) => match self.reexported_namespace(conn, &f, &b.name)? {
                        Some(Origin::File(g) | Origin::Guess(g)) => Origin::Guess(g),
                        Some(o) => o,
                        None => return Ok(None),
                    },
                    o => o,
                };
                Ok(match origin {
                    Origin::External => Some(Scope::External),
                    Origin::Missing if r == root => Some(Scope::Unplaced),
                    Origin::File(f) if r == root => Some(self.module_scope(conn, &f, name, false)?),
                    Origin::Guess(f) if r == root => Some(self.module_scope(conn, &f, name, true)?),
                    _ => None,
                })
            }
        }
    }

    /// What `export * as name from 'module'` in `file` names, if `file` re-exports a namespace as `name`.
    fn reexported_namespace(&mut self, conn: &Connection, file: &str, name: &str) -> Result<Option<Origin>> {
        let Some(b) = self.file_bindings(conn, file)?.iter().find(|b| b.export && b.local == name && b.name == "*").cloned() else {
            return Ok(None);
        };
        Ok(Some(self.origin(conn, file, &b.module)?))
    }

    /// The module `file` imports the name `local` from, when it imports one by that name.
    pub(crate) fn module_of(&mut self, conn: &Connection, file: &str, local: &str) -> Result<Option<String>> {
        Ok(self.imported(conn, file, local)?.map(|b| b.module))
    }

    /// Where the qualifier `q` of a type named in `file` (`JSX`, `HonoJSX.Inner`) is declared when its first name is
    /// imported: `Some(Some((file, q)))` for a repo file, with `q` as that file names it (an alias undone, a namespace
    /// import's own name dropped), `Some(None)` for a package or a module the index can't place. `None` when `file`
    /// imports no such name: the namespace is the file's own or a global one.
    pub(crate) fn qualifier_source(&mut self, conn: &Connection, file: &str, q: &str) -> Result<Option<Option<(String, String)>>> {
        let (head, rest) = q.split_once('.').map_or((q, None), |(h, r)| (h, Some(r)));
        let Some(b) = self.imported(conn, file, head)? else { return Ok(None) };
        let inner = match (b.name.as_str(), rest) {
            ("*", Some(rest)) => rest.to_string(),
            ("*", None) => return Ok(Some(None)),
            (name, Some(rest)) => format!("{name}.{rest}"),
            (name, None) => name.to_string(),
        };
        Ok(Some(match self.origin(conn, file, &b.module)? {
            Origin::File(f) | Origin::Guess(f) => Some((f, inner)),
            _ => None,
        }))
    }

    /// Whether `file` imports the type `ty` from a package outside the repo.
    /// Whether code in the TypeScript `file` can name the repo's top-level declaration `name`: the file declares it,
    /// imports or re-exports it, or the repo declares it in a script (a file with no import or export, whose declarations are
    /// global). A name the file neither declares nor imports is otherwise a global of the platform's (`Response`,
    /// `Event`), even when one of the repo's modules exports a declaration of that name. A file importing nothing is
    /// read as before: it sees every declaration.
    pub(crate) fn sees(&mut self, conn: &Connection, file: &str, name: &str) -> Result<bool> {
        let bindings = self.file_bindings(conn, file)?;
        // A file importing nothing at all: its imports may be ones the index doesn't read (triple-slash references,
        // a bundler's globals), so any of the repo's declarations may be what it names.
        if !bindings.iter().any(|b| !b.export) {
            return Ok(true);
        }
        // Imported, or re-exported (`export { X } from './x'` names the module's `X`).
        if bindings.iter().any(|b| b.local == name) {
            return Ok(true);
        }
        let files: Vec<String> = conn
            .prepare_cached(
                "SELECT DISTINCT f.path FROM symbols s JOIN files f ON f.id = s.file_id
                 WHERE s.name = ?1 AND s.name = ?1 COLLATE BINARY AND s.container IS NULL",
            )?
            .query_map([name], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for f in files {
            if f == file || !is_ts_path(&f) || self.script(conn, &f)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Whether `file` is a script: it neither imports nor exports anything.
    fn script(&mut self, conn: &Connection, file: &str) -> Result<bool> {
        if let Some(&s) = self.scripts.get(file) {
            return Ok(s);
        }
        let module: bool = conn
            .prepare_cached(
                "SELECT EXISTS(SELECT 1 FROM import_bindings b JOIN files f ON f.id = b.file_id WHERE f.path = ?1)
                 OR EXISTS(SELECT 1 FROM symbols s JOIN files f ON f.id = s.file_id
                           WHERE f.path = ?1 AND s.container IS NULL AND ' ' || s.signature LIKE '% export %')",
            )?
            .query_row([file], |r| r.get(0))?;
        self.scripts.insert(file.to_string(), !module);
        Ok(!module)
    }

    /// Whether the repo declares `name` at the top level of a TypeScript or JavaScript file.
    pub(crate) fn declared(&mut self, conn: &Connection, name: &str) -> Result<bool> {
        let files: Vec<String> = conn
            .prepare_cached(
                "SELECT DISTINCT f.path FROM symbols s JOIN files f ON f.id = s.file_id
                 WHERE s.name = ?1 AND s.name = ?1 COLLATE BINARY AND s.container IS NULL",
            )?
            .query_map([name], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(files.iter().any(|f| is_ts_path(f)))
    }

    pub(crate) fn imports_external(&mut self, conn: &Connection, file: &str, ty: &str) -> Result<bool> {
        let Some(b) = self.imported(conn, file, ty)? else { return Ok(false) };
        Ok(b.name != "*" && self.origin(conn, file, &b.module)? == Origin::External)
    }
}

/// `@scope/name/…` → `@scope/name`; `name/…` → `name`.
fn package_name(module: &str) -> &str {
    let mut parts = module.splitn(3, '/');
    let first = parts.next().unwrap_or(module);
    if first.starts_with('@')
        && let Some(second) = parts.next()
    {
        return &module[..first.len() + 1 + second.len()];
    }
    first
}

fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

/// The substitutions of the `paths` pattern matching `module`, and what its `*` stands for: an exact pattern first,
/// else the wildcard pattern with the longest prefix.
fn match_pattern<'p>(paths: &'p [(String, Vec<String>)], module: &'p str) -> Option<(&'p [String], &'p str)> {
    if let Some((_, subs)) = paths.iter().find(|(p, _)| !p.contains('*') && p == module) {
        return Some((subs, ""));
    }
    paths
        .iter()
        .filter_map(|(p, subs)| {
            let (pre, post) = p.split_once('*')?;
            let star = module.strip_prefix(pre)?.strip_suffix(post)?;
            Some((pre.len(), subs.as_slice(), star))
        })
        .max_by_key(|(n, _, _)| *n)
        .map(|(_, subs, star)| (subs, star))
}

/// `dir` joined with a relative module path, `..` applied; `None` when it climbs out of the repo.
fn normalize(dir: &str, module: &str) -> Option<String> {
    let mut parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
    for seg in module.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    Some(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_names_and_relative_paths() {
        assert_eq!(package_name("@ngrx/store/testing"), "@ngrx/store");
        assert_eq!(package_name("rxjs/operators"), "rxjs");
        assert_eq!(package_name("lodash"), "lodash");
        assert_eq!(normalize("src/app/a", "../b/c").as_deref(), Some("src/app/b/c"));
        assert_eq!(normalize("src/app", "./x").as_deref(), Some("src/app/x"));
        assert_eq!(normalize("", "../x"), None);
    }
}
